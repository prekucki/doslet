use super::{
    Accounting, Address, Alu, Checked, CompileError, Condition, EntryAlias, MemoryUnary, Operation,
    Region, RegionId, Register, Source, Terminator, Width, abi,
};
use crate::static_program::memory;
use alloc::vec::Vec;
use wasm_encoder::{
    BlockType, BranchHint, CodeSection, EntityType, ExportKind, ExportSection, FunctionSection,
    ImportSection, Instruction as W, MemArg, MemoryType, Module, Section, TypeSection, ValType,
};

#[path = "procedure_x87_emit.rs"]
mod procedure_x87_emit;
#[path = "x87_numeric_emit.rs"]
pub(super) mod x87_numeric_emit;

#[path = "inline_admission_emit.rs"]
mod inline_admission_emit;
#[path = "procedure_memory_emit.rs"]
mod procedure_memory_emit;
pub(super) use procedure_memory_emit::body as memory_procedure_body;
#[path = "procedure_memory_graph_emit.rs"]
mod procedure_memory_graph_emit;
pub(super) use procedure_memory_graph_emit::body as memory_procedure_graph_body;
#[path = "rle_kernel_emit.rs"]
mod rle_kernel_emit;
pub(super) use rle_kernel_emit::body as memory_rle_kernel_body;

#[path = "memory_emit.rs"]
mod memory_emit;
pub(super) use memory_emit::copy_kernel_body as memory_copy_kernel_body;
#[path = "shift_emit.rs"]
mod shift_emit;
#[path = "stack_emit.rs"]
mod stack_emit;

// Baseline function indices. Optional admission shifts defined functions by one
// while preserving the original module bytes when the option is disabled.
const RUNTIME: u32 = 0;
const ADMIT: u32 = 1;
// Inline admission excludes the runtime admission import, so the optional
// write helper is always the second imported function.
const MEMORY_WRITE: u32 = 1;
const RUN: u32 = 1;
const DISPATCH: u32 = 2;
const FALLBACK: u32 = 3;
const REGIONS: u32 = 4;
// Bound each address tree independently of the whole module. Its fixed
// per-entry emission stays below 2 MiB at this size, including RET service
// checks; a single tree for the full 262,144-entry limit exceeds engine limits.
const DISPATCH_CHUNK_ENTRIES: usize = 8192;
// Region parameters: frame, structural quota, GPR8, EFLAGS, budget, work, status.
const FRAME: u32 = 0;
const QUOTA: u32 = 1;
pub(super) const GPR: u32 = 2;
pub(super) const FLAGS: u32 = 10;
const BUDGET: u32 = 11;
const WORK: u32 = 12;
const STATUS: u32 = 13;
// Scratch locals are never transmitted over an edge.
const LEFT: u32 = 14;
const RIGHT: u32 = 15;
const RESULT: u32 = 16;
const CARRY: u32 = 17;
pub(super) const WIDE: u32 = 18;

pub(super) struct Function {
    inner: wasm_encoder::Function,
    branch_hints: Vec<BranchHint>,
}
impl Function {
    pub(super) fn new<L>(locals: L) -> Self
    where
        L: IntoIterator<Item = (u32, ValType)>,
        L::IntoIter: ExactSizeIterator,
    {
        Self {
            inner: wasm_encoder::Function::new(locals),
            branch_hints: Vec::new(),
        }
    }
    pub(super) fn byte_len(&self) -> usize {
        self.inner.byte_len()
    }
    pub(super) fn wasm(&self) -> &wasm_encoder::Function {
        &self.inner
    }
    pub(super) fn branch_hints(&self) -> &[BranchHint] {
        &self.branch_hints
    }
    pub(super) fn instruction(&mut self, instruction: &W<'_>) {
        self.inner.instruction(instruction);
    }
}

fn ins(f: &mut Function, op: W<'_>) {
    f.inner.instruction(&op);
}
fn ins_hinted(f: &mut Function, likely: bool, op: W<'_>) {
    debug_assert!(matches!(op, W::If(_) | W::BrIf(_)));
    f.branch_hints.push(BranchHint {
        branch_func_offset: f.inner.byte_len() as u32,
        branch_hint_value: u32::from(likely),
    });
    f.inner.instruction(&op);
}
fn c(f: &mut Function, value: u32) {
    ins(f, W::I32Const(value as i32));
}
fn wide(f: &mut Function, value: u64) {
    ins(f, W::I64Const(value as i64));
}
fn get(f: &mut Function, local: u32) {
    ins(f, W::LocalGet(local));
}
fn set(f: &mut Function, local: u32) {
    ins(f, W::LocalSet(local));
}
fn arg(offset: u32, is_wide: bool) -> MemArg {
    MemArg {
        offset: u64::from(offset),
        align: if is_wide { 3 } else { 2 },
        memory_index: 0,
    }
}
fn load(f: &mut Function, offset: u32, is_wide: bool) {
    get(f, FRAME);
    ins(
        f,
        if is_wide {
            W::I64Load(arg(offset, true))
        } else {
            W::I32Load(arg(offset, false))
        },
    );
}
fn store(f: &mut Function, offset: u32, local: u32, is_wide: bool) {
    get(f, FRAME);
    get(f, local);
    ins(
        f,
        if is_wide {
            W::I64Store(arg(offset, true))
        } else {
            W::I32Store(arg(offset, false))
        },
    );
}
fn store_constant(f: &mut Function, offset: u32, value: u32) {
    get(f, FRAME);
    c(f, value);
    ins(f, W::I32Store(arg(offset, false)));
}
fn read_register(f: &mut Function, r: Register) {
    get(f, GPR + r.index());
    if r.shift() != 0 {
        c(f, r.shift());
        ins(f, W::I32ShrU);
    }
    if r.width() != Width::Dword {
        c(f, r.width().mask());
        ins(f, W::I32And);
    }
}
// Value comes from a scratch local, so aliasing with the target is harmless.
fn write_register(f: &mut Function, r: Register, value: u32) {
    if r.width() == Width::Dword {
        get(f, value);
    } else {
        get(f, GPR + r.index());
        c(f, !(r.width().mask() << r.shift()));
        ins(f, W::I32And);
        get(f, value);
        c(f, r.width().mask());
        ins(f, W::I32And);
        if r.shift() != 0 {
            c(f, r.shift());
            ins(f, W::I32Shl);
        }
        ins(f, W::I32Or);
    }
    set(f, GPR + r.index());
}
fn source(f: &mut Function, s: Source) {
    match s {
        Source::Register(r) => read_register(f, r),
        Source::Immediate(v) => c(f, v),
        Source::VirtualRegister(local) => get(f, local),
    }
}

