//! Hardware settings supported by the PC builder.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyboardMode {
    #[default]
    Bios,
    At,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SoundCard {
    None,
    Adlib,
    Sb,
    #[default]
    Sb16,
}

impl SoundCard {
    pub const ALL: [Self; 4] = [Self::None, Self::Adlib, Self::Sb, Self::Sb16];

    pub const fn has_fm(self) -> bool {
        !matches!(self, Self::None)
    }

    pub const fn has_opl3(self) -> bool {
        matches!(self, Self::Sb16)
    }

    pub const fn has_dsp(self) -> bool {
        matches!(self, Self::Sb | Self::Sb16)
    }

    pub const fn has_dma16(self) -> bool {
        matches!(self, Self::Sb16)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Adlib => "adlib",
            Self::Sb => "sb",
            Self::Sb16 => "sb16",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "none" | "off" => Some(Self::None),
            "adlib" => Some(Self::Adlib),
            "sb" | "sb2" | "soundblaster" => Some(Self::Sb),
            "sb16" => Some(Self::Sb16),
            _ => None,
        }
    }
}

impl core::fmt::Display for SoundCard {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::None => "Off",
            Self::Adlib => "AdLib",
            Self::Sb => "Sound Blaster",
            Self::Sb16 => "Sound Blaster 16",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioHardware {
    pub sb16: bool,
    pub sound_card: SoundCard,
    pub mpu401: bool,
    pub irq: u8,
    pub dma8: u8,
    pub dma16: u8,
}

impl AudioHardware {
    pub const fn sound(self) -> SoundCard {
        if !self.sb16 {
            SoundCard::None
        } else if matches!(self.sound_card, SoundCard::None) {
            SoundCard::Sb16
        } else {
            self.sound_card
        }
    }

    pub fn set_sound_card(&mut self, card: SoundCard) {
        self.sound_card = card;
        self.sb16 = !matches!(card, SoundCard::None);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HardwareConfig {
    /// Nominal functional CPU clock; each execution step costs four clocks.
    pub cpu_mhz: u16,
    pub memory_mib: u8,
    pub x87: bool,
    pub sb16: bool,
    pub sound_card: SoundCard,
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
            sound_card: SoundCard::Sb16,
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
    pub const fn sound(self) -> SoundCard {
        if !self.sb16 {
            SoundCard::None
        } else if matches!(self.sound_card, SoundCard::None) {
            SoundCard::Sb16
        } else {
            self.sound_card
        }
    }

    pub fn set_sound_card(&mut self, card: SoundCard) {
        self.sound_card = card;
        self.sb16 = !matches!(card, SoundCard::None);
    }

    pub const fn with_sound_card(mut self, card: SoundCard) -> Self {
        self.sound_card = card;
        self.sb16 = !matches!(card, SoundCard::None);
        self
    }

    pub const fn audio(self) -> AudioHardware {
        let sound_card = self.sound();
        AudioHardware {
            sb16: !matches!(sound_card, SoundCard::None),
            sound_card,
            mpu401: self.mpu401,
            irq: self.sb_irq,
            dma8: self.sb_dma8,
            dma16: self.sb_dma16,
        }
    }
    pub const fn with_audio(mut self, audio: AudioHardware) -> Self {
        let sound_card = audio.sound();
        self.sb16 = !matches!(sound_card, SoundCard::None);
        self.sound_card = sound_card;
        self.mpu401 = audio.mpu401;
        self.sb_irq = audio.irq;
        self.sb_dma8 = audio.dma8;
        self.sb_dma16 = audio.dma16;
        self
    }
    pub fn validate(self) -> Result<(), HardwareError> {
        if !(1..=1000).contains(&self.cpu_mhz) {
            return Err(HardwareError::CpuClock);
        }
        if !matches!(self.memory_mib, 4 | 8 | 16 | 32) {
            return Err(HardwareError::MemorySize);
        }
        let sound = self.sound();
        if sound.has_dsp() {
            if !matches!(self.sb_irq, 5 | 7) {
                return Err(HardwareError::SoundIrq);
            }
            if !matches!(self.sb_dma8, 0 | 1 | 3) {
                return Err(HardwareError::SoundDma8);
            }
            if sound.has_dma16() && !matches!(self.sb_dma16, 5..=7) {
                return Err(HardwareError::SoundDma16);
            }
        }
        Ok(())
    }

    pub const fn memory_bytes(self) -> usize {
        self.memory_mib as usize * 1024 * 1024
    }
}

#[cfg(feature = "save-state")]
#[path = "hardware_saved.rs"]
mod saved_state;
