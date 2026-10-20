//! Direct VGA writes over a bounded descriptor; no runtime calls per pixel.
use super::*;
#[cfg(test)]
#[path = "device_write_tests.rs"]
mod tests;

/// A failed capability does not exit: earlier guest instructions must still
/// commit before the particular device store that needs this capability.
pub(super) fn initialize(f: &mut Function) {
    preflight_with(
        f,
        |f| {
            preflight_descriptor(f);
            controls(f);
            layout(f);
            descriptor(f, m::DEVICE_CHAIN4);
            set(f, DEVICE_WRITE_CHAIN4);
            descriptor(f, m::DEVICE_PLANE_MASK);
            set(f, DEVICE_WRITE_PLANE_MASK);
            simple_replacement(f);
            set(f, DEVICE_WRITE_SIMPLE);
            c(f, 1);
            set(f, DEVICE_WRITE_VALID);
        },
        |_| {},
    );
}

fn controls(f: &mut Function) {
    descriptor(f, m::DEVICE_WRITE_ENABLED);
    c(f, 1);
    ins(f, W::I32Eq);
    require(f, 0);
    descriptor(f, m::DEVICE_KIND);
    ins(f, W::LocalTee(DEVICE_WRITE_KIND));
    c(f, DeviceReadKind::Linear as u32);
    ins(f, W::I32Eq);
    get(f, DEVICE_WRITE_KIND);
    c(f, DeviceReadKind::FourLanes as u32);
    ins(f, W::I32Eq);
    ins(f, W::I32Or);
    require(f, 0);
    for (field, maximum) in [
        (m::DEVICE_CHAIN4, 1),
        (m::DEVICE_PLANE_MASK, 15),
        (m::DEVICE_WRITE_MODE, 3),
        (m::DEVICE_SET_RESET, 15),
        (m::DEVICE_ENABLE_SET_RESET, 15),
        (m::DEVICE_ROTATE, 31),
        (m::DEVICE_BIT_MASK, 255),
    ] {
        descriptor(f, field);
        c(f, maximum);
        ins(f, W::I32LeU);
        require(f, 0);
    }
}

fn layout(f: &mut Function) {
    // A foreign device mapping may not alias CPU frames, RAM or proof tables.
    table_bounds(f, m::DEVICE_POINTER, m::DEVICE_BACKING_LENGTH, 1);
    require(f, 0);
    descriptor_wide(f, m::DEVICE_POINTER);
    descriptor_wide(f, m::DEVICE_BACKING_LENGTH);
    ins(f, W::I64Add);
    local_wide(f, FRAME);
    ins(f, W::I64LeU);
    descriptor_wide(f, m::DEVICE_POINTER);
    local_wide(f, FRAME);
    wide(f, u64::from(m::OFFSET) + m::BYTES as u64);
    ins(f, W::I64Add);
    ins(f, W::I64GeU);
    ins(f, W::I32Or);
    require(f, 0);
    for (pointer, length, stride) in [
        (m::RAM_POINTER, m::RAM_LENGTH, 1),
        (m::GENERATIONS_POINTER, m::GENERATIONS_LENGTH, 8),
        (m::CODE_PAGES_POINTER, m::CODE_PAGES_LENGTH, 1),
    ] {
        descriptor_wide(f, m::DEVICE_POINTER);
        descriptor_wide(f, m::DEVICE_BACKING_LENGTH);
        ins(f, W::I64Add);
        descriptor_wide(f, pointer);
        ins(f, W::I64LeU);
        descriptor_wide(f, pointer);
        descriptor_wide(f, length);
        wide(f, stride);
        ins(f, W::I64Mul);
        ins(f, W::I64Add);
        descriptor_wide(f, m::DEVICE_POINTER);
        ins(f, W::I64LeU);
        ins(f, W::I32Or);
        require(f, 0);
    }
}

