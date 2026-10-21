//! Guest filesystem contracts, stable nodes, central handles and sparse uppers.
#![no_std]
extern crate alloc;
use doslet_storage::StorageError;
pub const HANDLE_COUNT: usize = 96;
pub const NAME_BYTES: usize = 128;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MountId(pub u8);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeId(pub u64);
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metadata {
    pub node: NodeId,
    pub length: u64,
    pub attributes: u8,
    pub directory: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Name {
    bytes: [u8; NAME_BYTES],
    len: u8,
}
impl Name {
    pub const ROOT: Self = Self {
        bytes: [0; NAME_BYTES],
        len: 0,
    };
    pub fn parse(path: &[u8]) -> Result<Self, StorageError> {
        if !path.is_ascii() || path.contains(&0) {
            return Err(StorageError::Corrupt);
        }
        let mut result = Self::ROOT;
        for part in path
            .split(|b| matches!(b, b'/' | b'\\'))
            .filter(|p| !p.is_empty() && *p != b".")
        {
            if part == b".." {
                result.len = result
                    .as_bytes()
                    .iter()
                    .rposition(|b| *b == b'\\')
                    .unwrap_or(0) as u8;
                continue;
            }
            if part.iter().any(|b| b":*?\"<>|".contains(b)) {
                return Err(StorageError::Corrupt);
            }
            let separator = usize::from(result.len != 0);
            let start = result.len as usize;
            if start + separator + part.len() >= NAME_BYTES {
                return Err(StorageError::Quota);
            }
            if separator != 0 {
                result.bytes[start] = b'\\';
            }
            for (i, b) in part.iter().enumerate() {
                result.bytes[start + separator + i] = b.to_ascii_uppercase();
            }
            result.len = (start + separator + part.len()) as u8;
        }
        Ok(result)
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}
pub trait FileSystem {
    fn lookup(&mut self, path: &[u8]) -> Result<Metadata, StorageError>;
    fn read_at(
        &mut self,
        node: NodeId,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, StorageError>;
    fn directory_entry(
        &mut self,
        parent: NodeId,
        cursor: u32,
    ) -> Result<Option<(Name, Metadata)>, StorageError>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpenFile {
    pub mount: MountId,
    pub node: NodeId,
    pub position: u64,
    pub mode: u8,
}
/// DOS handle aliases point to one open description, including its shared offset.
pub struct HandleTable {
    handles: [Option<u8>; HANDLE_COUNT],
    descriptions: [Option<OpenFile>; HANDLE_COUNT],
}
impl Default for HandleTable {
    fn default() -> Self {
        Self {
            handles: [None; HANDLE_COUNT],
            descriptions: [None; HANDLE_COUNT],
        }
    }
}
impl HandleTable {
    pub fn has_capacity(&self) -> bool {
        self.handles.iter().any(Option::is_none) && self.descriptions.iter().any(Option::is_none)
    }
    pub fn open(&mut self, file: OpenFile) -> Result<u16, u16> {
        let slot = self.handles.iter().position(Option::is_none).ok_or(4u16)?;
        let desc = self
            .descriptions
            .iter()
            .position(Option::is_none)
            .ok_or(4u16)?;
        self.descriptions[desc] = Some(file);
        self.handles[slot] = Some(desc as u8);
        Ok(slot as u16 + 5)
    }
    pub fn get(&self, handle: u16) -> Result<&OpenFile, u16> {
        let index = handle.checked_sub(5).ok_or(6u16)? as usize;
        let desc = self.handles.get(index).copied().flatten().ok_or(6u16)?;
        self.descriptions[desc as usize].as_ref().ok_or(6)
    }
    pub fn get_mut(&mut self, handle: u16) -> Result<&mut OpenFile, u16> {
        let index = handle.checked_sub(5).ok_or(6u16)? as usize;
        let desc = self.handles.get(index).copied().flatten().ok_or(6u16)?;
        self.descriptions[desc as usize].as_mut().ok_or(6)
    }
    /// The open description behind a handle; DOS numbers these in its SFT.
    pub fn description_index(&self, handle: u16) -> Option<usize> {
        let index = usize::from(handle.checked_sub(5)?);
        self.handles.get(index).copied().flatten().map(usize::from)
    }
    pub fn description(&self, index: usize) -> Option<&OpenFile> {
        self.descriptions.get(index)?.as_ref()
    }
    /// Handles sharing one open description.
    pub fn references(&self, index: usize) -> u16 {
        self.handles
            .iter()
            .filter(|desc| desc.map(usize::from) == Some(index))
            .count() as u16
    }
    pub fn close(&mut self, handle: u16) -> Result<(), u16> {
        let slot = handle.checked_sub(5).ok_or(6u16)? as usize;
        let desc = self.handles.get_mut(slot).ok_or(6u16)?.take().ok_or(6u16)?;
        if !self.handles.contains(&Some(desc)) {
            self.descriptions[desc as usize] = None;
        }
        Ok(())
    }
    pub fn duplicate(&mut self, handle: u16) -> Result<u16, u16> {
        self.get(handle)?;
        let desc = self.handles[(handle - 5) as usize];
        let slot = self.handles.iter().position(Option::is_none).ok_or(4u16)?;
        self.handles[slot] = desc;
        Ok(slot as u16 + 5)
    }
    pub fn force_duplicate(&mut self, source: u16, target: u16) -> Result<(), u16> {
        self.get(source)?;
        let index = target.checked_sub(5).ok_or(6u16)? as usize;
        if index >= HANDLE_COUNT {
            return Err(6);
        }
        if source == target {
            return Ok(());
        }
        let desc = self.handles[(source - 5) as usize];
        if self.handles[index].is_some() {
            self.close(target)?;
        }
        self.handles[index] = desc;
        Ok(())
    }
    pub fn clear(&mut self) {
        self.handles.fill(None);
        self.descriptions.fill(None);
    }
    pub fn any_open(&self, mount: MountId, node: NodeId) -> bool {
        self.descriptions
            .iter()
            .flatten()
            .any(|d| d.mount == mount && d.node == node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicates_share_position_and_forced_close_is_safe() {
        let mut handles = HandleTable::default();
        let h = handles
            .open(OpenFile {
                mount: MountId(0),
                node: NodeId(1),
                position: 0,
                mode: 2,
            })
            .unwrap();
        let d = handles.duplicate(h).unwrap();
        let index = handles.description_index(h).unwrap();
        assert_eq!(handles.description_index(d), Some(index));
        assert_eq!(handles.references(index), 2);
        handles.get_mut(h).unwrap().position = 42;
        assert_eq!(handles.get(d).unwrap().position, 42);
        handles.close(h).unwrap();
        assert_eq!(handles.get(d).unwrap().position, 42);
        assert_eq!(handles.references(index), 1);
        handles.force_duplicate(d, d).unwrap();
        handles.close(d).unwrap();
        assert_eq!(handles.get(d), Err(6));
        assert_eq!(handles.description(index), None);
        assert_eq!(handles.description_index(d), None);
    }
}

mod sparse;
pub use sparse::OverlayFs;

pub mod tree;

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
