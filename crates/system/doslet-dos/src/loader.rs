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
    if program.is_empty() {
        return Err(ComError::Empty);
    }
    if program.len() > MAX_COM_BYTES {
        return Err(ComError::TooLarge);
    }
    initialize_psp(bus, PSP_SEGMENT, PSP_SEGMENT + 0x1000)?;
    let base = real_address(PSP_SEGMENT, 0);
    for (offset, byte) in program.iter().enumerate() {
        bus.write_u8(base + 0x100 + offset as u32, *byte)?;
    }
    bus.write_u16(real_address(PSP_SEGMENT, 0xFFFE), 0)?;
    let mut registers = Registers::default();
    registers.cs = PSP_SEGMENT;
    registers.ds = PSP_SEGMENT;
    registers.es = PSP_SEGMENT;
    registers.ss = PSP_SEGMENT;
    registers.ip = 0x100;
    registers.set(Gpr16::Sp, 0xFFFE);
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
