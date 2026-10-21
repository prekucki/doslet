use crate::mz::ProcessMemory;
use core::fmt;
use doslet_bus::{BusError, MemoryBus};
use doslet_x86::{Gpr16, Registers, real_address};

pub const PSP_SEGMENT: u16 = 0x1000;
pub const MAX_COM_BYTES: usize = 0xFEFE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComError {
    Empty,
    TooLarge,
    Bus(BusError),
}
impl From<BusError> for ComError {
    fn from(error: BusError) -> Self {
        Self::Bus(error)
    }
}
impl fmt::Display for ComError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("COM program is empty"),
            Self::TooLarge => {
                f.write_str("COM program exceeds the segment's code and stack capacity")
            }
            Self::Bus(error) => error.fmt(f),
        }
    }
}
impl core::error::Error for ComError {}

pub fn load_com<B: MemoryBus>(bus: &mut B, program: &[u8]) -> Result<Registers, ComError> {
    load_com_at(
        bus,
        program,
        ProcessMemory {
            psp_segment: PSP_SEGMENT,
            end_segment: PSP_SEGMENT + 0x1000,
        },
    )
}

/// Load a COM program whose PSP starts the block up to `end_segment`, as DOS
/// EXEC does. The stack starts at the top of the 64-KiB segment, or of the
/// block when that is smaller.
pub fn load_com_at<B: MemoryBus>(
    bus: &mut B,
    program: &[u8],
    memory: ProcessMemory,
) -> Result<Registers, ComError> {
    if program.is_empty() {
        return Err(ComError::Empty);
    }
    let psp = memory.psp_segment;
    let bytes = u32::from(memory.end_segment.saturating_sub(psp)).min(0x1000) * 16;
    if program.len() > MAX_COM_BYTES || 0x100 + program.len() as u32 + 2 > bytes {
        return Err(ComError::TooLarge);
    }
    initialize_psp(bus, psp, memory.end_segment)?;
    let base = real_address(psp, 0);
    for (offset, byte) in program.iter().enumerate() {
        bus.write_u8(base + 0x100 + offset as u32, *byte)?;
    }
    let sp = (bytes - 2) as u16;
    bus.write_u16(real_address(psp, sp), 0)?;
    let mut registers = Registers::default();
    registers.cs = psp;
    registers.ds = psp;
    registers.es = psp;
    registers.ss = psp;
    registers.ip = 0x100;
    registers.set(Gpr16::Sp, sp);
    Ok(registers)
}

pub(crate) fn initialize_psp<B: MemoryBus>(
    bus: &mut B,
    segment: u16,
    end_segment: u16,
) -> Result<(), BusError> {
    let base = real_address(segment, 0);
    for offset in 0..256 {
        bus.write_u8(base + offset, 0)?;
    }
    bus.write_u8(base, 0xCD)?;
    bus.write_u8(base + 1, 0x20)?;
    bus.write_u16(base + 2, end_segment)?;
    bus.write_u8(base + 0x81, 0x0D)?;
    Ok(())
}
