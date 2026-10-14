use alloc::{boxed::Box, vec};
use core::ops::Range;
use doslet_bus::BusError;

pub const RAM_SIZE: usize = 16 * 1024 * 1024;

/// Machine wiring: the A20 gate masks bit 20, rather than truncating every address.
#[derive(Default)]
pub(crate) struct A20Gate {
    pub enabled: bool,
}
impl A20Gate {
    pub fn address(&self, address: u32) -> u32 {
        if self.enabled {
            address
        } else {
            address & !(1 << 20)
        }
    }
}
pub(crate) struct Ram {
    bytes: Box<[u8]>,
}
impl Default for Ram {
    fn default() -> Self {
        Self {
            bytes: vec![0; RAM_SIZE].into_boxed_slice(),
        }
    }
}
impl Ram {
    pub(crate) fn read(&self, address: u32) -> Result<u8, BusError> {
        self.bytes
            .get(address as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(address))
    }
    pub(crate) fn write(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        *self
            .bytes
            .get_mut(address as usize)
            .ok_or(BusError::UnmappedMemory(address))? = value;
        Ok(())
    }
    pub(crate) fn view(&self, range: Range<usize>) -> Option<&[u8]> {
        self.bytes.get(range)
    }
}
