//! Atomic strided copy: span proofs once, then native byte/plane locals only.

use super::super::procedure_memory_emit::{COMPLETED, EXIT_STATUS, PC, finish, initialize};
use super::*;
use crate::static_program::procedure_memory::{
    COMPLETE, CopyKernel, ProcedureCompileError, SIDE_EXIT,
};

// Reuse the ordinary procedure's scratch-local ABI. No scratch survives calls.
const COUNT: u32 = QUOTA;
const SOURCE: u32 = FIRST_PAGE;
const DESTINATION: u32 = LAST_PAGE;
const RETURN_TARGET: u32 = STORE_SOURCE;
const SPAN: u32 = STORE_MASK;
const CALL_WORK: u32 = DEVICE_WRITE_SIMPLE;
const REMAINING: u32 = DEVICE_WRITE_KIND;
const CHANGES: u32 = DEVICE_WRITE_CHAIN4;

fn span_segment(f: &mut Function, register: Register, write: bool) {
    let window = m::SEGMENTS + (u32::from(write) * 6 + m::Segment::Ds as u32) * m::SEGMENT_STRIDE;
    descriptor(f, window);
    c(f, 1);
    ins(f, W::I32Eq);
    require(f, 0);
    read_register(f, register);
    descriptor(f, window + 8);
    ins(f, W::I32GeU);
    require(f, 0);
    read_register(f, register);
    ins(f, W::I64ExtendI32U);
    local_wide(f, SPAN);
    ins(f, W::I64Add);
    wide(f, 1);
    ins(f, W::I64Sub);
    descriptor_wide(f, window + 12);
    ins(f, W::I64LeU);
    require(f, 0);
    read_register(f, register);
    ins(f, W::I64ExtendI32U);
    descriptor_wide(f, window + 4);
    ins(f, W::I64Add);
    ins(f, W::LocalTee(WIDE));
    local_wide(f, SPAN);
    ins(f, W::I64Add);
    wide(f, 1u64 << 32);
    ins(f, W::I64LeU);
    require(f, 0);
    get(f, WIDE);
    ins(f, W::I32WrapI64);
    set(f, LINEAR);
}

fn span_contains(f: &mut Function, base: u32, length: u32) {
    get(f, LINEAR);
    descriptor(f, base);
    ins(f, W::I32GeU);
    require(f, 0);
    get(f, LINEAR);
    descriptor(f, base);
    ins(f, W::I32Sub);
    ins(f, W::I64ExtendI32U);
    local_wide(f, SPAN);
    ins(f, W::I64Add);
    descriptor_wide(f, length);
    ins(f, W::I64LeU);
    require(f, 0);
}

fn span_excludes(f: &mut Function, start: u32, length: u32) {
    descriptor(f, length);
    ins(f, W::I32Eqz);
    local_wide(f, LINEAR);
    descriptor_wide(f, start);
    descriptor_wide(f, length);
    ins(f, W::I64Add);
    ins(f, W::I64GeU);
    ins(f, W::I32Or);
    local_wide(f, LINEAR);
    local_wide(f, SPAN);
    ins(f, W::I64Add);
    descriptor_wide(f, start);
    ins(f, W::I64LeU);
    ins(f, W::I32Or);
    require(f, 0);
}

