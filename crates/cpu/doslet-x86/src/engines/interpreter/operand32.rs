//! Dword instruction forms, selected by the code descriptor and operand override.
use super::{Cpu, CpuError, StepOutcome};
use crate::{
    Access, Gpr16, Gpr32, SegmentRegister,
    decode::{self, Operand},
    flags,
};
use doslet_bus::{CpuBus, MemoryBus};

#[derive(Clone, Copy)]
pub(super) struct Context {
    pub cs: u16,
    pub ip: u32,
    pub segment: SegmentRegister,
    pub segment_override: Option<SegmentRegister>,
    pub repeat: bool,
    pub repeat_zero: bool,
    pub address_32: bool,
}
impl Context {
    fn opcode(self, opcode: u8) -> CpuError {
        CpuError::UnsupportedOpcode {
            cs: self.cs,
            ip: self.ip,
            opcode,
        }
    }
    fn operand(self, opcode: u8, modrm: u8) -> CpuError {
        CpuError::UnsupportedOperand {
            cs: self.cs,
            ip: self.ip,
            opcode,
            modrm,
        }
    }
}

pub(super) fn handles(opcode: u8) -> bool {
    (opcode <= 0x3D && opcode & 1 != 0 && opcode & 7 <= 5)
        || matches!(opcode, 0x06 | 0x07 | 0x0E | 0x16 | 0x17 | 0x1E | 0x1F
            | 0x40..=0x61 | 0x68 | 0x69 | 0x6A | 0x6B | 0x81 | 0x83 | 0x85 | 0x87
            | 0x89 | 0x8B | 0x8C | 0x8D | 0x8F | 0x90..=0x9A | 0x9C | 0x9D | 0xA1 | 0xA3 | 0xA5 | 0xA7
            | 0xA9 | 0xAB | 0xAD | 0xAF | 0xB8..=0xBF | 0xC1..=0xC5 | 0xC7..=0xCB | 0xCF
            | 0xD1 | 0xD3 | 0xE8..=0xEA | 0xF7 | 0xFF)
}

