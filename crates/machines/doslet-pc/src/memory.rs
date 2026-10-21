use core::ops::Range;
use doslet_bus::{BusError, CodeCheck, CodeOwnerId};
pub(crate) mod batch;
mod journal;
pub(crate) mod storage;

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
    pub(crate) storage: storage::Storage,
    identity: Option<CodeOwnerId>,
    /// Owner identity (high 32 bits) and mapping generation (low 32 bits),
    /// precomputed for cache hits; 0 when spans are unavailable.
    code_space: u64,
    /// Mutations of any page, for idle proofs to detect device and service writes.
    writes: u64,
    log: Option<WriteLog>,
    journal: journal::Journal,
}
impl Default for Ram {
    fn default() -> Self {
        Self::new(RAM_SIZE)
    }
}
impl Ram {
    /// Resolve the owner's slices once for an ordinary decoded RAM-only batch.
    /// Logging paths retain the full PC bus so every write remains observable.
    pub(crate) fn interpreter_batch_bus(&mut self) -> Option<batch::BorrowedRamBus<'_>> {
        if !self.direct_execution_ready() {
            return None;
        }
        let (bytes, generations) = self.storage.parts_mut();
        Some(batch::BorrowedRamBus::new(
            bytes,
            generations,
            self.code_space,
        ))
    }

    /// VGA controls and its physical backing stay borrowed for the same slice.
    #[cfg(feature = "wasm-procedures")]
    pub(crate) fn interpreter_batch_bus_with_video<'a>(
        &'a mut self,
        vga: &'a mut doslet_vga::Vga,
    ) -> Option<batch::BorrowedRamBus<'a>> {
        if !self.direct_execution_ready() {
            return None;
        }
        let (bytes, generations, video) = self.storage.parts_with_video_mut()?;
        Some(batch::BorrowedRamBus::with_video(
            bytes,
            generations,
            video,
            vga,
            self.code_space,
        ))
    }

    pub(crate) fn complete_interpreter_batch(&mut self, effects: batch::Effects) {
        self.complete_direct_writes(effects.page_writes);
        if effects.code_exhausted {
            self.identity = None;
            self.code_space = 0;
        }
    }

    pub(crate) fn direct_execution_ready(&self) -> bool {
        self.code_space != 0 && self.log.is_none() && !self.journal.recording()
    }

    /// Setup-only backing migration. Preserve RAM accounting and replace its
    /// code identity so no decoded/native proof can retain the old owner.
    #[cfg(feature = "wasm-procedures")]
    pub(crate) fn replace_storage(&mut self, storage: storage::Storage) {
        let mapping = self.code_space & u64::from(u32::MAX);
        self.storage = storage;
        self.identity = CodeOwnerId::fresh();
        self.set_mapping(mapping);
    }
    /// Exclusive foreign execution maintains actual generations inline and
    /// reconciles its write count at the boundary. Recording cannot be active.
    pub(crate) fn direct_view_mut(
        &mut self,
        range: Range<usize>,
    ) -> Option<doslet_bus::NativeRamMut<'_>> {
        if self.code_space == 0 || self.log.is_some() || self.journal.recording() {
            return None;
        }
        let linear_base = u32::try_from(range.start).ok()?;
        let (bytes, page_generations) = self.storage.parts_mut();
        Some(doslet_bus::NativeRamMut {
            linear_base,
            bytes: bytes.get_mut(range)?,
            page_generations,
            excluded_start: 0,
            excluded_len: 0,
        })
    }

    pub(crate) fn complete_direct_writes(&mut self, count: u64) {
        self.writes = self.writes.wrapping_add(count);
    }

    pub(crate) fn new(size: usize) -> Self {
        Self::from_storage(storage::Storage::new(size))
    }
    fn from_storage(storage: storage::Storage) -> Self {
        let mut ram = Self {
            storage,
            identity: CodeOwnerId::fresh(),
            code_space: 0,
            writes: 0,
            log: None,
            journal: journal::Journal::default(),
        };
        ram.set_mapping(0);
        ram
    }
    pub(crate) fn len(&self) -> usize {
        self.storage.len()
    }
    pub(crate) fn text_revision(&self) -> u64 {
        let generations = self.storage.generations();
        generations[0].wrapping_add(
            generations[0xB8..0xC0]
                .iter()
                .fold(0u64, |sum, value| sum.wrapping_add(*value)),
        )
    }
    pub(crate) fn read(&self, address: u32) -> Result<u8, BusError> {
        self.storage
            .bytes()
            .get(address as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(address))
    }
    pub(crate) fn write(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        let (bytes, generations) = self.storage.parts_mut();
        let byte = bytes
            .get_mut(address as usize)
            .ok_or(BusError::UnmappedMemory(address))?;
        if let Some(log) = &mut self.log {
            log.record(address, *byte);
        }
        self.journal.record(address, *byte);
        *byte = value;
        self.writes = self.writes.wrapping_add(1);
        let generation = &mut generations[address as usize / PAGE_SIZE];
        if let Some(next) = generation.checked_add(1) {
            *generation = next;
        } else {
            self.identity = None;
            self.code_space = 0;
        }
        Ok(())
    }
    pub(crate) fn view(&self, range: Range<usize>) -> Option<&[u8]> {
        self.storage.bytes().get(range)
    }
    pub(crate) fn view_mut(&mut self, range: Range<usize>) -> Option<&mut [u8]> {
        // Resolve the owner once. Bounds, old-byte recording and generation
        // updates all use these disjoint slices, including the returned view.
        let (bytes, generations) = self.storage.parts_mut();
        let view = bytes.get_mut(range.clone())?;
        if !range.is_empty() {
            let pages = range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE;
            for generation in &mut generations[pages] {
                self.writes = self.writes.wrapping_add(1);
                if let Some(next) = generation.checked_add(1) {
                    *generation = next;
                } else {
                    self.identity = None;
                    self.code_space = 0;
                }
            }
        }
        if let Some(log) = &mut self.log {
            // The caller may change any byte of the borrowed range.
            if range.len() > WRITE_LOG {
                log.overflow = true;
            } else {
                for (offset, &old) in view.iter().enumerate() {
                    log.record((range.start + offset) as u32, old);
                }
            }
        }
        self.journal.record_range(range.start, view);
        Some(view)
    }
    pub(crate) fn recurrence_recording(&self) -> bool {
        self.journal.recording()
    }
    /// Native stores maintain generations themselves after checking exhaustion.
    /// Keep both mutable borrows disjoint and do not invalidate untouched pages.
    pub(crate) fn native_view_mut(
        &mut self,
        range: Range<usize>,
    ) -> Option<doslet_bus::NativeRamMut<'_>> {
        if self.code_space == 0 || self.journal.recording() {
            return None;
        }
        if let Some(log) = &mut self.log {
            log.overflow = true;
        }
        self.writes = self.writes.wrapping_add(1);
        let linear_base = u32::try_from(range.start).ok()?;
        let (bytes, page_generations) = self.storage.parts_mut();
        Some(doslet_bus::NativeRamMut {
            linear_base,
            bytes: bytes.get_mut(range)?,
            page_generations,
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
                    .all(|(address, old)| self.storage.bytes()[*address as usize] == *old)
        })
    }
    pub(crate) fn writes(&self) -> u64 {
        self.writes
    }
    pub(crate) fn begin_recurrence(&mut self) -> u64 {
        self.journal.begin();
        self.code_space
    }
    pub(crate) fn end_recurrence(&mut self, space: u64) -> bool {
        self.journal.end(self.storage.bytes()) && space != 0 && space == self.code_space
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
        if range.is_empty() || range.end > self.storage.len() || self.code_space == 0 {
            return None;
        }
        let pages = [range.start / PAGE_SIZE, (range.end - 1) / PAGE_SIZE];
        let generations = self.storage.generations();
        Some(CodeCheck {
            space: self.code_space,
            pages: pages.map(|page| page as u32),
            generations: pages.map(|page| generations[page]),
        })
    }
    #[inline]
    pub(crate) fn code_valid(&self, check: &CodeCheck) -> bool {
        if check.space != self.code_space {
            return false;
        }
        let generations = self.storage.generations();
        let current = |page: u32| generations.get(page as usize).copied();
        current(check.pages[0]) == Some(check.generations[0])
            && current(check.pages[1]) == Some(check.generations[1])
    }
    /// The former per-hit stamp, kept as the reference for code-check tests.
    #[cfg(test)]
    pub(crate) fn stamp_reference(
        &self,
        range: Range<usize>,
        mapping: u64,
    ) -> Option<(CodeOwnerId, u64, [u64; 2])> {
        if range.is_empty() || range.end > self.storage.len() || mapping == u64::MAX {
            return None;
        }
        Some((
            self.identity?,
            mapping,
            [
                self.storage.generations()[range.start / PAGE_SIZE],
                self.storage.generations()[(range.end - 1) / PAGE_SIZE],
            ],
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_and_direct_views_share_bytes_generations_and_identity() {
        let mut ram = Ram::new(3 * PAGE_SIZE);
        let range = PAGE_SIZE..PAGE_SIZE + 4;
        let bytes_pointer = ram.storage.bytes().as_ptr().wrapping_add(PAGE_SIZE);
        let generations_pointer = ram.storage.generations().as_ptr();
        let initial = ram.code_check(range.clone()).unwrap();
        {
            let bytes = ram.view_mut(range.clone()).unwrap();
            assert_eq!(bytes.as_ptr(), bytes_pointer);
            bytes.copy_from_slice(&[1, 2, 3, 4]);
        }
        assert_eq!(ram.read(PAGE_SIZE as u32), Ok(1));
        let ordinary = ram.code_check(range.clone()).unwrap();
        assert_eq!(ordinary.space, initial.space);
        assert_eq!(ordinary.generations, [1, 1]);
        assert!(!ram.code_valid(&initial));
        {
            let view = ram.direct_view_mut(range.clone()).unwrap();
            assert_eq!(view.bytes.as_ptr(), bytes_pointer);
            assert_eq!(view.page_generations.as_ptr(), generations_pointer);
            assert_eq!(view.bytes, &[1, 2, 3, 4]);
            view.bytes[1] = 42;
            view.page_generations[1] += 1;
        }
        ram.complete_direct_writes(1);
        assert_eq!(ram.view(range.clone()), Some(&[1, 42, 3, 4][..]));
        assert_eq!(ram.read((PAGE_SIZE + 1) as u32), Ok(42));
        let direct = ram.code_check(range.clone()).unwrap();
        assert_eq!(direct.space, initial.space);
        assert_eq!(direct.generations, [2, 2]);
        assert!(!ram.code_valid(&ordinary));
        assert_eq!(ram.writes(), 2);
        {
            let view = ram.native_view_mut(range).unwrap();
            assert_eq!(view.bytes.as_ptr(), bytes_pointer);
            assert_eq!(view.page_generations.as_ptr(), generations_pointer);
            assert_eq!(view.bytes, &[1, 42, 3, 4]);
        }
        assert!(ram.code_valid(&direct));
    }
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
        ram.storage.generations_mut()[1] = u64::MAX;
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
        assert_eq!(ram.storage.generations()[1], u64::MAX);
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
        ram.storage.generations_mut()[0] = u64::MAX;
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

#[cfg(feature = "save-state")]
#[path = "memory_saved.rs"]
mod saved_state;
