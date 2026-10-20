//! Bounded, allocation-free inspection of little-endian LE files.
//!
//! Format references (field layouts and the exceptional big-endian page number):
//! <https://github.com/open-watcom/open-watcom-v2/blob/master/bld/watcom/h/exeflat.h>
//! <https://github.com/open-watcom/open-watcom-v2/blob/master/bld/wl/c/loadflat.c>
//! LE fixup-page association (unlike LX, indexed by the mapped page number):
//! <https://github.com/amindlost/dos32a/blob/master/src/dos32a/loader.asm>
//!
//! Inspection validates the supported structural subset, not executable semantics.
//! In particular, fixup records, import names, exports and module directives are
//! opaque. No relocation is applied and no guest image is allocated.
//! Only enumerated page-map entries are supported. Invalid and zero-fill entries
//! are rejected until their physical-page layout is established; an object's
//! virtual size may still include an implicit BSS tail beyond its mapped pages.

use core::{fmt, ops::Range};

use crate::{ExecutableError, MzImage};

const HEADER_SIZE: usize = 196;
const OBJECT_SIZE: usize = 24;
const RESOURCE_SIZE: usize = 14;
const MAX_CONTAINERS: usize = 64;

/// Explicit denial-of-service limits. These are inspection policy, not format maxima.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeLimits {
    pub max_file_bytes: usize,
    /// Total bytes in both discovery scans (nested MZ declarations, then LE/LX).
    pub max_scan_bytes: usize,
    /// Combined number of encountered MZ/LE/LX signatures, including false hits.
    pub max_candidates: usize,
    pub max_objects: u32,
    pub max_pages: u32,
    pub max_resources: u32,
    pub max_page_size: u32,
    pub max_virtual_bytes: u64,
    /// Shared across discovery candidates; includes pairwise object comparisons.
    pub max_validation_steps: usize,
}

impl Default for LeLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 64 * 1024 * 1024,
            max_scan_bytes: 128 * 1024 * 1024,
            max_candidates: 4096,
            max_objects: 1024,
            max_pages: 262_144,
            max_resources: 65_536,
            max_page_size: 65_536,
            max_virtual_bytes: 256 * 1024 * 1024,
            max_validation_steps: 2_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeLimit {
    FileBytes,
    ScanBytes,
    Candidates,
    Containers,
    Objects,
    Pages,
    Resources,
    PageSize,
    VirtualBytes,
    ValidationSteps,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeError {
    Mz(ExecutableError),
    NotFound,
    /// Absolute header positions, which may coincide for distinct MZ data origins.
    Ambiguous {
        first: usize,
        second: usize,
    },
    Limit(LeLimit),
    Truncated(&'static str),
    Invalid(&'static str),
    UnsupportedLx,
    UnsupportedByteOrder,
    UnsupportedLevel(u32),
    UnsupportedCpu(u16),
    UnsupportedPageEncoding(u8),
}

impl fmt::Display for LeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mz(error) => write!(f, "DOS envelope: {error}"),
            Self::NotFound => f.write_str("no supported, structurally valid LE image found"),
            Self::Ambiguous { first, second } => {
                write!(f, "ambiguous LE images at {first:#x} and {second:#x}")
            }
            Self::Limit(limit) => write!(f, "LE inspection limit exceeded: {limit:?}"),
            Self::Truncated(field) => write!(f, "truncated LE {field}"),
            Self::Invalid(field) => write!(f, "invalid LE {field}"),
            Self::UnsupportedLx => f.write_str("LX inspection is unsupported"),
            Self::UnsupportedByteOrder => f.write_str("unsupported LE byte or word order"),
            Self::UnsupportedLevel(level) => write!(f, "unsupported LE format level {level}"),
            Self::UnsupportedCpu(cpu) => write!(f, "unsupported LE CPU type {cpu}"),
            Self::UnsupportedPageEncoding(flags) => {
                write!(f, "unsupported LE page encoding {flags:#x}")
            }
        }
    }
}
impl core::error::Error for LeError {}

/// Both positions are absolute in the supplied file. Data, debug and nonresident
/// name offsets are relative to `container_offset`; tables are header-relative.
/// A raw LE file has both positions zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeLocation {
    pub container_offset: usize,
    pub header_offset: usize,
}

