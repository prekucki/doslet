//! Portable, setup-only component snapshots. No host paths or execution hooks.
#![no_std]
extern crate alloc;

pub use prost;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidState(pub &'static str);

impl core::fmt::Display for InvalidState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "invalid saved state: {}", self.0)
    }
}
impl core::error::Error for InvalidState {}

/// Snapshots own their buffers and allocate only during explicit save/load.
/// Wire types use stable Protobuf field numbers, never Rust memory layouts.
pub trait State: Sized {
    type Wire: prost::Message + Default;
    fn save(&self) -> Self::Wire;
    fn load(state: Self::Wire) -> Result<Self, InvalidState>;
}

pub fn required<T>(value: Option<T>, name: &'static str) -> Result<T, InvalidState> {
    value.ok_or(InvalidState(name))
}

pub fn narrow<T, U: TryInto<T>>(value: U, name: &'static str) -> Result<T, InvalidState> {
    value.try_into().map_err(|_| InvalidState(name))
}

/// Logical external asset reference. Paths are DOS paths, never host filenames.
/// The length/checksum pair detects accidental media mismatch, not hostile edits.
#[derive(Clone, PartialEq, prost::Message)]
pub struct ResourceReference {
    #[prost(enumeration = "ResourceKind", tag = "1")]
    pub kind: i32,
    #[prost(uint32, tag = "2")]
    pub drive: u32,
    #[prost(bytes = "vec", tag = "3")]
    pub path: alloc::vec::Vec<u8>,
    #[prost(uint64, tag = "4")]
    pub length: u64,
    #[prost(fixed64, tag = "5")]
    pub checksum: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum ResourceKind {
    Unspecified = 0,
    DirectoryFile = 1,
    Iso9660 = 2,
}