/// Consume a source-width-masked stack value, then assign through the existing
/// alias writer. Both register reads and guarded memory loads share this path.
fn extend(f: &mut Function, target: Register, width: Width, signed: bool) {
    if signed {
        ins(
            f,
            match width {
                Width::Byte => W::I32Extend8S,
                Width::Word => W::I32Extend16S,
                Width::Dword => unreachable!("validated extension source width"),
            },
        );
    }
    set(f, RESULT);
    write_register(f, target, RESULT);
}

// LEFT/RIGHT hold captured width-masked operands. Signed i32 products fit i64;
// unsigned i32 products fit the complete unsigned i64 bit pattern.
fn multiply_product(f: &mut Function, width: Width, signed: bool) {
    for operand in [LEFT, RIGHT] {
        get(f, operand);
        if signed {
            match width {
                Width::Byte => ins(f, W::I32Extend8S),
                Width::Word => ins(f, W::I32Extend16S),
                Width::Dword => {}
            }
        }
        ins(
            f,
            if signed {
                W::I64ExtendI32S
            } else {
                W::I64ExtendI32U
            },
        );
    }
    ins(f, W::I64Mul);
    ins(f, W::LocalTee(WIDE));
    ins(f, W::I32WrapI64);
    set(f, RESULT);
}

// Compare the full product with the signed or unsigned low-width result.
fn multiply_flags(f: &mut Function, width: Width, signed: bool) {
    get(f, FLAGS);
    c(f, !0x801);
    ins(f, W::I32And);
    get(f, WIDE);
    if signed {
        get(f, RESULT);
        match width {
            Width::Byte => ins(f, W::I32Extend8S),
            Width::Word => ins(f, W::I32Extend16S),
            Width::Dword => {}
        }
        ins(f, W::I64ExtendI32S);
        ins(f, W::I64Ne);
    } else {
        wide(f, u64::from(width.mask()));
        ins(f, W::I64GtU);
    }
    c(f, 0x801);
    ins(f, W::I32Mul);
    ins(f, W::I32Or);
    set(f, FLAGS);
}

// Retain the original truncated IMUL instruction sequence for existing modules.
fn signed_multiply(f: &mut Function, target: Register) {
    multiply_product(f, target.width(), true);
    multiply_flags(f, target.width(), true);
    write_register(f, target, RESULT);
}

fn accumulator(width: Width) -> Register {
    match width {
        Width::Byte => Register::Al,
        Width::Word => Register::Ax,
        Width::Dword => Register::Eax,
    }
}

fn wide_multiply(f: &mut Function, width: Width, signed: bool) {
    multiply_product(f, width, signed);
    multiply_flags(f, width, signed);
    write_register(
        f,
        if width == Width::Byte {
            Register::Ax
        } else {
            accumulator(width)
        },
        RESULT,
    );
    if width != Width::Byte {
        get(f, WIDE);
        wide(f, u64::from(width.bits()));
        ins(f, W::I64ShrU);
        ins(f, W::I32WrapI64);
        set(f, RESULT);
        write_register(
            f,
            if width == Width::Word {
                Register::Dx
            } else {
                Register::Edx
            },
            RESULT,
        );
    }
}

// Leave the offset on the stack; segment translation belongs only to accesses.
// Keep this sequence shared with memory preflight, preserving its bytecode.
fn effective_address(f: &mut Function, address: super::memory::MemoryAddress) {
    if address.scale & memory::VIRTUAL_DISPLACEMENT != 0 {
        get(f, address.displacement);
    } else {
        c(f, address.displacement);
    }
    if let Some(base) = address.base {
        read_register(f, base);
        ins(f, W::I32Add);
    }
    if let Some(index) = address.index {
        read_register(f, index);
        c(f, u32::from(address.scale & !memory::VIRTUAL_DISPLACEMENT));
        ins(f, W::I32Mul);
        ins(f, W::I32Add);
    }
    if address.address16 {
        c(f, 0xffff);
        ins(f, W::I32And);
    }
}

