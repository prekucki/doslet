//! Fixed metadata for committed, identity-mapped guest pages. No host allocation
//! occurs in response to guest allocation requests.

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Block {
    pub handle: u32,
    pub address: u32,
    pub requested: u32,
    pub committed: u32,
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct MemoryStats {
    pub requested_bytes: u64,
    pub committed_bytes: u64,
    pub peak_requested_bytes: u64,
    pub peak_committed_bytes: u64,
    pub allocations: u64,
    pub frees: u64,
    pub resizes: u64,
    pub failures: u64,
}

#[derive(Clone)]
pub(super) struct Memory {
    pub blocks: [Block; 512],
    pub stats: MemoryStats,
    pub(super) start: u32,
    pub(super) end: u32,
    pub(super) next_handle: u32,
}

impl Memory {
    pub fn capacity(&self) -> u64 {
        u64::from(self.end - self.start)
    }
    pub fn new(start: u32, end: u32) -> Self {
        Self {
            blocks: [Block::default(); 512],
            stats: MemoryStats::default(),
            start,
            end,
            next_handle: 1,
        }
    }

    pub fn free_space(&self) -> (u32, u32) {
        let mut at = self.start;
        let mut largest = 0;
        let mut total = 0;
        loop {
            let next = self
                .blocks
                .iter()
                .filter(|b| b.handle != 0 && b.address >= at)
                .min_by_key(|b| b.address);
            let end = next.map_or(self.end, |b| b.address);
            let gap = end.saturating_sub(at);
            largest = largest.max(gap);
            total += gap;
            let Some(block) = next else { break };
            at = block.address + block.committed;
        }
        (largest, total)
    }

    pub fn allocate(&mut self, requested: u32) -> Result<Block, u16> {
        let result = self.allocate_inner(requested);
        if result.is_err() {
            self.stats.failures += 1;
        }
        result
    }

    fn allocate_inner(&mut self, requested: u32) -> Result<Block, u16> {
        let size = rounded(requested).ok_or(0x8021_u16)?;
        let slot = self
            .blocks
            .iter()
            .position(|b| b.handle == 0)
            .ok_or(0x8016_u16)?;
        let next_handle = self.next_handle.checked_add(1).ok_or(0x8016_u16)?;
        let address = self.find(size, None).ok_or(0x8013_u16)?;
        let block = Block {
            handle: self.next_handle,
            address,
            requested,
            committed: size,
        };
        self.next_handle = next_handle;
        self.blocks[slot] = block;
        self.stats.allocations += 1;
        self.account();
        Ok(block)
    }

    pub fn free(&mut self, handle: u32) -> Result<(), u16> {
        let Some(slot) = self
            .blocks
            .iter()
            .position(|b| b.handle != 0 && b.handle == handle)
        else {
            self.stats.failures += 1;
            return Err(0x8023);
        };
        self.blocks[slot] = Block::default();
        self.stats.frees += 1;
        self.account();
        Ok(())
    }

    /// The lowest free address for `size` bytes, ignoring block `except`.
    fn find(&self, size: u32, except: Option<usize>) -> Option<u32> {
        let mut address = self.start;
        loop {
            let end = address.checked_add(size).filter(|end| *end <= self.end)?;
            match self.blocks.iter().enumerate().find(|&(i, b)| {
                Some(i) != except
                    && b.handle != 0
                    && b.address < end
                    && b.address + b.committed > address
            }) {
                Some((_, b)) => address = b.address + b.committed,
                None => return Some(address),
            }
        }
    }

    /// Resize in place when the following pages are free, else move the
    /// block, as DPMI allows. A move also returns the old address and the
    /// bytes to copy from it; the caller copies them, at most installed RAM.
    pub fn resize(
        &mut self,
        handle: u32,
        requested: u32,
    ) -> Result<(Block, Option<(u32, u32)>), u16> {
        let result = self.resize_inner(handle, requested);
        if result.is_err() {
            self.stats.failures += 1;
        }
        result
    }

    fn resize_inner(
        &mut self,
        handle: u32,
        requested: u32,
    ) -> Result<(Block, Option<(u32, u32)>), u16> {
        let size = rounded(requested).ok_or(0x8021_u16)?;
        let slot = self
            .blocks
            .iter()
            .position(|b| b.handle != 0 && b.handle == handle)
            .ok_or(0x8023_u16)?;
        let old = self.blocks[slot];
        let in_place = old.address.checked_add(size).is_some_and(|end| {
            end <= self.end
                && !self.blocks.iter().enumerate().any(|(i, b)| {
                    i != slot
                        && b.handle != 0
                        && b.address < end
                        && b.address + b.committed > old.address
                })
        });
        // A failed move leaves the old block and its contents intact.
        let address = if in_place {
            old.address
        } else {
            self.find(size, Some(slot)).ok_or(0x8013_u16)?
        };
        self.blocks[slot] = Block {
            address,
            requested,
            committed: size,
            ..old
        };
        self.stats.resizes += 1;
        self.account();
        let moved = (address != old.address).then_some((old.address, old.committed.min(size)));
        Ok((self.blocks[slot], moved))
    }

    fn account(&mut self) {
        self.stats.requested_bytes = self.blocks.iter().map(|b| u64::from(b.requested)).sum();
        self.stats.committed_bytes = self.blocks.iter().map(|b| u64::from(b.committed)).sum();
        self.stats.peak_requested_bytes = self
            .stats
            .peak_requested_bytes
            .max(self.stats.requested_bytes);
        self.stats.peak_committed_bytes = self
            .stats
            .peak_committed_bytes
            .max(self.stats.committed_bytes);
    }
}

fn rounded(bytes: u32) -> Option<u32> {
    (bytes != 0)
        .then_some(bytes)?
        .checked_add(4095)
        .map(|v| v & !4095)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reuse_resize_failure_and_peak_accounting() {
        let mut m = Memory::new(0x110000, 0x120000);
        let a = m.allocate(1).unwrap();
        let b = m.allocate(4097).unwrap();
        assert_eq!(m.stats.requested_bytes, 4098);
        assert_eq!(m.stats.committed_bytes, 12288);
        // Without room after it, the block moves past B; its bytes follow.
        let (moved, copy) = m.resize(a.handle, 8192).unwrap();
        assert_eq!(moved.address, b.address + b.committed);
        assert_eq!(copy, Some((a.address, 4096)));
        assert_eq!(moved.handle, a.handle);
        let (back, _) = m.resize(a.handle, 1).unwrap();
        assert_eq!(back.address, moved.address);
        m.free(b.handle).unwrap();
        let (grown, copy) = m.resize(a.handle, 8193).unwrap();
        assert_eq!(grown.address, moved.address);
        assert_eq!(copy, None);
        // The moved A and B coexisted at 8192 + 4097 requested bytes.
        assert_eq!(m.stats.peak_requested_bytes, 12289);
        assert_eq!(m.stats.peak_committed_bytes, 16384);
        m.free(a.handle).unwrap();
        assert!(m.free(a.handle).is_err());
        let c = m.allocate(65536).unwrap();
        assert_eq!(c.address, a.address);
        assert_ne!(c.handle, a.handle);
        assert_eq!(m.free_space(), (0, 0));
        assert!(m.allocate(1).is_err());
        assert!(m.allocate(u32::MAX).is_err());
    }
}
