//! Preloaded read-only DOS drive. Host acquisition stays outside the DOS core.
use alloc::vec::Vec;
mod overlay;
pub mod search;

const NAME_BYTES: usize = 128;
const HANDLE_COUNT: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Name {
    bytes: [u8; NAME_BYTES],
    len: usize,
}
impl Name {
    const ROOT: Self = Self {
        bytes: [0; NAME_BYTES],
        len: 0,
    };
    fn parse(raw: &[u8]) -> Result<Self, u16> {
        Self::relative(raw, Self::ROOT)
    }
    fn relative(raw: &[u8], current: Self) -> Result<Self, u16> {
        if !raw.is_ascii() || raw.contains(&0) {
            return Err(3);
        }
        let raw = if raw.get(1) == Some(&b':') {
            if !raw[0].eq_ignore_ascii_case(&b'C') {
                return Err(15);
            }
            &raw[2..]
        } else {
            raw
        };
        let mut name = if raw.first().is_some_and(|byte| matches!(byte, b'/' | b'\\')) {
            Self::ROOT
        } else {
            current
        };
        for part in raw.split(|byte| matches!(byte, b'/' | b'\\')) {
            if part.is_empty() || part == b"." {
                continue;
            }
            if part == b".." {
                if name.len == 0 {
                    return Err(3);
                }
                name.len = name.bytes[..name.len]
                    .iter()
                    .rposition(|byte| *byte == b'\\')
                    .unwrap_or(0);
                continue;
            }
            if part
                .iter()
                .any(|byte| matches!(byte, b':' | b'*' | b'?' | b'"' | b'<' | b'>'))
            {
                return Err(3);
            }
            let separator = usize::from(name.len != 0);
            if name.len + separator + part.len() >= NAME_BYTES {
                return Err(3);
            }
            if separator != 0 {
                name.bytes[name.len] = b'\\';
                name.len += 1;
            }
            for byte in part {
                name.bytes[name.len] = byte.to_ascii_uppercase();
                name.len += 1;
            }
        }
        Ok(name)
    }
    fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

struct File {
    name: Name,
    bytes: Vec<u8>,
}
#[derive(Clone, Copy)]
struct Handle {
    file: usize,
    position: u32,
    mode: u8,
}

pub struct ReadOnlyDrive {
    files: Vec<File>,
    handles: [Option<Handle>; HANDLE_COUNT],
    current: Name,
    opens: u64,
    reads: u64,
    last_midi: Option<usize>,
    last_midi_request: Option<(Name, u8, u16)>,
    overlay: Option<overlay::Overlay>,
    new_file_limit: usize,
}
impl Default for ReadOnlyDrive {
    fn default() -> Self {
        Self::new()
    }
}
impl ReadOnlyDrive {
    pub const fn new() -> Self {
        Self {
            files: Vec::new(),
            handles: [None; HANDLE_COUNT],
            current: Name::ROOT,
            opens: 0,
            reads: 0,
            last_midi: None,
            last_midi_request: None,
            overlay: None,
            new_file_limit: 0,
        }
    }
    /// Enable an isolated sparse write overlay during setup. Input buffers and
    /// host files stay immutable; writes use reserved space and 64 fixed extents.
    pub fn with_write_overlay(mut self, capacity: usize) -> Self {
        self.files.reserve(64);
        self.new_file_limit = self.files.len() + 64;
        self.overlay = Some(overlay::Overlay::new(capacity));
        self
    }
    /// Create/truncate an ordinary overlay file using setup-reserved metadata.
    pub fn create(&mut self, path: &[u8], attributes: u16) -> Result<u16, u16> {
        if self.overlay.is_none() || attributes & !0x20 != 0 {
            return Err(5);
        }
        let name = Name::relative(path, self.current)?;
        if name.len == 0 || self.name_attributes(name) == Ok(0x10) {
            return Err(5);
        }
        if let Some(separator) = name.as_bytes().iter().rposition(|byte| *byte == b'\\') {
            let mut parent = name;
            parent.len = separator;
            if self.name_attributes(parent) != Ok(0x10) {
                return Err(3);
            }
        }
        let slot = self.handles.iter().position(Option::is_none).ok_or(4u16)?;
        let index = if let Some(index) = self.files.iter().position(|file| file.name == name) {
            index
        } else {
            if self.files.len() >= self.new_file_limit {
                return Err(8);
            }
            let index = self.files.len();
            // Verify overlay metadata before publishing the new name.
            self.overlay.as_mut().unwrap().reserve(index, 0, 0, 0)?;
            self.files.push(File {
                name,
                bytes: Vec::new(),
            });
            index
        };
        self.overlay.as_mut().unwrap().reserve(
            index,
            0,
            0,
            self.files[index].bytes.len() as u32,
        )?;
        self.handles[slot] = Some(Handle {
            file: index,
            position: 0,
            mode: 2,
        });
        self.opens += 1;
        Ok(slot as u16 + 5)
    }
    /// Transfer ownership once during setup; mounting is not a guest execution operation.
    pub fn mount(&mut self, path: &[u8], bytes: Vec<u8>) -> Result<(), u16> {
        let name = Name::parse(path)?;
        if name.len == 0 || bytes.len() > u32::MAX as usize {
            return Err(3);
        }
        if self
            .files
            .iter()
            .any(|file| file.name.as_bytes() == name.as_bytes())
        {
            return Err(5);
        }
        self.files.push(File { name, bytes });
        Ok(())
    }
    fn find(&self, path: &[u8]) -> Result<usize, u16> {
        let name = Name::relative(path, self.current)?;
        self.files
            .iter()
            .position(|file| file.name.as_bytes() == name.as_bytes())
            .ok_or(2)
    }
    pub fn file(&self, path: &[u8]) -> Result<&[u8], u16> {
        Ok(&self.files[self.find(path)?].bytes)
    }
    /// Search ordinary 8.3 files in the current directory without allocating.
    pub fn find_fcb(&self, pattern: &[u8; 11], start: usize) -> Option<(usize, [u8; 11], u32)> {
        for (index, file) in self.files.iter().enumerate().skip(start) {
            let path = file.name.as_bytes();
            let split = path.iter().rposition(|byte| *byte == b'\\');
            let (directory, name) =
                split.map_or((&b""[..], path), |i| (&path[..i], &path[i + 1..]));
            if directory != self.current.as_bytes() {
                continue;
            }
            let dot = name
                .iter()
                .position(|byte| *byte == b'.')
                .unwrap_or(name.len());
            let (base, ext) = (&name[..dot], name.get(dot + 1..).unwrap_or_default());
            if base.len() > 8 || ext.len() > 3 {
                continue;
            }
            let mut short = [b' '; 11];
            short[..base.len()].copy_from_slice(base);
            short[8..8 + ext.len()].copy_from_slice(ext);
            if pattern
                .iter()
                .zip(short)
                .all(|(p, b)| *p == b'?' || p.to_ascii_uppercase() == b)
            {
                let length = self
                    .overlay
                    .as_ref()
                    .map_or(file.bytes.len() as u32, |overlay| {
                        overlay.length(index, file.bytes.len() as u32)
                    });
                return Some((index + 1, short, length));
            }
        }
        None
    }
    pub fn attributes(&self, path: &[u8]) -> Result<u16, u16> {
        self.name_attributes(Name::relative(path, self.current)?)
    }
    fn name_attributes(&self, name: Name) -> Result<u16, u16> {
        if name.len == 0 {
            return Ok(0x10);
        }
        if self
            .files
            .iter()
            .any(|file| file.name.as_bytes() == name.as_bytes())
        {
            return Ok(0x20);
        }
        if self.files.iter().any(|file| {
            file.name.as_bytes().starts_with(name.as_bytes())
                && file.name.as_bytes().get(name.len) == Some(&b'\\')
        }) {
            return Ok(0x10);
        }
        Err(2)
    }
    /// Directory names are inferred from mounted file paths. DOS's current
    /// directory ABI reserves 64 bytes including the final NUL.
    pub fn change_directory(&mut self, path: &[u8]) -> Result<(), u16> {
        let name = Name::relative(path, self.current)?;
        if name.len > 63 || self.name_attributes(name).unwrap_or(0) & 0x10 == 0 {
            return Err(3);
        }
        self.current = name;
        Ok(())
    }
    /// No drive prefix, leading separator or NUL, as in INT 21h/AH=47h.
    pub fn current_directory(&self) -> &[u8] {
        self.current.as_bytes()
    }
    pub fn open(&mut self, path: &[u8], mode: u8) -> Result<u16, u16> {
        let name = Name::relative(path, self.current)?;
        let result = self.open_inner(path, mode);
        if name.as_bytes().ends_with(b".MID") {
            self.last_midi_request = Some((name, mode, result.err().unwrap_or(0)));
        }
        result
    }
    fn open_inner(&mut self, path: &[u8], mode: u8) -> Result<u16, u16> {
        if mode & 3 == 3 || (mode & 3 != 0 && self.overlay.is_none()) {
            return Err(5);
        }
        let file = self.find(path)?;
        let slot = self.handles.iter().position(Option::is_none).ok_or(4u16)?;
        self.handles[slot] = Some(Handle {
            file,
            position: 0,
            mode: mode & 3,
        });
        self.opens += 1;
        if self.files[file].name.as_bytes().ends_with(b".MID") {
            self.last_midi = Some(file);
        }
        Ok(slot as u16 + 5)
    }
    pub fn midi_request(&self) -> Option<(&[u8], u8, u16)> {
        self.last_midi_request
            .as_ref()
            .map(|(name, mode, error)| (name.as_bytes(), *mode, *error))
    }
    fn slot(handle: u16) -> Result<usize, u16> {
        let index = handle.checked_sub(5).ok_or(6u16)? as usize;
        if index >= HANDLE_COUNT {
            return Err(6);
        }
        Ok(index)
    }
    pub fn close(&mut self, handle: u16) -> Result<(), u16> {
        self.handles[Self::slot(handle)?].take().ok_or(6u16)?;
        Ok(())
    }
    pub fn is_open(&self, handle: u16) -> bool {
        Self::slot(handle).is_ok_and(|index| self.handles[index].is_some())
    }
    pub fn read(&mut self, handle: u16, count: u16) -> Result<&[u8], u16> {
        let handle = self.handles[Self::slot(handle)?].as_mut().ok_or(6u16)?;
        if handle.mode == 1 {
            return Err(5);
        }
        if self
            .overlay
            .as_ref()
            .is_some_and(|overlay| overlay.has_patches(handle.file))
        {
            return Err(1); // A fragmented view requires read_view, not a contiguous slice.
        }
        self.reads += 1;
        let bytes = &self.files[handle.file].bytes;
        let start = (handle.position as usize).min(bytes.len());
        let length = (bytes.len() - start).min(usize::from(count));
        handle.position += length as u32;
        Ok(&bytes[start..start + length])
    }
    pub fn read_view(&mut self, handle: u16, count: u16) -> Result<ReadView<'_>, u16> {
        let handle = self.handles[Self::slot(handle)?].as_mut().ok_or(6u16)?;
        if handle.mode == 1 {
            return Err(5);
        }
        let original = &self.files[handle.file].bytes;
        let length = self
            .overlay
            .as_ref()
            .map_or(original.len() as u32, |overlay| {
                overlay.length(handle.file, original.len() as u32)
            });
        let start = handle.position;
        let count = length.saturating_sub(start).min(u32::from(count)) as usize;
        handle.position += count as u32;
        self.reads += 1;
        Ok(ReadView {
            original,
            overlay: self
                .overlay
                .as_ref()
                .filter(|overlay| overlay.has_patches(handle.file)),
            file: handle.file,
            start,
            len: count,
        })
    }
    pub fn write_buffer(&mut self, handle: u16, count: u16) -> Result<&mut [u8], u16> {
        let handle = self.handles[Self::slot(handle)?].as_mut().ok_or(6u16)?;
        if handle.mode == 0 {
            return Err(5);
        }
        let original_len = self.files[handle.file].bytes.len() as u32;
        let bytes = self.overlay.as_mut().ok_or(5u16)?.reserve(
            handle.file,
            handle.position,
            usize::from(count),
            original_len,
        )?;
        handle.position += u32::from(count);
        Ok(bytes)
    }
    pub fn access_stats(&self) -> (u64, u64, Option<&[u8]>) {
        (
            self.opens,
            self.reads,
            self.last_midi.map(|file| self.files[file].name.as_bytes()),
        )
    }
    pub fn seek(&mut self, handle: u16, origin: u8, displacement: i32) -> Result<u32, u16> {
        let handle = self.handles[Self::slot(handle)?].as_mut().ok_or(6u16)?;
        let base =
            match origin {
                0 => 0,
                1 => i64::from(handle.position),
                2 => i64::from(self.overlay.as_ref().map_or(
                    self.files[handle.file].bytes.len() as u32,
                    |overlay| {
                        overlay.length(handle.file, self.files[handle.file].bytes.len() as u32)
                    },
                )),
                _ => return Err(1),
            };
        let position = base + i64::from(displacement);
        if !(0..=i64::from(u32::MAX)).contains(&position) {
            return Err(1);
        }
        handle.position = position as u32;
        Ok(handle.position)
    }
}
/// Borrowed merged view of base bytes and sparse overlay extents, without copying.
pub struct ReadView<'a> {
    original: &'a [u8],
    overlay: Option<&'a overlay::Overlay>,
    file: usize,
    start: u32,
    len: usize,
}
impl ReadView<'_> {
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn byte(&self, offset: usize) -> Option<u8> {
        if offset >= self.len {
            return None;
        }
        let position = self.start + offset as u32;
        Some(self.overlay.map_or_else(
            || self.original[position as usize],
            |overlay| overlay.byte(self.file, position, self.original),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn created_overlay_files_can_be_reopened_truncated_and_searched() {
        let mut drive = ReadOnlyDrive::new().with_write_overlay(256);
        assert_eq!(drive.create(b"MISSING/SAVE.SAV", 0), Err(3));
        let handle = drive.create(b"SAVE.SAV", 0).unwrap();
        drive
            .write_buffer(handle, 4)
            .unwrap()
            .copy_from_slice(b"save");
        drive.close(handle).unwrap();
        let handle = drive.open(b"save.sav", 0).unwrap();
        let view = drive.read_view(handle, 100).unwrap();
        assert_eq!(
            (0..view.len())
                .map(|i| view.byte(i).unwrap())
                .collect::<Vec<_>>(),
            b"save"
        );
        drive.close(handle).unwrap();
        assert_eq!(drive.find_fcb(b"SAVE    SAV", 0).unwrap().2, 4);
        let handle = drive.create(b"SAVE.SAV", 0).unwrap();
        assert_eq!(drive.read_view(handle, 100).unwrap().len(), 0);
        drive.close(handle).unwrap();
        assert_eq!(drive.find_fcb(b"???????????", 0).unwrap().2, 0);
        assert!(ReadOnlyDrive::new().create(b"SAVE.SAV", 0).is_err());
    }
    #[test]
    fn sparse_overlay_merges_overwrites_truncation_and_extensions_without_changing_inputs() {
        let mut drive = ReadOnlyDrive::new().with_write_overlay(8);
        drive.mount(b"TEST", Vec::from(&b"abcdef"[..])).unwrap();
        let handle = drive.open(b"TEST", 2).unwrap();
        drive.seek(handle, 0, 2).unwrap();
        drive
            .write_buffer(handle, 2)
            .unwrap()
            .copy_from_slice(b"XY");
        drive.seek(handle, 0, 3).unwrap();
        drive.write_buffer(handle, 1).unwrap().copy_from_slice(b"Z");
        drive.seek(handle, 0, 0).unwrap();
        let view = drive.read_view(handle, 20).unwrap();
        assert_eq!(
            (0..view.len())
                .map(|i| view.byte(i).unwrap())
                .collect::<Vec<_>>(),
            b"abXZef"
        );
        assert_eq!(drive.file(b"TEST").unwrap(), b"abcdef");
        drive.seek(handle, 0, 1).unwrap();
        drive.write_buffer(handle, 0).unwrap();
        drive.seek(handle, 0, 5).unwrap();
        drive.write_buffer(handle, 1).unwrap()[0] = b'!';
        drive.seek(handle, 0, 0).unwrap();
        let view = drive.read_view(handle, 20).unwrap();
        assert_eq!(
            (0..view.len())
                .map(|i| view.byte(i).unwrap())
                .collect::<Vec<_>>(),
            b"a\0\0\0\0!"
        );
        assert_eq!(drive.seek(handle, 2, 0), Ok(6));
        assert_eq!(drive.write_buffer(handle, 5), Err(8));
        assert_eq!(drive.seek(handle, 1, 0), Ok(6)); // Failed write leaves the handle position.
        assert_eq!(drive.file(b"TEST").unwrap(), b"abcdef");
        let read_only = drive.open(b"TEST", 0).unwrap();
        assert_eq!(drive.write_buffer(read_only, 1), Err(5));
    }
    #[test]
    fn current_directory_resolves_relative_paths_and_survives_failed_changes() {
        let mut drive = ReadOnlyDrive::new();
        drive
            .mount(b"DIR/SUB/TEST.DAT", Vec::from(&b"inside"[..]))
            .unwrap();
        drive.mount(b"TEST.DAT", Vec::from(&b"root"[..])).unwrap();
        drive.change_directory(b"C:\\DIR").unwrap();
        drive.change_directory(b"sub").unwrap();
        assert_eq!(drive.current_directory(), b"DIR\\SUB");
        assert_eq!(drive.file(b"c:test.dat"), Ok(&b"inside"[..]));
        assert_eq!(drive.file(b"\\TEST.DAT"), Ok(&b"root"[..]));
        assert_eq!(drive.change_directory(b"TEST.DAT"), Err(3));
        assert_eq!(drive.change_directory(b"missing"), Err(3));
        assert_eq!(drive.current_directory(), b"DIR\\SUB");
        drive.change_directory(b"..").unwrap();
        assert_eq!(drive.current_directory(), b"DIR");
        drive.change_directory(b"C:/").unwrap();
        assert_eq!(drive.current_directory(), b"");
    }
    #[test]
    fn names_handles_seeking_and_read_only_policy() {
        let mut drive = ReadOnlyDrive::new();
        drive
            .mount(b"DIR/TEST.DAT", Vec::from(&b"abcdef"[..]))
            .unwrap();
        assert_eq!(drive.attributes(b"c:\\dir"), Ok(0x10));
        assert_eq!(drive.attributes(b"c:\\dir\\test.dat"), Ok(0x20));
        let handle = drive.open(b"C:/DIR/./OTHER/../test.dat", 0).unwrap();
        assert_eq!(drive.read(handle, 3), Ok(&b"abc"[..]));
        assert_eq!(drive.seek(handle, 2, -2), Ok(4));
        assert_eq!(drive.read(handle, 20), Ok(&b"ef"[..]));
        assert_eq!(drive.read(handle, 20), Ok(&b""[..]));
        assert_eq!(drive.seek(handle, 0, -1), Err(1));
        assert_eq!(drive.seek(handle, 0, 100), Ok(100));
        assert_eq!(drive.read(handle, 1), Ok(&b""[..]));
        assert_eq!(drive.open(b"DIR/TEST.DAT", 2), Err(5));
        assert_eq!(drive.open(b"../DIR/TEST.DAT", 0), Err(3));
        assert_eq!(drive.open(b"D:\\DIR\\TEST.DAT", 0), Err(15));
        drive.close(handle).unwrap();
        assert_eq!(drive.read(handle, 1), Err(6));
        assert_eq!(drive.close(handle), Err(6));
    }
    #[test]
    fn handle_exhaustion_is_finite_and_closed_slots_are_reused() {
        let mut drive = ReadOnlyDrive::new();
        drive.mount(b"X", Vec::new()).unwrap();
        for expected in 5..5 + HANDLE_COUNT as u16 {
            assert_eq!(drive.open(b"X", 0), Ok(expected));
        }
        assert_eq!(drive.open(b"X", 0), Err(4));
        drive.close(6).unwrap();
        assert_eq!(drive.open(b"X", 0), Ok(6));
    }
}
