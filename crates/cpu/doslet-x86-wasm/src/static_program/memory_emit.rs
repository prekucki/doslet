//! Inline accesses over the explicit descriptor, never Rust object layouts.
//!
//! Preflight is deliberately separate from execution: every guest/segment and
//! backing span must pass before admission can reserve work or a VGA read can
//! update its latch mirror. Only small integer scratch values cross admission.

use super::*;
use crate::static_program::memory::{self as m, DeviceReadKind, MemoryAddress};
#[path = "copy_kernel_emit.rs"]
mod copy_kernel;
#[path = "memory_emit/device_write.rs"]
mod device_write;
pub(in crate::static_program) use copy_kernel::body as copy_kernel_body;

pub(super) const EA: u32 = WIDE + 1;
const LINEAR: u32 = EA + 1;
const OFFSET: u32 = LINEAR + 1;
const PATH: u32 = OFFSET + 1;
const POINTER: u32 = PATH + 1;
const LATCH: u32 = POINTER + 1;
const BYTE: u32 = LATCH + 1;
const FIRST_PAGE: u32 = BYTE + 1;
const LAST_PAGE: u32 = FIRST_PAGE + 1;
const STORE_SOURCE: u32 = LAST_PAGE + 1;
const PIXEL: u32 = STORE_SOURCE + 1;
const STORE_MASK: u32 = PIXEL + 1;
const DEVICE_WRITE_VALID: u32 = STORE_MASK + 1;
const DEVICE_WRITE_KIND: u32 = DEVICE_WRITE_VALID + 1;
const DEVICE_WRITE_CHAIN4: u32 = DEVICE_WRITE_KIND + 1;
const DEVICE_WRITE_PLANE_MASK: u32 = DEVICE_WRITE_CHAIN4 + 1;
const DEVICE_WRITE_SIMPLE: u32 = DEVICE_WRITE_PLANE_MASK + 1;
pub(super) const LOCAL_COUNT: u32 = LAST_PAGE - WIDE;
pub(super) const PROCEDURE_LOCAL_COUNT: u32 = DEVICE_WRITE_SIMPLE - WIDE;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Access {
    Read,
    RamRead,
    Write,
    /// Direct procedure-only device stores, with an explicit write capability.
    ProcedureWrite,
    WriteHelper,
    ReadModifyWrite,
}

fn descriptor(f: &mut Function, offset: u32) {
    load(f, m::OFFSET + offset, false);
}

fn descriptor_wide(f: &mut Function, offset: u32) {
    descriptor(f, offset);
    ins(f, W::I64ExtendI32U);
}

fn local_wide(f: &mut Function, local: u32) {
    get(f, local);
    ins(f, W::I64ExtendI32U);
}

fn memory_size(f: &mut Function) {
    ins(f, W::MemorySize(0));
    ins(f, W::I64ExtendI32U);
    wide(f, 16);
    ins(f, W::I64Shl);
}

/// Branch out of the preflight failure block on refusal. `depth` counts only
/// the surrounding preflight conditionals; no state is committed by a check.
fn require(f: &mut Function, depth: u32) {
    ins(f, W::I32Eqz);
    ins_hinted(f, false, W::BrIf(depth));
}

fn contains(f: &mut Function, base: u32, length: u32, bytes: u32) {
    get(f, LINEAR);
    descriptor(f, base);
    ins(f, W::I32GeU);
    get(f, LINEAR);
    descriptor(f, base);
    ins(f, W::I32Sub);
    ins(f, W::I64ExtendI32U);
    wide(f, u64::from(bytes));
    ins(f, W::I64Add);
    descriptor_wide(f, length);
    ins(f, W::I64LeU);
    ins(f, W::I32And);
}

/// True if the complete access misses the half-open descriptor interval. Wide
/// comparisons also handle intervals whose mathematical end exceeds u32::MAX.
fn excludes(f: &mut Function, start: u32, length: u32, bytes: u32) {
    descriptor(f, length);
    ins(f, W::I32Eqz);
    local_wide(f, LINEAR);
    descriptor_wide(f, start);
    descriptor_wide(f, length);
    ins(f, W::I64Add);
    ins(f, W::I64GeU);
    ins(f, W::I32Or);
    local_wide(f, LINEAR);
    wide(f, u64::from(bytes));
    ins(f, W::I64Add);
    descriptor_wide(f, start);
    ins(f, W::I64LeU);
    ins(f, W::I32Or);
}

