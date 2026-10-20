// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct I8042Pending {
    #[prost(uint32, tag = "1")]
    value: u32,
}
impl State for Pending {
    type Wire = I8042Pending;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            value: match self {
                Self::None => 0,
                Self::CommandByte => 1,
                Self::OutputPort => 2,
            },
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        match w.value {
            0 => Ok(Self::None),
            1 => Ok(Self::CommandByte),
            2 => Ok(Self::OutputPort),
            _ => Err(InvalidState("I8042Pending tag")),
        }
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct I8042Controller {
    #[prost(uint32, tag = "1")]
    command_byte: u32,
    #[prost(uint32, tag = "2")]
    output_port: u32,
    #[prost(message, optional, tag = "3")]
    pending: Option<<Pending as State>::Wire>,
    #[prost(bool, tag = "4")]
    last_command: bool,
    #[prost(message, optional, tag = "5")]
    protocol: Option<<doslet_at_keyboard::Keyboard as State>::Wire>,
    #[prost(uint64, tag = "6")]
    nanos: u64,
    #[prost(bytes = "vec", tag = "7")]
    output: Vec<u8>,
    #[prost(uint64, tag = "8")]
    head: u64,
    #[prost(uint64, tag = "9")]
    len: u64,
    #[prost(bytes = "vec", tag = "10")]
    releases: Vec<u8>,
    #[prost(uint64, tag = "11")]
    release_head: u64,
    #[prost(uint64, tag = "12")]
    release_len: u64,
}
impl State for Controller {
    type Wire = I8042Controller;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            command_byte: u32::from(self.command_byte),
            output_port: u32::from(self.output_port),
            pending: Some(State::save(&self.pending)),
            last_command: self.last_command,
            protocol: Some(State::save(&self.protocol)),
            nanos: self.nanos,
            output: self.output.to_vec(),
            head: self.head as u64,
            len: self.len as u64,
            releases: self.releases.to_vec(),
            release_head: self.release_head as u64,
            release_len: self.release_len as u64,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            command_byte: doslet_state::narrow(w.command_byte, "I8042Controller.command_byte")?,
            output_port: doslet_state::narrow(w.output_port, "I8042Controller.output_port")?,
            pending: <Pending as State>::load(doslet_state::required(
                w.pending,
                "I8042Controller.pending",
            )?)?,
            last_command: w.last_command,
            protocol: <doslet_at_keyboard::Keyboard as State>::load(doslet_state::required(
                w.protocol,
                "I8042Controller.protocol",
            )?)?,
            nanos: w.nanos,
            output: doslet_state::narrow(w.output, "I8042Controller.output length")?,
            head: doslet_state::narrow(w.head, "I8042Controller.head")?,
            len: doslet_state::narrow(w.len, "I8042Controller.len")?,
            releases: doslet_state::narrow(w.releases, "I8042Controller.releases length")?,
            release_head: doslet_state::narrow(w.release_head, "I8042Controller.release_head")?,
            release_len: doslet_state::narrow(w.release_len, "I8042Controller.release_len")?,
        };
        if !(value.head < 512
            && value.len <= 512
            && value.release_head < 512
            && value.release_len <= 512)
        {
            return Err(InvalidState("I8042Controller invariants"));
        }
        Ok(value)
    }
}
