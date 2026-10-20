//! A bounded interpreter borrow of the selected RAM owner's actual storage.
use core::ops::Range;
use doslet_bus::{BusError, CodeCheck, MemoryBus, PortBus};
use doslet_vga::{APERTURE_LEN, APERTURE_START, Vga};

use super::PAGE_SIZE;

pub(crate) struct Effects {
    pub page_writes: u64,
    pub code_exhausted: bool,
    pub device_accesses: u64,
    #[cfg(feature = "profiling")]
    pub bda_tick_reads: u64,
}

/// Identity-mapped RAM and, optionally, the PC's time-independent VGA aperture.
/// The CPU preflights complete operands and stops before I/O or faults. VGA uses
/// its ordinary byte operations, preserving latches, revisions and access counts.
/// No reference survives a foreign procedure call.
pub(crate) struct BorrowedRamBus<'a> {
    bytes: &'a mut [u8],
    generations: &'a mut [u64],
    code_space: u64,
    video: Option<Video<'a>>,
    effects: Effects,
}

struct Video<'a> {
    bytes: &'a mut [u8],
    vga: &'a mut Vga,
}

impl<'a> BorrowedRamBus<'a> {
    pub(super) fn new(bytes: &'a mut [u8], generations: &'a mut [u64], code_space: u64) -> Self {
        Self {
            bytes,
            generations,
            code_space,
            video: None,
            effects: Effects {
                page_writes: 0,
                code_exhausted: false,
                device_accesses: 0,
                #[cfg(feature = "profiling")]
                bda_tick_reads: 0,
            },
        }
    }

    pub(super) fn with_video(
        bytes: &'a mut [u8],
        generations: &'a mut [u64],
        video: &'a mut [u8],
        vga: &'a mut Vga,
        code_space: u64,
    ) -> Self {
        let mut bus = Self::new(bytes, generations, code_space);
        bus.video = Some(Video { bytes: video, vga });
        bus
    }

    pub(crate) fn finish(self) -> Effects {
        self.effects
    }

    #[inline]
    fn ram_range(&self, address: u32, length: usize) -> Option<Range<usize>> {
        let start = address as usize;
        let end = start.checked_add(length)?;
        if length == 0
            || end > self.bytes.len()
            || (start < APERTURE_START as usize + APERTURE_LEN && end > APERTURE_START as usize)
        {
            return None;
        }
        Some(start..end)
    }

    #[inline]
    fn read<const N: usize>(&mut self, address: u32) -> Result<[u8; N], BusError> {
        let Some(range) = self.ram_range(address, N) else {
            let video = self
                .video
                .as_mut()
                .ok_or(BusError::UnmappedMemory(address))?;
            let mut bytes = [0; N];
            for (index, byte) in bytes.iter_mut().enumerate() {
                let address = address.wrapping_add(index as u32);
                let offset = video_offset(address).ok_or(BusError::UnmappedMemory(address))?;
                self.effects.device_accesses = self.effects.device_accesses.wrapping_add(1);
                *byte = video
                    .vga
                    .read_memory_with_memory(video.bytes, offset)
                    .ok_or(BusError::UnmappedMemory(address))?;
            }
            return Ok(bytes);
        };
        #[cfg(feature = "profiling")]
        {
            self.effects.bda_tick_reads +=
                range.end.min(0x470).saturating_sub(range.start.max(0x46c)) as u64;
        }
        Ok(self.bytes[range]
            .try_into()
            .expect("fixed RAM access width"))
    }

    #[inline]
    fn write<const N: usize>(&mut self, address: u32, value: [u8; N]) -> Result<(), BusError> {
        // Check the complete span before changing bytes or page generations.
        let Some(range) = self.ram_range(address, N) else {
            let video = self
                .video
                .as_mut()
                .ok_or(BusError::UnmappedMemory(address))?;
            for (index, byte) in value.into_iter().enumerate() {
                let address = address.wrapping_add(index as u32);
                let offset = video_offset(address).ok_or(BusError::UnmappedMemory(address))?;
                self.effects.device_accesses = self.effects.device_accesses.wrapping_add(1);
                if !video
                    .vga
                    .write_memory_with_memory(video.bytes, offset, byte)
                {
                    return Err(BusError::UnmappedMemory(address));
                }
            }
            return Ok(());
        };
        for page in range.start / PAGE_SIZE..=(range.end - 1) / PAGE_SIZE {
            self.effects.page_writes = self.effects.page_writes.wrapping_add(1);
            if let Some(next) = self.generations[page].checked_add(1) {
                self.generations[page] = next;
            } else {
                // Match ordinary RAM writes: the write succeeds, but no cached
                // proof may remain valid, including one for an unrelated page.
                self.code_space = 0;
                self.effects.code_exhausted = true;
            }
        }
        self.bytes[range].copy_from_slice(&value);
        Ok(())
    }
}

impl MemoryBus for BorrowedRamBus<'_> {
    #[inline]
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        self.read(address).map(|[value]| value)
    }

    #[inline]
    fn read_u16(&mut self, address: u32) -> Result<u16, BusError> {
        self.read(address).map(u16::from_le_bytes)
    }

    #[inline]
    fn read_u32(&mut self, address: u32) -> Result<u32, BusError> {
        self.read(address).map(u32::from_le_bytes)
    }

    #[inline]
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        self.write(address, [value])
    }

    #[inline]
    fn write_u16(&mut self, address: u32, value: u16) -> Result<(), BusError> {
        self.write(address, value.to_le_bytes())
    }

    #[inline]
    fn write_u32(&mut self, address: u32, value: u32) -> Result<(), BusError> {
        self.write(address, value.to_le_bytes())
    }

    #[inline]
    fn is_plain_ram(&self, address: u32, length: u8) -> bool {
        self.ram_range(address, usize::from(length)).is_some()
    }

    #[inline]
    fn is_batch_memory(&self, address: u32, length: u8) -> bool {
        self.is_plain_ram(address, length)
            || (length != 0
                && self.video.as_ref().is_some_and(|video| {
                    (0..u32::from(length)).all(|byte| {
                        video_offset(address.wrapping_add(byte))
                            .is_some_and(|offset| video.vga.memory_available(offset))
                    })
                }))
    }

    fn code_check(&self, address: u32, length: u8) -> Option<CodeCheck> {
        let range = self.ram_range(address, usize::from(length))?;
        if self.code_space == 0 {
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
    fn code_valid(&self, check: &CodeCheck) -> bool {
        let current = |page: u32| self.generations.get(page as usize).copied();
        check.space == self.code_space
            && current(check.pages[0]) == Some(check.generations[0])
            && current(check.pages[1]) == Some(check.generations[1])
    }

    fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
        let start = address as usize;
        if start >= self.bytes.len()
            || (APERTURE_START as usize..APERTURE_START as usize + APERTURE_LEN).contains(&start)
        {
            return None;
        }
        let mut length = usize::from(length).min(self.bytes.len() - start);
        if start < APERTURE_START as usize {
            length = length.min(APERTURE_START as usize - start);
        }
        Some(&self.bytes[start..start + length])
    }
}

#[inline]
fn video_offset(address: u32) -> Option<usize> {
    let offset = address.checked_sub(APERTURE_START)? as usize;
    (offset < APERTURE_LEN).then_some(offset)
}

impl PortBus for BorrowedRamBus<'_> {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
        Err(BusError::UnmappedPort(port))
    }

    fn write_port_u8(&mut self, port: u16, _value: u8) -> Result<(), BusError> {
        Err(BusError::UnmappedPort(port))
    }
}

#[cfg(test)]
mod tests;
