//! Native positional media and a fenced, atomically published upper save store.
use doslet_config::{SourceKind, WritePolicy};
use doslet_runtime::Session;
use doslet_storage::{
    CachedSource, CommitBatch, HostCompletion, HostRequest, SourceId, SourceIdentity, StorageError,
    payload_digest,
};
use std::{
    error::Error,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread,
    time::SystemTime,
};

const MAX_SNAPSHOT: usize = 16 * 1024 * 1024;
const MAX_LEDGER: usize = 4096;
const QUEUE: usize = 8;
static TEMP_ID: AtomicU64 = AtomicU64::new(1);
static SOURCE_ID: AtomicU64 = AtomicU64::new(1);
static SESSION_EPOCH: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
struct Manifest {
    namespace: [u8; 32],
    base: [u8; 32],
    fence: u64,
    revision: u64,
    digest: u64,
    ledger: Vec<(u64, u64, u64)>,
}
impl Manifest {
    fn encode(&self) -> Vec<u8> {
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
    fn decode(bytes: &[u8]) -> Result<Self, StorageError> {
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
    fn snapshot_name(&self) -> String {
        format!("snapshot-{:016x}-{:016x}.bin", self.revision, self.digest)
    }
}

/// One owner retains the OS lock until its host worker has completed all work.
struct NativeStore {
    directory: PathBuf,
    _lock: File,
    manifest: Manifest,
    payload: Vec<u8>,
}
impl NativeStore {
    fn open(directory: &Path, namespace: [u8; 32], base: [u8; 32]) -> Result<Self, StorageError> {
        fs::create_dir_all(directory).map_err(io_error)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("writer.lock"))
            .map_err(io_error)?;
        lock.try_lock().map_err(|_| StorageError::BusyWriter)?;
        let directory = fs::canonicalize(directory).map_err(io_error)?;
        let mut manifest =
            match bounded_read(&directory.join("manifest.bin"), 108 + MAX_LEDGER * 24) {
                Ok(bytes) => Manifest::decode(&bytes)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Manifest {
                    namespace,
                    base,
                    fence: 0,
                    revision: 0,
                    digest: 0,
                    ledger: Vec::new(),
                },
                Err(e) => return Err(io_error(e)),
            };
        if manifest.namespace != namespace || manifest.base != base {
            return Err(StorageError::MissingBase);
        }
        let payload = if manifest.revision == 0 {
            Vec::new()
        } else {
            let bytes = bounded_read(&directory.join(manifest.snapshot_name()), MAX_SNAPSHOT)
                .map_err(io_error)?;
            if payload_digest(&bytes) != manifest.digest {
                return Err(StorageError::Corrupt);
            }
            bytes
        };
        manifest.fence = manifest.fence.checked_add(1).ok_or(StorageError::Quota)?;
        publish(&directory, "manifest.bin", &manifest.encode()).map_err(io_error)?;
        let store = Self {
            directory,
            _lock: lock,
            manifest,
            payload,
        };
        store.prune_snapshots();
        Ok(store)
    }
    fn commit(&mut self, batch: &CommitBatch) -> Result<u64, StorageError> {
        batch.validate(MAX_SNAPSHOT)?;
        if batch.namespace != self.manifest.namespace || batch.base != self.manifest.base {
            return Err(StorageError::MissingBase);
        }
        if batch.fencing_token != self.manifest.fence {
            return Err(StorageError::BusyWriter);
        }
        if let Some(&(_, digest, revision)) = self
            .manifest
            .ledger
            .iter()
            .find(|&&(id, _, _)| id == batch.mutation_id)
        {
            return if digest == batch.digest
                && batch.expected_revision.checked_add(1) == Some(revision)
            {
                Ok(revision)
            } else {
                Err(StorageError::MutationConflict)
            };
        }
        if batch.expected_revision != self.manifest.revision {
            return Err(StorageError::StaleRevision);
        }
        if self.manifest.ledger.len() == MAX_LEDGER {
            return Err(StorageError::Quota);
        }
        let mut next = self.manifest.clone();
        next.revision = next.revision.checked_add(1).ok_or(StorageError::Quota)?;
        next.digest = batch.digest;
        next.ledger
            .push((batch.mutation_id, batch.digest, next.revision));
        let snapshot = self.directory.join(next.snapshot_name());
        match bounded_read(&snapshot, MAX_SNAPSHOT) {
            Ok(bytes) => {
                if bytes != batch.payload {
                    return Err(StorageError::Corrupt);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                publish(&self.directory, &next.snapshot_name(), &batch.payload).map_err(io_error)?
            }
            Err(error) => return Err(io_error(error)),
        }
        // Immutable data is synced before the only publication point. Orphaned
        // snapshots from interruption never change the mounted revision.
        let published = publish(&self.directory, "manifest.bin", &next.encode());
        if let Err(error) = published {
            // A directory-sync failure can happen after rename. Reconcile the
            // visible transaction so retries cannot overwrite an acknowledged ID.
            if bounded_read(&self.directory.join("manifest.bin"), 108 + MAX_LEDGER * 24)
                .ok()
                .as_deref()
                == Some(next.encode().as_slice())
            {
                self.manifest = next;
                self.payload = batch.payload.clone();
            }
            return Err(io_error(error));
        }
        self.manifest = next;
        self.payload = batch.payload.clone();
        self.prune_snapshots();
        Ok(self.manifest.revision)
    }
    fn prune_snapshots(&self) {
        // Only exact names recorded in this namespace's committed ledger are
        // eligible. Keep the current and previous immutable payload. Cleanup
        // follows durable manifest publication and never changes commit success.
        for &(_, digest, revision) in
            &self.manifest.ledger[..self.manifest.ledger.len().saturating_sub(2)]
        {
            let name = format!("snapshot-{revision:016x}-{digest:016x}.bin");
            let _ = fs::remove_file(self.directory.join(name));
        }
    }
}
fn io_error(error: std::io::Error) -> StorageError {
    match error.kind() {
        std::io::ErrorKind::StorageFull => StorageError::Quota,
        std::io::ErrorKind::NotFound => StorageError::Unavailable,
        _ => StorageError::Unavailable,
    }
}
fn bounded_read(path: &Path, limit: usize) -> std::io::Result<Vec<u8>> {
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
fn destination_path(path: &Path) -> std::io::Result<PathBuf> {
    if path.exists() {
        return fs::canonicalize(path);
    }
    let filename = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "save store needs a directory path",
        )
    })?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(destination_path(parent)?.join(filename))
}
fn publish(directory: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    let temporary = directory.join(format!(
        ".{name}.{}.{:032x}.{:016x}.tmp",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
        TEMP_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, directory.join(name))?;
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    Ok(())
}
fn fingerprint(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0; 32];
    for index in 0..4 {
        let mut seed = Vec::with_capacity(bytes.len() + 1);
        seed.push(index as u8);
        seed.extend_from_slice(bytes);
        out[index * 8..index * 8 + 8].copy_from_slice(&payload_digest(&seed).to_le_bytes());
    }
    out
}

