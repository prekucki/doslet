//! On-disk native save stores, shared by the desktop store and offline tools.
//! One redb database holds the committed snapshot; version-1 directory stores
//! remain readable for a one-time import.
use doslet_storage::{StorageError, payload_digest};
use redb::{ReadOnlyDatabase, ReadableDatabase, TableDefinition};
use std::{fs::File, io::Read, path::Path};

pub const MAX_SNAPSHOT: usize = 16 * 1024 * 1024;
pub const MAX_LEDGER: usize = 4096;
pub const STORE_FILE: &str = "overlay.redb";
pub const SCHEMA: u64 = 1;
/// Schema version, writer fence, committed revision and its payload digest.
pub const HEAD: TableDefinition<&str, u64> = TableDefinition::new("head");
/// Save namespace and immutable base identity.
pub const IDENTITY: TableDefinition<&str, [u8; 32]> = TableDefinition::new("identity");
/// Acknowledged mutation ID to (payload digest, revision).
pub const LEDGER: TableDefinition<u64, (u64, u64)> = TableDefinition::new("ledger");
/// (revision, segment) to part of the versioned upper snapshot. Only the
/// committed revision is kept.
pub const SNAPSHOTS: TableDefinition<(u64, u32), &[u8]> = TableDefinition::new("snapshots");
/// redb allocates power-of-two page runs, so values just below 64 KiB fill
/// them. One 16-MiB value or exact 4-KiB values would occupy about twice that.
pub const SEGMENT: usize = 64 * 1024 - 64;

