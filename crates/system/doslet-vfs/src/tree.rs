//! Preloaded guest directory tree with stable file nodes and a sparse upper.
//! Host acquisition remains in platform adapters; DOS names use ASCII folding.
use alloc::{sync::Arc, vec::Vec};
mod overlay;
pub mod search;

const NAME_BYTES: usize = 128;
pub const HANDLE_COUNT: usize = 96;

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

// Empty files created by the guest must not allocate an Arc after setup.
#[derive(Clone, Default)]
struct FileBytes(Option<Arc<Vec<u8>>>);
impl From<Vec<u8>> for FileBytes {
    fn from(bytes: Vec<u8>) -> Self {
        Self((!bytes.is_empty()).then(|| Arc::new(bytes)))
    }
}
impl core::ops::Deref for FileBytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.0.as_ref().map_or(&[], |bytes| bytes.as_slice())
    }
}

#[derive(Clone)]
struct File {
    name: Name,
    bytes: FileBytes,
    base_name: Name,
    deleted: bool,
    directory: bool,
}
#[derive(Clone, Copy)]
struct Handle {
    file: usize,
    position: u32,
    mode: u8,
}

#[derive(Clone)]
pub struct DirectoryTree {
    files: Vec<File>,
    handles: [Option<usize>; HANDLE_COUNT],
    descriptions: [Option<Handle>; HANDLE_COUNT],
    namespace_dirty: bool,
    current: Name,
    opens: u64,
    reads: u64,
    last_midi: Option<usize>,
    last_midi_request: Option<(Name, u8, u16)>,
    overlay: Option<overlay::Overlay>,
    new_file_limit: usize,
}
/// Read-only view of a file after applying the sparse writable upper layer.
/// Intended for host-side export; reading does not mutate DOS file handles.
pub struct VisibleFile<'a> {
    tree: &'a DirectoryTree,
    index: usize,
}
impl VisibleFile<'_> {
    pub fn name(&self) -> &[u8] {
        self.tree.files[self.index].name.as_bytes()
    }
    pub fn len(&self) -> u32 {
        let original = self.tree.files[self.index].bytes.len() as u32;
        self.tree
            .overlay
            .as_ref()
            .map_or(original, |upper| upper.length(self.index, original))
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    pub fn read_into(&self, position: u32, output: &mut [u8]) {
        let original = &self.tree.files[self.index].bytes;
        let count = output
            .len()
            .min(self.len().saturating_sub(position) as usize);
        let (within, beyond) = output.split_at_mut(count);
        beyond.fill(0);
        if let Some(upper) = &self.tree.overlay {
            upper.read_into(self.index, position, original, within);
        } else {
            within.fill(0);
            let count = within
                .len()
                .min(original.len().saturating_sub(position as usize));
            if count != 0 {
                within[..count]
                    .copy_from_slice(&original[position as usize..position as usize + count]);
            }
        }
    }
}
impl Default for DirectoryTree {
    fn default() -> Self {
        Self::new()
    }
}
impl DirectoryTree {
    /// Files visible to DOS, including created and renamed files and their
    /// effective contents. Deleted files and directory records are omitted.
    pub fn visible_files(&self) -> impl Iterator<Item = VisibleFile<'_>> {
        self.files.iter().enumerate().filter_map(|(index, file)| {
            (!file.deleted && !file.directory).then_some(VisibleFile { tree: self, index })
        })
    }
    /// ZIP mounting infers directories from file paths, so an empty guest
    /// directory cannot currently be represented in a flattened archive.
    pub fn has_empty_directories(&self) -> bool {
        self.files.iter().any(|directory| {
            !directory.deleted
                && directory.directory
                && !self.files.iter().any(|file| {
                    !file.deleted
                        && !file.directory
                        && file.name.as_bytes().starts_with(directory.name.as_bytes())
                        && file.name.as_bytes().get(directory.name.len) == Some(&b'\\')
                })
        })
    }
    /// Mounted input plus the bounded writable overlay, in 512-byte clusters.
    /// Free space describes the emulated write budget, not the host filesystem.
    pub fn disk_space(&self) -> (u64, u64) {
        let input = self
            .files
            .iter()
            .map(|file| (file.bytes.len() as u64).div_ceil(512))
            .sum::<u64>();
        let (free, capacity) = self
            .overlay
            .as_ref()
            .map_or((0, 0), |overlay| overlay.space());
        (
            free as u64 / 512,
            1 + input + (capacity as u64).div_ceil(512),
        )
    }
    pub const fn new() -> Self {
        Self {
            files: Vec::new(),
            handles: [None; HANDLE_COUNT],
            descriptions: [None; HANDLE_COUNT],
            namespace_dirty: false,
            current: Name::ROOT,
            opens: 0,
            reads: 0,
            last_midi: None,
            last_midi_request: None,
            overlay: None,
            new_file_limit: 0,
        }
    }
    /// Reserve an isolated sparse upper during setup. Ordinary capacities use
    /// 4-KiB changed chunks and 64 length records. Sub-chunk legacy capacities
    /// retain compact extents. Immutable lower file buffers are never copied.
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
        let index = if let Some(index) = self
            .files
            .iter()
            .position(|file| !file.deleted && file.name == name)
        {
            index
        } else {
            if self.files.len() >= self.new_file_limit {
                return Err(8);
            }
            let index = self.files.len();
            // Verify overlay metadata before publishing the new name.
            self.overlay.as_mut().unwrap().reserve(index, 0, 0, &[])?;
            self.files.push(File {
                name,
                base_name: Name::ROOT,
                deleted: false,
                directory: false,
                bytes: FileBytes::default(),
            });
            index
        };
        self.overlay
            .as_mut()
            .unwrap()
            .reserve(index, 0, 0, &self.files[index].bytes)?;
        let description = self
            .descriptions
            .iter()
            .position(Option::is_none)
            .ok_or(4u16)?;
        self.handles[slot] = Some(description);
        self.descriptions[description] = Some(Handle {
            file: index,
            position: 0,
            mode: 2,
        });
        self.opens += 1;
        self.namespace_dirty = true;
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
            .any(|file| !file.deleted && file.name.as_bytes() == name.as_bytes())
        {
            return Err(5);
        }
        if self.overlay.is_some() {
            self.files.reserve(65);
            self.new_file_limit += 1;
        }
        self.files.push(File {
            name,
            base_name: name,
            deleted: false,
            directory: false,
            bytes: FileBytes::from(bytes),
        });
        Ok(())
    }
    fn find(&self, path: &[u8]) -> Result<usize, u16> {
        let name = Name::relative(path, self.current)?;
        self.files
            .iter()
            .position(|file| !file.deleted && file.name.as_bytes() == name.as_bytes())
            .ok_or(2)
    }
    pub fn file(&self, path: &[u8]) -> Result<&[u8], u16> {
        Ok(&self.files[self.find(path)?].bytes)
    }
    /// Search ordinary 8.3 files in the current directory without allocating.
    pub fn find_fcb(&self, pattern: &[u8; 11], start: usize) -> Option<(usize, [u8; 11], u32)> {
        for (index, file) in self.files.iter().enumerate().skip(start) {
            if file.deleted || file.directory {
                continue;
            }
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
        if let Some(file) = self
            .files
            .iter()
            .find(|file| !file.deleted && file.name.as_bytes() == name.as_bytes())
        {
            return Ok(if file.directory { 0x10 } else { 0x20 });
        }
        if self.files.iter().any(|file| {
            !file.deleted
                && file.name.as_bytes().starts_with(name.as_bytes())
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
        if self.files[file].directory {
            return Err(5);
        }
        let slot = self.handles.iter().position(Option::is_none).ok_or(4u16)?;
        let description = self
            .descriptions
            .iter()
            .position(Option::is_none)
            .ok_or(4u16)?;
        self.handles[slot] = Some(description);
        self.descriptions[description] = Some(Handle {
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
        let description = self.handles[Self::slot(handle)?].take().ok_or(6u16)?;
        if !self.handles.contains(&Some(description)) {
            self.descriptions[description] = None;
        }
        Ok(())
    }
    pub fn close_all(&mut self) {
        self.handles.fill(None);
        self.descriptions.fill(None);
    }
    pub fn is_open(&self, handle: u16) -> bool {
        Self::slot(handle).is_ok_and(|index| self.handles[index].is_some())
    }
    pub fn read(&mut self, handle: u16, count: u16) -> Result<&[u8], u16> {
        let index = self.handles[Self::slot(handle)?].ok_or(6u16)?;
        let handle = self.descriptions[index].as_mut().ok_or(6u16)?;
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
        let index = self.handles[Self::slot(handle)?].ok_or(6u16)?;
        let handle = self.descriptions[index].as_mut().ok_or(6u16)?;
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
        let index = self.handles[Self::slot(handle)?].ok_or(6u16)?;
        let handle = self.descriptions[index].as_mut().ok_or(6u16)?;
        if handle.mode == 0 {
            return Err(5);
        }
        let original = &self.files[handle.file].bytes;
        let bytes = self.overlay.as_mut().ok_or(5u16)?.reserve(
            handle.file,
            handle.position,
            usize::from(count),
            original,
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
    /// Platform writeback visits only dirty files after all their handles close.
    /// A failed write remains dirty and can be retried by the platform adapter.
    pub fn flush_writes<E>(
        &mut self,
        mut write: impl FnMut(&[u8], ReadView<'_>) -> Result<(), E>,
    ) -> Result<usize, E> {
        let Some(overlay) = &mut self.overlay else {
            return Ok(0);
        };
        let mut count = 0;
        for (index, file) in self.files.iter().enumerate() {
            if !overlay.dirty(index)
                || self
                    .descriptions
                    .iter()
                    .flatten()
                    .any(|handle| handle.file == index)
            {
                continue;
            }
            let view = ReadView {
                original: &file.bytes,
                overlay: Some(overlay),
                file: index,
                start: 0,
                len: overlay.length(index, file.bytes.len() as u32) as usize,
            };
            write(file.name.as_bytes(), view)?;
            overlay.acknowledge(index);
            count += 1;
        }
        Ok(count)
    }
    pub fn has_closed_writes(&self) -> bool {
        self.overlay.as_ref().is_some_and(|overlay| {
            overlay.dirty_files().any(|file| {
                !self
                    .descriptions
                    .iter()
                    .flatten()
                    .any(|handle| handle.file == file)
            })
        })
    }
    pub fn seek(&mut self, handle: u16, origin: u8, displacement: i32) -> Result<u32, u16> {
        let index = self.handles[Self::slot(handle)?].ok_or(6u16)?;
        let handle = self.descriptions[index].as_mut().ok_or(6u16)?;
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
impl<'a> ReadView<'a> {
    pub fn contiguous(bytes: &'a [u8]) -> Self {
        Self {
            original: bytes,
            overlay: None,
            file: 0,
            start: 0,
            len: bytes.len(),
        }
    }
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
        let mut drive = DirectoryTree::new().with_write_overlay(256);
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
        assert!(DirectoryTree::new().create(b"SAVE.SAV", 0).is_err());
    }
    #[test]
    fn sparse_overlay_merges_overwrites_truncation_and_extensions_without_changing_inputs() {
        let mut drive = DirectoryTree::new().with_write_overlay(8);
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
        let mut drive = DirectoryTree::new();
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
        let mut drive = DirectoryTree::new();
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
        let mut drive = DirectoryTree::new();
        drive.mount(b"X", Vec::new()).unwrap();
        for expected in 5..5 + HANDLE_COUNT as u16 {
            assert_eq!(drive.open(b"X", 0), Ok(expected));
        }
        assert_eq!(drive.open(b"X", 0), Err(4));
        drive.close(6).unwrap();
        assert_eq!(drive.open(b"X", 0), Ok(6));
    }
}

impl DirectoryTree {
    pub fn duplicate(&mut self, handle: u16) -> Result<u16, u16> {
        let desc = self.handles[Self::slot(handle)?].ok_or(6u16)?;
        let slot = self.handles.iter().position(Option::is_none).ok_or(4u16)?;
        self.handles[slot] = Some(desc);
        Ok(slot as u16 + 5)
    }
    pub fn force_duplicate(&mut self, source: u16, target: u16) -> Result<(), u16> {
        let desc = self.handles[Self::slot(source)?].ok_or(6u16)?;
        let slot = Self::slot(target)?;
        if source == target {
            return Ok(());
        }
        if self.handles[slot].is_some() {
            self.close(target)?;
        }
        self.handles[slot] = Some(desc);
        Ok(())
    }
    pub fn delete(&mut self, path: &[u8]) -> Result<(), u16> {
        if self.overlay.is_none() {
            return Err(5);
        }
        let file = self.find(path)?;
        if self.files[file].directory {
            return Err(5);
        }
        if self.descriptions.iter().flatten().any(|h| h.file == file) {
            return Err(5);
        }
        self.files[file].deleted = true;
        self.namespace_dirty = true;
        Ok(())
    }
    pub fn rename(&mut self, from: &[u8], to: &[u8]) -> Result<(), u16> {
        if self.overlay.is_none() {
            return Err(5);
        }
        let source = Name::relative(from, self.current)?;
        let name = Name::relative(to, self.current)?;
        if source.len == 0 || name.len == 0 || self.name_attributes(name).is_ok() {
            return Err(5);
        }
        let attributes = self.name_attributes(source)?;
        if let Some(separator) = name.as_bytes().iter().rposition(|b| *b == b'\\') {
            let mut parent = name;
            parent.len = separator;
            if self.name_attributes(parent) != Ok(0x10) {
                return Err(3);
            }
        }
        if attributes & 0x10 == 0 {
            let file = self.find(from)?;
            self.files[file].name = name;
        } else {
            if name.as_bytes().starts_with(source.as_bytes())
                && name.as_bytes().get(source.len) == Some(&b'\\')
            {
                return Err(5);
            }
            for file in &self.files {
                if file.deleted {
                    continue;
                }
                if (file.name == source
                    || (file.name.as_bytes().starts_with(source.as_bytes())
                        && file.name.as_bytes().get(source.len) == Some(&b'\\')))
                    && name.len + file.name.len - source.len >= NAME_BYTES
                {
                    return Err(3);
                }
            }
            if (self.current == source
                || (self.current.as_bytes().starts_with(source.as_bytes())
                    && self.current.as_bytes().get(source.len) == Some(&b'\\')))
                && name.len + self.current.len - source.len > 63
            {
                return Err(3);
            }
            for file in &mut self.files {
                if file.deleted {
                    continue;
                }
                if file.name == source
                    || (file.name.as_bytes().starts_with(source.as_bytes())
                        && file.name.as_bytes().get(source.len) == Some(&b'\\'))
                {
                    let suffix = file.name.len - source.len;
                    file.name
                        .bytes
                        .copy_within(source.len..file.name.len, name.len);
                    file.name.bytes[..name.len].copy_from_slice(name.as_bytes());
                    file.name.len = name.len + suffix;
                }
            }
            if self.current == source
                || (self.current.as_bytes().starts_with(source.as_bytes())
                    && self.current.as_bytes().get(source.len) == Some(&b'\\'))
            {
                let suffix = self.current.len - source.len;
                self.current
                    .bytes
                    .copy_within(source.len..self.current.len, name.len);
                self.current.bytes[..name.len].copy_from_slice(name.as_bytes());
                self.current.len = name.len + suffix;
            }
        }
        self.namespace_dirty = true;
        Ok(())
    }
    pub fn make_directory(&mut self, path: &[u8]) -> Result<(), u16> {
        if self.overlay.is_none() {
            return Err(5);
        }
        let name = Name::relative(path, self.current)?;
        if name.len == 0 || self.name_attributes(name).is_ok() {
            return Err(5);
        }
        if let Some(separator) = name.as_bytes().iter().rposition(|b| *b == b'\\') {
            let mut parent = name;
            parent.len = separator;
            if self.name_attributes(parent) != Ok(0x10) {
                return Err(3);
            }
        }
        if self.files.len() >= self.new_file_limit {
            return Err(8);
        }
        self.files.push(File {
            name,
            base_name: Name::ROOT,
            deleted: false,
            directory: true,
            bytes: FileBytes::default(),
        });
        self.namespace_dirty = true;
        Ok(())
    }
    pub fn remove_directory(&mut self, path: &[u8]) -> Result<(), u16> {
        if self.overlay.is_none() {
            return Err(5);
        }
        let name = Name::relative(path, self.current)?;
        if name.len == 0 || name == self.current || self.name_attributes(name) != Ok(0x10) {
            return Err(5);
        }
        if self.files.iter().any(|file| {
            !file.deleted
                && file.name.as_bytes().starts_with(name.as_bytes())
                && file.name.as_bytes().get(name.len) == Some(&b'\\')
        }) {
            return Err(5);
        }
        let index = self.find(path)?;
        self.files[index].deleted = true;
        self.namespace_dirty = true;
        Ok(())
    }

    pub fn dirty(&self) -> bool {
        self.namespace_dirty
            || self
                .overlay
                .as_ref()
                .is_some_and(overlay::Overlay::any_dirty)
    }
    pub fn mark_dirty(&mut self) {
        self.namespace_dirty = true;
    }
    pub fn acknowledge_all(&mut self) {
        self.namespace_dirty = false;
        if let Some(overlay) = &mut self.overlay {
            overlay.acknowledge_all();
        }
    }
    pub fn snapshot_capacity(&self) -> usize {
        16 + self.files.capacity().max(self.files.len()) * (2 * NAME_BYTES + 8)
            + self
                .overlay
                .as_ref()
                .map_or(0, overlay::Overlay::snapshot_capacity)
    }
    /// Encode only names, metadata and changed bytes, never immutable game data.
    /// Output capacity must be reserved during session setup.
    pub fn encode_snapshot(&mut self, out: &mut Vec<u8>) -> Result<(), u16> {
        if out.capacity() < self.snapshot_capacity() {
            return Err(8);
        }
        out.clear();
        out.extend_from_slice(b"DOV1");
        out.extend_from_slice(&(self.files.len() as u32).to_le_bytes());
        for file in &self.files {
            out.push(file.base_name.len as u8);
            out.extend_from_slice(file.base_name.as_bytes());
            out.push(file.name.len as u8);
            out.extend_from_slice(file.name.as_bytes());
            out.push(u8::from(file.deleted) | (u8::from(file.directory) << 1));
        }
        let overlay = self.overlay.as_mut().ok_or(5u16)?;
        overlay.encode(out);
        Ok(())
    }
    /// Restore during setup, after mounting the identical immutable lower tree.
    /// Original path identities remap records independently of mount order.
    pub fn restore_snapshot(&mut self, mut input: &[u8]) -> Result<(), u16> {
        fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], u16> {
            let value = input.get(..count).ok_or(13u16)?;
            *input = &input[count..];
            Ok(value)
        }
        if self.handles.iter().any(Option::is_some) {
            return Err(5);
        }
        if take(&mut input, 4)? != b"DOV1" {
            return Err(13);
        }
        let count = u32::from_le_bytes(take(&mut input, 4)?.try_into().unwrap()) as usize;
        let lower_count = self
            .files
            .iter()
            .filter(|file| file.base_name.len != 0)
            .count();
        if count < lower_count {
            return Err(13);
        }
        if count > self.files.capacity().max(self.files.len()) || count - lower_count > 64 {
            return Err(8);
        }
        let mut entries: Vec<(Name, Name, bool, bool, usize)> = Vec::with_capacity(count);
        let mut mapping = Vec::with_capacity(count);
        let mut created = 0;
        for _ in 0..count {
            let length = take(&mut input, 1)?[0] as usize;
            let base = Name::parse(take(&mut input, length)?)?;
            let length = take(&mut input, 1)?[0] as usize;
            let name = Name::parse(take(&mut input, length)?)?;
            if name.len == 0 {
                return Err(13);
            }
            let flags = take(&mut input, 1)?[0];
            if flags & !3 != 0 {
                return Err(13);
            }
            let deleted = flags & 1 != 0;
            let directory = flags & 2 != 0;
            let index = if base.len == 0 {
                let index = lower_count + created;
                created += 1;
                index
            } else {
                if directory {
                    return Err(13);
                }
                self.files
                    .iter()
                    .filter(|file| file.base_name.len != 0)
                    .position(|file| file.base_name == base)
                    .ok_or(13u16)?
            };
            if mapping.contains(&index)
                || (!deleted
                    && entries
                        .iter()
                        .any(|(_, previous, hidden, _, _)| !hidden && *previous == name))
            {
                return Err(13);
            }
            mapping.push(index);
            entries.push((base, name, deleted, directory, index));
        }
        if count - created != lower_count {
            return Err(13);
        }
        let capacity = self.overlay.as_ref().ok_or(5u16)?.space().1;
        let mut overlay = overlay::Overlay::new(capacity);
        overlay.decode(&mut input, &mapping)?;
        if !input.is_empty() {
            return Err(13);
        }
        // Publish only after complete validation. Repeated import replaces the
        // previous upper namespace instead of retaining old created nodes.
        self.files.retain(|file| file.base_name.len != 0);
        for (base, name, deleted, directory, index) in entries {
            if base.len == 0 {
                debug_assert_eq!(self.files.len(), index);
                self.files.push(File {
                    name,
                    base_name: Name::ROOT,
                    deleted,
                    directory,
                    bytes: FileBytes::default(),
                });
            } else {
                self.files[index].name = name;
                self.files[index].deleted = deleted;
                self.files[index].directory = false;
            }
        }
        self.overlay = Some(overlay);
        self.namespace_dirty = false;
        Ok(())
    }
}
impl DirectoryTree {
    pub fn base_identity(&self) -> [u8; 32] {
        let mut lanes = [
            0xcbf29ce484222325u64,
            0x84222325cbf29ce4,
            0x9e3779b97f4a7c15,
            0xd6e8feb86659fd93,
        ];
        // Commutative per-file accumulation keeps native/ZIP mount order irrelevant.
        let mut totals = [0u64; 4];
        for file in &self.files {
            if file.base_name.len == 0 {
                continue;
            }
            let mut hashes = lanes;
            for byte in file
                .base_name
                .as_bytes()
                .iter()
                .chain(core::iter::once(&0))
                .chain(file.bytes.iter())
            {
                for (i, hash) in hashes.iter_mut().enumerate() {
                    *hash =
                        (*hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3u64 + (i as u64) * 2);
                }
            }
            for (i, hash) in hashes.into_iter().enumerate() {
                totals[i] = totals[i].wrapping_add(hash);
            }
        }
        lanes = totals;
        let mut result = [0; 32];
        for (i, lane) in lanes.into_iter().enumerate() {
            result[i * 8..i * 8 + 8].copy_from_slice(&lane.to_le_bytes());
        }
        result
    }
}
#[cfg(test)]
mod overlay_contract_tests {
    use super::*;
    fn view(drive: &mut DirectoryTree, handle: u16) -> Vec<u8> {
        drive.seek(handle, 0, 0).unwrap();
        let bytes = drive.read_view(handle, 65535).unwrap();
        (0..bytes.len()).map(|i| bytes.byte(i).unwrap()).collect()
    }
    #[test]
    fn chunk_upper_reuses_overwrites_and_truncation_never_resurrects_base() {
        let mut drive = DirectoryTree::new().with_write_overlay(8192);
        drive.mount(b"FILE", alloc::vec![b'x';6000]).unwrap();
        let handle = drive.open(b"FILE", 2).unwrap();
        for byte in 0..100 {
            drive.seek(handle, 0, 4094).unwrap();
            drive.write_buffer(handle, 4).unwrap().fill(byte);
        }
        assert_eq!(drive.disk_space().0, 0);
        let bytes = view(&mut drive, handle);
        assert_eq!(&bytes[4094..4098], &[99; 4]);
        assert_eq!(bytes[4093], b'x');
        assert_eq!(bytes[4098], b'x');
        drive.seek(handle, 0, 4095).unwrap();
        drive.write_buffer(handle, 0).unwrap();
        drive.seek(handle, 0, 6000).unwrap();
        drive.write_buffer(handle, 0).unwrap();
        let bytes = view(&mut drive, handle);
        assert_eq!(bytes[4094], 99);
        assert!(bytes[4095..].iter().all(|b| *b == 0));
    }
    #[test]
    fn snapshot_recovers_renames_deletions_newfiles_and_base_order() {
        let mut drive = DirectoryTree::new().with_write_overlay(16384);
        drive.mount(b"ORIGINAL", Vec::from(&b"abcdef"[..])).unwrap();
        drive.mount(b"DELETED", Vec::from(&b"gone"[..])).unwrap();
        let base = drive.base_identity();
        let handle = drive.open(b"ORIGINAL", 2).unwrap();
        let duplicate = drive.duplicate(handle).unwrap();
        drive.seek(handle, 0, 2).unwrap();
        drive
            .write_buffer(duplicate, 2)
            .unwrap()
            .copy_from_slice(b"XY");
        assert_eq!(drive.seek(handle, 1, 0), Ok(4));
        drive.close(handle).unwrap();
        drive.rename(b"ORIGINAL", b"RENAMED").unwrap();
        assert_eq!(view(&mut drive, duplicate), b"abXYef");
        drive.close(duplicate).unwrap();
        drive.delete(b"DELETED").unwrap();
        let created = drive.create(b"NEW", 0).unwrap();
        drive
            .write_buffer(created, 3)
            .unwrap()
            .copy_from_slice(b"new");
        drive.close(created).unwrap();
        let mut payload = Vec::with_capacity(drive.snapshot_capacity());
        drive.encode_snapshot(&mut payload).unwrap();
        let mut recovered = DirectoryTree::new().with_write_overlay(16384);
        recovered
            .mount(b"DELETED", Vec::from(&b"gone"[..]))
            .unwrap();
        recovered
            .mount(b"ORIGINAL", Vec::from(&b"abcdef"[..]))
            .unwrap();
        assert_eq!(recovered.base_identity(), base);
        recovered.restore_snapshot(&payload).unwrap();
        assert!(recovered.open(b"ORIGINAL", 0).is_err());
        assert!(recovered.open(b"DELETED", 0).is_err());
        let renamed = recovered.open(b"RENAMED", 0).unwrap();
        assert_eq!(view(&mut recovered, renamed), b"abXYef");
        let created = recovered.open(b"NEW", 0).unwrap();
        assert_eq!(view(&mut recovered, created), b"new");
        assert!(!recovered.dirty());
    }
}
impl DirectoryTree {
    pub fn prepare_read(&self, handle: u16, count: u16) -> Result<(u32, usize), u16> {
        let index = self.handles[Self::slot(handle)?].ok_or(6u16)?;
        let handle = self.descriptions[index].as_ref().ok_or(6u16)?;
        if handle.mode == 1 {
            return Err(5);
        }
        let original = self.files[handle.file].bytes.len() as u32;
        let length = self
            .overlay
            .as_ref()
            .map_or(original, |upper| upper.length(handle.file, original));
        Ok((
            handle.position,
            length.saturating_sub(handle.position).min(u32::from(count)) as usize,
        ))
    }
    pub fn read_at_into(&self, handle: u16, position: u32, output: &mut [u8]) -> Result<(), u16> {
        let index = self.handles[Self::slot(handle)?].ok_or(6u16)?;
        let handle = self.descriptions[index].as_ref().ok_or(6u16)?;
        if handle.mode == 1 {
            return Err(5);
        }
        let original = &self.files[handle.file].bytes;
        if let Some(upper) = &self.overlay {
            upper.read_into(handle.file, position, original, output);
        } else {
            output.fill(0);
            let count = output
                .len()
                .min(original.len().saturating_sub(position as usize));
            if count > 0 {
                output[..count]
                    .copy_from_slice(&original[position as usize..position as usize + count]);
            }
        }
        Ok(())
    }
    pub fn finish_read(&mut self, handle: u16, count: usize) -> Result<(), u16> {
        let index = self.handles[Self::slot(handle)?].ok_or(6u16)?;
        let handle = self.descriptions[index].as_mut().ok_or(6u16)?;
        handle.position = handle.position.checked_add(count as u32).ok_or(1u16)?;
        self.reads += 1;
        Ok(())
    }
}
impl DirectoryTree {
    pub fn write_prepared(&mut self, handle: u16, offset: usize, input: &[u8]) -> Result<(), u16> {
        let index = self.handles[Self::slot(handle)?].ok_or(6u16)?;
        let file = self.descriptions[index].as_ref().ok_or(6u16)?.file;
        self.overlay
            .as_mut()
            .ok_or(5u16)?
            .write_window(file, offset, input)
    }
}
#[cfg(test)]
mod directory_overlay_tests {
    use super::*;
    #[test]
    fn directory_rename_preserves_open_node_and_empty_upper_directories_search() {
        let mut drive = DirectoryTree::new().with_write_overlay(8192);
        drive
            .mount(b"OLD/SUB/FILE", Vec::from(&b"base"[..]))
            .unwrap();
        let handle = drive.open(b"OLD/SUB/FILE", 0).unwrap();
        drive.rename(b"OLD", b"NEW").unwrap();
        assert!(drive.open(b"OLD/SUB/FILE", 0).is_err());
        assert_eq!(drive.read(handle, 4), Ok(&b"base"[..]));
        assert_eq!(drive.attributes(b"NEW/SUB"), Ok(0x10));
        drive.make_directory(b"EMPTY").unwrap();
        let mut search = drive.start_search(b"EMPTY/*.*", 0x10).unwrap();
        assert!(drive.next_search(&mut search).is_none());
        drive.remove_directory(b"EMPTY").unwrap();
        assert!(drive.attributes(b"EMPTY").is_err());
        assert_eq!(drive.remove_directory(b"NEW"), Err(5));
    }
}
#[cfg(test)]
mod import_compatibility_tests {
    use super::*;
    #[test]
    fn compact_legacy_snapshot_can_be_imported_then_written_in_larger_upper() {
        let mut old = DirectoryTree::new().with_write_overlay(1024);
        let h = old.create(b"SAVE", 0).unwrap();
        old.write_buffer(h, 3).unwrap().copy_from_slice(b"old");
        old.close(h).unwrap();
        let mut payload = Vec::with_capacity(old.snapshot_capacity());
        old.encode_snapshot(&mut payload).unwrap();
        let mut new = DirectoryTree::new().with_write_overlay(8192);
        new.restore_snapshot(&payload).unwrap();
        let h = new.open(b"SAVE", 2).unwrap();
        new.write_buffer(h, 6).unwrap().copy_from_slice(b"newest");
        new.seek(h, 0, 0).unwrap();
        let view = new.read_view(h, 20).unwrap();
        assert_eq!(
            (0..view.len())
                .map(|i| view.byte(i).unwrap())
                .collect::<Vec<_>>(),
            b"newest"
        );
    }
}

#[cfg(feature = "save-state")]
#[path = "tree_saved.rs"]
mod saved_state;
