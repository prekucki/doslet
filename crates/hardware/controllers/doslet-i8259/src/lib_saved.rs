// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct I8259Pic {
    #[prost(uint32, tag = "1")]
    pending: u32,
    #[prost(uint32, tag = "2")]
    mask: u32,
    #[prost(uint32, tag = "3")]
    in_service: u32,
    #[prost(uint32, tag = "4")]
    vector_base: u32,
    #[prost(uint32, tag = "5")]
    init_word: u32,
    #[prost(bool, tag = "6")]
    needs_icw4: bool,
    #[prost(bool, tag = "7")]
    single: bool,
    #[prost(bool, tag = "8")]
    read_isr: bool,
}
impl State for Pic {
    type Wire = I8259Pic;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            pending: u32::from(self.pending),
            mask: u32::from(self.mask),
            in_service: u32::from(self.in_service),
            vector_base: u32::from(self.vector_base),
            init_word: u32::from(self.init_word),
            needs_icw4: self.needs_icw4,
            single: self.single,
            read_isr: self.read_isr,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            pending: doslet_state::narrow(w.pending, "I8259Pic.pending")?,
            mask: doslet_state::narrow(w.mask, "I8259Pic.mask")?,
            in_service: doslet_state::narrow(w.in_service, "I8259Pic.in_service")?,
            vector_base: doslet_state::narrow(w.vector_base, "I8259Pic.vector_base")?,
            init_word: doslet_state::narrow(w.init_word, "I8259Pic.init_word")?,
            needs_icw4: w.needs_icw4,
            single: w.single,
            read_isr: w.read_isr,
        };
        Ok(value)
    }
}
