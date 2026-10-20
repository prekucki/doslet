//! Timestamped stereo PCM and a bounded guest-clock resampler/mixer.
#![no_std]
extern crate alloc;
pub mod spectrum;
use alloc::{boxed::Box, vec};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    /// Guest nanoseconds at the start of this source sample.
    pub nanos: u64,
    /// Source sample interval; silence follows the last sample's interval.
    pub duration: u32,
    pub samples: [i16; 2],
}
pub const EMPTY: Frame = Frame {
    nanos: 0,
    duration: 0,
    samples: [0; 2],
};
/// A hardware source sample before host resampling or card-output mixing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceFrame {
    pub nanos: u64,
    pub rate: u32,
    pub samples: [i16; 2],
}
const CAPACITY: usize = 16384;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    SampleRate,
    Full,
    TimeReversed,
    InvalidFrame,
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::SampleRate => "invalid PCM output sample rate",
            Self::Full => "PCM resampler queue full",
            Self::TimeReversed => "PCM time reversed",
            Self::InvalidFrame => "invalid PCM frame",
        })
    }
}
pub struct Mixer {
    queue: Box<[Frame]>,
    head: usize,
    len: usize,
    previous: Option<Frame>,
    from: [i16; 2],
    last: Option<u64>,
    cursor: u64,
    rate: u32,
    received: u64,
    nonzero: u64,
}
impl Mixer {
    pub fn new(rate: u32) -> Result<Self, Error> {
        if !(22050..=192000).contains(&rate) {
            return Err(Error::SampleRate);
        }
        Ok(Self {
            queue: vec![EMPTY; CAPACITY].into_boxed_slice(),
            head: 0,
            len: 0,
            previous: None,
            from: [0; 2],
            last: None,
            cursor: 0,
            rate,
            received: 0,
            nonzero: 0,
        })
    }
    pub fn reset(&mut self, rate: u32, nanos: u64) -> Result<(), Error> {
        if !(22050..=192000).contains(&rate) {
            return Err(Error::SampleRate);
        }
        self.rate = rate;
        self.cursor = (u128::from(nanos) * u128::from(rate) / 1_000_000_000) as u64;
        self.head = 0;
        self.len = 0;
        self.previous = None;
        self.from = [0; 2];
        self.last = None;
        self.received = 0;
        self.nonzero = 0;
        Ok(())
    }
    /// Validate the entire batch before changing the queue. No allocation.
    pub fn receive(&mut self, frames: &[Frame]) -> Result<(), Error> {
        if frames.len() > CAPACITY - self.len {
            return Err(Error::Full);
        }
        let mut last = self.last;
        for frame in frames {
            if frame.duration == 0 || frame.nanos.checked_add(u64::from(frame.duration)).is_none() {
                return Err(Error::InvalidFrame);
            }
            if last.is_some_and(|nanos| frame.nanos < nanos) {
                return Err(Error::TimeReversed);
            }
            last = Some(frame.nanos);
        }
        for &frame in frames {
            self.queue[(self.head + self.len) % CAPACITY] = frame;
            self.len += 1;
            self.received += 1;
            self.nonzero += u64::from(frame.samples != [0; 2]);
        }
        self.last = last;
        Ok(())
    }
    /// Mix PCM into an existing MIDI block. Both share the output frame clock.
    /// Linear interpolation is bounded to consecutive source sample intervals;
    /// gaps and stopped playback produce silence. Clamp after summing sources.
    pub fn mix(&mut self, output: &mut [[f32; 2]]) {
        for target in output {
            let nanos = (u128::from(self.cursor) * 1_000_000_000 / u128::from(self.rate)) as u64;
            while self.len != 0 && self.queue[self.head].nanos <= nanos {
                let next = self.queue[self.head];
                self.from = if let Some(previous) = self.previous
                    && next.nanos <= previous.nanos + u64::from(previous.duration) + 1
                {
                    previous.samples
                } else {
                    next.samples
                };
                self.previous = Some(next);
                self.head = (self.head + 1) % CAPACITY;
                self.len -= 1;
            }
            let mut pcm = [0.0; 2];
            if let Some(previous) = self.previous
                && nanos < previous.nanos + u64::from(previous.duration)
            {
                for (channel, sample) in pcm.iter_mut().enumerate() {
                    // Causal interpolation uses two already received samples.
                    // One source interval of delay makes packet partitioning irrelevant.
                    let fraction = (nanos - previous.nanos) as f32 / previous.duration as f32;
                    *sample = f32::from(self.from[channel])
                        + (f32::from(previous.samples[channel]) - f32::from(self.from[channel]))
                            * fraction;
                    *sample /= 32768.0;
                }
            }
            for channel in 0..2 {
                target[channel] = (target[channel] + pcm[channel]).clamp(-1.0, 1.0);
            }
            self.cursor += 1;
        }
    }
    pub fn render_until(&mut self, nanos: u64, output: &mut [[f32; 2]]) -> Result<usize, Error> {
        let end = (u128::from(nanos) * u128::from(self.rate) / 1_000_000_000) as u64;
        let count = end
            .checked_sub(self.cursor)
            .ok_or(Error::TimeReversed)?
            .min(output.len() as u64) as usize;
        output[..count].fill([0.0; 2]);
        self.mix(&mut output[..count]);
        Ok(count)
    }
    pub fn received(&self) -> u64 {
        self.received
    }
    pub fn nonzero(&self) -> u64 {
        self.nonzero
    }
}

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
