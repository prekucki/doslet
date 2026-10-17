// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct TimeEmuTime {
    #[prost(uint64, tag = "1")]
    value: u64,
}
impl State for EmuTime {
    type Wire = TimeEmuTime;
    fn save(&self) -> Self::Wire {
        Self::Wire { value: self.0 }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self(w.value);
        Ok(value)
    }
}
