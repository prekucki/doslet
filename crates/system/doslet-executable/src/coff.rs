//! Bounded, allocation-free inspection of DJGPP v2 (go32) COFF executables.
//!
//! A DJGPP program is a small real-mode MZ stub followed by an i386 COFF
//! image. The stub allocates DPMI memory, creates code and data selectors
//! whose base is that block, and copies each section to its virtual address
//! inside the block. Sections are linked at fixed virtual addresses and an
//! executable has no relocations, so the loaded bytes equal the file bytes
//! until the program changes them.
//!
//! Format references (headers, flags and the symbol table):
//! <https://www.delorie.com/djgpp/doc/coff/> and the stub's loader
//! `stub.asm` in the DJGPP sources. Section file offsets are relative to
//! the COFF header, not to the start of the MZ file.
//!
//! Inspection validates the structure only: no section is loaded, no symbol
//! establishes reachable code, and line numbers and relocations stay opaque.

use core::fmt;

use crate::{ExecutableError, MzImage};

const FILE_HEADER: usize = 20;
const OPTIONAL_HEADER: usize = 28;
const SECTION_HEADER: usize = 40;
const SYMBOL: usize = 18;

/// `f_magic` of an i386 COFF file.
pub const I386_MAGIC: u16 = 0x014c;
/// `a.out` optional header magic of a demand-paged executable.
pub const ZMAGIC: u16 = 0x010b;
/// `f_flags`: relocation information was stripped.
pub const F_RELFLG: u16 = 0x0001;
/// `f_flags`: the file is executable (no unresolved external references).
pub const F_EXEC: u16 = 0x0002;
/// Section flags.
pub const STYP_TEXT: u32 = 0x20;
pub const STYP_DATA: u32 = 0x40;
pub const STYP_BSS: u32 = 0x80;

/// Explicit denial-of-service limits, not format maxima.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoffLimits {
    pub max_file_bytes: usize,
    pub max_sections: u16,
    pub max_symbols: u32,
    /// Highest exclusive virtual end of any section.
    pub max_virtual_bytes: u64,
}

impl Default for CoffLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 64 * 1024 * 1024,
            max_sections: 32,
            max_symbols: 1_000_000,
            max_virtual_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoffLimit {
    FileBytes,
    Sections,
    Symbols,
    VirtualBytes,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoffError {
    Mz(ExecutableError),
    /// No COFF header follows the MZ load module.
    NotFound,
    Limit(CoffLimit),
    Truncated(&'static str),
    Invalid(&'static str),
    UnsupportedMachine(u16),
    UnsupportedOptionalHeader(u16),
}

impl fmt::Display for CoffError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mz(error) => write!(f, "DOS stub: {error}"),
            Self::NotFound => f.write_str("no COFF image follows the DOS stub"),
            Self::Limit(limit) => write!(f, "COFF inspection limit exceeded: {limit:?}"),
            Self::Truncated(field) => write!(f, "truncated COFF {field}"),
            Self::Invalid(field) => write!(f, "invalid COFF {field}"),
            Self::UnsupportedMachine(magic) => write!(f, "unsupported COFF machine {magic:#06x}"),
            Self::UnsupportedOptionalHeader(magic) => {
                write!(f, "unsupported COFF optional header {magic:#06x}")
            }
        }
    }
}
impl core::error::Error for CoffError {}

/// File and `a.out` header fields. `offset` is the absolute position of the
/// COFF header in the supplied file; section and symbol offsets in the file
/// are relative to it, and the accessors below resolve them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoffHeader {
    pub offset: usize,
    pub section_count: u16,
    pub timestamp: u32,
    pub flags: u16,
    pub text_size: u32,
    pub data_size: u32,
    pub bss_size: u32,
    /// Initial EIP, an offset in the code selector.
    pub entry: u32,
    pub text_start: u32,
    pub data_start: u32,
    /// Absolute file offset of the symbol table, when it has symbols.
    pub symbols_offset: Option<usize>,
    pub symbol_count: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoffSection<'a> {
    pub name: [u8; 8],
    /// Offset in the program's selectors, relative to their common base.
    pub virtual_address: u32,
    pub size: u32,
    pub flags: u32,
    pub relocation_count: u16,
    /// Absolute file position of the data; `None` for zero-filled sections.
    pub file_offset: Option<usize>,
    /// Borrowed section bytes; empty for zero-filled sections.
    pub data: &'a [u8],
}

impl CoffSection<'_> {
    pub fn is_text(&self) -> bool {
        self.flags & STYP_TEXT != 0
    }
    pub fn is_data(&self) -> bool {
        self.flags & STYP_DATA != 0
    }
    pub fn is_bss(&self) -> bool {
        self.flags & STYP_BSS != 0
    }
    /// The name without trailing NULs.
    pub fn name(&self) -> &[u8] {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(8);
        &self.name[..end]
    }
}

