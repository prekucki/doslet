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
}
impl State for Mixer {
    type Wire = SbMixerMixer;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            index: u32::from(self.index),
            registers: self.registers.to_vec(),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            index: doslet_state::narrow(w.index, "SbMixerMixer.index")?,
            registers: doslet_state::narrow(w.registers, "SbMixerMixer.registers length")?,
        };
        Ok(value)
    }
}
