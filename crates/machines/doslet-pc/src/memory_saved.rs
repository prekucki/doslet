// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct PcA20Gate {
    #[prost(bool, tag = "1")]
    enabled: bool,
    #[prost(uint64, tag = "2")]
    generation: u64,
}
impl State for A20Gate {
    type Wire = PcA20Gate;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            enabled: self.enabled,
            generation: self.generation,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            enabled: w.enabled,
            generation: w.generation,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct PcRam {
    #[prost(bytes = "vec", tag = "1")]
    bytes: Vec<u8>,
}
impl State for Ram {
    type Wire = PcRam;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            bytes: self.bytes.to_vec(),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        if !matches!(w.bytes.len(), 4194304 | 8388608 | 16777216 | 33554432) {
            return Err(InvalidState("PcRam length"));
        }
        let value = {
            let mut value = Self::new(w.bytes.len());
            value.bytes = (w.bytes).into_boxed_slice();
            value
        };
        Ok(value)
    }
}