struct LocalSource {
    file: File,
    identity: SourceIdentity,
    modified: Option<SystemTime>,
}
impl LocalSource {
    fn open(path: &Path) -> Result<Self, Box<dyn Error>> {
        let path = fs::canonicalize(path)?;
        let file = File::open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err("ISO source is not a file".into());
        }
        let mut key = path.to_string_lossy().as_bytes().to_vec();
        key.extend_from_slice(&metadata.len().to_le_bytes());
        let modified = metadata.modified().ok();
        if let Some(time) = modified.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()) {
            key.extend_from_slice(&time.as_nanos().to_le_bytes());
        }
        Ok(Self {
            file,
            identity: SourceIdentity {
                id: SourceId(SOURCE_ID.fetch_add(1, Ordering::Relaxed)),
                generation: 1,
                length: metadata.len(),
                validator: fingerprint(&key),
            },
            modified,
        })
    }
    fn read(&mut self, offset: u64, length: u32) -> Result<Vec<u8>, StorageError> {
        if length as usize > doslet_storage::CACHE_CHUNK
            || offset
                .checked_add(u64::from(length))
                .is_none_or(|end| end > self.identity.length)
        {
            return Err(StorageError::OutOfBounds);
        }
        self.validate()?;
        self.file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
        let mut bytes = vec![0; length as usize];
        self.file.read_exact(&mut bytes).map_err(io_error)?;
        self.validate()?;
        Ok(bytes)
    }
    fn validate(&self) -> Result<(), StorageError> {
        let metadata = self.file.metadata().map_err(io_error)?;
        if metadata.len() != self.identity.length || metadata.modified().ok() != self.modified {
            Err(StorageError::IdentityChanged)
        } else {
            Ok(())
        }
    }
}

