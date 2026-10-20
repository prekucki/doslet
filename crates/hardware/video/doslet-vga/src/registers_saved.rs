// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct VgaRegisters {
    #[prost(uint32, tag = "1")]
    misc: u32,
    #[prost(uint32, tag = "2")]
    sequencer_index: u32,
    #[prost(bytes = "vec", tag = "3")]
    sequencer: Vec<u8>,
    #[prost(uint32, tag = "4")]
    graphics_index: u32,
    #[prost(bytes = "vec", tag = "5")]
    graphics: Vec<u8>,
    #[prost(uint32, tag = "6")]
    crtc_index: u32,
    #[prost(bytes = "vec", tag = "7")]
    crtc: Vec<u8>,
    #[prost(bytes = "vec", tag = "8")]
    attribute: Vec<u8>,
    #[prost(uint32, tag = "9")]
    attribute_index: u32,
    #[prost(bool, tag = "10")]
    attribute_data: bool,
}
impl State for Registers {
    type Wire = VgaRegisters;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            misc: u32::from(self.misc),
            sequencer_index: u32::from(self.sequencer_index),
            sequencer: self.sequencer.to_vec(),
            graphics_index: u32::from(self.graphics_index),
            graphics: self.graphics.to_vec(),
            crtc_index: u32::from(self.crtc_index),
            crtc: self.crtc.to_vec(),
            attribute: self.attribute.to_vec(),
            attribute_index: u32::from(self.attribute_index),
            attribute_data: self.attribute_data,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            misc: doslet_state::narrow(w.misc, "VgaRegisters.misc")?,
            sequencer_index: doslet_state::narrow(
                w.sequencer_index,
                "VgaRegisters.sequencer_index",
            )?,
            sequencer: doslet_state::narrow(w.sequencer, "VgaRegisters.sequencer length")?,
            graphics_index: doslet_state::narrow(w.graphics_index, "VgaRegisters.graphics_index")?,
            graphics: doslet_state::narrow(w.graphics, "VgaRegisters.graphics length")?,
            crtc_index: doslet_state::narrow(w.crtc_index, "VgaRegisters.crtc_index")?,
            crtc: doslet_state::narrow(w.crtc, "VgaRegisters.crtc length")?,
            attribute: doslet_state::narrow(w.attribute, "VgaRegisters.attribute length")?,
            attribute_index: doslet_state::narrow(
                w.attribute_index,
                "VgaRegisters.attribute_index",
            )?,
            attribute_data: w.attribute_data,
        };
        Ok(value)
    }
}
