//! MT-32-family audio through the no_std moont CM-32L engine. ROMs load at setup.
//! No filesystem or PC dependency; MIDI framing and output storage are bounded.
#![no_std]
use doslet_midi_synth::wire::{Decoder, WireError};
use moont::{Frame, Synth, cm32l};
use sha1::{Digest, Sha1};

#[derive(Debug)]
pub enum Error {
    Rom(cm32l::RomError),
    UnsupportedRom,
    Wire(WireError),
    Rate,
    TimeReversed,
    QueueFull,
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Rom(error) => write!(f, "CM-32L ROM: {error}"),
            Self::UnsupportedRom => {
                f.write_str("CM-32L requires Control ROM v1.02 and the matching 1-MiB PCM ROM")
            }
            Self::Wire(error) => write!(f, "MIDI framing: {error:?}"),
            Self::Rate => f.write_str("CM-32L output rate must be 22050..192000 Hz"),
            Self::TimeReversed => f.write_str("CM-32L time moved backwards"),
            Self::QueueFull => f.write_str("CM-32L MIDI queue full"),
        }
    }
}
impl core::error::Error for Error {}

enum Device {
    Mt32(cm32l::Device),
    Gm(cm32l::GmDevice),
}
impl Device {
    fn play_msg(&mut self, message: u32) -> bool {
        match self {
            Self::Mt32(d) => d.play_msg(message),
            Self::Gm(d) => d.play_msg(message),
        }
    }
    fn play_sysex(&mut self, message: &[u8]) -> bool {
        match self {
            Self::Mt32(d) => d.play_sysex(message),
            Self::Gm(d) => d.play_sysex(message),
        }
    }
    fn render(&mut self, frames: &mut [Frame]) {
        match self {
            Self::Mt32(d) => d.render(frames),
            Self::Gm(d) => d.render(frames),
        }
    }
}
pub struct Timeline {
    device: Device,
    wire: Decoder,
    rate: u32,
    frames: u64,
    phase: u64,
    previous: Frame,
    current: Frame,
    scratch: [Frame; 384],
    notes: u64,
    sysex: u64,
}
impl Timeline {
    pub fn new(control: &[u8], pcm: &[u8], rate: u32) -> Result<Self, Error> {
        Self::with_gm(control, pcm, rate, false)
    }
    /// GM compatibility wrapper using CM-32L instruments (eight melodic parts).
    pub fn with_gm(control: &[u8], pcm: &[u8], rate: u32, gm: bool) -> Result<Self, Error> {
        if !(22050..=192000).contains(&rate) {
            return Err(Error::Rate);
        }
        // moont's ROM layout is fixed to CM-32L v1.02. Validate before its parser
        // indexes ROM tables; equal-sized MT-32 or corrupt images are unsuitable.
        const CONTROL: [u8; 20] = [
            0xa4, 0x39, 0xfb, 0xb3, 0x90, 0xda, 0x38, 0xca, 0xda, 0x95, 0xa7, 0xcb, 0xb1, 0xd6,
            0xca, 0x19, 0x9c, 0xd6, 0x6e, 0xf8,
        ];
        const PCM: [u8; 20] = [
            0x28, 0x9c, 0xc2, 0x98, 0xad, 0x53, 0x2b, 0x70, 0x24, 0x61, 0xbf, 0xc7, 0x38, 0x00,
            0x9d, 0x9e, 0xbe, 0x80, 0x25, 0xea,
        ];
        if Sha1::digest(control).as_slice() != CONTROL || Sha1::digest(pcm).as_slice() != PCM {
            return Err(Error::UnsupportedRom);
        }
        let rom = cm32l::Rom::new(control, pcm).map_err(Error::Rom)?;
        Ok(Self {
            device: if gm {
                Device::Gm(cm32l::GmDevice::new(rom))
            } else {
                Device::Mt32(cm32l::Device::new(rom))
            },
            wire: Decoder::new(),
            rate,
            frames: 0,
            phase: 0,
            previous: Frame(0, 0),
            current: Frame(0, 0),
            scratch: [Frame(0, 0); 384],
            notes: 0,
            sysex: 0,
        })
    }
    /// SysEx ownership is copied by moont into its bounded event queue here,
    /// outside the audio render callback. Channel messages allocate nothing.
    pub fn push_byte(&mut self, byte: u8) -> Result<(), Error> {
        if let Some(message) = self.wire.push(byte).map_err(Error::Wire)? {
            let accepted = if message[0] == 0xF0 {
                self.sysex += 1;
                self.device.play_sysex(message)
            } else if (0x80..=0xEF).contains(&message[0]) {
                if message[0] & 0xF0 == 0x90 && message.get(2).copied().unwrap_or(0) != 0 {
                    self.notes += 1;
                }
                let packed = message.iter().enumerate().fold(0u32, |value, (i, byte)| {
                    value | (u32::from(*byte) << (i * 8))
                });
                self.device.play_msg(packed)
            } else {
                true
            };
            if !accepted {
                return Err(Error::QueueFull);
            }
        }
        Ok(())
    }
    /// Rebase a freshly restored output to the current guest clock.
    pub fn rebase(&mut self, nanos: u64) {
        self.frames = (u128::from(nanos) * u128::from(self.rate) / 1_000_000_000) as u64;
        self.phase = (self.frames % u64::from(self.rate)) * 32000 % u64::from(self.rate);
    }
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }
    pub fn note_events(&self) -> u64 {
        self.notes
    }
    pub fn sysex_messages(&self) -> u64 {
        self.sysex
    }
    /// Bounded synthesis at 32 kHz, then linear resampling into caller storage.
    /// It renders no source frames beyond the requested output timeline.
    pub fn render_until(&mut self, nanos: u64, output: &mut [[f32; 2]]) -> Result<usize, Error> {
        let target = (u128::from(nanos) * u128::from(self.rate) / 1_000_000_000) as u64;
        let remaining = target.checked_sub(self.frames).ok_or(Error::TimeReversed)?;
        let count = remaining.min(output.len() as u64).min(256) as usize;
        let source_count = ((self.phase + count as u64 * 32000) / u64::from(self.rate)) as usize;
        self.device.render(&mut self.scratch[..source_count]);
        let mut source = 0;
        for frame in &mut output[..count] {
            self.phase += 32000;
            while self.phase >= u64::from(self.rate) {
                self.phase -= u64::from(self.rate);
                self.previous = self.current;
                self.current = self.scratch[source];
                source += 1;
            }
            let fraction = self.phase as f32 / self.rate as f32;
            frame[0] = (f32::from(self.previous.0)
                + (f32::from(self.current.0) - f32::from(self.previous.0)) * fraction)
                / 32768.0;
            frame[1] = (f32::from(self.previous.1)
                + (f32::from(self.current.1) - f32::from(self.previous.1)) * fraction)
                / 32768.0;
        }
        self.frames += count as u64;
        Ok(count)
    }
}
