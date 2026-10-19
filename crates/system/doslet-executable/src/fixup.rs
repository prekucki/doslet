//! Borrowed decoding of the shared LE/LX fixup record layout, for LE inspection.
//!
//! Only direct internal targets are supported. Imports, entry-table targets,
//! internal chains, additive flags and the ordinal flag are explicit errors.
//! This module neither applies relocations nor checks target object existence,
//! object-relative ranges, alias suitability or cross-page duplicate consistency.
//! In particular, a one-past-object target offset is not rejected here.
//!
//! Layout references (not implementation sources):
//! <https://github.com/open-watcom/open-watcom-v2/blob/master/bld/watcom/h/exeflat.h>
//! <https://github.com/open-watcom/open-watcom-v2/blob/master/bld/wl/c/obj2supp.c>
//! <https://komh.github.io/os2books/os2tk45/lxref.htm>
//! The IBM reference describes LX; only the shared record layout is used here,
//! not its page-map or fixup-page-table indexing rules.

use core::{fmt, iter::FusedIterator};

use crate::LePage;

/// Decode policy, not a format maximum. One step is charged for each attempted
/// record and each source offset checked before that record can be published.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeFixupLimits {
    pub max_decode_steps: usize,
}

impl Default for LeFixupLimits {
    fn default() -> Self {
        Self {
            max_decode_steps: 2_000_000,
        }
    }
}

/// The numeric discriminants are the on-disk source type codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum LeFixupSourceType {
    Byte = 0,
    Selector16 = 2,
    /// A 16-bit selector and a 16-bit offset.
    Pointer1616 = 3,
    Offset16 = 5,
    /// A 16-bit selector and a 32-bit offset.
    Pointer1632 = 6,
    Offset32 = 7,
    SelfRelative32 = 8,
}

impl LeFixupSourceType {
    pub fn width(self) -> u8 {
        match self {
            Self::Byte => 1,
            Self::Selector16 | Self::Offset16 => 2,
            Self::Pointer1616 | Self::Offset32 | Self::SelfRelative32 => 4,
            Self::Pointer1632 => 6,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeFixupField {
    SourceType,
    TargetFlags,
    SourceOffset,
    SourceCount,
    ObjectNumber,
    TargetOffset,
    SourceList,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeFixupUnsupported {
    ImportByOrdinal,
    ImportByName,
    InternalViaEntry,
    InternalChain,
    /// Includes the 32-bit additive modifier, even without the additive flag.
    Additive,
    Ordinal8,
    PageSize(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeFixupErrorKind {
    Truncated(LeFixupField),
    ReservedSourceFlags(u8),
    UnknownSourceType(u8),
    InvalidAlias(LeFixupSourceType),
    ZeroObjectNumber,
    /// The source's write interval has no intersection with this page.
    SourceOutsidePage {
        source_offset: i16,
        width: u8,
        page_size: u32,
    },
    Unsupported(LeFixupUnsupported),
    DecodeBudget,
}

/// Offsets are relative to the beginning of the supplied page record stream.
/// `byte_offset` identifies the field (or individual source-list entry) that
/// failed. Constructor configuration errors use zero for both offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeFixupError {
    pub record_offset: usize,
    pub byte_offset: usize,
    pub kind: LeFixupErrorKind,
}

impl fmt::Display for LeFixupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "LE fixup record at {:#x}, field at {:#x}: ",
            self.record_offset, self.byte_offset
        )?;
        match self.kind {
            LeFixupErrorKind::Truncated(field) => write!(f, "truncated {field:?}"),
            LeFixupErrorKind::ReservedSourceFlags(flags) => {
                write!(f, "reserved source flags {flags:#x}")
            }
            LeFixupErrorKind::UnknownSourceType(code) => {
                write!(f, "unknown source type {code:#x}")
            }
            LeFixupErrorKind::InvalidAlias(source) => {
                write!(f, "alias flag is invalid for {source:?}")
            }
            LeFixupErrorKind::ZeroObjectNumber => f.write_str("target object number is zero"),
            LeFixupErrorKind::SourceOutsidePage {
                source_offset,
                width,
                page_size,
            } => write!(
                f,
                "source {source_offset} with width {width} does not touch page of size {page_size}"
            ),
            LeFixupErrorKind::Unsupported(feature) => write!(f, "unsupported {feature:?}"),
            LeFixupErrorKind::DecodeBudget => f.write_str("decode step budget exhausted"),
        }
    }
}

impl core::error::Error for LeFixupError {}

/// A nonzero one-based object reference and an optional object-relative offset.
/// Selector sources omit the offset field entirely. Offset values are preserved
/// without imposing object-range or relocation-application rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeFixupTarget {
    object_number: u16,
    offset: Option<u32>,
}

impl LeFixupTarget {
    pub fn object_number(self) -> u16 {
        self.object_number
    }