fn backing(f: &mut Function, pointer: u32, length: u32, bytes: u32) {
    local_wide(f, LEFT);
    wide(f, u64::from(bytes));
    ins(f, W::I64Add);
    descriptor_wide(f, length);
    ins(f, W::I64LeU);
    descriptor_wide(f, pointer);
    local_wide(f, LEFT);
    ins(f, W::I64Add);
    wide(f, u64::from(bytes));
    ins(f, W::I64Add);
    memory_size(f);
    ins(f, W::I64LeU);
    ins(f, W::I32And);
}

/// Set LEFT to this guest byte's backing offset. The caller preflights the
/// multiplication/addition in u64 before any guest load is emitted.
fn device_byte_offset(f: &mut Function, byte: u32) {
    descriptor(f, m::DEVICE_KIND);
    c(f, DeviceReadKind::Linear as u32);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Result(ValType::I32)));
    get(f, OFFSET);
    c(f, byte);
    ins(f, W::I32Add);
    descriptor(f, m::DEVICE_BASE);
    ins(f, W::I32Add);
    ins(f, W::Else);
    descriptor(f, m::DEVICE_CHAIN4);
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

pub(super) fn preflight_access(
    f: &mut Function,
    checked: &Checked<'_>,
    r: &Region,
    width: Width,
    address: MemoryAddress,
    access: Access,
) {
    preflight(f, checked, r, |f| {
        preflight_checks(f, width, address, access);
    });
}

/// Stack offsets are calculated from the live SS.B capability after the common
/// descriptor check, then use the same complete segment/RAM/store proofs.
pub(super) fn preflight_ram_access(
    f: &mut Function,
    checked: &Checked<'_>,
    r: &Region,
    width: Width,
    write: bool,
    offset: impl FnOnce(&mut Function),
) {
    preflight(f, checked, r, |f| {
        preflight_descriptor(f);
        offset(f);
        set(f, EA);
        preflight_operand(
            f,
            width,
            m::Segment::Ss,
            if write {
                Access::Write
            } else {
                Access::RamRead
            },
        );
    });
}

fn preflight(
    f: &mut Function,
    checked: &Checked<'_>,
    r: &Region,
    checks: impl FnOnce(&mut Function),
) {
    preflight_with(f, checks, |f| {
        flush(f, r.entry);
        callback(f, checked, abi::REASON_FALLBACK);
    });
}

pub(super) fn preflight_with(
    f: &mut Function,
    checks: impl FnOnce(&mut Function),
    refused: impl FnOnce(&mut Function),
) {
    // All checks share one refusal path. Repeating the full CPU-frame flush
    // for every predicate otherwise dominates large static modules' size.
    ins(f, W::Block(BlockType::Empty)); // success
    ins(f, W::Block(BlockType::Empty)); // failure
    checks(f);
    ins(f, W::Br(1));
    ins(f, W::End);
    refused(f);
    ins(f, W::End);
}

pub(super) fn preflight_descriptor(f: &mut Function) {
    // Missing/truncated descriptors decline cleanly rather than trapping.
    local_wide(f, FRAME);
    wide(f, u64::from(m::OFFSET) + m::BYTES as u64);
    ins(f, W::I64Add);
    memory_size(f);
    ins(f, W::I64LeU);
    require(f, 0);
    descriptor(f, m::VERSION_OFFSET);
    c(f, m::VERSION);
    ins(f, W::I32Eq);
    require(f, 0);
}

pub(super) fn preflight_checks(
    f: &mut Function,
    width: Width,
    address: MemoryAddress,
    access: Access,
) {
    preflight_descriptor(f);
    effective_address(f, address);
    set(f, EA);
    preflight_operand(f, width, address.segment, access);
}

