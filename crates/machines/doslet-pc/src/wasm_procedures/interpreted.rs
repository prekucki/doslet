//! Native reference for the same complete finite-copy admission used by AOT.
use super::*;
use doslet_x86::{StepOutcome, flags, procedure_calls::ProcedureDecline};

fn span(cpu: &Cpu, register: Gpr32, access: Access, length: u32) -> Option<(usize, usize)> {
    let (base, first, last) = cpu.registers.window(SegmentRegister::Ds, access)?.parts();
    let offset = cpu.registers.get32(register);
    let end = offset.checked_add(length.checked_sub(1)?)?;
    if offset < first || end > last {
        return None;
    }
    let linear = u64::from(base) + u64::from(offset);
    let end = linear + u64::from(length);
    (end <= 1u64 << 32).then_some((linear as usize, end as usize))
}

fn return_target(cpu: &Cpu, bus: &PcBus, kernel: CopyKernel) -> Option<u32> {
    let count = cpu.registers.get32(Gpr32::Ecx);
    let (start, end) = span(cpu, Gpr32::Esi, Access::Read, count)?;
    // Match the shared-memory descriptor: low firmware RAM and the complete
    // VGA/ROM hole are excluded, even when the ordinary bus can read them.
    if start < 0x500 || (start < 0xc0000 && end > 0xa0000) {
        return None;
    }
    bus.ram_view(start..end)?;
    let extent = (count - 1).checked_mul(kernel.stride)?.checked_add(1)?;
    let (start, end) = span(cpu, Gpr32::Edi, Access::Write, extent)?;
    if start < doslet_vga::APERTURE_START as usize
        || end > doslet_vga::APERTURE_START as usize + doslet_vga::APERTURE_LEN
    {
        return None;
    }
    let view = bus.vga.direct_read_view_with_memory(bus.vram_view())?;
    if view.planar?.chain4 {
        return None;
    }
    let write = bus.vga.direct_write_controls();
    if write.mode != 0 || write.enable_set_reset != 0 || write.rotate != 0 || write.bit_mask != 255
    {
        return None;
    }
    let stack = cpu
        .registers
        .address(
            SegmentRegister::Ss,
            cpu.registers.get32(Gpr32::Esp),
            4,
            Access::Read,
        )
        .ok()? as usize;
    let end = stack.checked_add(4)?;
    if stack < 0x500 || (stack < 0xc0000 && end > 0xa0000) {
        return None;
    }
    let target = u32::from_le_bytes(bus.ram_view(stack..end)?.try_into().ok()?);
    cpu.registers
        .address(SegmentRegister::Cs, target, 1, Access::Execute)
        .ok()?;
    Some(target)
}

pub(super) fn execute(
    cpu: &mut Cpu,
    bus: &mut PcBus,
    kernel: Option<CopyKernel>,
    rle_return: Option<u32>,
    finite: Option<&FiniteComputation>,
    work: u32,
) -> Result<MemoryProcedureOutcome, MemoryProcedureError<String>> {
    // Keep the transient eligibility rules of Cpu::try_call_memory_procedure.
    let reason = if cpu.is_halted() {
        Some(ProcedureDecline::Halted)
    } else if cpu.flags & (flags::TRAP | flags::RESUME | flags::VIRTUAL) != 0 {
        Some(ProcedureDecline::DebugOrVirtual8086)
    } else if cpu.has_interrupt_shadow() {
        Some(ProcedureDecline::InterruptShadow)
    } else {
        None
    };
    if let Some(reason) = reason {
        return Ok(MemoryProcedureOutcome::Declined(reason));
    }
    if let Some(finite) = finite {
        let mut instructions = 0;
        loop {
            for guard in &finite.guards {
                if guard.entry_ip == cpu.registers.ip {
                    let register = [
                        Gpr32::Eax,
                        Gpr32::Ecx,
                        Gpr32::Edx,
                        Gpr32::Ebx,
                        Gpr32::Esp,
                        Gpr32::Ebp,
                        Gpr32::Esi,
                        Gpr32::Edi,
                    ][guard.counter as usize];
                    let value = cpu.registers.get32(register);
                    let trips = if guard.decrement {
                        value
                    } else {
                        value.wrapping_neg()
                    };
                    if trips == 0 || trips > guard.maximum_trips {
                        return Ok(MemoryProcedureOutcome::SideExit { instructions });
                    }
                }
            }
            if finite.exits.binary_search(&cpu.registers.ip).is_ok() {
                return Ok(MemoryProcedureOutcome::SideExit { instructions });
            }
            if instructions == work {
                return Err(MemoryProcedureError::Backend(
                    "finite interpreted computation exceeded its proved bound".into(),
                ));
            }
            let returning = finite.returns.binary_search(&cpu.registers.ip).is_ok();
            let outcome = cpu.step(bus).map_err(|error| {
                MemoryProcedureError::Backend(alloc::format!("finite computation: {error}"))
            })?;
            if outcome != StepOutcome::Continue {
                return Err(MemoryProcedureError::Backend(
                    "finite computation halted".into(),
                ));
            }
            instructions += 1;
            if returning {
                return Ok(MemoryProcedureOutcome::Complete { instructions });
            }
        }
    }
    let target = rle_return.or_else(|| kernel.and_then(|kernel| return_target(cpu, bus, kernel)));
    let Some(target) = target else {
        return Ok(MemoryProcedureOutcome::Declined(
            ProcedureDecline::ReturnStack,
        ));
    };
    let stack = cpu.registers.get32(Gpr32::Esp);
    let cleanup = if rle_return.is_some() { 12 } else { 4 };
    let flags = cpu.flags;
    let mut remaining = work;
    while remaining != 0 {
        let completed = cpu.interpret_batch(bus, remaining.min(256), |_| remaining.min(256));
        remaining -= completed;
        if completed == 0 {
            let outcome = cpu.step(bus).map_err(|error| {
                MemoryProcedureError::Backend(alloc::format!("interpreted procedure: {error}"))
            })?;
            if outcome != StepOutcome::Continue {
                return Err(MemoryProcedureError::Backend(
                    "interpreted procedure halted".into(),
                ));
            }
            remaining -= 1;
        }
    }
    let invalid = if cpu.registers.ip != target
        || cpu.registers.get32(Gpr32::Esp) != stack.wrapping_add(cleanup)
    {
        Some(MemoryProcedureResultError::InstructionPointer)
    } else if (flags ^ cpu.flags)
        & !(flags::CARRY
            | flags::PARITY
            | flags::AUXILIARY
            | flags::ZERO
            | flags::SIGN
            | flags::OVERFLOW)
        != 0
    {
        Some(MemoryProcedureResultError::ControlFlags)
    } else {
        None
    };
    if let Some(reason) = invalid {
        return Err(MemoryProcedureError::InvalidResult(reason));
    }
    Ok(MemoryProcedureOutcome::Complete { instructions: work })
}