/// Object numbers in on-disk metadata are one-based, including these references.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeObjectOffset {
    pub object_number: u32,
    pub offset: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeHeader {
    pub location: LeLocation,
    pub cpu_type: u16,
    pub os_type: u16,
    pub module_version: u32,
    pub module_flags: u32,
    pub page_count: u32,
    pub page_size: u32,
    /// Zero in the file denotes a full final physical page.
    pub last_page_bytes: u32,
    pub object_count: u32,
    pub entry: Option<LeObjectOffset>,
    pub stack: Option<LeObjectOffset>,
    pub auto_data_object: Option<u32>,
    /// Resolved absolute file offset, including the containing MZ origin.
    pub data_pages_offset: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeObject {
    pub virtual_size: u32,
    pub preferred_base: u32,
    pub flags: u32,
    /// One-based index in the object page map; zero is allowed only with no pages.
    pub first_page_number: u32,
    pub page_count: u32,
}

impl LeObject {
    pub fn is_executable(self) -> bool {
        self.flags & 4 != 0
    }

    pub fn is_writable(self) -> bool {
        self.flags & 2 != 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LePageData<'a> {
    Enumerated(&'a [u8]),
    /// Reserved for future support; currently never constructed by `LeImage`.
    ZeroFill,
    /// Reserved for future support; currently never constructed by `LeImage`.
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LePage<'a> {
    /// One-based physical page number. Currently only enumerated pages are supported.
    pub physical_page_number: u32,
    pub data: LePageData<'a>,
    /// Borrowed records for this logical page, with no interpretation or relocation.
    pub fixup_records: &'a [u8],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeResource {
    pub type_id: u16,
    pub name_id: u16,
    pub byte_length: u32,
    pub object_number: u16,
    pub offset: u32,
}

/// Immutable borrowed views. Construction validates every exposed fixed-size table.
/// Accessor indices are zero-based; stored LE object/page references stay one-based.
#[derive(Debug)]
pub struct LeImage<'a> {
    bytes: &'a [u8],
    header: LeHeader,
    objects: &'a [u8],
    pages: &'a [u8],
    resources: &'a [u8],
    fixup_pages: &'a [u8],
    fixup_records: &'a [u8],
    loader: &'a [u8],
    fixups: &'a [u8],
}

impl<'a> LeImage<'a> {
    /// Inspect a specific header. `bytes` is the entire file, since data-page
    /// offsets use the explicit containing-image origin for bound DOS extenders.
    pub fn parse_at(
        bytes: &'a [u8],
        location: LeLocation,
        limits: LeLimits,
    ) -> Result<Self, LeError> {
        let mut work = Work(limits.max_validation_steps);
        Self::parse(bytes, location, limits, &mut work)
    }

    /// Inspect raw LE, or discover a unique supported LE in a validated MZ file.
    ///
    /// Bound DOS extenders can use an old MZ relocation header with no e_lfanew.
    /// Therefore every LE magic after the MZ header is checked, including those
    /// inside the DOS load module. Magic alone never establishes a candidate.
    /// Invalid/unsupported incidental candidates do not hide a valid image. With
    /// no valid image, return the declared LE header's error if one exists, else
    /// `NotFound`; use `parse_at` for precise diagnostics at a known offset.
    /// A declared LX is rejected directly. At most 64 declared MZ containers
    /// are retained on the stack; nested old-MZ images without a declaration
    /// require an explicit `parse_at` location. Limits are shared by candidates;
    /// exhausting one prevents a uniqueness claim and is always an error.
    pub fn discover(bytes: &'a [u8], limits: LeLimits) -> Result<Self, LeError> {
        check_limit(
            bytes.len() as u64,
            limits.max_file_bytes as u64,
            LeLimit::FileBytes,
        )?;
        if bytes.starts_with(b"LE") || bytes.starts_with(b"LX") {
            return Self::parse_at(
                bytes,
                LeLocation {
                    container_offset: 0,
                    header_offset: 0,
                },
                limits,
            );
        }
        let mz = MzImage::parse_envelope(bytes).map_err(LeError::Mz)?;
        let outer_declaration = declaration(&mz, 0);
        if outer_declaration.is_some_and(|location| {
            &bytes[location.header_offset..location.header_offset + 2] == b"LX"
        }) {
            return Err(LeError::UnsupportedLx);
        }
        let start = mz.header.header_bytes;
        let scan_bytes = (bytes.len() - start)
            .checked_mul(2)
            .ok_or(LeError::Limit(LeLimit::ScanBytes))?;
        check_limit(
            scan_bytes as u64,
            limits.max_scan_bytes as u64,
            LeLimit::ScanBytes,
        )?;
        let mut containers = [None; MAX_CONTAINERS];
        let mut container_count = 0;
        if let Some(location) = outer_declaration {
            containers[0] = Some(location);
            container_count = 1;
        }
        let mut candidates = 0u64;
        // Bound programs may contain another MZ whose absolute offsets use its
        // own origin. Accept that origin only with a validated DOS header and
        // a declared LE/LX pointer, never by making a page range fit the file.
        for (index, magic) in bytes[start..].windows(2).enumerate() {
            if magic != b"MZ" {
                continue;
            }
            candidates += 1;
            check_limit(
                candidates,
                limits.max_candidates as u64,
                LeLimit::Candidates,
            )?;
            let origin = start + index;
            if let Ok(nested) = MzImage::parse_envelope(&bytes[origin..])
                && let Some(location) = declaration(&nested, origin)
            {
                if container_count == MAX_CONTAINERS {
                    return Err(LeError::Limit(LeLimit::Containers));
                }
                containers[container_count] = Some(location);
                container_count += 1;
            }
        }
        let mut work = Work(limits.max_validation_steps);
        let mut found: Option<Self> = None;
        let mut declared_error = None;
        for (index, magic) in bytes[start..].windows(2).enumerate() {
            if magic != b"LE" && magic != b"LX" {
                continue;
            }
            candidates += 1;
            check_limit(
                candidates,
                limits.max_candidates as u64,
                LeLimit::Candidates,
            )?;
            let offset = start + index;
            let declared = containers[..container_count]
                .iter()
                .flatten()
                .any(|location| location.header_offset == offset);
            // Unbound incidental LX magic is not a container declaration.
            if magic == b"LX" && !declared {
                continue;
            }
            let fallback = LeLocation {
                container_offset: 0,
                header_offset: offset,
            };
            for location in containers[..container_count]
                .iter()
                .flatten()
                .copied()
                .filter(|location| location.header_offset == offset)
                .chain((!declared).then_some(fallback))
            {
                match Self::parse(bytes, location, limits, &mut work) {
                    Ok(image) => {
                        if let Some(first) = &found {
                            if first.header.location == location {
                                continue;
                            }
                            return Err(LeError::Ambiguous {
                                first: first.header.location.header_offset,
                                second: offset,
                            });
                        }
                        found = Some(image);
                    }
                    Err(error @ LeError::Limit(_)) => return Err(error),
                    Err(error) => {
                        if declared && declared_error.is_none() {
                            declared_error = Some(error);
                        }
                    }
                }
            }
        }
        found.ok_or(declared_error.unwrap_or(LeError::NotFound))
    }

    pub fn header(&self) -> LeHeader {
        self.header
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// Declared loader section, including opaque names, exports and directives.
    /// Some producers include the complete trailing fixup section in this envelope.
    pub fn loader_section(&self) -> &'a [u8] {
        self.loader
    }

    /// Raw fixup section, including opaque import tables.
    pub fn fixup_section(&self) -> &'a [u8] {
        self.fixups
    }

    pub fn object(&self, index: usize) -> Option<LeObject> {
        let record = record(self.objects, index, OBJECT_SIZE)?;
        Some(LeObject {
            virtual_size: read32(record, 0),
            preferred_base: read32(record, 4),
            flags: read32(record, 8),
            first_page_number: read32(record, 12),
            page_count: read32(record, 16),
        })
    }

    pub fn objects(&self) -> impl ExactSizeIterator<Item = LeObject> + '_ {
        (0..self.objects.len() / OBJECT_SIZE).map(|index| self.object(index).unwrap())
    }

    pub fn page(&self, index: usize) -> Option<LePage<'a>> {
        let record = record(self.pages, index, 4)?;
        let number = page_number(record);
        let start =
            self.header.data_pages_offset + (number as usize - 1) * self.header.page_size as usize;
        let length = if number == self.header.page_count {
            self.header.last_page_bytes
        } else {
            self.header.page_size
        } as usize;
        let data = LePageData::Enumerated(&self.bytes[start..start + length]);
        // LE's page number indexes both physical data and the fixup page table.
        // The accessor index is the logical position in the object page map;
        // using that position here would associate reordered pages with the
        // wrong relocation records. LX uses a different association.
        let fixup_index = number as usize - 1;
        let begin = read32(self.fixup_pages, fixup_index * 4) as usize;
        let end = read32(self.fixup_pages, (fixup_index + 1) * 4) as usize;
        Some(LePage {
            physical_page_number: number,
            data,
            fixup_records: &self.fixup_records[begin..end],
        })
    }

    pub fn pages(&self) -> impl ExactSizeIterator<Item = LePage<'a>> + '_ {
        (0..self.pages.len() / 4).map(|index| self.page(index).unwrap())
    }

    pub fn resource(&self, index: usize) -> Option<LeResource> {
        let record = record(self.resources, index, RESOURCE_SIZE)?;
        Some(LeResource {
            type_id: read16(record, 0),
            name_id: read16(record, 2),
            byte_length: read32(record, 4),
            object_number: read16(record, 8),
            offset: read32(record, 10),
        })
    }

    pub fn resources(&self) -> impl ExactSizeIterator<Item = LeResource> + '_ {
        (0..self.resources.len() / RESOURCE_SIZE).map(|index| self.resource(index).unwrap())
    }

    fn parse(
        bytes: &'a [u8],
        location: LeLocation,
        limits: LeLimits,
        work: &mut Work,
    ) -> Result<Self, LeError> {
        check_limit(
            bytes.len() as u64,
            limits.max_file_bytes as u64,
            LeLimit::FileBytes,
        )?;
        work.step()?;
        let offset = location.header_offset;
        if location.container_offset > offset {
            return Err(LeError::Invalid("container origin"));
        }
        let signature = slice(bytes, offset, 2, "signature")?;
        if signature == b"LX" {
            return Err(LeError::UnsupportedLx);
        }
        if signature != b"LE" {
            return Err(LeError::Invalid("signature"));
        }
        let raw = slice(bytes, offset, HEADER_SIZE, "header")?;
        if raw[2] != 0 || raw[3] != 0 {
            return Err(LeError::UnsupportedByteOrder);
        }
        if read32(raw, 4) != 0 {
            return Err(LeError::UnsupportedLevel(read32(raw, 4)));
        }
        if !matches!(read16(raw, 8), 2 | 3) {
            return Err(LeError::UnsupportedCpu(read16(raw, 8)));
        }
        let page_count = read32(raw, 20);
        let object_count = read32(raw, 68);
        let resource_count = read32(raw, 84);
        let page_size = read32(raw, 40);
        check_limit(
            object_count.into(),
            limits.max_objects.into(),
            LeLimit::Objects,
        )?;
        check_limit(page_count.into(), limits.max_pages.into(), LeLimit::Pages)?;
        check_limit(
            resource_count.into(),
            limits.max_resources.into(),
            LeLimit::Resources,
        )?;
        check_limit(
            page_size.into(),
            limits.max_page_size.into(),
            LeLimit::PageSize,
        )?;
        if object_count == 0 || !page_size.is_power_of_two() {
            return Err(LeError::Invalid("object count or page size"));
        }
        let last_page_bytes = match read32(raw, 44) {
            0 => page_size,
            count if count <= page_size => count,
            _ => return Err(LeError::Invalid("last page size")),
        };
        let loader = section(
            bytes,
            offset,
            read32(raw, 64),
            read32(raw, 56),
            "loader section",
        )?;
        let fixups = section(
            bytes,
            offset,
            read32(raw, 104),
            read32(raw, 48),
            "fixup section",
        )?;
        let header_end = offset
            .checked_add(HEADER_SIZE)
            .ok_or(LeError::Invalid("header offset"))?;
        if loader.start < header_end || fixups.start < loader.start {
            return Err(LeError::Invalid("section order or overlap"));
        }
        // Open Watcom emits disjoint sections. Older producers also include
        // the complete trailing fixup section in the loader envelope. Accept
        // those two layouts, never partial overlap; loader tables precede fixups.
        if loader.end > fixups.start && loader.end != fixups.end {
            return Err(LeError::Invalid("partial loader/fixup overlap"));
        }
        let loader_tables = loader.start..loader.end.min(fixups.start);
        let objects = table(
            offset,
            read32(raw, 64),
            object_count,
            OBJECT_SIZE,
            &loader_tables,
            "object table",
        )?;
        let pages = table(
            offset,
            read32(raw, 72),
            page_count,
            4,
            &loader_tables,
            "page map",
        )?;
        let resources = table(
            offset,
            read32(raw, 80),
            resource_count,
            RESOURCE_SIZE,
            &loader_tables,
            "resource table",
        )?;
        disjoint(&objects, &pages)?;
        disjoint(&objects, &resources)?;
        disjoint(&pages, &resources)?;
        for field in [88, 92, 96] {
            optional_pointer(
                offset,
                read32(raw, field),
                &loader_tables,
                "loader table offset",
            )?;
            if read32(raw, field) != 0 {
                let position = absolute(offset, read32(raw, field), "loader table offset")?;
                if [&objects, &pages, &resources]
                    .iter()
                    .any(|table| table.contains(&position))
                {
                    return Err(LeError::Invalid("loader table overlaps fixed table"));
                }
            }
        }
        if read32(raw, 100) != 0 && read32(raw, 96) == 0 {
            return Err(LeError::Invalid("module directive table"));
        }
        let fixup_pages = table(
            offset,
            read32(raw, 104),
            page_count
                .checked_add(1)
                .ok_or(LeError::Invalid("page count"))?,
            4,
            &fixups,
            "fixup page table",
        )?;
        let records_start = absolute(offset, read32(raw, 108), "fixup records")?;
        if records_start < fixup_pages.end || records_start > fixups.end {
            return Err(LeError::Invalid("fixup record offset"));
        }
        let mut records_end = fixups.end;
        for field in [112, 120, 124] {
            let value = read32(raw, field);
            optional_pointer(offset, value, &fixups, "fixup table offset")?;
            if value != 0 {
                let position = absolute(offset, value, "fixup table offset")?;
                if position < records_start {
                    return Err(LeError::Invalid("fixup table order"));
                }
                records_end = records_end.min(position);
            }
        }
        if read32(raw, 116) != 0 && read32(raw, 112) == 0 {
            return Err(LeError::Invalid("import module table"));
        }
        let mut previous = 0;
        for index in 0..=page_count as usize {
            work.step()?;
            let value = read32(&bytes[fixup_pages.clone()], index * 4) as usize;
            if (index == 0 && value != 0) || value < previous || value > records_end - records_start
            {
                return Err(LeError::Invalid("fixup page offsets"));
            }
            previous = value;
        }
        let data_pages_offset = absolute(
            location.container_offset,
            read32(raw, 128),
            "data pages offset",
        )?;
        let has_file_pages = bytes[pages.clone()]
            .as_chunks::<4>()
            .0
            .iter()
            .any(|record| record[3] == 0);
        if (has_file_pages && data_pages_offset < fixups.end) || data_pages_offset > bytes.len() {
            return Err(LeError::Invalid("data pages offset"));
        }
        // Absolute offsets, not relative to the LE header.
        for (position, length, name) in [(136, 140, "nonresident names"), (152, 156, "debug data")]
        {
            let start = absolute(location.container_offset, read32(raw, position), name)?;
            let length = read32(raw, length) as usize;
            if length != 0 {
                slice(bytes, start, length, name)?;
            }
        }
        if read32(raw, 132) > page_count {
            return Err(LeError::Invalid("preload page count"));
        }
        let image = Self {
            bytes,
            header: LeHeader {
                location,
                cpu_type: read16(raw, 8),
                os_type: read16(raw, 10),
                module_version: read32(raw, 12),
                module_flags: read32(raw, 16),
                page_count,
                page_size,
                last_page_bytes,
                object_count,
                entry: object_offset(read32(raw, 24), read32(raw, 28))?,
                stack: object_offset(read32(raw, 32), read32(raw, 36))?,
                auto_data_object: match read32(raw, 148) {
                    0 => None,
                    value => Some(value),
                },
                data_pages_offset,
            },
            objects: &bytes[objects],
            pages: &bytes[pages],
            resources: &bytes[resources],
            fixup_pages: &bytes[fixup_pages],
            fixup_records: &bytes[records_start..records_start + previous],
            loader: &bytes[loader],
            fixups: &bytes[fixups],
        };
        image.validate(work, limits)?;
        Ok(image)
    }

    fn validate(&self, work: &mut Work, limits: LeLimits) -> Result<(), LeError> {
        let mut virtual_bytes = 0u64;
        let mut mapped_pages = 0u64;
        for (index, object) in self.objects().enumerate() {
            work.step()?;
            if read32(self.objects, index * OBJECT_SIZE + 20) != 0 {
                return Err(LeError::Invalid("reserved object field"));
            }
            virtual_bytes += u64::from(object.virtual_size);
            check_limit(
                virtual_bytes,
                limits.max_virtual_bytes,
                LeLimit::VirtualBytes,
            )?;
            if u64::from(object.preferred_base) + u64::from(object.virtual_size) > 1u64 << 32 {
                return Err(LeError::Invalid("object virtual address overflow"));
            }
            let capacity =
                u64::from(object.virtual_size).div_ceil(u64::from(self.header.page_size));
            if object.page_count == 0
                && u64::from(object.first_page_number) > u64::from(self.header.page_count) + 1
            {
                return Err(LeError::Invalid("empty object page range"));
            }
            if u64::from(object.page_count) > capacity {
                return Err(LeError::Invalid("object page capacity"));
            }
            let range = object_pages(object)?;
            if range.end > u64::from(self.header.page_count) {
                return Err(LeError::Invalid("object page range"));
            }
            mapped_pages += u64::from(object.page_count);
            for other in self.objects().take(index) {
                work.step()?;
                let other_range = object_pages(other)?;
                if range.start < range.end
                    && other_range.start < other_range.end
                    && range.start < other_range.end
                    && other_range.start < range.end
                {
                    return Err(LeError::Invalid("overlapping object page ranges"));
                }
            }
        }
        if mapped_pages != u64::from(self.header.page_count) {
            return Err(LeError::Invalid("unowned logical pages"));
        }
        for record in self.pages.as_chunks::<4>().0 {
            work.step()?;
            let number = page_number(record);
            match record[3] {
                0 => {
                    if number == 0 || number > self.header.page_count {
                        return Err(LeError::Invalid("physical page number"));
                    }
                    let length = if number == self.header.page_count {
                        self.header.last_page_bytes
                    } else {
                        self.header.page_size
                    } as usize;
                    let displacement = (number as usize - 1)
                        .checked_mul(self.header.page_size as usize)
                        .ok_or(LeError::Invalid("physical page offset"))?;
                    let start = self
                        .header
                        .data_pages_offset
                        .checked_add(displacement)
                        .ok_or(LeError::Invalid("physical page offset"))?;
                    slice(self.bytes, start, length, "page data")?;
                }
                encoding => return Err(LeError::UnsupportedPageEncoding(encoding)),
            }
        }
        if let Some(entry) = self.header.entry {
            let object = self.referenced_object(entry.object_number)?;
            if !object.is_executable() || entry.offset >= object.virtual_size {
                return Err(LeError::Invalid("entry point"));
            }
            let object_page = entry.offset / self.header.page_size;
            if object_page >= object.page_count {
                return Err(LeError::Invalid("entry point has no file-backed page"));
            }
            let page = self
                .page((object.first_page_number - 1 + object_page) as usize)
                .unwrap();
            match page.data {
                LePageData::Enumerated(data)
                    if (entry.offset % self.header.page_size) < data.len() as u32 => {}
                _ => return Err(LeError::Invalid("entry point has no file-backed byte")),
            }
        }
        if let Some(stack) = self.header.stack {
            let object = self.referenced_object(stack.object_number)?;
            if !object.is_writable() || stack.offset > object.virtual_size {
                return Err(LeError::Invalid("stack pointer"));
            }
        }
        if let Some(number) = self.header.auto_data_object {
            self.referenced_object(number)?;
        }
        for resource in self.resources() {
            work.step()?;
            let object = self.referenced_object(resource.object_number.into())?;
            if u64::from(resource.offset) + u64::from(resource.byte_length)
                > u64::from(object.virtual_size)
            {
                return Err(LeError::Invalid("resource object range"));
            }
        }
        Ok(())
    }

    fn referenced_object(&self, number: u32) -> Result<LeObject, LeError> {
        number
            .checked_sub(1)
            .and_then(|index| self.object(index as usize))
            .ok_or(LeError::Invalid("object reference"))
    }
}