pub(super) fn preflight_operand(
    f: &mut Function,
    width: Width,
    segment: m::Segment,
    access: Access,
) {
    let bytes = width.bits() / 8;
    let write = matches!(
        access,
        Access::Write | Access::ProcedureWrite | Access::WriteHelper | Access::ReadModifyWrite
    );
    let window = m::SEGMENTS + (u32::from(write) * 6 + segment as u32) * m::SEGMENT_STRIDE;
    descriptor(f, window);
    c(f, 1);
    ins(f, W::I32Eq);
    get(f, EA);
    descriptor(f, window + 8);
    ins(f, W::I32GeU);
    ins(f, W::I32And);
    local_wide(f, EA);
    wide(f, u64::from(bytes - 1));
    ins(f, W::I64Add);
    descriptor_wide(f, window + 12);
    ins(f, W::I64LeU);
    ins(f, W::I32And);
    require(f, 0);
    if access == Access::ReadModifyWrite {
        // The write proof below excludes every device and validates all RAM
        // backing and generation spans. Add the independent read segment
        // proof without emitting the device-read preflight or touching latches.
        let read_window = m::SEGMENTS + segment as u32 * m::SEGMENT_STRIDE;
        descriptor(f, read_window);
        c(f, 1);
        ins(f, W::I32Eq);
        get(f, EA);
        descriptor(f, read_window + 8);
        ins(f, W::I32GeU);
        ins(f, W::I32And);
        local_wide(f, EA);
        wide(f, u64::from(bytes - 1));
        ins(f, W::I64Add);
        descriptor_wide(f, read_window + 12);
        ins(f, W::I64LeU);
        ins(f, W::I32And);
        descriptor(f, read_window + 4);
        descriptor(f, window + 4);
        ins(f, W::I32Eq);
        ins(f, W::I32And);
        require(f, 0);
    }
    get(f, EA);
    descriptor(f, window + 4);
    ins(f, W::I32Add); // x86 segment-base addition wraps to 32 bits.
    set(f, LINEAR);
    local_wide(f, LINEAR);
    wide(f, u64::from(bytes - 1));
    ins(f, W::I64Add);
    wide(f, u64::from(u32::MAX));
    ins(f, W::I64LeU);
    require(f, 0);

    if access == Access::WriteHelper {
        // Descriptor, segment and nonwrapping linear-span failures above must
        // leave before admission. A failed direct-RAM proof below can instead
        // select the ordinary bus helper, which validates its own whole span.
        c(f, 1);
        set(f, PATH);
        ins(f, W::Block(BlockType::Empty));
        contains(f, m::RAM_BASE, m::RAM_LENGTH, bytes);
        excludes(f, m::RAM_EXCLUDED_START, m::RAM_EXCLUDED_LENGTH, bytes);
        ins(f, W::I32And);
        excludes(f, m::DEVICE_START, m::DEVICE_LENGTH, bytes);
        ins(f, W::I32And);
        require(f, 0);
        ram_pointer(f, bytes, 0);
        preflight_store(f, bytes, 0);
        c(f, 0);
        set(f, PATH);
        ins(f, W::End);
        return;
    }

    contains(f, m::RAM_BASE, m::RAM_LENGTH, bytes);
    excludes(f, m::RAM_EXCLUDED_START, m::RAM_EXCLUDED_LENGTH, bytes);
    ins(f, W::I32And);
    excludes(f, m::DEVICE_START, m::DEVICE_LENGTH, bytes);
    ins(f, W::I32And);
    if access == Access::ProcedureWrite {
        ins(f, W::If(BlockType::Empty));
        ram_pointer(f, bytes, 1);
        preflight_store(f, bytes, 1);
        procedure_store_layout_at(f, 1);
        c(f, 0);
        set(f, PATH);
        ins(f, W::Else);
        device_write::preflight(f, bytes, 1);
        ins(f, W::End);
        return;
    }
    if write || access == Access::RamRead {
        require(f, 0);
        ram_pointer(f, bytes, 0);
        if write {
            preflight_store(f, bytes, 0);
        }
        c(f, 0);
        set(f, PATH);
        return;
    }
    ins(f, W::If(BlockType::Empty));
    ram_pointer(f, bytes, 1);
    c(f, 0);
    set(f, PATH);
    ins(f, W::Else);
    contains(f, m::DEVICE_START, m::DEVICE_LENGTH, bytes);
    require(f, 1);
    descriptor(f, m::DEVICE_KIND);
    ins(f, W::LocalTee(PATH));
    c(f, DeviceReadKind::Linear as u32);
    ins(f, W::I32Eq);
    get(f, PATH);
    c(f, DeviceReadKind::FourLanes as u32);
    ins(f, W::I32Eq);
    ins(f, W::I32Or);
    require(f, 1);
    get(f, LINEAR);
    descriptor(f, m::DEVICE_START);
    ins(f, W::I32Sub);
    set(f, OFFSET);
    get(f, PATH);
    c(f, DeviceReadKind::FourLanes as u32);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Empty));
    descriptor(f, m::DEVICE_READ_PLANE);
    c(f, 3);
    ins(f, W::I32LeU);
    require(f, 2);
    ins(f, W::End);
    // Check every four-lane group even for wide unaligned/chain-4 reads.
    // Checking only the first/last selected plane would miss latch accesses.
    for byte in 0..bytes {
        get(f, PATH);
        c(f, DeviceReadKind::Linear as u32);
        ins(f, W::I32Eq);
        ins(f, W::If(BlockType::Result(ValType::I64)));
        local_wide(f, OFFSET);
        wide(f, u64::from(byte));
        ins(f, W::I64Add);
        descriptor_wide(f, m::DEVICE_BASE);
        ins(f, W::I64Add);
        ins(f, W::Else);
        descriptor(f, m::DEVICE_CHAIN4);
        ins(f, W::I32Eqz);
        ins(f, W::If(BlockType::Result(ValType::I64)));
        local_wide(f, OFFSET);
        wide(f, u64::from(byte));
        ins(f, W::I64Add);
        wide(f, 2);
        ins(f, W::I64Shl);
        ins(f, W::Else);
        local_wide(f, OFFSET);
        wide(f, u64::from(byte));
        ins(f, W::I64Add);
        wide(f, !3u64);
        ins(f, W::I64And);
        ins(f, W::End);
        ins(f, W::End);
        ins(f, W::LocalTee(WIDE));
        wide(f, u64::from(u32::MAX));
        ins(f, W::I64LeU);
        require(f, 1);
        get(f, WIDE);
        ins(f, W::I32WrapI64);
        set(f, LEFT);
        get(f, PATH);
        c(f, DeviceReadKind::Linear as u32);
        ins(f, W::I32Eq);
        ins(f, W::If(BlockType::Result(ValType::I32)));
        backing(f, m::DEVICE_POINTER, m::DEVICE_BACKING_LENGTH, 1);
        ins(f, W::Else);
        backing(f, m::DEVICE_POINTER, m::DEVICE_BACKING_LENGTH, 4);
        ins(f, W::End);
        require(f, 1);
    }
    ins(f, W::End);
}