/// One primary symbol-table entry; auxiliary entries are skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoffSymbol<'a> {
    /// Short name, or a string-table name; empty when it cannot be resolved.
    pub name: &'a [u8],
    pub value: u32,
    /// One-based section number; zero is undefined, negative is absolute/debug.
    pub section: i16,
    pub kind: u16,
    pub storage_class: u8,
}

impl CoffSymbol<'_> {
    /// External or static symbol defined in a section, the kind a linker
    /// uses for functions and global variables.
    pub fn is_defined_global_or_static(&self) -> bool {
        self.section > 0 && matches!(self.storage_class, 2 | 3)
    }
    /// The derived type marks a function (`DT_FCN` in the low type bits).
    pub fn is_function(&self) -> bool {
        (self.kind >> 4) & 3 == 2
    }
}

/// Immutable borrowed views, validated at construction.
#[derive(Debug)]
pub struct CoffImage<'a> {
    bytes: &'a [u8],
    header: CoffHeader,
    sections: &'a [u8],
    symbols: &'a [u8],
    strings: &'a [u8],
}

impl<'a> CoffImage<'a> {
    /// Inspect a raw COFF file, or the COFF image that follows the load module
    /// of an MZ stub, as DJGPP's `stubify` produces.
    pub fn discover(bytes: &'a [u8], limits: CoffLimits) -> Result<Self, CoffError> {
        if bytes.len() > limits.max_file_bytes {
            return Err(CoffError::Limit(CoffLimit::FileBytes));
        }
        if bytes.starts_with(&I386_MAGIC.to_le_bytes()) {
            return Self::parse_at(bytes, 0, limits);
        }
        let mz = MzImage::parse(bytes).map_err(CoffError::Mz)?;
        if mz.extended.is_some() {
            return Err(CoffError::NotFound);
        }
        let offset = mz.header.image_bytes;
        if bytes.get(offset..offset + 2) == Some(&I386_MAGIC.to_le_bytes()[..]) {
            Self::parse_at(bytes, offset, limits)
        } else {
            Err(CoffError::NotFound)
        }
    }

    /// Inspect the COFF header at an explicit absolute file offset.
    pub fn parse_at(bytes: &'a [u8], offset: usize, limits: CoffLimits) -> Result<Self, CoffError> {
        if bytes.len() > limits.max_file_bytes {
            return Err(CoffError::Limit(CoffLimit::FileBytes));
        }
        let file = slice(bytes, offset, FILE_HEADER, "file header")?;
        let magic = read16(file, 0);
        if magic != I386_MAGIC {
            return Err(CoffError::UnsupportedMachine(magic));
        }
        let section_count = read16(file, 2);
        let symbol_pointer = read32(file, 8);
        let symbol_count = read32(file, 12);
        let optional_size = usize::from(read16(file, 16));
        let flags = read16(file, 18);
        if section_count == 0 {
            return Err(CoffError::Invalid("section count"));
        }
        if section_count > limits.max_sections {
            return Err(CoffError::Limit(CoffLimit::Sections));
        }
        if symbol_count > limits.max_symbols {
            return Err(CoffError::Limit(CoffLimit::Symbols));
        }
        if flags & F_EXEC == 0 {
            return Err(CoffError::Invalid("non-executable object file"));
        }
        if optional_size != OPTIONAL_HEADER {
            return Err(CoffError::Invalid("optional header size"));
        }
        let optional = slice(
            bytes,
            offset + FILE_HEADER,
            OPTIONAL_HEADER,
            "optional header",
        )?;
        let optional_magic = read16(optional, 0);
        if optional_magic != ZMAGIC {
            return Err(CoffError::UnsupportedOptionalHeader(optional_magic));
        }
        let sections_start = offset + FILE_HEADER + OPTIONAL_HEADER;
        let sections = slice(
            bytes,
            sections_start,
            usize::from(section_count) * SECTION_HEADER,
            "section table",
        )?;
        let (symbols_offset, symbols, strings) = if symbol_count == 0 {
            (None, &bytes[..0], &bytes[..0])
        } else {
            let start = offset
                .checked_add(symbol_pointer as usize)
                .ok_or(CoffError::Invalid("symbol table offset"))?;
            let length = (symbol_count as usize)
                .checked_mul(SYMBOL)
                .ok_or(CoffError::Limit(CoffLimit::Symbols))?;
            let symbols = slice(bytes, start, length, "symbol table")?;
            // The string table follows; its first word counts itself. A file
            // without long names may omit it entirely.
            let end = start + length;
            let strings = match bytes.get(end..end + 4) {
                Some(size) => {
                    let size = read32(size, 0) as usize;
                    if size < 4 {
                        &bytes[..0]
                    } else {
                        slice(bytes, end, size, "string table")?
                    }
                }
                None => &bytes[..0],
            };
            (Some(start), symbols, strings)
        };
        let header = CoffHeader {
            offset,
            section_count,
            timestamp: read32(file, 4),
            flags,
            text_size: read32(optional, 4),
            data_size: read32(optional, 8),
            bss_size: read32(optional, 12),
            entry: read32(optional, 16),
            text_start: read32(optional, 20),
            data_start: read32(optional, 24),
            symbols_offset,
            symbol_count,
        };
        let image = Self {
            bytes,
            header,
            sections,
            symbols,
            strings,
        };
        image.validate(limits)?;
        Ok(image)
    }