/// Version-1 directory store, read once to import it into the database.
#[derive(Clone, Debug)]
pub struct Manifest {
    pub namespace: [u8; 32],
    pub base: [u8; 32],
    pub fence: u64,
    pub revision: u64,
    pub digest: u64,
    pub ledger: Vec<(u64, u64, u64)>,
}
impl Manifest {
    #[cfg(test)]
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(100 + self.ledger.len() * 24);
        out.extend_from_slice(b"DSN1");
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&self.namespace);
        out.extend_from_slice(&self.base);
        for value in [self.fence, self.revision, self.digest] {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out.extend_from_slice(&(self.ledger.len() as u32).to_le_bytes());
        for &(id, digest, revision) in &self.ledger {
            for value in [id, digest, revision] {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        let digest = payload_digest(&out);
        out.extend_from_slice(&digest.to_le_bytes());
        out
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, StorageError> {
        if bytes.get(..4) != Some(b"DSN1") {
            return Err(StorageError::Corrupt);
        }
        if bytes.get(4..8) != Some(&1u32.to_le_bytes()) {
            return Err(StorageError::UnsupportedSchema);
        }
        if bytes.len() < 108 {
            return Err(StorageError::Corrupt);
        }
        let u64_at = |offset| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        let count = u32::from_le_bytes(bytes[96..100].try_into().unwrap()) as usize;
        if count > MAX_LEDGER
            || bytes.len() != 108 + count * 24
            || u64_at(bytes.len() - 8) != payload_digest(&bytes[..bytes.len() - 8])
        {
            return Err(StorageError::Corrupt);
        }
        let mut ledger = Vec::with_capacity(count);
        for i in 0..count {
            let start = 100 + i * 24;
            let entry = (u64_at(start), u64_at(start + 8), u64_at(start + 16));
            if entry.2 == 0
                || entry.2 > u64_at(80)
                || ledger.iter().any(|&(id, _, _)| id == entry.0)
            {
                return Err(StorageError::Corrupt);
            }
            ledger.push(entry);
        }
        Ok(Self {
            namespace: bytes[8..40].try_into().unwrap(),
            base: bytes[40..72].try_into().unwrap(),
            fence: u64_at(72),
            revision: u64_at(80),
            digest: u64_at(88),
            ledger,
        })
    }
}

/// The committed revision and, after the first commit, its snapshot.
pub fn committed(database: &impl ReadableDatabase) -> Result<(u64, Option<Vec<u8>>), StorageError> {
    let transaction = database.begin_read().map_err(db_error)?;
    let head = transaction.open_table(HEAD).map_err(db_error)?;
    let value = |key| -> Result<_, StorageError> {
        Ok(head
            .get(key)
            .map_err(db_error)?
            .map_or(0, |value| value.value()))
    };
    let revision = value("revision")?;
    if revision == 0 {
        return Ok((0, None));
    }
    let snapshots = transaction.open_table(SNAPSHOTS).map_err(db_error)?;
    let mut payload = Vec::new();
    for (index, entry) in snapshots
        .range((revision, 0)..=(revision, u32::MAX))
        .map_err(db_error)?
        .enumerate()
    {
        let (key, segment) = entry.map_err(db_error)?;
        let segment = segment.value();
        if key.value().1 as usize != index || payload.len() + segment.len() > MAX_SNAPSHOT {
            return Err(StorageError::Corrupt);
        }
        payload.extend_from_slice(segment);
    }
    if payload_digest(&payload) != value("digest")? {
        return Err(StorageError::Corrupt);
    }
    Ok((revision, Some(payload)))
}
/// Read-only copy of a store directory's committed snapshot, for offline
/// tools. A running desktop owner holds the database, which reports busy.
pub fn read_committed(directory: &Path) -> Result<Option<Vec<u8>>, StorageError> {
    let database = directory.join(STORE_FILE);
    if database.exists() {
        let database = ReadOnlyDatabase::open(database).map_err(db_error)?;
        return committed(&database).map(|(_, payload)| payload);
    }
    if directory.join("manifest.bin").exists() {
        return read_legacy(directory).map(|(_, payload)| payload);
    }
    Ok(None)
}
pub fn read_legacy(directory: &Path) -> Result<(Manifest, Option<Vec<u8>>), StorageError> {
    let bytes =
        bounded_read(&directory.join("manifest.bin"), 108 + MAX_LEDGER * 24).map_err(io_error)?;
    let manifest = Manifest::decode(&bytes)?;
    if manifest.revision == 0 {
        return Ok((manifest, None));
    }
    let name = format!(
        "snapshot-{:016x}-{:016x}.bin",
        manifest.revision, manifest.digest
    );
    let payload = bounded_read(&directory.join(name), MAX_SNAPSHOT).map_err(io_error)?;
    if payload_digest(&payload) != manifest.digest {
        return Err(StorageError::Corrupt);
    }
    Ok((manifest, Some(payload)))
}
pub fn db_error(error: impl Into<redb::Error>) -> StorageError {
    match error.into() {
        redb::Error::DatabaseAlreadyOpen => StorageError::BusyWriter,
        redb::Error::UpgradeRequired(_) => StorageError::UnsupportedSchema,
        redb::Error::ValueTooLarge(_) => StorageError::Quota,
        redb::Error::Corrupted(_)
        | redb::Error::TableTypeMismatch { .. }
        | redb::Error::TableIsMultimap(_)
        | redb::Error::TypeDefinitionChanged { .. } => StorageError::Corrupt,
        // redb reports a foreign file through its I/O error kind.
        redb::Error::Io(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            StorageError::Corrupt
        }
        redb::Error::Io(error) => io_error(error),
        _ => StorageError::Unavailable,
    }
}
pub fn io_error(error: std::io::Error) -> StorageError {
    match error.kind() {
        std::io::ErrorKind::StorageFull => StorageError::Quota,
        std::io::ErrorKind::NotFound => StorageError::Unavailable,
        _ => StorageError::Unavailable,
    }
}
pub fn bounded_read(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
    let file = File::open(path)?;
    if file.metadata()?.len() > limit as u64 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "save data exceeds reserved capacity",
        ));
    }
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "save data exceeds reserved capacity",
        ));
    }
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayFileKind {
    Unchanged,
    Created,
    Modified,
    Renamed,
    Deleted,
    Directory,
}

