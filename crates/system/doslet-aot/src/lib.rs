//! Bounded, offline symbolic executable images for static analysis.
//!
//! This preparation layer owns reconstructed object bytes, but never loads a
//! machine. Its optional binding plan encodes offline relocations only after
//! validating an explicit caller-supplied DOS-flat layout. Object identities are
//! independent of preferred linear bases and runtime selectors. File-backed spans distinguish input bytes
//! from implicit zero-filled storage; neither executable flags nor relocation
//! targets establish that an address is reachable code.
#![no_std]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod binding;
pub mod cfg;
mod coff;
pub mod procedures;
pub mod profile;
pub mod regions;
pub mod waits;
#[cfg(feature = "wasm")]
pub mod wasm;

use alloc::vec::Vec;
use core::{cmp::Ordering, fmt, mem::size_of, ops::Range};
use doslet_executable::{
    LeFixupError, LeFixupLimits, LeFixupSourceType, LeHeader, LeImage, LeObject, LePageData,
};

/// Preparation policy, independent of the borrowed parser's limits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageLimits {
    /// Sum of requested vector storage, including temporary canonicalization
    /// storage. Allocator bookkeeping is excluded. Storage is charged before
    /// reservation and never refunded during construction.
    pub max_owned_bytes: usize,
    /// Source occurrences before deduplication, including source-list entries.
    pub max_relocation_sources: usize,
    /// One shared budget for all page fixup streams in the complete image.
    pub max_fixup_decode_steps: usize,
}

impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            max_owned_bytes: 256 * 1024 * 1024,
            max_relocation_sources: 1_000_000,
            max_fixup_decode_steps: LeFixupLimits::default().max_decode_steps,
        }
    }
}

/// One-based LE object number and an offset within that object.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjectAddress {
    pub object_number: u32,
    pub offset: u32,
}

/// An address expression, not a proof of an in-range code destination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RelocationTarget {
    pub object_number: u32,
    /// Selector-only records have no offset. Other offsets are retained
    /// verbatim, including one-past-object and larger address addends.
    pub offset: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultBitness {
    Bits16,
    Bits32,
}

/// A copied input span. The remainder of the object is implicit zero-fill.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileBackedSpan {
    pub object_offset: u32,
    pub file_offset: usize,
    pub byte_len: u32,
}

/// Logical and physical numbering are both one-based; the object page index is
/// zero-based. Records and field errors use offsets within that page's stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FixupContext {
    pub object_number: u32,
    pub object_page_index: u32,
    pub logical_page_number: u32,
    pub physical_page_number: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceProvenance {
    pub context: FixupContext,
    pub record_offset: usize,
    /// Zero-based index in the record's source list (zero for a single source).
    pub source_index: usize,
    pub page_relative_offset: i16,
}

/// An immutable view of one canonical write site and all equivalent sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relocation<'a> {
    pub source: ObjectAddress,
    pub kind: LeFixupSourceType,
    pub target: RelocationTarget,
    /// Metadata only; no alias selector is created or validated here.
    pub is_alias: bool,
    pub provenance: &'a [SourceProvenance],
}

pub use coff::coff_offset_bindings;

#[derive(Debug)]
pub struct SymbolicObject {
    object_number: u32,
    metadata: LeObject,
    bytes: Vec<u8>,
    file_backed_spans: Vec<FileBackedSpan>,
}

impl SymbolicObject {
    pub fn object_number(&self) -> u32 {
        self.object_number
    }

    /// Original metadata; `preferred_base` remains an unbound format hint.
    pub fn metadata(&self) -> LeObject {
        self.metadata
    }

    pub fn default_bitness(&self) -> DefaultBitness {
        if self.metadata.flags & 0x2000 != 0 {
            DefaultBitness::Bits32
        } else {
            DefaultBitness::Bits16
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Ordered by object offset, with one span per nonempty copied page.
    pub fn file_backed_spans(&self) -> &[FileBackedSpan] {
        &self.file_backed_spans
    }
}

#[derive(Debug)]
struct Site {
    source: ObjectAddress,
    kind: LeFixupSourceType,
    target: RelocationTarget,
    is_alias: bool,
    provenance: Range<usize>,
}

/// The executable format an image was reconstructed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFormat {
    Le,
    /// A DJGPP COFF image: objects are its sections, in section order, and
    /// their preferred bases are offsets in the program's selectors.
    Coff,
}

/// Complete owned image, published only after reconstruction and validation.
#[derive(Debug)]
pub struct SymbolicImage {
    format: ImageFormat,
    header: LeHeader,
    objects: Vec<SymbolicObject>,
    sites: Vec<Site>,
    provenance: Vec<SourceProvenance>,
}

impl SymbolicImage {
    pub fn format(&self) -> ImageFormat {
        self.format
    }

