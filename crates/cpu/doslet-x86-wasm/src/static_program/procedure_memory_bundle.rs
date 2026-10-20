//! Bounded, opaque metadata in a WebAssembly 1 custom section.
//!
//! This container only checks the module header and top-level section framing.
//! Executable section validation belongs to the WebAssembly engine; metadata
//! interpretation and authentication belong to the caller. Reading borrows the
//! input, and appending allocates one new module without rewriting its sections.

use alloc::vec::Vec;

pub const SECTION_NAME: &str = "doslet.procedures";
/// Maximum bytes in the complete module after removing the metadata section.
pub const MAX_CODE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
/// Includes the section ID, two possibly padded u32 LEBs and the UTF-8 name.
pub const MAX_BUNDLE_BYTES: usize =
    MAX_CODE_BYTES + MAX_METADATA_BYTES + 1 + 5 + 5 + SECTION_NAME.len();

const HEADER: &[u8; 8] = b"\0asm\x01\0\0\0";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BundleError {
    InvalidHeader,
    ModuleTooLarge,
    MetadataTooLarge,
    TruncatedLeb,
    InvalidLeb,
    TruncatedSection,
    TruncatedCustomName,
    InvalidCustomName,
    DuplicateMetadata,
    Allocation,
}

impl core::fmt::Display for BundleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::InvalidHeader => "expected a WebAssembly 1 module header",
            Self::ModuleTooLarge => "WebAssembly procedure bundle exceeds the module size limit",
            Self::MetadataTooLarge => "embedded procedure metadata exceeds the size limit",
            Self::TruncatedLeb => "truncated WebAssembly u32 LEB length",
            Self::InvalidLeb => "overflowing WebAssembly u32 LEB length",
            Self::TruncatedSection => "truncated WebAssembly section",
            Self::TruncatedCustomName => "truncated WebAssembly custom section name",
            Self::InvalidCustomName => "WebAssembly custom section name is not UTF-8",
            Self::DuplicateMetadata => "duplicate embedded procedure metadata section",
            Self::Allocation => "could not allocate the WebAssembly procedure bundle",
        })
    }
}

impl core::error::Error for BundleError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EmbeddedBundle<'a> {
    /// Opaque bytes following the custom section's name; may be empty.
    pub metadata: &'a [u8],
    /// Original module prefix, including its header, before the whole section.
    pub before: &'a [u8],
    /// Original module suffix after the whole section.
    /// Hash `before` followed by `after` to identify the module without metadata.
    pub after: &'a [u8],
}

/// Find exactly one named section anywhere in a WebAssembly 1 module.
///
/// Returns `None` when the section is absent; an empty section returns `Some`.
/// Other custom sections and executable payloads remain opaque. All custom
/// section names must be valid UTF-8, and all top-level lengths must fit u32.
/// This does not validate executable section ordering, types or instructions.
pub fn read(module: &[u8]) -> Result<Option<EmbeddedBundle<'_>>, BundleError> {
    if module.len() > MAX_BUNDLE_BYTES {
        return Err(BundleError::ModuleTooLarge);
    }
    if module.get(..HEADER.len()) != Some(HEADER.as_slice()) {
        return Err(BundleError::InvalidHeader);
    }
    let mut cursor = HEADER.len();
    let mut found = None;
    while cursor < module.len() {
        let section_start = cursor;
        let id = module[cursor];
        cursor += 1;
        let length = read_u32(module, &mut cursor)? as usize;
        let end = cursor
            .checked_add(length)
            .ok_or(BundleError::TruncatedSection)?;
        let payload = module
            .get(cursor..end)
            .ok_or(BundleError::TruncatedSection)?;
        if id == 0 {
            let mut name_start = 0;
            let name_length = read_u32(payload, &mut name_start)? as usize;
            let name_end = name_start
                .checked_add(name_length)
                .ok_or(BundleError::TruncatedCustomName)?;
            let name = payload
                .get(name_start..name_end)
                .ok_or(BundleError::TruncatedCustomName)?;
            let name = core::str::from_utf8(name).map_err(|_| BundleError::InvalidCustomName)?;
            if name == SECTION_NAME {
                if found.is_some() {
                    return Err(BundleError::DuplicateMetadata);
                }
                let metadata = &payload[name_end..];
                if metadata.len() > MAX_METADATA_BYTES {
                    return Err(BundleError::MetadataTooLarge);
                }
                found = Some(EmbeddedBundle {
                    metadata,
                    before: &module[..section_start],
                    after: &module[end..],
                });
            }
        }
        cursor = end;
    }
    let code_bytes = found.map_or(module.len(), |bundle| {
        bundle.before.len() + bundle.after.len()
    });
    if code_bytes > MAX_CODE_BYTES {
        return Err(BundleError::ModuleTooLarge);
    }
    Ok(found)
}

/// Append one opaque metadata section while preserving every input byte.
///
/// Rejects a preexisting metadata section, malformed framing or exceeded
/// limits before reserving output storage. Empty metadata is allowed. The
/// caller retains ownership of the input module; the result is one new buffer.
pub fn append(module: &[u8], metadata: &[u8]) -> Result<Vec<u8>, BundleError> {
    if metadata.len() > MAX_METADATA_BYTES {
        return Err(BundleError::MetadataTooLarge);
    }
    if read(module)?.is_some() {
        return Err(BundleError::DuplicateMetadata);
    }
    // The fixed limits above guarantee both conversions and arithmetic fit u32.
    let name_length = SECTION_NAME.len() as u32;
    let payload_length = u32_length(name_length) + SECTION_NAME.len() + metadata.len();
    let output_length = module.len() + 1 + u32_length(payload_length as u32) + payload_length;
    let mut output = Vec::new();
    output
        .try_reserve_exact(output_length)
        .map_err(|_| BundleError::Allocation)?;
    output.extend_from_slice(module);
    output.push(0);
    write_u32(payload_length as u32, &mut output);
    write_u32(name_length, &mut output);
    output.extend_from_slice(SECTION_NAME.as_bytes());
    output.extend_from_slice(metadata);
    Ok(output)
}

fn read_u32(bytes: &[u8], cursor: &mut usize) -> Result<u32, BundleError> {
    let mut value = 0;
    for shift in [0, 7, 14, 21, 28] {
        let byte = *bytes.get(*cursor).ok_or(BundleError::TruncatedLeb)?;
        *cursor += 1;
        if shift == 28 && byte & 0xf0 != 0 {
            return Err(BundleError::InvalidLeb);
        }
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
    }
    Err(BundleError::InvalidLeb)
}

fn u32_length(mut value: u32) -> usize {
    let mut length = 1;
    while value >= 0x80 {
        value >>= 7;
        length += 1;
    }
    length
}

fn write_u32(mut value: u32, output: &mut Vec<u8>) {
    loop {
        let byte = value as u8 & 0x7f;
        value >>= 7;
        output.push(byte | if value == 0 { 0 } else { 0x80 });
        if value == 0 {
            return;
        }
    }
}

#[cfg(test)]
#[path = "procedure_memory_bundle_tests.rs"]
mod tests;
