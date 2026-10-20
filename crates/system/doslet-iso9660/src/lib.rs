//! Owned immutable ISO 9660 media with borrowed sectors and file extents.
#![no_std]
extern crate alloc;
use alloc::{sync::Arc, vec::Vec};
use core::ops::Range;
pub const SECTOR_SIZE: usize = 2048;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidImage,
    InvalidRecord,
    NotFound,
    Unsupported,
    Storage(doslet_storage::StorageError),
}
#[derive(Clone, Debug)]
pub struct Entry {
    pub data: Range<usize>,
    pub record: Range<usize>,
    pub directory: bool,
}
#[derive(Clone)]
pub struct Image {
    bytes: Arc<Vec<u8>>,
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
            bytes: Arc::new(bytes),
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

/// ISO metadata over positional storage; no source-sized allocation is needed.
pub struct Provider<S> {
    source: S,
    primary: u64,
    volume_len: u64,
    root: Option<ProviderEntry>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderEntry {
    pub data: Range<u64>,
    pub record: Range<u64>,
    pub directory: bool,
}
impl<S: doslet_storage::ReadAt> Provider<S> {
    pub fn new(source: S) -> Self {
        Self {
            source,
            primary: 0,
            volume_len: 0,
            root: None,
        }
    }
    pub fn source(&self) -> &S {
        &self.source
    }
    pub fn source_mut(&mut self) -> &mut S {
        &mut self.source
    }
    pub fn into_source(self) -> S {
        self.source
    }
    pub fn volume_len(&mut self) -> Result<u64, Error> {
        self.prepare()?;
        Ok(self.volume_len)
    }
    pub fn primary_descriptor(&mut self) -> Result<u64, Error> {
        self.prepare()?;
        Ok(self.primary)
    }
    fn exact(&mut self, offset: u64, output: &mut [u8]) -> Result<(), Error> {
        let count = self
            .source
            .read_at(offset, output)
            .map_err(Error::Storage)?;
        if count != output.len() {
            return Err(Error::InvalidImage);
        }
        Ok(())
    }
    pub fn prepare(&mut self) -> Result<(), Error> {
        if self.root.is_some() {
            return Ok(());
        }
        let len = self.source.identity().length;
        if !len.is_multiple_of(SECTOR_SIZE as u64) {
            return Err(Error::InvalidImage);
        }
        let mut descriptor = [0; SECTOR_SIZE];
        for sector in 16..80u64 {
            self.exact(sector * SECTOR_SIZE as u64, &mut descriptor)?;
            if &descriptor[1..6] != b"CD001" || descriptor[6] != 1 {
                return Err(Error::InvalidImage);
            }
            match descriptor[0] {
                1 => {}
                255 => return Err(Error::InvalidImage),
                _ => continue,
            }
            if descriptor[128..132] != [0, 8, 8, 0] {
                return Err(Error::Unsupported);
            }
            let volume_len = u64::from(both32(&descriptor[80..88])?) * SECTOR_SIZE as u64;
            if volume_len > len || volume_len < (sector + 1) * SECTOR_SIZE as u64 {
                return Err(Error::InvalidImage);
            }
            let root = provider_record(
                &descriptor[156..],
                sector * SECTOR_SIZE as u64 + 156,
                volume_len,
            )?;
            if !root.directory {
                return Err(Error::InvalidImage);
            }
            self.primary = sector * SECTOR_SIZE as u64;
            self.volume_len = volume_len;
            self.root = Some(root);
            return Ok(());
        }
        Err(Error::Unsupported)
    }
    fn record_at(&mut self, offset: u64) -> Result<(ProviderEntry, [u8; 255]), Error> {
        let mut raw = [0; 255];
        let mut sector = [0; SECTOR_SIZE];
        self.exact(
            offset / SECTOR_SIZE as u64 * SECTOR_SIZE as u64,
            &mut sector,
        )?;
        let within = (offset % SECTOR_SIZE as u64) as usize;
        let length = sector[within] as usize;
        if length == 0 || within + length > SECTOR_SIZE {
            return Err(Error::InvalidRecord);
        }
        raw[..length].copy_from_slice(&sector[within..within + length]);
        Ok((
            provider_record(&raw[..length], offset, self.volume_len)?,
            raw,
        ))
    }
    pub fn lookup(&mut self, path: &[u8]) -> Result<ProviderEntry, Error> {
        self.prepare()?;
        if path.len() > 127 || !path.is_ascii() || path.contains(&0) {
            return Err(Error::NotFound);
        }
        let mut current = self.root.clone().unwrap();
        let mut visited = 0;
        for component in path
            .split(|b| matches!(b, b'/' | b'\\'))
            .filter(|p| !p.is_empty() && *p != b".")
        {
            if component == b".." || !current.directory {
                return Err(Error::NotFound);
            }
            let mut offset = current.data.start;
            let mut found = None;
            while offset < current.data.end {
                visited += 1;
                if visited > 4096 {
                    return Err(Error::Unsupported);
                }
                let mut length = [0];
                self.exact(offset, &mut length)?;
                if length[0] == 0 {
                    offset = (offset / SECTOR_SIZE as u64 + 1) * SECTOR_SIZE as u64;
                    continue;
                }
                if offset + u64::from(length[0]) > current.data.end {
                    return Err(Error::InvalidRecord);
                }
                let (entry, raw) = self.record_at(offset)?;
                let name = &raw[33..33 + raw[32] as usize];
                if name != [0] && name != [1] && raw[25] & 4 == 0 {
                    let name = name.split(|b| *b == b';').next().unwrap();
                    let name = name.strip_suffix(b".").unwrap_or(name);
                    if name.eq_ignore_ascii_case(component) {
                        found = Some(entry);
                        break;
                    }
                }
                offset += u64::from(length[0]);
            }
            current = found.ok_or(Error::NotFound)?;
        }
        Ok(current)
    }
    pub fn read_file(
        &mut self,
        entry: &ProviderEntry,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, Error> {
        let length = entry
            .data
            .end
            .checked_sub(entry.data.start)
            .ok_or(Error::InvalidRecord)?;
        if offset >= length {
            return Ok(0);
        }
        let count = output
            .len()
            .min((length - offset).min(usize::MAX as u64) as usize);
        self.source
            .read_at(entry.data.start + offset, &mut output[..count])
            .map_err(Error::Storage)
    }
    pub fn read_raw(&mut self, offset: u64, output: &mut [u8]) -> Result<usize, Error> {
        self.prepare()?;
        if offset
            .checked_add(output.len() as u64)
            .is_none_or(|end| end > self.volume_len)
        {
            return Err(Error::InvalidImage);
        }
        self.source.read_at(offset, output).map_err(Error::Storage)
    }
}
fn provider_record(raw: &[u8], offset: u64, limit: u64) -> Result<ProviderEntry, Error> {
    if raw.len() < 34
        || raw[0] as usize > raw.len()
        || raw[0] < 34
        || raw[32] as usize + 33 > raw[0] as usize
    {
        return Err(Error::InvalidRecord);
    }
    if raw[25] & 0x80 != 0 || raw[26] != 0 || raw[27] != 0 {
        return Err(Error::Unsupported);
    }
    let start = (u64::from(both32(&raw[2..10])?) + u64::from(raw[1])) * SECTOR_SIZE as u64;
    let end = start
        .checked_add(u64::from(both32(&raw[10..18])?))
        .filter(|end| *end <= limit)
        .ok_or(Error::InvalidRecord)?;
    Ok(ProviderEntry {
        data: start..end,
        record: offset..offset + u64::from(raw[0]),
        directory: raw[25] & 2 != 0,
    })
}
impl<S: doslet_storage::ReadAt> doslet_vfs::FileSystem for Provider<S> {
    fn lookup(
        &mut self,
        path: &[u8],
    ) -> Result<doslet_vfs::Metadata, doslet_storage::StorageError> {
        let entry = Provider::lookup(self, path).map_err(storage_error)?;
        Ok(doslet_vfs::Metadata {
            node: doslet_vfs::NodeId(entry.record.start),
            length: entry.data.end - entry.data.start,
            attributes: if entry.directory { 0x11 } else { 0x21 },
            directory: entry.directory,
        })
    }
    fn read_at(
        &mut self,
        node: doslet_vfs::NodeId,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, doslet_storage::StorageError> {
        self.prepare().map_err(storage_error)?;
        let (entry, _) = self.record_at(node.0).map_err(storage_error)?;
        self.read_file(&entry, offset, output)
            .map_err(storage_error)
    }
    fn directory_entry(
        &mut self,
        parent: doslet_vfs::NodeId,
        cursor: u32,
    ) -> Result<Option<(doslet_vfs::Name, doslet_vfs::Metadata)>, doslet_storage::StorageError>
    {
        self.prepare().map_err(storage_error)?;
        let (directory, _) = self.record_at(parent.0).map_err(storage_error)?;
        if !directory.directory {
            return Err(doslet_storage::StorageError::Corrupt);
        }
        let mut offset = directory.data.start;
        let mut index = 0;
        while offset < directory.data.end {
            let mut length = [0];
            self.exact(offset, &mut length).map_err(storage_error)?;
            if length[0] == 0 {
                offset = (offset / SECTOR_SIZE as u64 + 1) * SECTOR_SIZE as u64;
                continue;
            }
            let (entry, raw) = self.record_at(offset).map_err(storage_error)?;
            let raw_name = &raw[33..33 + raw[32] as usize];
            offset += u64::from(length[0]);
            if raw_name == [0] || raw_name == [1] || raw[25] & 4 != 0 {
                continue;
            }
            if index < cursor {
                index += 1;
                continue;
            }
            let name = raw_name.split(|b| *b == b';').next().unwrap();
            let name = name.strip_suffix(b".").unwrap_or(name);
            return Ok(Some((
                doslet_vfs::Name::parse(name)?,
                doslet_vfs::Metadata {
                    node: doslet_vfs::NodeId(entry.record.start),
                    length: entry.data.end - entry.data.start,
                    attributes: if entry.directory { 0x11 } else { 0x21 },
                    directory: entry.directory,
                },
            )));
        }
        Ok(None)
    }
}
fn storage_error(error: Error) -> doslet_storage::StorageError {
    match error {
        Error::Storage(error) => error,
        Error::NotFound => doslet_storage::StorageError::MissingBase,
        _ => doslet_storage::StorageError::Corrupt,
    }
}
#[cfg(test)]
mod provider_tests {
    use super::*;
    use alloc::vec;
    include!("../../../../tests/fixtures/iso9660.rs");
    #[test]
    fn positional_source_matches_owned_parser() {
        let bytes = test_iso();
        let mut provider = Provider::new(doslet_storage::VecSource::new(bytes));
        let entry = provider.lookup(b"HEROES/SOUND/HEROES01.62S").unwrap();
        let mut output = [0; 12];
        assert_eq!(provider.read_file(&entry, 0, &mut output), Ok(4));
        assert_eq!(&output[..4], b"data");
    }
}
