//! Local immutable media is embedded, so a fixture has no host-path dependency.
use super::*;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeCdrom {
    #[prost(message, optional, tag = "4")]
    resource: Option<doslet_state::ResourceReference>,
    #[prost(uint32, tag = "1")]
    drive: u32,
    #[prost(bytes = "vec", tag = "2")]
    image: Vec<u8>,
    #[prost(message, repeated, tag = "3")]
    handles: Vec<CdromHandle>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct CdromHandle {
    #[prost(uint32, tag = "1")]
    slot: u32,
    #[prost(uint64, tag = "2")]
    data_start: u64,
    #[prost(uint64, tag = "3")]
    data_end: u64,
    #[prost(uint64, tag = "4")]
    record_start: u64,
    #[prost(uint64, tag = "5")]
    record_end: u64,
    #[prost(bool, tag = "6")]
    directory: bool,
    #[prost(uint32, tag = "7")]
    position: u32,
}

impl CdromImage {
    pub(in crate::session) fn portable(&self) -> bool {
        matches!(self.image, Media::Local(_))
            && self
                .handles
                .iter()
                .flatten()
                .all(|h| matches!(h.entry, FileEntry::Local(_)))
    }
}

impl CdromImage {
    pub(in crate::session) fn save_referenced(&self) -> RuntimeCdrom {
        self.save_cd(true)
    }
    fn save_cd(&self, references: bool) -> RuntimeCdrom {
        // Session::save_state checks this before constructing any saved buffers.
        let Media::Local(image) = &self.image else {
            unreachable!("portable media precondition")
        };
        RuntimeCdrom {
            drive: u32::from(self.drive),
            image: if references {
                Vec::new()
            } else {
                image.bytes().to_vec()
            },
            resource: references.then(|| doslet_state::ResourceReference {
                kind: doslet_state::ResourceKind::Iso9660 as i32,
                drive: u32::from(self.drive),
                path: Vec::new(),
                length: image.bytes().len() as u64,
                checksum: doslet_storage::payload_digest(image.bytes()),
            }),
            handles: self
                .handles
                .iter()
                .enumerate()
                .filter_map(|(slot, h)| {
                    let h = h.as_ref()?;
                    let FileEntry::Local(e) = &h.entry else {
                        unreachable!("portable handles")
                    };
                    Some(CdromHandle {
                        slot: slot as u32,
                        data_start: e.data.start as u64,
                        data_end: e.data.end as u64,
                        record_start: e.record.start as u64,
                        record_end: e.record.end as u64,
                        directory: e.directory,
                        position: h.position,
                    })
                })
                .collect(),
        }
    }

    pub(in crate::session) fn load_referenced(
        w: RuntimeCdrom,
        media: Option<&Self>,
    ) -> Result<Self, InvalidState> {
        let drive = doslet_state::narrow(w.drive, "CD drive")?;
        let mut cdrom = if let Some(r) = w.resource {
            let media = media.ok_or(InvalidState(
                "missing ISO resource; supply the game configuration",
            ))?;
            let Media::Local(image) = &media.image else {
                return Err(InvalidState("ISO resource is not local"));
            };
            if r.kind != doslet_state::ResourceKind::Iso9660 as i32
                || r.drive != w.drive
                || media.drive != drive
                || !r.path.is_empty()
                || !w.image.is_empty()
                || r.length != image.bytes().len() as u64
                || r.checksum != doslet_storage::payload_digest(image.bytes())
            {
                return Err(InvalidState("ISO resource content/type/drive mismatch"));
            }
            Self::attach(drive, Media::Local(image.clone()))
                .map_err(|_| InvalidState("CD drive"))?
        } else {
            Self::new(drive, w.image).map_err(|_| InvalidState("CD image"))?
        };
        let length = cdrom.length();
        for h in w.handles {
            let slot = h.slot as usize;
            if slot >= HANDLES
                || cdrom.handles[slot].is_some()
                || h.data_start > h.data_end
                || h.data_end > length
                || h.record_start > h.record_end
                || h.record_end > length
            {
                return Err(InvalidState("CD handle extent/slot"));
            }
            cdrom.handles[slot] = Some(Handle {
                entry: FileEntry::Local(Entry {
                    data: h.data_start as usize..h.data_end as usize,
                    record: h.record_start as usize..h.record_end as usize,
                    directory: h.directory,
                }),
                position: h.position,
            });
        }
        Ok(cdrom)
    }
}

impl State for CdromImage {
    type Wire = RuntimeCdrom;
    fn save(&self) -> Self::Wire {
        self.save_cd(false)
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        Self::load_referenced(w, None)
    }
}
