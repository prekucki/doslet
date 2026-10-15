//! Owned immutable ISO 9660 media with borrowed sectors and file extents.
#![no_std]
extern crate alloc;
use alloc::vec::Vec;
use core::ops::Range;
pub const SECTOR_SIZE: usize = 2048;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidImage,
    InvalidRecord,
    NotFound,
    Unsupported,
}
#[derive(Clone, Debug)]
pub struct Entry {
    pub data: Range<usize>,
    pub record: Range<usize>,
    pub directory: bool,
}
pub struct Image {
    bytes: Vec<u8>,
    volume_len: usize,
    primary: usize,
    root: Entry,
}
impl Image {
    pub fn new(bytes: Vec<u8>) -> Result<Self, Error> {
        if !bytes.len().is_multiple_of(SECTOR_SIZE) {
            return Err(Error::InvalidImage);
        }
        let mut primary = 16 * SECTOR_SIZE;
        loop {
            if primary >= 80 * SECTOR_SIZE {
                return Err(Error::Unsupported);
            }
            let descriptor = bytes
                .get(primary..primary + SECTOR_SIZE)
                .ok_or(Error::InvalidImage)?;
            if &descriptor[1..6] != b"CD001" || descriptor[6] != 1 {
                return Err(Error::InvalidImage);
            }
            match descriptor[0] {
                1 => break,
                255 => return Err(Error::InvalidImage),
                _ => primary += SECTOR_SIZE,
            }
        }
        let pvd = &bytes[primary..primary + SECTOR_SIZE];
        let sectors = both32(&pvd[80..88])? as usize;
        if pvd[128..132] != [0, 8, 8, 0] {
            return Err(Error::Unsupported);
        }
        let volume_len = sectors
            .checked_mul(SECTOR_SIZE)
            .filter(|len| *len <= bytes.len())
            .ok_or(Error::InvalidImage)?;
        if volume_len < primary + SECTOR_SIZE {
            return Err(Error::InvalidImage);
        }
        let root = record(&bytes, primary + 156, volume_len)?;
        if !root.directory {
            return Err(Error::InvalidImage);
        }
        Ok(Self {
            bytes,
            volume_len,
            primary,
            root,
        })
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes[..self.volume_len]
    }
    pub fn primary_descriptor(&self) -> usize {
        self.primary
    }
    pub fn data(&self, entry: &Entry) -> &[u8] {
        &self.bytes[entry.data.clone()]
    }
    pub fn lookup(&self, path: &[u8]) -> Result<Entry, Error> {
        if path.len() > 127 || !path.is_ascii() || path.contains(&0) {
            return Err(Error::NotFound);
        }
        let mut current = self.root.clone();
        let mut entries = 0;
        for component in path
            .split(|byte| *byte == b'\\' || *byte == b'/')
            .filter(|s| !s.is_empty() && *s != b".")
        {
            if component == b".." || !current.directory {
                return Err(Error::NotFound);
            }
            let mut offset = current.data.start;
            let mut found = None;
            while offset < current.data.end {
                entries += 1;
                if entries > 4096 {
                    return Err(Error::Unsupported);
                }
                let length = usize::from(self.bytes[offset]);
                if length == 0 {
                    offset = (offset / SECTOR_SIZE + 1) * SECTOR_SIZE;
                    continue;
                }
                if offset + length > current.data.end || offset % SECTOR_SIZE + length > SECTOR_SIZE
                {
                    return Err(Error::InvalidRecord);
                }
                let entry = record(&self.bytes, offset, self.volume_len)?;
                let raw = &self.bytes[entry.record.clone()];
                let name = &raw[33..33 + usize::from(raw[32])];
                if name != [0] && name != [1] && raw[25] & 4 == 0 {
                    let name = name.split(|b| *b == b';').next().unwrap();
                    let name = name.strip_suffix(b".").unwrap_or(name);
                    if name.eq_ignore_ascii_case(component) {
                        found = Some(entry);
                        break;
                    }
                }
                offset += length;
            }
            current = found.ok_or(Error::NotFound)?;
        }
        Ok(current)
    }
}
fn both32(bytes: &[u8]) -> Result<u32, Error> {
    let little = u32::from_le_bytes(bytes[..4].try_into().unwrap());
    if little != u32::from_be_bytes(bytes[4..8].try_into().unwrap()) {
        return Err(Error::InvalidRecord);
    }
    Ok(little)
}
fn record(bytes: &[u8], offset: usize, limit: usize) -> Result<Entry, Error> {
    let length = usize::from(*bytes.get(offset).ok_or(Error::InvalidRecord)?);
    let raw = bytes
        .get(offset..offset + length)
        .filter(|raw| raw.len() >= 34)
        .ok_or(Error::InvalidRecord)?;
    if usize::from(raw[32]) + 33 > raw.len() {
        return Err(Error::InvalidRecord);
    }
    if raw[25] & 0x80 != 0 || raw[26] != 0 || raw[27] != 0 {
        return Err(Error::Unsupported);
    }
    let sector = usize::try_from(both32(&raw[2..10])?).map_err(|_| Error::InvalidRecord)?;
    let start = sector
        .checked_add(usize::from(raw[1]))
        .and_then(|s| s.checked_mul(SECTOR_SIZE))
        .ok_or(Error::InvalidRecord)?;
    let end = start
        .checked_add(both32(&raw[10..18])? as usize)
        .filter(|end| *end <= limit)
        .ok_or(Error::InvalidRecord)?;
    Ok(Entry {
        data: start..end,
        record: offset..offset + length,
        directory: raw[25] & 2 != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    include!("../../../../tests/fixtures/iso9660.rs");
    #[test]
    fn nested_versioned_files_return_borrowed_media_ranges() {
        let bytes = test_iso();
        let address = bytes.as_ptr();
        let image = Image::new(bytes).unwrap();
        assert_eq!(image.bytes().as_ptr(), address);
        let entry = image.lookup(b"\\heroes/sound/HEROES01.62S").unwrap();
        assert!(!entry.directory);
        assert_eq!(image.data(&entry), b"data");
        assert!(image.lookup(b"HEROES/SOUND").unwrap().directory);
        assert!(matches!(
            image.lookup(b"HEROES/MISSING"),
            Err(Error::NotFound)
        ));
        assert!(matches!(
            image.lookup(b"HEROES/SOUND/HEROES01.62S/CHILD"),
            Err(Error::NotFound)
        ));
    }
    #[test]
    fn malformed_sizes_endian_fields_and_directory_records_are_rejected() {
        for length in [0, 2048, 16 * 2048 + 7] {
            assert!(Image::new(vec![0; length]).is_err());
        }
        let mut bytes = test_iso();
        bytes[16 * 2048 + 84] ^= 1;
        assert!(Image::new(bytes).is_err());
        let mut bytes = test_iso();
        bytes[20 * 2048] = 3;
        let image = Image::new(bytes).unwrap();
        assert!(matches!(image.lookup(b"HEROES"), Err(Error::InvalidRecord)));
        let mut bytes = test_iso();
        bytes[16 * 2048 + 156 + 10..16 * 2048 + 156 + 18].copy_from_slice(&[0xFF; 8]);
        assert!(Image::new(bytes).is_err());
    }
}
