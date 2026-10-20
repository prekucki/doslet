//! Optional direct-memory ABI. All offsets below are relative to a descriptor
//! immediately following the unchanged 72-byte CPU frame. Only trusted matching
//! compiler/runtime pairs may use it; no Rust object layout crosses this ABI.
//! Buffer ownership and mappings remain stable until the next runtime boundary.

pub const OFFSET: u32 = 72;
pub const BYTES: usize = 384;
pub const VERSION: u32 = 2;
pub const VERSION_OFFSET: u32 = 0;
pub const RAM_POINTER: u32 = 8;
pub const RAM_BASE: u32 = 12;
pub const RAM_LENGTH: u32 = 16;
pub const RAM_EXCLUDED_START: u32 = 20;
pub const RAM_EXCLUDED_LENGTH: u32 = 24;
pub const GENERATIONS_POINTER: u32 = 28;
pub const GENERATIONS_LENGTH: u32 = 32;
pub const CODE_PAGES_POINTER: u32 = 36;
pub const CODE_PAGES_LENGTH: u32 = 40;
pub const STACK_ADDRESS: u32 = 44;
pub const DEVICE_START: u32 = 48;
pub const DEVICE_LENGTH: u32 = 52;
pub const DEVICE_POINTER: u32 = 56;
pub const DEVICE_BACKING_LENGTH: u32 = 60;
pub const DEVICE_KIND: u32 = 64;
pub const DEVICE_BASE: u32 = 68;
pub const DEVICE_CHAIN4: u32 = 72;
pub const DEVICE_READ_PLANE: u32 = 76;
pub const DEVICE_COMPARE_ENABLED: u32 = 80;
pub const DEVICE_COMPARE: u32 = 84;
pub const DEVICE_CARE: u32 = 88;
pub const DEVICE_LATCHES: u32 = 92;
pub const DEVICE_READS: u32 = 96;
pub const SEGMENTS: u32 = 104;
pub const SEGMENT_STRIDE: u32 = 16;
pub const RAM_WRITES: u32 = 296;
pub const INLINE_GRANT: u32 = 304;
pub const INLINE_RESERVED: u32 = 308;
pub const REGION_GUARDS_POINTER: u32 = 312;
pub const REGION_GUARDS_LENGTH: u32 = 316;
pub const REGION_GUARD_BYTES: usize = 32;
pub const DEVICE_WRITE_ENABLED: u32 = 320;
pub const DEVICE_PLANE_MASK: u32 = 324;
pub const DEVICE_WRITE_MODE: u32 = 328;
pub const DEVICE_SET_RESET: u32 = 332;
pub const DEVICE_ENABLE_SET_RESET: u32 = 336;
pub const DEVICE_ROTATE: u32 = 340;
pub const DEVICE_BIT_MASK: u32 = 344;
pub const DEVICE_WRITES: u32 = 352;
pub const DEVICE_CHANGES: u32 = 360;

