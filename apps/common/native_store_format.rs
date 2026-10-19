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
