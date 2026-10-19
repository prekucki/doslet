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
    start: u32,
    end: u32,
    next_handle: u32,
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
        let mut address = self.start;
        loop {
            let end = address
                .checked_add(size)
                .filter(|end| *end <= self.end)
                .ok_or(0x8013_u16)?;
            match self
                .blocks
                .iter()
                .find(|b| b.handle != 0 && b.address < end && b.address + b.committed > address)
            {
                Some(b) => address = b.address + b.committed,
                None => break,
            }
        }
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

    pub fn resize(&mut self, handle: u32, requested: u32) -> Result<Block, u16> {
        let result = self.resize_inner(handle, requested);
        if result.is_err() {
            self.stats.failures += 1;
        }
        result
    }

    fn resize_inner(&mut self, handle: u32, requested: u32) -> Result<Block, u16> {
        let size = rounded(requested).ok_or(0x8021_u16)?;
        let slot = self
            .blocks
            .iter()
            .position(|b| b.handle != 0 && b.handle == handle)
            .ok_or(0x8023_u16)?;
        let address = self.blocks[slot].address;
        let end = address
            .checked_add(size)
            .filter(|end| *end <= self.end)
            .ok_or(0x8013_u16)?;
        if self.blocks.iter().enumerate().any(|(i, b)| {
            i != slot && b.handle != 0 && b.address < end && b.address + b.committed > address
        }) {
            return Err(0x8013);
        }
        // In-place growth only: a host call must never copy an unbounded guest
        // block. Failure leaves the old block and its contents intact.
        self.blocks[slot].requested = requested;
        self.blocks[slot].committed = size;
        self.stats.resizes += 1;
        self.account();
        Ok(self.blocks[slot])
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
        assert!(m.resize(a.handle, 8192).is_err());
        assert_eq!(m.blocks[0], a);
        m.free(b.handle).unwrap();
        let grown = m.resize(a.handle, 8193).unwrap();
        assert_eq!(grown.address, a.address);
        assert_eq!(m.stats.peak_requested_bytes, 8193);
        assert_eq!(m.stats.peak_committed_bytes, 12288);
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
