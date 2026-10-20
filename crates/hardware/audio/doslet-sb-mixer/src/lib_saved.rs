// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct SbMixerMixer {
    #[prost(uint32, tag = "1")]
    index: u32,
    #[prost(bytes = "vec", tag = "2")]
    registers: Vec<u8>,
    #[prost(bool, tag = "3")]
    pub(super) irq_enable_initialized: bool,
}
impl State for Mixer {
    type Wire = SbMixerMixer;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            index: u32::from(self.index),
            registers: self.registers.to_vec(),
            irq_enable_initialized: true,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let mut value = Self {
            index: doslet_state::narrow(w.index, "SbMixerMixer.index")?,
            registers: doslet_state::narrow(w.registers, "SbMixerMixer.registers length")?,
        };
        if !w.irq_enable_initialized {
            // Older states had no IRQ-enable register; preserve their unmasked behavior.
            value.registers[0x83] = 0x0F;
        }
        if value.registers[0x83] & 0xF0 != 0 {
            return Err(InvalidState("SbMixerMixer.irq_enable"));
        }
        Ok(value)
    }
}
