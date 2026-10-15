//! Bounded positional media reads and transactional upper-store messages.
#![no_std]
extern crate alloc;
use alloc::{boxed::Box, vec, vec::Vec};

pub const CACHE_CHUNK: usize = 128 * 1024;
pub const CACHE_BUDGET: usize = 16 * 1024 * 1024;
pub const MAX_IN_FLIGHT: usize = 4;
pub const OVERLAY_CHUNK: usize = 4096;
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceIdentity {
    pub id: SourceId,
    pub generation: u64,
    pub length: u64,
    /// Pinned media fingerprint. URL/display name is deliberately not identity.
    pub validator: [u8; 32],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestTag {
    pub epoch: u64,
    pub id: u64,
    pub generation: u64,
    pub slot: u16,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StorageError {
    Pending,
    OutOfBounds,
    Unavailable,
    Quota,
    BusyWriter,
    StaleRevision,
    MissingBase,
    Corrupt,
    IdentityChanged,
    UnsupportedRange,
    StaleCompletion,
    MutationConflict,
    UnsupportedSchema,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitBatch {
    pub namespace: [u8; 32],
    pub base: [u8; 32],
    pub fencing_token: u64,
    pub expected_revision: u64,
    pub mutation_id: u64,
    pub digest: u64,
    /// Versioned upper snapshot. Commit must atomically replace this value,
    /// advance revision, and record mutation_id/digest/result in one transaction.
    pub payload: Vec<u8>,
}
impl CommitBatch {
    pub fn validate(&self, limit: usize) -> Result<(), StorageError> {
        if self.payload.len() > limit {
            return Err(StorageError::Quota);
        }
        if payload_digest(&self.payload) != self.digest {
            return Err(StorageError::Corrupt);
        }
        Ok(())
    }
}
/// Deterministic integrity checksum, not a cryptographic authentication mechanism.
pub fn payload_digest(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
    })
}
#[derive(Debug)]
pub enum HostRequest {
    Read {
        tag: RequestTag,
        source: SourceId,
        offset: u64,
        length: u32,
    },
    Commit {
        tag: RequestTag,
        batch: CommitBatch,
    },
}
impl HostRequest {
    pub fn tag(&self) -> RequestTag {
        match self {
            Self::Read { tag, .. } | Self::Commit { tag, .. } => *tag,
        }
    }
}
#[derive(Debug)]
pub enum HostCompletion {
    Read {
        tag: RequestTag,
        bytes: Vec<u8>,
        result: Result<(), StorageError>,
    },
    Commit {
        tag: RequestTag,
        result: Result<u64, StorageError>,
    },
}
impl HostCompletion {
    pub fn tag(&self) -> RequestTag {
        match self {
            Self::Read { tag, .. } | Self::Commit { tag, .. } => *tag,
        }
    }
}
/// A read either supplies the entire available requested range, or returns
/// Pending/error without changing the caller's buffer. Offsets never truncate.
pub trait ReadAt {
    fn identity(&self) -> SourceIdentity;
    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<usize, StorageError>;
}
pub struct VecSource {
    identity: SourceIdentity,
    bytes: Vec<u8>,
}
impl VecSource {
    pub fn new(bytes: Vec<u8>) -> Self {
        let digest = payload_digest(&bytes);
        let mut validator = [0; 32];
        validator[..8].copy_from_slice(&digest.to_le_bytes());
        Self {
            identity: SourceIdentity {
                id: SourceId(digest),
                generation: 1,
                length: bytes.len() as u64,
                validator,
            },
            bytes,
        }
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}
impl ReadAt for VecSource {
    fn identity(&self) -> SourceIdentity {
        self.identity
    }
    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<usize, StorageError> {
        if offset > self.identity.length {
            return Err(StorageError::OutOfBounds);
        }
        let start = usize::try_from(offset).map_err(|_| StorageError::OutOfBounds)?;
        let count = output.len().min(self.bytes.len() - start);
        output[..count].copy_from_slice(&self.bytes[start..start + count]);
        Ok(count)
    }
}
#[derive(Clone, Copy)]
struct CacheLine {
    chunk: u64,
    used: u64,
    valid: bool,
}
#[derive(Clone, Copy)]
struct Flight {
    tag: RequestTag,
    chunk: u64,
    emitted: bool,
}
/// A single source's fixed 16-MiB cache, shared by metadata/file/raw-sector users.
/// Cache misses enqueue at most four reads. Allocation happens only at setup
/// and when the host transfers its owned completion buffer.
pub struct CachedSource {
    identity: SourceIdentity,
    epoch: u64,
    next_id: u64,
    tick: u64,
    bytes: Box<[u8]>,
    lines: Box<[CacheLine]>,
    flights: [Option<Flight>; MAX_IN_FLIGHT],
    failure: Option<(u64, StorageError)>,
}
impl CachedSource {
    pub fn new(identity: SourceIdentity, epoch: u64) -> Self {
        Self::with_budget(identity, epoch, CACHE_BUDGET)
    }
    pub fn with_budget(identity: SourceIdentity, epoch: u64, budget: usize) -> Self {
        let count = (budget / CACHE_CHUNK).clamp(2, CACHE_BUDGET / CACHE_CHUNK);
        Self {
            identity,
            epoch,
            next_id: 1,
            tick: 0,
            bytes: vec![0; count * CACHE_CHUNK].into_boxed_slice(),
            lines: vec![
                CacheLine {
                    chunk: 0,
                    used: 0,
                    valid: false
                };
                count
            ]
            .into_boxed_slice(),
            flights: [None; MAX_IN_FLIGHT],
            failure: None,
        }
    }
    pub fn next_request(&mut self) -> Option<HostRequest> {
        let flight = self.flights.iter_mut().flatten().find(|f| !f.emitted)?;
        flight.emitted = true;
        let offset = flight.chunk * CACHE_CHUNK as u64;
        Some(HostRequest::Read {
            tag: flight.tag,
            source: self.identity.id,
            offset,
            length: (self.identity.length - offset).min(CACHE_CHUNK as u64) as u32,
        })
    }
    pub fn complete(&mut self, completion: HostCompletion) -> Result<(), StorageError> {
        let HostCompletion::Read { tag, bytes, result } = completion else {
            return Err(StorageError::StaleCompletion);
        };
        let slot = usize::from(tag.slot);
        let Some(flight) = self
            .flights
            .get(slot)
            .copied()
            .flatten()
            .filter(|f| f.tag == tag)
        else {
            return Err(StorageError::StaleCompletion);
        };
        if tag.epoch != self.epoch || tag.generation != self.identity.generation {
            return Err(StorageError::StaleCompletion);
        }
        self.flights[slot] = None;
        if let Err(error) = result {
            self.failure = Some((flight.chunk, error));
            return Ok(());
        }
        let expected = (self.identity.length - flight.chunk * CACHE_CHUNK as u64)
            .min(CACHE_CHUNK as u64) as usize;
        if bytes.len() != expected {
            self.failure = Some((flight.chunk, StorageError::Corrupt));
            return Ok(());
        }
        let index = self
            .lines
            .iter()
            .position(|line| !line.valid)
            .unwrap_or_else(|| {
                self.lines
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, line)| line.used)
                    .unwrap()
                    .0
            });
        self.bytes[index * CACHE_CHUNK..index * CACHE_CHUNK + expected].copy_from_slice(&bytes);
        self.tick = self.tick.saturating_add(1);
        self.lines[index] = CacheLine {
            chunk: flight.chunk,
            used: self.tick,
            valid: true,
        };
        Ok(())
    }
    /// Discardable reads are cancelled by changing epochs; committed writes use
    /// a separate coordinator and must be settled before reset.
    pub fn reset_epoch(&mut self, epoch: u64) {
        self.epoch = epoch;
        self.flights.fill(None);
        self.failure = None;
    }
    pub fn pending(&self) -> usize {
        self.flights.iter().flatten().count()
    }
    fn line(&self, chunk: u64) -> Option<usize> {
        self.lines
            .iter()
            .position(|line| line.valid && line.chunk == chunk)
    }
    fn request(&mut self, chunk: u64) -> Result<(), StorageError> {
        if let Some((failed, error)) = self.failure
            && failed == chunk
        {
            self.failure = None;
            return Err(error);
        }
        if self
            .flights
            .iter()
            .flatten()
            .any(|flight| flight.chunk == chunk)
        {
            return Ok(());
        }
        let Some(slot) = self.flights.iter().position(Option::is_none) else {
            return Ok(());
        };
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(StorageError::Unavailable)?;
        self.flights[slot] = Some(Flight {
            tag: RequestTag {
                epoch: self.epoch,
                id,
                generation: self.identity.generation,
                slot: slot as u16,
            },
            chunk,
            emitted: false,
        });
        Ok(())
    }
}
impl ReadAt for CachedSource {
    fn identity(&self) -> SourceIdentity {
        self.identity
    }
    fn read_at(&mut self, offset: u64, output: &mut [u8]) -> Result<usize, StorageError> {
        if offset > self.identity.length {
            return Err(StorageError::OutOfBounds);
        }
        let count = output
            .len()
            .min(usize::try_from(self.identity.length - offset).unwrap_or(usize::MAX));
        if count == 0 {
            return Ok(0);
        }
        if count > CACHE_CHUNK {
            return Err(StorageError::OutOfBounds);
        }
        let first = offset / CACHE_CHUNK as u64;
        let last = (offset + count as u64 - 1) / CACHE_CHUNK as u64;
        let mut missing = false;
        for chunk in first..=last {
            if self.line(chunk).is_none() {
                self.request(chunk)?;
                missing = true;
            }
        }
        if missing {
            return Err(StorageError::Pending);
        }
        let mut done = 0;
        while done < count {
            let absolute = offset + done as u64;
            let index = self.line(absolute / CACHE_CHUNK as u64).unwrap();
            let within = (absolute % CACHE_CHUNK as u64) as usize;
            let size = (count - done).min(CACHE_CHUNK - within);
            output[done..done + size].copy_from_slice(
                &self.bytes[index * CACHE_CHUNK + within..index * CACHE_CHUNK + within + size],
            );
            self.tick = self.tick.saturating_add(1);
            self.lines[index].used = self.tick;
            done += size;
        }
        Ok(count)
    }
}
/// Strict validation of a host HTTP range response before caching it.
/// The adapter must pin redirects and strong validators during its initial probe.
#[derive(Clone, Copy, Debug)]
pub struct RangeResponse<'a> {
    pub status: u16,
    pub start: u64,
    pub end_inclusive: u64,
    pub total: u64,
    pub body_len: usize,
    pub validator: &'a [u8; 32],
    pub encoded: bool,
}
pub fn validate_range(
    identity: SourceIdentity,
    offset: u64,
    length: u32,
    response: RangeResponse<'_>,
) -> Result<(), StorageError> {
    if response.validator != &identity.validator {
        return Err(StorageError::IdentityChanged);
    }
    if response.encoded || response.status != 206 {
        return Err(StorageError::UnsupportedRange);
    }
    let end = offset
        .checked_add(u64::from(length))
        .and_then(|end| end.checked_sub(1))
        .ok_or(StorageError::OutOfBounds)?;
    if response.start != offset
        || response.end_inclusive != end
        || response.total != identity.length
        || response.body_len != length as usize
    {
        return Err(StorageError::Corrupt);
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoreCapabilities {
    pub persistent: bool,
    pub atomic_commit: bool,
    pub max_batch: usize,
    pub schema: u32,
}
/// Reference transactional RAM backend; persistence is explicitly false.
pub struct RamStore {
    pub namespace: [u8; 32],
    pub base: [u8; 32],
    pub fencing_token: u64,
    pub revision: u64,
    payload: Vec<u8>,
    history: Vec<(u64, u64, u64, u64)>,
    max_batch: usize,
}
impl RamStore {
    pub fn new(namespace: [u8; 32], base: [u8; 32], max_batch: usize) -> Self {
        Self {
            namespace,
            base,
            fencing_token: 1,
            revision: 0,
            payload: Vec::with_capacity(max_batch),
            history: Vec::with_capacity(256),
            max_batch,
        }
    }
    pub fn capabilities(&self) -> StoreCapabilities {
        StoreCapabilities {
            persistent: false,
            atomic_commit: true,
            max_batch: self.max_batch,
            schema: SCHEMA_VERSION,
        }
    }
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
    pub fn commit(&mut self, batch: &CommitBatch) -> Result<u64, StorageError> {
        batch.validate(self.max_batch)?;
        if batch.namespace != self.namespace || batch.base != self.base {
            return Err(StorageError::MissingBase);
        }
        if batch.fencing_token != self.fencing_token {
            return Err(StorageError::BusyWriter);
        }
        if let Some((_, digest, expected, revision)) = self
            .history
            .iter()
            .find(|(id, _, _, _)| *id == batch.mutation_id)
        {
            return if *digest == batch.digest && *expected == batch.expected_revision {
                Ok(*revision)
            } else {
                Err(StorageError::MutationConflict)
            };
        }
        if batch.expected_revision != self.revision {
            return Err(StorageError::StaleRevision);
        }
        if self.history.len() == self.history.capacity() {
            return Err(StorageError::Quota);
        }
        let revision = self.revision.checked_add(1).ok_or(StorageError::Quota)?;
        self.payload.clear();
        self.payload.extend_from_slice(&batch.payload);
        self.revision = revision;
        self.history.push((
            batch.mutation_id,
            batch.digest,
            batch.expected_revision,
            revision,
        ));
        Ok(revision)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_exact_once_epoch_and_boundary() {
        let identity = SourceIdentity {
            id: SourceId(1),
            generation: 3,
            length: 2 * CACHE_CHUNK as u64,
            validator: [7; 32],
        };
        let mut source = CachedSource::new(identity, 1);
        let mut output = [9; 32];
        assert_eq!(
            source.read_at(CACHE_CHUNK as u64 - 16, &mut output),
            Err(StorageError::Pending)
        );
        assert_eq!(output, [9; 32]);
        let first = source.next_request().unwrap();
        let second = source.next_request().unwrap();
        assert!(source.next_request().is_none());
        for (request, byte) in [(first, 1), (second, 2)] {
            let tag = request.tag();
            source
                .complete(HostCompletion::Read {
                    tag,
                    bytes: vec![byte; CACHE_CHUNK],
                    result: Ok(()),
                })
                .unwrap();
            assert_eq!(
                source.complete(HostCompletion::Read {
                    tag,
                    bytes: Vec::new(),
                    result: Ok(())
                }),
                Err(StorageError::StaleCompletion)
            );
        }
        assert_eq!(source.read_at(CACHE_CHUNK as u64 - 16, &mut output), Ok(32));
        assert_eq!(&output[..16], &[1; 16]);
        assert_eq!(&output[16..], &[2; 16]);
        let mut source = CachedSource::new(identity, 4);
        assert_eq!(source.read_at(0, &mut output), Err(StorageError::Pending));
        let tag = source.next_request().unwrap().tag();
        source.reset_epoch(5);
        assert_eq!(
            source.complete(HostCompletion::Read {
                tag,
                bytes: vec![0; CACHE_CHUNK],
                result: Ok(())
            }),
            Err(StorageError::StaleCompletion)
        );
    }
    #[test]
    fn transactional_dedup_checks_payload_and_fencing() {
        let mut store = RamStore::new([1; 32], [2; 32], 128);
        let payload = vec![3; 10];
        let batch = CommitBatch {
            namespace: [1; 32],
            base: [2; 32],
            fencing_token: 1,
            expected_revision: 0,
            mutation_id: 1,
            digest: payload_digest(&payload),
            payload,
        };
        assert_eq!(store.commit(&batch), Ok(1));
        assert_eq!(store.commit(&batch), Ok(1));
        let mut changed = batch.clone();
        changed.expected_revision = 1;
        assert_eq!(store.commit(&changed), Err(StorageError::MutationConflict));
        let mut changed = batch.clone();
        changed.payload[0] = 4;
        changed.digest = payload_digest(&changed.payload);
        assert_eq!(store.commit(&changed), Err(StorageError::MutationConflict));
        changed.mutation_id = 2;
        assert_eq!(store.commit(&changed), Err(StorageError::StaleRevision));
        changed.expected_revision = 1;
        changed.fencing_token = 0;
        assert_eq!(store.commit(&changed), Err(StorageError::BusyWriter));
        assert_eq!(store.payload(), &[3; 10]);
    }
}