// A declaration requires an extended MZ layout, a pointer beyond the DOS
// header, and LE/LX magic at that exact pointer. Its LE contents are untrusted.
fn declaration(mz: &MzImage<'_>, origin: usize) -> Option<LeLocation> {
    if mz.header.header_bytes < 64 || (mz.header.relocation_count != 0 && read16(mz.bytes, 24) < 64)
    {
        return None;
    }
    let relative = read32(mz.bytes, 60) as usize;
    if relative < mz.header.header_bytes {
        return None;
    }
    let magic = mz.bytes.get(relative..relative.checked_add(2)?)?;
    if magic != b"LE" && magic != b"LX" {
        return None;
    }
    Some(LeLocation {
        container_offset: origin,
        header_offset: origin.checked_add(relative)?,
    })
}

fn object_offset(number: u32, offset: u32) -> Result<Option<LeObjectOffset>, LeError> {
    if number == 0 {
        if offset != 0 {
            return Err(LeError::Invalid("offset without object"));
        }
        Ok(None)
    } else {
        Ok(Some(LeObjectOffset {
            object_number: number,
            offset,
        }))
    }
}

fn object_pages(object: LeObject) -> Result<Range<u64>, LeError> {
    if object.page_count == 0 {
        return Ok(0..0);
    }
    let start = object
        .first_page_number
        .checked_sub(1)
        .ok_or(LeError::Invalid("object first page number"))?;
    Ok(u64::from(start)..u64::from(start) + u64::from(object.page_count))
}