fn ram_pointer(f: &mut Function, bytes: u32, depth: u32) {
    get(f, LINEAR);
    descriptor(f, m::RAM_BASE);
    ins(f, W::I32Sub);
    set(f, LEFT);
    backing(f, m::RAM_POINTER, m::RAM_LENGTH, bytes);
    require(f, depth);
    descriptor(f, m::RAM_POINTER);
    get(f, LEFT);
    ins(f, W::I32Add);
    set(f, POINTER);
}

/// Check the full tables in u64 before calculating any wasm32 entry pointer.
fn table_bounds(f: &mut Function, pointer: u32, length: u32, stride: u64) {
    descriptor_wide(f, pointer);
    descriptor_wide(f, length);
    wide(f, stride);
    ins(f, W::I64Mul);
    ins(f, W::I64Add);
    memory_size(f);
    ins(f, W::I64LeU);
}

fn generation_pointer(f: &mut Function, page: u32) {
    descriptor(f, m::GENERATIONS_POINTER);
    get(f, page);
    c(f, 8);
    ins(f, W::I32Mul);
    ins(f, W::I32Add);
}

fn preflight_page(f: &mut Function, page: u32, depth: u32) {
    for length in [m::GENERATIONS_LENGTH, m::CODE_PAGES_LENGTH] {
        get(f, page);
        descriptor(f, length);
        ins(f, W::I32LtU);
        require(f, depth);
    }
    descriptor(f, m::CODE_PAGES_POINTER);
    get(f, page);
    ins(f, W::I32Add);
    ins(
        f,
        W::I32Load8U(MemArg {
            align: 0,
            ..arg(0, false)
        }),
    );
    ins(f, W::I32Eqz);
    require(f, depth);
    generation_pointer(f, page);
    ins(f, W::I64Load(arg(0, true)));
    wide(f, u64::MAX);
    ins(f, W::I64Ne);
    require(f, depth);
}

