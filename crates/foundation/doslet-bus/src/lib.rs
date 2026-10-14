//! Bus contracts shared by processors and machine adapters.
#![no_std]

use core::fmt;
use core::sync::atomic::{AtomicU32, Ordering};

// Setup-only identities shared by all buses implementing the code contract.
// This counter contains no guest/device state and never wraps.
static NEXT_CODE_OWNER: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeOwnerId(u32);
impl CodeOwnerId {
    /// Obtain one identity for a memory owner's lifetime; exhaustion disables
    /// caching. Moving that owner retains its ID, replacing it needs a fresh ID.
    pub fn fresh() -> Option<Self> {
        NEXT_CODE_OWNER
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .ok()
            .map(Self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusError {
    UnmappedMemory(u32),
    UnmappedPort(u16),
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnmappedMemory(address) => write!(f, "unmapped memory at {address:#x}"),
            Self::UnmappedPort(port) => write!(f, "unmapped I/O port {port:#x}"),
        }
    }
}

impl core::error::Error for BusError {}

/// Identity and write generations for an immutable, side-effect-free code span.
/// Owners must be unique across bus lifetimes, including replacement buses.
/// Every write (including borrowed mutation) must change the relevant generation;
/// mapping changes must change `mapping`. Return None if a counter is exhausted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeStamp {
    pub owner: CodeOwnerId,
    pub mapping: u64,
    pub pages: [u64; 2],
}

/// Accesses preserve device side effects, including when a word spans regions.
pub trait MemoryBus {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError>;
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError>;

    /// Optional cache contract. Defaults keep other buses on the interpreter.
    fn code_stamp(&self, _address: u32, _length: u8) -> Option<CodeStamp> {
        None
    }
    /// Borrow up to `length` contiguous ordinary-memory code bytes. Inspecting
    /// them must have no device side effects. A short view is allowed.
    fn code_bytes(&self, _address: u32, _length: u8) -> Option<&[u8]> {
        None
    }

    fn read_u16(&mut self, address: u32) -> Result<u16, BusError> {
        let low = self.read_u8(address)?;
        let high = self.read_u8(address.wrapping_add(1))?;
        Ok(u16::from_le_bytes([low, high]))
    }

    fn write_u16(&mut self, address: u32, value: u16) -> Result<(), BusError> {
        let [low, high] = value.to_le_bytes();
        self.write_u8(address, low)?;
        self.write_u8(address.wrapping_add(1), high)
    }

    fn read_u32(&mut self, address: u32) -> Result<u32, BusError> {
        let mut bytes = [0; 4];
        for (offset, byte) in bytes.iter_mut().enumerate() {
            *byte = self.read_u8(address.wrapping_add(offset as u32))?;
        }
        Ok(u32::from_le_bytes(bytes))
    }

    fn write_u32(&mut self, address: u32, value: u32) -> Result<(), BusError> {
        for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
            self.write_u8(address.wrapping_add(offset as u32), byte)?;
        }
        Ok(())
    }
}

pub trait PortBus {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError>;
    fn write_port_u8(&mut self, port: u16, value: u8) -> Result<(), BusError>;
    /// Default PC-style splitting for byte-wide devices. Wider devices may
    /// override these methods to preserve their transaction semantics.
    fn read_port_u16(&mut self, port: u16) -> Result<u16, BusError> {
        Ok(u16::from_le_bytes([
            self.read_port_u8(port)?,
            self.read_port_u8(port.wrapping_add(1))?,
        ]))
    }
    fn write_port_u16(&mut self, port: u16, value: u16) -> Result<(), BusError> {
        let [low, high] = value.to_le_bytes();
        self.write_port_u8(port, low)?;
        self.write_port_u8(port.wrapping_add(1), high)
    }
    fn read_port_u32(&mut self, port: u16) -> Result<u32, BusError> {
        let low = self.read_port_u16(port)?;
        let high = self.read_port_u16(port.wrapping_add(2))?;
        Ok(u32::from(low) | (u32::from(high) << 16))
    }
    fn write_port_u32(&mut self, port: u16, value: u32) -> Result<(), BusError> {
        self.write_port_u16(port, value as u16)?;
        self.write_port_u16(port.wrapping_add(2), (value >> 16) as u16)
    }
}

pub trait CpuBus: MemoryBus + PortBus {}
impl<T: MemoryBus + PortBus> CpuBus for T {}
