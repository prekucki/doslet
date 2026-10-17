//! Bus contracts shared by processors and machine adapters.
#![no_std]
pub mod observation;

use core::fmt;
use core::sync::atomic::{AtomicU32, Ordering};

// Setup-only identities shared by all buses implementing the code contract.
// This counter contains no guest/device state and never wraps.
static NEXT_CODE_OWNER: AtomicU32 = AtomicU32::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeOwnerId(u32);
impl CodeOwnerId {
    /// Obtain one identity for a memory owner's lifetime; exhaustion disables
    /// caching. Moving that owner retains its ID, replacing it needs a fresh ID.
    pub fn fresh() -> Option<Self> {
        NEXT_CODE_OWNER
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
                value.checked_add(1)
            })
            .ok()
            .map(Self)
    }
    /// Nonzero for every issued identity.
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BusError {
    UnmappedMemory(u32),
    UnmappedPort(u16),
}

impl fmt::Display for BusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnmappedMemory(address) => write!(f, "unmapped memory at {address:#x}"),
            Self::UnmappedPort(port) => write!(f, "unmapped I/O port {port:#x}"),
        }
    }
}

impl core::error::Error for BusError {}

/// Validity of one side-effect-free code span, resolved when it is decoded.
/// `space` packs a unique owner identity (high 32 bits) with the address-mapping
/// generation (low 32 bits); owners are never reused, including by replacement
/// buses. `pages` are the physical pages of the first and last byte and
/// `generations` their write generations. Every write, including borrowed
/// mutation, must change the relevant generation, and every mapping change must
/// change `space`. Exhausted counters make spans unavailable instead of wrapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeCheck {
    pub space: u64,
    pub pages: [u32; 2],
    pub generations: [u64; 2],
}

/// One borrowed RAM backing span, optionally containing one excluded interval.
/// Every access must fit `bytes` and avoid `[excluded_start, excluded_start +
/// excluded_len)` in CPU address space; zero length disables the exclusion.
/// Excluded bytes are backing storage only, not CPU-visible ordinary RAM.
/// Accesses outside the exclusion must have no device or accounting effects;
/// mappings cannot change while borrowed and no pointer may outlive the borrow.
#[derive(Clone, Copy, Debug)]
pub struct NativeRam<'a> {
    pub linear_base: u32,
    pub bytes: &'a [u8],
    pub excluded_start: u32,
    pub excluded_len: u32,
}

/// An exclusive ordinary-RAM view with explicit code-cache invalidation.
/// `page_generations` is the complete physical 4-KiB page table: a CPU-visible
/// address indexes it with `address >> 12`, independently of `linear_base`.
/// Allowed bytes use that same identity mapping and require no device or
/// access-accounting callbacks. Every complete access must avoid the optional
/// excluded interval, as described for `NativeRam`; its backing bytes are never
/// CPU-visible through this view. Borrowing alone does not invalidate pages.
///
/// Before any write, the caller must check the complete byte span and every
/// affected generation, and decline the write if any generation is `u64::MAX`.
/// Each completed write increments every distinct affected page exactly once,
/// without wrapping. Executing native code must also reject writes to any page
/// containing its currently validated instructions. No pointer may outlive the
/// borrow, and mappings cannot change while it is held.
#[derive(Debug)]
pub struct NativeRamMut<'a> {
    pub linear_base: u32,
    pub bytes: &'a mut [u8],
    pub page_generations: &'a mut [u64],
    pub excluded_start: u32,
    pub excluded_len: u32,
}

/// Accesses preserve device side effects, including when a word spans regions.
pub trait MemoryBus {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError>;
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError>;

    /// Optional batching contract for a complete ordinary, writable RAM span.
    /// True guarantees byte/word/dword accesses within the span succeed without
    /// device effects, changing mappings, or observing/raising hardware events.
    /// Writes must still invalidate code checks. The guarantee lasts until the
    /// next mutable bus operation; ROM, MMIO and wrapping spans return false.
    fn is_plain_ram(&self, _address: u32, _length: u8) -> bool {
        false
    }

