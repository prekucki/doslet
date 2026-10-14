//! Minimal COM and relocated real-mode MZ process setup. File services and DPMI are pending.
#![no_std]
extern crate alloc;
pub mod fcb;
pub mod files;
pub mod loader;
pub mod memory;
pub mod mouse;
pub mod mz;
pub use loader::{ComError, load_com};
pub use mz::{MzError, ProcessMemory, load_mz, load_mz_stub, load_overlay};
