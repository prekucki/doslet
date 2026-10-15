//! Hardware settings supported by the PC builder.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyboardMode {
    #[default]
    Bios,
    At,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HardwareConfig {
    /// Nominal functional CPU clock; each execution step costs four clocks.
    pub cpu_mhz: u16,
    pub memory_mib: u8,
    pub x87: bool,
    pub sb16: bool,
    pub mpu401: bool,
    pub sb_irq: u8,
    pub sb_dma8: u8,
    pub sb_dma16: u8,
    pub keyboard: KeyboardMode,
    pub gameport: bool,
}

impl Default for HardwareConfig {
    fn default() -> Self {
        Self {
            cpu_mhz: 33,
            memory_mib: 16,
            x87: true,
            sb16: true,
            mpu401: true,
            sb_irq: 7,
            sb_dma8: 1,
            sb_dma16: 5,
            keyboard: KeyboardMode::Bios,
            gameport: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HardwareError {
    CpuClock,
    MemorySize,
    SoundIrq,
    SoundDma8,
    SoundDma16,
}

impl HardwareConfig {
    pub fn validate(self) -> Result<(), HardwareError> {
        if !(1..=1000).contains(&self.cpu_mhz) {
            return Err(HardwareError::CpuClock);
        }
        if !matches!(self.memory_mib, 4 | 8 | 16 | 32) {
            return Err(HardwareError::MemorySize);
        }
        if self.sb16 {
            if !matches!(self.sb_irq, 5 | 7) {
                return Err(HardwareError::SoundIrq);
            }
            if !matches!(self.sb_dma8, 0 | 1 | 3) {
                return Err(HardwareError::SoundDma8);
            }
            if !matches!(self.sb_dma16, 5..=7) {
                return Err(HardwareError::SoundDma16);
            }
        }
        Ok(())
    }

    pub const fn memory_bytes(self) -> usize {
        self.memory_mib as usize * 1024 * 1024
    }
}