fn alu(f: &mut Function, kind: Alu, width: Width, preserve_carry: bool) {
    alu_carry(f, kind);
    alu_result(f, kind, width, preserve_carry);
}

fn alu_carry(f: &mut Function, kind: Alu) {
    if matches!(kind, Alu::Adc | Alu::Sbb) {
        get(f, FLAGS);
        c(f, 1);
        ins(f, W::I32And);
    } else {
        c(f, 0);
    }
    set(f, CARRY);
}

/// Consume the snapshotted LEFT/RIGHT/CARRY operands. Shared by register and
/// memory instructions so arithmetic and deterministic undefined flags agree.
fn alu_result(f: &mut Function, kind: Alu, width: Width, preserve_carry: bool) {
    let subtract = matches!(kind, Alu::Sub | Alu::Sbb | Alu::Cmp);
    let logical = matches!(kind, Alu::And | Alu::Or | Alu::Xor | Alu::Test);
    get(f, LEFT);
    if !logical {
        ins(f, W::I64ExtendI32U);
    }
    get(f, RIGHT);
    if logical {
        ins(
            f,
            match kind {
                Alu::And | Alu::Test => W::I32And,
                Alu::Or => W::I32Or,
                _ => W::I32Xor,
            },
        );
    } else {
        ins(f, W::I64ExtendI32U);
        get(f, CARRY);
        ins(f, W::I64ExtendI32U);
        ins(f, W::I64Add);
        ins(f, if subtract { W::I64Sub } else { W::I64Add });
        ins(f, W::LocalTee(WIDE));
        ins(f, W::I32WrapI64);
    }
    if width != Width::Dword {
        c(f, width.mask());
        ins(f, W::I32And);
    }
    set(f, RESULT);
    get(f, FLAGS);
    c(f, !0x8d5);
    ins(f, W::I32And);
    c(f, 2);
    ins(f, W::I32Or);
    // ZF, SF and even parity of the low byte. Logical AF deterministically clears.
    get(f, RESULT);
    ins(f, W::I32Eqz);
    c(f, 6);
    ins(f, W::I32Shl);
    ins(f, W::I32Or);
    get(f, RESULT);
    c(f, width.bits() - 1);
    ins(f, W::I32ShrU);
    c(f, 7);
    ins(f, W::I32Shl);
    ins(f, W::I32Or);
    get(f, RESULT);
    c(f, 255);
    ins(f, W::I32And);
    ins(f, W::I32Popcnt);
    c(f, 1);
    ins(f, W::I32And);
    ins(f, W::I32Eqz);
    c(f, 2);
    ins(f, W::I32Shl);
    ins(f, W::I32Or);
    if !logical {
        // Widened subtraction wraps to 64 bits on borrow. Its bit at the
        // operand width detects both borrow and ADC carry at every width.
        get(f, WIDE);
        wide(f, u64::from(width.bits()));
        ins(f, W::I64ShrU);
        ins(f, W::I32WrapI64);
        c(f, 1);
        ins(f, W::I32And);
        ins(f, W::I32Or);
        get(f, LEFT);
        get(f, RIGHT);
        ins(f, W::I32Xor);
        get(f, RESULT);
        ins(f, W::I32Xor);
        c(f, 16);
        ins(f, W::I32And);
        ins(f, W::I32Or);
        get(f, LEFT);
        get(f, RIGHT);
        ins(f, W::I32Xor);
        if !subtract {
            c(f, u32::MAX);
            ins(f, W::I32Xor);
        }
        get(f, LEFT);
        get(f, RESULT);
        ins(f, W::I32Xor);
        ins(f, W::I32And);
        c(f, width.bits() - 1);
        ins(f, W::I32ShrU);
        c(f, 1);
        ins(f, W::I32And);
        c(f, 11);
        ins(f, W::I32Shl);
        ins(f, W::I32Or);
    }
    if preserve_carry {
        c(f, !1);
        ins(f, W::I32And);
        get(f, FLAGS);
        c(f, 1);
        ins(f, W::I32And);
        ins(f, W::I32Or);
    }
    set(f, FLAGS);
}
pub(super) fn operation(f: &mut Function, op: Operation) {
    match op {
        Operation::X87(_) => unreachable!("x87 requires opted-in procedure emitter"),
        Operation::Nop => {}
        Operation::Move { target, source: s } => {
            source(f, s);
            set(f, RESULT);
            write_register(f, target, RESULT);
        }
        Operation::Extend {
            target,
            source,
            signed,
        } => {
            read_register(f, source);
            extend(f, target, source.width(), signed);
        }
        Operation::SignedMultiply {
            target,
            left,
            right,
        } => {
            source(f, left);
            set(f, LEFT);
            source(f, right);
            set(f, RIGHT);
            signed_multiply(f, target);
        }
        Operation::WideMultiply {
            signed,
            width,
            source: s,
        } => {
            source(f, s);
            set(f, LEFT);
            read_register(f, accumulator(width));
            set(f, RIGHT);
            wide_multiply(f, width, signed);
        }
        Operation::Lea { target, address } => {
            effective_address(f, address);
            set(f, RESULT);
            write_register(f, target, RESULT);
        }
        Operation::LoadMemory { target, .. } => memory_emit::load_memory(f, target),
        Operation::ExtendMemory {
            target,
            width,
            signed,
            ..
        } => {
            memory_emit::load_value(f, width);
            get(f, RESULT);
            extend(f, target, width, signed);
        }
        Operation::SignedMultiplyMemory {
            target, source: s, ..
        } => {
            // Read helpers preserve RIGHT while using LEFT/RESULT as scratch.
            source(f, s);
            set(f, RIGHT);
            memory_emit::load_value(f, target.width());
            get(f, RESULT);
            set(f, LEFT);
            signed_multiply(f, target);
        }
        Operation::WideMultiplyMemory { signed, width, .. } => {
            // Read helpers preserve RIGHT while using LEFT/RESULT as scratch.
            read_register(f, accumulator(width));
            set(f, RIGHT);
            memory_emit::load_value(f, width);
            get(f, RESULT);
            set(f, LEFT);
            wide_multiply(f, width, signed);
        }
        Operation::StoreMemory { width, source, .. } => memory_emit::store_memory(f, width, source),
        Operation::StringLoad {
            width, address16, ..
        } => {
            memory_emit::load_memory(f, accumulator(width));
            string_advance(f, true, width, address16);
        }
        Operation::StringStore { width, address16 } => {
            memory_emit::store_memory(f, width, Source::Register(accumulator(width)));
            string_advance(f, false, width, address16);
        }
        Operation::Push { width, source } => stack_emit::push(f, width, source),
        Operation::Pop { target } => stack_emit::pop(f, target),
        Operation::AluMemory { kind, target, .. } => {
            // Device reads use LEFT/RESULT as scratch, but preserve GPRs,
            // FLAGS and CARRY. Snapshot CF before reading the source.
            alu_carry(f, kind);
            memory_emit::load_value(f, target.width());
            get(f, RESULT);
            set(f, RIGHT);
            read_register(f, target);
            set(f, LEFT);
            alu_result(f, kind, target.width(), false);
            if !matches!(kind, Alu::Cmp | Alu::Test) {
                write_register(f, target, RESULT);
            }
        }
        Operation::CompareMemory {
            test,
            width,
            source: s,
            ..
        } => {
            let kind = if test { Alu::Test } else { Alu::Cmp };
            alu_carry(f, kind);
            source(f, s);
            set(f, RIGHT);
            memory_emit::load_value(f, width);
            get(f, RESULT);
            set(f, LEFT);
            alu_result(f, kind, width, false);
        }
        Operation::AluMemoryStore {
            kind,
            width,
            source: s,
            ..
        } => {
            // Capture old CF and aliased source registers before the RAM read.
            alu_carry(f, kind);
            source(f, s);
            set(f, RIGHT);
            memory_emit::load_ram_value(f, width);
            get(f, RESULT);
            set(f, LEFT);
            alu_result(f, kind, width, false);
            memory_emit::store_result(f, width);
        }
        Operation::UnaryMemory { kind, width, .. } => {
            memory_emit::load_ram_value(f, width);
            match kind {
                MemoryUnary::Inc | MemoryUnary::Dec => {
                    get(f, RESULT);
                    set(f, LEFT);
                    c(f, 1);
                    set(f, RIGHT);
                    alu(
                        f,
                        if kind == MemoryUnary::Inc {
                            Alu::Add
                        } else {
                            Alu::Sub
                        },
                        width,
                        true,
                    );
                }
                MemoryUnary::Neg => {
                    get(f, RESULT);
                    set(f, RIGHT);
                    c(f, 0);
                    set(f, LEFT);
                    alu(f, Alu::Sub, width, false);
                }
                MemoryUnary::Not => {
                    get(f, RESULT);
                    c(f, width.mask());
                    ins(f, W::I32Xor);
                    set(f, RESULT);
                }
            }
            memory_emit::store_result(f, width);
        }
        Operation::Alu {
            kind,
            target,
            source: s,
        } => {
            read_register(f, target);
            set(f, LEFT);
            source(f, s);
            set(f, RIGHT);
            alu(f, kind, target.width(), false);
            if !matches!(kind, Alu::Cmp | Alu::Test) {
                write_register(f, target, RESULT);
            }
        }
        Operation::Shift {
            kind,
            target,
            count,
        } => shift_emit::shift(f, kind, target, count),
        Operation::DoubleShift {
            kind,
            target,
            source,
            count,
        } => shift_emit::double_shift(f, kind, target, source, count),
        Operation::Inc(r) | Operation::Dec(r) => {
            read_register(f, r);
            set(f, LEFT);
            c(f, 1);
            set(f, RIGHT);
            alu(
                f,
                if matches!(op, Operation::Inc(_)) {
                    Alu::Add
                } else {
                    Alu::Sub
                },
                r.width(),
                true,
            );
            write_register(f, r, RESULT);
        }
        Operation::Neg(r) => {
            c(f, 0);
            set(f, LEFT);
            read_register(f, r);
            set(f, RIGHT);
            alu(f, Alu::Sub, r.width(), false);
            write_register(f, r, RESULT);
        }
        Operation::Not(r) => {
            read_register(f, r);
            c(f, r.width().mask());
            ins(f, W::I32Xor);
            set(f, RESULT);
            write_register(f, r, RESULT);
        }
        Operation::Exchange { left, right } => {
            // Snapshot both old aliases before either merge (AL <-> AH).
            read_register(f, left);
            set(f, LEFT);
            read_register(f, right);
            set(f, RIGHT);
            write_register(f, left, RIGHT);
            write_register(f, right, LEFT);
        }
    }
}
fn flag(f: &mut Function, bit: u32) {
    get(f, FLAGS);
    c(f, bit);
    ins(f, W::I32And);
    ins(f, W::I32Eqz);
    ins(f, W::I32Eqz);
}
fn condition(f: &mut Function, cc: Condition) {
    let cc = cc as u8;
    match cc >> 1 {
        0 => flag(f, 0x800),
        1 => flag(f, 1),
        2 => flag(f, 0x40),
        3 => {
            flag(f, 1);
            flag(f, 0x40);
            ins(f, W::I32Or);
        }
        4 => flag(f, 0x80),
        5 => flag(f, 4),
        _ => {
            flag(f, 0x80);
            flag(f, 0x800);
            ins(f, W::I32Xor);
            if cc >> 1 == 7 {
                flag(f, 0x40);
                ins(f, W::I32Or);
            }
        }
    }
    if cc & 1 != 0 {
        ins(f, W::I32Eqz);
    }
}
fn flush(f: &mut Function, address: Address) {
    for i in 0..8 {
        store(f, i * 4, GPR + i, false);
    }
    store(f, abi::EFLAGS, FLAGS, false);
    store_constant(f, abi::SPACE, address.space);
    store_constant(f, abi::OFFSET, address.offset);
    store(f, abi::BUDGET, BUDGET, true);
    store(f, abi::CHARGED_WORK, WORK, true);
    store(f, abi::STATUS, STATUS, false);
}
fn flush_return(f: &mut Function, target_space: u32) {
    for i in 0..8 {
        store(f, i * 4, GPR + i, false);
    }
    store(f, abi::EFLAGS, FLAGS, false);
    store_constant(f, abi::SPACE, target_space);
    store(f, abi::OFFSET, RESULT, false);
    store(f, abi::BUDGET, BUDGET, true);
    store(f, abi::CHARGED_WORK, WORK, true);
    store(f, abi::STATUS, STATUS, false);
}
fn function_index(checked: &Checked<'_>, index: u32) -> u32 {
    index
        + u32::from(checked.options.runtime_admission)
        + u32::from(checked.options.memory_write_helper)
}
fn callback(f: &mut Function, checked: &Checked<'_>, reason: u32) {
    get(f, FRAME);
    c(f, reason);
    ins(f, W::Call(RUNTIME));
    ins(f, W::If(BlockType::Empty));
    c(f, abi::EXIT_RUNTIME);
    ins(f, W::Return);
    ins(f, W::End);
    get(f, FRAME);
    c(f, checked.options.service_interval_regions);
    ins(f, W::ReturnCall(function_index(checked, DISPATCH)));
}
fn region_index(checked: &Checked<'_>, id: RegionId) -> u32 {
    // All targets were validated before emission; this lookup cannot fail.
    function_index(checked, REGIONS) + checked.position(id).expect("validated region target") as u32
}
fn destination(checked: &Checked<'_>, id: RegionId) -> Address {
    checked.program.regions[checked.by_id[checked.position(id).expect("validated region target")]]
        .entry
}
fn charge(f: &mut Function, count: usize) {
    if count == 0 {
        return;
    }
    // Saturating charged counter, with a sticky indication that it is inexact.
    get(f, WORK);
    wide(f, u64::MAX - count as u64);
    ins(f, W::I64GtU);
    ins(f, W::If(BlockType::Empty));
    wide(f, u64::MAX);
    set(f, WORK);
    get(f, STATUS);
    c(f, abi::COUNTER_OVERFLOW);
    ins(f, W::I32Or);
    set(f, STATUS);
    ins(f, W::Else);
    get(f, WORK);
    wide(f, count as u64);
    ins(f, W::I64Add);
    set(f, WORK);
    ins(f, W::End);
    // Saturating budget subtraction allows only one bounded region overshoot.
    get(f, BUDGET);
    wide(f, count as u64);
    ins(f, W::I64LeU);
    ins(f, W::If(BlockType::Result(ValType::I64)));
    wide(f, 0);
    ins(f, W::Else);
    get(f, BUDGET);
    wide(f, count as u64);
    ins(f, W::I64Sub);
    ins(f, W::End);
    set(f, BUDGET);
}
fn edge(
    f: &mut Function,
    checked: &Checked<'_>,
    r: &Region,
    target: Option<RegionId>,
    address: Address,
) {
    if r.accounting == Accounting::Counted {
        get(f, BUDGET);
        ins(f, W::I64Eqz);
        ins(f, W::If(BlockType::Empty));
        flush(f, address);
        c(f, abi::EXIT_BUDGET);
        ins(f, W::Return);
        ins(f, W::End);
    }
    if target.is_none() {
        flush(f, address);
        callback(f, checked, abi::REASON_FALLBACK);
        return;
    }
    get(f, QUOTA);
    c(f, 1);
    ins(f, W::I32Sub);
    ins(f, W::LocalTee(QUOTA));
    ins(f, W::I32Eqz);
    ins(f, W::If(BlockType::Empty));
    flush(f, address);
    callback(f, checked, abi::REASON_SERVICE);
    ins(f, W::End);
    for local in FRAME..=STATUS {
        get(f, local);
    }
    ins(
        f,
        W::ReturnCall(region_index(checked, target.expect("direct edge"))),
    );
}
/// Advance the string index after a successful single-element access. These
/// WASM arithmetic operations deliberately leave the guest FLAGS local intact.
pub(super) fn string_advance(f: &mut Function, source: bool, width: Width, address16: bool) {
    let target = match (source, address16) {
        (true, true) => Register::Si,
        (true, false) => Register::Esi,
        (false, true) => Register::Di,
        (false, false) => Register::Edi,
    };
    read_register(f, target);
    get(f, FLAGS);
    c(f, 0x400); // DF
    ins(f, W::I32And);
    ins(f, W::If(BlockType::Result(ValType::I32)));
    c(f, (width.bits() / 8).wrapping_neg());
    ins(f, W::Else);
    c(f, width.bits() / 8);
    ins(f, W::End);
    ins(f, W::I32Add);
    set(f, RESULT);
    write_register(f, target, RESULT);
}

