// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsMountId {
    #[prost(uint32, tag = "1")]
    value: u32,
}
impl State for MountId {
    type Wire = VfsMountId;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            value: u32::from(self.0),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self(doslet_state::narrow(w.value, "VfsMountId.0")?);
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsNodeId {
    #[prost(uint64, tag = "1")]
    value: u64,
}
impl State for NodeId {
    type Wire = VfsNodeId;
    fn save(&self) -> Self::Wire {
        Self::Wire { value: self.0 }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self(w.value);
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsOpenFile {
    #[prost(message, optional, tag = "1")]
    mount: Option<<MountId as State>::Wire>,
    #[prost(message, optional, tag = "2")]
    node: Option<<NodeId as State>::Wire>,
    #[prost(uint64, tag = "3")]
    position: u64,
    #[prost(uint32, tag = "4")]
    mode: u32,
}
impl State for OpenFile {
    type Wire = VfsOpenFile;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            mount: Some(State::save(&self.mount)),
            node: Some(State::save(&self.node)),
            position: self.position,
            mode: u32::from(self.mode),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            mount: <MountId as State>::load(doslet_state::required(w.mount, "VfsOpenFile.mount")?)?,
            node: <NodeId as State>::load(doslet_state::required(w.node, "VfsOpenFile.node")?)?,
            position: w.position,
            mode: doslet_state::narrow(w.mode, "VfsOpenFile.mode")?,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsHandleTableHandlesItem {
    #[prost(uint32, optional, tag = "1")]
    value: Option<u32>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsHandleTableDescriptionsItem {
    #[prost(message, optional, tag = "1")]
    value: Option<<OpenFile as State>::Wire>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsHandleTable {
    #[prost(message, repeated, tag = "1")]
    handles: Vec<VfsHandleTableHandlesItem>,
    #[prost(message, repeated, tag = "2")]
    descriptions: Vec<VfsHandleTableDescriptionsItem>,
}
impl State for HandleTable {
    type Wire = VfsHandleTable;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            handles: self
                .handles
                .iter()
                .map(|v| VfsHandleTableHandlesItem {
                    value: (v).as_ref().map(|v| u32::from(*(v))),
                })
                .collect(),
            descriptions: self
                .descriptions
                .iter()
                .map(|v| VfsHandleTableDescriptionsItem {
                    value: (v).as_ref().map(State::save),
                })
                .collect(),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            handles: doslet_state::narrow(
                w.handles
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        v.value
                            .map(|v| -> Result<_, InvalidState> {
                                doslet_state::narrow(v, "VfsHandleTable.handles")
                            })
                            .transpose()
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "VfsHandleTable.handles length",
            )?,
            descriptions: doslet_state::narrow(
                w.descriptions
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        v.value
                            .map(|v| -> Result<_, InvalidState> { <OpenFile as State>::load(v) })
                            .transpose()
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "VfsHandleTable.descriptions length",
            )?,
        };
        if !(value.handles.iter().flatten().all(|&i| {
            usize::from(i) < HANDLE_COUNT && value.descriptions[usize::from(i)].is_some()
        })) {
            return Err(InvalidState("VfsHandleTable invariants"));
        }
        Ok(value)
    }
}
