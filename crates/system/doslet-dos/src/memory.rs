//! A bounded DOS conventional-memory arena with guest-visible MCBs.
use doslet_bus::{BusError, MemoryBus};

// PRINCE allocates 233 conventional-memory buffers through its extender.
// Keep metadata fixed and allocation-free without exhausting a 64-entry table.
const CAPACITY: usize = 1024;
#[derive(Clone, Copy, Default, Debug)]
struct Block {
    segment: u16,
    paragraphs: u16,
    owner: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryFailure {
    pub code: u16,
    pub available: u16,
}

/// DOS-owned blocks, excluding MCB headers and firmware reservations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryStats {
    pub allocated_bytes: u64,
    pub free_bytes: u64,
    pub largest_free_bytes: u64,
    pub allocated_blocks: u32,
}

pub struct MemoryArena {
    blocks: [Block; CAPACITY],
    len: usize,
}
impl MemoryArena {
    /// Read fixed allocation metadata without scanning or copying guest RAM.
    pub fn stats(&self) -> MemoryStats {
        let mut stats = MemoryStats::default();
        for block in &self.blocks[..self.len] {
            let bytes = u64::from(block.paragraphs) * 16;
            if block.owner == 0 {
                stats.free_bytes += bytes;
                stats.largest_free_bytes = stats.largest_free_bytes.max(bytes);
            } else {
                stats.allocated_bytes += bytes;
                stats.allocated_blocks += 1;
            }
        }
        stats
    }
    /// Initial process block followed by free memory up to the exclusive end.
    pub fn new(psp: u16, process_end: u16, arena_end: u16) -> Self {
        assert!(psp != 0 && psp < process_end && process_end <= arena_end);
        let mut blocks = [Block::default(); CAPACITY];
        blocks[0] = Block {
            segment: psp,
            paragraphs: process_end - psp,
            owner: psp,
        };
        let mut len = 1;
        if process_end < arena_end {
            blocks[1] = Block {
                segment: process_end + 1,
                paragraphs: arena_end - process_end - 1,
                owner: 0,
            };
            len = 2;
        }
        Self { blocks, len }
    }
    pub fn allocate(&mut self, paragraphs: u16, owner: u16) -> Result<u16, MemoryFailure> {
        let mut largest = 0;
        for index in 0..self.len {
            let block = self.blocks[index];
            if block.owner != 0 {
                continue;
            }
            largest = largest.max(block.paragraphs);
            if block.paragraphs >= paragraphs {
                self.split(index, paragraphs)?;
                self.blocks[index].owner = owner;
                return Ok(block.segment);
            }
        }
        Err(MemoryFailure {
            code: 8,
            available: largest,
        })
    }
    /// Reserve an environment immediately before the first process MCB at setup.
    pub fn with_environment(mut self, segment: u16, owner: u16) -> Self {
        assert!(segment != 0 && segment < self.blocks[0].segment && self.len < CAPACITY);
        let paragraphs = self.blocks[0].segment - segment - 1;
        self.blocks.copy_within(0..self.len, 1);
        self.blocks[0] = Block {
            segment,
            paragraphs,
            owner,
        };
        self.len += 1;
        self
    }
    pub fn free(&mut self, segment: u16) -> Result<(), MemoryFailure> {
        let index = self.find(segment)?;
        self.blocks[index].owner = 0;
        self.merge_next(index);
        if index != 0 && self.blocks[index - 1].owner == 0 {
            self.merge_next(index - 1);
        }
        Ok(())
    }
    pub fn resize(&mut self, segment: u16, paragraphs: u16) -> Result<(), MemoryFailure> {
        let index = self.find(segment)?;
        self.merge_next(index);
        if self.blocks[index].paragraphs < paragraphs {
            return Err(MemoryFailure {
                code: 8,
                available: self.blocks[index].paragraphs,
            });
        }
        self.split(index, paragraphs)
    }
    fn find(&self, segment: u16) -> Result<usize, MemoryFailure> {
        (0..self.len)
            .find(|index| self.blocks[*index].segment == segment && self.blocks[*index].owner != 0)
            .ok_or(MemoryFailure {
                code: 9,
                available: 0,
            })
    }
    fn split(&mut self, index: usize, paragraphs: u16) -> Result<(), MemoryFailure> {
        let block = self.blocks[index];
        if block.paragraphs == paragraphs {
            return Ok(());
        }
        if self.len == CAPACITY {
            return Err(MemoryFailure {
                code: 8,
                available: block.paragraphs,
            });
        }
        self.blocks.copy_within(index + 1..self.len, index + 2);
        self.blocks[index + 1] = Block {
            segment: block.segment + paragraphs + 1,
            paragraphs: block.paragraphs - paragraphs - 1,
            owner: 0,
        };
        self.blocks[index].paragraphs = paragraphs;
        self.len += 1;
        Ok(())
    }
    fn merge_next(&mut self, index: usize) {
        if index + 1 < self.len && self.blocks[index + 1].owner == 0 {
            self.blocks[index].paragraphs += self.blocks[index + 1].paragraphs + 1;
            self.blocks.copy_within(index + 2..self.len, index + 1);
            self.len -= 1;
        }
    }
    /// Publish small allocation metadata through the bus; never borrow/clone RAM.
    pub fn write_mcbs<B: MemoryBus>(&self, bus: &mut B) -> Result<(), BusError> {
        for (index, block) in self.blocks[..self.len].iter().enumerate() {
            let address = u32::from(block.segment - 1) * 16;
            for offset in 0..16 {
                bus.write_u8(address + offset, 0)?;
            }
            bus.write_u8(address, if index + 1 == self.len { b'Z' } else { b'M' })?;
            bus.write_u16(address + 1, block.owner)?;
            bus.write_u16(address + 3, block.paragraphs)?;
        }
        Ok(())
    }
    pub fn first_mcb(&self) -> u16 {
        self.blocks[0].segment - 1
    }
    pub fn block_end(&self, segment: u16) -> Result<u16, MemoryFailure> {
        let index = self.find(segment)?;
        Ok(segment + self.blocks[index].paragraphs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stats_follow_allocations_frees_and_mcb_overhead() {
        let mut arena = MemoryArena::new(0x1000, 0x2000, 0xA000);
        let initial = arena.stats();
        assert_eq!(initial.allocated_bytes, 0x10000);
        assert_eq!(initial.free_bytes, 0x7FFF0);
        assert_eq!(initial.largest_free_bytes, initial.free_bytes);
        assert_eq!(initial.allocated_blocks, 1);
        let segment = arena.allocate(0x100, 0x1000).unwrap();
        let allocated = arena.stats();
        assert_eq!(allocated.allocated_bytes, initial.allocated_bytes + 4096);
        assert_eq!(allocated.free_bytes, initial.free_bytes - 4096 - 16);
        assert_eq!(allocated.allocated_blocks, 2);
        arena.free(segment).unwrap();
        assert_eq!(arena.stats(), initial);
    }
    #[test]
    fn hundreds_of_small_dos_buffers_fit_without_false_out_of_memory() {
        let mut arena = MemoryArena::new(0x1000, 0x2000, 0xA000);
        for _ in 0..233 {
            arena.allocate(64, 0x1000).unwrap();
        }
        assert_eq!(arena.len, 235);
        assert!(arena.allocate(0x4000, 0x1000).is_ok());
    }
    #[test]
    fn resize_allocate_free_and_coalesce_account_for_mcb_paragraphs() {
        let mut arena = MemoryArena::new(0x1000, 0x2000, 0x2000);
        arena.resize(0x1000, 0x335).unwrap();
        assert_eq!(arena.blocks[1].segment, 0x1336);
        let first = arena.allocate(0x100, 0x1000).unwrap();
        let second = arena.allocate(0x100, 0x1000).unwrap();
        assert_eq!((first, second), (0x1336, 0x1437));
        arena.free(first).unwrap();
        arena.free(second).unwrap();
        arena.resize(0x1000, 0x1000).unwrap();
        assert_eq!(arena.len, 1);
        assert_eq!(
            arena.allocate(1, 0x1000),
            Err(MemoryFailure {
                code: 8,
                available: 0
            })
        );
        assert_eq!(
            arena.free(second),
            Err(MemoryFailure {
                code: 9,
                available: 0
            })
        );
    }
    #[test]
    fn exhausting_metadata_does_not_overrun_its_fixed_storage() {
        let mut arena = MemoryArena::new(0x1000, 0x1001, 0x2000);
        for _ in 0..CAPACITY - 2 {
            arena.allocate(1, 0x1000).unwrap();
        }
        assert_eq!(arena.len, CAPACITY);
        assert_eq!(arena.allocate(1, 0x1000).unwrap_err().code, 8);
    }
}

#[cfg(feature = "save-state")]
#[path = "memory_saved.rs"]
mod saved_state;
