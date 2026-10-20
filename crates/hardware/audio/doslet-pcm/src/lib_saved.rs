// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct PcmFrame {
    #[prost(uint64, tag = "1")]
    nanos: u64,
    #[prost(uint32, tag = "2")]
    duration: u32,
    #[prost(sint32, repeated, tag = "3")]
    samples: Vec<i32>,
}
impl State for Frame {
    type Wire = PcmFrame;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            nanos: self.nanos,
            duration: self.duration,
            samples: self.samples.iter().map(|v| i32::from(*(v))).collect(),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            nanos: w.nanos,
            duration: w.duration,
            samples: doslet_state::narrow(
                w.samples
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        doslet_state::narrow(v, "PcmFrame.samples")
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "PcmFrame.samples length",
            )?,
        };
        Ok(value)
    }
}
