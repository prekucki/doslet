// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct I8237Channel {
    #[prost(uint32, tag = "1")]
    base_address: u32,
    #[prost(uint32, tag = "2")]
    address: u32,
    #[prost(uint32, tag = "3")]
    base_count: u32,
    #[prost(uint32, tag = "4")]
    count: u32,
    #[prost(uint32, tag = "5")]
    mode: u32,
}
impl State for Channel {
    type Wire = I8237Channel;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            base_address: u32::from(self.base_address),
            address: u32::from(self.address),
            base_count: u32::from(self.base_count),
            count: u32::from(self.count),
            mode: u32::from(self.mode),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            base_address: doslet_state::narrow(w.base_address, "I8237Channel.base_address")?,
            address: doslet_state::narrow(w.address, "I8237Channel.address")?,
            base_count: doslet_state::narrow(w.base_count, "I8237Channel.base_count")?,
            count: doslet_state::narrow(w.count, "I8237Channel.count")?,
            mode: doslet_state::narrow(w.mode, "I8237Channel.mode")?,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct I8237Dma {
    #[prost(message, repeated, tag = "1")]
    channels: Vec<<Channel as State>::Wire>,
    #[prost(bool, tag = "2")]
    high: bool,
    #[prost(uint32, tag = "3")]
    mask: u32,
    #[prost(uint32, tag = "4")]
    command: u32,
    #[prost(uint32, tag = "5")]
    software_request: u32,
    #[prost(uint32, tag = "6")]
    dreq: u32,
    #[prost(uint32, tag = "7")]
    terminal: u32,
}
impl State for Dma {
    type Wire = I8237Dma;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            channels: self.channels.iter().map(State::save).collect(),
            high: self.high,
            mask: u32::from(self.mask),
            command: u32::from(self.command),
            software_request: u32::from(self.software_request),
            dreq: u32::from(self.dreq),
            terminal: u32::from(self.terminal),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            channels: doslet_state::narrow(
                w.channels
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { <Channel as State>::load(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "I8237Dma.channels length",
            )?,
            high: w.high,
            mask: doslet_state::narrow(w.mask, "I8237Dma.mask")?,
            command: doslet_state::narrow(w.command, "I8237Dma.command")?,
            software_request: doslet_state::narrow(
                w.software_request,
                "I8237Dma.software_request",
            )?,
            dreq: doslet_state::narrow(w.dreq, "I8237Dma.dreq")?,
            terminal: doslet_state::narrow(w.terminal, "I8237Dma.terminal")?,
        };
        if !(value.mask < 16
            && value.software_request < 16
            && value.dreq < 16
            && value.terminal < 16)
        {
            return Err(InvalidState("I8237Dma invariants"));
        }
        Ok(value)
    }
}