fn distinct_last_page(f: &mut Function) {
    get(f, FIRST_PAGE);
    get(f, LAST_PAGE);
    ins(f, W::I32Ne);
}

fn preflight_store(f: &mut Function, bytes: u32, depth: u32) {
    // Runtime descriptors promise identity-mapped, A20-enabled ordinary RAM.
    // Validate the complete advertised RAM span as well as this operand.
    descriptor(f, m::RAM_BASE);
    c(f, 0x500);
    ins(f, W::I32GeU);
    require(f, depth);
    descriptor_wide(f, m::RAM_BASE);
    descriptor_wide(f, m::RAM_LENGTH);
    ins(f, W::I64Add);
    wide(f, 1u64 << 32);
    ins(f, W::I64LeU);
    require(f, depth);
    table_bounds(f, m::RAM_POINTER, m::RAM_LENGTH, 1);
    require(f, depth);
    table_bounds(f, m::GENERATIONS_POINTER, m::GENERATIONS_LENGTH, 8);
    require(f, depth);
    table_bounds(f, m::CODE_PAGES_POINTER, m::CODE_PAGES_LENGTH, 1);
    require(f, depth);

    get(f, LINEAR);
    c(f, 12);
    ins(f, W::I32ShrU);
    set(f, FIRST_PAGE);
    get(f, LINEAR);
    c(f, bytes - 1);
    ins(f, W::I32Add);
    c(f, 12);
    ins(f, W::I32ShrU);
    set(f, LAST_PAGE);
    preflight_page(f, FIRST_PAGE, depth);
    distinct_last_page(f);
    ins_hinted(f, false, W::If(BlockType::Empty));
    preflight_page(f, LAST_PAGE, depth + 1);
    ins(f, W::End);
}

fn device_byte(f: &mut Function, byte: u32) {
    device_byte_offset(f, byte);
    descriptor(f, m::DEVICE_POINTER);
    get(f, LEFT);
    ins(f, W::I32Add);
    set(f, POINTER);
    get(f, PATH);
    c(f, DeviceReadKind::Linear as u32);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Result(ValType::I32)));
    get(f, POINTER);
    ins(
        f,
        W::I32Load8U(MemArg {
            align: 0,
            ..arg(0, false)
        }),
    );
    ins(f, W::Else);
    // One unaligned little-endian wasm load reads exactly the four actual
    // backing bytes and packs the last-read VGA latch mirror in lane order.
    get(f, POINTER);
    ins(f, W::I32Load(arg(0, false)));
    set(f, LATCH);
    store(f, m::OFFSET + m::DEVICE_LATCHES, LATCH, false);
    descriptor(f, m::DEVICE_COMPARE_ENABLED);
    ins(f, W::If(BlockType::Result(ValType::I32)));
    c(f, 255);
    set(f, BYTE);
    for plane in 0..4 {
        descriptor(f, m::DEVICE_CARE);
        c(f, 1 << plane);
        ins(f, W::I32And);
        ins(f, W::If(BlockType::Empty));
        get(f, BYTE);
        get(f, LATCH);
        c(f, plane * 8);
        ins(f, W::I32ShrU);
        descriptor(f, m::DEVICE_COMPARE);
        c(f, 1 << plane);
        ins(f, W::I32And);
        ins(f, W::I32Eqz);
        c(f, 255);
        ins(f, W::I32Mul);
        ins(f, W::I32Xor);
        ins(f, W::I32And);
        set(f, BYTE);
        ins(f, W::End);
    }
    get(f, BYTE);
    ins(f, W::Else);
    get(f, LATCH);
    descriptor(f, m::DEVICE_CHAIN4);
    ins(f, W::If(BlockType::Result(ValType::I32)));
    get(f, OFFSET);
    c(f, byte);
    ins(f, W::I32Add);
    c(f, 3);
    ins(f, W::I32And);
    ins(f, W::Else);
    descriptor(f, m::DEVICE_READ_PLANE);
    ins(f, W::End);
    c(f, 8);
    ins(f, W::I32Mul);
    ins(f, W::I32ShrU);
    c(f, 255);
    ins(f, W::I32And);
    ins(f, W::End);
    ins(f, W::End);
}

