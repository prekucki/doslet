use super::RuntimeError;
use alloc::vec::Vec;
use doslet_dos::files::ReadView;
use doslet_iso9660::{Entry, Image};
pub(super) const HANDLE_BASE: u16 = 5 + doslet_dos::files::HANDLE_COUNT as u16;
const HANDLES: usize = 32;
struct Handle {
    entry: Entry,
    position: u32,
}

pub(super) struct CdromImage {
    pub drive: u8,
    pub image: Image,
    handles: [Option<Handle>; HANDLES],
}
impl CdromImage {
    pub fn new(drive: u8, bytes: Vec<u8>) -> Result<Self, RuntimeError> {
        if !(b'D'..=b'Z').contains(&drive.to_ascii_uppercase()) {
            return Err(RuntimeError::InvalidCdromImage);
        }
        let image = Image::new(bytes).map_err(|_| RuntimeError::InvalidCdromImage)?;
        Ok(Self {
            drive: drive.to_ascii_uppercase(),
            image,
            handles: core::array::from_fn(|_| None),
        })
    }
    pub fn matches_path(&self, path: &[u8]) -> bool {
        path.len() >= 2 && path[1] == b':' && path[0].to_ascii_uppercase() == self.drive
    }
    pub fn attributes(&self, path: &[u8]) -> Result<u16, u16> {
        let entry = self.image.lookup(&path[2..]).map_err(|_| 2u16)?;
        Ok(if entry.directory { 0x11 } else { 0x21 })
    }
    pub fn open(&mut self, path: &[u8], mode: u8) -> Result<u16, u16> {
        if mode & 3 != 0 {
            return Err(5);
        }
        let entry = self.image.lookup(&path[2..]).map_err(|_| 2u16)?;
        if entry.directory {
            return Err(5);
        }
        let index = self.handles.iter().position(Option::is_none).ok_or(4u16)?;
        self.handles[index] = Some(Handle { entry, position: 0 });
        Ok(HANDLE_BASE + index as u16)
    }
    pub fn owns_handle(handle: u16) -> bool {
        (HANDLE_BASE..HANDLE_BASE + HANDLES as u16).contains(&handle)
    }
    pub fn is_open(&self, handle: u16) -> bool {
        handle
            .checked_sub(HANDLE_BASE)
            .and_then(|index| self.handles.get(usize::from(index)))
            .is_some_and(Option::is_some)
    }
    fn handle(&mut self, handle: u16) -> Result<&mut Handle, u16> {
        self.handles
            .get_mut(usize::from(handle.checked_sub(HANDLE_BASE).ok_or(6u16)?))
            .and_then(Option::as_mut)
            .ok_or(6)
    }
    pub fn close(&mut self, handle: u16) -> Result<(), u16> {
        self.handle(handle)?;
        self.handles[usize::from(handle - HANDLE_BASE)] = None;
        Ok(())
    }
    pub fn read(&mut self, handle: u16, count: u16) -> Result<ReadView<'_>, u16> {
        let handle = self.handle(handle)?;
        let start =
            handle.entry.data.start + (handle.position as usize).min(handle.entry.data.len());
        let length = (handle.entry.data.end - start).min(usize::from(count));
        handle.position += length as u32;
        Ok(ReadView::contiguous(
            &self.image.bytes()[start..start + length],
        ))
    }
    pub fn seek(&mut self, handle: u16, origin: u8, displacement: i32) -> Result<u32, u16> {
        let handle = self.handle(handle)?;
        let base = match origin {
            0 => 0,
            1 => i64::from(handle.position),
            2 => handle.entry.data.len() as i64,
            _ => return Err(1),
        };
        handle.position = u32::try_from(base + i64::from(displacement)).map_err(|_| 1u16)?;
        Ok(handle.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    include!("../../../../../tests/fixtures/iso9660.rs");
    #[test]
    fn cd_files_are_readonly_and_handles_do_not_alias_c_drive() {
        let mut cd = CdromImage::new(b'd', test_iso()).unwrap();
        let path = b"D:\\HEROES\\SOUND\\HEROES01.62S";
        assert!(cd.matches_path(path));
        assert_eq!(cd.open(path, 1), Err(5));
        assert_eq!(cd.attributes(path), Ok(0x21));
        let handle = cd.open(path, 0).unwrap();
        assert_eq!(handle, HANDLE_BASE);
        assert_eq!(cd.seek(handle, 2, -2), Ok(2));
        let data = cd.read(handle, 20).unwrap();
        assert_eq!(data.len(), 2);
        assert_eq!(data.byte(0), Some(b't'));
        assert_eq!(data.byte(1), Some(b'a'));
        assert!(cd.read(handle, 20).unwrap().is_empty());
        assert_eq!(cd.seek(handle, 0, -1), Err(1));
        cd.close(handle).unwrap();
        assert_eq!(cd.close(handle), Err(6));
        assert_eq!(cd.open(path, 0), Ok(handle));
        assert_eq!(cd.open(b"D:\\MISSING", 0), Err(2));
    }
}
