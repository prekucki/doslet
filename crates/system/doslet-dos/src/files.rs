//! Compatibility facade over the VFS directory tree and reserved sparse upper.
//! DOS services retain their established handle and error-code ABI.
pub use doslet_vfs::tree::search;
pub use doslet_vfs::tree::{DirectoryTree as ReadOnlyDrive, HANDLE_COUNT, ReadView, VisibleFile};
