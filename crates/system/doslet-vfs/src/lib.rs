//! Guest filesystem contracts, stable nodes, central handles and sparse uppers.
#![no_std]
extern crate alloc;
use doslet_storage::StorageError;
pub const HANDLE_COUNT: usize = 96;
/// Nested DOS EXEC levels whose parents keep their handles.
pub const MAX_PROCESS_DEPTH: usize = 4;
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
/// Each process has its own handles; a child's start as its parent's
/// inheritable ones, and the parents' wait on a stack while it runs.
pub struct HandleTable {
    handles: [Option<u8>; HANDLE_COUNT],
    descriptions: [Option<OpenFile>; HANDLE_COUNT],
    parents: [[Option<u8>; HANDLE_COUNT]; MAX_PROCESS_DEPTH],
    depth: usize,
}
impl Default for HandleTable {
    fn default() -> Self {
        Self {
            handles: [None; HANDLE_COUNT],
            descriptions: [None; HANDLE_COUNT],
            parents: [[None; HANDLE_COUNT]; MAX_PROCESS_DEPTH],
            depth: 0,
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
    /// Handles of every process sharing one open description.
    pub fn references(&self, index: usize) -> u16 {
        core::iter::once(&self.handles)
            .chain(&self.parents[..self.depth])
            .flatten()
            .filter(|desc| desc.map(usize::from) == Some(index))
            .count() as u16
    }
    fn referenced(&self, desc: u8) -> bool {
        core::iter::once(&self.handles)
            .chain(&self.parents[..self.depth])
            .any(|handles| handles.contains(&Some(desc)))
    }
    pub fn close(&mut self, handle: u16) -> Result<(), u16> {
        let slot = handle.checked_sub(5).ok_or(6u16)? as usize;
        let desc = self.handles.get_mut(slot).ok_or(6u16)?.take().ok_or(6u16)?;
        if !self.referenced(desc) {
            self.descriptions[desc as usize] = None;
        }
        Ok(())
    }
    /// Processes waiting for a child, outermost first.
    pub fn depth(&self) -> usize {
        self.depth
    }
    /// EXEC: the parent's handles wait, and the child gets the inheritable
    /// ones; open mode bit 7 marks the others. Error 8 past the depth limit.
    pub fn begin_child(&mut self) -> Result<(), u16> {
        if self.depth == MAX_PROCESS_DEPTH {
            return Err(8);
        }
        self.parents[self.depth] = self.handles;
        self.depth += 1;
        for slot in &mut self.handles {
            if slot.is_some_and(|desc| {
                self.descriptions[usize::from(desc)].is_some_and(|file| file.mode & 0x80 != 0)
            }) {
                *slot = None;
            }
        }
        Ok(())
    }
    /// The child terminated: its handles close and the parent's return.
    /// `closed` receives each open description no process uses any more.
    pub fn end_child(&mut self, mut closed: impl FnMut(OpenFile)) {
        if self.depth == 0 {
            return;
        }
        let child = self.handles;
        self.depth -= 1;
        self.handles = self.parents[self.depth];
        self.parents[self.depth] = [None; HANDLE_COUNT];
        for desc in child.into_iter().flatten() {
            if !self.referenced(desc)
                && let Some(file) = self.descriptions[usize::from(desc)].take()
            {
                closed(file);
            }
        }
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
        self.parents = [[None; HANDLE_COUNT]; MAX_PROCESS_DEPTH];
        self.depth = 0;
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

    #[test]
    fn children_inherit_handles_and_close_only_their_own() {
        let file = |node, mode| OpenFile {
            mount: MountId(0),
            node: NodeId(node),
            position: 0,
            mode,
        };
        let mut handles = HandleTable::default();
        let shared = handles.open(file(1, 0)).unwrap();
        let private = handles.open(file(2, 0x80)).unwrap();
        let index = handles.description_index(shared).unwrap();
        handles.begin_child().unwrap();
        assert_eq!(handles.depth(), 1);
        assert_eq!(handles.get(private), Err(6));
        assert_eq!(handles.references(index), 2);
        // The child's own file takes the free slot; closing the inherited
        // handle leaves the parent's open.
        assert_eq!(handles.open(file(3, 0)).unwrap(), private);
        handles.close(shared).unwrap();
        assert_eq!(handles.references(index), 1);
        assert!(handles.description(index).is_some());
        let mut closed = alloc::vec::Vec::new();
        handles.end_child(|file| closed.push(file.node));
        assert_eq!(closed, [NodeId(3)]);
        assert_eq!(handles.depth(), 0);
        assert_eq!(handles.get(shared).unwrap().node, NodeId(1));
        assert_eq!(handles.get(private).unwrap().node, NodeId(2));
        for _ in 0..MAX_PROCESS_DEPTH {
            handles.begin_child().unwrap();
        }
        assert_eq!(handles.begin_child(), Err(8));
    }

    #[cfg(feature = "save-state")]
    #[test]
    fn waiting_parents_survive_portable_states() {
        use doslet_state::State;
        let mut handles = HandleTable::default();
        let parent = handles
            .open(OpenFile {
                mount: MountId(0),
                node: NodeId(7),
                position: 3,
                mode: 0,
            })
            .unwrap();
        handles.begin_child().unwrap();
        handles.close(parent).unwrap();
        let mut restored = HandleTable::load(handles.save()).unwrap();
        assert_eq!(restored.depth(), 1);
        assert_eq!(restored.get(parent), Err(6));
        restored.end_child(|_| panic!("the parent still uses its file"));
        assert_eq!(restored.get(parent).unwrap().position, 3);
    }
}

mod sparse;
pub use sparse::OverlayFs;

pub mod tree;

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