struct Work(usize);
impl Work {
    fn step(&mut self) -> Result<(), LeError> {
        self.0 = self
            .0
            .checked_sub(1)
            .ok_or(LeError::Limit(LeLimit::ValidationSteps))?;
        Ok(())
    }
}

fn check_limit(value: u64, maximum: u64, limit: LeLimit) -> Result<(), LeError> {
    if value > maximum {
        Err(LeError::Limit(limit))
    } else {
        Ok(())
    }
}

fn absolute(base: usize, offset: u32, name: &'static str) -> Result<usize, LeError> {
    base.checked_add(offset as usize)
        .ok_or(LeError::Invalid(name))
}

fn slice<'a>(
    bytes: &'a [u8],
    start: usize,
    length: usize,
    name: &'static str,
) -> Result<&'a [u8], LeError> {
    let end = start.checked_add(length).ok_or(LeError::Invalid(name))?;
    bytes.get(start..end).ok_or(LeError::Truncated(name))
}

fn section(
    bytes: &[u8],
    base: usize,
    offset: u32,
    length: u32,
    name: &'static str,
) -> Result<Range<usize>, LeError> {
    let start = absolute(base, offset, name)?;
    slice(bytes, start, length as usize, name)?;
    Ok(start..start + length as usize)
}

fn table(
    base: usize,
    offset: u32,
    count: u32,
    size: usize,
    section: &Range<usize>,
    name: &'static str,
) -> Result<Range<usize>, LeError> {
    if count == 0 {
        optional_pointer(base, offset, section, name)?;
        return Ok(section.start..section.start);
    }
    let start = absolute(base, offset, name)?;
    let length = (count as usize)
        .checked_mul(size)
        .ok_or(LeError::Invalid(name))?;
    let end = start.checked_add(length).ok_or(LeError::Invalid(name))?;
    if start < section.start || end > section.end {
        return Err(LeError::Invalid(name));
    }
    Ok(start..end)
}

fn optional_pointer(
    base: usize,
    offset: u32,
    section: &Range<usize>,
    name: &'static str,
) -> Result<(), LeError> {
    if offset != 0 {
        let pointer = absolute(base, offset, name)?;
        if pointer < section.start || pointer > section.end {
            return Err(LeError::Invalid(name));
        }
    }
    Ok(())
}

fn disjoint(left: &Range<usize>, right: &Range<usize>) -> Result<(), LeError> {
    if !left.is_empty() && !right.is_empty() && left.start < right.end && right.start < left.end {
        Err(LeError::Invalid("overlapping tables"))
    } else {
        Ok(())
    }
}

fn record(bytes: &[u8], index: usize, size: usize) -> Option<&[u8]> {
    let start = index.checked_mul(size)?;
    bytes.get(start..start.checked_add(size)?)
}

// Call only on validated fixed-size records. LE's three-byte page number is
// big-endian even though all header/object integers are little-endian.
fn page_number(record: &[u8]) -> u32 {
    u32::from_be_bytes([0, record[0], record[1], record[2]])
}
fn read16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
fn read32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[cfg(test)]
mod tests;
