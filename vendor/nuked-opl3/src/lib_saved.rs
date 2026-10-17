// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct OplCoreOpl3Chip {
    #[prost(message, optional, tag = "1")]
    chip: Option<<core::Chip as State>::Wire>,
}
impl State for Opl3Chip {
    type Wire = OplCoreOpl3Chip;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            chip: Some(State::save(&self.chip)),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            chip: <core::Chip as State>::load(doslet_state::required(
                w.chip,
                "OplCoreOpl3Chip.chip",
            )?)?,
        };
        Ok(value)
    }
}
