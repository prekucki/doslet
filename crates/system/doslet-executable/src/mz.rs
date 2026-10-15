use core::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutableError {
    Truncated,
    InvalidSignature,
    InvalidSize,
    InvalidRelocations,
    UnsupportedByteOrder,
}
impl fmt::Display for ExecutableError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Truncated => "truncated executable header or image",
            Self::InvalidSignature => "expected a DOS MZ executable",
            Self::InvalidSize => "invalid MZ image or header size",
            Self::InvalidRelocations => "MZ relocation table exceeds its header",
            Self::UnsupportedByteOrder => "unsupported LE/LX byte order",
        })
    }
}
impl core::error::Error for ExecutableError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MzHeader {
    pub header_bytes: usize,
    pub image_bytes: usize,
    pub relocation_count: u16,
    pub initial_cs: u16,
    pub initial_ip: u16,
    pub initial_ss: u16,
    pub initial_sp: u16,
    pub minimum_extra_paragraphs: u16,
    pub maximum_extra_paragraphs: u16,
    pub overlay_number: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Relocation {
    pub offset: u16,
    pub segment: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtendedHeader {
    pub signature: [u8; 2],
    pub file_offset: usize,
    pub cpu_type: u16,
    pub os_type: u16,
    pub entry_object: u32,
    pub entry_offset: u32,
}

pub struct MzImage<'a> {
    pub header: MzHeader,
    pub extended: Option<ExtendedHeader>,
    /// Declared MZ load module; appended LE/LX data is not included.
    pub load_module: &'a [u8],
    pub bytes: &'a [u8],
    relocations: &'a [u8],
}

impl<'a> MzImage<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, ExecutableError> {
        if bytes.len() < 28 {
            return Err(ExecutableError::Truncated);
        }
        if &bytes[..2] != b"MZ" {
            return Err(ExecutableError::InvalidSignature);
        }
        let last_page = word(bytes, 2)? as usize;
        let pages = word(bytes, 4)? as usize;
        if pages == 0 || last_page >= 512 {
            return Err(ExecutableError::InvalidSize);
        }
        let image_bytes = if last_page == 0 {
            pages * 512
        } else {
            (pages - 1) * 512 + last_page
        };
        let header_bytes = usize::from(word(bytes, 8)?) * 16;
        if header_bytes < 28 || header_bytes > image_bytes {
            return Err(ExecutableError::InvalidSize);
        }
        if image_bytes > bytes.len() {
            return Err(ExecutableError::Truncated);
        }
        let relocation_count = word(bytes, 6)?;
        let relocations = if relocation_count != 0 {
            let relocation_start = usize::from(word(bytes, 24)?);
            if relocation_start < 28
                || relocation_start + usize::from(relocation_count) * 4 > header_bytes
            {
                return Err(ExecutableError::InvalidRelocations);
            }
            &bytes[relocation_start..relocation_start + usize::from(relocation_count) * 4]
        } else {
            &bytes[..0]
        };
        let header = MzHeader {
            header_bytes,
            image_bytes,
            relocation_count,
            initial_ss: word(bytes, 14)?,
            initial_sp: word(bytes, 16)?,
            initial_ip: word(bytes, 20)?,
            initial_cs: word(bytes, 22)?,
            minimum_extra_paragraphs: word(bytes, 10)?,
            maximum_extra_paragraphs: word(bytes, 12)?,
            overlay_number: word(bytes, 26)?,
        };
        let mut extended = None;
        // Old MZ files can put their relocation table across offset 3Ch.
        // In that layout those four bytes are relocations, not e_lfanew.
        let has_extended_pointer = relocation_count == 0 || word(bytes, 24)? >= 64;
        if header_bytes >= 64 && has_extended_pointer {
            let offset = dword(bytes, 60)? as usize;
            if offset != 0 {
                let signature = bytes
                    .get(offset..offset.saturating_add(2))
                    .ok_or(ExecutableError::Truncated)?;
                if signature == b"LE" || signature == b"LX" {
                    let ext = bytes.get(offset..).ok_or(ExecutableError::Truncated)?;
                    if *ext.get(2).ok_or(ExecutableError::Truncated)? != 0
                        || *ext.get(3).ok_or(ExecutableError::Truncated)? != 0
                    {
                        return Err(ExecutableError::UnsupportedByteOrder);
                    }
                    extended = Some(ExtendedHeader {
                        signature: [signature[0], signature[1]],
                        file_offset: offset,
                        cpu_type: word(ext, 8)?,
                        os_type: word(ext, 10)?,
                        entry_object: dword(ext, 24)?,
                        entry_offset: dword(ext, 28)?,
                    });
                }
            }
        }
        Ok(Self {
            header,
            extended,
            load_module: &bytes[header_bytes..image_bytes],
            bytes,
            relocations,
        })
    }

    /// Borrow the validated relocation table without allocating an entry list.
    pub fn relocations(&self) -> impl ExactSizeIterator<Item = Relocation> + '_ {
        self.relocations.as_chunks::<4>().0.iter().map(
            |&[offset_lo, offset_hi, segment_lo, segment_hi]| Relocation {
                offset: u16::from_le_bytes([offset_lo, offset_hi]),
                segment: u16::from_le_bytes([segment_lo, segment_hi]),
            },
        )
    }
}

fn word(bytes: &[u8], offset: usize) -> Result<u16, ExecutableError> {
    let bytes = bytes
        .get(offset..offset + 2)
        .ok_or(ExecutableError::Truncated)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}
fn dword(bytes: &[u8], offset: usize) -> Result<u32, ExecutableError> {
    let bytes = bytes
        .get(offset..offset + 4)
        .ok_or(ExecutableError::Truncated)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> [u8; 128] {
        let mut bytes = [0; 128];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[2..4].copy_from_slice(&128u16.to_le_bytes());
        bytes[4..6].copy_from_slice(&1u16.to_le_bytes());
        bytes[8..10].copy_from_slice(&4u16.to_le_bytes());
        bytes
    }
    #[test]
    fn mz_views_borrow_original_file_storage() {
        let bytes = fixture();
        let image = MzImage::parse(&bytes).unwrap();
        assert_eq!(image.load_module.as_ptr(), bytes[64..].as_ptr());
        assert!(image.extended.is_none());
    }
    #[test]
    fn legacy_relocations_at_3c_are_not_an_extended_header_pointer() {
        let mut bytes = fixture();
        bytes[6..8].copy_from_slice(&9u16.to_le_bytes());
        bytes[24..26].copy_from_slice(&28u16.to_le_bytes());
        bytes[60..64].fill(0xFF);
        let image = MzImage::parse(&bytes).unwrap();
        assert!(image.extended.is_none());
        assert_eq!(image.relocations().len(), 9);
        let last = image.relocations().last().unwrap();
        assert_eq!((last.segment, last.offset), (0xFFFF, 0xFFFF));
    }
    #[test]
    fn le_metadata_and_malformed_boundaries() {
        let mut bytes = fixture();
        bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..66].copy_from_slice(b"LE");
        bytes[72..74].copy_from_slice(&2u16.to_le_bytes());
        bytes[88..92].copy_from_slice(&1u32.to_le_bytes());
        assert_eq!(
            MzImage::parse(&bytes).unwrap().extended.unwrap().cpu_type,
            2
        );
        assert!(MzImage::parse(&bytes[..100]).is_err());
        bytes[6..8].copy_from_slice(&20u16.to_le_bytes());
        bytes[24..26].copy_from_slice(&28u16.to_le_bytes());
        assert!(matches!(
            MzImage::parse(&bytes),
            Err(ExecutableError::InvalidRelocations)
        ));
    }
}
