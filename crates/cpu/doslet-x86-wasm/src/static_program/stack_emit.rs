//! Ordinary-RAM register/immediate stack operations. The live descriptor SS.B
//! capability is separate from both the operand width and code/address widths.

use super::*;
use crate::static_program::memory::{self as m, StackAddress};

fn stack_mode(f: &mut Function) {
    load(f, m::OFFSET + m::STACK_ADDRESS, false);
}

pub(super) fn preflight(
    f: &mut Function,
    checked: &Checked<'_>,
    r: &Region,
    width: Width,
    push: bool,
) {
    memory_emit::preflight_ram_access(f, checked, r, width, push, |f| {
        stack_offset(f, width, push);
    });
}

fn stack_offset(f: &mut Function, width: Width, push: bool) {
    stack_mode(f);
    c(f, StackAddress::Bits16 as u32);
    ins(f, W::I32Eq);
    stack_mode(f);
    c(f, StackAddress::Bits32 as u32);
    ins(f, W::I32Eq);
    ins(f, W::I32Or);
    ins(f, W::I32Eqz);
    ins(f, W::BrIf(0)); // shared preflight refusal, before any effects

    read_register(f, Register::Esp);
    if push {
        c(f, width.bits() / 8);
        ins(f, W::I32Sub);
    }
    stack_mode(f);
    c(f, StackAddress::Bits16 as u32);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Result(ValType::I32)));
    c(f, 0xffff);
    ins(f, W::Else);
    c(f, u32::MAX);
    ins(f, W::End);
    ins(f, W::I32And);
}

pub(super) fn procedure_preflight(
    f: &mut Function,
    width: Width,
    push: bool,
    refused: impl FnOnce(&mut Function),
) {
    memory_emit::preflight_with(
        f,
        |f| {
            memory_emit::preflight_descriptor(f);
            stack_offset(f, width, push);
            set(f, memory_emit::EA);
            memory_emit::preflight_operand(
                f,
                width,
                m::Segment::Ss,
                if push {
                    memory_emit::Access::Write
                } else {
                    memory_emit::Access::RamRead
                },
            );
            if push {
                memory_emit::procedure_store_layout(f);
            }
        },
        refused,
    );
}

fn adjust_pointer(f: &mut Function, width: Width, push: bool) {
    adjust_pointer_bytes(f, width.bits() / 8, push);
}

fn adjust_pointer_bytes(f: &mut Function, bytes: u32, push: bool) {
    read_register(f, Register::Esp);
    c(f, bytes);
    ins(f, if push { W::I32Sub } else { W::I32Add });
    set(f, LEFT);
    stack_mode(f);
    c(f, StackAddress::Bits16 as u32);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Empty));
    write_register(f, Register::Sp, LEFT);
    ins(f, W::Else);
    write_register(f, Register::Esp, LEFT);
    ins(f, W::End);
}

pub(super) fn push(f: &mut Function, width: Width, value: Source) {
    // Store the source before changing ESP: PUSH SP/ESP captures its old value.
    memory_emit::store_memory(f, width, value);
    adjust_pointer(f, width, true);
}

pub(super) fn pop(f: &mut Function, target: Register) {
    memory_emit::load_ram_value(f, target.width());
    adjust_pointer(f, target.width(), false);
    // Assignment follows increment, including aliases of the stack pointer.
    write_register(f, target, RESULT);
}

pub(super) fn near_return(f: &mut Function, width: Width, pop_bytes: u16) {
    // Only the return address is read. The extra adjustment need not describe
    // backed or segment-valid memory and wraps independently in the SS width.
    memory_emit::load_ram_value(f, width);
    adjust_pointer_bytes(f, width.bits() / 8 + u32::from(pop_bytes), false);
}