pub(super) fn load_memory(f: &mut Function, target: Register) {
    load_value(f, target.width());
    write_register(f, target, RESULT);
}

fn ram_value(f: &mut Function, width: Width) {
    get(f, POINTER);
    ins(
        f,
        match width {
            Width::Byte => W::I32Load8U(MemArg {
                align: 0,
                ..arg(0, false)
            }),
            Width::Word => W::I32Load16U(MemArg {
                align: 1,
                ..arg(0, false)
            }),
            Width::Dword => W::I32Load(arg(0, false)),
        },
    );
}

/// The write preflight proves ordinary RAM; no device path is emitted.
pub(super) fn load_ram_value(f: &mut Function, width: Width) {
    ram_value(f, width);
    set(f, RESULT);
}

/// Read the preflighted operand into RESULT, preserving GPRs, FLAGS, RIGHT
/// and CARRY. Device reads may clobber LEFT and the memory scratch locals.
pub(super) fn load_value(f: &mut Function, width: Width) {
    get(f, PATH);
    ins(f, W::I32Eqz);
    ins(f, W::If(BlockType::Result(ValType::I32)));
    ram_value(f, width);
    ins(f, W::Else);
    c(f, 0);
    set(f, RESULT);
    let bytes = width.bits() / 8;
    for byte in 0..bytes {
        get(f, RESULT);
        device_byte(f, byte);
        c(f, byte * 8);
        ins(f, W::I32Shl);
        ins(f, W::I32Or);
        set(f, RESULT);
    }
    get(f, FRAME);
    load(f, m::OFFSET + m::DEVICE_READS, true);
    wide(f, u64::from(bytes));
    ins(f, W::I64Add);
    ins(f, W::I64Store(arg(m::OFFSET + m::DEVICE_READS, true)));
    get(f, RESULT);
    ins(f, W::End);
    set(f, RESULT);
}

fn increment_generation(f: &mut Function, page: u32) {
    generation_pointer(f, page);
    generation_pointer(f, page);
    ins(f, W::I64Load(arg(0, true)));
    wide(f, 1);
    ins(f, W::I64Add);
    ins(f, W::I64Store(arg(0, true)));
}

pub(super) fn store_memory(f: &mut Function, width: Width, value: Source) {
    store_value(f, width, |f| source(f, value));
}

pub(super) fn store_with_helper(
    f: &mut Function,
    checked: &Checked<'_>,
    r: &Region,
    width: Width,
    value: Source,
) {
    get(f, PATH);
    ins(f, W::If(BlockType::Empty));
    get(f, FRAME);
    get(f, LINEAR);
    c(f, width.bits() / 8);
    // Capture the source from live register parameters before entering the
    // helper; the frame still contains the last dispatch boundary's registers.
    source(f, value);
    ins(f, W::Call(MEMORY_WRITE));
    ins(f, W::LocalTee(LEFT));
    ins(f, W::I32Eqz);
    ins(f, W::If(BlockType::Empty));
    // A declined helper has no effects and leaves the descriptor unchanged.
    // Undo only this instruction's reservation; the preceding prefix remains
    // pending for the ordinary fallback callback to commit.
    for (offset, op) in [
        (m::INLINE_RESERVED, W::I32Sub),
        (m::INLINE_GRANT, W::I32Add),
    ] {
        get(f, FRAME);
        descriptor(f, offset);
        c(f, 1);
        ins(f, op);
        ins(f, W::I32Store(arg(m::OFFSET + offset, false)));
    }
    flush(f, r.entry);
    callback(f, checked, abi::REASON_FALLBACK);
    ins(f, W::End);
    get(f, LEFT);
    c(f, 1);
    ins(f, W::I32Ne);
    ins(f, W::If(BlockType::Empty));
    ins(f, W::Unreachable);
    ins(f, W::End);
    ins(f, W::Else);
    store_memory(f, width, value);
    ins(f, W::End);
}