    pub fn offset(self) -> Option<u32> {
        self.offset
    }
}

/// Validated, borrowed little-endian signed source offsets. A single-source
/// record borrows its source field; a list record borrows its trailing list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeFixupSources<'a> {
    bytes: &'a [u8],
}

impl<'a> LeFixupSources<'a> {
    pub fn len(self) -> usize {
        self.bytes.len() / 2
    }

    pub fn is_empty(self) -> bool {
        self.bytes.is_empty()
    }

    /// Negative offsets describe a write starting on the preceding page.
    pub fn iter(
        self,
    ) -> impl ExactSizeIterator<Item = i16> + DoubleEndedIterator + FusedIterator + 'a {
        self.bytes
            .as_chunks::<2>()
            .0
            .iter()
            .copied()
            .map(i16::from_le_bytes)
    }
}

/// One completely decoded record. Every source touches the current page for
/// this source type's write width; enclosing object bounds remain unchecked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LeFixup<'a> {
    record_offset: usize,
    bytes: &'a [u8],
    source_type: LeFixupSourceType,
    target: LeFixupTarget,
    sources: LeFixupSources<'a>,
}

impl<'a> LeFixup<'a> {
    pub fn record_offset(self) -> usize {
        self.record_offset
    }

    pub fn bytes(self) -> &'a [u8] {
        self.bytes
    }

    pub fn source_type(self) -> LeFixupSourceType {
        self.source_type
    }

    /// Metadata only: the decoder does not create or validate an alias selector.
    pub fn is_alias(self) -> bool {
        self.bytes[0] & 0x10 != 0
    }

    pub fn is_source_list(self) -> bool {
        self.bytes[0] & 0x20 != 0
    }

    /// Preserves the target-offset and object-number encoding widths.
    pub fn target_flags(self) -> u8 {
        self.bytes[1]
    }

    pub fn target(self) -> LeFixupTarget {
        self.target
    }

    pub fn sources(self) -> LeFixupSources<'a> {
        self.sources
    }
}

/// Allocation-free fallible iterator over one page's fixup record stream.
///
/// Construction checks only configuration. Iteration must reach `None` without
/// an error to validate the complete stream. Each record, including every list
/// entry, is checked before publication. The first error is yielded once; the
/// iterator is then permanently exhausted. No malformed record is skipped.
///
/// Page sizes are restricted to 1..=32768: larger pages cannot distinguish all
/// positive in-page source positions from negative cross-page positions in the
/// signed 16-bit source encoding. This is a supported-subset restriction, not
/// an assumption that the LE header itself forbids larger pages.
#[derive(Debug)]
pub struct LeFixups<'a> {
    bytes: &'a [u8],
    position: usize,
    page_size: u32,
    remaining_steps: usize,
    finished: bool,
}

impl<'a> LeFixups<'a> {
    pub fn new(
        bytes: &'a [u8],
        page_size: u32,
        limits: LeFixupLimits,
    ) -> Result<Self, LeFixupError> {
        if !(1..=32768).contains(&page_size) {
            return Err(LeFixupError {
                record_offset: 0,
                byte_offset: 0,
                kind: LeFixupErrorKind::Unsupported(LeFixupUnsupported::PageSize(page_size)),
            });
        }
        Ok(Self {
            bytes,
            position: 0,
            page_size,
            remaining_steps: limits.max_decode_steps,
            finished: false,
        })
    }

    /// May be passed to the next page's limits to share one file-wide budget.
    pub fn remaining_decode_steps(&self) -> usize {
        self.remaining_steps
    }