    fn validate(&self, limits: CoffLimits) -> Result<(), CoffError> {
        let mut text = 0;
        for index in 0..usize::from(self.header.section_count) {
            let raw = record(self.sections, index);
            let size = read32(raw, 16);
            let virtual_address = read32(raw, 12);
            let end = u64::from(virtual_address) + u64::from(size);
            if end > limits.max_virtual_bytes {
                return Err(CoffError::Limit(CoffLimit::VirtualBytes));
            }
            let flags = read32(raw, 36);
            if flags & STYP_TEXT != 0 {
                text += 1;
            }
            if flags & STYP_BSS == 0 && size != 0 {
                let start = self
                    .header
                    .offset
                    .checked_add(read32(raw, 20) as usize)
                    .ok_or(CoffError::Invalid("section data offset"))?;
                slice(self.bytes, start, size as usize, "section data")?;
            }
            // Loaded extents must not overlap: the stub copies every section
            // into one address space.
            for other in 0..index {
                let other = record(self.sections, other);
                let (start, length) = (read32(other, 12), read32(other, 16));
                if size != 0
                    && length != 0
                    && u64::from(virtual_address) < u64::from(start) + u64::from(length)
                    && u64::from(start) < end
                {
                    return Err(CoffError::Invalid("overlapping sections"));
                }
            }
        }
        if text != 1 {
            return Err(CoffError::Invalid("text section count"));
        }
        let entry = self.header.entry;
        if !self.sections().any(|s| {
            s.is_text() && entry >= s.virtual_address && entry - s.virtual_address < s.size
        }) {
            return Err(CoffError::Invalid("entry outside the text section"));
        }
        Ok(())
    }

    pub fn header(&self) -> CoffHeader {
        self.header
    }

    pub fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub fn section(&self, index: usize) -> Option<CoffSection<'a>> {
        if index >= usize::from(self.header.section_count) {
            return None;
        }
        let raw = record(self.sections, index);
        let mut name = [0; 8];
        name.copy_from_slice(&raw[..8]);
        let size = read32(raw, 16);
        let flags = read32(raw, 36);
        let (file_offset, data) = if flags & STYP_BSS != 0 || size == 0 {
            (None, &self.bytes[..0])
        } else {
            let start = self.header.offset + read32(raw, 20) as usize;
            (Some(start), &self.bytes[start..start + size as usize])
        };
        Some(CoffSection {
            name,
            virtual_address: read32(raw, 12),
            size,
            flags,
            relocation_count: read16(raw, 32),
            file_offset,
            data,
        })
    }

    pub fn sections(&self) -> impl ExactSizeIterator<Item = CoffSection<'a>> + '_ {
        (0..usize::from(self.header.section_count)).map(|index| self.section(index).unwrap())
    }

    /// The only text section.
    pub fn text(&self) -> CoffSection<'a> {
        self.sections()
            .find(CoffSection::is_text)
            .expect("validated text section")
    }

    /// Primary symbols in table order, skipping auxiliary entries.
    pub fn symbols(&self) -> impl Iterator<Item = CoffSymbol<'a>> + '_ {
        let mut index = 0usize;
        let count = self.symbols.len() / SYMBOL;
        core::iter::from_fn(move || {
            if index >= count {
                return None;
            }
            let raw = &self.symbols[index * SYMBOL..(index + 1) * SYMBOL];
            index += 1 + usize::from(raw[17]);
            let name = if read32(raw, 0) == 0 {
                let start = read32(raw, 4) as usize;
                self.strings
                    .get(start..)
                    .map(|tail| &tail[..tail.iter().position(|&b| b == 0).unwrap_or(tail.len())])
                    .unwrap_or(&[])
            } else {
                let end = raw[..8].iter().position(|&b| b == 0).unwrap_or(8);
                &raw[..end]
            };
            Some(CoffSymbol {
                name,
                value: read32(raw, 8),
                section: read16(raw, 12) as i16,
                kind: read16(raw, 14),
                storage_class: raw[16],
            })
        })
    }
}

fn slice<'a>(
    bytes: &'a [u8],
    start: usize,
    length: usize,
    field: &'static str,
) -> Result<&'a [u8], CoffError> {
    bytes
        .get(
            start
                ..start
                    .checked_add(length)
                    .ok_or(CoffError::Truncated(field))?,
        )
        .ok_or(CoffError::Truncated(field))
}

// Call only on the validated section table.
fn record(sections: &[u8], index: usize) -> &[u8] {
    &sections[index * SECTION_HEADER..(index + 1) * SECTION_HEADER]
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
#[path = "coff/tests.rs"]
mod tests;