impl OverlayFileKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unchanged => "Base",
            Self::Created => "+ Created",
            Self::Modified => "~ Modified",
            Self::Renamed => "→ Renamed",
            Self::Deleted => "− Deleted",
            Self::Directory => "Folder",
        }
    }

    pub const fn is_changed(self) -> bool {
        !matches!(self, Self::Unchanged)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayFileEntry {
    pub path: String,
    pub base_path: Option<String>,
    pub kind: OverlayFileKind,
    pub size: Option<u32>,
    pub preview: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreSummary {
    pub revision: u64,
    pub used_bytes: usize,
    pub changed_count: usize,
    pub files: Vec<OverlayFileEntry>,
}

/// Inspect a persistent save store directory (`overlay.redb` or legacy `manifest.bin`)
/// and return a summary of all DOS files in the VFS tree (both base and overlay).
pub fn inspect_store(directory: &Path) -> Result<Option<StoreSummary>, StorageError> {
    let database = directory.join(STORE_FILE);
    if database.exists() {
        let database = ReadOnlyDatabase::open(database).map_err(db_error)?;
        let (revision, payload) = committed(&database)?;
        return match payload {
            Some(bytes) => inspect_dov1(revision, &bytes).map(Some),
            None => Ok(None),
        };
    }
    if directory.join("manifest.bin").exists() {
        let (manifest, payload) = read_legacy(directory)?;
        return match payload {
            Some(bytes) => inspect_dov1(manifest.revision, &bytes).map(Some),
            None => Ok(None),
        };
    }
    Ok(None)
}

/// Decode a `DOV1` VFS overlay snapshot into a human-inspectable summary.
pub fn inspect_dov1(revision: u64, payload: &[u8]) -> Result<StoreSummary, StorageError> {
    if payload.get(..4) != Some(b"DOV1") || payload.len() < 8 {
        return Err(StorageError::Corrupt);
    }
    let count = u32::from_le_bytes(payload[4..8].try_into().unwrap()) as usize;
    let mut cursor = 8usize;
    let mut raw_files = Vec::with_capacity(count.min(4096));
    for _ in 0..count {
        let base_len = *payload.get(cursor).ok_or(StorageError::Corrupt)? as usize;
        cursor += 1;
        let base_bytes = payload
            .get(cursor..cursor + base_len)
            .ok_or(StorageError::Corrupt)?;
        cursor += base_len;
        let name_len = *payload.get(cursor).ok_or(StorageError::Corrupt)? as usize;
        cursor += 1;
        let name_bytes = payload
            .get(cursor..cursor + name_len)
            .ok_or(StorageError::Corrupt)?;
        cursor += name_len;
        let flags = *payload.get(cursor).ok_or(StorageError::Corrupt)?;
        cursor += 1;
        raw_files.push((
            String::from_utf8_lossy(base_bytes).into_owned(),
            String::from_utf8_lossy(name_bytes).into_owned(),
            flags & 1 != 0,
            flags & 2 != 0,
        ));
    }

    let read_u64 = |pos: usize| -> Result<u64, StorageError> {
        let slice = payload.get(pos..pos + 8).ok_or(StorageError::Corrupt)?;
        Ok(u64::from_le_bytes(slice.try_into().unwrap()))
    };
    let read_u32 = |pos: usize| -> Result<u32, StorageError> {
        let slice = payload.get(pos..pos + 4).ok_or(StorageError::Corrupt)?;
        Ok(u32::from_le_bytes(slice.try_into().unwrap()))
    };

    let mut used_bytes = 0usize;
    let mut patches: Vec<(usize, u32, usize, usize)> = Vec::new();
    let mut lengths: Vec<(usize, u32, u32)> = Vec::new();

    if cursor + 16 <= payload.len() {
        used_bytes = usize::try_from(read_u64(cursor)?).map_err(|_| StorageError::Corrupt)?;
        let patches_count = read_u32(cursor + 8)? as usize;
        let lengths_count = read_u32(cursor + 12)? as usize;
        cursor += 16;
        let data_bytes = payload
            .get(cursor..cursor + used_bytes)
            .ok_or(StorageError::Corrupt)?;
        cursor += used_bytes;
        for _ in 0..patches_count {
            let file = usize::try_from(read_u64(cursor)?).map_err(|_| StorageError::Corrupt)?;
            let position =
                u32::try_from(read_u64(cursor + 8)?).map_err(|_| StorageError::Corrupt)?;
            let offset =
                usize::try_from(read_u64(cursor + 16)?).map_err(|_| StorageError::Corrupt)?;
            let len = usize::try_from(read_u64(cursor + 24)?).map_err(|_| StorageError::Corrupt)?;
            cursor += 32;
            if offset
                .checked_add(len)
                .is_none_or(|end| end > data_bytes.len())
            {
                return Err(StorageError::Corrupt);
            }
            patches.push((file, position, offset, len));
        }
        for _ in 0..lengths_count {
            let file = usize::try_from(read_u64(cursor)?).map_err(|_| StorageError::Corrupt)?;
            let len = read_u32(cursor + 8)?;
            let base_visible = read_u32(cursor + 12)?;
            cursor += 16;
            lengths.push((file, len, base_visible));
        }

        let mut files = Vec::new();
        for (idx, (base_name, name, deleted, directory)) in raw_files.into_iter().enumerate() {
            let length_entry = lengths.iter().find(|&&(f, _, _)| f == idx);
            let mut file_patches: Vec<_> = patches
                .iter()
                .copied()
                .filter(|&(f, _, _, _)| f == idx)
                .collect();
            file_patches.sort_by_key(|&(_, pos, _, _)| pos);

            let kind = if deleted {
                if base_name.is_empty() {
                    continue;
                }
                OverlayFileKind::Deleted
            } else if directory {
                if base_name.is_empty() {
                    OverlayFileKind::Directory
                } else if name != base_name {
                    OverlayFileKind::Renamed
                } else {
                    OverlayFileKind::Directory
                }
            } else if base_name.is_empty() {
                OverlayFileKind::Created
            } else if length_entry.is_some() || !file_patches.is_empty() {
                OverlayFileKind::Modified
            } else if name != base_name {
                OverlayFileKind::Renamed
            } else {
                OverlayFileKind::Unchanged
            };

            let size = length_entry.map(|&(_, len, _)| len).or_else(|| {
                file_patches
                    .iter()
                    .map(|&(_, pos, _, len)| pos.saturating_add(len as u32))
                    .max()
            });

            let preview = if matches!(kind, OverlayFileKind::Created | OverlayFileKind::Modified) {
                file_patches.first().map(|&(_, pos, offset, len)| {
                    let effective_len = size
                        .map_or(len, |total| total.saturating_sub(pos) as usize)
                        .min(len)
                        .min(256);
                    format_preview(pos, &data_bytes[offset..offset + effective_len])
                })
            } else {
                None
            };

            let display_path = if name.is_empty() {
                base_name.clone()
            } else {
                name
            };
            files.push(OverlayFileEntry {
                path: display_path,
                base_path: (!base_name.is_empty()).then_some(base_name),
                kind,
                size,
                preview,
            });
        }
        let changed_count = files.iter().filter(|f| f.kind.is_changed()).count();
        return Ok(StoreSummary {
            revision,
            used_bytes,
            changed_count,
            files,
        });
    }

    Ok(StoreSummary {
        revision,
        used_bytes,
        changed_count: 0,
        files: Vec::new(),
    })
}

pub fn format_preview(start_offset: u32, bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "(empty file)".to_owned();
    }
    let printable = bytes
        .iter()
        .filter(|&&b| matches!(b, b'\t' | b'\r' | b'\n' | 0x20..=0x7E))
        .count();
    if printable * 10 >= bytes.len() * 9 {
        let text = String::from_utf8_lossy(bytes);
        let mut lines: Vec<&str> = text.lines().take(12).collect();
        if text.lines().count() > 12 {
            lines.push("…");
        }
        return lines.join("\n");
    }
    let mut out = String::new();
    for (row_idx, chunk) in bytes.chunks(16).take(8).enumerate() {
        if row_idx > 0 {
            out.push('\n');
        }
        let addr = start_offset as usize + row_idx * 16;
        out.push_str(&format!("{addr:04X}: "));
        for i in 0..16 {
            if let Some(&b) = chunk.get(i) {
                out.push_str(&format!("{b:02X} "));
            } else {
                out.push_str("   ");
            }
            if i == 7 {
                out.push(' ');
            }
        }
        out.push('|');
        for &b in chunk {
            let c = if (0x20..=0x7E).contains(&b) {
                b as char
            } else {
                '.'
            };
            out.push(c);
        }
        out.push('|');
    }
    out
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn decodes_dov1_snapshot_into_created_modified_and_deleted_entries() {
        let mut snapshot = Vec::new();
        snapshot.extend_from_slice(b"DOV1");
        snapshot.extend_from_slice(&3u32.to_le_bytes());

        // Entry 0: Modified SOUND.CFG
        snapshot.push(9);
        snapshot.extend_from_slice(b"SOUND.CFG");
        snapshot.push(9);
        snapshot.extend_from_slice(b"SOUND.CFG");
        snapshot.push(0);

        // Entry 1: Deleted OLD.TMP
        snapshot.push(7);
        snapshot.extend_from_slice(b"OLD.TMP");
        snapshot.push(7);
        snapshot.extend_from_slice(b"OLD.TMP");
        snapshot.push(1);

        // Entry 2: Created SAVE0.GM1
        snapshot.push(0);
        snapshot.push(9);
        snapshot.extend_from_slice(b"SAVE0.GM1");
        snapshot.push(0);

        let cfg_bytes = b"PORT=220\r\nIRQ=7\r\n";
        let sav_bytes: &[u8] = &[0x48, 0x4D, 0x4D, 0x31, 0x00, 0x01, 0x02, 0xFF];
        let used = (cfg_bytes.len() + sav_bytes.len()) as u64;
        snapshot.extend_from_slice(&used.to_le_bytes());
        snapshot.extend_from_slice(&2u32.to_le_bytes()); // 2 patches
        snapshot.extend_from_slice(&2u32.to_le_bytes()); // 2 lengths
        snapshot.extend_from_slice(cfg_bytes);
        snapshot.extend_from_slice(sav_bytes);

        // Patch 0: file 0, pos 0, offset 0, len 17
        for val in [0u64, 0, 0, cfg_bytes.len() as u64] {
            snapshot.extend_from_slice(&val.to_le_bytes());
        }
        // Patch 1: file 2, pos 0, offset 17, len 8
        for val in [2u64, 0, cfg_bytes.len() as u64, sav_bytes.len() as u64] {
            snapshot.extend_from_slice(&val.to_le_bytes());
        }
        // Length 0: file 0, len 17, base_visible 17
        snapshot.extend_from_slice(&0u64.to_le_bytes());
        snapshot.extend_from_slice(&(cfg_bytes.len() as u32).to_le_bytes());
        snapshot.extend_from_slice(&(cfg_bytes.len() as u32).to_le_bytes());
        // Length 1: file 2, len 8, base_visible 0
        snapshot.extend_from_slice(&2u64.to_le_bytes());
        snapshot.extend_from_slice(&(sav_bytes.len() as u32).to_le_bytes());
        snapshot.extend_from_slice(&0u32.to_le_bytes());

        let summary = inspect_dov1(3, &snapshot).unwrap();
        assert_eq!(summary.revision, 3);
        assert_eq!(summary.files.len(), 3);

        assert_eq!(summary.files[0].path, "SOUND.CFG");
        assert_eq!(summary.files[0].kind, OverlayFileKind::Modified);
        assert_eq!(summary.files[0].size, Some(17));
        assert!(
            summary.files[0]
                .preview
                .as_deref()
                .unwrap()
                .contains("IRQ=7")
        );

        assert_eq!(summary.files[1].path, "OLD.TMP");
        assert_eq!(summary.files[1].kind, OverlayFileKind::Deleted);

        assert_eq!(summary.files[2].path, "SAVE0.GM1");
        assert_eq!(summary.files[2].kind, OverlayFileKind::Created);
        assert_eq!(summary.files[2].size, Some(8));
        assert!(
            summary.files[2]
                .preview
                .as_deref()
                .unwrap()
                .contains("48 4D 4D 31")
        );
    }
}
