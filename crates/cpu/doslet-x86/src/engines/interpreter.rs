use crate::ControlRegisters;
use crate::{Access, SegmentRegister};
use crate::{Gpr16, Gpr32, Registers, decode, flags};
use core::fmt;
use doslet_bus::{BusError, CpuBus, MemoryBus};
mod cache;
mod operand32;
mod segments;
mod x87;
#[cfg(feature = "profiling")]
pub use cache::DecodeCacheStats;
pub use segments::DescriptorError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepOutcome {
    Continue,
    Halted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpuError {
    X87Unsupported {
        cs: u16,
        ip: u32,
        opcode: u8,
        modrm: u8,
        instruction: crate::x87_decode::Instruction,
        later_isa: bool,
    },
    X87Reserved {
        cs: u16,
        ip: u32,
        opcode: u8,
        modrm: u8,
    },
    X87Arithmetic {
        cs: u16,
        ip: u32,
        reason: doslet_x87::ArithmeticError,
    },
    X87Stack {
        cs: u16,
        ip: u32,
        reason: doslet_x87::StackError,
    },
    SegmentAccess {
        segment: SegmentRegister,
        selector: u16,
        offset: u32,
        length: u8,
        access: Access,
    },
    Descriptor {
        selector: u16,
        reason: DescriptorError,
    },
    PagingUnavailable {
        cs: u16,
        ip: u32,
    },
    PrivilegeChangeUnavailable {
        selector: u16,
    },
    Virtual8086Unavailable,
    X87Unavailable {
        cs: u16,
        ip: u32,
    },
    Bus(BusError),
    UnsupportedOpcode {
        cs: u16,
        ip: u32,
        opcode: u8,
    },
    UnsupportedOperand {
        cs: u16,
        ip: u32,
        opcode: u8,
        modrm: u8,
    },
    InstructionTooLong {
        cs: u16,
        ip: u32,
    },
    DivideError {
        cs: u16,
        ip: u32,
    },
    UnsupportedControlFlow {
        cs: u16,
        ip: u32,
        target: u32,
    },
    UnsupportedExtendedOpcode {
        cs: u16,
        ip: u32,
        opcode: u8,
    },
    ProtectedModeUnavailable {
        cs: u16,
        ip: u32,
    },
}

impl From<BusError> for CpuError {
    fn from(value: BusError) -> Self {
        Self::Bus(value)
    }
}
impl fmt::Display for CpuError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::X87Unsupported {
                cs,
                ip,
                opcode,
                modrm,
                instruction,
                later_isa,
            } => {
                write!(
                    f,
                    "unsupported x87 {:?} {:?} ({opcode:02X} {modrm:02X}) at {cs:04X}:{ip:08X}",
                    instruction.mnemonic, instruction.operands
                )?;
                if *later_isa {
                    write!(f, " (requires an ISA newer than 80387)")?;
                }
                Ok(())
            }
            Self::X87Reserved {
                cs,
                ip,
                opcode,
                modrm,
            } => {
                write!(
                    f,
                    "reserved x87 encoding {opcode:02X} {modrm:02X} at {cs:04X}:{ip:08X}"
                )
            }
            Self::X87Arithmetic { cs, ip, reason } => {
                write!(f, "x87 arithmetic {reason:?} at {cs:04X}:{ip:08X}")
            }
            Self::X87Stack { cs, ip, reason } => {
                write!(f, "x87 stack {reason:?} at {cs:04X}:{ip:08X}")
            }
            Self::SegmentAccess {
                segment,
                selector,
                offset,
                length,
                access,
            } => write!(
                f,
                "segment {segment:?}={selector:04X} rejects {access:?} at {offset:#x} ({length} bytes)"
            ),
            Self::Descriptor { selector, reason } => {
                write!(f, "descriptor {selector:04X}: {reason:?}")
            }
            Self::PagingUnavailable { cs, ip } => {
                write!(f, "paging is not implemented at {cs:04X}:{ip:08X}")
            }
            Self::PrivilegeChangeUnavailable { selector } => write!(
                f,
                "privilege transition to selector {selector:04X} is not implemented"
            ),
            Self::Virtual8086Unavailable => {
                f.write_str("virtual-8086 execution is not implemented")
            }
            Self::Bus(error) => error.fmt(f),
            Self::X87Unavailable { cs, ip } => write!(
                f,
                "x87 is absent or blocked by CR0.EM/TS at {cs:04X}:{ip:08X}"
            ),
            Self::UnsupportedOpcode { cs, ip, opcode } => {
                write!(f, "unsupported opcode {opcode:02X} at {cs:04X}:{ip:04X}")
            }
            Self::UnsupportedOperand {
                cs,
                ip,
                opcode,
                modrm,
            } => write!(
                f,
                "unsupported operand {modrm:02X} for opcode {opcode:02X} at {cs:04X}:{ip:04X}"
            ),
            Self::InstructionTooLong { cs, ip } => {
                write!(f, "instruction exceeds 15 bytes at {cs:04X}:{ip:04X}")
            }
            Self::DivideError { cs, ip } => write!(f, "divide error at {cs:04X}:{ip:04X}"),
            Self::UnsupportedControlFlow { cs, ip, target } => write!(
                f,
                "control-flow target {target:#x} exceeds the real-mode code limit at {cs:04X}:{ip:04X}"
            ),
            Self::UnsupportedExtendedOpcode { cs, ip, opcode } => {
                write!(f, "unsupported opcode 0F {opcode:02X} at {cs:04X}:{ip:04X}")
            }
            Self::ProtectedModeUnavailable { cs, ip } => write!(
                f,
                "protected-mode execution is not implemented at {cs:04X}:{ip:04X}"
            ),
        }
    }
}
impl core::error::Error for CpuError {}