pub(super) fn preflight(f: &mut Function, bytes: u32, depth: u32) {
    get(f, DEVICE_WRITE_VALID);
    require(f, depth);
    contains(f, m::DEVICE_START, m::DEVICE_LENGTH, bytes);
    require(f, depth);
    get(f, LINEAR);
    descriptor(f, m::DEVICE_START);
    ins(f, W::I32Sub);
    set(f, OFFSET);
    // The capability proves that the entire backing lies in memory. Each
    // physical group must still fit before the first guest byte is written.
    // This also proves every subsequent wasm32 offset/addition nonwrapping.
    for byte in 0..bytes {
        get(f, DEVICE_WRITE_KIND);
        c(f, DeviceReadKind::Linear as u32);
        ins(f, W::I32Eq);
        ins(f, W::If(BlockType::Result(ValType::I64)));
        local_wide(f, OFFSET);
        wide(f, u64::from(byte));
        ins(f, W::I64Add);
        descriptor_wide(f, m::DEVICE_BASE);
        ins(f, W::I64Add);
        wide(f, 1);
        ins(f, W::I64Add);
        ins(f, W::Else);
        get(f, DEVICE_WRITE_CHAIN4);
        ins(f, W::If(BlockType::Result(ValType::I64)));
        local_wide(f, OFFSET);
        wide(f, u64::from(byte));
        ins(f, W::I64Add);
        wide(f, !3u64);
        ins(f, W::I64And);
        ins(f, W::Else);
        local_wide(f, OFFSET);
        wide(f, u64::from(byte));
        ins(f, W::I64Add);
        wide(f, 2);
        ins(f, W::I64Shl);
        ins(f, W::End);
        wide(f, 4);
        ins(f, W::I64Add);
        ins(f, W::End);
        descriptor_wide(f, m::DEVICE_BACKING_LENGTH);
        ins(f, W::I64LeU);
        require(f, depth);
    }
    c(f, 3);
    set(f, PATH);
}

fn increment(f: &mut Function, field: u32, amount: u64) {
    get(f, FRAME);
    load(f, m::OFFSET + field, true);
    wide(f, amount);
    ins(f, W::I64Add);
    ins(f, W::I64Store(arg(m::OFFSET + field, true)));
}

fn commit(f: &mut Function, plane: u32) {
    get(f, POINTER);
    ins(
        f,
        W::I32Load8U(MemArg {
            align: 0,
            ..arg(plane, false)
        }),
    );
    get(f, PIXEL);
    ins(f, W::I32Ne);
    ins(f, W::If(BlockType::Empty));
    get(f, POINTER);
    get(f, PIXEL);
    ins(
        f,
        W::I32Store8(MemArg {
            align: 0,
            ..arg(plane, false)
        }),
    );
    increment(f, m::DEVICE_CHANGES, 1);
    ins(f, W::End);
}

fn plane_bit(f: &mut Function, field: u32, plane: u32) {
    descriptor(f, field);
    c(f, 1 << plane);
    ins(f, W::I32And);
    ins(f, W::I32Eqz);
    ins(f, W::I32Eqz);
    c(f, 255);
    ins(f, W::I32Mul);
}

fn simple_replacement(f: &mut Function) {
    descriptor(f, m::DEVICE_WRITE_MODE);
    ins(f, W::I32Eqz);
    descriptor(f, m::DEVICE_ENABLE_SET_RESET);
    ins(f, W::I32Eqz);
    ins(f, W::I32And);
    descriptor(f, m::DEVICE_ROTATE);
    ins(f, W::I32Eqz);
    ins(f, W::I32And);
    descriptor(f, m::DEVICE_BIT_MASK);
    c(f, 255);
    ins(f, W::I32Eq);
    ins(f, W::I32And);
}