fn preflight(f: &mut Function, kernel: CopyKernel) {
    preflight_descriptor(f);
    get(f, DEVICE_WRITE_VALID);
    require(f, 0);
    get(f, DEVICE_WRITE_KIND);
    c(f, DeviceReadKind::FourLanes as u32);
    ins(f, W::I32Eq);
    require(f, 0);
    get(f, DEVICE_WRITE_CHAIN4);
    ins(f, W::I32Eqz);
    require(f, 0);
    get(f, DEVICE_WRITE_SIMPLE);
    require(f, 0);

    read_register(f, Register::Ecx);
    ins(f, W::LocalTee(COUNT));
    ins(f, W::I32Eqz);
    ins(f, W::I32Eqz);
    require(f, 0);
    get(f, COUNT);
    c(f, 1);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Result(ValType::I64)));
    wide(f, 6);
    ins(f, W::Else);
    get(f, COUNT);
    c(f, 1);
    ins(f, W::I32ShrU);
    ins(f, W::I64ExtendI32U);
    wide(f, 7);
    ins(f, W::I64Mul);
    wide(f, 6 + u64::from(kernel.prefix_nops));
    ins(f, W::I64Add);
    get(f, COUNT);
    c(f, 1);
    ins(f, W::I32And);
    ins(f, W::I64ExtendI32U);
    wide(f, 2 + u64::from(kernel.tail_nops));
    ins(f, W::I64Mul);
    ins(f, W::I64Add);
    ins(f, W::End);
    ins(f, W::LocalTee(WIDE));
    wide(f, u64::from(kernel.max_work_per_call));
    ins(f, W::I64LeU);
    require(f, 0);
    get(f, WIDE);
    descriptor_wide(f, m::INLINE_GRANT);
    ins(f, W::I64LeU);
    require(f, 0);
    get(f, WIDE);
    ins(f, W::I32WrapI64);
    set(f, CALL_WORK);

    // Full ordinary source RAM, including excluded holes and the device range.
    get(f, COUNT);
    set(f, SPAN);
    span_segment(f, Register::Esi, false);
    span_contains(f, m::RAM_BASE, m::RAM_LENGTH);
    span_excludes(f, m::RAM_EXCLUDED_START, m::RAM_EXCLUDED_LENGTH);
    span_excludes(f, m::DEVICE_START, m::DEVICE_LENGTH);
    descriptor_wide(f, m::RAM_BASE);
    descriptor_wide(f, m::RAM_LENGTH);
    ins(f, W::I64Add);
    wide(f, 1u64 << 32);
    ins(f, W::I64LeU);
    require(f, 0);
    table_bounds(f, m::RAM_POINTER, m::RAM_LENGTH, 1);
    require(f, 0);
    descriptor(f, m::RAM_POINTER);
    get(f, LINEAR);
    descriptor(f, m::RAM_BASE);
    ins(f, W::I32Sub);
    ins(f, W::I32Add);
    set(f, SOURCE);

    // The strided destination's complete extent is monotonic and nonwrapping.
    get(f, COUNT);
    c(f, 1);
    ins(f, W::I32Sub);
    ins(f, W::I64ExtendI32U);
    wide(f, u64::from(kernel.stride));
    ins(f, W::I64Mul);
    wide(f, 1);
    ins(f, W::I64Add);
    ins(f, W::LocalTee(WIDE));
    wide(f, u64::from(u32::MAX));
    ins(f, W::I64LeU);
    require(f, 0);
    get(f, WIDE);
    ins(f, W::I32WrapI64);
    set(f, SPAN);
    span_segment(f, Register::Edi, true);
    span_contains(f, m::DEVICE_START, m::DEVICE_LENGTH);
    get(f, LINEAR);
    descriptor(f, m::DEVICE_START);
    ins(f, W::I32Sub);
    set(f, OFFSET);
    local_wide(f, OFFSET);
    local_wide(f, SPAN);
    ins(f, W::I64Add);
    wide(f, 4);
    ins(f, W::I64Mul);
    descriptor_wide(f, m::DEVICE_BACKING_LENGTH);
    ins(f, W::I64LeU);
    require(f, 0);
    descriptor(f, m::DEVICE_POINTER);
    get(f, OFFSET);
    c(f, 2);
    ins(f, W::I32Shl);
    ins(f, W::I32Add);
    set(f, DESTINATION);
}

fn final_registers(f: &mut Function, kernel: CopyKernel) {
    // Source RAM cannot alias VGA, so these final reads may precede the loop.
    get(f, COUNT);
    c(f, 2);
    ins(f, W::I32GeU);
    ins(f, W::If(BlockType::Empty));
    get(f, SOURCE);
    get(f, COUNT);
    c(f, !1);
    ins(f, W::I32And);
    c(f, 2);
    ins(f, W::I32Sub);
    ins(f, W::I32Add);
    ins(
        f,
        W::I32Load16U(MemArg {
            align: 0,
            ..arg(0, false)
        }),
    );
    set(f, RESULT);
    write_register(f, Register::Ax, RESULT);
    ins(f, W::End);
    get(f, COUNT);
    c(f, 1);
    ins(f, W::I32And);
    ins(f, W::If(BlockType::Empty));
    get(f, SOURCE);
    get(f, COUNT);
    ins(f, W::I32Add);
    c(f, 1);
    ins(f, W::I32Sub);
    ins(
        f,
        W::I32Load8U(MemArg {
            align: 0,
            ..arg(0, false)
        }),
    );
    set(f, RESULT);
    write_register(f, Register::Al, RESULT);
    ins(f, W::End);

    // Preserve final ABI register values and compute architectural flags once.
    get(f, COUNT);
    c(f, !1);
    ins(f, W::I32And);
    set(f, RIGHT);
    read_register(f, Register::Esi);
    get(f, RIGHT);
    ins(f, W::I32Add);
    set(f, GPR + Register::Esi.index());
    read_register(f, Register::Edi);
    get(f, RIGHT);
    c(f, kernel.stride);
    ins(f, W::I32Mul);
    ins(f, W::I32Add);
    set(f, GPR + Register::Edi.index());
    get(f, COUNT);
    c(f, 1);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Empty));
    c(f, 1);
    set(f, GPR + Register::Ebx.index());
    operation(
        f,
        Operation::Shift {
            target: Register::Ebx,
            kind: crate::static_program::Shift::Shr,
            count: crate::static_program::ShiftCount::One,
        },
    );
    ins(f, W::Else);
    c(f, 0);
    set(f, GPR + Register::Ebx.index());
    operation(
        f,
        Operation::Alu {
            target: Register::Ecx,
            kind: Alu::And,
            source: Source::Immediate(1),
        },
    );
    ins(f, W::End);

    // SS.B is independent of operand width; RET reads were proved before stores.
    read_register(f, Register::Esp);
    c(f, 4);
    ins(f, W::I32Add);
    set(f, RESULT);
    descriptor(f, m::STACK_ADDRESS);
    c(f, m::StackAddress::Bits16 as u32);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Empty));
    write_register(f, Register::Sp, RESULT);
    ins(f, W::Else);
    write_register(f, Register::Esp, RESULT);
    ins(f, W::End);
}