#[derive(Debug)]
pub struct Cpu {
    pub registers: Registers,
    pub flags: u32,
    pub control: ControlRegisters,
    pub fpu: Option<doslet_x87::X87>,
    halted: bool,
    interrupt_shadow: u8,
    decode_cache: cache::Cache,
}

impl Default for Cpu {
    fn default() -> Self {
        Self {
            registers: Registers::default(),
            flags: 2 | flags::INTERRUPT,
            control: ControlRegisters::default(),
            fpu: None,
            halted: false,
            interrupt_shadow: 0,
            decode_cache: cache::Cache::default(),
        }
    }
}

impl Cpu {
    /// Keep the original byte-fetching interpreter available as a reference.
    pub fn set_decode_cache(&mut self, enabled: bool) {
        self.decode_cache.clear();
        self.decode_cache.enabled = enabled;
    }
    #[cfg(feature = "profiling")]
    pub fn decode_cache_stats(&self) -> DecodeCacheStats {
        self.decode_cache.stats
    }
    pub fn with_x87(mut self) -> Self {
        self.fpu = Some(doslet_x87::X87::new());
        self.control.cr0 &= !4;
        self
    }
    pub fn is_halted(&self) -> bool {
        self.halted
    }
    pub fn accepts_maskable_interrupt(&self) -> bool {
        self.flags & flags::INTERRUPT != 0 && self.interrupt_shadow == 0
    }
    /// An STI or SS reload still inhibits the next instruction boundary.
    pub fn has_interrupt_shadow(&self) -> bool {
        self.interrupt_shadow != 0
    }