fn region(checked: &Checked<'_>, r: &Region) -> Result<Function, CompileError> {
    use memory_emit::Access;
    let memory = r.operations.iter().find_map(|op| match *op {
        Operation::LoadMemory { target, address }
        | Operation::SignedMultiplyMemory {
            target, address, ..
        }
        | Operation::AluMemory {
            target, address, ..
        } => Some((target.width(), address, Access::Read)),
        Operation::CompareMemory { width, address, .. }
        | Operation::ExtendMemory { width, address, .. }
        | Operation::WideMultiplyMemory { width, address, .. } => {
            Some((width, address, Access::Read))
        }
        Operation::StoreMemory { width, address, .. } => Some((
            width,
            address,
            if checked.options.memory_write_helper {
                Access::WriteHelper
            } else {
                Access::Write
            },
        )),
        Operation::StringLoad {
            width,
            address16,
            segment,
        } => Some((
            width,
            memory::MemoryAddress::string(true, address16, segment),
            Access::Read,
        )),
        Operation::StringStore { width, address16 } => Some((
            width,
            memory::MemoryAddress::string(false, address16, memory::Segment::Es),
            Access::Write,
        )),
        Operation::AluMemoryStore { width, address, .. }
        | Operation::UnaryMemory { width, address, .. } => {
            Some((width, address, Access::ReadModifyWrite))
        }
        _ => None,
    });
    let stack = r
        .operations
        .iter()
        .find_map(|op| match *op {
            Operation::Push { width, .. } => Some((width, true)),
            Operation::Pop { target } => Some((target.width(), false)),
            _ => None,
        })
        .or(match r.terminator {
            Terminator::Return { width, .. } => Some((width, false)),
            _ => None,
        });
    // Retain the original declarations/bytecode for register-only regions.
    let mut f = if memory.is_some() || stack.is_some() {
        Function::new([
            (4, ValType::I32),
            (1, ValType::I64),
            (memory_emit::LOCAL_COUNT, ValType::I32),
        ])
    } else {
        Function::new([(4, ValType::I32), (1, ValType::I64)])
    };
    if let Some((width, address, access)) = memory {
        memory_emit::preflight_access(&mut f, checked, r, width, address, access);
    }
    if let Some((width, push)) = stack {
        stack_emit::preflight(&mut f, checked, r, width, push);
    }
    if checked.options.inline_admission && r.completed_instructions() != 0 {
        inline_admission_emit::preflight(&mut f, checked, r);
    }
    if checked.options.runtime_admission && r.completed_instructions() != 0 {
        get(&mut f, FRAME);
        c(&mut f, r.id.0);
        c(&mut f, r.completed_instructions() as u32);
        ins(&mut f, W::Call(ADMIT));
        ins(&mut f, W::I32Eqz);
        ins(&mut f, W::If(BlockType::Empty));
        flush(&mut f, r.entry);
        callback(&mut f, checked, abi::REASON_FALLBACK);
        ins(&mut f, W::End);
    }
    for op in &r.operations {
        if let Operation::StoreMemory { width, source, .. } = *op
            && checked.options.memory_write_helper
        {
            memory_emit::store_with_helper(&mut f, checked, r, width, source);
        } else {
            operation(&mut f, *op);
        }
        if f.byte_len() > checked.options.max_output_bytes {
            return Err(CompileError::OutputTooLarge);
        }
    }
    if let Terminator::Return {
        width, pop_bytes, ..
    } = r.terminator
    {
        stack_emit::near_return(&mut f, width, pop_bytes);
    }
    if r.accounting == Accounting::Counted {
        charge(&mut f, r.completed_instructions());
    } else {
        get(&mut f, STATUS);
        c(&mut f, abi::UNCOUNTED_SEEN);
        ins(&mut f, W::I32Or);
        set(&mut f, STATUS);
    }
    match r.terminator {
        Terminator::Fallthrough(id) | Terminator::Jump(id) => {
            edge(&mut f, checked, r, Some(id), destination(checked, id))
        }
        Terminator::Branch {
            condition: cc,
            taken,
            not_taken,
        } => {
            condition(&mut f, cc);
            ins(&mut f, W::If(BlockType::Empty));
            edge(&mut f, checked, r, Some(taken), destination(checked, taken));
            ins(&mut f, W::Else);
            edge(
                &mut f,
                checked,
                r,
                Some(not_taken),
                destination(checked, not_taken),
            );
            ins(&mut f, W::End);
        }
        Terminator::Fallback(address) => edge(&mut f, checked, r, None, address),
        Terminator::Return { target_space, .. } => {
            flush_return(&mut f, target_space);
            get(&mut f, FRAME);
            get(&mut f, QUOTA);
            c(&mut f, 1);
            ins(&mut f, W::I32Sub);
            ins(&mut f, W::ReturnCall(function_index(checked, DISPATCH)));
        }
    }
    // Both branch arms tail-return, but an empty-typed if does not make the
    // enclosing control frame polymorphic for the validator.
    ins(&mut f, W::Unreachable);
    ins(&mut f, W::End);
    Ok(f)
}
fn loaded_arguments(f: &mut Function) {
    get(f, FRAME);
    get(f, QUOTA);
    for i in 0..8 {
        load(f, i * 4, false);
    }
    load(f, abi::EFLAGS, false);
    load(f, abi::BUDGET, true);
    load(f, abi::CHARGED_WORK, true);
    load(f, abi::STATUS, false);
}
fn address_equal(f: &mut Function, address: Address) {
    get(f, 2);
    c(f, address.space);
    ins(f, W::I32Eq);
    get(f, 3);
    c(f, address.offset);
    ins(f, W::I32Eq);
    ins(f, W::I32And);
}
fn address_less(f: &mut Function, address: Address) {
    get(f, 2);
    c(f, address.space);
    ins(f, W::I32LtU);
    get(f, 2);
    c(f, address.space);
    ins(f, W::I32Eq);
    get(f, 3);
    c(f, address.offset);
    ins(f, W::I32LtU);
    ins(f, W::I32And);
    ins(f, W::I32Or);
}
fn decisions(
    f: &mut Function,
    checked: &Checked<'_>,
    entries: &[EntryAlias],
    service_at_dispatch: bool,
) -> Result<(), CompileError> {
    if f.byte_len() > checked.options.max_output_bytes {
        return Err(CompileError::OutputTooLarge);
    }
    if entries.is_empty() {
        get(f, FRAME);
        get(f, QUOTA);
        ins(f, W::ReturnCall(function_index(checked, FALLBACK)));
        return Ok(());
    }
    let middle = entries.len() / 2;
    let entry = entries[middle];
    address_less(f, entry.address);
    ins(f, W::If(BlockType::Empty));
    decisions(f, checked, &entries[..middle], service_at_dispatch)?;
    ins(f, W::Else);
    address_equal(f, entry.address);
    ins(f, W::If(BlockType::Empty));
    if service_at_dispatch {
        // A dynamic return consumes its structural edge before lookup. Service
        // only known targets here: unknown targets retain ordinary fallback,
        // and the dispatcher's budget check still precedes either callback.
        get(f, QUOTA);
        ins(f, W::I32Eqz);
        ins(f, W::If(BlockType::Empty));
        callback(f, checked, abi::REASON_SERVICE);
        ins(f, W::End);
    }
    loaded_arguments(f);
    ins(f, W::ReturnCall(region_index(checked, entry.target)));
    ins(f, W::Else);
    decisions(f, checked, &entries[middle + 1..], service_at_dispatch)?;
    ins(f, W::End);
    ins(f, W::End);
    Ok(())
}

