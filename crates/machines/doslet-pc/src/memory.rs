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
/// Bytes an idle-loop proof may see written within one pass.
const WRITE_LOG: usize = 64;

/// The first previous value of each byte written while a proof observes RAM.
struct WriteLog {
    entries: [(u32, u8); WRITE_LOG],
    len: usize,
    overflow: bool,
}
impl WriteLog {
    fn record(&mut self, address: u32, old: u8) {
        if self.entries[..self.len]
            .iter()
            .any(|(logged, _)| *logged == address)
        {
            return;
        }
        if self.len == WRITE_LOG {
            self.overflow = true;
            return;
        }
        self.entries[self.len] = (address, old);
        self.len += 1;
    }
}

pub(crate) struct Ram {
    bytes: Box<[u8]>,
    generations: Box<[u64]>,
    identity: Option<CodeOwnerId>,
    /// Owner identity (high 32 bits) and mapping generation (low 32 bits),
    /// precomputed for cache hits; 0 when spans are unavailable.
    code_space: u64,
    /// Mutations of any page, for idle proofs to detect device and service writes.
    writes: u64,
    log: Option<WriteLog>,
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
            writes: 0,
            log: None,
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
        let byte = self
            .bytes
            .get_mut(address as usize)
            .ok_or(BusError::UnmappedMemory(address))?;
        if let Some(log) = &mut self.log {
            log.record(address, *byte);
        }
        *byte = value;
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
        if let Some(log) = &mut self.log {
            // The caller may change any byte of the borrowed range.
            if range.len() > WRITE_LOG {
                log.overflow = true;
            } else {
                for address in range.clone() {
                    log.record(address as u32, self.bytes[address]);
                }
            }
        }
        self.bytes.get_mut(range)
    }
    /// Native stores maintain generations themselves after checking exhaustion.
    /// Keep both mutable borrows disjoint and do not invalidate untouched pages.
    pub(crate) fn native_view_mut(
        &mut self,
        range: Range<usize>,
    ) -> Option<doslet_bus::NativeRamMut<'_>> {
        if self.code_space == 0 {
            return None;
        }
        if let Some(log) = &mut self.log {
            log.overflow = true;
        }
        self.writes = self.writes.wrapping_add(1);
        let linear_base = u32::try_from(range.start).ok()?;
        Some(doslet_bus::NativeRamMut {
            linear_base,
            bytes: self.bytes.get_mut(range)?,
            page_generations: &mut self.generations,
            excluded_start: 0,
            excluded_len: 0,
        })
    }
    /// Start recording the previous values of written bytes.
    pub(crate) fn begin_write_log(&mut self) {
        self.log = Some(WriteLog {
            entries: [(0, 0); WRITE_LOG],
            len: 0,
            overflow: false,
        });
    }
    /// Stop recording; true when every byte written since holds its old value.
    pub(crate) fn end_write_log(&mut self) -> bool {
        self.log.take().is_some_and(|log| {
            !log.overflow
                && log.entries[..log.len]
                    .iter()
                    .all(|(address, old)| self.bytes[*address as usize] == *old)
        })
    }
    pub(crate) fn writes(&self) -> u64 {
        self.writes
    }
    fn bump(&mut self, page: usize) {
        self.writes = self.writes.wrapping_add(1);
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
    fn native_borrow_invalidates_only_pages_explicitly_written() {
        let mut ram = Ram::default();
        let range = 3 * PAGE_SIZE - 2..3 * PAGE_SIZE + 2;
        let untouched = ram.code_check(0..4).unwrap();
        let before = ram.code_check(range.clone()).unwrap();
        {
            let view = ram.native_view_mut(PAGE_SIZE..RAM_SIZE).unwrap();
            assert_eq!(view.linear_base, PAGE_SIZE as u32);
            assert_eq!(view.page_generations.len(), RAM_SIZE / PAGE_SIZE);
            assert!(
                view.page_generations
                    .iter()
                    .all(|&generation| generation == 0)
            );
        }
        assert!(ram.code_valid(&before));
        {
            let view = ram.native_view_mut(PAGE_SIZE..RAM_SIZE).unwrap();
            let offset = range.start - view.linear_base as usize;
            // Native stores check both generations before changing any state.
            for &page in &before.pages {
                assert_ne!(view.page_generations[page as usize], u64::MAX);
            }
            view.bytes[offset..offset + 4].copy_from_slice(&0x1234_5678u32.to_le_bytes());
            for page in before.pages {
                view.page_generations[page as usize] += 1;
            }
        }
        assert_eq!(
            ram.view(range.clone()).unwrap(),
            &0x1234_5678u32.to_le_bytes()
        );
        assert!(!ram.code_valid(&before));
        assert!(ram.code_valid(&untouched));
        assert_eq!(ram.code_check(range).unwrap().generations, [1, 1]);
    }
    #[test]
    fn native_generation_exhaustion_can_fall_back_without_wrapping() {
        let mut ram = Ram::default();
        ram.generations[1] = u64::MAX;
        let before = ram.code_check(PAGE_SIZE..PAGE_SIZE + 4).unwrap();
        {
            let view = ram.native_view_mut(PAGE_SIZE..RAM_SIZE).unwrap();
            assert_eq!(view.page_generations[1], u64::MAX);
            // No native write is permitted: ordinary stepping owns exhaustion.
            assert_eq!(&view.bytes[..4], &[0; 4]);
        }
        assert!(ram.code_valid(&before));
        ram.write(PAGE_SIZE as u32, 42).unwrap();
        assert!(!ram.code_valid(&before));
        assert!(ram.native_view_mut(PAGE_SIZE..RAM_SIZE).is_none());
        assert_eq!(ram.generations[1], u64::MAX);
    }
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
