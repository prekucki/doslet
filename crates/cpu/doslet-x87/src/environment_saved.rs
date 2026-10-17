// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct X87Pointers {
    #[prost(uint32, tag = "1")]
    ip: u32,
    #[prost(uint32, tag = "2")]
    cs: u32,
    #[prost(uint32, tag = "3")]
    data: u32,
    #[prost(uint32, tag = "4")]
    ds: u32,
    #[prost(uint32, tag = "5")]
    opcode: u32,
}
impl State for Pointers {
    type Wire = X87Pointers;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            ip: self.ip,
            cs: u32::from(self.cs),
            data: self.data,
            ds: u32::from(self.ds),
            opcode: u32::from(self.opcode),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            ip: w.ip,
            cs: doslet_state::narrow(w.cs, "X87Pointers.cs")?,
            data: w.data,
            ds: doslet_state::narrow(w.ds, "X87Pointers.ds")?,
            opcode: doslet_state::narrow(w.opcode, "X87Pointers.opcode")?,
        };
        Ok(value)
    }
}
