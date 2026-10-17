// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct PcPcmQueue {
    #[prost(message, repeated, tag = "1")]
    frames: Vec<<Frame as State>::Wire>,
    #[prost(uint64, tag = "2")]
    head: u64,
    #[prost(uint64, tag = "3")]
    len: u64,
    #[prost(uint64, tag = "4")]
    generated: u64,
    #[prost(uint64, tag = "5")]
    nonzero: u64,
}
impl State for PcmQueue {
    type Wire = PcPcmQueue;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            frames: self.frames.iter().map(State::save).collect(),
            head: self.head as u64,
            len: self.len as u64,
            generated: self.generated,
            nonzero: self.nonzero,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            frames: (w
                .frames
                .into_iter()
                .map(|v| -> Result<_, InvalidState> { <Frame as State>::load(v) })
                .collect::<Result<Vec<_>, _>>()?)
            .into_boxed_slice(),
            head: doslet_state::narrow(w.head, "PcPcmQueue.head")?,
            len: doslet_state::narrow(w.len, "PcPcmQueue.len")?,
            generated: w.generated,
            nonzero: w.nonzero,
        };
        if !(value.frames.len() == 4096 && value.head < 4096 && value.len <= 4096) {
            return Err(InvalidState("PcPcmQueue invariants"));
        }
        Ok(value)
    }
}
