// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct CmosCmos {
    #[prost(bytes = "vec", tag = "1")]
    registers: Vec<u8>,
    #[prost(uint32, tag = "2")]
    selected: u32,
}
impl State for Cmos {
    type Wire = CmosCmos;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            registers: self.registers.to_vec(),
            selected: u32::from(self.selected),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            registers: doslet_state::narrow(w.registers, "CmosCmos.registers length")?,
            selected: doslet_state::narrow(w.selected, "CmosCmos.selected")?,
        };
        Ok(value)
    }
}