/// Strong-validator HTTPS ranges. Redirects are resolved only by the initial
/// probe; later requests use the pinned URI with redirects disabled.
struct HttpSource {
    agent: ureq::Agent,
    url: String,
    etag: String,
    identity: SourceIdentity,
}
struct HttpPart {
    url: String,
    etag: String,
    total: u64,
    start: u64,
    end: u64,
    bytes: Vec<u8>,
}
impl HttpSource {
    fn open(url: &str) -> Result<Self, Box<dyn Error>> {
        if !url.starts_with("https://") {
            return Err("ISO URL must use HTTPS".into());
        }
        let probe_agent: ureq::Agent = ureq::Agent::config_builder()
            .https_only(true)
            .http_status_as_error(false)
            .max_redirects(5)
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .into();
        let part = Self::fetch(&probe_agent, url, None, 0, 65536)
            .map_err(|error| format!("HTTPS ISO probe: {error:?}"))?;
        if part.start != 0
            || part.total == 0
            || part.end != part.total.min(65536) - 1
            || part.bytes.len() != part.total.min(65536) as usize
        {
            return Err("HTTPS ISO probe returned inconsistent range bounds".into());
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .https_only(true)
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .build()
            .into();
        let validator = http_validator(&part.url, &part.etag, part.total);
        Ok(Self {
            agent,
            url: part.url,
            etag: part.etag,
            identity: SourceIdentity {
                id: SourceId(SOURCE_ID.fetch_add(1, Ordering::Relaxed)),
                generation: 1,
                length: part.total,
                validator,
            },
        })
    }
    fn read(&mut self, offset: u64, length: u32) -> Result<Vec<u8>, StorageError> {
        if length == 0
            || length as usize > doslet_storage::CACHE_CHUNK
            || offset
                .checked_add(u64::from(length))
                .is_none_or(|end| end > self.identity.length)
        {
            return Err(StorageError::OutOfBounds);
        }
        let part = Self::fetch(&self.agent, &self.url, Some(&self.etag), offset, length)?;
        if part.url != self.url || part.etag != self.etag {
            return Err(StorageError::IdentityChanged);
        }
        let validator = http_validator(&part.url, &part.etag, part.total);
        doslet_storage::validate_range(
            self.identity,
            offset,
            length,
            doslet_storage::RangeResponse {
                status: 206,
                start: part.start,
                end_inclusive: part.end,
                total: part.total,
                body_len: part.bytes.len(),
                validator: &validator,
                encoded: false,
            },
        )?;
        Ok(part.bytes)
    }
    fn fetch(
        agent: &ureq::Agent,
        url: &str,
        etag: Option<&str>,
        offset: u64,
        length: u32,
    ) -> Result<HttpPart, StorageError> {
        use ureq::ResponseExt;
        let end = offset
            .checked_add(u64::from(length))
            .and_then(|end| end.checked_sub(1))
            .ok_or(StorageError::OutOfBounds)?;
        let started = std::time::Instant::now();
        for attempt in 0..3 {
            if attempt != 0 {
                thread::sleep(std::time::Duration::from_millis(if attempt == 1 {
                    250
                } else {
                    1000
                }));
            }
            if started.elapsed() >= std::time::Duration::from_secs(35) {
                return Err(StorageError::Unavailable);
            }
            let mut request = agent
                .get(url)
                .header("Range", format!("bytes={offset}-{end}"))
                .header("Accept-Encoding", "identity");
            if let Some(etag) = etag {
                request = request.header("If-Match", etag);
            }
            let mut response = match request.call() {
                Ok(response) => response,
                Err(error) => {
                    if matches!(
                        error,
                        ureq::Error::Io(_)
                            | ureq::Error::Timeout(_)
                            | ureq::Error::HostNotFound
                            | ureq::Error::ConnectionFailed
                    ) && attempt < 2
                    {
                        continue;
                    }
                    return Err(StorageError::Unavailable);
                }
            };
            let status = response.status().as_u16();
            if matches!(status, 408 | 429 | 500 | 502 | 503 | 504) && attempt < 2 {
                continue;
            }
            if status == 412 {
                return Err(StorageError::IdentityChanged);
            }
            if status != 206 {
                return Err(StorageError::UnsupportedRange);
            }
            let resolved = response.get_uri().to_string();
            let headers = response.headers();
            let field = |name: &str| {
                if headers.get_all(name).iter().count() != 1 {
                    return Err(StorageError::Corrupt);
                }
                headers
                    .get(name)
                    .and_then(|header| header.to_str().ok())
                    .ok_or(StorageError::Corrupt)
            };
            let actual_etag = field("etag")?.to_owned();
            if !strong_etag(&actual_etag) {
                return Err(StorageError::IdentityChanged);
            }
            if etag.is_some_and(|expected| expected != actual_etag) {
                return Err(StorageError::IdentityChanged);
            }
            if headers
                .get("content-encoding")
                .is_some_and(|header| header.to_str().map_or(true, |value| value != "identity"))
            {
                return Err(StorageError::UnsupportedRange);
            }
            let (start, part_end, total) = parse_content_range(field("content-range")?)?;
            if total == 0 || start != offset || part_end != end.min(total - 1) || start > part_end {
                return Err(StorageError::Corrupt);
            }
            let expected =
                usize::try_from(part_end - start + 1).map_err(|_| StorageError::Corrupt)?;
            if expected > length as usize {
                return Err(StorageError::Corrupt);
            }
            if let Some(header) = headers.get("content-length")
                && header
                    .to_str()
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    != Some(expected as u64)
            {
                return Err(StorageError::Corrupt);
            }
            let bytes = match response
                .body_mut()
                .with_config()
                .limit(expected as u64 + 1)
                .read_to_vec()
            {
                Ok(bytes) => bytes,
                Err(error) => {
                    if matches!(error, ureq::Error::Io(_) | ureq::Error::Timeout(_)) && attempt < 2
                    {
                        continue;
                    }
                    return Err(StorageError::Corrupt);
                }
            };
            if bytes.len() != expected {
                return Err(StorageError::Corrupt);
            }
            return Ok(HttpPart {
                url: resolved,
                etag: actual_etag,
                total,
                start,
                end: part_end,
                bytes,
            });
        }
        Err(StorageError::Unavailable)
    }
}
fn strong_etag(etag: &str) -> bool {
    etag.len() >= 2
        && etag.starts_with('"')
        && etag.ends_with('"')
        && etag.as_bytes()[1..etag.len() - 1]
            .iter()
            .all(|byte| *byte >= 0x21 && *byte != b'"' && *byte != 0x7F)
}
fn parse_content_range(value: &str) -> Result<(u64, u64, u64), StorageError> {
    let range = value.strip_prefix("bytes ").ok_or(StorageError::Corrupt)?;
    let (range, total) = range.split_once('/').ok_or(StorageError::Corrupt)?;
    let (start, end) = range.split_once('-').ok_or(StorageError::Corrupt)?;
    let numbers = [start, end, total];
    if numbers
        .iter()
        .any(|number| number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(StorageError::Corrupt);
    }
    Ok((
        start.parse().map_err(|_| StorageError::Corrupt)?,
        end.parse().map_err(|_| StorageError::Corrupt)?,
        total.parse().map_err(|_| StorageError::Corrupt)?,
    ))
}
fn http_validator(url: &str, etag: &str, length: u64) -> [u8; 32] {
    let mut key = url.as_bytes().to_vec();
    key.push(0);
    key.extend_from_slice(etag.as_bytes());
    key.extend_from_slice(&length.to_le_bytes());
    fingerprint(&key)
}
enum MediaSource {
    Local(LocalSource),
    Https(HttpSource),
}
impl MediaSource {
    fn identity(&self) -> SourceIdentity {
        match self {
            Self::Local(source) => source.identity,
            Self::Https(source) => source.identity,
        }
    }
    fn read(&mut self, offset: u64, length: u32) -> Result<Vec<u8>, StorageError> {
        match self {
            Self::Local(source) => source.read(offset, length),
            Self::Https(source) => source.read(offset, length),
        }
    }
}

pub struct NativeStorage {
    requests: SyncSender<HostRequest>,
    completions: Receiver<HostCompletion>,
    deferred: Option<HostRequest>,
    store: Option<Arc<Mutex<NativeStore>>>,
    in_flight: usize,
}
impl NativeStorage {
    #[allow(dead_code)]
    pub fn prepare(
        mut session: Session,
        settings: &crate::native_input::LaunchSettings,
    ) -> Result<(Session, Self), Box<dyn Error>> {
        let storage = Self::prepare_mut(&mut session, settings)?;
        Ok((session, storage))
    }
    pub fn prepare_mut(
        session: &mut Session,
        settings: &crate::native_input::LaunchSettings,
    ) -> Result<Self, Box<dyn Error>> {
        Self::prepare_inner(session, settings, None)
    }
    #[allow(dead_code)]
    pub fn prepare_replacement(
        &mut self,
        mut session: Session,
        settings: &crate::native_input::LaunchSettings,
    ) -> Result<(Session, Self), Box<dyn Error>> {
        let storage = self.prepare_replacement_mut(&mut session, settings)?;
        Ok((session, storage))
    }
    pub fn prepare_replacement_mut(
        &mut self,
        session: &mut Session,
        settings: &crate::native_input::LaunchSettings,
    ) -> Result<Self, Box<dyn Error>> {
        if self.in_flight != 0 || self.deferred.is_some() {
            return Err("storage operations must settle before replacing the session".into());
        }
        Self::prepare_inner(session, settings, self.store.as_ref())
    }
    fn prepare_inner(
        session: &mut Session,
        settings: &crate::native_input::LaunchSettings,
        reuse: Option<&Arc<Mutex<NativeStore>>>,
    ) -> Result<Self, Box<dyn Error>> {
        let mut source = None;
        let mut store = None;
        if let Some(spec) = &settings.machine_spec {
            spec.resolve().map_err(|e| e.to_string())?;
            if let Some(mount) = spec
                .mounts
                .iter()
                .find(|mount| mount.letter == b'C' && mount.policy == WritePolicy::Persistent)
            {
                let binding = mount
                    .store
                    .as_deref()
                    .ok_or("persistent mount requires a save store")?;
                let path = Path::new(spec.binding(binding));
                if mount.kind == SourceKind::NativeDirectory {
                    let root = fs::canonicalize(spec.binding(&mount.source))?;
                    if destination_path(path)?.starts_with(root) {
                        return Err("save store must be outside the mounted C: directory".into());
                    }
                }
                let namespace = fingerprint(binding.as_bytes());
                let base = session.base_identity();
                let existing = reuse.filter(|store| {
                    store.lock().is_ok_and(|store| {
                        fs::canonicalize(path).is_ok_and(|path| path == store.directory)
                            && store.manifest.namespace == namespace
                            && store.manifest.base == base
                    })
                });
                let selected = if let Some(existing) = existing {
                    Arc::clone(existing)
                } else {
                    Arc::new(Mutex::new(
                        NativeStore::open(path, namespace, base)
                            .map_err(|error| format!("save store: {error:?}"))?,
                    ))
                };
                {
                    let current = selected.lock().map_err(|_| "save store worker failed")?;
                    session.attach_persistent_store(
                        namespace,
                        base,
                        current.manifest.fence,
                        current.manifest.revision,
                        if current.manifest.revision == 0 {
                            None
                        } else {
                            Some(&current.payload)
                        },
                    )?;
                }
                store = Some(selected);
            }
            if let Some(mount) = spec.mounts.iter().find(|mount| {
                matches!(mount.kind, SourceKind::LocalIso | SourceKind::HttpsIso)
                    && !(mount.kind == SourceKind::LocalIso && settings.preload_local_media)
            }) {
                let media = match mount.kind {
                    SourceKind::HttpsIso => {
                        MediaSource::Https(HttpSource::open(spec.binding(&mount.source))?)
                    }
                    _ => MediaSource::Local(LocalSource::open(Path::new(
                        spec.binding(&mount.source),
                    ))?),
                };
                session.attach_cdrom_source(
                    mount.letter,
                    CachedSource::new(
                        media.identity(),
                        SESSION_EPOCH.fetch_add(1, Ordering::Relaxed),
                    ),
                )?;
                source = Some(media);
            }
        }
        let (request_tx, request_rx) = mpsc::sync_channel::<HostRequest>(QUEUE);
        let (completion_tx, completion_rx) = mpsc::sync_channel(QUEUE);
        let worker_store = store.clone();
        thread::Builder::new()
            .name("doslet-storage".into())
            .spawn(move || {
                while let Ok(request) = request_rx.recv() {
                    let completion = match request {
                        HostRequest::Read {
                            tag,
                            source: id,
                            offset,
                            length,
                        } => {
                            let result = source
                                .as_mut()
                                .filter(|source| {
                                    source.identity().id == id
                                        && source.identity().generation == tag.generation
                                })
                                .ok_or(StorageError::StaleCompletion)
                                .and_then(|source| source.read(offset, length));
                            match result {
                                Ok(bytes) => HostCompletion::Read {
                                    tag,
                                    bytes,
                                    result: Ok(()),
                                },
                                Err(error) => HostCompletion::Read {
                                    tag,
                                    bytes: Vec::new(),
                                    result: Err(error),
                                },
                            }
                        }
                        HostRequest::Commit { tag, batch } => {
                            let result = worker_store
                                .as_ref()
                                .ok_or(StorageError::Unavailable)
                                .and_then(|store| {
                                    store
                                        .lock()
                                        .map_err(|_| StorageError::Unavailable)?
                                        .commit(&batch)
                                });
                            HostCompletion::Commit { tag, result }
                        }
                    };
                    if completion_tx.send(completion).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            requests: request_tx,
            completions: completion_rx,
            deferred: None,
            store,
            in_flight: 0,
        })
    }
    /// Polling never waits on a file or worker. Queue saturation retains the
    /// already emitted request and leaves the session waiting at its service.
    pub fn service(&mut self, session: &mut Session) -> Result<(), String> {
        let mut failure = None;
        for _ in 0..QUEUE {
            match self.completions.try_recv() {
                Ok(completion) => {
                    self.in_flight = self.in_flight.saturating_sub(1);
                    let result = match &completion {
                        HostCompletion::Read { result, .. } => *result,
                        HostCompletion::Commit { result, .. } => result.map(|_| ()),
                    };
                    if let Err(error) = session.complete_host_request(completion)
                        && error != StorageError::StaleCompletion
                    {
                        failure = Some(format!("storage completion: {error:?}"));
                    }
                    if let Err(error) = result {
                        failure = Some(format!(
                            "storage operation: {error:?}; changed files remain available for export"
                        ));
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if self.in_flight != 0 {
                        failure = Some("storage worker disconnected".into());
                    }
                    break;
                }
            }
        }
        for _ in 0..QUEUE {
            let Some(request) = self.deferred.take().or_else(|| session.next_host_request()) else {
                break;
            };
            match self.requests.try_send(request) {
                Ok(()) => self.in_flight += 1,
                Err(TrySendError::Full(request)) => {
                    self.deferred = Some(request);
                    break;
                }
                Err(TrySendError::Disconnected(request)) => {
                    let completion = match request {
                        HostRequest::Read { tag, .. } => HostCompletion::Read {
                            tag,
                            bytes: Vec::new(),
                            result: Err(StorageError::Unavailable),
                        },
                        HostRequest::Commit { tag, .. } => HostCompletion::Commit {
                            tag,
                            result: Err(StorageError::Unavailable),
                        },
                    };
                    let _ = session.complete_host_request(completion);
                    return Err("storage worker disconnected".into());
                }
            }
        }
        failure.map_or(Ok(()), Err)
    }
    pub fn export(&mut self, session: &mut Session, path: &Path) -> Result<(), String> {
        let bytes = session
            .export_overlay()
            .map_err(|error| format!("export: {error:?}"))?;
        let directory = path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or("export path requires a filename")?;
        publish(directory, name, &bytes).map_err(|error| error.to_string())
    }
    pub fn import(&mut self, session: &mut Session, path: &Path) -> Result<(), String> {
        if self.in_flight != 0 {
            return Err("storage operations must settle before importing".into());
        }
        let bytes = bounded_read(path, MAX_SNAPSHOT).map_err(|error| error.to_string())?;
        session
            .import_overlay(&bytes)
            .map_err(|error| format!("import: {error:?}"))?;
        session
            .request_overlay_commit()
            .map_err(|error| format!("import commit: {error:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn http_fixture(responses: Vec<String>, offset: u64) -> Result<HttpPart, StorageError> {
        use std::net::TcpListener;
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let worker = thread::spawn(move || {
            for response in responses {
                let started = std::time::Instant::now();
                let (mut stream, _) = loop {
                    match listener.accept() {
                        Ok(socket) => break socket,
                        Err(error)
                            if error.kind() == std::io::ErrorKind::WouldBlock
                                && started.elapsed() < std::time::Duration::from_secs(5) =>
                        {
                            thread::sleep(std::time::Duration::from_millis(5))
                        }
                        Err(error) => panic!("fixture request missing: {error}"),
                    }
                };
                // Accepted sockets inherit O_NONBLOCK on macOS. The fixture
                // reads synchronously with a timeout after polling accept().
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    stream.read_exact(&mut byte).unwrap();
                    request.push(byte[0]);
                    assert!(request.len() < 8192);
                }
                let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
                assert!(request.contains("if-match: \"v1\""));
                assert!(request.contains(&format!("range: bytes={offset}-{}", offset + 3)));
                let _ = stream.write_all(response.as_bytes());
            }
        });
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(std::time::Duration::from_secs(2)))
            .build()
            .into();
        let result = HttpSource::fetch(
            &agent,
            &format!("http://{address}/test.iso"),
            Some("\"v1\""),
            offset,
            4,
        );
        worker.join().unwrap();
        result
    }
    #[test]
    fn controlled_http_transport_validates_ranges_and_retries_only_transient_statuses() {
        let success = "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-3/100\r\nETag: \"v1\"\r\nContent-Length: 4\r\nConnection: close\r\n\r\nDATA";
        assert_eq!(
            http_fixture(vec![success.into()], 0).unwrap().bytes,
            b"DATA"
        );
        assert_eq!(http_fixture(vec!["HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),success.into()],0).unwrap().bytes,b"DATA");
        assert!(matches!(
            http_fixture(
                vec![
                    "HTTP/1.1 200 OK\r\nContent-Length: 1000000000\r\nConnection: close\r\n\r\n"
                        .into()
                ],
                0
            ),
            Err(StorageError::UnsupportedRange)
        ));
        assert!(matches!(
            http_fixture(vec![success.replace("\"v1\"", "\"v2\"")], 0),
            Err(StorageError::IdentityChanged)
        ));
        assert!(matches!(
            http_fixture(vec![success.replace("bytes 0-3/100", "bytes 1-4/100")], 0),
            Err(StorageError::Corrupt)
        ));
        assert!(matches!(
            http_fixture(
                vec![success.replace("Content-Length: 4", "Content-Length: 3")],
                0
            ),
            Err(StorageError::Corrupt)
        ));
        assert!(matches!(http_fixture(vec!["HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()],0),Err(StorageError::IdentityChanged)));
        let offset = 1u64 << 32;
        let large = success.replace(
            "bytes 0-3/100",
            &format!("bytes {offset}-{}/5000000000", offset + 3),
        );
        assert_eq!(http_fixture(vec![large], offset).unwrap().bytes, b"DATA");
    }
    #[test]
    fn retention_keeps_two_published_versions_and_preserves_unrelated_files() {
        let directory = Directory::new();
        let mut store = NativeStore::open(&directory.0, [1; 32], [2; 32]).unwrap();
        let unrelated = directory.0.join("snapshot-user-data.bin");
        fs::write(&unrelated, b"user").unwrap();
        let mut names = Vec::new();
        for id in 1..=4 {
            let request = batch(&store, id, &[id as u8]);
            store.commit(&request).unwrap();
            names.push(store.manifest.snapshot_name());
        }
        assert!(!directory.0.join(&names[0]).exists());
        assert!(!directory.0.join(&names[1]).exists());
        assert!(directory.0.join(&names[2]).exists());
        assert!(directory.0.join(&names[3]).exists());
        assert_eq!(fs::read(unrelated).unwrap(), b"user");
    }
    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "doslet-store-test-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn batch(store: &NativeStore, id: u64, bytes: &[u8]) -> CommitBatch {
        CommitBatch {
            namespace: store.manifest.namespace,
            base: store.manifest.base,
            fencing_token: store.manifest.fence,
            expected_revision: store.manifest.revision,
            mutation_id: id,
            digest: payload_digest(bytes),
            payload: bytes.to_vec(),
        }
    }
    #[test]
    fn writer_lock_dedup_fencing_and_reopen() {
        let directory = Directory::new();
        let mut store = NativeStore::open(&directory.0, [1; 32], [2; 32]).unwrap();
        assert!(matches!(
            NativeStore::open(&directory.0, [1; 32], [2; 32]),
            Err(StorageError::BusyWriter)
        ));
        let first = batch(&store, 1, b"saved");
        assert_eq!(store.commit(&first), Ok(1));
        assert_eq!(store.commit(&first), Ok(1));
        let mut wrong_revision = first.clone();
        wrong_revision.expected_revision = 1;
        assert_eq!(
            store.commit(&wrong_revision),
            Err(StorageError::MutationConflict)
        );
        let mut conflict = first.clone();
        conflict.payload = b"changed".to_vec();
        conflict.digest = payload_digest(&conflict.payload);
        assert_eq!(store.commit(&conflict), Err(StorageError::MutationConflict));
        let fence = store.manifest.fence;
        drop(store);
        let mut reopened = NativeStore::open(&directory.0, [1; 32], [2; 32]).unwrap();
        assert!(reopened.manifest.fence > fence);
        assert_eq!(reopened.payload, b"saved");
        assert_eq!(reopened.commit(&first), Err(StorageError::BusyWriter));
    }
    #[test]
    fn orphan_snapshot_and_unpublished_manifest_do_not_replace_revision() {
        let directory = Directory::new();
        let mut store = NativeStore::open(&directory.0, [1; 32], [2; 32]).unwrap();
        let first = batch(&store, 1, b"first");
        store.commit(&first).unwrap();
        let second = batch(&store, 2, b"second");
        let mut next = store.manifest.clone();
        next.revision = 2;
        next.digest = second.digest;
        fs::write(directory.0.join(next.snapshot_name()), &second.payload).unwrap();
        fs::write(
            directory.0.join(".manifest.bin.interrupted.tmp"),
            next.encode(),
        )
        .unwrap();
        drop(store);
        let reopened = NativeStore::open(&directory.0, [1; 32], [2; 32]).unwrap();
        assert_eq!(reopened.manifest.revision, 1);
        assert_eq!(reopened.payload, b"first");
    }
    #[test]
    fn checksum_and_missing_base_are_visible() {
        let directory = Directory::new();
        let store = NativeStore::open(&directory.0, [1; 32], [2; 32]).unwrap();
        drop(store);
        assert!(matches!(
            NativeStore::open(&directory.0, [1; 32], [3; 32]),
            Err(StorageError::MissingBase)
        ));
        let mut bytes = fs::read(directory.0.join("manifest.bin")).unwrap();
        bytes[72] ^= 1;
        fs::write(directory.0.join("manifest.bin"), bytes).unwrap();
        assert!(matches!(
            NativeStore::open(&directory.0, [1; 32], [2; 32]),
            Err(StorageError::Corrupt)
        ));
    }
    #[test]
    fn local_reads_keep_full_64_bit_offsets_and_detect_mutation() {
        let directory = Directory::new();
        let path = directory.0.join("sparse.iso");
        let mut file = File::create(&path).unwrap();
        let offset = (1u64 << 32) + 7;
        file.set_len(offset + 4).unwrap();
        file.seek(SeekFrom::Start(offset)).unwrap();
        file.write_all(&[1, 2, 3, 4]).unwrap();
        file.sync_all().unwrap();
        let mut source = LocalSource::open(&path).unwrap();
        assert_eq!(source.read(offset, 4), Ok(vec![1, 2, 3, 4]));
        assert_eq!(source.read(offset + 3, 2), Err(StorageError::OutOfBounds));
        file.set_len(offset + 5).unwrap();
        assert_eq!(source.read(offset, 4), Err(StorageError::IdentityChanged));
    }
    #[test]
    fn strict_http_metadata_rejects_weak_tags_and_malformed_ranges() {
        for valid in ["\"abc\"", "\"\""] {
            assert!(strong_etag(valid));
        }
        for invalid in ["W/\"abc\"", "abc", "\"abc\n\"", "\"a\"b\""] {
            assert!(!strong_etag(invalid));
        }
        assert_eq!(
            parse_content_range("bytes 4294967296-4294967299/5000000000"),
            Ok((4294967296, 4294967299, 5000000000))
        );
        for invalid in [
            "bytes */42",
            "bytes 0-3/*",
            "bytes 0-3/+4",
            "Bytes 0-3/4",
            "bytes 18446744073709551616-3/4",
        ] {
            assert_eq!(parse_content_range(invalid), Err(StorageError::Corrupt));
        }
    }
}
