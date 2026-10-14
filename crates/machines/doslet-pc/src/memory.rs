use alloc::{boxed::Box, vec};
use core::ops::Range;
use doslet_bus::{BusError, CodeOwnerId, CodeStamp};

pub const RAM_SIZE: usize = 16 * 1024 * 1024;
const PAGE_SIZE: usize = 4096;

/// Machine wiring: the A20 gate masks bit 20, rather than truncating every address.
#[derive(Default)]
pub(crate) struct A20Gate {
    pub enabled: bool,
    pub generation: u64,
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
    generations: Box<[u64]>,
    identity: Option<CodeOwnerId>,
}
impl Default for Ram {
    fn default() -> Self {
        Self {
            bytes: vec![0; RAM_SIZE].into_boxed_slice(),
            generations: vec![0; RAM_SIZE / PAGE_SIZE].into_boxed_slice(),
            identity: CodeOwnerId::fresh(),
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
        self.bump(address as usize / PAGE_SIZE);
        Ok(())
    }
    pub(crate) fn view(&self, range: Range<usize>) -> Option<&[u8]> {
        self.bytes.get(range)
    }
    pub(crate) fn view_mut(&mut self, range: Range<usize>) -> Option<&mut [u8]> {
        if range.start > range.end || range.end > self.bytes.len() {
            return None;
        }
        if !range.is_empty() {
            for page in range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE {
                self.bump(page);
            }
        }
        self.bytes.get_mut(range)
    }
    fn bump(&mut self, page: usize) {
        if let Some(next) = self.generations[page].checked_add(1) {
            self.generations[page] = next;
        } else {
            self.identity = None;
        }
    }
    pub(crate) fn code_stamp(&self, range: Range<usize>, mapping: u64) -> Option<CodeStamp> {
        if range.is_empty() || range.end > self.bytes.len() || mapping == u64::MAX {
            return None;
        }
        Some(CodeStamp {
            owner: self.identity?,
            mapping,
            pages: [
                self.generations[range.start / PAGE_SIZE],
                self.generations[(range.end - 1) / PAGE_SIZE],
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn borrowed_mutation_bumps_both_pages_and_exhaustion_disables_stamps() {
        let mut ram = Ram::default();
        let range = PAGE_SIZE - 2..PAGE_SIZE + 2;
        let before = ram.code_stamp(range.clone(), 0).unwrap();
        ram.view_mut(range.clone()).unwrap().fill(42);
        let after = ram.code_stamp(range.clone(), 0).unwrap();
        assert_eq!(after.pages, [before.pages[0] + 1, before.pages[1] + 1]);
        ram.generations[0] = u64::MAX;
        ram.write(0, 1).unwrap();
        assert_eq!(ram.code_stamp(range, 0), None);
        assert_eq!(ram.read(0), Ok(1));
    }
}
