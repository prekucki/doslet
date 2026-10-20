// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct ConfigHardwareConfig {
    #[prost(uint32, tag = "1")]
    cpu_mhz: u32,
    #[prost(uint32, tag = "2")]
    memory_mib: u32,
    #[prost(bool, tag = "3")]
    x87: bool,
    #[prost(bool, tag = "4")]
    sb16: bool,
    #[prost(bool, tag = "5")]
    mpu401: bool,
    #[prost(uint32, tag = "6")]
    sb_irq: u32,
    #[prost(uint32, tag = "7")]
    sb_dma8: u32,
    #[prost(uint32, tag = "8")]
    sb_dma16: u32,
    #[prost(message, optional, tag = "9")]
    keyboard: Option<<KeyboardMode as State>::Wire>,
    #[prost(bool, tag = "10")]
    gameport: bool,
}
impl State for HardwareConfig {
    type Wire = ConfigHardwareConfig;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            cpu_mhz: u32::from(self.cpu_mhz),
            memory_mib: u32::from(self.memory_mib),
            x87: self.x87,
            sb16: self.sb16,
            mpu401: self.mpu401,
            sb_irq: u32::from(self.sb_irq),
            sb_dma8: u32::from(self.sb_dma8),
            sb_dma16: u32::from(self.sb_dma16),
            keyboard: Some(State::save(&self.keyboard)),
            gameport: self.gameport,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            cpu_mhz: doslet_state::narrow(w.cpu_mhz, "ConfigHardwareConfig.cpu_mhz")?,
            memory_mib: doslet_state::narrow(w.memory_mib, "ConfigHardwareConfig.memory_mib")?,
            x87: w.x87,
            sb16: w.sb16,
            mpu401: w.mpu401,
            sb_irq: doslet_state::narrow(w.sb_irq, "ConfigHardwareConfig.sb_irq")?,
            sb_dma8: doslet_state::narrow(w.sb_dma8, "ConfigHardwareConfig.sb_dma8")?,
            sb_dma16: doslet_state::narrow(w.sb_dma16, "ConfigHardwareConfig.sb_dma16")?,
            keyboard: <KeyboardMode as State>::load(doslet_state::required(
                w.keyboard,
                "ConfigHardwareConfig.keyboard",
            )?)?,
            gameport: w.gameport,
        };
        if !(value.validate().is_ok()) {
            return Err(InvalidState("ConfigHardwareConfig invariants"));
        }
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct ConfigKeyboardMode {
    #[prost(uint32, tag = "1")]
    value: u32,
}
impl State for KeyboardMode {
    type Wire = ConfigKeyboardMode;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            value: match self {
                Self::Bios => 0,
                Self::At => 1,
            },
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        match w.value {
            0 => Ok(Self::Bios),
            1 => Ok(Self::At),
            _ => Err(InvalidState("ConfigKeyboardMode tag")),
        }
    }
}
