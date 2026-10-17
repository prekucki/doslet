//! Independent CT1745 mixer registers and FM output levels.
#![no_std]

#[derive(Debug)]
pub struct Mixer {
    index: u8,
    registers: [u8; 256],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnsupportedRegister(pub u8);
impl Default for Mixer {
    fn default() -> Self {
        let mut mixer = Self {
            index: 0,
            registers: [0; 256],
        };
        mixer.reset_volumes();
        mixer.registers[0x80] = 4; // IRQ7.
        mixer.registers[0x81] = 0x22; // DMA1/DMA5.
        mixer
    }
}
impl Mixer {
    /// Apply the CT1745 master and FM levels independently to both channels.
    /// Each step below 31 attenuates by approximately 2 dB; zero mutes.
    pub fn fm_samples(&self, mut samples: [i16; 2]) -> [i16; 2] {
        for (channel, sample) in samples.iter_mut().enumerate() {
            let master = self.registers[0x30 + channel] >> 3;
            let fm = self.registers[0x34 + channel] >> 3;
            if master == 0 || fm == 0 {
                *sample = 0;
            } else {
                let mut gain = 32768_i64;
                for _ in 0..(62 - u16::from(master) - u16::from(fm)) {
                    gain = gain * 26029 / 32768;
                }
                *sample = (i64::from(*sample) * gain / 32768) as i16;
            }
        }
        samples
    }
    /// Machine builder selects the initial, already validated ISA resources.
    pub fn with_routing(irq: u8, dma8: u8, dma16: u8) -> Self {
        let mut mixer = Self::default();
        mixer.registers[0x80] = match irq {
            5 => 2,
            7 => 4,
            _ => 0,
        };
        mixer.registers[0x81] = 1u8.checked_shl(u32::from(dma8)).unwrap_or(0)
            | 1u8.checked_shl(u32::from(dma16)).unwrap_or(0);
        mixer
    }
    /// Decode without modifying registers so machine resource checks are atomic.
    pub fn proposed_routing(&self, value: u8) -> Option<(Option<u8>, Option<u8>, Option<u8>)> {
        if !matches!(self.index, 0x80 | 0x81) {
            return None;
        }
        let mut proposed = Self::default();
        proposed.registers[0x80] = self.registers[0x80];
        proposed.registers[0x81] = self.registers[0x81];
        proposed.index = self.index;
        proposed.write(value).ok()?;
        Some((
            proposed.interrupt_line(),
            proposed.dma_channel(false),
            proposed.dma_channel(true),
        ))
    }
    pub fn interrupt_line(&self) -> Option<u8> {
        match self.registers[0x80] {
            1 => Some(2),
            2 => Some(5),
            4 => Some(7),
            8 => Some(10),
            _ => None,
        }
    }
    pub fn dma_channel(&self, word: bool) -> Option<u8> {
        match self.registers[0x81] & if word { 0xE0 } else { 0x0B } {
            1 if !word => Some(0),
            2 if !word => Some(1),
            8 if !word => Some(3),
            0x20 if word => Some(5),
            0x40 if word => Some(6),
            0x80 if word => Some(7),
            _ => None,
        }
    }
    pub fn set_interrupt_status(&mut self, value: u8) {
        self.registers[0x82] = value & 7;
    }
    pub fn select(&mut self, index: u8) {
        self.index = index;
    }
    pub fn index(&self) -> u8 {
        self.index
    }
    fn reset_volumes(&mut self) {
        self.registers[0x30..=0x39].fill(0xD8);
        self.registers[0x3A] = 0;
        self.registers[0x3B] = 0xC0;
        self.registers[0x3C] = 0x1F;
        self.registers[0x3D] = 0x15;
        self.registers[0x3E] = 0x0B;
        self.registers[0x3F..=0x47].fill(0);
    }
    fn stereo_alias(index: u8) -> Option<usize> {
        match index {
            4 => Some(0x32),
            0x22 => Some(0x30),
            0x26 => Some(0x34),
            0x28 => Some(0x36),
            0x2E => Some(0x38),
            _ => None,
        }
    }
    pub fn read(&self) -> Result<u8, UnsupportedRegister> {
        if let Some(base) = Self::stereo_alias(self.index) {
            return Ok((self.registers[base] & 0xF0) | (self.registers[base + 1] >> 4));
        }
        match self.index {
            0 => Ok(0),
            0x30..=0x47 | 0x80..=0x82 => Ok(self.registers[usize::from(self.index)]),
            _ => Err(UnsupportedRegister(self.index)),
        }
    }
    pub fn write(&mut self, value: u8) -> Result<(), UnsupportedRegister> {
        if let Some(base) = Self::stereo_alias(self.index) {
            self.registers[base] = (value & 0xF0) | 8;
            self.registers[base + 1] = (value << 4) | 8;
            return Ok(());
        }
        let mask = match self.index {
            0 => {
                self.reset_volumes();
                return Ok(());
            }
            0x30..=0x3A => 0xF8,
            0x3B | 0x3F..=0x42 => 0xC0,
            0x3C => 0x1F,
            0x3D | 0x3E => 0x7F,
            0x43 => 1,
            0x44..=0x47 => 0xF0,
            0x80 => 15,
            0x81 => 0xEB,
            0x82 => return Ok(()), // Interrupt status is read-only; no source is active yet.
            _ => return Err(UnsupportedRegister(self.index)),
        };
        self.registers[usize::from(self.index)] = value & mask;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn master_and_fm_levels_attenuate_each_channel_and_zero_mutes() {
        let mut mixer = Mixer::default();
        for register in [0x30, 0x31, 0x34, 0x35] {
            mixer.select(register);
            mixer.write(0xF8).unwrap();
        }
        assert_eq!(mixer.fm_samples([10000, -10000]), [10000, -10000]);
        mixer.select(0x34);
        mixer.write(0xF0).unwrap();
        let samples = mixer.fm_samples([10000, -10000]);
        assert!((7900..8000).contains(&samples[0]));
        assert_eq!(samples[1], -10000);
        mixer.select(0x26);
        mixer.write(0).unwrap();
        assert!(
            mixer
                .fm_samples([10000, -10000])
                .iter()
                .all(|v| v.abs() <= 10)
        );
        for register in [0x34, 0x35] {
            mixer.select(register);
            mixer.write(0).unwrap();
        }
        assert_eq!(mixer.fm_samples([10000, -10000]), [0; 2]);
    }
    #[test]
    fn legacy_volume_aliases_and_reset_preserve_irq_dma_selection() {
        let mut mixer = Mixer::default();
        mixer.select(0x22);
        mixer.write(0xA3).unwrap();
        mixer.select(0x30);
        assert_eq!(mixer.read(), Ok(0xA8));
        mixer.select(0x31);
        assert_eq!(mixer.read(), Ok(0x38));
        mixer.select(0);
        mixer.write(0).unwrap();
        mixer.select(0x80);
        assert_eq!(mixer.read(), Ok(4));
        mixer.select(0x81);
        assert_eq!(mixer.read(), Ok(0x22));
        mixer.select(0x82);
        mixer.write(0xFF).unwrap();
        assert_eq!(mixer.read(), Ok(0));
        mixer.select(0xFF);
        assert_eq!(mixer.write(0), Err(UnsupportedRegister(0xFF)));
    }
}

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
