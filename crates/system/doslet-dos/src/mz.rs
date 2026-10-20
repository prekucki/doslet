use crate::loader::initialize_psp;
use core::fmt;
use doslet_bus::{BusError, MemoryBus};
use doslet_executable::{ExecutableError, MzImage};
use doslet_x86::{Gpr16, Registers};

/// Caller-owned ordinary RAM available to this process, ending exclusively.
/// The caller must exclude devices, firmware and other reserved memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessMemory {
    pub psp_segment: u16,
    pub end_segment: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MzError {
    Executable(ExecutableError),
    Empty,
    InvalidMemoryRequest,
    InsufficientMemory,
    InvalidRelocation { index: u16 },
    InvalidEntry,
    InvalidStack,
    UnsupportedMode,
    Bus(BusError),
}
impl From<ExecutableError> for MzError {
    fn from(error: ExecutableError) -> Self {
        Self::Executable(error)
    }
}
impl From<BusError> for MzError {
    fn from(error: BusError) -> Self {
        Self::Bus(error)
    }
}
impl fmt::Display for MzError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Executable(error) => error.fmt(f),
            Self::Empty => f.write_str("MZ load module is empty"),
            Self::InvalidMemoryRequest => f.write_str("invalid MZ memory allocation request"),
            Self::InsufficientMemory => f.write_str("MZ process exceeds available guest RAM"),
            Self::InvalidRelocation { index } => {
                write!(f, "MZ relocation {index} targets outside the load module")
            }
            Self::InvalidEntry => {
                f.write_str("MZ entry is outside the load module or wraps a segment")
            }
            Self::InvalidStack => {
                f.write_str("MZ stack is outside the process or lacks an interrupt frame")
            }
            Self::UnsupportedMode => f.write_str("NE/PE/LE/LX execution is unsupported"),
            Self::Bus(error) => error.fmt(f),
        }
    }
}
impl core::error::Error for MzError {}

/// DOS EXEC/AL=03: load an MZ overlay without creating a PSP or changing registers.
/// The DOS caller supplies separate load and relocation segments.
pub fn load_overlay<B: MemoryBus>(
    bus: &mut B,
    bytes: &[u8],
    load_segment: u16,
    relocation_segment: u16,
    end_segment: u16,
) -> Result<(), MzError> {
    let image = MzImage::parse(bytes)?;
    if image.load_module.is_empty() {
        return Err(MzError::Empty);
    }
    let base = u32::from(load_segment) * 16;
    let available = end_segment
        .checked_sub(load_segment)
        .ok_or(MzError::InvalidMemoryRequest)?;
    if image.load_module.len() as u32 > u32::from(available) * 16 {
        return Err(MzError::InsufficientMemory);
    }
    for (index, relocation) in image.relocations().enumerate() {
        let target = usize::from(relocation.segment) * 16 + usize::from(relocation.offset);
        if target + 2 > image.load_module.len() {
            return Err(MzError::InvalidRelocation {
                index: index as u16,
            });
        }
    }
    for (offset, byte) in image.load_module.iter().enumerate() {
        bus.write_u8(base + offset as u32, *byte)?;
    }
    for relocation in image.relocations() {
        let address = base + u32::from(relocation.segment) * 16 + u32::from(relocation.offset);
        let word = bus.read_u16(address)?.wrapping_add(relocation_segment);
        bus.write_u16(address, word)?;
    }
    Ok(())
}

/// Load a standalone real-mode MZ image. Validate file/layout before any writes.
/// Bus failures during loading may leave partial writes; no RAM rollback is made.
pub fn load_mz<B: MemoryBus>(
    bus: &mut B,
    bytes: &[u8],
    memory: ProcessMemory,
) -> Result<Registers, MzError> {
    load_image(bus, bytes, memory, false)
}

/// Execute only the DOS MZ portion of an LE/LX file, as DOS EXEC initially does.
/// This does not load the extended image or provide an extender.
pub fn load_mz_stub<B: MemoryBus>(
    bus: &mut B,
    bytes: &[u8],
    memory: ProcessMemory,
) -> Result<Registers, MzError> {
    load_image(bus, bytes, memory, true)
}

