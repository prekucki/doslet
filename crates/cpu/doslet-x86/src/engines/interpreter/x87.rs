use super::{Cpu, CpuError, StepOutcome, operand32::Context};
use crate::{Access, Gpr16, decode};
use doslet_bus::CpuBus;
impl Cpu {
    pub(super) fn x87_step<B: CpuBus>(
        &mut self,
        bus: &mut B,
        opcode: u8,
        context: Context,
    ) -> Result<StepOutcome, CpuError> {
        if self.fpu.is_none() || self.control.cr0 & 12 != 0 {
            return Err(CpuError::X87Unavailable {
                cs: context.cs,
                ip: context.ip,
            });
        }
        let modrm = decode::fetch(&mut self.registers, bus)?;
        let instruction = match crate::x87_decode::decode(opcode, modrm) {
            crate::x87_decode::Encoding::I387(instruction) if instruction.implemented() => {
                instruction
            }
            crate::x87_decode::Encoding::I387(instruction)
            | crate::x87_decode::Encoding::LaterIsa(instruction) => {
                return Err(CpuError::X87Unsupported {
                    cs: context.cs,
                    ip: context.ip,
                    opcode,
                    modrm,
                    instruction,
                    later_isa: matches!(
                        crate::x87_decode::decode(opcode, modrm),
                        crate::x87_decode::Encoding::LaterIsa(_)
                    ),
                });
            }
            _ => {
                return Err(CpuError::X87Reserved {
                    cs: context.cs,
                    ip: context.ip,
                    opcode,
                    modrm,
                });
            }
        };
        let stack_error = |reason| CpuError::X87Stack {
            cs: context.cs,
            ip: context.ip,
            reason,
        };
        let arithmetic_error = |reason| CpuError::X87Arithmetic {
            cs: context.cs,
            ip: context.ip,
            reason,
        };
        if modrm >> 6 == 3 {
            match (opcode, modrm) {
                (0xDC, 0xC0..=0xC7) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .add_register(modrm & 7)
                    .map_err(arithmetic_error)?,
                (0xDC, 0xC8..=0xCF | 0xE0..=0xEF) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .arithmetic_destination(modrm & 7, modrm < 0xD0, modrm & 8 == 0, false)
                    .map_err(arithmetic_error)?,
                (0xDC, 0xF0..=0xFF) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .divide_destination(modrm & 7, modrm & 8 == 0)
                    .map_err(arithmetic_error)?,
                (0xD8, 0xD0..=0xDF) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .compare_register(modrm & 7, if modrm & 8 == 0 { 0 } else { 1 })
                    .map_err(arithmetic_error)?,
                (0xDD, 0xC0..=0xC7) => self.fpu.as_mut().unwrap().free(modrm & 7),
                (0xD9, 0xF6..=0xF7) => self.fpu.as_mut().unwrap().rotate_top(modrm & 1 != 0),
                (0xD9, 0xE1) => self.fpu.as_mut().unwrap().absolute().map_err(stack_error)?,
                (0xD9, 0xE4) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .test_zero()
                    .map_err(arithmetic_error)?,
                (0xDD, 0xD0..=0xDF) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .store_register(modrm & 7, modrm & 8 != 0)
                    .map_err(stack_error)?,
                (0xDE, 0xC0..=0xC7) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .add_pop(modrm & 7)
                    .map_err(arithmetic_error)?,
                (0xD8, 0xC0..=0xCF | 0xE0..=0xEF) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .arithmetic_register(modrm & 7, (modrm >> 3) & 7)
                    .map_err(arithmetic_error)?,
                (0xD9, 0xFC) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .round_integer()
                    .map_err(arithmetic_error)?,
                (0xD9, 0xF1) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .y_log2_x()
                    .map_err(arithmetic_error)?,
                (0xD9, 0xF0) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .exp2_minus_one()
                    .map_err(arithmetic_error)?,
                (0xD9, 0xFD) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .scale()
                    .map_err(arithmetic_error)?,
                (0xDE, 0xC8..=0xCF | 0xE0..=0xEF) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .arithmetic_pop(modrm & 7, modrm < 0xD0, modrm & 8 == 0)
                    .map_err(arithmetic_error)?,
                (0xD8, 0xF0..=0xFF) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .divide(modrm & 7, modrm & 8 != 0)
                    .map_err(arithmetic_error)?,
                (0xD9, 0xC8..=0xCF) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .exchange(modrm & 7)
                    .map_err(stack_error)?,
                (0xDE, 0xF0..=0xFF) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .divide_pop(modrm & 7, modrm & 8 == 0)
                    .map_err(arithmetic_error)?,
                (0xDE, 0xD9) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .compare_pop_twice()
                    .map_err(arithmetic_error)?,
                (0xD9, 0xC0..=0xC7) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .duplicate(modrm & 7)
                    .map_err(stack_error)?,
                (0xD9, 0xE0) => self
                    .fpu
                    .as_mut()
                    .unwrap()
                    .change_sign()
                    .map_err(stack_error)?,
                (0xD9, 0xE8..=0xEE) => {
                    use doslet_x87::Constant::*;
                    self.fpu
                        .as_mut()
                        .unwrap()
                        .load_constant(
                            [One, Log2Ten, Log2E, Pi, Log10Two, LnTwo, Zero]
                                [usize::from(modrm - 0xE8)],
                        )
                        .map_err(stack_error)?;
                }
                (0xD9, 0xD0) => {} // FNOP.
                (0xDB, 0xE3) => self.fpu.as_mut().unwrap().initialize(),
                (0xDB, 0xE2) => self.fpu.as_mut().unwrap().clear_exceptions(),
                (0xDB, 0xE0 | 0xE1 | 0xE4) => {} // Obsolete 8087/287 controls are NOPs on 387.
                (0xDF, 0xE0) => self
                    .registers
                    .set(Gpr16::Ax, self.fpu.as_ref().unwrap().status()),
                _ => {
                    return Err(CpuError::X87Unsupported {
                        cs: context.cs,
                        ip: context.ip,
                        opcode,
                        modrm,
                        instruction,
                        later_isa: false,
                    });
                }
            }
        } else {
            let operand = decode::operand(
                &mut self.registers,
                bus,
                modrm,
                context.segment_override,
                context.address_32,
            )?;
            match (opcode, (modrm >> 3) & 7) {
                (0xDA | 0xDE, operation) => {
                    let width = if opcode == 0xDA { 4 } else { 2 };
                    let address = operand.address(&self.registers, width, Access::Read)?;
                    let value = if width == 4 {
                        i64::from(decode::read_dword(bus, address)? as i32)
                    } else {
                        i64::from(bus.read_u16(address)? as i16)
                    };
                    let fpu = self.fpu.as_mut().unwrap();
                    if operation == 2 || operation == 3 {
                        fpu.compare_integer(value, operation == 3)
                            .map_err(arithmetic_error)?;
                    } else {
                        fpu.arithmetic_integer(value, operation)
                            .map_err(arithmetic_error)?;
                    }
                }
                (0xDB | 0xDF, operation @ (2 | 3)) | (0xDF, operation @ 7) => {
                    let width = if operation == 7 {
                        8
                    } else if opcode == 0xDB {
                        4
                    } else {
                        2
                    };
                    let address = operand.address(&self.registers, width, Access::Write)?;
                    let value = self
                        .fpu
                        .as_mut()
                        .unwrap()
                        .store_integer(width * 8)
                        .map_err(arithmetic_error)?;
                    if width == 2 {
                        bus.write_u16(address, value as u16)?;
                    } else {
                        decode::write_dword(bus, address, value as u32)?;
                        if width == 8 {
                            decode::write_dword(bus, address + 4, (value >> 32) as u32)?;
                        }
                    }
                    if operation != 2 {
                        self.fpu.as_mut().unwrap().pop().map_err(stack_error)?;
                    }
                }
                (0xDF, 0 | 5) | (0xDB, 0) => {
                    let width = if opcode == 0xDB {
                        4
                    } else if modrm & 0x38 == 0 {
                        2
                    } else {
                        8
                    };
                    let address = operand.address(&self.registers, width, Access::Read)?;
                    let value = if width == 2 {
                        i64::from(bus.read_u16(address)? as i16)
                    } else {
                        let low = decode::read_dword(bus, address)?;
                        if width == 4 {
                            i64::from(low as i32)
                        } else {
                            let high = decode::read_dword(bus, address + 4)?;
                            (u64::from(low) | (u64::from(high) << 32)) as i64
                        }
                    };
                    self.fpu
                        .as_mut()
                        .unwrap()
                        .push_integer(value)
                        .map_err(stack_error)?;
                }
                (0xD8 | 0xDC, operation) => {
                    let address = operand.address(
                        &self.registers,
                        if opcode == 0xD8 { 4 } else { 8 },
                        Access::Read,
                    )?;
                    let low = decode::read_dword(bus, address)?;
                    let value = if opcode == 0xD8 {
                        f64::from(f32::from_bits(low))
                    } else {
                        let high = decode::read_dword(bus, address + 4)?;
                        f64::from_bits(u64::from(low) | (u64::from(high) << 32))
                    };
                    let fpu = self.fpu.as_mut().unwrap();
                    if operation == 2 || operation == 3 {
                        fpu.compare_memory(value, operation == 3)
                            .map_err(arithmetic_error)?;
                    } else {
                        fpu.arithmetic_memory(value, operation)
                            .map_err(arithmetic_error)?;
                    }
                }
                (0xD9 | 0xDD, 0) => {
                    let address = operand.address(
                        &self.registers,
                        if opcode == 0xD9 { 4 } else { 8 },
                        Access::Read,
                    )?;
                    let low = decode::read_dword(bus, address)?;
                    let value = if opcode == 0xD9 {
                        f64::from(f32::from_bits(low))
                    } else {
                        let high = decode::read_dword(bus, address + 4)?;
                        f64::from_bits(u64::from(low) | (u64::from(high) << 32))
                    };
                    if !value.is_finite() {
                        return Err(arithmetic_error(doslet_x87::ArithmeticError::Unsupported));
                    }
                    self.fpu
                        .as_mut()
                        .unwrap()
                        .push(value)
                        .map_err(stack_error)?;
                }
                (0xD9 | 0xDD, operation @ (2 | 3)) => {
                    let address = operand.address(
                        &self.registers,
                        if opcode == 0xD9 { 4 } else { 8 },
                        Access::Write,
                    )?;
                    let value = self
                        .fpu
                        .as_mut()
                        .unwrap()
                        .store_float(opcode == 0xD9)
                        .map_err(arithmetic_error)?;
                    if opcode == 0xD9 && f64::from(value as f32) != value {
                        return Err(arithmetic_error(doslet_x87::ArithmeticError::Unsupported));
                    }
                    if opcode == 0xD9 {
                        decode::write_dword(bus, address, (value as f32).to_bits())?;
                    } else {
                        let bits = value.to_bits();
                        decode::write_dword(bus, address, bits as u32)?;
                        decode::write_dword(bus, address + 4, (bits >> 32) as u32)?;
                    }
                    if operation == 3 {
                        self.fpu.as_mut().unwrap().pop().map_err(stack_error)?;
                    }
                }
                (0xD9, 5) => {
                    let value = operand.read(&self.registers, bus, true)?;
                    self.fpu.as_mut().unwrap().load_control(value);
                }
                (0xD9, 7) => operand.write(
                    &mut self.registers,
                    bus,
                    true,
                    self.fpu.as_ref().unwrap().control(),
                )?,
                (0xDD, 7) => operand.write(
                    &mut self.registers,
                    bus,
                    true,
                    self.fpu.as_ref().unwrap().status(),
                )?,
                _ => {
                    return Err(CpuError::X87Unsupported {
                        cs: context.cs,
                        ip: context.ip,
                        opcode,
                        modrm,
                        instruction,
                        later_isa: false,
                    });
                }
            }
        }
        Ok(StepOutcome::Continue)
    }
}
