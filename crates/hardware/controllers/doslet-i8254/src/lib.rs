//! Binary mode-2/mode-3 timer foundation. The machine supplies input clock ticks.
//! Other modes, BCD counting and read-back commands return explicit errors.
#![no_std]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PitError {
    UnsupportedControl(u8),
    InvalidChannel(u8),
    InvalidReload,
}

#[derive(Debug)]
struct Channel {
    reload: u32,
    remaining: u32,
    access: u8,
    low_write: Option<u8>,
    high_read: bool,
    latched: Option<u16>,
    programmed: bool,
    mode: u8,
}
impl Default for Channel {
    fn default() -> Self {
        Self {
            reload: 65536,
            remaining: 65536,
            access: 3,
            low_write: None,
            high_read: false,
            latched: None,
            programmed: false,
            mode: 2,
        }
    }
}

#[derive(Debug, Default)]
pub struct Pit {
    channels: [Channel; 3],
}
impl Pit {
    /// Pure deadline query in this chip's own clock domain.
    pub fn ticks_until_pulse(&self, index: u8) -> Result<Option<u32>, PitError> {
        let channel = self
            .channels
            .get(usize::from(index))
            .ok_or(PitError::InvalidChannel(index))?;
        Ok(channel.programmed.then_some(channel.remaining))
    }
    pub fn write_control(&mut self, value: u8) -> Result<(), PitError> {
        let index = value >> 6;
        if index == 3 {
            return Err(PitError::UnsupportedControl(value));
        }
        let channel = &mut self.channels[usize::from(index)];
        let access = (value >> 4) & 3;
        if access == 0 {
            if channel.latched.is_none() {
                channel.latched = Some(channel.count());
                channel.high_read = false;
            }
            return Ok(());
        }
        let mode = (value >> 1) & 7;
        if value & 1 != 0 || !matches!(mode, 2 | 3 | 6 | 7) {
            return Err(PitError::UnsupportedControl(value));
        }
        channel.access = access;
        channel.mode = mode & 3;
        channel.low_write = None;
        channel.high_read = false;
        channel.latched = None;
        channel.programmed = false;
        Ok(())
    }
    pub fn write_counter(&mut self, index: u8, value: u8) -> Result<(), PitError> {
        let channel = self
            .channels
            .get_mut(usize::from(index))
            .ok_or(PitError::InvalidChannel(index))?;
        let count = match channel.access {
            1 => Some(u16::from(value)),
            2 => Some(u16::from(value) << 8),
            _ => match channel.low_write.take() {
                Some(low) => Some(u16::from_le_bytes([low, value])),
                None => {
                    channel.low_write = Some(value);
                    None
                }
            },
        };
        if let Some(count) = count {
            if count == 1 {
                return Err(PitError::InvalidReload);
            }
            channel.reload = if count == 0 { 65536 } else { u32::from(count) };
            channel.remaining = channel.reload;
            channel.programmed = true;
        }
        Ok(())
    }
    pub fn read_counter(&mut self, index: u8) -> Result<u8, PitError> {
        let channel = self
            .channels
            .get_mut(usize::from(index))
            .ok_or(PitError::InvalidChannel(index))?;
        let count = channel.latched.unwrap_or_else(|| channel.count());
        let value = if channel.access == 2 || (channel.access == 3 && channel.high_read) {
            (count >> 8) as u8
        } else {
            count as u8
        };
        if channel.access == 3 && !channel.high_read {
            channel.high_read = true;
        } else {
            channel.high_read = false;
            channel.latched = None;
        }
        Ok(value)
    }
    /// Number of terminal-count pulses; computed in constant time for an entire batch.
    pub fn advance(&mut self, index: u8, ticks: u32) -> Result<u64, PitError> {
        let channel = self
            .channels
            .get_mut(usize::from(index))
            .ok_or(PitError::InvalidChannel(index))?;
        if !channel.programmed {
            return Ok(0);
        }
        if ticks < channel.remaining {
            channel.remaining -= ticks;
            return Ok(0);
        }
        let elapsed = u64::from(ticks - channel.remaining);
        let pulses = 1 + elapsed / u64::from(channel.reload);
        channel.remaining = channel.reload - (elapsed % u64::from(channel.reload)) as u32;
        Ok(pulses)
    }
}
impl Channel {
    fn count(&self) -> u16 {
        if self.mode != 3 || !self.programmed {
            return self.remaining as u16;
        }
        let phase = self.reload - self.remaining;
        let high_ticks = self.reload.div_ceil(2);
        if phase == 0 {
            self.reload as u16
        } else if phase < high_ticks {
            ((high_ticks - phase) * 2) as u16
        } else {
            ((self.reload - phase) * 2) as u16
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batched_ticks_and_latched_reads_keep_counter_phase() {
        let mut pit = Pit::default();
        pit.write_control(0x34).unwrap();
        pit.write_counter(0, 100).unwrap();
        pit.write_counter(0, 0).unwrap();
        assert_eq!(pit.advance(0, 250).unwrap(), 2);
        pit.write_control(0).unwrap();
        pit.advance(0, 20).unwrap();
        assert_eq!(pit.read_counter(0).unwrap(), 50);
        assert_eq!(pit.read_counter(0).unwrap(), 0);
        assert_eq!(pit.read_counter(0).unwrap(), 30);
    }
    #[test]
    fn zero_reload_means_65536_and_unsupported_modes_are_reported() {
        let mut pit = Pit::default();
        pit.write_control(0x34).unwrap();
        pit.write_counter(0, 0).unwrap();
        pit.write_counter(0, 0).unwrap();
        assert_eq!(pit.advance(0, 65536).unwrap(), 1);
        assert!(pit.write_control(0x30).is_err());
    }
    #[test]
    fn square_wave_counter_even_odd_halves_and_latch_follow_guest_ticks() {
        let mut pit = Pit::default();
        pit.write_control(0x16).unwrap(); // Mode3, low-byte access.
        pit.write_counter(0, 5).unwrap();
        for expected in [5, 4, 2, 4, 2] {
            assert_eq!(pit.read_counter(0), Ok(expected));
            pit.advance(0, 1).unwrap();
        }
        assert_eq!(pit.read_counter(0), Ok(5));
        pit.write_counter(0, 6).unwrap();
        for expected in [6, 4, 2, 6, 4, 2] {
            assert_eq!(pit.read_counter(0), Ok(expected));
            pit.advance(0, 1).unwrap();
        }
        assert_eq!(pit.advance(0, 18), Ok(3));
    }
}
