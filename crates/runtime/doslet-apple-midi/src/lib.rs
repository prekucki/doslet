//! macOS system GM synthesis. The caller owns the guest clock and PCM storage.
//! No output device, callback, thread or guest-machine reference lives here.
#![cfg(target_os = "macos")]

#[allow(unsafe_code)]
mod unit;

use doslet_midi_synth::wire::Decoder;

pub struct Timeline {
    unit: unit::Unit,
    wire: Decoder,
    rate: u32,
    frames: u64,
    notes: u64,
}

impl Timeline {
    pub fn new(rate: u32) -> Result<Self, String> {
        if !(22050..=192000).contains(&rate) {
            return Err("Apple MIDI output rate must be 22050..192000 Hz".into());
        }
        Ok(Self {
            unit: unit::Unit::new(rate)?,
            wire: Decoder::new(),
            rate,
            frames: 0,
            notes: 0,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.rate
    }

    pub fn note_events(&self) -> u64 {
        self.notes
    }

    pub fn push_byte(&mut self, byte: u8) -> Result<(), String> {
        self.receive(byte, false)
    }

    /// Setup-only replay services the AU between messages to bound its queue.
    /// Voices restart on an output switch; discarded PCM is not guest time.
    pub fn restore_byte(&mut self, byte: u8) -> Result<(), String> {
        self.receive(byte, true)
    }

    fn receive(&mut self, byte: u8, restore: bool) -> Result<(), String> {
        if let Some(message) = self
            .wire
            .push(byte)
            .map_err(|e| format!("Apple MIDI: {e:?}"))?
        {
            self.unit.message(message)?;
            if message[0] & 0xF0 == 0x90 && message[2] != 0 {
                self.notes += 1;
            }
            if restore {
                self.unit.render(&mut [[0.0; 2]; 64])?;
            }
        }
        Ok(())
    }

    /// Rebase a fresh synth without rendering skipped guest time.
    pub fn skip_until(&mut self, nanos: u64) -> Result<(), String> {
        let target = self.target(nanos);
        if target < self.frames {
            return Err("Apple MIDI time moved backwards".into());
        }
        self.frames = target;
        Ok(())
    }

    pub fn render_until(&mut self, nanos: u64, output: &mut [[f32; 2]]) -> Result<usize, String> {
        let remaining = self
            .target(nanos)
            .checked_sub(self.frames)
            .ok_or("Apple MIDI time moved backwards")?;
        let count = remaining
            .min(output.len() as u64)
            .min(unit::MAX_FRAMES as u64) as usize;
        if count != 0 {
            self.unit.render(&mut output[..count])?;
            self.frames += count as u64;
        }
        Ok(count)
    }

    fn target(&self, nanos: u64) -> u64 {
        (u128::from(nanos) * u128::from(self.rate) / 1_000_000_000) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_bank_renders_notes_and_preserves_guest_timing() {
        let mut synth = Timeline::new(48000).unwrap();
        synth.skip_until(1_000_000_000).unwrap();
        let mut samples = [[0.0; 2]; 512];
        assert_eq!(synth.render_until(1_000_000_000, &mut samples).unwrap(), 0);
        // Program, channel volume, pan and a note: no MIDI file or game assets.
        for byte in [0xC0, 0, 0xB0, 7, 100, 10, 64, 0x90, 60, 100] {
            synth.push_byte(byte).unwrap();
        }
        let mut rendered = 0;
        let mut peak = 0.0_f32;
        loop {
            let count = synth.render_until(1_100_000_000, &mut samples).unwrap();
            if count == 0 {
                break;
            }
            rendered += count;
            for &sample in samples[..count].iter().flatten() {
                assert!(sample.is_finite());
                peak = peak.max(sample.abs());
            }
        }
        assert_eq!(rendered, 4800);
        assert_eq!(synth.note_events(), 1);
        assert!(
            peak > 0.0001,
            "system GM bank must produce PCM, peak={peak}"
        );
        assert!(synth.render_until(1_000_000_000, &mut samples).is_err());
        for byte in [0x80, 60, 0, 0xB0, 120, 0] {
            synth.push_byte(byte).unwrap();
        }
    }

    #[test]
    fn snapshot_replay_does_not_advance_guest_time() {
        let mut synth = Timeline::new(44100).unwrap();
        synth.skip_until(2_000_000_000).unwrap();
        // Larger than a typical Audio Unit event queue, serviced at setup only.
        for _ in 0..4096 {
            for byte in [0xB0, 7, 100] {
                synth.restore_byte(byte).unwrap();
            }
        }
        let mut samples = [[0.0; 2]; 512];
        assert_eq!(synth.render_until(2_000_000_000, &mut samples).unwrap(), 0);
        assert_eq!(
            synth.render_until(2_010_000_000, &mut samples).unwrap(),
            441
        );
        assert!(Timeline::new(0).is_err());
    }
}
