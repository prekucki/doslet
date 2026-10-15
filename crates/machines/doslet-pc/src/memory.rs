use alloc::{boxed::Box, vec};
use core::ops::Range;
use doslet_bus::{BusError, CodeCheck, CodeOwnerId};

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
    /// Owner identity (high 32 bits) and mapping generation (low 32 bits),
    /// precomputed for cache hits; 0 when spans are unavailable.
    code_space: u64,
}
impl Default for Ram {
    fn default() -> Self {
        Self::new(RAM_SIZE)
    }
}
impl Ram {
    pub(crate) fn new(size: usize) -> Self {
        let mut ram = Self {
            bytes: vec![0; size].into_boxed_slice(),
            generations: vec![0; size / PAGE_SIZE].into_boxed_slice(),
            identity: CodeOwnerId::fresh(),
            code_space: 0,
        };
        ram.set_mapping(0);
        ram
    }
    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }
    pub(crate) fn text_revision(&self) -> u64 {
        self.generations[0].wrapping_add(
            self.generations[0xB8..0xC0]
                .iter()
                .fold(0u64, |sum, value| sum.wrapping_add(*value)),
        )
    }
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
            self.code_space = 0;
        }
    }
    /// Record the machine's address-mapping generation. Generations beyond
    /// 32 bits make code spans unavailable rather than wrapping.
    pub(crate) fn set_mapping(&mut self, mapping: u64) {
        self.code_space = match (self.identity, u32::try_from(mapping)) {
            (Some(owner), Ok(mapping)) if mapping != u32::MAX => {
                u64::from(owner.get()) << 32 | u64::from(mapping)
            }
            _ => 0,
        };
    }
    pub(crate) fn code_check(&self, range: Range<usize>) -> Option<CodeCheck> {
        if range.is_empty() || range.end > self.bytes.len() || self.code_space == 0 {
            return None;
        }
        let pages = [range.start / PAGE_SIZE, (range.end - 1) / PAGE_SIZE];
        Some(CodeCheck {
            space: self.code_space,
            pages: pages.map(|page| page as u32),
            generations: pages.map(|page| self.generations[page]),
        })
    }
    #[inline]
    pub(crate) fn code_valid(&self, check: &CodeCheck) -> bool {
        let current = |page: u32| self.generations.get(page as usize).copied();
        check.space == self.code_space
            && current(check.pages[0]) == Some(check.generations[0])
            && current(check.pages[1]) == Some(check.generations[1])
    }
    /// The former per-hit stamp, kept as the reference for code-check tests.
    #[cfg(test)]
    pub(crate) fn stamp_reference(
        &self,
        range: Range<usize>,
        mapping: u64,
    ) -> Option<(CodeOwnerId, u64, [u64; 2])> {
        if range.is_empty() || range.end > self.bytes.len() || mapping == u64::MAX {
            return None;
        }
        Some((
            self.identity?,
            mapping,
            [
                self.generations[range.start / PAGE_SIZE],
                self.generations[(range.end - 1) / PAGE_SIZE],
            ],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn borrowed_mutation_bumps_both_pages_and_exhaustion_disables_checks() {
        let mut ram = Ram::default();
        let range = PAGE_SIZE - 2..PAGE_SIZE + 2;
        let before = ram.code_check(range.clone()).unwrap();
        assert_eq!(before.pages, [0, 1]);
        ram.view_mut(range.clone()).unwrap().fill(42);
        let after = ram.code_check(range.clone()).unwrap();
        assert_eq!(
            after.generations,
            [before.generations[0] + 1, before.generations[1] + 1]
        );
        assert!(!ram.code_valid(&before) && ram.code_valid(&after));
        ram.generations[0] = u64::MAX;
        ram.write(0, 1).unwrap();
        assert_eq!(ram.code_check(range), None);
        assert!(!ram.code_valid(&after));
        assert_eq!(ram.read(0), Ok(1));
    }
    #[test]
    fn mapping_generations_change_the_space_and_exhaust_without_wrapping() {
        let mut ram = Ram::default();
        let range = 0..4;
        let first = ram.code_check(range.clone()).unwrap();
        ram.set_mapping(1);
        assert!(!ram.code_valid(&first));
        let second = ram.code_check(range.clone()).unwrap();
        assert_eq!(second.space & 0xFFFF_FFFF, 1);
        assert_eq!(second.space >> 32, first.space >> 32);
        ram.set_mapping(u64::from(u32::MAX) - 1);
        assert!(ram.code_check(range.clone()).is_some());
        for mapping in [u64::from(u32::MAX), u64::MAX] {
            ram.set_mapping(mapping);
            assert_eq!(ram.code_check(range.clone()), None);
            assert!(!ram.code_valid(&second));
        }
        // Foreign or corrupt page indices are rejected rather than indexed.
        let mut check = first;
        check.pages[1] = u32::MAX;
        ram.set_mapping(0);
        assert!(ram.code_valid(&first) && !ram.code_valid(&check));
    }
}