fn dispatch_address(f: &mut Function) {
    load(f, abi::SPACE, false);
    set(f, 2);
    load(f, abi::OFFSET, false);
    set(f, 3);
}

fn dispatch_helper_index(checked: &Checked<'_>, chunk: usize) -> u32 {
    // Append after every region and the optional inline-admission helper.
    function_index(checked, REGIONS)
        + checked.by_id.len() as u32
        + u32::from(checked.options.inline_admission)
        + chunk as u32
}

fn chunk_decisions(
    f: &mut Function,
    checked: &Checked<'_>,
    first: usize,
    count: usize,
) -> Result<(), CompileError> {
    if f.byte_len() > checked.options.max_output_bytes {
        return Err(CompileError::OutputTooLarge);
    }
    if count == 1 {
        get(f, FRAME);
        get(f, QUOTA);
        ins(f, W::ReturnCall(dispatch_helper_index(checked, first)));
        return Ok(());
    }
    let left_count = count / 2;
    let middle = first + left_count;
    // The first address of the right chunk separates two sorted ranges.
    // Gaps and out-of-range addresses still reach an ordinary exact lookup,
    // which alone can choose either a known target or FALLBACK.
    address_less(
        f,
        checked.by_address[middle * DISPATCH_CHUNK_ENTRIES].address,
    );
    ins(f, W::If(BlockType::Empty));
    chunk_decisions(f, checked, first, left_count)?;
    ins(f, W::Else);
    chunk_decisions(f, checked, middle, count - left_count)?;
    ins(f, W::End);
    Ok(())
}

