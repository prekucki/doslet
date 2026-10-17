// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct MpuQueue {
    #[prost(bytes = "vec", tag = "1")]
    bytes: Vec<u8>,
    #[prost(uint64, tag = "2")]
    head: u64,
    #[prost(uint64, tag = "3")]
    len: u64,
}
impl<const N: usize> State for Queue<N> {
    type Wire = MpuQueue;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            bytes: self.bytes.to_vec(),
            head: self.head as u64,
            len: self.len as u64,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            bytes: doslet_state::narrow(w.bytes, "MpuQueue.bytes length")?,
            head: doslet_state::narrow(w.head, "MpuQueue.head")?,
            len: doslet_state::narrow(w.len, "MpuQueue.len")?,
        };
        if !(N != 0 && value.head < N && value.len <= N) {
            return Err(InvalidState("MpuQueue invariants"));
        }
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct MpuMpu401 {
    #[prost(bool, tag = "1")]
    uart: bool,
    #[prost(uint32, optional, tag = "2")]
    acknowledgement: Option<u32>,
    #[prost(message, optional, tag = "3")]
    output: Option<<Queue<1024> as State>::Wire>,
    #[prost(uint64, tag = "4")]
    commands: u64,
    #[prost(uint64, tag = "5")]
    transmitted: u64,
}
impl State for Mpu401 {
    type Wire = MpuMpu401;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            uart: self.uart,
            acknowledgement: self.acknowledgement.as_ref().map(|v| u32::from(*(v))),
            output: Some(State::save(&self.output)),
            commands: self.commands,
            transmitted: self.transmitted,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            uart: w.uart,
            acknowledgement: w
                .acknowledgement
                .map(|v| -> Result<_, InvalidState> {
                    doslet_state::narrow(v, "MpuMpu401.acknowledgement")
                })
                .transpose()?,
            output: <Queue<1024> as State>::load(doslet_state::required(
                w.output,
                "MpuMpu401.output",
            )?)?,
            commands: w.commands,
            transmitted: w.transmitted,
        };
        Ok(value)
    }
}
