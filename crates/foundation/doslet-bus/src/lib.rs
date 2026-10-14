//! Bus contracts shared by processors and machine adapters.
#![no_std]

use core::fmt;

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

/// Accesses preserve device side effects, including when a word spans regions.
pub trait MemoryBus {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError>;
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError>;

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
