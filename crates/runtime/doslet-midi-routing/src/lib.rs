//! Guest MIDI dialect selection and output-independent replay state.
#![no_std]
extern crate alloc;
use alloc::boxed::Box;
use doslet_midi_synth::wire::{Decoder, REPLAY_CAPACITY, State, WireError};
use doslet_mt32_gm::Translator;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Gm,
    Mt32,
}
impl Mode {
    pub const ALL: [Self; 2] = [Self::Gm, Self::Mt32];
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gm => "gm",
            Self::Mt32 => "mt32",
        }
    }
}
impl core::fmt::Display for Mode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Gm => "General MIDI",
            Self::Mt32 => "MT-32",
        })
    }
}
// Worst case: alternating written bytes need individual 11-byte DT1 packets,
// followed by 16-channel replay. Do not assume every dirty region is contiguous.
pub const SNAPSHOT_CAPACITY: usize = 262144;
pub struct Routing {
    mode: Mode,
    decoder: Decoder,
    raw: State,
    gm: State,
    translator: Box<Translator>,
    upgraded: bool,
}
impl Routing {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            decoder: Decoder::new(),
            raw: State::new(),
            gm: State::new(),
            translator: Box::new(Translator::new()),
            upgraded: false,
        }
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }
    pub fn upgraded(&self) -> bool {
        self.upgraded
    }
    pub fn translator(&self) -> &Translator {
        &self.translator
    }
    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.upgraded = false;
    }
    /// Emits complete messages, preserving wire framing across host batches.
    /// A Roland MT-32 SysEx upgrades GM input mode before reaching a GM synth.
    pub fn push<F: FnMut(&[u8])>(
        &mut self,
        byte: u8,
        output: Mode,
        emit: &mut F,
    ) -> Result<(), WireError> {
        let Some(message) = self.decoder.push(byte)? else {
            return Ok(());
        };
        if self.mode == Mode::Gm
            && message.len() >= 5
            && message[0] == 0xF0
            && message[1] == 0x41
            && message[2] <= 0x10
            && message[3] == 0x16
        {
            self.mode = Mode::Mt32;
            self.upgraded = true;
            // Earlier GM notes must not remain hanging on the upgraded output.
            for channel in 0..16 {
                emit(&[0xB0 | channel, 120, 0]);
            }
            self.raw = State::new();
            self.gm = State::new();
        }
        self.raw.receive(message);
        if self.mode == Mode::Mt32 {
            if message.len() >= 11
                && message[..8] == [0xF0, 0x41, 0x10, 0x16, 0x12, 0x7F, 0, 0]
                && message[5..message.len() - 1]
                    .iter()
                    .fold(0u8, |sum, byte| sum.wrapping_add(*byte))
                    & 127
                    == 0
            {
                self.raw = State::new();
            }
            let gm = &mut self.gm;
            self.translator.translate(message, &mut |translated| {
                gm.receive(translated);
                if output == Mode::Gm {
                    emit(translated);
                }
            });
            if output == Mode::Mt32 {
                emit(message);
            }
        } else {
            emit(message);
        }
        Ok(())
    }
    /// Streams replay in bounded messages. No partial wire message is replayed:
    /// the decoder belongs to the router and survives every output switch.
    pub fn replay<F: FnMut(&[u8])>(&self, output: Mode, emit: &mut F) {
        let mut bytes = [0u8; REPLAY_CAPACITY];
        if self.mode == Mode::Mt32 && output == Mode::Mt32 {
            emit(&[0xF0, 0x41, 0x10, 0x16, 0x12, 0x7F, 0, 0, 1, 0, 0xF7]);
            self.translator.memory().replay(emit);
            let count = self.raw.write(&mut bytes);
            // Program/pan/volume/bend range are already in parameter memory;
            // replay only expression/modulation/sustain and pitch before notes.
            let mut wire = Decoder::new();
            for &byte in &bytes[..count] {
                if let Ok(Some(message)) = wire.push(byte) {
                    let kind = message[0] & 0xF0;
                    if matches!(kind, 0x80 | 0x90 | 0xE0)
                        || kind == 0xB0 && matches!(message[1], 1 | 11 | 64)
                    {
                        emit(message);
                    }
                }
            }
        } else {
            let state = if self.mode == Mode::Mt32 {
                &self.gm
            } else {
                &self.raw
            };
            let count = state.write(&mut bytes);
            let mut wire = Decoder::new();
            for &byte in &bytes[..count] {
                if let Ok(Some(message)) = wire.push(byte) {
                    emit(message);
                }
            }
        }
    }
}