pub(super) fn store_result(f: &mut Function, width: Width) {
    store_value(f, width, |f| get(f, RESULT));
}

fn store_value(f: &mut Function, width: Width, value: impl FnOnce(&mut Function)) {
    get(f, POINTER);
    value(f);
    ins(
        f,
        match width {
            Width::Byte => W::I32Store8(MemArg {
                align: 0,
                ..arg(0, false)
            }),
            Width::Word => W::I32Store16(MemArg {
                align: 1,
                ..arg(0, false)
            }),
            Width::Dword => W::I32Store(arg(0, false)),
        },
    );
    // The whole operand and both pages passed before admission. Invalidate
    // each physical page once before the next region checks its source proof.
    increment_generation(f, FIRST_PAGE);
    distinct_last_page(f);
    ins_hinted(f, false, W::If(BlockType::Empty));
    increment_generation(f, LAST_PAGE);
    ins(f, W::End);
    get(f, FRAME);
    load(f, m::OFFSET + m::RAM_WRITES, true);
    wide(f, 1);
    ins(f, W::I64Add);
    distinct_last_page(f);
    ins(f, W::I64ExtendI32U);
    ins(f, W::I64Add);
    ins(f, W::I64Store(arg(m::OFFSET + m::RAM_WRITES, true)));
}

pub(super) fn procedure_store_memory(f: &mut Function, width: Width, value: Source) {
    source(f, value);
    set(f, STORE_SOURCE);
    get(f, PATH);
    c(f, 3);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Empty));
    device_write::store(f, width);
    ins(f, W::Else);
    store_value(f, width, |f| get(f, STORE_SOURCE));
    ins(f, W::End);
}

/// Cache immutable direct-device capabilities without refusing the procedure.
/// The actual operand must still pass its own checks at its precise boundary.
pub(super) fn procedure_initialize(f: &mut Function) {
    device_write::initialize(f);
}

/// Load each virtual register once in the procedure prologue from ordinary RAM
/// after a single descriptor/bounds preflight. Inside the body, each slot is a
/// WASM `i32` local (`local.get`) with zero per-iteration memory overhead.
pub(super) fn procedure_virtual_registers(
    f: &mut Function,
    vregs: &[u32],
    local_base: u32,
    refused: impl FnOnce(&mut Function),
) {
    if vregs.is_empty() {
        return;
    }
    preflight_with(
        f,
        |f| {
            preflight_descriptor(f);
            descriptor(f, m::RAM_BASE);
            c(f, 0x500);
            ins(f, W::I32GeU);
            require(f, 0);
            descriptor_wide(f, m::RAM_BASE);
            descriptor_wide(f, m::RAM_LENGTH);
            ins(f, W::I64Add);
            wide(f, 1u64 << 32);
            ins(f, W::I64LeU);
            require(f, 0);
            table_bounds(f, m::RAM_POINTER, m::RAM_LENGTH, 1);
            require(f, 0);
            for &addr in vregs {
                c(f, addr);
                set(f, LINEAR);
                contains(f, m::RAM_BASE, m::RAM_LENGTH, 4);
                excludes(f, m::RAM_EXCLUDED_START, m::RAM_EXCLUDED_LENGTH, 4);
                ins(f, W::I32And);
                excludes(f, m::DEVICE_START, m::DEVICE_LENGTH, 4);
                ins(f, W::I32And);
                require(f, 0);
            }
        },
        refused,
    );
    for (i, &addr) in vregs.iter().enumerate() {
        descriptor(f, m::RAM_POINTER);
        c(f, addr);
        descriptor(f, m::RAM_BASE);
        ins(f, W::I32Sub);
        ins(f, W::I32Add);
        ins(f, W::I32Load(arg(0, false)));
        set(f, local_base + i as u32);
    }
}

