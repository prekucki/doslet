//! Platform hosts for the pinned v86 CPU and its generated WASM modules.
pub mod devices;
#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::*;
#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::*;