/// The order matches x86 ES, CS, SS, DS, FS, GS, without borrowing CPU state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Segment {
    Es,
    Cs,
    Ss,
    Ds,
    Fs,
    Gs,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SegmentBounds {
    pub valid: bool,
    pub base: u32,
    pub first: u32,
    pub last: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryAddress {
    /// Full 32-bit GPR identities; final address-size16 masking is explicit.
    pub base: Option<super::Register>,
    pub index: Option<super::Register>,
    pub scale: u8,
    pub displacement: u32,
    pub address16: bool,
    pub segment: Segment,
}

impl MemoryAddress {
    /// Effective address of a single LODS or STOS, before its pointer update.
    pub(super) fn string(source: bool, address16: bool, segment: Segment) -> Self {
        Self {
            base: Some(if source {
                super::Register::Esi
            } else {
                super::Register::Edi
            }),
            index: None,
            scale: 1,
            displacement: 0,
            address16,
            segment,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RamDescriptor {
    pub pointer: u32,
    pub linear_base: u32,
    pub length: u32,
    pub excluded_start: u32,
    pub excluded_length: u32,
    pub generations_pointer: u32,
    pub generations_length: u32,
    /// One byte per physical 4-KiB page; nonzero forbids a direct store.
    pub code_pages_pointer: u32,
    pub code_pages_length: u32,
}

/// Live SS.B capability, independent of code and instruction address widths.
/// Disabled descriptors cannot execute direct stack operations.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum StackAddress {
    #[default]
    Disabled = 0,
    Bits16 = 1,
    Bits32 = 2,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u32)]
pub enum DeviceReadKind {
    #[default]
    Disabled = 0,
    Linear = 1,
    FourLanes = 2,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceReadDescriptor {
    pub guest_start: u32,
    pub guest_length: u32,
    pub pointer: u32,
    pub backing_length: u32,
    pub kind: DeviceReadKind,
    pub base_offset: u32,
    pub chain4: bool,
    pub read_plane: u8,
    pub compare_enabled: bool,
    pub compare: u8,
    pub care: u8,
    pub latches: [u8; 4],
}

/// The write pipeline captured for one bounded exclusive execution slice.
/// Four-lane writes use the same physical backing and latch mirror as reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DeviceWriteDescriptor {
    pub enabled: bool,
    pub plane_mask: u8,
    pub mode: u8,
    pub set_reset: u8,
    pub enable_set_reset: u8,
    pub rotate: u8,
    pub bit_mask: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Descriptor {
    pub ram: RamDescriptor,
    pub stack_address: StackAddress,
    pub device: DeviceReadDescriptor,
    pub device_write: DeviceWriteDescriptor,
    /// First six are read windows, next six are write windows.
    pub windows: [[SegmentBounds; 6]; 2],
    pub inline_grant: u32,
    pub region_guards_pointer: u32,
    pub region_guards_length: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Effects {
    pub device_reads: u64,
    pub device_writes: u64,
    pub device_changes: u64,
    pub latches: [u8; 4],
    pub ram_writes: u64,
    pub inline_reserved: u32,
    pub inline_remaining: u32,
}

impl Descriptor {
    pub fn write(&self, bytes: &mut [u8]) -> Result<(), super::FrameError> {
        let bytes = bytes.get_mut(..BYTES).ok_or(super::FrameError)?;
        bytes.fill(0);
        let mut word = |offset: u32, value: u32| {
            bytes[offset as usize..offset as usize + 4].copy_from_slice(&value.to_le_bytes());
        };
        word(VERSION_OFFSET, VERSION);
        for (offset, value) in [
            (RAM_POINTER, self.ram.pointer),
            (RAM_BASE, self.ram.linear_base),
            (RAM_LENGTH, self.ram.length),
            (RAM_EXCLUDED_START, self.ram.excluded_start),
            (RAM_EXCLUDED_LENGTH, self.ram.excluded_length),
            (GENERATIONS_POINTER, self.ram.generations_pointer),
            (GENERATIONS_LENGTH, self.ram.generations_length),
            (CODE_PAGES_POINTER, self.ram.code_pages_pointer),
            (CODE_PAGES_LENGTH, self.ram.code_pages_length),
            (STACK_ADDRESS, self.stack_address as u32),
            (DEVICE_START, self.device.guest_start),
            (DEVICE_LENGTH, self.device.guest_length),
            (DEVICE_POINTER, self.device.pointer),
            (DEVICE_BACKING_LENGTH, self.device.backing_length),
            (DEVICE_KIND, self.device.kind as u32),
            (DEVICE_BASE, self.device.base_offset),
            (DEVICE_CHAIN4, u32::from(self.device.chain4)),
            (DEVICE_READ_PLANE, u32::from(self.device.read_plane)),
            (
                DEVICE_COMPARE_ENABLED,
                u32::from(self.device.compare_enabled),
            ),
            (DEVICE_COMPARE, u32::from(self.device.compare)),
            (DEVICE_CARE, u32::from(self.device.care)),
            (DEVICE_LATCHES, u32::from_le_bytes(self.device.latches)),
            (DEVICE_WRITE_ENABLED, u32::from(self.device_write.enabled)),
            (DEVICE_PLANE_MASK, u32::from(self.device_write.plane_mask)),
            (DEVICE_WRITE_MODE, u32::from(self.device_write.mode)),
            (DEVICE_SET_RESET, u32::from(self.device_write.set_reset)),
            (
                DEVICE_ENABLE_SET_RESET,
                u32::from(self.device_write.enable_set_reset),
            ),
            (DEVICE_ROTATE, u32::from(self.device_write.rotate)),
            (DEVICE_BIT_MASK, u32::from(self.device_write.bit_mask)),
        ] {
            word(offset, value);
        }
        for (access, windows) in self.windows.iter().enumerate() {
            for (segment, window) in windows.iter().enumerate() {
                let offset = SEGMENTS + ((access * 6 + segment) as u32) * SEGMENT_STRIDE;
                word(offset, u32::from(window.valid));
                word(offset + 4, window.base);
                word(offset + 8, window.first);
                word(offset + 12, window.last);
            }
        }
        word(INLINE_GRANT, self.inline_grant);
        word(REGION_GUARDS_POINTER, self.region_guards_pointer);
        word(REGION_GUARDS_LENGTH, self.region_guards_length);
        Ok(())
    }
}

impl Effects {
    pub fn read(bytes: &[u8]) -> Result<Self, super::FrameError> {
        let bytes = bytes.get(..BYTES).ok_or(super::FrameError)?;
        let wide = |offset: u32| {
            let mut value = [0; 8];
            value.copy_from_slice(&bytes[offset as usize..offset as usize + 8]);
            u64::from_le_bytes(value)
        };
        let mut latches = [0; 4];
        latches.copy_from_slice(&bytes[DEVICE_LATCHES as usize..DEVICE_LATCHES as usize + 4]);
        Ok(Self {
            device_reads: wide(DEVICE_READS),
            device_writes: wide(DEVICE_WRITES),
            device_changes: wide(DEVICE_CHANGES),
            latches,
            ram_writes: wide(RAM_WRITES),
            inline_reserved: u32::from_le_bytes(
                bytes[INLINE_RESERVED as usize..INLINE_RESERVED as usize + 4]
                    .try_into()
                    .expect("fixed reserved counter"),
            ),
            inline_remaining: u32::from_le_bytes(
                bytes[INLINE_GRANT as usize..INLINE_GRANT as usize + 4]
                    .try_into()
                    .expect("fixed grant"),
            ),
        })
    }
    pub fn clear(bytes: &mut [u8]) -> Result<(), super::FrameError> {
        let bytes = bytes.get_mut(..BYTES).ok_or(super::FrameError)?;
        bytes[DEVICE_READS as usize..DEVICE_READS as usize + 8].fill(0);
        bytes[RAM_WRITES as usize..RAM_WRITES as usize + 8].fill(0);
        bytes[DEVICE_WRITES as usize..DEVICE_WRITES as usize + 8].fill(0);
        bytes[DEVICE_CHANGES as usize..DEVICE_CHANGES as usize + 8].fill(0);
        bytes[INLINE_RESERVED as usize..INLINE_RESERVED as usize + 4].fill(0);
        Ok(())
    }
}

/// A runtime-verified source span. Valid sources occupy at most two physical
/// pages; their actual generations must still match before a region executes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegionGuard {
    pub valid: bool,
    pub pages: [u32; 2],
    pub generations: [u64; 2],
}
impl RegionGuard {
    pub fn write(self, bytes: &mut [u8]) -> Result<(), super::FrameError> {
        let bytes = bytes
            .get_mut(..REGION_GUARD_BYTES)
            .ok_or(super::FrameError)?;
        bytes.fill(0);
        bytes[..4].copy_from_slice(&u32::from(self.valid).to_le_bytes());
        bytes[4..8].copy_from_slice(&self.pages[0].to_le_bytes());
        bytes[8..12].copy_from_slice(&self.pages[1].to_le_bytes());
        bytes[16..24].copy_from_slice(&self.generations[0].to_le_bytes());
        bytes[24..32].copy_from_slice(&self.generations[1].to_le_bytes());
        Ok(())
    }
}
