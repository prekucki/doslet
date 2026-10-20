// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsPatch {
    #[prost(uint64, tag = "1")]
    file: u64,
    #[prost(uint32, tag = "2")]
    position: u32,
    #[prost(uint64, tag = "3")]
    offset: u64,
    #[prost(uint64, tag = "4")]
    len: u64,
}
impl State for Patch {
    type Wire = VfsPatch;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            file: self.file as u64,
            position: self.position,
            offset: self.offset as u64,
            len: self.len as u64,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            file: doslet_state::narrow(w.file, "VfsPatch.file")?,
            position: w.position,
            offset: doslet_state::narrow(w.offset, "VfsPatch.offset")?,
            len: doslet_state::narrow(w.len, "VfsPatch.len")?,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsLength {
    #[prost(uint64, tag = "1")]
    file: u64,
    #[prost(uint32, tag = "2")]
    len: u32,
    #[prost(uint32, tag = "3")]
    base_visible: u32,
    #[prost(bool, tag = "4")]
    dirty: bool,
}
impl State for Length {
    type Wire = VfsLength;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            file: self.file as u64,
            len: self.len,
            base_visible: self.base_visible,
            dirty: self.dirty,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            file: doslet_state::narrow(w.file, "VfsLength.file")?,
            len: w.len,
            base_visible: w.base_visible,
            dirty: w.dirty,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsOverlayFsPatchesItem {
    #[prost(message, optional, tag = "1")]
    value: Option<<Patch as State>::Wire>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsOverlayFsLengthsItem {
    #[prost(message, optional, tag = "1")]
    value: Option<<Length as State>::Wire>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsOverlayFsPendingTuple {
    #[prost(uint64, tag = "1")]
    item0: u64,
    #[prost(uint32, tag = "2")]
    item1: u32,
    #[prost(uint64, tag = "3")]
    item2: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsOverlayFs {
    #[prost(bytes = "vec", tag = "1")]
    bytes: Vec<u8>,
    #[prost(uint64, tag = "2")]
    used: u64,
    #[prost(message, repeated, tag = "3")]
    patches: Vec<VfsOverlayFsPatchesItem>,
    #[prost(message, repeated, tag = "4")]
    lengths: Vec<VfsOverlayFsLengthsItem>,
    #[prost(bytes = "vec", tag = "5")]
    staging: Vec<u8>,
    #[prost(message, optional, tag = "6")]
    pending: Option<VfsOverlayFsPendingTuple>,
    #[prost(bool, tag = "7")]
    chunked: bool,
}
impl State for OverlayFs {
    type Wire = VfsOverlayFs;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            bytes: self.bytes.to_vec(),
            used: self.used as u64,
            patches: self
                .patches
                .iter()
                .map(|v| VfsOverlayFsPatchesItem {
                    value: (v).as_ref().map(State::save),
                })
                .collect(),
            lengths: self
                .lengths
                .iter()
                .map(|v| VfsOverlayFsLengthsItem {
                    value: (v).as_ref().map(State::save),
                })
                .collect(),
            staging: self.staging.to_vec(),
            pending: self.pending.as_ref().map(|v| VfsOverlayFsPendingTuple {
                item0: (v).0 as u64,
                item1: (v).1,
                item2: (v).2 as u64,
            }),
            chunked: self.chunked,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            bytes: (w.bytes).into_boxed_slice(),
            used: doslet_state::narrow(w.used, "VfsOverlayFs.used")?,
            patches: (w
                .patches
                .into_iter()
                .map(|v| -> Result<_, InvalidState> {
                    v.value
                        .map(|v| -> Result<_, InvalidState> { <Patch as State>::load(v) })
                        .transpose()
                })
                .collect::<Result<Vec<_>, _>>()?)
            .into_boxed_slice(),
            lengths: doslet_state::narrow(
                w.lengths
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        v.value
                            .map(|v| -> Result<_, InvalidState> { <Length as State>::load(v) })
                            .transpose()
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "VfsOverlayFs.lengths length",
            )?,
            staging: (w.staging).into_boxed_slice(),
            pending: w
                .pending
                .map(|v| -> Result<_, InvalidState> {
                    Ok({
                        let v = doslet_state::required(Some(v), "VfsOverlayFs.pending")?;
                        (
                            doslet_state::narrow(v.item0, "VfsOverlayFs.pending.item0")?,
                            v.item1,
                            doslet_state::narrow(v.item2, "VfsOverlayFs.pending.item2")?,
                        )
                    })
                })
                .transpose()?,
            chunked: w.chunked,
        };
        if !(value.used <= value.bytes.len()
            && value.staging.len() == 65536
            && value
                .patches
                .iter()
                .flatten()
                .all(|p| p.offset <= value.used && p.len <= value.used - p.offset)
            && value
                .pending
                .is_none_or(|(_, _, len)| len <= value.staging.len()))
        {
            return Err(InvalidState("VfsOverlayFs invariants"));
        }
        Ok(value)
    }
}