    /// Reconstruct all objects and validate every mapped fixup stream. Source
    /// writes must fit their object completely; target offsets remain symbolic.
    /// No partially prepared image is returned on any failure.
    pub fn from_le(image: &LeImage<'_>, limits: ImageLimits) -> Result<Self, ImageError> {
        let mut budget = StorageBudget(limits.max_owned_bytes);
        let mut objects = Vec::new();
        budget.reserve(&mut objects, image.objects().len(), ImageStorage::Objects)?;
        let mut pending = Vec::new();
        let mut remaining_steps = limits.max_fixup_decode_steps;
        let header = image.header();
        let page_size = host_size(header.page_size)?;
        for (index, metadata) in image.objects().enumerate() {
            let object_number = u32::try_from(index)
                .ok()
                .and_then(|index| index.checked_add(1))
                .ok_or(ImageError::Arithmetic("object number"))?;
            let mut object = SymbolicObject {
                object_number,
                metadata,
                bytes: Vec::new(),
                file_backed_spans: Vec::new(),
            };
            let length = host_size(metadata.virtual_size)?;
            budget.reserve(&mut object.bytes, length, ImageStorage::ObjectBytes)?;
            object.bytes.resize(length, 0);
            budget.reserve(
                &mut object.file_backed_spans,
                host_size(metadata.page_count)?,
                ImageStorage::FileBackedSpans,
            )?;
            for object_page_index in 0..metadata.page_count {
                let logical_page_number = metadata
                    .first_page_number
                    .checked_add(object_page_index)
                    .ok_or(ImageError::Arithmetic("logical page number"))?;
                let page_index = logical_page_number
                    .checked_sub(1)
                    .ok_or(ImageError::Arithmetic("logical page number"))?;
                let page = image
                    .page(host_size(page_index)?)
                    .ok_or(ImageError::Arithmetic("validated page reference"))?;
                let context = FixupContext {
                    object_number,
                    object_page_index,
                    logical_page_number,
                    physical_page_number: page.physical_page_number,
                };
                let page_offset = object_page_index
                    .checked_mul(header.page_size)
                    .ok_or(ImageError::Arithmetic("object page offset"))?;
                let LePageData::Enumerated(data) = page.data else {
                    return Err(ImageError::UnsupportedPage { context });
                };
                let start = host_size(page_offset)?;
                let remaining = object
                    .bytes
                    .len()
                    .checked_sub(start)
                    .ok_or(ImageError::Arithmetic("object page range"))?;
                let copied = data.len().min(remaining);
                if copied != 0 {
                    object.bytes[start..start + copied].copy_from_slice(&data[..copied]);
                    let physical_index = page
                        .physical_page_number
                        .checked_sub(1)
                        .ok_or(ImageError::Arithmetic("physical page number"))?;
                    let file_offset = host_size(physical_index)?
                        .checked_mul(page_size)
                        .and_then(|offset| header.data_pages_offset.checked_add(offset))
                        .ok_or(ImageError::Arithmetic("file-backed page offset"))?;
                    object.file_backed_spans.push(FileBackedSpan {
                        object_offset: page_offset,
                        file_offset,
                        byte_len: u32::try_from(copied)
                            .map_err(|_| ImageError::Arithmetic("file-backed length"))?,
                    });
                }
                let mut records = page
                    .fixups(
                        header.page_size,
                        LeFixupLimits {
                            max_decode_steps: remaining_steps,
                        },
                    )
                    .map_err(|error| ImageError::Fixup { context, error })?;
                for result in records.by_ref() {
                    let record = result.map_err(|error| ImageError::Fixup { context, error })?;
                    let target = RelocationTarget {
                        object_number: record.target().object_number().into(),
                        offset: record.target().offset(),
                    };
                    if target.object_number > header.object_count {
                        return Err(ImageError::MissingTargetObject {
                            context,
                            record_offset: record.record_offset(),
                            target_object_number: target.object_number,
                        });
                    }
                    for (source_index, source_offset) in record.sources().iter().enumerate() {
                        let provenance = SourceProvenance {
                            context,
                            record_offset: record.record_offset(),
                            source_index,
                            page_relative_offset: source_offset,
                        };
                        let offset = i64::from(page_offset) + i64::from(source_offset);
                        let kind = record.source_type();
                        if offset < 0
                            || offset + i64::from(kind.width()) > i64::from(metadata.virtual_size)
                        {
                            return Err(ImageError::SourceOutsideObject {
                                provenance,
                                object_offset: offset,
                                width: kind.width(),
                                virtual_size: metadata.virtual_size,
                            });
                        }
                        if pending.len() == limits.max_relocation_sources {
                            return Err(ImageError::RelocationSourceLimit);
                        }
                        reserve_source(&mut pending, limits, &mut budget)?;
                        pending.push(PendingRelocation {
                            source: ObjectAddress {
                                object_number,
                                offset: u32::try_from(offset)
                                    .map_err(|_| ImageError::Arithmetic("source object offset"))?,
                            },
                            kind,
                            target,
                            is_alias: record.is_alias(),
                            provenance,
                        });
                    }
                }
                // Exhaustion without an error is mandatory; an iterator's first
                // successful record alone never validates its remaining bytes.
                remaining_steps = records.remaining_decode_steps();
            }
            objects.push(object);
        }
        let (sites, provenance) = canonicalize(pending, &mut budget)?;
        Ok(Self {
            format: ImageFormat::Le,
            header,
            objects,
            sites,
            provenance,
        })
    }

