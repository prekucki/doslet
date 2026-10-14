//! Platform-independent, bounded sessions. No filesystem, window, thread or host clock access.
#![no_std]
extern crate alloc;
mod bios;
pub mod demo;
pub mod session;
pub use session::{RuntimeError, Session, SessionStatus};