fn load_image<B: MemoryBus>(
    bus: &mut B,
    bytes: &[u8],
    memory: ProcessMemory,
    allow_dos_stub: bool,
) -> Result<Registers, MzError> {
    let image = MzImage::parse(bytes)?;
    let header = image.header;
    // DOS EXEC ignores the overlay number; DJGPP's stub keeps text there.
    if image.extended.is_some() && !allow_dos_stub {
        return Err(MzError::UnsupportedMode);
    }
    // The inspector exposes LE/LX metadata; other known extended formats are
    // also rejected here rather than implying their DOS stub is supported.
    if header.header_bytes >= 64
        && (header.relocation_count == 0 || u16::from_le_bytes([bytes[24], bytes[25]]) >= 64)
    {
        let offset = u32::from_le_bytes(bytes[60..64].try_into().unwrap()) as usize;
        if matches!(
            bytes.get(offset..offset.saturating_add(2)),
            Some(b"NE" | b"PE")
        ) {
            return Err(MzError::UnsupportedMode);
        }
    }
    if image.load_module.is_empty() {
        return Err(MzError::Empty);
    }
    let available = memory
        .end_segment
        .checked_sub(memory.psp_segment)
        .ok_or(MzError::InvalidMemoryRequest)?;
    if header.maximum_extra_paragraphs != 0
        && header.maximum_extra_paragraphs < header.minimum_extra_paragraphs
    {
        return Err(MzError::InvalidMemoryRequest);
    }
    let module_paragraphs = image.load_module.len().div_ceil(16) as u32;
    let minimum = 16 + module_paragraphs + u32::from(header.minimum_extra_paragraphs);
    if minimum > u32::from(available) {
        return Err(MzError::InsufficientMemory);
    }
    let allocated = if header.maximum_extra_paragraphs == 0 {
        u32::from(available)
    } else {
        u32::from(available)
            .min(16 + module_paragraphs + u32::from(header.maximum_extra_paragraphs))
    };
    let end_segment = memory.psp_segment + allocated as u16;
    let load_segment =
        if header.minimum_extra_paragraphs == 0 && header.maximum_extra_paragraphs == 0 {
            // DOS loads these images at the high end of the allocated block.
            end_segment - module_paragraphs as u16
        } else {
            memory.psp_segment + 16
        };
    let base = u32::from(load_segment) * 16;
    let end = u32::from(end_segment) * 16;
    let cs = load_segment
        .checked_add(header.initial_cs)
        .ok_or(MzError::InvalidEntry)?;
    let entry = u32::from(cs) * 16 + u32::from(header.initial_ip);
    if entry < base || entry >= base + image.load_module.len() as u32 {
        return Err(MzError::InvalidEntry);
    }
    let ss = load_segment
        .checked_add(header.initial_ss)
        .ok_or(MzError::InvalidStack)?;
    let stack_offset = if header.initial_sp == 0 {
        0x10000
    } else {
        u32::from(header.initial_sp)
    };
    let stack_end = u32::from(ss) * 16 + stack_offset;
    if stack_offset < 6 || stack_end > end || stack_end - 6 < base {
        return Err(MzError::InvalidStack);
    }
    for (index, relocation) in image.relocations().enumerate() {
        let target = usize::from(relocation.segment) * 16 + usize::from(relocation.offset);
        if target + 2 > image.load_module.len() {
            return Err(MzError::InvalidRelocation {
                index: index as u16,
            });
        }
    }

    initialize_psp(bus, memory.psp_segment, end_segment)?;
    for (offset, byte) in image.load_module.iter().enumerate() {
        bus.write_u8(base + offset as u32, *byte)?;
    }
    for address in base + image.load_module.len() as u32..end {
        bus.write_u8(address, 0)?;
    }
    for relocation in image.relocations() {
        let target = base + u32::from(relocation.segment) * 16 + u32::from(relocation.offset);
        let value = bus.read_u16(target)?.wrapping_add(load_segment);
        bus.write_u16(target, value)?;
    }
    let mut registers = Registers::default();
    registers.cs = cs;
    registers.ip = u32::from(header.initial_ip);
    registers.ss = ss;
    registers.set(Gpr16::Sp, header.initial_sp);
    registers.ds = memory.psp_segment;
    registers.es = memory.psp_segment;
    Ok(registers)
}