    /// The LE header, or for COFF an equivalent one: no pages, one object
    /// per section and the entry in the text object.
    pub fn header(&self) -> LeHeader {
        self.header
    }

    pub fn objects(&self) -> &[SymbolicObject] {
        &self.objects
    }

    /// Object numbers are one-based.
    pub fn object(&self, object_number: u32) -> Option<&SymbolicObject> {
        let index = usize::try_from(object_number.checked_sub(1)?).ok()?;
        self.objects.get(index)
    }

    /// Deterministically ordered by source object and offset. Equivalent writes
    /// have one site with all original source occurrences retained as provenance.
    pub fn relocations(
        &self,
    ) -> impl ExactSizeIterator<Item = Relocation<'_>> + DoubleEndedIterator + '_ {
        self.sites.iter().map(|site| Relocation {
            source: site.source,
            kind: site.kind,
            target: site.target,
            is_alias: site.is_alias,
            provenance: &self.provenance[site.provenance.clone()],
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageStorage {
    Objects,
    ObjectBytes,
    FileBackedSpans,
    RelocationSources,
    Relocations,
    Provenance,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    OwnedByteLimit(ImageStorage),
    RelocationSourceLimit,
    Allocation(ImageStorage),
    Arithmetic(&'static str),
    UnsupportedPage {
        context: FixupContext,
    },
    Fixup {
        context: FixupContext,
        error: LeFixupError,
    },
    MissingTargetObject {
        context: FixupContext,
        record_offset: usize,
        target_object_number: u32,
    },
    SourceOutsideObject {
        provenance: SourceProvenance,
        object_offset: i64,
        width: u8,
        virtual_size: u32,
    },
    /// A COFF construct outside the supported executable subset.
    UnsupportedCoff(&'static str),
    ConflictingRelocations {
        source: ObjectAddress,
        first: SourceProvenance,
        second: SourceProvenance,
    },
    OverlappingRelocations {
        first_source: ObjectAddress,
        first: SourceProvenance,
        second_source: ObjectAddress,
        second: SourceProvenance,
    },
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OwnedByteLimit(storage) => {
                write!(f, "symbolic image owned-byte limit: {storage:?}")
            }
            Self::RelocationSourceLimit => f.write_str("symbolic image relocation-source limit"),
            Self::Allocation(storage) => write!(f, "cannot reserve symbolic image {storage:?}"),
            Self::Arithmetic(field) => write!(f, "symbolic image arithmetic overflow: {field}"),
            Self::UnsupportedPage { context } => write!(f, "unsupported page at {context:?}"),
            Self::Fixup { context, error } => write!(f, "{context:?}: {error}"),
            Self::MissingTargetObject {
                context,
                record_offset,
                target_object_number,
            } => write!(
                f,
                "{context:?}, record {record_offset:#x}: missing target object {target_object_number}"
            ),
            Self::SourceOutsideObject {
                provenance,
                object_offset,
                width,
                virtual_size,
            } => write!(
                f,
                "{provenance:?}: write at {object_offset} of width {width} exceeds object size {virtual_size}"
            ),
            Self::UnsupportedCoff(reason) => write!(f, "unsupported COFF image: {reason}"),
            Self::ConflictingRelocations {
                source,
                first,
                second,
            } => write!(
                f,
                "conflicting relocations at {source:?}: {first:?} and {second:?}"
            ),
            Self::OverlappingRelocations {
                first_source,
                first,
                second_source,
                second,
            } => write!(
                f,
                "overlapping relocations at {first_source:?} ({first:?}) and {second_source:?} ({second:?})"
            ),
        }
    }
}

impl core::error::Error for ImageError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        if let Self::Fixup { error, .. } = self {
            Some(error)
        } else {
            None
        }
    }
}

struct StorageBudget(usize);

impl StorageBudget {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        capacity: usize,
        storage: ImageStorage,
    ) -> Result<(), ImageError> {
        let additional = capacity.saturating_sub(values.capacity());
        let bytes = additional
            .checked_mul(size_of::<T>())
            .ok_or(ImageError::OwnedByteLimit(storage))?;
        self.0 = self
            .0
            .checked_sub(bytes)
            .ok_or(ImageError::OwnedByteLimit(storage))?;
        if additional != 0 {
            values
                .try_reserve_exact(capacity - values.len())
                .map_err(|_| ImageError::Allocation(storage))?;
        }
        Ok(())
    }
}

fn host_size(value: u32) -> Result<usize, ImageError> {
    usize::try_from(value).map_err(|_| ImageError::Arithmetic("host size"))
}

#[derive(Clone, Copy)]
struct PendingRelocation {
    source: ObjectAddress,
    kind: LeFixupSourceType,
    target: RelocationTarget,
    is_alias: bool,
    provenance: SourceProvenance,
}

impl PendingRelocation {
    fn compare_site(&self, other: &Self) -> Ordering {
        (self.source, self.kind as u8, self.target, self.is_alias).cmp(&(
            other.source,
            other.kind as u8,
            other.target,
            other.is_alias,
        ))
    }
}

fn reserve_source(
    pending: &mut Vec<PendingRelocation>,
    limits: ImageLimits,
    budget: &mut StorageBudget,
) -> Result<(), ImageError> {
    if pending.len() == pending.capacity() {
        let needed = pending.len() + 1; // Source-count limit was checked first.
        let desired = pending
            .capacity()
            .saturating_mul(2)
            .max(16)
            .min(limits.max_relocation_sources);
        let affordable = budget.0 / size_of::<PendingRelocation>();
        let capacity = desired.min(pending.capacity().saturating_add(affordable));
        if capacity < needed {
            return Err(ImageError::OwnedByteLimit(ImageStorage::RelocationSources));
        }
        budget.reserve(pending, capacity, ImageStorage::RelocationSources)?;
    }
    Ok(())
}

fn canonicalize(
    mut pending: Vec<PendingRelocation>,
    budget: &mut StorageBudget,
) -> Result<(Vec<Site>, Vec<SourceProvenance>), ImageError> {
    // Unstable sorting has bounded stack usage and needs no heap scratch buffer.
    pending.sort_unstable_by(|a, b| {
        a.compare_site(b)
            .then_with(|| a.provenance.cmp(&b.provenance))
    });
    let mut previous: Option<&PendingRelocation> = None;
    let mut site_count = 0;
    for current in &pending {
        if let Some(first) = previous {
            if first.compare_site(current) == Ordering::Equal {
                continue;
            }
            if first.source == current.source {
                return Err(ImageError::ConflictingRelocations {
                    source: current.source,
                    first: first.provenance,
                    second: current.provenance,
                });
            }
            if first.source.object_number == current.source.object_number
                && u64::from(current.source.offset)
                    < u64::from(first.source.offset) + u64::from(first.kind.width())
            {
                return Err(ImageError::OverlappingRelocations {
                    first_source: first.source,
                    first: first.provenance,
                    second_source: current.source,
                    second: current.provenance,
                });
            }
        }
        site_count += 1;
        previous = Some(current);
    }
    let mut sites: Vec<Site> = Vec::new();
    let mut provenance = Vec::new();
    budget.reserve(&mut sites, site_count, ImageStorage::Relocations)?;
    budget.reserve(&mut provenance, pending.len(), ImageStorage::Provenance)?;
    let mut previous: Option<PendingRelocation> = None;
    for source in pending {
        let duplicate =
            previous.is_some_and(|first| first.compare_site(&source) == Ordering::Equal);
        provenance.push(source.provenance);
        if duplicate {
            // A duplicate always follows a site created by the preceding source.
            if let Some(site) = sites.last_mut() {
                site.provenance.end = provenance.len();
            }
        } else {
            sites.push(Site {
                source: source.source,
                kind: source.kind,
                target: source.target,
                is_alias: source.is_alias,
                provenance: provenance.len() - 1..provenance.len(),
            });
        }
        previous = Some(source);
    }
    Ok((sites, provenance))
}

#[cfg(test)]
mod tests;