pub(super) fn module(checked: &Checked<'_>) -> Result<(Vec<u8>, usize), CompileError> {
    // Small programs retain the original dispatcher and identical bytecode.
    // No chunk index/list is allocated: Checked already owns the bounded,
    // sorted entry slice, and each helper is emitted and appended separately.
    let dispatch_helpers = if checked.by_address.len() > DISPATCH_CHUNK_ENTRIES {
        checked.by_address.len().div_ceil(DISPATCH_CHUNK_ENTRIES)
    } else {
        0
    };
    let mut types = TypeSection::new();
    types
        .ty()
        .function([ValType::I32, ValType::I32], [ValType::I32]);
    types.ty().function([ValType::I32], [ValType::I32]);
    types.ty().function(
        [
            ValType::I32,
            ValType::I32,
            ValType::I32,
            ValType::I32,
            ValType::I32,
            ValType::I32,
            ValType::I32,
            ValType::I32,
            ValType::I32,
            ValType::I32,
            ValType::I32,
            ValType::I64,
            ValType::I64,
            ValType::I32,
        ],
        [ValType::I32],
    );
    if checked.options.runtime_admission || checked.options.inline_admission {
        types
            .ty()
            .function([ValType::I32, ValType::I32, ValType::I32], [ValType::I32]);
    }
    if checked.options.memory_write_helper {
        types.ty().function([ValType::I32; 4], [ValType::I32]);
    }
    let mut imports = ImportSection::new();
    imports.import(
        abi::IMPORT_MODULE,
        abi::MEMORY_IMPORT,
        EntityType::Memory(MemoryType {
            minimum: 1,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        }),
    );
    imports.import(
        abi::IMPORT_MODULE,
        abi::RUNTIME_IMPORT,
        EntityType::Function(0),
    );
    if checked.options.runtime_admission {
        imports.import(
            abi::IMPORT_MODULE,
            abi::ADMIT_IMPORT,
            EntityType::Function(3),
        );
    }
    if checked.options.memory_write_helper {
        imports.import(
            abi::IMPORT_MODULE,
            abi::MEMORY_WRITE_IMPORT,
            EntityType::Function(4),
        );
    }
    let mut functions = FunctionSection::new();
    functions.function(1);
    functions.function(0);
    functions.function(0);
    for _ in &checked.by_id {
        functions.function(2);
    }
    // Append the helper so every existing function index remains unchanged.
    if checked.options.inline_admission {
        functions.function(3);
    }
    for _ in 0..dispatch_helpers {
        functions.function(0);
    }
    let mut exports = ExportSection::new();
    exports.export(
        abi::RUN_EXPORT,
        ExportKind::Func,
        function_index(checked, RUN),
    );
    exports.export("memory", ExportKind::Memory, 0);
    let mut code = CodeSection::new();
    let mut run = Function::new([]);
    get(&mut run, FRAME);
    c(&mut run, checked.options.service_interval_regions);
    ins(&mut run, W::ReturnCall(function_index(checked, DISPATCH)));
    ins(&mut run, W::End);
    code.function(run.wasm());
    let mut dispatch = Function::new([(2, ValType::I32)]);
    // An exhausted initial/refilled budget never starts a new region. A HOT
    // loop with a positive budget remains bounded by the structural callback.
    load(&mut dispatch, abi::BUDGET, true);
    ins(&mut dispatch, W::I64Eqz);
    ins(&mut dispatch, W::If(BlockType::Empty));
    c(&mut dispatch, abi::EXIT_BUDGET);
    ins(&mut dispatch, W::Return);
    ins(&mut dispatch, W::End);
    dispatch_address(&mut dispatch);
    let service_at_dispatch = checked
        .program
        .regions
        .iter()
        .any(|r| matches!(r.terminator, Terminator::Return { .. }));
    if dispatch_helpers == 0 {
        decisions(
            &mut dispatch,
            checked,
            &checked.by_address,
            service_at_dispatch,
        )?;
    } else {
        chunk_decisions(&mut dispatch, checked, 0, dispatch_helpers)?;
    }
    ins(&mut dispatch, W::Unreachable);
    ins(&mut dispatch, W::End);
    let mut dispatcher_bytes = dispatch.byte_len();
    code.function(dispatch.wasm());
    let mut fallback = Function::new([]);
    callback(&mut fallback, checked, abi::REASON_FALLBACK);
    ins(&mut fallback, W::End);
    code.function(fallback.wasm());
    for &index in &checked.by_id {
        let f = region(checked, &checked.program.regions[index])?;
        if code
            .byte_len()
            .saturating_add(f.byte_len())
            .saturating_add(5)
            > checked.options.max_output_bytes
        {
            return Err(CompileError::OutputTooLarge);
        }
        code.function(f.wasm());
    }
    if checked.options.inline_admission {
        code.function(inline_admission_emit::helper().wasm());
    }
    if dispatch_helpers != 0 {
        for entries in checked.by_address.chunks(DISPATCH_CHUNK_ENTRIES) {
            let mut helper = Function::new([(2, ValType::I32)]);
            dispatch_address(&mut helper);
            decisions(&mut helper, checked, entries, service_at_dispatch)?;
            ins(&mut helper, W::Unreachable);
            ins(&mut helper, W::End);
            if code
                .byte_len()
                .saturating_add(helper.byte_len())
                .saturating_add(5)
                > checked.options.max_output_bytes
            {
                return Err(CompileError::OutputTooLarge);
            }
            dispatcher_bytes += helper.byte_len();
            code.function(helper.wasm());
        }
    }
    let capacity = code
        .byte_len()
        .saturating_add((checked.by_id.len() + dispatch_helpers) * 5)
        .saturating_add(1024);
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| CompileError::Allocation)?;
    bytes.extend_from_slice(&Module::HEADER);
    types.append_to(&mut bytes);
    imports.append_to(&mut bytes);
    functions.append_to(&mut bytes);
    exports.append_to(&mut bytes);
    code.append_to(&mut bytes);
    if bytes.len() > checked.options.max_output_bytes {
        return Err(CompileError::OutputTooLarge);
    }
    Ok((bytes, dispatcher_bytes))
}
