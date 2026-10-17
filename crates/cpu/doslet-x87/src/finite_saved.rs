// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct X87Finite {
    #[prost(bool, tag = "1")]
    negative: bool,
    #[prost(sint32, tag = "2")]
    exponent: i32,
    #[prost(uint64, tag = "3")]
    mantissa: u64,
}
impl State for Finite {
    type Wire = X87Finite;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            negative: self.negative,
            exponent: self.exponent,
            mantissa: self.mantissa,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            negative: w.negative,
            exponent: w.exponent,
            mantissa: w.mantissa,
        };
        Ok(value)
    }
}
