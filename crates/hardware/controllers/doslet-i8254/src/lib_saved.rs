// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct I8254Channel {
    #[prost(uint32, tag = "1")]
    reload: u32,
    #[prost(uint32, tag = "2")]
    remaining: u32,
    #[prost(uint32, tag = "3")]
    access: u32,
    #[prost(uint32, optional, tag = "4")]
    low_write: Option<u32>,
    #[prost(bool, tag = "5")]
    high_read: bool,
    #[prost(uint32, optional, tag = "6")]
    latched: Option<u32>,
    #[prost(bool, tag = "7")]
    programmed: bool,
    #[prost(uint32, tag = "8")]
    mode: u32,
    #[prost(bool, tag = "9")]
    gate: bool,
    #[prost(bool, tag = "10")]
    terminal: bool,
    #[prost(bool, tag = "11")]
    load_pending: bool,
}
impl State for Channel {
    type Wire = I8254Channel;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            reload: self.reload,
            remaining: self.remaining,
            access: u32::from(self.access),
            low_write: self.low_write.as_ref().map(|v| u32::from(*(v))),
            high_read: self.high_read,
            latched: self.latched.as_ref().map(|v| u32::from(*(v))),
            programmed: self.programmed,
            mode: u32::from(self.mode),
            gate: self.gate,
            terminal: self.terminal,
            load_pending: self.load_pending,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            reload: w.reload,
            remaining: w.remaining,
            access: doslet_state::narrow(w.access, "I8254Channel.access")?,
            low_write: w
                .low_write
                .map(|v| -> Result<_, InvalidState> {
                    doslet_state::narrow(v, "I8254Channel.low_write")
                })
                .transpose()?,
            high_read: w.high_read,
            latched: w
                .latched
                .map(|v| -> Result<_, InvalidState> {
                    doslet_state::narrow(v, "I8254Channel.latched")
                })
                .transpose()?,
            programmed: w.programmed,
            mode: doslet_state::narrow(w.mode, "I8254Channel.mode")?,
            gate: w.gate,
            terminal: w.terminal,
            load_pending: w.load_pending,
        };
        if !((1..=65536).contains(&value.reload)
            && value.remaining <= value.reload
            && (1..=3).contains(&value.access)
            && matches!(value.mode, 0 | 2 | 3))
        {
            return Err(InvalidState("I8254Channel invariants"));
        }
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct I8254Pit {
    #[prost(message, repeated, tag = "1")]
    channels: Vec<<Channel as State>::Wire>,
}
impl State for Pit {
    type Wire = I8254Pit;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            channels: self.channels.iter().map(State::save).collect(),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            channels: doslet_state::narrow(
                w.channels
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { <Channel as State>::load(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "I8254Pit.channels length",
            )?,
        };
        Ok(value)
    }
}
