use super::RuntimeError;
use alloc::{boxed::Box, vec::Vec};
use doslet_iso9660::{Entry, Image, Provider, ProviderEntry};
use doslet_storage::{CachedSource, HostCompletion, HostRequest, StorageError};
pub(super) const HANDLE_BASE: u16 = 5 + doslet_dos::files::HANDLE_COUNT as u16;
const HANDLES: usize = 96;
pub(super) const PENDING: u16 = 0xFFFF;
#[derive(Clone)]
enum FileEntry {
    Local(Entry),
    Remote(ProviderEntry),
}
struct Handle {
    entry: FileEntry,
    position: u32,
}
pub(super) enum Media {
    Local(Image),
    Remote(Box<Provider<CachedSource>>),
}
pub(super) struct CdromImage {
    pub drive: u8,
    pub image: Media,
    handles: [Option<Handle>; HANDLES],
}
impl CdromImage {
    pub fn new(drive: u8, bytes: Vec<u8>) -> Result<Self, RuntimeError> {
        let image = Image::new(bytes).map_err(|_| RuntimeError::InvalidCdromImage)?;
        Self::attach(drive, Media::Local(image))
    }
    pub fn from_source(drive: u8, source: CachedSource) -> Result<Self, RuntimeError> {
        Self::attach(drive, Media::Remote(Box::new(Provider::new(source))))
    }
    fn attach(drive: u8, image: Media) -> Result<Self, RuntimeError> {
        if !(b'D'..=b'Z').contains(&drive.to_ascii_uppercase()) {
            return Err(RuntimeError::InvalidCdromImage);
        }
        Ok(Self {
            drive: drive.to_ascii_uppercase(),
            image,
            handles: core::array::from_fn(|_| None),
        })
    }
    pub fn matches_path(&self, path: &[u8]) -> bool {
        path.len() >= 2 && path[1] == b':' && path[0].to_ascii_uppercase() == self.drive
    }
    pub fn next_request(&mut self) -> Option<HostRequest> {
        match &mut self.image {
            Media::Remote(provider) => provider.source_mut().next_request(),
            _ => None,
        }
    }
    pub fn complete(&mut self, completion: HostCompletion) -> Result<(), StorageError> {
        match &mut self.image {
            Media::Remote(provider) => provider.source_mut().complete(completion),
            _ => Err(StorageError::StaleCompletion),
        }
    }
    pub fn pending(&self) -> usize {
        match &self.image {
            Media::Remote(provider) => provider.source().pending(),
            _ => 0,
        }
    }
    pub fn length(&self) -> u64 {
        match &self.image {
            Media::Local(image) => image.bytes().len() as u64,
            Media::Remote(provider) => doslet_storage::ReadAt::identity(provider.source()).length,
        }
    }
    pub fn attributes(&mut self, path: &[u8]) -> Result<u16, u16> {
        let dir = match &mut self.image {
            Media::Local(image) => image.lookup(&path[2..]).map_err(error)?.directory,
            Media::Remote(image) => image.lookup(&path[2..]).map_err(error)?.directory,
        };
        Ok(if dir { 0x11 } else { 0x21 })
    }
    pub fn open(&mut self, path: &[u8], mode: u8) -> Result<u16, u16> {
        if mode & 3 != 0 {
            return Err(5);
        }
        let entry = match &mut self.image {
            Media::Local(image) => FileEntry::Local(image.lookup(&path[2..]).map_err(error)?),
            Media::Remote(image) => FileEntry::Remote(image.lookup(&path[2..]).map_err(error)?),
        };
        if match &entry {
            FileEntry::Local(e) => e.directory,
            FileEntry::Remote(e) => e.directory,
        } {
            return Err(5);
        }
        let index = self.handles.iter().position(Option::is_none).ok_or(4u16)?;
        self.handles[index] = Some(Handle { entry, position: 0 });
        Ok(HANDLE_BASE + index as u16)
    }
    pub fn owns_handle(handle: u16) -> bool {
        (HANDLE_BASE..HANDLE_BASE + HANDLES as u16).contains(&handle)
    }
    fn handle(&mut self, handle: u16) -> Result<&mut Handle, u16> {
        self.handles
            .get_mut(handle.checked_sub(HANDLE_BASE).ok_or(6u16)? as usize)
            .and_then(Option::as_mut)
            .ok_or(6)
    }
    pub fn close_all(&mut self) {
        for handle in &mut self.handles {
            *handle = None;
        }
    }
    pub fn close(&mut self, handle: u16) -> Result<(), u16> {
        self.handle(handle)?;
        self.handles[(handle - HANDLE_BASE) as usize] = None;
        Ok(())
    }
    pub fn prepare_read(&mut self, handle: u16, count: u16) -> Result<(u32, usize), u16> {
        let h = self.handle(handle)?;
        let length = match &h.entry {
            FileEntry::Local(entry) => entry.data.len() as u64,
            FileEntry::Remote(entry) => entry.data.end - entry.data.start,
        };
        Ok((
            h.position,
            length
                .saturating_sub(u64::from(h.position))
                .min(u64::from(count)) as usize,
        ))
    }
    pub fn read_at_into(
        &mut self,
        handle: u16,
        position: u32,
        output: &mut [u8],
    ) -> Result<usize, u16> {
        let entry = self.handle(handle)?.entry.clone();
        match (&mut self.image, entry) {
            (Media::Local(image), FileEntry::Local(entry)) => {
                let start = entry.data.start + (position as usize).min(entry.data.len());
                let count = output.len().min(entry.data.end - start);
                output[..count].copy_from_slice(&image.bytes()[start..start + count]);
                Ok(count)
            }
            (Media::Remote(image), FileEntry::Remote(entry)) => image
                .read_file(&entry, u64::from(position), output)
                .map_err(error),
            _ => Err(6),
        }
    }
    pub fn finish_read(&mut self, handle: u16, count: usize) -> Result<(), u16> {
        self.handle(handle)?.position += count as u32;
        Ok(())
    }
    pub fn seek(&mut self, handle: u16, origin: u8, displacement: i32) -> Result<u32, u16> {
        let h = self.handle(handle)?;
        let length = match &h.entry {
            FileEntry::Local(e) => e.data.len() as u64,
            FileEntry::Remote(e) => e.data.end - e.data.start,
        };
        let base = match origin {
            0 => 0,
            1 => i64::from(h.position),
            2 => i64::try_from(length).map_err(|_| 1u16)?,
            _ => return Err(1),
        };
        h.position = u32::try_from(base + i64::from(displacement)).map_err(|_| 1u16)?;
        Ok(h.position)
    }
    pub fn primary(&mut self) -> Result<u64, u16> {
        match &mut self.image {
            Media::Local(image) => Ok(image.primary_descriptor() as u64),
            Media::Remote(image) => image.primary_descriptor().map_err(error),
        }
    }
    pub fn raw(&mut self, offset: u64, output: &mut [u8]) -> Result<usize, u16> {
        match &mut self.image {
            Media::Local(image) => {
                let start = usize::try_from(offset).map_err(|_| 21u16)?;
                let bytes = image
                    .bytes()
                    .get(start..start + output.len())
                    .ok_or(21u16)?;
                output.copy_from_slice(bytes);
                Ok(bytes.len())
            }
            Media::Remote(image) => image.read_raw(offset, output).map_err(error),
        }
    }
}
fn error(error: doslet_iso9660::Error) -> u16 {
    match error {
        doslet_iso9660::Error::Storage(StorageError::Pending) => PENDING,
        doslet_iso9660::Error::NotFound => 2,
        _ => 21,
    }
}