fn copy_loop(f: &mut Function, kernel: CopyKernel) {
    get(f, COUNT);
    set(f, REMAINING);
    c(f, 0);
    set(f, CHANGES);
    ins(f, W::Loop(BlockType::Empty));
    get(f, SOURCE);
    ins(
        f,
        W::I32Load8U(MemArg {
            align: 0,
            ..arg(0, false)
        }),
    );
    set(f, BYTE);
    for plane in 0..4 {
        get(f, DEVICE_WRITE_PLANE_MASK);
        c(f, 1 << plane);
        ins(f, W::I32And);
        ins(f, W::If(BlockType::Empty));
        get(f, CHANGES);
        get(f, DESTINATION);
        ins(
            f,
            W::I32Load8U(MemArg {
                align: 0,
                ..arg(plane, false)
            }),
        );
        get(f, BYTE);
        ins(f, W::I32Ne);
        ins(f, W::I32Add);
        set(f, CHANGES);
        get(f, DESTINATION);
        get(f, BYTE);
        ins(
            f,
            W::I32Store8(MemArg {
                align: 0,
                ..arg(plane, false)
            }),
        );
        ins(f, W::End);
    }
    get(f, SOURCE);
    c(f, 1);
    ins(f, W::I32Add);
    set(f, SOURCE);
    get(f, DESTINATION);
    c(f, kernel.stride.wrapping_mul(4));
    ins(f, W::I32Add);
    set(f, DESTINATION);
    get(f, REMAINING);
    c(f, 1);
    ins(f, W::I32Sub);
    ins(f, W::LocalTee(REMAINING));
    ins(f, W::BrIf(0));
    ins(f, W::End);
    for (field, local) in [(m::DEVICE_WRITES, COUNT), (m::DEVICE_CHANGES, CHANGES)] {
        get(f, FRAME);
        descriptor_wide_counter(f, field);
        local_wide(f, local);
        ins(f, W::I64Add);
        ins(f, W::I64Store(arg(m::OFFSET + field, true)));
    }
}

fn descriptor_wide_counter(f: &mut Function, field: u32) {
    load(f, m::OFFSET + field, true);
}

pub(in crate::static_program) fn body(
    entry: Address,
    kernel: CopyKernel,
    max_output: usize,
) -> Result<Function, ProcedureCompileError> {
    let mut f = initialize(true);
    c(&mut f, entry.offset);
    set(&mut f, PC);
    c(&mut f, SIDE_EXIT);
    set(&mut f, EXIT_STATUS);
    ins(&mut f, W::Block(BlockType::Empty));
    // Read the RET target early without changing ESP, GPRs, flags or effects.
    stack_emit::procedure_preflight(&mut f, Width::Dword, false, |f| ins(f, W::Br(1)));
    load_ram_value(&mut f, Width::Dword);
    get(&mut f, RESULT);
    set(&mut f, RETURN_TARGET);
    preflight_with(&mut f, |f| preflight(f, kernel), |f| ins(f, W::Br(1)));
    final_registers(&mut f, kernel);
    copy_loop(&mut f, kernel);
    get(&mut f, CALL_WORK);
    set(&mut f, COMPLETED);
    get(&mut f, RETURN_TARGET);
    set(&mut f, PC);
    c(&mut f, COMPLETE);
    set(&mut f, EXIT_STATUS);
    ins(&mut f, W::End);
    finish(&mut f, entry);
    if f.byte_len() > max_output {
        return Err(ProcedureCompileError::OutputTooLarge);
    }
    Ok(f)
}