impl Cpu {
    pub(super) fn push32<B: MemoryBus>(&mut self, bus: &mut B, value: u32) -> Result<(), CpuError> {
        let next = self.stack_pointer().wrapping_sub(4);
        let next = if self.registers.stack32() {
            next
        } else {
            u32::from(next as u16)
        };
        let address = self
            .registers
            .address(SegmentRegister::Ss, next, 4, Access::Write)?;
        decode::write_dword(bus, address, value)?;
        self.set_stack_pointer(next);
        Ok(())
    }
    pub(super) fn pop32<B: MemoryBus>(&mut self, bus: &mut B) -> Result<u32, CpuError> {
        let address =
            self.registers
                .address(SegmentRegister::Ss, self.stack_pointer(), 4, Access::Read)?;
        let value = decode::read_dword(bus, address)?;
        self.adjust_stack(4);
        Ok(value)
    }
    fn jump32(&mut self, target: u32, _context: Context) -> Result<(), CpuError> {
        self.near_jump(target)
    }
    pub(super) fn dword_step<B: CpuBus>(
        &mut self,
        bus: &mut B,
        opcode: u8,
        context: Context,
    ) -> Result<StepOutcome, CpuError> {
        match opcode {
            0x90..=0x97 => {
                let register = Gpr32::decode(opcode - 0x90);
                let value = self.registers.get32(register);
                self.registers
                    .set32(register, self.registers.get32(Gpr32::Eax));
                self.registers.set32(Gpr32::Eax, value);
            }
            0x98 => self.registers.set32(
                Gpr32::Eax,
                self.registers.get(Gpr16::Ax) as i16 as i32 as u32,
            ),
            0x99 => self.registers.set32(
                Gpr32::Edx,
                if self.registers.get32(Gpr32::Eax) & 0x80000000 != 0 {
                    u32::MAX
                } else {
                    0
                },
            ),
            0xB8..=0xBF => {
                let value = decode::fetch_dword(&mut self.registers, bus)?;
                self.registers.set32(Gpr32::decode(opcode - 0xB8), value);
            }
            0x89 | 0x8B | 0x8C | 0x8D | 0x8F | 0xC7 | 0x81 | 0x83 | 0x85 | 0x87 => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let target = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                let source = Operand::Register((modrm >> 3) & 7);
                match opcode {
                    0x89 => {
                        let value = source.read32(&self.registers, bus)?;
                        target.write32(&mut self.registers, bus, value)?;
                    }
                    0x8B => {
                        let value = target.read32(&self.registers, bus)?;
                        source.write32(&mut self.registers, bus, value)?;
                    }
                    0x8C => {
                        let value = match (modrm >> 3) & 7 {
                            0 => self.registers.es,
                            1 => self.registers.cs,
                            2 => self.registers.ss,
                            3 => self.registers.ds,
                            4 => self.registers.fs,
                            5 => self.registers.gs,
                            _ => return Err(context.operand(opcode, modrm)),
                        };
                        if matches!(target, Operand::Register(_)) {
                            target.write32(&mut self.registers, bus, u32::from(value))?;
                        } else {
                            target.write(&mut self.registers, bus, true, value)?;
                        }
                    }
                    0x8D => {
                        let Operand::Memory { offset, .. } = target else {
                            return Err(context.operand(opcode, modrm));
                        };
                        source.write32(&mut self.registers, bus, offset)?;
                    }
                    0x8F if modrm & 0x38 == 0 => {
                        let value = self.pop32(bus)?;
                        target.write32(&mut self.registers, bus, value)?;
                    }
                    0xC7 if modrm & 0x38 == 0 => {
                        let value = decode::fetch_dword(&mut self.registers, bus)?;
                        target.write32(&mut self.registers, bus, value)?;
                    }
                    0x81 | 0x83 | 0x85 | 0x87 => {
                        let left = target.read32(&self.registers, bus)?;
                        let right = match opcode {
                            0x81 => decode::fetch_dword(&mut self.registers, bus)?,
                            0x83 => decode::fetch(&mut self.registers, bus)? as i8 as i32 as u32,
                            _ => source.read32(&self.registers, bus)?,
                        };
                        if opcode == 0x87 {
                            target.write32(&mut self.registers, bus, right)?;
                            source.write32(&mut self.registers, bus, left)?;
                        } else {
                            let operation = if opcode == 0x85 { 4 } else { (modrm >> 3) & 7 };
                            let (value, flags) = flags::alu32(self.flags, left, right, operation);
                            self.flags = flags;
                            if opcode != 0x85 && operation != 7 {
                                target.write32(&mut self.registers, bus, value)?;
                            }
                        }
                    }
                    _ => return Err(context.operand(opcode, modrm)),
                }
            }
            code if code <= 0x3D && code & 1 != 0 && code & 7 <= 5 => {
                let operation = code >> 3;
                let (target, left, right) = if code & 7 >= 4 {
                    let target = Operand::Register(0);
                    let right = decode::fetch_dword(&mut self.registers, bus)?;
                    (target, target.read32(&self.registers, bus)?, right)
                } else {
                    let modrm = decode::fetch(&mut self.registers, bus)?;
                    let rm = decode::operand(
                        &mut self.registers,
                        bus,
                        modrm,
                        context.segment_override,
                        context.address_32,
                    )?;
                    let reg = Operand::Register((modrm >> 3) & 7);
                    let (target, source) = if code & 2 != 0 { (reg, rm) } else { (rm, reg) };
                    (
                        target,
                        target.read32(&self.registers, bus)?,
                        source.read32(&self.registers, bus)?,
                    )
                };
                let (value, flags) = flags::alu32(self.flags, left, right, operation);
                self.flags = flags;
                if operation != 7 {
                    target.write32(&mut self.registers, bus, value)?;
                }
            }
            0x40..=0x4F => {
                let register = Gpr32::decode(opcode);
                let (value, flags) = flags::alu32(
                    self.flags,
                    self.registers.get32(register),
                    1,
                    if opcode < 0x48 { 0 } else { 5 },
                );
                self.registers.set32(register, value);
                self.flags = (flags & !flags::CARRY) | (self.flags & flags::CARRY);
            }
            0x50..=0x57 => self.push32(bus, self.registers.get32(Gpr32::decode(opcode)))?,
            0x58..=0x5F => {
                let value = self.pop32(bus)?;
                self.registers.set32(Gpr32::decode(opcode), value);
            }
            0x06 | 0x0E | 0x16 | 0x1E | 0x9C => {
                let value = match opcode {
                    0x06 => u32::from(self.registers.es),
                    0x0E => u32::from(self.registers.cs),
                    0x16 => u32::from(self.registers.ss),
                    0x1E => u32::from(self.registers.ds),
                    _ => self.flags & !(flags::RESUME | flags::VIRTUAL),
                };
                self.push32(bus, value)?;
            }
            0x07 | 0x17 | 0x1F | 0x9D => {
                let value = self.pop32(bus)?;
                match opcode {
                    0x07 => self.load_segment(bus, SegmentRegister::Es, value as u16)?,
                    0x17 => self.load_segment(bus, SegmentRegister::Ss, value as u16)?,
                    0x1F => self.load_segment(bus, SegmentRegister::Ds, value as u16)?,
                    _ => {
                        self.flags = (self.flags & !flags::POPF_386) | (value & flags::POPF_386) | 2
                    }
                }
            }
            0x68 | 0x6A => {
                let value = if opcode == 0x68 {
                    decode::fetch_dword(&mut self.registers, bus)?
                } else {
                    decode::fetch(&mut self.registers, bus)? as i8 as i32 as u32
                };
                self.push32(bus, value)?;
            }
            0x69 | 0x6B => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                let immediate = if opcode == 0x69 {
                    decode::fetch_dword(&mut self.registers, bus)? as i32
                } else {
                    decode::fetch(&mut self.registers, bus)? as i8 as i32
                };
                let value = operand.read32(&self.registers, bus)? as i32;
                let product = i64::from(value) * i64::from(immediate);
                let overflow = product != i64::from(product as i32);
                self.registers
                    .set32(Gpr32::decode(modrm >> 3), product as u32);
                self.flags = (self.flags & !(flags::CARRY | flags::OVERFLOW))
                    | if overflow {
                        flags::CARRY | flags::OVERFLOW
                    } else {
                        0
                    };
            }
            0x60 => {
                let original_sp = self.registers.get32(Gpr32::Esp);
                for register in [
                    Gpr32::Eax,
                    Gpr32::Ecx,
                    Gpr32::Edx,
                    Gpr32::Ebx,
                    Gpr32::Esp,
                    Gpr32::Ebp,
                    Gpr32::Esi,
                    Gpr32::Edi,
                ] {
                    let value = if register == Gpr32::Esp {
                        original_sp
                    } else {
                        self.registers.get32(register)
                    };
                    self.push32(bus, value)?;
                }
            }
            0x61 => {
                for register in [
                    Gpr32::Edi,
                    Gpr32::Esi,
                    Gpr32::Ebp,
                    Gpr32::Esp,
                    Gpr32::Ebx,
                    Gpr32::Edx,
                    Gpr32::Ecx,
                    Gpr32::Eax,
                ] {
                    let value = self.pop32(bus)?;
                    if register != Gpr32::Esp {
                        self.registers.set32(register, value);
                    }
                }
            }
            0xA1 | 0xA3 => {
                let offset = if context.address_32 {
                    decode::fetch_dword(&mut self.registers, bus)?
                } else {
                    u32::from(decode::fetch_word(&mut self.registers, bus)?)
                };
                let operand = Operand::Memory {
                    segment: context.segment,
                    offset,
                };
                if opcode == 0xA1 {
                    let value = operand.read32(&self.registers, bus)?;
                    self.registers.set32(Gpr32::Eax, value);
                } else {
                    let value = self.registers.get32(Gpr32::Eax);
                    operand.write32(&mut self.registers, bus, value)?;
                }
            }
            0xA9 => {
                let value = decode::fetch_dword(&mut self.registers, bus)?;
                self.flags = flags::alu32(self.flags, self.registers.get32(Gpr32::Eax), value, 4).1;
            }
            0xA5 | 0xA7 | 0xAB | 0xAD | 0xAF => self.string32(bus, opcode, context)?,
            0xC1 | 0xD1 | 0xD3 => self.shift32(bus, opcode, context)?,
            0xC4 | 0xC5 => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                let Operand::Memory { .. } = operand else {
                    return Err(context.operand(opcode, modrm));
                };
                let address = operand.address(&self.registers, 6, Access::Read)?;
                let value = decode::read_dword(bus, address)?;
                let selector = bus.read_u16(address + 4)?;
                self.load_segment(
                    bus,
                    if opcode == 0xC4 {
                        SegmentRegister::Es
                    } else {
                        SegmentRegister::Ds
                    },
                    selector,
                )?;
                self.registers.set32(Gpr32::decode(modrm >> 3), value);
            }
            0xF7 => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                let value = operand.read32(&self.registers, bus)?;
                match (modrm >> 3) & 7 {
                    0 => {
                        let immediate = decode::fetch_dword(&mut self.registers, bus)?;
                        self.flags = flags::alu32(self.flags, value, immediate, 4).1;
                    }
                    2 => operand.write32(&mut self.registers, bus, !value)?,
                    3 => {
                        let (result, changed) = flags::alu32(self.flags, 0, value, 5);
                        self.flags = changed;
                        operand.write32(&mut self.registers, bus, result)?;
                    }
                    operation @ (4 | 5) => {
                        let accumulator = self.registers.get32(Gpr32::Eax);
                        let (product, overflow) = if operation == 4 {
                            let product = u64::from(accumulator) * u64::from(value);
                            (product, product >> 32 != 0)
                        } else {
                            let product = i64::from(accumulator as i32) * i64::from(value as i32);
                            (product as u64, product != i64::from(product as i32))
                        };
                        self.registers.set32(Gpr32::Eax, product as u32);
                        self.registers.set32(Gpr32::Edx, (product >> 32) as u32);
                        self.flags = (self.flags & !(flags::CARRY | flags::OVERFLOW))
                            | if overflow {
                                flags::CARRY | flags::OVERFLOW
                            } else {
                                0
                            };
                    }
                    operation @ (6 | 7) => {
                        let dividend = (u64::from(self.registers.get32(Gpr32::Edx)) << 32)
                            | u64::from(self.registers.get32(Gpr32::Eax));
                        let result = if operation == 6 {
                            dividend
                                .checked_div(u64::from(value))
                                .filter(|quotient| *quotient <= u64::from(u32::MAX))
                                .map(|quotient| {
                                    (quotient as u32, (dividend % u64::from(value)) as u32)
                                })
                        } else {
                            (dividend as i64)
                                .checked_div(i64::from(value as i32))
                                .filter(|quotient| i32::try_from(*quotient).is_ok())
                                .map(|quotient| {
                                    (
                                        quotient as u32,
                                        ((dividend as i64) % i64::from(value as i32)) as u32,
                                    )
                                })
                        };
                        let Some((quotient, remainder)) = result else {
                            return Err(CpuError::DivideError {
                                cs: context.cs,
                                ip: context.ip,
                            });
                        };
                        self.registers.set32(Gpr32::Eax, quotient);
                        self.registers.set32(Gpr32::Edx, remainder);
                    }
                    _ => return Err(context.operand(opcode, modrm)),
                }
            }
            0xFF => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                let value = operand.read32(&self.registers, bus)?;
                match (modrm >> 3) & 7 {
                    operation @ 0..=1 => {
                        let (value, changed) =
                            flags::alu32(self.flags, value, 1, if operation == 0 { 0 } else { 5 });
                        self.flags = (changed & !flags::CARRY) | (self.flags & flags::CARRY);
                        operand.write32(&mut self.registers, bus, value)?;
                    }
                    operation @ (2 | 4) => {
                        if operation == 2 {
                            self.push32(bus, self.registers.ip)?;
                        }
                        self.near_jump(value)?;
                    }
                    operation @ (3 | 5) => {
                        let Operand::Memory { .. } = operand else {
                            return Err(context.operand(opcode, modrm));
                        };
                        let address = operand.address(&self.registers, 6, Access::Read)?;
                        let selector = bus.read_u16(address + 4)?;
                        if operation == 3 {
                            self.push32(bus, u32::from(self.registers.cs))?;
                            self.push32(bus, self.registers.ip)?;
                        }
                        self.far_jump(bus, selector, value)?;
                    }
                    6 => self.push32(bus, value)?,
                    _ => return Err(context.operand(opcode, modrm)),
                }
            }
            0xC8 => {
                let allocation = decode::fetch_word(&mut self.registers, bus)?;
                let nesting = decode::fetch(&mut self.registers, bus)? & 31;
                let original_bp = self.registers.get32(Gpr32::Ebp);
                self.push32(bus, original_bp)?;
                let frame = self.stack_pointer();
                let mut bp = original_bp;
                if nesting != 0 {
                    for _ in 1..nesting {
                        bp = bp.wrapping_sub(4);
                        let offset = if self.registers.stack32() {
                            bp
                        } else {
                            u32::from(bp as u16)
                        };
                        let address =
                            self.registers
                                .address(SegmentRegister::Ss, offset, 4, Access::Read)?;
                        let value = decode::read_dword(bus, address)?;
                        self.push32(bus, value)?;
                    }
                    self.push32(bus, frame)?;
                }
                self.registers.set32(Gpr32::Ebp, frame);
                self.adjust_stack(u32::from(allocation).wrapping_neg());
            }
            0xC9 => {
                self.set_stack_pointer(self.registers.get32(Gpr32::Ebp));
                let value = self.pop32(bus)?;
                self.registers.set32(Gpr32::Ebp, value);
            }
            0xE8 | 0xE9 => {
                let displacement = decode::fetch_dword(&mut self.registers, bus)?;
                let next = self.registers.ip;
                let target = next.wrapping_add(displacement);
                self.jump32(target, context)?;
                if opcode == 0xE8 {
                    self.push32(bus, next)?;
                }
            }
            0xC2 | 0xC3 => {
                let extra = if opcode == 0xC2 {
                    decode::fetch_word(&mut self.registers, bus)?
                } else {
                    0
                };
                let target = self.pop32(bus)?;
                self.jump32(target, context)?;
                self.adjust_stack(u32::from(extra));
            }
            0xCF => {
                let offset = self.pop32(bus)?;
                let selector = self.pop32(bus)? as u16;
                let restored = self.pop32(bus)?;
                if self.protected_mode() && restored & flags::VIRTUAL != 0 {
                    return Err(CpuError::Virtual8086Unavailable);
                }
                if self.protected_mode() && (selector & 3) as u8 != self.registers.cpl {
                    return Err(CpuError::PrivilegeChangeUnavailable { selector });
                }
                self.far_jump(bus, selector, offset)?;
                self.flags = (self.flags & !(flags::POPF_386 | flags::RESUME))
                    | (restored & (flags::POPF_386 | flags::RESUME))
                    | 2;
            }
            0xCA | 0xCB => {
                let extra = if opcode == 0xCA {
                    u32::from(decode::fetch_word(&mut self.registers, bus)?)
                } else {
                    0
                };
                let offset = self.pop32(bus)?;
                let selector = self.pop32(bus)? as u16;
                if self.protected_mode() && (selector & 3) as u8 != self.registers.cpl {
                    return Err(CpuError::PrivilegeChangeUnavailable { selector });
                }
                self.far_jump(bus, selector, offset)?;
                self.adjust_stack(extra);
            }
            0xEA | 0x9A => {
                let offset = decode::fetch_dword(&mut self.registers, bus)?;
                let selector = decode::fetch_word(&mut self.registers, bus)?;
                if opcode == 0x9A {
                    self.push32(bus, u32::from(self.registers.cs))?;
                    self.push32(bus, self.registers.ip)?;
                }
                self.far_jump(bus, selector, offset)?;
            }
            _ => return Err(context.opcode(opcode)),
        }
        Ok(StepOutcome::Continue)
    }

    fn string32<B: CpuBus>(
        &mut self,
        bus: &mut B,
        opcode: u8,
        context: Context,
    ) -> Result<(), CpuError> {
        let count = self.address_register(Gpr16::Cx, context.address_32);
        if context.repeat && count == 0 {
            return Ok(());
        }
        let si = self.address_register(Gpr16::Si, context.address_32);
        let di = self.address_register(Gpr16::Di, context.address_32);
        let source = Operand::Memory {
            segment: context.segment,
            offset: si,
        };
        let target = Operand::Memory {
            segment: SegmentRegister::Es,
            offset: di,
        };
        match opcode {
            0xA5 => {
                let value = source.read32(&self.registers, bus)?;
                target.write32(&mut self.registers, bus, value)?;
            }
            0xA7 => {
                let left = source.read32(&self.registers, bus)?;
                let right = target.read32(&self.registers, bus)?;
                self.flags = flags::alu32(self.flags, left, right, 7).1;
            }
            0xAB => {
                let value = self.registers.get32(Gpr32::Eax);
                target.write32(&mut self.registers, bus, value)?;
            }
            0xAD => {
                let value = source.read32(&self.registers, bus)?;
                self.registers.set32(Gpr32::Eax, value);
            }
            _ => {
                let right = target.read32(&self.registers, bus)?;
                self.flags = flags::alu32(self.flags, self.registers.get32(Gpr32::Eax), right, 7).1;
            }
        }
        let displacement = if self.flags & flags::DIRECTION == 0 {
            4
        } else {
            -4
        };
        if matches!(opcode, 0xA5 | 0xA7 | 0xAD) {
            self.set_address_register(
                Gpr16::Si,
                si.wrapping_add_signed(displacement),
                context.address_32,
            );
        }
        if opcode != 0xAD {
            self.set_address_register(
                Gpr16::Di,
                di.wrapping_add_signed(displacement),
                context.address_32,
            );
        }
        if context.repeat {
            self.set_address_register(Gpr16::Cx, count - 1, context.address_32);
            if count > 1
                && (!matches!(opcode, 0xA7 | 0xAF)
                    || (self.flags & flags::ZERO != 0) == context.repeat_zero)
            {
                self.registers.ip = context.ip;
            }
        }
        Ok(())
    }

    fn shift32<B: CpuBus>(
        &mut self,
        bus: &mut B,
        opcode: u8,
        context: Context,
    ) -> Result<(), CpuError> {
        let modrm = decode::fetch(&mut self.registers, bus)?;
        let operand = decode::operand(
            &mut self.registers,
            bus,
            modrm,
            context.segment_override,
            context.address_32,
        )?;
        let count = match opcode {
            0xC1 => decode::fetch(&mut self.registers, bus)?,
            0xD3 => self.registers.get_byte(1),
            _ => 1,
        } & 31;
        if count == 0 {
            return Ok(());
        }
        let operation = (modrm >> 3) & 7;
        let original = operand.read32(&self.registers, bus)?;
        let mut value = original;
        let mut carry = self.flags & flags::CARRY != 0;
        for _ in 0..count {
            let old_carry = carry;
            if matches!(operation, 0 | 2 | 4 | 6) {
                carry = value & 0x80000000 != 0;
                value <<= 1;
                if operation == 0 {
                    value |= u32::from(carry);
                }
                if operation == 2 {
                    value |= u32::from(old_carry);
                }
            } else {
                carry = value & 1 != 0;
                let high = value & 0x80000000;
                value >>= 1;
                if operation == 1 {
                    value |= u32::from(carry) << 31;
                }
                if operation == 3 {
                    value |= u32::from(old_carry) << 31;
                }
                if operation == 7 {
                    value |= high;
                }
            }
        }
        if operation >= 4 {
            self.flags = flags::alu32(self.flags, value, 0, 1).1 | (self.flags & flags::OVERFLOW);
        }
        self.flags = (self.flags & !flags::CARRY) | u32::from(carry);
        if count == 1 {
            let overflow = match operation {
                0 | 2 | 4 | 6 => (value & 0x80000000 != 0) != carry,
                1 | 3 => ((value >> 31) ^ (value >> 30)) & 1 != 0,
                5 => original & 0x80000000 != 0,
                _ => false,
            };
            self.flags =
                (self.flags & !flags::OVERFLOW) | if overflow { flags::OVERFLOW } else { 0 };
        }
        operand.write32(&mut self.registers, bus, value)?;
        Ok(())
    }

    pub(super) fn extended_step<B: CpuBus>(
        &mut self,
        bus: &mut B,
        dword: bool,
        context: Context,
    ) -> Result<StepOutcome, CpuError> {
        let opcode = decode::fetch(&mut self.registers, bus)?;
        match opcode {
            0xA4 | 0xA5 | 0xAC | 0xAD => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                let count = if opcode & 1 == 0 {
                    decode::fetch(&mut self.registers, bus)?
                } else {
                    self.registers.get_byte(1)
                } & 31;
                if count == 0 {
                    return Ok(StepOutcome::Continue);
                }
                let width = if dword { 32 } else { 16 };
                if count > width {
                    return Err(context.operand(opcode, modrm));
                }
                let (original, source) = if dword {
                    (
                        operand.read32(&self.registers, bus)?,
                        self.registers.get32(Gpr32::decode(modrm >> 3)),
                    )
                } else {
                    (
                        u32::from(operand.read(&self.registers, bus, true)?),
                        u32::from(self.registers.get(Gpr16::decode(modrm >> 3))),
                    )
                };
                let mask = if dword { u32::MAX } else { 0xFFFF };
                let (value, carry) = if opcode & 8 != 0 {
                    let joined = (u64::from(source) << width) | u64::from(original);
                    (
                        (joined >> count) as u32 & mask,
                        (original >> (count - 1)) & 1,
                    )
                } else {
                    let joined = (u64::from(original) << width) | u64::from(source);
                    (
                        ((joined << count) >> width) as u32 & mask,
                        (original >> (width - count)) & 1,
                    )
                };
                let mut updated =
                    self.flags & !(flags::CARRY | flags::SIGN | flags::ZERO | flags::PARITY);
                updated |= carry;
                if value == 0 {
                    updated |= flags::ZERO;
                }
                if value & (1 << (width - 1)) != 0 {
                    updated |= flags::SIGN;
                }
                if (value as u8).count_ones().is_multiple_of(2) {
                    updated |= flags::PARITY;
                }
                // AF is undefined; OF is defined only for a one-bit shift.
                if count == 1 {
                    updated &= !flags::OVERFLOW;
                    if (original ^ value) & (1 << (width - 1)) != 0 {
                        updated |= flags::OVERFLOW;
                    }
                }
                if dword {
                    operand.write32(&mut self.registers, bus, value)?;
                } else {
                    operand.write(&mut self.registers, bus, true, value as u16)?;
                }
                self.flags = updated;
            }
            0xAF => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                let overflow = if dword {
                    let target = Gpr32::decode(modrm >> 3);
                    let lhs = self.registers.get32(target) as i32;
                    let rhs = operand.read32(&self.registers, bus)? as i32;
                    let product = i64::from(lhs) * i64::from(rhs);
                    self.registers.set32(target, product as u32);
                    product != i64::from(product as i32)
                } else {
                    let target = Gpr16::decode(modrm >> 3);
                    let lhs = self.registers.get(target) as i16;
                    let rhs = operand.read(&self.registers, bus, true)? as i16;
                    let product = i32::from(lhs) * i32::from(rhs);
                    self.registers.set(target, product as u16);
                    product != i32::from(product as i16)
                };
                self.flags = (self.flags & !(flags::CARRY | flags::OVERFLOW))
                    | if overflow {
                        flags::CARRY | flags::OVERFLOW
                    } else {
                        0
                    };
            }
            0x06 => {
                if self.protected_mode() && self.registers.cpl != 0 {
                    return Err(CpuError::Descriptor {
                        selector: self.registers.cs,
                        reason: super::DescriptorError::Privilege,
                    });
                }
                self.control.cr0 &= !8; // CLTS.
            }
            0x90..=0x9F => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                let value = u16::from(flags::condition(self.flags, opcode & 15));
                operand.write(&mut self.registers, bus, false, value)?;
            }
            0xB2 | 0xB4 | 0xB5 => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                let Operand::Memory { .. } = operand else {
                    return Err(context.operand(opcode, modrm));
                };
                let address =
                    operand.address(&self.registers, if dword { 6 } else { 4 }, Access::Read)?;
                let value = if dword {
                    decode::read_dword(bus, address)?
                } else {
                    u32::from(bus.read_u16(address)?)
                };
                let selector = bus.read_u16(address + if dword { 4 } else { 2 })?;
                let segment = match opcode {
                    0xB2 => SegmentRegister::Ss,
                    0xB4 => SegmentRegister::Fs,
                    _ => SegmentRegister::Gs,
                };
                self.load_segment(bus, segment, selector)?;
                if dword {
                    self.registers.set32(Gpr32::decode(modrm >> 3), value);
                } else {
                    self.registers.set(Gpr16::decode(modrm >> 3), value as u16);
                }
            }
            0x02 | 0x03 => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                let selector = operand.read(&self.registers, bus, true)?;
                if !self.protected_mode() {
                    return Err(context.opcode(opcode));
                }
                self.flags &= !flags::ZERO;
                match self.descriptor_raw(bus, selector) {
                    Ok(cache) => {
                        let code = cache.access & 0x18 == 0x18;
                        let conforms = code && cache.access & 4 != 0;
                        let accessible = conforms
                            || self.registers.cpl.max((selector & 3) as u8)
                                <= (cache.access >> 5) & 3;
                        let kind = cache.access & 0x1F;
                        let allowed = cache.access & 0x10 != 0
                            || if opcode == 0x02 {
                                matches!(kind, 1 | 2 | 3 | 4 | 5 | 9 | 11 | 12)
                            } else {
                                matches!(kind, 1 | 2 | 3 | 9 | 11)
                            };
                        if accessible && allowed {
                            let value = if opcode == 0x02 {
                                (u32::from(cache.access) << 8) | (u32::from(cache.flags) << 16)
                            } else {
                                cache.limit
                            };
                            if dword {
                                self.registers.set32(Gpr32::decode(modrm >> 3), value);
                            } else {
                                self.registers.set(Gpr16::decode(modrm >> 3), value as u16);
                            }
                            self.flags |= flags::ZERO;
                        }
                    }
                    Err(CpuError::Descriptor { .. }) => {}
                    Err(error) => return Err(error),
                }
            }
            0x00 => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                match (modrm >> 3) & 7 {
                    operation @ 0..=1 => {
                        let value = if operation == 0 {
                            self.control.ldtr_selector
                        } else {
                            self.control.task.map_or(0, |cache| cache.selector)
                        };
                        if dword && matches!(operand, Operand::Register(_)) {
                            operand.write32(&mut self.registers, bus, u32::from(value))?;
                        } else {
                            operand.write(&mut self.registers, bus, true, value)?;
                        }
                    }
                    operation @ 2..=3 => {
                        let selector = operand.read(&self.registers, bus, true)?;
                        if !self.protected_mode() || self.registers.cpl != 0 {
                            return Err(CpuError::Descriptor {
                                selector,
                                reason: super::DescriptorError::Privilege,
                            });
                        }
                        if operation == 2 && selector & !3 == 0 {
                            self.control.ldtr = None;
                            self.control.ldtr_selector = selector;
                        } else {
                            if selector & 4 != 0 {
                                return Err(context.operand(opcode, modrm));
                            }
                            let cache = self.descriptor(bus, selector)?;
                            let kind = cache.access & 0x1F;
                            if operation == 2 {
                                if kind != 2 || cache.limit > 0xFFFF {
                                    return Err(context.operand(opcode, modrm));
                                }
                                self.control.ldtr = Some(crate::DescriptorTable {
                                    base: cache.base,
                                    limit: cache.limit as u16,
                                });
                                self.control.ldtr_selector = selector;
                            } else {
                                if !matches!(kind, 1 | 9) {
                                    return Err(context.operand(opcode, modrm));
                                }
                                bus.write_u8(
                                    self.control
                                        .gdtr
                                        .base
                                        .wrapping_add(u32::from(selector & !7) + 5),
                                    cache.access | 2,
                                )?;
                                self.control.task = Some(crate::SegmentCache {
                                    access: cache.access | 2,
                                    ..cache
                                });
                            }
                        }
                    }
                    _ => return Err(context.operand(opcode, modrm)),
                }
            }
            0xA0 | 0xA8 => {
                let value = if opcode == 0xA0 {
                    self.registers.fs
                } else {
                    self.registers.gs
                };
                if dword {
                    self.push32(bus, u32::from(value))?;
                } else {
                    self.push(bus, value)?;
                }
            }
            0xA1 | 0xA9 => {
                let value = if dword {
                    self.pop32(bus)? as u16
                } else {
                    self.pop(bus)?
                };
                if opcode == 0xA1 {
                    self.load_segment(bus, SegmentRegister::Fs, value)?;
                } else {
                    self.load_segment(bus, SegmentRegister::Gs, value)?;
                }
            }
            0x01 => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                match (modrm >> 3) & 7 {
                    operation @ 0..=3 => {
                        let Operand::Memory { segment, offset } = operand else {
                            return Err(context.operand(opcode, modrm));
                        };
                        let address = self.registers.address(segment, offset, 6, Access::Read)?;
                        if operation < 2 {
                            let table = if operation == 0 {
                                self.control.gdtr
                            } else {
                                self.control.idtr
                            };
                            bus.write_u16(address, table.limit)?;
                            decode::write_dword(bus, address + 2, table.base)?;
                        } else {
                            let limit = bus.read_u16(address)?;
                            let base = decode::read_dword(bus, address + 2)?
                                & if dword { u32::MAX } else { 0xFFFFFF };
                            let table = crate::DescriptorTable { base, limit };
                            if operation == 2 {
                                self.control.gdtr = table;
                            } else {
                                self.control.idtr = table;
                            }
                        }
                    }
                    4 => {
                        if dword && matches!(operand, Operand::Register(_)) {
                            operand.write32(&mut self.registers, bus, self.control.cr0 & 0xFFFF)?;
                        } else {
                            operand.write(
                                &mut self.registers,
                                bus,
                                true,
                                self.control.cr0 as u16,
                            )?;
                        }
                    }
                    6 => {
                        let value = u32::from(operand.read(&self.registers, bus, true)?);
                        self.control.cr0 =
                            (self.control.cr0 & !15) | (value & 15) | (self.control.cr0 & 1);
                    }
                    _ => return Err(context.operand(opcode, modrm)),
                }
            }
            0x20 | 0x22 => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                if modrm >> 6 != 3 {
                    return Err(context.operand(opcode, modrm));
                }
                let register = Gpr32::decode(modrm);
                let index = (modrm >> 3) & 7;
                if opcode == 0x20 {
                    let value = match index {
                        0 => self.control.cr0,
                        2 => self.control.cr2,
                        3 => self.control.cr3,
                        _ => return Err(context.operand(opcode, modrm)),
                    };
                    self.registers.set32(register, value);
                } else {
                    let value = self.registers.get32(register);
                    match index {
                        0 => {
                            if value & 0x80000000 != 0 && value & 1 == 0 {
                                return Err(context.operand(opcode, modrm));
                            }
                            self.control.cr0 = value;
                        }
                        2 => self.control.cr2 = value,
                        3 => self.control.cr3 = value,
                        _ => return Err(context.operand(opcode, modrm)),
                    }
                }
            }
            0xBC | 0xBD => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                let value = if dword {
                    operand.read32(&self.registers, bus)?
                } else {
                    u32::from(operand.read(&self.registers, bus, true)?)
                };
                if value == 0 {
                    self.flags |= flags::ZERO; // Destination is architecturally undefined; retain it.
                } else {
                    self.flags &= !flags::ZERO;
                    let bit = if opcode == 0xBC {
                        value.trailing_zeros()
                    } else {
                        31 - value.leading_zeros()
                    };
                    if dword {
                        self.registers.set32(Gpr32::decode(modrm >> 3), bit);
                    } else {
                        self.registers.set(Gpr16::decode(modrm >> 3), bit as u16);
                    }
                }
            }
            0xBA => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                let operation = (modrm >> 3) & 7;
                if operation < 4 {
                    return Err(context.operand(opcode, modrm));
                }
                let bit = decode::fetch(&mut self.registers, bus)? & if dword { 31 } else { 15 };
                let value = if dword {
                    operand.read32(&self.registers, bus)?
                } else {
                    u32::from(operand.read(&self.registers, bus, true)?)
                };
                let mask = 1 << bit;
                self.flags = (self.flags & !flags::CARRY) | u32::from(value & mask != 0);
                let value = match operation {
                    5 => value | mask,
                    6 => value & !mask,
                    7 => value ^ mask,
                    _ => value,
                };
                if operation != 4 {
                    if dword {
                        operand.write32(&mut self.registers, bus, value)?;
                    } else {
                        operand.write(&mut self.registers, bus, true, value as u16)?;
                    }
                }
            }
            0xB6 | 0xB7 | 0xBE | 0xBF => {
                let modrm = decode::fetch(&mut self.registers, bus)?;
                let operand = decode::operand(
                    &mut self.registers,
                    bus,
                    modrm,
                    context.segment_override,
                    context.address_32,
                )?;
                let word = opcode & 1 != 0;
                let value = operand.read(&self.registers, bus, word)?;
                let value = if opcode & 8 != 0 {
                    if word {
                        value as i16 as i32 as u32
                    } else {
                        value as u8 as i8 as i32 as u32
                    }
                } else {
                    u32::from(value)
                };
                if dword {
                    self.registers.set32(Gpr32::decode(modrm >> 3), value);
                } else {
                    self.registers.set(Gpr16::decode(modrm >> 3), value as u16);
                }
            }
            0x80..=0x8F => {
                if dword {
                    let displacement = decode::fetch_dword(&mut self.registers, bus)?;
                    if flags::condition(self.flags, opcode & 15) {
                        self.jump32(self.registers.ip.wrapping_add(displacement), context)?;
                    }
                } else {
                    let displacement = decode::fetch_word(&mut self.registers, bus)? as i16;
                    if flags::condition(self.flags, opcode & 15) {
                        self.near_jump(u32::from(
                            (self.registers.ip as u16).wrapping_add_signed(displacement),
                        ))?;
                    }
                }
            }
            _ => {
                return Err(CpuError::UnsupportedExtendedOpcode {
                    cs: context.cs,
                    ip: context.ip,
                    opcode,
                });
            }
        }
        Ok(StepOutcome::Continue)
    }
}