/// The enclosing compiler supplies a precise prefix refusal; ordinary region
/// callback behavior and bytes remain unchanged.
pub(super) fn procedure_access(
    f: &mut Function,
    width: Width,
    address: MemoryAddress,
    access: Access,
    refused: impl FnOnce(&mut Function),
) {
    preflight_with(
        f,
        |f| {
            preflight_checks(f, width, address, access);
            if matches!(access, Access::Write | Access::ReadModifyWrite) {
                procedure_store_layout(f);
            }
        },
        refused,
    );
    if access != Access::ProcedureWrite {
        c(f, 0);
        set(f, PATH);
    }
}

/// The extended portable FPU frame must be disjoint from every advertised
/// writable/readable backing span before any procedure instruction executes.
/// This is additional to the unchanged per-operand scalar proofs.
pub(super) fn x87_frame_layout(f: &mut Function) {
    use crate::static_program::procedure_memory::X87_FRAME_BYTES;
    preflight_descriptor(f);
    local_wide(f, FRAME);
    wide(f, u64::from(X87_FRAME_BYTES));
    ins(f, W::I64Add);
    memory_size(f);
    ins(f, W::I64LeU);
    require(f, 0);
    for (pointer, length, stride) in [
        (m::RAM_POINTER, m::RAM_LENGTH, 1),
        (m::GENERATIONS_POINTER, m::GENERATIONS_LENGTH, 8),
        (m::CODE_PAGES_POINTER, m::CODE_PAGES_LENGTH, 1),
        (m::DEVICE_POINTER, m::DEVICE_BACKING_LENGTH, 1),
    ] {
        descriptor_wide(f, pointer);
        descriptor_wide(f, length);
        wide(f, stride);
        ins(f, W::I64Mul);
        ins(f, W::I64Add);
        local_wide(f, FRAME);
        ins(f, W::I64LeU);
        descriptor_wide(f, pointer);
        local_wide(f, FRAME);
        wide(f, u64::from(X87_FRAME_BYTES));
        ins(f, W::I64Add);
        ins(f, W::I64GeU);
        ins(f, W::I32Or);
        require(f, 0);
    }
}

/// A guest store must never overwrite its descriptor, frame, generation table
/// or code-page protection table through an overlapping advertised RAM backing.
/// These are host control buffers, with exclusive ownership during this call.
pub(super) fn procedure_store_layout(f: &mut Function) {
    procedure_store_layout_at(f, 0);
}

fn procedure_store_layout_at(f: &mut Function, depth: u32) {
    let spans = [
        (m::RAM_POINTER, m::RAM_LENGTH, 1),
        (m::GENERATIONS_POINTER, m::GENERATIONS_LENGTH, 8),
        (m::CODE_PAGES_POINTER, m::CODE_PAGES_LENGTH, 1),
    ];
    for (i, &(pointer, length, stride)) in spans.iter().enumerate() {
        // Empty tables cannot pass the common per-page store checks, and their
        // extent remains harmless here. All arithmetic is nonwrapping u64.
        descriptor_wide(f, pointer);
        descriptor_wide(f, length);
        wide(f, stride);
        ins(f, W::I64Mul);
        ins(f, W::I64Add);
        local_wide(f, FRAME);
        ins(f, W::I64LeU);
        descriptor_wide(f, pointer);
        local_wide(f, FRAME);
        wide(f, u64::from(m::OFFSET) + m::BYTES as u64);
        ins(f, W::I64Add);
        ins(f, W::I64GeU);
        ins(f, W::I32Or);
        require(f, depth);
        for &(other_pointer, other_length, other_stride) in &spans[..i] {
            descriptor_wide(f, pointer);
            descriptor_wide(f, length);
            wide(f, stride);
            ins(f, W::I64Mul);
            ins(f, W::I64Add);
            descriptor_wide(f, other_pointer);
            ins(f, W::I64LeU);
            descriptor_wide(f, other_pointer);
            descriptor_wide(f, other_length);
            wide(f, other_stride);
            ins(f, W::I64Mul);
            ins(f, W::I64Add);
            descriptor_wide(f, pointer);
            ins(f, W::I64LeU);
            ins(f, W::I32Or);
            require(f, depth);
        }
    }
}
