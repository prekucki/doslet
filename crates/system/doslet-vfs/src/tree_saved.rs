// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsTreeName {
    #[prost(bytes = "vec", tag = "1")]
    bytes: Vec<u8>,
    #[prost(uint64, tag = "2")]
    len: u64,
}
impl State for Name {
    type Wire = VfsTreeName;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            bytes: self.bytes.to_vec(),
            len: self.len as u64,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            bytes: doslet_state::narrow(w.bytes, "VfsTreeName.bytes length")?,
            len: doslet_state::narrow(w.len, "VfsTreeName.len")?,
        };
        if !(value.len < NAME_BYTES) {
            return Err(InvalidState("VfsTreeName invariants"));
        }
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsTreeFile {
    #[prost(message, optional, tag = "6")]
    resource: Option<doslet_state::ResourceReference>,
    #[prost(message, optional, tag = "1")]
    name: Option<<Name as State>::Wire>,
    #[prost(bytes = "vec", tag = "2")]
    bytes: Vec<u8>,
    #[prost(message, optional, tag = "3")]
    base_name: Option<<Name as State>::Wire>,
    #[prost(bool, tag = "4")]
    deleted: bool,
    #[prost(bool, tag = "5")]
    directory: bool,
}
impl File {
    fn save_file(&self, references: bool) -> VfsTreeFile {
        VfsTreeFile {
            resource: (references && !self.bytes.is_empty()).then(|| {
                doslet_state::ResourceReference {
                    kind: doslet_state::ResourceKind::DirectoryFile as i32,
                    drive: u32::from(b'C'),
                    path: self.base_name.as_bytes().to_vec(),
                    length: self.bytes.len() as u64,
                    checksum: doslet_storage::payload_digest(&self.bytes),
                }
            }),
            name: Some(State::save(&self.name)),
            bytes: if references {
                Vec::new()
            } else {
                self.bytes.to_vec()
            },
            base_name: Some(State::save(&self.base_name)),
            deleted: self.deleted,
            directory: self.directory,
        }
    }
    fn load_file(mut w: VfsTreeFile, media: Option<&DirectoryTree>) -> Result<Self, InvalidState> {
        let resource = w.resource.take();
        let mut value = Self {
            name: <Name as State>::load(doslet_state::required(w.name, "VfsTreeFile.name")?)?,
            bytes: FileBytes::from(w.bytes),
            base_name: <Name as State>::load(doslet_state::required(
                w.base_name,
                "VfsTreeFile.base_name",
            )?)?,
            deleted: w.deleted,
            directory: w.directory,
        };
        if let Some(r) = resource {
            if r.kind != doslet_state::ResourceKind::DirectoryFile as i32
                || r.drive != u32::from(b'C')
                || r.path != value.base_name.as_bytes()
                || !value.bytes.is_empty()
            {
                return Err(InvalidState("invalid directory resource reference"));
            }
            let file = media
                .and_then(|m| m.files.iter().find(|f| f.base_name.as_bytes() == r.path))
                .ok_or(InvalidState(
                    "missing directory resource; supply the game configuration",
                ))?;
            if file.bytes.len() as u64 != r.length
                || doslet_storage::payload_digest(&file.bytes) != r.checksum
            {
                return Err(InvalidState("directory resource content mismatch"));
            }
            value.bytes = file.bytes.clone();
        }
        Ok(value)
    }
}
impl State for File {
    type Wire = VfsTreeFile;
    fn save(&self) -> Self::Wire {
        self.save_file(false)
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        Self::load_file(w, None)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsTreeHandle {
    #[prost(uint64, tag = "1")]
    file: u64,
    #[prost(uint32, tag = "2")]
    position: u32,
    #[prost(uint32, tag = "3")]
    mode: u32,
}
impl State for Handle {
    type Wire = VfsTreeHandle;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            file: self.file as u64,
            position: self.position,
            mode: u32::from(self.mode),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            file: doslet_state::narrow(w.file, "VfsTreeHandle.file")?,
            position: w.position,
            mode: doslet_state::narrow(w.mode, "VfsTreeHandle.mode")?,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsTreeDirectoryTreeHandlesItem {
    #[prost(uint64, optional, tag = "1")]
    value: Option<u64>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsTreeDirectoryTreeDescriptionsItem {
    #[prost(message, optional, tag = "1")]
    value: Option<<Handle as State>::Wire>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsTreeDirectoryTreeLastMidiRequestTuple {
    #[prost(message, optional, tag = "1")]
    item0: Option<<Name as State>::Wire>,
    #[prost(uint32, tag = "2")]
    item1: u32,
    #[prost(uint32, tag = "3")]
    item2: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VfsTreeDirectoryTree {
    #[prost(message, repeated, tag = "1")]
    files: Vec<<File as State>::Wire>,
    #[prost(message, repeated, tag = "2")]
    handles: Vec<VfsTreeDirectoryTreeHandlesItem>,
    #[prost(message, repeated, tag = "3")]
    descriptions: Vec<VfsTreeDirectoryTreeDescriptionsItem>,
    #[prost(bool, tag = "4")]
    namespace_dirty: bool,
    #[prost(message, optional, tag = "5")]
    current: Option<<Name as State>::Wire>,
    #[prost(uint64, tag = "6")]
    opens: u64,
    #[prost(uint64, tag = "7")]
    reads: u64,
    #[prost(uint64, optional, tag = "8")]
    last_midi: Option<u64>,
    #[prost(message, optional, tag = "9")]
    last_midi_request: Option<VfsTreeDirectoryTreeLastMidiRequestTuple>,
    #[prost(message, optional, tag = "10")]
    overlay: Option<<overlay::Overlay as State>::Wire>,
    #[prost(uint64, tag = "11")]
    new_file_limit: u64,
}
impl DirectoryTree {
    pub fn save_referenced(&self) -> VfsTreeDirectoryTree {
        self.save_tree(true)
    }
    fn save_tree(&self, references: bool) -> VfsTreeDirectoryTree {
        VfsTreeDirectoryTree {
            files: self.files.iter().map(|f| f.save_file(references)).collect(),
            handles: self
                .handles
                .iter()
                .map(|v| VfsTreeDirectoryTreeHandlesItem {
                    value: (v).as_ref().map(|v| *(v) as u64),
                })
                .collect(),
            descriptions: self
                .descriptions
                .iter()
                .map(|v| VfsTreeDirectoryTreeDescriptionsItem {
                    value: (v).as_ref().map(State::save),
                })
                .collect(),
            namespace_dirty: self.namespace_dirty,
            current: Some(State::save(&self.current)),
            opens: self.opens,
            reads: self.reads,
            last_midi: self.last_midi.as_ref().map(|v| *(v) as u64),
            last_midi_request: self.last_midi_request.as_ref().map(|v| {
                VfsTreeDirectoryTreeLastMidiRequestTuple {
                    item0: Some(State::save(&(v).0)),
                    item1: u32::from((v).1),
                    item2: u32::from((v).2),
                }
            }),
            overlay: self.overlay.as_ref().map(State::save),
            new_file_limit: self.new_file_limit as u64,
        }
    }
    pub fn load_referenced(
        w: VfsTreeDirectoryTree,
        media: Option<&Self>,
    ) -> Result<Self, InvalidState> {
        let value = Self {
            files: w
                .files
                .into_iter()
                .map(|v| -> Result<_, InvalidState> { File::load_file(v, media) })
                .collect::<Result<Vec<_>, _>>()?,
            handles: doslet_state::narrow(
                w.handles
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        v.value
                            .map(|v| -> Result<_, InvalidState> {
                                doslet_state::narrow(v, "VfsTreeDirectoryTree.handles")
                            })
                            .transpose()
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "VfsTreeDirectoryTree.handles length",
            )?,
            descriptions: doslet_state::narrow(
                w.descriptions
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        v.value
                            .map(|v| -> Result<_, InvalidState> { <Handle as State>::load(v) })
                            .transpose()
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "VfsTreeDirectoryTree.descriptions length",
            )?,
            namespace_dirty: w.namespace_dirty,
            current: <Name as State>::load(doslet_state::required(
                w.current,
                "VfsTreeDirectoryTree.current",
            )?)?,
            opens: w.opens,
            reads: w.reads,
            last_midi: w
                .last_midi
                .map(|v| -> Result<_, InvalidState> {
                    doslet_state::narrow(v, "VfsTreeDirectoryTree.last_midi")
                })
                .transpose()?,
            last_midi_request: w
                .last_midi_request
                .map(|v| -> Result<_, InvalidState> {
                    Ok({
                        let v = doslet_state::required(
                            Some(v),
                            "VfsTreeDirectoryTree.last_midi_request",
                        )?;
                        (
                            <Name as State>::load(doslet_state::required(
                                v.item0,
                                "VfsTreeDirectoryTree.last_midi_request.item0",
                            )?)?,
                            doslet_state::narrow(
                                v.item1,
                                "VfsTreeDirectoryTree.last_midi_request.item1",
                            )?,
                            doslet_state::narrow(
                                v.item2,
                                "VfsTreeDirectoryTree.last_midi_request.item2",
                            )?,
                        )
                    })
                })
                .transpose()?,
            overlay: w
                .overlay
                .map(|v| -> Result<_, InvalidState> { <overlay::Overlay as State>::load(v) })
                .transpose()?,
            new_file_limit: doslet_state::narrow(
                w.new_file_limit,
                "VfsTreeDirectoryTree.new_file_limit",
            )?,
        };
        if !(value
            .handles
            .iter()
            .flatten()
            .all(|&i| i < HANDLE_COUNT && value.descriptions[i].is_some())
            && value
                .descriptions
                .iter()
                .flatten()
                .all(|h| h.file < value.files.len())
            && value.new_file_limit <= value.files.len() + 64)
        {
            return Err(InvalidState("VfsTreeDirectoryTree invariants"));
        }
        let mut value = value;
        value
            .files
            .reserve(value.new_file_limit.saturating_sub(value.files.len()));
        Ok(value)
    }
}

impl State for DirectoryTree {
    type Wire = VfsTreeDirectoryTree;
    fn save(&self) -> Self::Wire {
        self.save_tree(false)
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        Self::load_referenced(w, None)
    }
}
