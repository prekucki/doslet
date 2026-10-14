//! Borrowed DOS MZ / LE / LX header inspection; no loading or execution of LE/LX images.
#![no_std]
mod mz;
pub use mz::{ExecutableError, ExtendedHeader, MzHeader, MzImage, Relocation};