    /// One bounded execution step. REP STOSB performs at most one write per step.
    /// Errors stop execution; precise guest exception delivery is not implemented yet.
    pub fn step<B: CpuBus>(&mut self, bus: &mut B) -> Result<StepOutcome, CpuError> {
        if self.halted {
            return Ok(StepOutcome::Halted);
        }
        self.interrupt_shadow = self.interrupt_shadow.saturating_sub(1);
        let cs = self.registers.cs;
        let ip = self.registers.ip;
        if self.control.cr0 & 0x80000000 != 0 {
            return Err(CpuError::PagingUnavailable { cs, ip });
        }
        // CR0 is public, so compare at every step but resynchronise only on PE changes.
        let protected = self.protected_mode();
        if self.registers.protected != protected {
            self.registers.sync_mode(protected);
        }
        if let Some(result) = self.cached_step(bus) {
            return result;
        }
        #[cfg(feature = "profiling")]
        {
            self.decode_cache.stats.fallbacks += 1;
        }
        let mut repeat = false;
        let mut repeat_zero = true;
        let default32 = self.registers.code32();
        let mut operand_32 = default32;
        let mut address_32 = default32;
        let mut segment = SegmentRegister::Ds;
        let mut segment_override = None;
        let mut opcode = None;
        for _ in 0..15 {
            let byte = decode::fetch(&mut self.registers, bus)?;
            match byte {
                0x66 => operand_32 = !default32,
                0x67 => address_32 = !default32,
                0xF2 | 0xF3 => {
                    repeat = true;
                    repeat_zero = byte == 0xF3;
                }
                0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 => {
                    segment = match byte {
                        0x26 => SegmentRegister::Es,
                        0x2E => SegmentRegister::Cs,
                        0x36 => SegmentRegister::Ss,
                        0x64 => SegmentRegister::Fs,
                        0x65 => SegmentRegister::Gs,
                        _ => SegmentRegister::Ds,
                    };
                    segment_override = Some(segment);
                }
                _ => {
                    opcode = Some(byte);
                    break;
                }
            }
        }
        let opcode = opcode.ok_or(CpuError::InstructionTooLong { cs, ip })?;
        let context = operand32::Context {
            cs,
            ip,
            segment,
            segment_override,
            repeat,
            repeat_zero,
            address_32,
        };
        if opcode == 0x0F {
            return self.extended_step(bus, operand_32, context);
        }
        if operand_32 && operand32::handles(opcode) {
            return self.dword_step(bus, opcode, context);
        }
        match opcode {
            0x27 | 0x2F => {
                let old = self.registers.get_byte(0);
                let old_carry = self.flags & flags::CARRY != 0;
                let low = old & 15 > 9 || self.flags & flags::AUXILIARY != 0;
                let subtract = opcode == 0x2F;
                let mut value = old;
                let mut carry = false;
                if low {
                    value = if subtract {
                        old.wrapping_sub(6)
                    } else {
                        old.wrapping_add(6)
                    };
                    carry = old_carry || if subtract { old < 6 } else { old > 249 };
                }
                if old > 0x99 || old_carry {
                    value = if subtract {
                        value.wrapping_sub(0x60)
                    } else {
                        value.wrapping_add(0x60)
                    };
                    carry = true;
                } else if !subtract {
                    carry = false;
                }
                let mask =
                    flags::CARRY | flags::AUXILIARY | flags::SIGN | flags::ZERO | flags::PARITY;
                self.flags = (self.flags & !mask)
                    | if carry { flags::CARRY } else { 0 }
                    | if low { flags::AUXILIARY } else { 0 }
                    | if value == 0 { flags::ZERO } else { 0 }
                    | if value & 0x80 != 0 { flags::SIGN } else { 0 }
                    | if value.count_ones().is_multiple_of(2) {
                        flags::PARITY
                    } else {
                        0
                    };
                self.registers.set_byte(0, value);
            }
            0x6C..=0x6F => {
                let count = if address_32 {
                    self.registers.get32(Gpr32::Ecx)
                } else {
                    u32::from(self.registers.get(Gpr16::Cx))
                };
                if !repeat || count != 0 {
                    let input = opcode < 0x6E;
                    let width = if opcode & 1 == 0 {
                        1
                    } else if operand_32 {
                        4
                    } else {
                        2
                    };
                    let reg = if input { Gpr32::Edi } else { Gpr32::Esi };
                    let offset = if address_32 {
                        self.registers.get32(reg)
                    } else {
                        u32::from(self.registers.get(Gpr16::decode(reg as u8)))
                    };
                    let address = self.registers.address(
                        if input { SegmentRegister::Es } else { segment },
                        offset,
                        width,
                        if input { Access::Write } else { Access::Read },
                    )?;
                    let port = self.registers.get(Gpr16::Dx);
                    if input {
                        match width {
                            1 => {
                                let value = bus.read_port_u8(port)?;
                                bus.write_u8(address, value)?;
                            }
                            2 => {
                                let value = bus.read_port_u16(port)?;
                                bus.write_u16(address, value)?;
                            }
                            _ => {
                                let value = bus.read_port_u32(port)?;
                                bus.write_u32(address, value)?;
                            }
                        }
                    } else {
                        match width {
                            1 => {
                                let value = bus.read_u8(address)?;
                                bus.write_port_u8(port, value)?;
                            }
                            2 => {
                                let value = bus.read_u16(address)?;
                                bus.write_port_u16(port, value)?;
                            }
                            _ => {
                                let value = bus.read_u32(address)?;
                                bus.write_port_u32(port, value)?;
                            }
                        }
                    }
                    let delta = u32::from(width);
                    let next = if self.flags & flags::DIRECTION != 0 {
                        offset.wrapping_sub(delta)
                    } else {
                        offset.wrapping_add(delta)
                    };
                    if address_32 {
                        self.registers.set32(reg, next);
                    } else {
                        self.registers.set(Gpr16::decode(reg as u8), next as u16);
                    }
                    if repeat {
                        if address_32 {
                            self.registers.set32(Gpr32::Ecx, count - 1);
                        } else {
                            self.registers.set(Gpr16::Cx, (count - 1) as u16);
                        }
                        if count > 1 {
                            self.registers.ip = ip;
                        }
                    }
                }
            }
            0xD7 => {
                let index = u32::from(self.registers.get_byte(0));
                let offset = if address_32 {
                    self.registers.get32(Gpr32::Ebx).wrapping_add(index)
                } else {
                    u32::from(self.registers.get(Gpr16::Bx).wrapping_add(index as u16))
                };
                let value = decode::Operand::Memory { segment, offset }.read(
                    &self.registers,
                    bus,
                    false,
                )?;
                self.registers.set_byte(0, value as u8);
            }
            0xD8..=0xDF => return self.x87_step(bus, opcode, context, operand_32),
            0x9B => {
                if self.fpu.is_none() || self.control.cr0 & 0x0A == 0x0A {
                    return Err(CpuError::X87Unavailable { cs, ip });
                }
            }
            0x90..=0x97 => {
                let register = Gpr16::decode(opcode - 0x90);
                let value = self.registers.get(register);
                self.registers.set(register, self.registers.get(Gpr16::Ax));
                self.registers.set(Gpr16::Ax, value);
            }
            0x98 => {
                let value = self.registers.get_byte(0) as i8 as i16 as u16;
                self.registers.set(Gpr16::Ax, value);
            }
            0x99 => {
                self.registers.set(
                    Gpr16::Dx,
                    if self.registers.get(Gpr16::Ax) & 0x8000 != 0 {
                        0xFFFF
                    } else {
                        0
                    },
                );
            }
            0xB8..=0xBF => {
                let value = decode::fetch_word(&mut self.registers, bus)?;
                self.registers.set(Gpr16::decode(opcode - 0xB8), value);
            }
            0xB0..=0xB7 => {
                let value = decode::fetch(&mut self.registers, bus)?;
                self.registers.set_byte(opcode - 0xB0, value);
            }
            0x88..=0x8F | 0xC6 | 0xC7 => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let target = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    segment_override,
                    address_32,
                )?;
                let source = decode::Operand::Register((modrm >> 3) & 7);
                let word = opcode & 1 != 0;
                match opcode {
                    0x88 | 0x89 => {
                        let value = source.read(&self.registers, bus, word)?;
                        target.write(&mut self.registers, bus, word, value)?;
                    }
                    0x8A | 0x8B => {
                        let value = target.read(&self.registers, bus, word)?;
                        source.write(&mut self.registers, bus, word, value)?;
                    }
                    0x8C => {
                        let value = match (modrm >> 3) & 7 {
                            0 => self.registers.es,
                            1 => self.registers.cs,
                            2 => self.registers.ss,
                            3 => self.registers.ds,
                            4 => self.registers.fs,
                            5 => self.registers.gs,
                            _ => {
                                return Err(CpuError::UnsupportedOperand {
                                    cs,
                                    ip,
                                    opcode,
                                    modrm,
                                });
                            }
                        };
                        target.write(&mut self.registers, bus, true, value)?;
                    }
                    0x8D => {
                        let decode::Operand::Memory { offset, .. } = target else {
                            return Err(CpuError::UnsupportedOperand {
                                cs,
                                ip,
                                opcode,
                                modrm,
                            });
                        };
                        source.write(&mut self.registers, bus, true, offset as u16)?;
                    }
                    0x8E => {
                        let value = target.read(&self.registers, bus, true)?;
                        let segment = SegmentRegister::decode((modrm >> 3) & 7)
                            .filter(|segment| *segment != SegmentRegister::Cs)
                            .ok_or(CpuError::UnsupportedOperand {
                                cs,
                                ip,
                                opcode,
                                modrm,
                            })?;
                        self.load_segment(bus, segment, value)?;
                    }
                    0x8F if modrm & 0x38 == 0 => {
                        let value = self.pop(bus)?;
                        target.write(&mut self.registers, bus, true, value)?;
                    }
                    0xC6 | 0xC7 if modrm & 0x38 == 0 => {
                        let value = if word {
                            decode::fetch_word(&mut self.registers, bus)?
                        } else {
                            u16::from(decode::fetch(&mut self.registers, bus)?)
                        };
                        target.write(&mut self.registers, bus, word, value)?;
                    }
                    _ => {
                        return Err(CpuError::UnsupportedOperand {
                            cs,
                            ip,
                            opcode,
                            modrm,
                        });
                    }
                }
            }
            code if code <= 0x3D && code & 7 <= 5 => {
                let word = code & 1 != 0;
                let operation = code >> 3;
                let (target, left, right) = if code & 7 >= 4 {
                    let target = decode::Operand::Register(0);
                    let left = target.read(&self.registers, bus, word)?;
                    let right = if word {
                        decode::fetch_word(&mut self.registers, bus)?
                    } else {
                        u16::from(decode::fetch(&mut self.registers, bus)?)
                    };
                    (target, left, right)
                } else {
                    let modrm = decode::fetch(&mut self.registers, bus)?;
                    let rm = decode::operand(
                        &mut self.registers,
                        bus,
                        modrm,
                        segment_override,
                        address_32,
                    )?;
                    let reg = decode::Operand::Register((modrm >> 3) & 7);
                    let (target, source) = if code & 2 != 0 { (reg, rm) } else { (rm, reg) };
                    (
                        target,
                        target.read(&self.registers, bus, word)?,
                        source.read(&self.registers, bus, word)?,
                    )
                };
                let (value, next_flags) = flags::alu(self.flags, left, right, operation, word);
                self.flags = next_flags;
                if operation != 7 {
                    target.write(&mut self.registers, bus, word, value)?;
                }
            }
            0x80 | 0x81 | 0x83 | 0x84 | 0x85 | 0x86 | 0x87 => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let target = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    segment_override,
                    address_32,
                )?;
                let word = opcode & 1 != 0;
                let source = decode::Operand::Register((modrm >> 3) & 7);
                let right = match opcode {
                    0x80 => u16::from(decode::fetch(&mut self.registers, bus)?),
                    0x81 => decode::fetch_word(&mut self.registers, bus)?,
                    0x83 => decode::fetch(&mut self.registers, bus)? as i8 as i16 as u16,
                    _ => source.read(&self.registers, bus, word)?,
                };
                let left = target.read(&self.registers, bus, word)?;
                if opcode >= 0x86 {
                    target.write(&mut self.registers, bus, word, right)?;
                    source.write(&mut self.registers, bus, word, left)?;
                } else {
                    let operation = if opcode >= 0x84 { 4 } else { (modrm >> 3) & 7 };
                    let (value, next_flags) = flags::alu(self.flags, left, right, operation, word);
                    self.flags = next_flags;
                    if operation != 7 && opcode < 0x84 {
                        target.write(&mut self.registers, bus, word, value)?;
                    }
                }
            }
            0x40..=0x4F => {
                let register = Gpr16::decode(opcode);
                let before = self.registers.get(register);
                let (value, next_flags) = if opcode < 0x48 {
                    flags::add(self.flags, before, 1)
                } else {
                    flags::sub(self.flags, before, 1)
                };
                self.flags = (next_flags & !flags::CARRY) | (self.flags & flags::CARRY);
                self.registers.set(register, value);
            }
            0xC0 | 0xC1 | 0xD0..=0xD3 => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    segment_override,
                    address_32,
                )?;
                let count = match opcode {
                    0xC0 | 0xC1 => decode::fetch(&mut self.registers, bus)?,
                    0xD2 | 0xD3 => self.registers.get_byte(1),
                    _ => 1,
                } & 31;
                if count != 0 {
                    let word = opcode & 1 != 0;
                    let mask = if word { 0xFFFF } else { 0xFF };
                    let sign = if word { 0x8000 } else { 0x80 };
                    let bits = if word { 16 } else { 8 };
                    let operation = (modrm >> 3) & 7;
                    let original = operand.read(&self.registers, bus, word)?;
                    let mut value = original;
                    let mut carry = self.flags & flags::CARRY != 0;
                    for _ in 0..count {
                        let previous_carry = carry;
                        if matches!(operation, 0 | 2 | 4 | 6) {
                            carry = value & sign != 0;
                            value = (value << 1) & mask;
                            if operation == 0 {
                                value |= u16::from(carry);
                            }
                            if operation == 2 {
                                value |= u16::from(previous_carry);
                            }
                        } else {
                            carry = value & 1 != 0;
                            let high = value & sign;
                            value >>= 1;
                            if operation == 1 {
                                value |= u16::from(carry) << (bits - 1);
                            }
                            if operation == 3 {
                                value |= u16::from(previous_carry) << (bits - 1);
                            }
                            if operation == 7 {
                                value |= high;
                            }
                        }
                    }
                    if operation >= 4 {
                        let old_overflow = self.flags & flags::OVERFLOW;
                        self.flags = flags::alu(self.flags, value, 0, 1, word).1 | old_overflow;
                    }
                    self.flags = (self.flags & !flags::CARRY) | u32::from(carry);
                    if count == 1 {
                        let overflow = match operation {
                            0 | 2 | 4 | 6 => (value & sign != 0) != carry,
                            1 | 3 => ((value >> (bits - 1)) ^ (value >> (bits - 2))) & 1 != 0,
                            5 => original & sign != 0,
                            _ => false,
                        };
                        self.flags = (self.flags & !flags::OVERFLOW)
                            | if overflow { flags::OVERFLOW } else { 0 };
                    }
                    operand.write(&mut self.registers, bus, word, value)?;
                }
            }
            0xC4 | 0xC5 => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    segment_override,
                    address_32,
                )?;
                let decode::Operand::Memory { segment, offset } = operand else {
                    return Err(CpuError::UnsupportedOperand {
                        cs,
                        ip,
                        opcode,
                        modrm,
                    });
                };
                let address = self.registers.address(segment, offset, 4, Access::Read)?;
                let value = read_word(bus, address)?;
                let selector = read_word(bus, address + 2)?;
                self.registers.set(Gpr16::decode(modrm >> 3), value);
                if opcode == 0xC4 {
                    self.load_segment(bus, SegmentRegister::Es, selector)?;
                } else {
                    self.load_segment(bus, SegmentRegister::Ds, selector)?;
                }
            }
            0x50..=0x57 => {
                let value = self.registers.get(Gpr16::decode(opcode));
                self.push(bus, value)?;
            }
            0x58..=0x5F => {
                let value = self.pop(bus)?;
                self.registers.set(Gpr16::decode(opcode), value);
            }
            0xA0..=0xA3 => {
                let offset = if address_32 {
                    decode::fetch_dword(&mut self.registers, bus)?
                } else {
                    u32::from(decode::fetch_word(&mut self.registers, bus)?)
                };
                let operand = decode::Operand::Memory { segment, offset };
                let word = opcode & 1 != 0;
                if opcode < 0xA2 {
                    let value = operand.read(&self.registers, bus, word)?;
                    if word {
                        self.registers.set(Gpr16::Ax, value);
                    } else {
                        self.registers.set_byte(0, value as u8);
                    }
                } else {
                    let value = if word {
                        self.registers.get(Gpr16::Ax)
                    } else {
                        u16::from(self.registers.get_byte(0))
                    };
                    operand.write(&mut self.registers, bus, word, value)?;
                }
            }
            0xA8 | 0xA9 => {
                let word = opcode & 1 != 0;
                let right = if word {
                    decode::fetch_word(&mut self.registers, bus)?
                } else {
                    u16::from(decode::fetch(&mut self.registers, bus)?)
                };
                let left = decode::Operand::Register(0).read(&self.registers, bus, word)?;
                self.flags = flags::alu(self.flags, left, right, 4, word).1;
            }
            0xA4..=0xA7 | 0xAA..=0xAF => {
                let count = self.address_register(Gpr16::Cx, address_32);
                if !repeat || count != 0 {
                    let word = opcode & 1 != 0;
                    let si = self.address_register(Gpr16::Si, address_32);
                    let di = self.address_register(Gpr16::Di, address_32);
                    let source = decode::Operand::Memory {
                        segment,
                        offset: si,
                    };
                    let target = decode::Operand::Memory {
                        segment: SegmentRegister::Es,
                        offset: di,
                    };
                    let accumulator = decode::Operand::Register(0);
                    match opcode & 0xFE {
                        0xA4 => {
                            let value = source.read(&self.registers, bus, word)?;
                            target.write(&mut self.registers, bus, word, value)?;
                        }
                        0xA6 => {
                            let left = source.read(&self.registers, bus, word)?;
                            let right = target.read(&self.registers, bus, word)?;
                            self.flags = flags::alu(self.flags, left, right, 7, word).1;
                        }
                        0xAA => {
                            let value = accumulator.read(&self.registers, bus, word)?;
                            target.write(&mut self.registers, bus, word, value)?;
                        }
                        0xAC => {
                            let value = source.read(&self.registers, bus, word)?;
                            accumulator.write(&mut self.registers, bus, word, value)?;
                        }
                        _ => {
                            let left = accumulator.read(&self.registers, bus, word)?;
                            let right = target.read(&self.registers, bus, word)?;
                            self.flags = flags::alu(self.flags, left, right, 7, word).1;
                        }
                    }
                    let amount = if word { 2 } else { 1 };
                    let displacement = if self.flags & flags::DIRECTION == 0 {
                        amount
                    } else {
                        -amount
                    };
                    if opcode <= 0xA7 || matches!(opcode, 0xAC | 0xAD) {
                        self.set_address_register(
                            Gpr16::Si,
                            si.wrapping_add_signed(displacement),
                            address_32,
                        );
                    }
                    if !matches!(opcode, 0xAC | 0xAD) {
                        self.set_address_register(
                            Gpr16::Di,
                            di.wrapping_add_signed(displacement),
                            address_32,
                        );
                    }
                    if repeat {
                        self.set_address_register(Gpr16::Cx, count - 1, address_32);
                        let compare = matches!(opcode, 0xA6 | 0xA7 | 0xAE | 0xAF);
                        if count > 1 && (!compare || (self.flags & flags::ZERO != 0) == repeat_zero)
                        {
                            self.registers.ip = ip;
                        }
                    }
                }
            }
            0xF6 | 0xF7 | 0xFE | 0xFF => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    segment_override,
                    address_32,
                )?;
                let word = opcode & 1 != 0;
                let operation = (modrm >> 3) & 7;
                let value = operand.read(&self.registers, bus, word)?;
                match (opcode, operation) {
                    (0xFE | 0xFF, 0 | 1) => {
                        let (value, flags) = flags::alu(
                            self.flags,
                            value,
                            1,
                            if operation == 0 { 0 } else { 5 },
                            word,
                        );
                        self.flags = (flags & !flags::CARRY) | (self.flags & flags::CARRY);
                        operand.write(&mut self.registers, bus, word, value)?;
                    }
                    (0xFF, 2 | 4) => {
                        if operation == 2 {
                            self.push(bus, self.registers.ip as u16)?;
                        }
                        self.near_jump(u32::from(value))?;
                    }
                    (0xFF, 3 | 5) => {
                        let decode::Operand::Memory { segment, offset } = operand else {
                            return Err(CpuError::UnsupportedOperand {
                                cs,
                                ip,
                                opcode,
                                modrm,
                            });
                        };
                        let selector = read_word(
                            bus,
                            self.registers.address(segment, offset, 4, Access::Read)? + 2,
                        )?;
                        if operation == 3 {
                            self.push(bus, self.registers.cs)?;
                            self.push(bus, self.registers.ip as u16)?;
                        }
                        self.far_jump(bus, selector, u32::from(value))?;
                    }
                    (0xFF, 6) => self.push(bus, value)?,
                    (0xF6 | 0xF7, 0) => {
                        let right = if word {
                            decode::fetch_word(&mut self.registers, bus)?
                        } else {
                            u16::from(decode::fetch(&mut self.registers, bus)?)
                        };
                        self.flags = flags::alu(self.flags, value, right, 4, word).1;
                    }
                    (0xF6 | 0xF7, 2) => operand.write(&mut self.registers, bus, word, !value)?,
                    (0xF6 | 0xF7, 3) => {
                        let (value, flags) = flags::alu(self.flags, 0, value, 5, word);
                        self.flags = flags;
                        operand.write(&mut self.registers, bus, word, value)?;
                    }
                    (0xF6 | 0xF7, 4 | 5) => {
                        let ax = self.registers.get(Gpr16::Ax);
                        let product = if operation == 4 {
                            if word {
                                u32::from(ax) * u32::from(value)
                            } else {
                                u32::from(ax as u8) * u32::from(value)
                            }
                        } else if word {
                            (i32::from(ax as i16) * i32::from(value as i16)) as u32
                        } else {
                            (i32::from(ax as u8 as i8) * i32::from(value as u8 as i8)) as u32
                        };
                        let overflow = if operation == 4 {
                            product > if word { 0xFFFF } else { 0xFF }
                        } else if word {
                            product as i32 != i32::from(product as i16)
                        } else {
                            product as i32 != i32::from(product as i8)
                        };
                        self.registers.set(Gpr16::Ax, product as u16);
                        if word {
                            self.registers.set(Gpr16::Dx, (product >> 16) as u16);
                        }
                        self.flags = (self.flags & !(flags::CARRY | flags::OVERFLOW))
                            | if overflow {
                                flags::CARRY | flags::OVERFLOW
                            } else {
                                0
                            };
                    }
                    (0xF6 | 0xF7, 6 | 7) => {
                        let ax = self.registers.get(Gpr16::Ax);
                        let raw = (u32::from(self.registers.get(Gpr16::Dx)) << 16) | u32::from(ax);
                        let signed = operation == 7;
                        let dividend = if signed {
                            if word {
                                i64::from(raw as i32)
                            } else {
                                i64::from(ax as i16)
                            }
                        } else if word {
                            i64::from(raw)
                        } else {
                            i64::from(ax)
                        };
                        let divisor = if signed {
                            if word {
                                i64::from(value as i16)
                            } else {
                                i64::from(value as u8 as i8)
                            }
                        } else {
                            i64::from(value)
                        };
                        if divisor == 0 {
                            return Err(CpuError::DivideError { cs, ip });
                        }
                        let quotient = dividend / divisor;
                        let remainder = dividend % divisor;
                        let (minimum, maximum) = match (signed, word) {
                            (true, true) => (-32768, 32767),
                            (true, false) => (-128, 127),
                            (false, true) => (0, 65535),
                            _ => (0, 255),
                        };
                        if !(minimum..=maximum).contains(&quotient) {
                            return Err(CpuError::DivideError { cs, ip });
                        }
                        if word {
                            self.registers.set(Gpr16::Ax, quotient as u16);
                            self.registers.set(Gpr16::Dx, remainder as u16);
                        } else {
                            self.registers.set_byte(0, quotient as u8);
                            self.registers.set_byte(4, remainder as u8);
                        }
                    }
                    _ => {
                        return Err(CpuError::UnsupportedOperand {
                            cs,
                            ip,
                            opcode,
                            modrm,
                        });
                    }
                }
            }
            0x70..=0x7F | 0xEB | 0xE0..=0xE3 => {
                let displacement = decode::fetch(&mut self.registers, bus)? as i8;
                let take = match opcode {
                    0x70..=0x7F => flags::condition(self.flags, opcode & 15),
                    0xE0..=0xE2 => {
                        let count = self.address_register(Gpr16::Cx, address_32).wrapping_sub(1);
                        self.set_address_register(Gpr16::Cx, count, address_32);
                        let count = self.address_register(Gpr16::Cx, address_32);
                        count != 0
                            && (opcode == 0xE2
                                || (self.flags & flags::ZERO != 0) == (opcode == 0xE1))
                    }
                    0xE3 => self.address_register(Gpr16::Cx, address_32) == 0,
                    _ => true,
                };
                if take {
                    let target = self
                        .registers
                        .ip
                        .wrapping_add_signed(i32::from(displacement));
                    self.near_jump(if self.registers.code32() {
                        target
                    } else {
                        u32::from(target as u16)
                    })?;
                }
            }
            0xE8 | 0xE9 => {
                let displacement = decode::fetch_word(&mut self.registers, bus)? as i16;
                if opcode == 0xE8 {
                    self.push(bus, self.registers.ip as u16)?;
                }
                self.near_jump(u32::from(
                    (self.registers.ip as u16).wrapping_add_signed(displacement),
                ))?;
            }
            0xEA | 0x9A => {
                let offset = decode::fetch_word(&mut self.registers, bus)?;
                let segment = decode::fetch_word(&mut self.registers, bus)?;
                if opcode == 0x9A {
                    self.push(bus, self.registers.cs)?;
                    self.push(bus, self.registers.ip as u16)?;
                }
                self.far_jump(bus, segment, u32::from(offset))?;
            }
            0xC2 | 0xC3 | 0xCA | 0xCB => {
                let extra = if opcode & 1 == 0 {
                    decode::fetch_word(&mut self.registers, bus)?
                } else {
                    0
                };
                self.registers.ip = u32::from(self.pop(bus)?);
                if opcode >= 0xCA {
                    let selector = self.pop(bus)?;
                    self.far_jump(bus, selector, self.registers.ip)?;
                }
                self.adjust_stack(u32::from(extra));
            }
            0x06 | 0x0E | 0x16 | 0x1E | 0x9C => {
                let value = match opcode {
                    0x06 => self.registers.es,
                    0x0E => self.registers.cs,
                    0x16 => self.registers.ss,
                    0x1E => self.registers.ds,
                    _ => self.flags as u16,
                };
                self.push(bus, value)?;
            }
            0x07 | 0x17 | 0x1F | 0x9D => {
                let value = self.pop(bus)?;
                match opcode {
                    0x07 => self.load_segment(bus, SegmentRegister::Es, value)?,
                    0x17 => self.load_segment(bus, SegmentRegister::Ss, value)?,
                    0x1F => self.load_segment(bus, SegmentRegister::Ds, value)?,
                    _ => {
                        self.flags = (self.flags & !flags::POPF_386)
                            | (u32::from(value) & flags::POPF_386)
                            | 2
                    }
                }
            }
            0x68 | 0x6A => {
                let value = if opcode == 0x68 {
                    decode::fetch_word(&mut self.registers, bus)?
                } else {
                    decode::fetch(&mut self.registers, bus)? as i8 as i16 as u16
                };
                self.push(bus, value)?;
            }
            0x69 | 0x6B => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    segment_override,
                    address_32,
                )?;
                let immediate = if opcode == 0x69 {
                    decode::fetch_word(&mut self.registers, bus)? as i16
                } else {
                    decode::fetch(&mut self.registers, bus)? as i8 as i16
                };
                let value = operand.read(&self.registers, bus, true)? as i16;
                let product = i32::from(value) * i32::from(immediate);
                let overflow = product != i32::from(product as i16);
                self.registers
                    .set(Gpr16::decode(modrm >> 3), product as u16);
                self.flags = (self.flags & !(flags::CARRY | flags::OVERFLOW))
                    | if overflow {
                        flags::CARRY | flags::OVERFLOW
                    } else {
                        0
                    };
            }
            0x60 => {
                let original_sp = self.registers.get(Gpr16::Sp);
                for register in [
                    Gpr16::Ax,
                    Gpr16::Cx,
                    Gpr16::Dx,
                    Gpr16::Bx,
                    Gpr16::Sp,
                    Gpr16::Bp,
                    Gpr16::Si,
                    Gpr16::Di,
                ] {
                    let value = if register == Gpr16::Sp {
                        original_sp
                    } else {
                        self.registers.get(register)
                    };
                    self.push(bus, value)?;
                }
            }
            0xC9 => {
                self.set_stack_pointer(if self.registers.stack32() {
                    self.registers.get32(Gpr32::Ebp)
                } else {
                    u32::from(self.registers.get(Gpr16::Bp))
                });
                let value = self.pop(bus)?;
                self.registers.set(Gpr16::Bp, value);
            }
            0xC8 => {
                let allocation = decode::fetch_word(&mut self.registers, bus)?;
                let nesting = decode::fetch(&mut self.registers, bus)? & 31;
                let original_bp = self.registers.get(Gpr16::Bp);
                self.push(bus, original_bp)?;
                let frame = self.registers.get(Gpr16::Sp);
                let mut bp = original_bp;
                if nesting != 0 {
                    for _ in 1..nesting {
                        bp = bp.wrapping_sub(2);
                        let value = read_word(
                            bus,
                            self.registers.address(
                                SegmentRegister::Ss,
                                u32::from(bp),
                                2,
                                Access::Read,
                            )?,
                        )?;
                        self.push(bus, value)?;
                    }
                    self.push(bus, frame)?;
                }
                self.registers.set(Gpr16::Bp, frame);
                self.registers.set(
                    Gpr16::Sp,
                    self.registers.get(Gpr16::Sp).wrapping_sub(allocation),
                );
            }
            0x61 => {
                for register in [
                    Gpr16::Di,
                    Gpr16::Si,
                    Gpr16::Bp,
                    Gpr16::Sp,
                    Gpr16::Bx,
                    Gpr16::Dx,
                    Gpr16::Cx,
                    Gpr16::Ax,
                ] {
                    let value = self.pop(bus)?;
                    if register != Gpr16::Sp {
                        self.registers.set(register, value);
                    }
                }
            }
            0x9E => {
                let mask =
                    flags::SIGN | flags::ZERO | flags::AUXILIARY | flags::PARITY | flags::CARRY;
                self.flags =
                    (self.flags & !mask) | (u32::from(self.registers.get_byte(4)) & mask) | 2;
            }
            0x9F => self.registers.set_byte(4, (self.flags as u8 & 0xD5) | 2),
            0xF8 => self.flags &= !flags::CARRY,
            0xF9 => self.flags |= flags::CARRY,
            0xF5 => self.flags ^= flags::CARRY,
            0xCD => {
                let vector = decode::fetch(&mut self.registers, bus)?;
                self.interrupt(bus, vector)?;
            }
            0xCF => {
                self.registers.ip = u32::from(self.pop(bus)?);
                let selector = self.pop(bus)?;
                self.far_jump(bus, selector, self.registers.ip)?;
                self.flags = u32::from(self.pop(bus)?) | 2;
            }
            0xE4 | 0xE6 | 0xEC | 0xEE => {
                let port = if opcode < 0xEC {
                    u16::from(decode::fetch(&mut self.registers, bus)?)
                } else {
                    self.registers.get(Gpr16::Dx)
                };
                if opcode == 0xE4 || opcode == 0xEC {
                    let value = bus.read_port_u8(port)?;
                    self.registers.set_byte(0, value);
                } else {
                    bus.write_port_u8(port, self.registers.get_byte(0))?;
                }
            }
            0xE5 | 0xE7 | 0xED | 0xEF => {
                let port = if opcode < 0xEC {
                    u16::from(decode::fetch(&mut self.registers, bus)?)
                } else {
                    self.registers.get(Gpr16::Dx)
                };
                if matches!(opcode, 0xE5 | 0xED) {
                    if operand_32 {
                        let value = bus.read_port_u32(port)?;
                        self.registers.set32(Gpr32::Eax, value);
                    } else {
                        let value = bus.read_port_u16(port)?;
                        self.registers.set(Gpr16::Ax, value);
                    }
                } else if operand_32 {
                    bus.write_port_u32(port, self.registers.get32(Gpr32::Eax))?;
                } else {
                    bus.write_port_u16(port, self.registers.get(Gpr16::Ax))?;
                }
            }
            0xFA => self.flags &= !flags::INTERRUPT,
            0xFB => {
                self.flags |= flags::INTERRUPT;
                self.interrupt_shadow = 1;
            }
            0xFC => self.flags &= !flags::DIRECTION,
            0xFD => self.flags |= flags::DIRECTION,
            0xF4 => {
                self.halted = true;
                return Ok(StepOutcome::Halted);
            }
            _ => return Err(CpuError::UnsupportedOpcode { cs, ip, opcode }),
        }
        Ok(StepOutcome::Continue)
    }

    pub fn interrupt<B: MemoryBus>(&mut self, bus: &mut B, vector: u8) -> Result<(), CpuError> {
        if self.protected_mode() {
            self.interrupt_protected(bus, vector)?;
        } else {
            self.push(bus, self.flags as u16)?;
            self.push(bus, self.registers.cs)?;
            self.push(bus, self.registers.ip as u16)?;
            let address = u32::from(vector) * 4;
            let offset = bus.read_u16(address)?;
            let selector = bus.read_u16(address + 2)?;
            self.far_jump(bus, selector, u32::from(offset))?;
            self.flags &= !(flags::INTERRUPT | flags::TRAP);
        }
        self.halted = false;
        Ok(())
    }
    fn push<B: MemoryBus>(&mut self, bus: &mut B, value: u16) -> Result<(), CpuError> {
        let next = self.stack_pointer().wrapping_sub(2);
        let next = if self.registers.stack32() {
            next
        } else {
            u32::from(next as u16)
        };
        let address = self
            .registers
            .address(SegmentRegister::Ss, next, 2, Access::Write)?;
        write_word(bus, address, value)?;
        self.set_stack_pointer(next);
        Ok(())
    }
    fn pop<B: MemoryBus>(&mut self, bus: &mut B) -> Result<u16, CpuError> {
        let sp = self.stack_pointer();
        let address = self
            .registers
            .address(SegmentRegister::Ss, sp, 2, Access::Read)?;
        let value = read_word(bus, address)?;
        self.adjust_stack(2);
        Ok(value)
    }
    fn address_register(&self, register: Gpr16, wide: bool) -> u32 {
        if wide {
            self.registers.get32(Gpr32::decode(register as u8))
        } else {
            u32::from(self.registers.get(register))
        }
    }
    fn set_address_register(&mut self, register: Gpr16, value: u32, wide: bool) {
        if wide {
            self.registers.set32(Gpr32::decode(register as u8), value);
        } else {
            self.registers.set(register, value as u16);
        }
    }
}

fn read_word<B: MemoryBus>(bus: &mut B, address: u32) -> Result<u16, BusError> {
    let low = bus.read_u8(address)?;
    let high = bus.read_u8(address.wrapping_add(1))?;
    Ok(u16::from_le_bytes([low, high]))
}
fn write_word<B: MemoryBus>(bus: &mut B, address: u32, value: u16) -> Result<(), BusError> {
    let [low, high] = value.to_le_bytes();
    bus.write_u8(address, low)?;
    bus.write_u8(address.wrapping_add(1), high)
}
