// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct DosBlock {
    #[prost(uint32, tag = "1")]
    segment: u32,
    #[prost(uint32, tag = "2")]
    paragraphs: u32,
    #[prost(uint32, tag = "3")]
    owner: u32,
}
impl State for Block {
    type Wire = DosBlock;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            segment: u32::from(self.segment),
            paragraphs: u32::from(self.paragraphs),
            owner: u32::from(self.owner),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            segment: doslet_state::narrow(w.segment, "DosBlock.segment")?,
            paragraphs: doslet_state::narrow(w.paragraphs, "DosBlock.paragraphs")?,
            owner: doslet_state::narrow(w.owner, "DosBlock.owner")?,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DosMemoryArena {
    #[prost(message, repeated, tag = "1")]
    blocks: Vec<<Block as State>::Wire>,
    #[prost(uint64, tag = "2")]
    len: u64,
}
impl State for MemoryArena {
    type Wire = DosMemoryArena;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            blocks: self.blocks.iter().map(State::save).collect(),
            len: self.len as u64,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            blocks: doslet_state::narrow(
                w.blocks
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { <Block as State>::load(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "DosMemoryArena.blocks length",
            )?,
            len: doslet_state::narrow(w.len, "DosMemoryArena.len")?,
        };
        if !(value.len <= CAPACITY) {
            return Err(InvalidState("DosMemoryArena invariants"));
        }
        Ok(value)
    }
}
