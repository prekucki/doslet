//! Whole-region admission through runtime-owned grants and generation guards.
//!
//! One internal helper validates/reserves a region without importing runtime
//! functions or touching CPU/frame fields. Its separate locals preserve a
//! direct-memory instruction's preflight pointers/path across admission.

use super::*;
use crate::static_program::memory as m;

// Helper parameters and private i32 locals; FRAME is shared with the emitter.
const REGION_ID: u32 = 1;
const COUNT: u32 = 2;
const GUARD_POINTER: u32 = 3;
const GENERATIONS_POINTER: u32 = 4;
const FIRST_PAGE: u32 = 5;
const LAST_PAGE: u32 = 6;

fn descriptor(f: &mut Function, offset: u32) {
    load(f, m::OFFSET + offset, false);
}

fn descriptor_wide(f: &mut Function, offset: u32) {
    descriptor(f, offset);
    ins(f, W::I64ExtendI32U);
}

fn memory_size(f: &mut Function) {
    ins(f, W::MemorySize(0));
    ins(f, W::I64ExtendI32U);
    wide(f, 16);
    ins(f, W::I64Shl);
}

fn require(f: &mut Function) {
    ins(f, W::I32Eqz);
    ins(f, W::If(BlockType::Empty));
    c(f, 0);
    ins(f, W::Return);
    ins(f, W::End);
}

/// Validate a complete table before loading any of its entries. Widening before
/// multiplying/adding handles malformed lengths without wasm32 wraparound.
fn table_bounds(f: &mut Function, pointer: u32, length: u32, stride: u64) {
    descriptor_wide(f, pointer);
    descriptor_wide(f, length);
    wide(f, stride);
    ins(f, W::I64Mul);
    ins(f, W::I64Add);
    memory_size(f);
    ins(f, W::I64LeU);
}

pub(super) fn preflight(f: &mut Function, checked: &Checked<'_>, r: &Region) {
    get(f, FRAME);
    c(f, r.id.0);
    c(f, r.completed_instructions() as u32);
    ins(
        f,
        W::Call(function_index(checked, REGIONS) + checked.by_id.len() as u32),
    );
    ins(f, W::I32Eqz);
    ins(f, W::If(BlockType::Empty));
    flush(f, r.entry);
    callback(f, checked, abi::REASON_FALLBACK);
    ins(f, W::End);
}

/// `(frame, region_id, count) -> success`. Every refusal precedes all stores;
/// only a successful complete proof transfers the count into reservations.
pub(super) fn helper() -> Function {
    let mut function = Function::new([(4, ValType::I32)]);
    let f = &mut function;
    get(f, FRAME);
    ins(f, W::I64ExtendI32U);
    wide(f, u64::from(m::OFFSET) + m::BYTES as u64);
    ins(f, W::I64Add);
    memory_size(f);
    ins(f, W::I64LeU);
    require(f);
    descriptor(f, m::VERSION_OFFSET);
    c(f, m::VERSION);
    ins(f, W::I32Eq);
    require(f);

    descriptor(f, m::INLINE_GRANT);
    get(f, COUNT);
    ins(f, W::I32GeU);
    require(f);
    get(f, REGION_ID);
    descriptor(f, m::REGION_GUARDS_LENGTH);
    ins(f, W::I32LtU);
    require(f);
    table_bounds(
        f,
        m::REGION_GUARDS_POINTER,
        m::REGION_GUARDS_LENGTH,
        m::REGION_GUARD_BYTES as u64,
    );
    require(f);
    table_bounds(f, m::GENERATIONS_POINTER, m::GENERATIONS_LENGTH, 8);
    require(f);

    // Bounds above prove both this multiplication and pointer addition fit.
    descriptor(f, m::REGION_GUARDS_POINTER);
    get(f, REGION_ID);
    c(f, m::REGION_GUARD_BYTES as u32);
    ins(f, W::I32Mul);
    ins(f, W::I32Add);
    set(f, GUARD_POINTER);
    get(f, GUARD_POINTER);
    ins(f, W::I32Load(arg(0, false)));
    c(f, 1);
    ins(f, W::I32Eq);
    require(f);

    for (offset, local) in [(4, FIRST_PAGE), (8, LAST_PAGE)] {
        get(f, GUARD_POINTER);
        ins(f, W::I32Load(arg(offset, false)));
        ins(f, W::LocalTee(local));
        descriptor(f, m::GENERATIONS_LENGTH);
        ins(f, W::I32LtU);
        require(f);
    }
    descriptor(f, m::GENERATIONS_POINTER);
    set(f, GENERATIONS_POINTER);
    for (local, expected_offset) in [(FIRST_PAGE, 16), (LAST_PAGE, 24)] {
        get(f, GENERATIONS_POINTER);
        get(f, local);
        c(f, 8);
        ins(f, W::I32Mul);
        ins(f, W::I32Add);
        ins(f, W::I64Load(arg(0, true)));
        get(f, GUARD_POINTER);
        ins(f, W::I64Load(arg(expected_offset, true)));
        ins(f, W::I64Eq);
        require(f);
    }

    // Even malformed reservation counters must decline before any descriptor
    // or architectural/device effect. The valid runtime grant cannot overflow.
    descriptor(f, m::INLINE_RESERVED);
    c(f, u32::MAX);
    get(f, COUNT);
    ins(f, W::I32Sub);
    ins(f, W::I32LeU);
    require(f);
    get(f, FRAME);
    descriptor(f, m::INLINE_GRANT);
    get(f, COUNT);
    ins(f, W::I32Sub);
    ins(f, W::I32Store(arg(m::OFFSET + m::INLINE_GRANT, false)));
    get(f, FRAME);
    descriptor(f, m::INLINE_RESERVED);
    get(f, COUNT);
    ins(f, W::I32Add);
    ins(f, W::I32Store(arg(m::OFFSET + m::INLINE_RESERVED, false)));
    c(f, 1);
    ins(f, W::End);
    function
}
