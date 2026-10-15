//! A bounded guest-time queue for pull-based audio hosts. Allocate storage once,
//! then receive/render/reset without allocation or host clocks.
use crate::{MidiError, Synth};
use alloc::{boxed::Box, vec};

const CAPACITY: usize = 16384;
#[derive(Clone, Copy)]
struct Byte {
    frame: u64,
    value: u8,
}
const EMPTY: Byte = Byte { frame: 0, value: 0 };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamError {
    Full,
    TimeReversed,
    Midi(MidiError),
}

pub struct Stream {
    synth: Synth,
    queue: Box<[Byte]>,
    head: usize,
    len: usize,
    cursor: u64,
    committed: u64,
    underruns: u64,
}
impl Stream {
    pub fn new(rate: u32) -> Result<Self, MidiError> {
        Ok(Self {
            synth: Synth::new(rate)?,
            queue: vec![EMPTY; CAPACITY].into_boxed_slice(),
            head: 0,
            len: 0,
            cursor: 0,
            committed: 0,
            underruns: 0,
        })
    }
    fn frame(&self, nanos: u64) -> u64 {
        (u128::from(nanos) * u128::from(self.synth.sample_rate()) / 1_000_000_000) as u64
    }
    /// Discontinuities require an explicit reset, e.g. a different game or audio
    /// becoming available after silent execution. This never reallocates.
    pub fn reset(&mut self, nanos: u64) {
        self.synth = Synth::new(self.synth.sample_rate()).expect("existing sample rate");
        self.head = 0;
        self.len = 0;
        self.cursor = self.frame(nanos);
        self.committed = self.cursor;
        self.underruns = 0;
    }
    /// Wire bytes occur at this batch boundary. The watermark certifies that all
    /// preceding guest MIDI has arrived, including intervals containing no bytes.
    pub fn receive(&mut self, nanos: u64, bytes: &[u8]) -> Result<(), StreamError> {
        let frame = self.frame(nanos);
        if frame < self.committed {
            return Err(StreamError::TimeReversed);
        }
        if bytes.len() > CAPACITY - self.len {
            return Err(StreamError::Full);
        }
        for &value in bytes {
            self.queue[(self.head + self.len) % CAPACITY] = Byte { frame, value };
            self.len += 1;
        }
        self.committed = frame;
        Ok(())
    }
    pub fn render(&mut self, output: &mut [[f32; 2]]) -> Result<usize, StreamError> {
        output.fill([0.0; 2]);
        let mut written = 0;
        while written < output.len() {
            while self.len > 0 && self.queue[self.head].frame <= self.cursor {
                let byte = self.queue[self.head].value;
                self.synth.push_byte(byte).map_err(StreamError::Midi)?;
                self.head = (self.head + 1) % CAPACITY;
                self.len -= 1;
            }
            let end = if self.len > 0 {
                self.queue[self.head].frame
            } else {
                self.committed
            };
            let count = (end - self.cursor).min((output.len() - written) as u64) as usize;
            if count == 0 {
                break;
            }
            self.synth.render(&mut output[written..written + count]);
            self.cursor += count as u64;
            written += count;
        }
        if written < output.len() {
            self.underruns += 1;
        }
        Ok(written)
    }
    pub fn nanos(&self) -> u64 {
        (u128::from(self.cursor) * 1_000_000_000 / u128::from(self.synth.sample_rate())) as u64
    }
    pub fn sample_rate(&self) -> u32 {
        self.synth.sample_rate()
    }
    pub fn notes(&self) -> u64 {
        self.synth.note_events()
    }
    pub fn underruns(&self) -> u64 {
        self.underruns
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timestamped_wire_matches_the_original_timeline_across_callback_sizes() {
        let mut pull = Stream::new(48000).unwrap();
        let mut reference = crate::Timeline::new(48000).unwrap();
        let mut expected = [[0.0; 2]; 4096];
        let mut used = 0;
        for (nanos, bytes) in [
            (5_000_000, &[0x90, 69, 100][..]),
            (23_000_000, &[69, 0][..]),
            (80_000_000, &[][..]),
        ] {
            pull.receive(nanos, bytes).unwrap();
            used += reference
                .render_until(nanos, &mut expected[used..])
                .unwrap();
            for &byte in bytes {
                reference.push_byte(byte).unwrap();
            }
        }
        let mut actual = [[0.0; 2]; 4096];
        let mut rendered = 0;
        for chunk in actual[..used].chunks_mut(137) {
            rendered += pull.render(chunk).unwrap();
        }
        assert_eq!(rendered, used);
        assert_eq!(actual, expected);
        assert_eq!(pull.notes(), 1);
    }
    #[test]
    fn underrun_holds_guest_time_and_recovers_without_losing_running_status() {
        let mut stream = Stream::new(48000).unwrap();
        stream.receive(0, &[0x90, 69, 100]).unwrap();
        let mut output = [[1.0; 2]; 128];
        assert_eq!(stream.render(&mut output).unwrap(), 0);
        assert_eq!(stream.nanos(), 0);
        assert_eq!(output, [[0.0; 2]; 128]);
        stream.receive(10_000_000, &[72, 100]).unwrap();
        assert_eq!(stream.render(&mut output).unwrap(), 128);
        assert!(output.iter().flatten().any(|v| *v != 0.0));
        assert_eq!(stream.notes(), 1);
        assert_eq!(stream.receive(0, &[]), Err(StreamError::TimeReversed));
        stream.reset(1_000_000_000);
        assert_eq!(stream.nanos(), 1_000_000_000);
        assert_eq!(stream.render(&mut output).unwrap(), 0);
    }
    #[test]
    fn overflow_is_atomic_and_reset_clears_stale_events() {
        let mut stream = Stream::new(48000).unwrap();
        stream.receive(0, &[0xF8; CAPACITY]).unwrap();
        assert_eq!(stream.receive(1_000_000, &[0x90]), Err(StreamError::Full));
        stream.reset(0);
        stream.receive(1_000_000, &[]).unwrap();
        assert_eq!(stream.render(&mut [[1.0; 2]; 128]).unwrap(), 48);
    }
}
