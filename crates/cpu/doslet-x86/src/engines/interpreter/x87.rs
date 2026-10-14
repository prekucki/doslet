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
                (0xD9, 0xEE) => self.fpu.as_mut().unwrap().push(0.0).map_err(stack_error)?,
                (0xD9, 0xE8) => self.fpu.as_mut().unwrap().push(1.0).map_err(stack_error)?,
                (0xD9, 0xD0) => {} // FNOP.
                (0xDB, 0xE3) => self.fpu.as_mut().unwrap().initialize(),
                (0xDB, 0xE2) => self.fpu.as_mut().unwrap().clear_exceptions(),
                (0xDB, 0xE4) => {} // FSETPM is an obsolete 287 operation, ignored by 387.
                (0xDF, 0xE0) => self
                    .registers
                    .set(Gpr16::Ax, self.fpu.as_ref().unwrap().status()),
                _ => {
                    return Err(CpuError::UnsupportedOperand {
                        cs: context.cs,
                        ip: context.ip,
                        opcode,
                        modrm,
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
                        return Err(CpuError::UnsupportedOperand {
                            cs: context.cs,
                            ip: context.ip,
                            opcode,
                            modrm,
                        });
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
                    let value = self.fpu.as_ref().unwrap().st0().map_err(stack_error)?;
                    if opcode == 0xD9 && f64::from(value as f32) != value {
                        return Err(CpuError::UnsupportedOperand {
                            cs: context.cs,
                            ip: context.ip,
                            opcode,
                            modrm,
                        });
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
                    return Err(CpuError::UnsupportedOperand {
                        cs: context.cs,
                        ip: context.ip,
                        opcode,
                        modrm,
                    });
                }
            }
        }
        Ok(StepOutcome::Continue)
    }
}