    /// Optional batching contract that also admits device memory whose
    /// accesses neither depend on guest time nor raise hardware events, such
    /// as a frame-buffer window. True guarantees byte/word/dword accesses
    /// within the span succeed through the ordinary byte paths and cannot
    /// change mappings; their device effects are the same as unbatched
    /// accesses. The guarantee lasts until the next port access.
    fn is_batch_memory(&self, address: u32, length: u8) -> bool {
        self.is_plain_ram(address, length)
    }

    /// Optional read-only native execution view. Implementations may expose a
    /// conservative subset of RAM, or decline entirely. Generated code must
    /// check every complete access against the span and exclusion, and never
    /// retain pointers.
    fn native_ram(&self) -> Option<NativeRam<'_>> {
        None
    }

    /// Optional exclusive native RAM view. The caller must maintain the page
    /// generations according to `NativeRamMut` before releasing the borrow.
    fn native_ram_mut(&mut self) -> Option<NativeRamMut<'_>> {
        None
    }

    /// Optional cache contract. Defaults keep other buses on the interpreter.
    /// Decode time: resolve the span; None keeps it on byte fetching.
    fn code_check(&self, _address: u32, _length: u8) -> Option<CodeCheck> {
        None
    }
    /// Hit time: true only if no byte of the span changed since `code_check`
    /// and the same storage is still mapped. False negatives are allowed.
    fn code_valid(&self, _check: &CodeCheck) -> bool {
        false
    }
    /// Borrow up to `length` contiguous ordinary-memory code bytes. Inspecting
    /// them must have no device side effects. A short view is allowed.
    fn code_bytes(&self, _address: u32, _length: u8) -> Option<&[u8]> {
        None
    }

    fn read_u16(&mut self, address: u32) -> Result<u16, BusError> {
        let low = self.read_u8(address)?;
        let high = self.read_u8(address.wrapping_add(1))?;
        Ok(u16::from_le_bytes([low, high]))
    }

    fn write_u16(&mut self, address: u32, value: u16) -> Result<(), BusError> {
        let [low, high] = value.to_le_bytes();
        self.write_u8(address, low)?;
        self.write_u8(address.wrapping_add(1), high)
    }

    fn read_u32(&mut self, address: u32) -> Result<u32, BusError> {
        let mut bytes = [0; 4];
        for (offset, byte) in bytes.iter_mut().enumerate() {
            *byte = self.read_u8(address.wrapping_add(offset as u32))?;
        }
        Ok(u32::from_le_bytes(bytes))
    }

    fn write_u32(&mut self, address: u32, value: u32) -> Result<(), BusError> {
        for (offset, byte) in value.to_le_bytes().into_iter().enumerate() {
            self.write_u8(address.wrapping_add(offset as u32), byte)?;
        }
        Ok(())
    }
}

pub trait PortBus {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError>;
    fn write_port_u8(&mut self, port: u16, value: u8) -> Result<(), BusError>;
    /// Default PC-style splitting for byte-wide devices. Wider devices may
    /// override these methods to preserve their transaction semantics.
    fn read_port_u16(&mut self, port: u16) -> Result<u16, BusError> {
        Ok(u16::from_le_bytes([
            self.read_port_u8(port)?,
            self.read_port_u8(port.wrapping_add(1))?,
        ]))
    }
    fn write_port_u16(&mut self, port: u16, value: u16) -> Result<(), BusError> {
        let [low, high] = value.to_le_bytes();
        self.write_port_u8(port, low)?;
        self.write_port_u8(port.wrapping_add(1), high)
    }
    fn read_port_u32(&mut self, port: u16) -> Result<u32, BusError> {
        let low = self.read_port_u16(port)?;
        let high = self.read_port_u16(port.wrapping_add(2))?;
        Ok(u32::from(low) | (u32::from(high) << 16))
    }
    fn write_port_u32(&mut self, port: u16, value: u32) -> Result<(), BusError> {
        self.write_port_u16(port, value as u16)?;
        self.write_port_u16(port.wrapping_add(2), (value >> 16) as u16)
    }
}

pub trait CpuBus: MemoryBus + PortBus {}
impl<T: MemoryBus + PortBus> CpuBus for T {}
