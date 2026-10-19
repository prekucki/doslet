// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct OplTimer {
    #[prost(uint32, tag = "1")]
    preset: u32,
    #[prost(bool, tag = "2")]
    running: bool,
    #[prost(bool, tag = "3")]
    masked: bool,
    #[prost(bool, tag = "4")]
    overflow: bool,
    #[prost(uint64, tag = "5")]
    deadline: u64,
}
impl State for Timer {
    type Wire = OplTimer;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            preset: u32::from(self.preset),
            running: self.running,
            masked: self.masked,
            overflow: self.overflow,
            deadline: self.deadline,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            preset: doslet_state::narrow(w.preset, "OplTimer.preset")?,
            running: w.running,
            masked: w.masked,
            overflow: w.overflow,
            deadline: w.deadline,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct OplOpl3 {
    #[prost(message, optional, tag = "1")]
    chip: Option<<nuked_opl3::Opl3Chip as State>::Wire>,
    #[prost(bytes = "vec", tag = "2")]
    address: Vec<u8>,
    #[prost(message, repeated, tag = "3")]
    timers: Vec<<Timer as State>::Wire>,
    #[prost(bool, tag = "4")]
    started: bool,
    #[prost(uint64, tag = "5")]
    fraction: u64,
    #[prost(uint64, tag = "6")]
    key_on_writes: u64,
    #[prost(uint32, tag = "7")]
    peak: u32,
}
impl State for Opl3 {
    type Wire = OplOpl3;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            chip: Some(State::save(&self.chip)),
            address: self.address.to_vec(),
            timers: self.timers.iter().map(State::save).collect(),
            started: self.started,
            fraction: self.fraction,
            key_on_writes: self.key_on_writes,
            peak: u32::from(self.peak),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            chip: <nuked_opl3::Opl3Chip as State>::load(doslet_state::required(
                w.chip,
                "OplOpl3.chip",
            )?)?,
            address: doslet_state::narrow(w.address, "OplOpl3.address length")?,
            timers: doslet_state::narrow(
                w.timers
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { <Timer as State>::load(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "OplOpl3.timers length",
            )?,
            started: w.started,
            fraction: w.fraction,
            key_on_writes: w.key_on_writes,
            peak: doslet_state::narrow(w.peak, "OplOpl3.peak")?,
        };
        if !(value.fraction < 1_000_000_000) {
            return Err(InvalidState("OplOpl3 invariants"));
        }
        Ok(value)
    }
}
