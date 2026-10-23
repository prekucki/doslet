//! Hardware changes occur only between batches, before the affected device's first I/O.
use crate::{MachineError, PcMachine};
use doslet_config::AudioHardware;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AudioUse {
    pub sb16: bool,
    pub opl3: bool,
    pub mpu401: bool,
}
#[derive(Debug)]
pub enum AudioConfigError {
    InUse(&'static str),
    RestartRequired(&'static str),
    Invalid(MachineError),
}
impl core::fmt::Display for AudioConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::RestartRequired(reason) => write!(f, "{reason}; restart to change it"),
            Self::InUse(device) => write!(
                f,
                "{device} has already been accessed by the guest; restart to change it"
            ),
            Self::Invalid(error) => write!(f, "{error}"),
        }
    }
}
impl PcMachine {
    pub fn audio_use(&self) -> AudioUse {
        self.bus.audio_use
    }
    pub fn configure_unused_audio(&mut self, audio: AudioHardware) -> Result<(), AudioConfigError> {
        if let Some(device) = self
            .audio_use()
            .change_blocker(self.hardware_config().audio(), audio)
        {
            return Err(AudioConfigError::InUse(device));
        }
        self.bus
            .configure_unused_audio(audio)
            .map_err(AudioConfigError::Invalid)
    }
}

impl AudioUse {
    pub fn change_blocker(self, old: AudioHardware, new: AudioHardware) -> Option<&'static str> {
        let old_sound = old.sound();
        let new_sound = new.sound();
        if old_sound != new_sound && (self.sb16 || self.opl3) {
            Some("SB16 / OPL3")
        } else if (old.irq != new.irq
            || old.dma8 != new.dma8
            || (old_sound.has_dma16() && old.dma16 != new.dma16))
            && self.sb16
        {
            Some("SB16 resources")
        } else if old.mpu401 != new.mpu401 && self.mpu401 {
            Some("MPU-401")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doslet_bus::{MemoryBus, PortBus};
    #[cfg(feature = "save-state")]
    use doslet_state::State;
    #[test]
    fn live_routes_and_resources_change_without_resetting_the_pc() {
        let mut m = PcMachine::default();
        m.bus.write_u8(0x2000, 0xa5).unwrap();
        let time = m.time();
        let mut audio = m.hardware_config().audio();
        audio.sb16 = false;
        audio.mpu401 = false;
        m.configure_unused_audio(audio).unwrap();
        assert!(m.bus.read_port_u8(0x22e).is_err());
        assert!(m.bus.read_port_u8(0x388).is_err());
        assert!(m.bus.read_port_u8(0x331).is_err());
        assert_eq!(m.audio_use(), AudioUse::default());
        audio.sb16 = true;
        audio.mpu401 = true;
        audio.irq = 5;
        audio.dma8 = 3;
        audio.dma16 = 6;
        m.configure_unused_audio(audio).unwrap();
        assert_eq!(m.audio_status().irq, Some(5));
        assert_eq!(m.audio_status().dma8, Some(3));
        assert_eq!(m.audio_status().dma16, Some(6));
        assert_eq!(m.bus.read_u8(0x2000), Ok(0xa5));
        assert_eq!(m.time(), time);
        assert!(m.bus.read_port_u8(0x331).is_ok());
        audio.mpu401 = false;
        assert!(matches!(
            m.configure_unused_audio(audio),
            Err(AudioConfigError::InUse("MPU-401"))
        ));
    }
    #[test]
    fn probing_one_device_does_not_lock_the_other_and_invalid_changes_are_atomic() {
        let mut m = PcMachine::default();
        m.bus.read_port_u8(0x388).unwrap();
        assert_eq!(
            m.audio_use(),
            AudioUse {
                opl3: true,
                ..Default::default()
            }
        );
        let mut audio = m.hardware_config().audio();
        audio.irq = 5;
        audio.mpu401 = false;
        m.configure_unused_audio(audio).unwrap();
        audio.sb16 = false;
        assert!(matches!(
            m.configure_unused_audio(audio),
            Err(AudioConfigError::InUse(_))
        ));
        audio.sb16 = true;
        audio.dma8 = 2;
        let before = m.hardware_config();
        assert!(matches!(
            m.configure_unused_audio(audio),
            Err(AudioConfigError::Invalid(_))
        ));
        assert_eq!(m.hardware_config(), before);
        assert_eq!(m.audio_status().irq, Some(5));
        m.bus.write_port_u8(0x224, 0x80).unwrap();
        audio = before.audio();
        audio.irq = 7;
        assert!(matches!(
            m.configure_unused_audio(audio),
            Err(AudioConfigError::InUse("SB16 resources"))
        ));
        m.configure_unused_audio(before.audio()).unwrap(); // No-op remains legal.
    }
    #[test]
    #[cfg(feature = "save-state")]
    fn saved_state_keeps_first_access_even_after_guest_reset() {
        let mut m = PcMachine::default();
        m.bus.write_port_u8(0x331, 0xff).unwrap();
        m.bus.write_port_u8(0x226, 1).unwrap();
        m.bus.write_port_u8(0x226, 0).unwrap();
        let restored = PcMachine::load(m.save()).unwrap();
        assert_eq!(
            restored.audio_use(),
            AudioUse {
                sb16: true,
                mpu401: true,
                opl3: false
            }
        );
        let mut untouched = PcMachine::load(PcMachine::default().save()).unwrap();
        let mut audio = untouched.hardware_config().audio();
        audio.sb16 = false;
        untouched.configure_unused_audio(audio).unwrap();
    }
}