fn pipeline(f: &mut Function, plane: u32) {
    get(f, DEVICE_WRITE_SIMPLE);
    ins(f, W::If(BlockType::Empty));
    get(f, BYTE);
    set(f, PIXEL);
    ins(f, W::Else);
    descriptor(f, m::DEVICE_LATCHES);
    c(f, plane * 8);
    ins(f, W::I32ShrU);
    c(f, 255);
    ins(f, W::I32And);
    set(f, LATCH);
    descriptor(f, m::DEVICE_WRITE_MODE);
    c(f, 1);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Result(ValType::I32)));
    get(f, LATCH);
    ins(f, W::Else);
    // Rotate an eight-bit value, rather than using a 32-bit rotate.
    get(f, BYTE);
    descriptor(f, m::DEVICE_ROTATE);
    c(f, 7);
    ins(f, W::I32And);
    ins(f, W::I32ShrU);
    get(f, BYTE);
    c(f, 8);
    descriptor(f, m::DEVICE_ROTATE);
    c(f, 7);
    ins(f, W::I32And);
    ins(f, W::I32Sub);
    ins(f, W::I32Shl);
    ins(f, W::I32Or);
    c(f, 255);
    ins(f, W::I32And);
    set(f, PIXEL);
    descriptor(f, m::DEVICE_BIT_MASK);
    set(f, STORE_MASK);
    descriptor(f, m::DEVICE_WRITE_MODE);
    c(f, 3);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Result(ValType::I32)));
    get(f, STORE_MASK);
    get(f, PIXEL);
    ins(f, W::I32And);
    set(f, STORE_MASK);
    plane_bit(f, m::DEVICE_SET_RESET, plane);
    ins(f, W::Else);
    descriptor(f, m::DEVICE_WRITE_MODE);
    c(f, 2);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Result(ValType::I32)));
    get(f, BYTE);
    c(f, 1 << plane);
    ins(f, W::I32And);
    ins(f, W::I32Eqz);
    ins(f, W::I32Eqz);
    c(f, 255);
    ins(f, W::I32Mul);
    ins(f, W::Else);
    descriptor(f, m::DEVICE_ENABLE_SET_RESET);
    c(f, 1 << plane);
    ins(f, W::I32And);
    ins(f, W::If(BlockType::Result(ValType::I32)));
    plane_bit(f, m::DEVICE_SET_RESET, plane);
    ins(f, W::Else);
    get(f, PIXEL);
    ins(f, W::End);
    ins(f, W::End);
    ins(f, W::End);
    set(f, PIXEL);
    for (function, operation) in [(1, W::I32And), (2, W::I32Or), (3, W::I32Xor)] {
        descriptor(f, m::DEVICE_ROTATE);
        c(f, 3);
        ins(f, W::I32ShrU);
        c(f, 3);
        ins(f, W::I32And);
        c(f, function);
        ins(f, W::I32Eq);
        ins(f, W::If(BlockType::Empty));
        get(f, PIXEL);
        get(f, LATCH);
        ins(f, operation);
        set(f, PIXEL);
        ins(f, W::End);
    }
    get(f, PIXEL);
    get(f, STORE_MASK);
    ins(f, W::I32And);
    get(f, LATCH);
    get(f, STORE_MASK);
    c(f, 255);
    ins(f, W::I32Xor);
    ins(f, W::I32And);
    ins(f, W::I32Or);
    ins(f, W::End);
    set(f, PIXEL);
    ins(f, W::End);
}

pub(super) fn store(f: &mut Function, width: Width) {
    let bytes = width.bits() / 8;
    for byte in 0..bytes {
        get(f, STORE_SOURCE);
        c(f, byte * 8);
        ins(f, W::I32ShrU);
        c(f, 255);
        ins(f, W::I32And);
        set(f, BYTE);
        byte_offset(f, byte);
        descriptor(f, m::DEVICE_POINTER);
        get(f, LEFT);
        ins(f, W::I32Add);
        set(f, POINTER);
        get(f, DEVICE_WRITE_KIND);
        c(f, DeviceReadKind::Linear as u32);
        ins(f, W::I32Eq);
        ins(f, W::If(BlockType::Empty));
        get(f, BYTE);
        set(f, PIXEL);
        commit(f, 0);
        ins(f, W::Else);
        for plane in 0..4 {
            get(f, DEVICE_WRITE_PLANE_MASK);
            c(f, 1 << plane);
            ins(f, W::I32And);
            get(f, DEVICE_WRITE_CHAIN4);
            ins(f, W::I32Eqz);
            get(f, OFFSET);
            c(f, byte);
            ins(f, W::I32Add);
            c(f, 3);
            ins(f, W::I32And);
            c(f, plane);
            ins(f, W::I32Eq);
            ins(f, W::I32Or);
            // Convert the plane mask to a boolean before combining it.
            set(f, PIXEL);
            ins(f, W::I32Eqz);
            ins(f, W::I32Eqz);
            get(f, PIXEL);
            ins(f, W::I32And);
            ins(f, W::If(BlockType::Empty));
            pipeline(f, plane);
            commit(f, plane);
            ins(f, W::End);
        }
        ins(f, W::End);
    }
    increment(f, m::DEVICE_WRITES, u64::from(bytes));
}

fn byte_offset(f: &mut Function, byte: u32) {
    get(f, DEVICE_WRITE_KIND);
    c(f, DeviceReadKind::Linear as u32);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Result(ValType::I32)));
    get(f, OFFSET);
    c(f, byte);
    ins(f, W::I32Add);
    descriptor(f, m::DEVICE_BASE);
    ins(f, W::I32Add);
    ins(f, W::Else);
    get(f, DEVICE_WRITE_CHAIN4);
    ins(f, W::If(BlockType::Result(ValType::I32)));
    get(f, OFFSET);
    c(f, byte);
    ins(f, W::I32Add);
    c(f, !3);
    ins(f, W::I32And);
    ins(f, W::Else);
    get(f, OFFSET);
    c(f, byte);
    ins(f, W::I32Add);
    c(f, 2);
    ins(f, W::I32Shl);
    ins(f, W::End);
    ins(f, W::End);
    set(f, LEFT);
}