    fn decode(&mut self) -> Result<LeFixup<'a>, LeFixupError> {
        let start = self.position;
        let mut cursor = Cursor {
            bytes: self.bytes,
            record_offset: start,
            position: start,
            remaining_steps: &mut self.remaining_steps,
        };
        cursor.step(start)?;
        let source_flags = cursor.byte(LeFixupField::SourceType)?;
        if source_flags & 0xc0 != 0 {
            return Err(cursor.error(start, LeFixupErrorKind::ReservedSourceFlags(source_flags)));
        }
        let source_type = match source_flags & 0x0f {
            0 => LeFixupSourceType::Byte,
            2 => LeFixupSourceType::Selector16,
            3 => LeFixupSourceType::Pointer1616,
            5 => LeFixupSourceType::Offset16,
            6 => LeFixupSourceType::Pointer1632,
            7 => LeFixupSourceType::Offset32,
            8 => LeFixupSourceType::SelfRelative32,
            code => return Err(cursor.error(start, LeFixupErrorKind::UnknownSourceType(code))),
        };
        if source_flags & 0x10 != 0
            && !matches!(
                source_type,
                LeFixupSourceType::Selector16
                    | LeFixupSourceType::Pointer1616
                    | LeFixupSourceType::Pointer1632
            )
        {
            return Err(cursor.error(start, LeFixupErrorKind::InvalidAlias(source_type)));
        }
        let target_flags = cursor.byte(LeFixupField::TargetFlags)?;
        let unsupported = match target_flags & 3 {
            1 => Some(LeFixupUnsupported::ImportByOrdinal),
            2 => Some(LeFixupUnsupported::ImportByName),
            3 => Some(LeFixupUnsupported::InternalViaEntry),
            _ if target_flags & 8 != 0 => Some(LeFixupUnsupported::InternalChain),
            _ if target_flags & 0x24 != 0 => Some(LeFixupUnsupported::Additive),
            _ if target_flags & 0x80 != 0 => Some(LeFixupUnsupported::Ordinal8),
            _ => None,
        };
        if let Some(feature) = unsupported {
            return Err(cursor.error(start + 1, LeFixupErrorKind::Unsupported(feature)));
        }
        let source_field = cursor.position;
        let (single_source, source_count) = if source_flags & 0x20 != 0 {
            (None, usize::from(cursor.byte(LeFixupField::SourceCount)?))
        } else {
            (Some(cursor.take(2, LeFixupField::SourceOffset)?), 1)
        };
        let object_field = cursor.position;
        let object_number = if target_flags & 0x40 != 0 {
            cursor.word(LeFixupField::ObjectNumber)?
        } else {
            u16::from(cursor.byte(LeFixupField::ObjectNumber)?)
        };
        if object_number == 0 {
            return Err(cursor.error(object_field, LeFixupErrorKind::ZeroObjectNumber));
        }
        let offset = if source_type == LeFixupSourceType::Selector16 {
            None
        } else if target_flags & 0x10 != 0 {
            let bytes = cursor.take(4, LeFixupField::TargetOffset)?;
            Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        } else {
            Some(u32::from(cursor.word(LeFixupField::TargetOffset)?))
        };
        let (source_bytes, sources_field) = if let Some(bytes) = single_source {
            (bytes, source_field)
        } else {
            let list_field = cursor.position;
            (
                cursor.take(source_count * 2, LeFixupField::SourceList)?,
                list_field,
            )
        };
        let sources = LeFixupSources {
            bytes: source_bytes,
        };
        for (index, source_offset) in sources.iter().enumerate() {
            let field = sources_field + index * 2;
            cursor.step(field)?;
            let source_start = i32::from(source_offset);
            if source_start >= self.page_size as i32
                || source_start + i32::from(source_type.width()) <= 0
            {
                return Err(cursor.error(
                    field,
                    LeFixupErrorKind::SourceOutsidePage {
                        source_offset,
                        width: source_type.width(),
                        page_size: self.page_size,
                    },
                ));
            }
        }
        self.position = cursor.position;
        Ok(LeFixup {
            record_offset: start,
            bytes: &self.bytes[start..self.position],
            source_type,
            target: LeFixupTarget {
                object_number,
                offset,
            },
            sources,
        })
    }
}

impl<'a> Iterator for LeFixups<'a> {
    type Item = Result<LeFixup<'a>, LeFixupError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished || self.position == self.bytes.len() {
            self.finished = true;
            return None;
        }
        let result = self.decode();
        if result.is_err() {
            self.finished = true;
        }
        Some(result)
    }
}

impl FusedIterator for LeFixups<'_> {}

impl<'a> LePage<'a> {
    /// Decode this page's raw records using the module header's page size.
    /// A manually constructed page is checked in exactly the same way.
    pub fn fixups(
        self,
        page_size: u32,
        limits: LeFixupLimits,
    ) -> Result<LeFixups<'a>, LeFixupError> {
        LeFixups::new(self.fixup_records, page_size, limits)
    }
}

struct Cursor<'a, 'b> {
    bytes: &'a [u8],
    record_offset: usize,
    position: usize,
    remaining_steps: &'b mut usize,
}

impl<'a> Cursor<'a, '_> {
    fn error(&self, byte_offset: usize, kind: LeFixupErrorKind) -> LeFixupError {
        LeFixupError {
            record_offset: self.record_offset,
            byte_offset,
            kind,
        }
    }

    fn step(&mut self, byte_offset: usize) -> Result<(), LeFixupError> {
        *self.remaining_steps = self
            .remaining_steps
            .checked_sub(1)
            .ok_or_else(|| self.error(byte_offset, LeFixupErrorKind::DecodeBudget))?;
        Ok(())
    }

    fn take(&mut self, length: usize, field: LeFixupField) -> Result<&'a [u8], LeFixupError> {
        // position stays within bytes; successful additions cannot overflow.
        let bytes = self.bytes[self.position..]
            .get(..length)
            .ok_or_else(|| self.error(self.position, LeFixupErrorKind::Truncated(field)))?;
        self.position += length;
        Ok(bytes)
    }

    fn byte(&mut self, field: LeFixupField) -> Result<u8, LeFixupError> {
        Ok(self.take(1, field)?[0])
    }

    fn word(&mut self, field: LeFixupField) -> Result<u16, LeFixupError> {
        let bytes = self.take(2, field)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }
}

#[cfg(test)]
mod tests;
