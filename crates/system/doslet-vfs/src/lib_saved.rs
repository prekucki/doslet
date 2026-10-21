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
pub struct VfsHandleTableParent {
    #[prost(message, repeated, tag = "1")]
    handles: Vec<VfsHandleTableHandlesItem>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsHandleTable {
    #[prost(message, repeated, tag = "1")]
    handles: Vec<VfsHandleTableHandlesItem>,
    #[prost(message, repeated, tag = "2")]
    descriptions: Vec<VfsHandleTableDescriptionsItem>,
    /// Waiting parents' handles; absent in states without child processes.
    #[prost(message, repeated, tag = "3")]
    parents: Vec<VfsHandleTableParent>,
}

fn save_handles(handles: &[Option<u8>; HANDLE_COUNT]) -> Vec<VfsHandleTableHandlesItem> {
    handles
        .iter()
        .map(|v| VfsHandleTableHandlesItem {
            value: v.map(u32::from),
        })
        .collect()
}

fn load_handles(
    items: Vec<VfsHandleTableHandlesItem>,
) -> Result<[Option<u8>; HANDLE_COUNT], InvalidState> {
    doslet_state::narrow(
        items
            .into_iter()
            .map(|v| {
                v.value
                    .map(|v| doslet_state::narrow(v, "VfsHandleTable.handles"))
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?,
        "VfsHandleTable.handles length",
    )
}
impl State for HandleTable {
    type Wire = VfsHandleTable;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            handles: save_handles(&self.handles),
            parents: self.parents[..self.depth]
                .iter()
                .map(|handles| VfsHandleTableParent {
                    handles: save_handles(handles),
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
        if w.parents.len() > MAX_PROCESS_DEPTH {
            return Err(InvalidState("VfsHandleTable.parents length"));
        }
        let depth = w.parents.len();
        let mut parents = [[None; HANDLE_COUNT]; MAX_PROCESS_DEPTH];
        for (slot, parent) in parents.iter_mut().zip(w.parents) {
            *slot = load_handles(parent.handles)?;
        }
        let value = Self {
            handles: load_handles(w.handles)?,
            parents,
            depth,
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
        if !(core::iter::once(&value.handles)
            .chain(&value.parents[..value.depth])
            .flatten()
            .flatten()
            .all(|&i| {
                usize::from(i) < HANDLE_COUNT && value.descriptions[usize::from(i)].is_some()
            }))
        {
            return Err(InvalidState("VfsHandleTable invariants"));
        }
        Ok(value)
    }
}
