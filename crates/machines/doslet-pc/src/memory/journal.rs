//! Bounded old-byte journal for measured recurrence, independent of idle skipping.
use alloc::{boxed::Box, vec};

const CAPACITY: usize = 2048;
const SLOTS: usize = CAPACITY * 2;

#[derive(Clone, Copy, Default)]
struct Entry {
    epoch: u64,
    address: u32,
    old: u8,
}

pub(super) struct Journal {
    entries: Box<[Entry]>,
    touched: Box<[usize]>,
    epoch: u64,
    len: usize,
    active: bool,
    overflow: bool,
}
impl Default for Journal {
    fn default() -> Self {
        Self {
            entries: vec![Entry::default(); SLOTS].into_boxed_slice(),
            touched: vec![0; CAPACITY].into_boxed_slice(),
            epoch: 0,
            len: 0,
            active: false,
            overflow: false,
        }
    }
}
impl Journal {
    pub fn recording(&self) -> bool {
        self.active && !self.overflow
    }
    pub fn begin(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.entries.fill(Entry::default());
            self.epoch = 1;
        }
        self.len = 0;
        self.active = true;
        self.overflow = false;
    }
    pub fn record(&mut self, address: u32, old: u8) {
        if !self.recording() {
            return;
        }
        let start = address.wrapping_mul(0x9e37_79b9) as usize & (SLOTS - 1);
        for offset in 0..SLOTS {
            let index = (start + offset) & (SLOTS - 1);
            let entry = &mut self.entries[index];
            if entry.epoch == self.epoch {
                if entry.address == address {
                    return;
                }
            } else {
                if self.len == CAPACITY {
                    self.overflow = true;
                    return;
                }
                *entry = Entry {
                    epoch: self.epoch,
                    address,
                    old,
                };
                self.touched[self.len] = index;
                self.len += 1;
                return;
            }
        }
        self.overflow = true;
    }
    pub fn record_range(&mut self, start: usize, bytes: &[u8]) {
        if !self.recording() {
            return;
        }
        if bytes.len() > CAPACITY {
            self.overflow = true;
            return;
        }
        for (i, &old) in bytes.iter().enumerate() {
            self.record((start + i) as u32, old);
        }
    }
    pub fn end(&mut self, bytes: &[u8]) -> bool {
        let unchanged = self.recording()
            && self.touched[..self.len].iter().all(|&index| {
                let entry = &self.entries[index];
                bytes[entry.address as usize] == entry.old
            });
        self.active = false;
        unchanged
    }
}
