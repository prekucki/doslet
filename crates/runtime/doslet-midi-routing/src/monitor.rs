//! Small presentation snapshots; no synth ownership or PCM feedback.
use super::{Mode, Routing};
use doslet_midi_synth::wire::ChannelState;
use doslet_mt32_gm::{memory, tables};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Channel {
    pub state: ChannelState,
    pub enabled: bool,
    pub instrument: [u8; 16],
}
impl Channel {
    pub const BYTES: usize = 25;
    pub fn write(self, bytes: &mut [u8]) {
        let s = self.state;
        bytes[..9].copy_from_slice(&[
            s.program,
            s.volume,
            s.expression,
            s.pan,
            u8::from(s.sustain),
            s.notes as u8,
            (s.notes >> 8) as u8,
            s.velocity,
            u8::from(self.enabled),
        ]);
        bytes[9..Self::BYTES].copy_from_slice(&self.instrument);
    }
    pub fn read(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::BYTES {
            return None;
        }
        Some(Self {
            state: ChannelState {
                program: bytes[0],
                volume: bytes[1],
                expression: bytes[2],
                pan: bytes[3],
                sustain: bytes[4] != 0,
                notes: u16::from_le_bytes([bytes[5], bytes[6]]),
                velocity: bytes[7],
            },
            enabled: bytes[8] != 0,
            instrument: bytes[9..].try_into().ok()?,
        })
    }
}
impl Routing {
    /// State on the selected side of the translator. Raw MT-32 parameters also
    /// reflect SysEx patch/volume/channel assignment, not just channel messages.
    pub fn channels(&self, output: Mode) -> [Channel; 16] {
        let state = if self.mode == Mode::Mt32 && output == Mode::Gm {
            &self.gm
        } else {
            &self.raw
        };
        let mut channels = state.channels().map(|state| Channel {
            state,
            enabled: self.mode != Mode::Mt32 || output != Mode::Mt32,
            instrument: [0; 16],
        });
        if self.mode == Mode::Mt32 && output == Mode::Mt32 {
            let mem = self.translator.memory();
            for part in 0..memory::PARTS {
                let Some(channel) = mem.channel(part) else {
                    continue;
                };
                let out = &mut channels[usize::from(channel)];
                out.enabled = true;
                let patch = &mem.patch_temp[part];
                out.state.volume = (u32::from(patch[memory::OUTPUT_LEVEL].min(100))
                    * u32::from(mem.system[memory::MASTER_VOLUME].min(100))
                    * 127
                    / 10000) as u8;
                out.state.pan = (u16::from(patch[memory::PANPOT].min(14)) * 127 / 14) as u8;
                let group = patch[memory::TIMBRE_GROUP] & 3;
                let number = patch[memory::TIMBRE_NUMBER] & 63;
                let name = if part == memory::RHYTHM_PART {
                    b"Rhythm".as_slice()
                } else if group < 2 {
                    out.state.program = group * 64 + number;
                    tables::PRESETS[usize::from(out.state.program)].0.as_bytes()
                } else {
                    &mem.timbre_temp[part][..10]
                };
                let n = name.len().min(out.instrument.len());
                out.instrument[..n].copy_from_slice(&name[..n]);
            }
        }
        channels
    }
}
