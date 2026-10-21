//! DJGPP COFF executables in the symbolic image model.
//!
//! Each section becomes one object, numbered in section order, whose
//! preferred base is the section's virtual address: an offset in the
//! program's code and data selectors, which share one base (the DPMI block).
//! COFF executables carry no relocations, so the image has none.
//!
//! Analysis binds those objects in that offset space, as if the selectors
//! had base zero: the CFG and generated procedures then name the actual
//! EIP values, and only source proofs add the live selector base. This holds
//! because DJGPP's CS and DS describe the same block; data accesses still
//! go through the live segment windows at run time.

use alloc::vec::Vec;
use doslet_executable::{
    CoffImage, LeHeader, LeLocation, LeObject, LeObjectOffset,
    coff::{STYP_BSS, STYP_DATA, STYP_TEXT},
};

use crate::{
    DefaultBitness, FileBackedSpan, ImageError, ImageFormat, ImageLimits, ImageStorage,
    StorageBudget, SymbolicImage, SymbolicObject,
    binding::{DosFlatObjectBinding, DosFlatSelector},
    host_size,
};

const READABLE: u32 = 0x0001;
const WRITABLE: u32 = 0x0002;
const EXECUTABLE: u32 = 0x0004;
/// LE's "big/default 32-bit" object flag.
const BIG: u32 = 0x2000;

impl SymbolicImage {
    /// Reconstruct the text, data and BSS sections of a DJGPP executable.
    /// Relocations and other section kinds are rejected explicitly.
    pub fn from_coff(image: &CoffImage<'_>, limits: ImageLimits) -> Result<Self, ImageError> {
        Self::from_coff_with_text(image, None, limits)
    }

    /// As [`Self::from_coff`], with `text` in place of the file's text bytes:
    /// the code as loaded now, for programs that patch their own code.
    /// It must have the text section's exact size.
    pub fn from_coff_with_text(
        image: &CoffImage<'_>,
        text: Option<&[u8]>,
        limits: ImageLimits,
    ) -> Result<Self, ImageError> {
        let header = image.header();
        let mut budget = StorageBudget(limits.max_owned_bytes);
        let mut objects = Vec::new();
        budget.reserve(
            &mut objects,
            usize::from(header.section_count),
            ImageStorage::Objects,
        )?;
        let mut entry = None;
        for (index, section) in image.sections().enumerate() {
            let object_number =
                u32::try_from(index + 1).map_err(|_| ImageError::Arithmetic("object number"))?;
            if section.relocation_count != 0 {
                return Err(ImageError::UnsupportedCoff("section relocations"));
            }
            let kind = section.flags & (STYP_TEXT | STYP_DATA | STYP_BSS);
            let flags = match kind {
                STYP_TEXT => READABLE | EXECUTABLE | BIG,
                STYP_DATA | STYP_BSS => READABLE | WRITABLE | BIG,
                _ => return Err(ImageError::UnsupportedCoff("section kind")),
            };
            let source = match (kind, text) {
                (STYP_TEXT, Some(text)) if text.len() != section.data.len() => {
                    return Err(ImageError::UnsupportedCoff("live text size"));
                }
                (STYP_TEXT, Some(text)) => text,
                _ => section.data,
            };
            let mut object = SymbolicObject {
                object_number,
                metadata: LeObject {
                    virtual_size: section.size,
                    preferred_base: section.virtual_address,
                    flags,
                    first_page_number: 0,
                    page_count: 0,
                },
                bytes: Vec::new(),
                file_backed_spans: Vec::new(),
            };
            let length = host_size(section.size)?;
            budget.reserve(&mut object.bytes, length, ImageStorage::ObjectBytes)?;
            if kind == STYP_BSS {
                object.bytes.resize(length, 0);
            } else {
                object.bytes.extend_from_slice(source);
                if let Some(file_offset) = section.file_offset {
                    budget.reserve(
                        &mut object.file_backed_spans,
                        1,
                        ImageStorage::FileBackedSpans,
                    )?;
                    object.file_backed_spans.push(FileBackedSpan {
                        object_offset: 0,
                        file_offset,
                        byte_len: section.size,
                    });
                }
            }
            if kind == STYP_TEXT {
                entry = header
                    .entry
                    .checked_sub(section.virtual_address)
                    .filter(|&offset| offset < section.size)
                    .map(|offset| LeObjectOffset {
                        object_number,
                        offset,
                    });
            }
            objects.push(object);
        }
        Ok(Self {
            format: ImageFormat::Coff,
            header: LeHeader {
                location: LeLocation {
                    container_offset: header.offset,
                    header_offset: header.offset,
                },
                cpu_type: 2,
                os_type: 0,
                module_version: 0,
                module_flags: 0,
                page_count: 0,
                page_size: 4096,
                last_page_bytes: 0,
                object_count: u32::from(header.section_count),
                entry,
                stack: None,
                auto_data_object: None,
                data_pages_offset: header.offset,
            },
            objects,
            sites: Vec::new(),
            provenance: Vec::new(),
        })
    }
}

/// Bindings of a COFF image in its selectors' offset space: every object at
/// its virtual address through one flat 32-bit view named `selector`. Add the
/// live selector base to obtain linear addresses. Empty for an LE image.
pub fn coff_offset_bindings(image: &SymbolicImage, selector: u16) -> Vec<DosFlatObjectBinding> {
    if image.format() != ImageFormat::Coff {
        return Vec::new();
    }
    image
        .objects()
        .iter()
        .map(|object| DosFlatObjectBinding {
            object_number: object.object_number(),
            linear_base: object.metadata().preferred_base,
            normal: Some(DosFlatSelector {
                value: selector,
                base: 0,
                limit: u32::MAX,
                default_bitness: DefaultBitness::Bits32,
            }),
            alias: None,
        })
        .collect()
}

#[cfg(test)]
#[path = "coff_tests.rs"]
mod tests;
