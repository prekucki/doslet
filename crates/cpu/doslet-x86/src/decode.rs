use crate::{Access, CpuError, Gpr16, Gpr32, Registers, SegmentRegister};
use doslet_bus::{BusError, MemoryBus};

pub(crate) fn fetch<B: MemoryBus>(r: &mut Registers, bus: &mut B) -> Result<u8, CpuError> {
    let address = r.address(SegmentRegister::Cs, r.ip, 1, Access::Execute)?;
    let value = bus.read_u8(address)?;
    r.ip = if r.protected || r.code32() {
        r.ip.wrapping_add(1)
    } else {
        u32::from((r.ip as u16).wrapping_add(1))
    };
    Ok(value)
}
pub(crate) fn fetch_word<B: MemoryBus>(r: &mut Registers, bus: &mut B) -> Result<u16, CpuError> {
    let low = fetch(r, bus)?;
    let high = fetch(r, bus)?;
    Ok(u16::from_le_bytes([low, high]))
}
pub(crate) fn fetch_dword<B: MemoryBus>(r: &mut Registers, bus: &mut B) -> Result<u32, CpuError> {
    let low = fetch_word(r, bus)?;
    let high = fetch_word(r, bus)?;
    Ok(u32::from(low) | (u32::from(high) << 16))
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum Operand {
    Register(u8),
    Memory {
        segment: SegmentRegister,
        offset: u32,
    },
}
impl Operand {
    pub fn address(self, r: &Registers, length: u8, access: Access) -> Result<u32, CpuError> {
        match self {
            Self::Memory { segment, offset } => r.address(segment, offset, length, access),
            Self::Register(_) => unreachable!("memory operand required"),
        }
    }
    pub fn read32<B: MemoryBus>(self, r: &Registers, bus: &mut B) -> Result<u32, CpuError> {
        match self {
            Self::Register(code) => Ok(r.get32(Gpr32::decode(code))),
            _ => Ok(read_dword(bus, self.address(r, 4, Access::Read)?)?),
        }
    }
    pub fn write32<B: MemoryBus>(
        self,
        r: &mut Registers,
        bus: &mut B,
        value: u32,
    ) -> Result<(), CpuError> {
        match self {
            Self::Register(code) => r.set32(Gpr32::decode(code), value),
            _ => write_dword(bus, self.address(r, 4, Access::Write)?, value)?,
        }
        Ok(())
    }
    pub fn read<B: MemoryBus>(
        self,
        r: &Registers,
        bus: &mut B,
        word: bool,
    ) -> Result<u16, CpuError> {
        match self {
            Self::Register(code) => Ok(if word {
                r.get(Gpr16::decode(code))
            } else {
                u16::from(r.get_byte(code))
            }),
            _ => {
                let address = self.address(r, if word { 2 } else { 1 }, Access::Read)?;
                Ok(if word {
                    bus.read_u16(address)?
                } else {
                    u16::from(bus.read_u8(address)?)
                })
            }
        }
    }
    pub fn write<B: MemoryBus>(
        self,
        r: &mut Registers,
        bus: &mut B,
        word: bool,
        value: u16,
    ) -> Result<(), CpuError> {
        match self {
            Self::Register(code) => {
                if word {
                    r.set(Gpr16::decode(code), value);
                } else {
                    r.set_byte(code, value as u8);
                }
            }
            _ => {
                let address = self.address(r, if word { 2 } else { 1 }, Access::Write)?;
                if word {
                    bus.write_u16(address, value)?;
                } else {
                    bus.write_u8(address, value as u8)?;
                }
            }
        }
        Ok(())
    }
}
pub(crate) fn read_dword<B: MemoryBus>(bus: &mut B, address: u32) -> Result<u32, BusError> {
    bus.read_u32(address)
}
pub(crate) fn write_dword<B: MemoryBus>(
    bus: &mut B,
    address: u32,
    value: u32,
) -> Result<(), BusError> {
    bus.write_u32(address, value)
}
/// Decode all 16-bit addresses and 32-bit ModR/M/SIB addresses without allocating.
pub(crate) fn operand<B: MemoryBus>(
    r: &mut Registers,
    bus: &mut B,
    modrm: u8,
    segment_override: Option<SegmentRegister>,
    address32: bool,
) -> Result<Operand, CpuError> {
    let mode = modrm >> 6;
    let code = modrm & 7;
    if mode == 3 {
        return Ok(Operand::Register(code));
    }
    let (offset, stack) = if address32 {
        let (base, index, stack) = if code == 4 {
            let sib = fetch(r, bus)?;
            let base_code = sib & 7;
            let index_code = (sib >> 3) & 7;
            let index = if index_code == 4 {
                0
            } else {
                r.get32(Gpr32::decode(index_code))
                    .wrapping_shl(u32::from(sib >> 6))
            };
            if base_code == 5 && mode == 0 {
                (fetch_dword(r, bus)?, index, false)
            } else {
                (
                    r.get32(Gpr32::decode(base_code)),
                    index,
                    matches!(base_code, 4 | 5),
                )
            }
        } else if code == 5 && mode == 0 {
            (fetch_dword(r, bus)?, 0, false)
        } else {
            (r.get32(Gpr32::decode(code)), 0, code == 5)
        };
        let displacement = match mode {
            1 => fetch(r, bus)? as i8 as i32 as u32,
            2 => fetch_dword(r, bus)?,
            _ => 0,
        };
        (base.wrapping_add(index).wrapping_add(displacement), stack)
    } else {
        let bx = r.get(Gpr16::Bx);
        let bp = r.get(Gpr16::Bp);
        let si = r.get(Gpr16::Si);
        let di = r.get(Gpr16::Di);
        let (base, stack) = match code {
            0 => (bx.wrapping_add(si), false),
            1 => (bx.wrapping_add(di), false),
            2 => (bp.wrapping_add(si), true),
            3 => (bp.wrapping_add(di), true),
            4 => (si, false),
            5 => (di, false),
            6 if mode == 0 => (fetch_word(r, bus)?, false),
            6 => (bp, true),
            _ => (bx, false),
        };
        let displacement = match mode {
            1 => fetch(r, bus)? as i8 as i16 as u16,
            2 => fetch_word(r, bus)?,
            _ => 0,
        };
        (u32::from(base.wrapping_add(displacement)), stack)
    };
    Ok(Operand::Memory {
        segment: segment_override.unwrap_or(if stack {
            SegmentRegister::Ss
        } else {
            SegmentRegister::Ds
        }),
        offset,
    })
}
