//! Platform-independent, bounded sessions. No filesystem, window, thread or host clock access.
#![no_std]
extern crate alloc;
mod bios;
pub mod demo;
pub mod session;
pub use session::{IdleLoopStats, RuntimeError, Session, SessionStatus};

#[cfg(all(
    feature = "dynamic-core",
    not(target_arch = "wasm32"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use doslet_x86::DynamicCompilation;
