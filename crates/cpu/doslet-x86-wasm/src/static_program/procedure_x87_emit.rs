//! Exact PC24 values stay in locals across scalar instructions and backedges.
//! The relative stack topology is proved at setup. Only boundary import/export
//! rotates slots; numerical operations have no calls or owner synchronization.
use super::procedure_memory_emit::{VirtualRegisters, instruction_body};
use super::x87_numeric_emit::{self as numeric, NumericLocals};
use super::*;
use crate::static_program::procedure_memory::{
    X87_BOUNDARY_OFFSET as BASE, X87_SELECTORS_OFFSET, X87Procedure, x87,
};
use crate::static_program::{X87Instruction, X87Operation};
use doslet_x87::pc24_boundary as wire;

const I32_COUNT: u32 = 24 + NumericLocals::I32_COUNT;
const F64_COUNT: u32 = 11 + NumericLocals::F64_COUNT;
const I64_COUNT: u32 = NumericLocals::I64_COUNT;

#[derive(Clone, Copy)]
pub(super) struct Locals {
    base: u32,
}
impl Locals {
    pub(super) fn new(vregs: &VirtualRegisters) -> Self {
        Self {
            base: vregs.locals_end(),
        }
    }
    fn i(self, n: u32) -> u32 {
        self.base + n
    }
    fn value(self, slot: u8) -> u32 {
        self.base + I32_COUNT + u32::from(slot)
    }
    fn double(self, n: u32) -> u32 {
        self.base + I32_COUNT + 8 + n
    }
    fn numeric(self) -> NumericLocals {
        NumericLocals {
            f64_base: self.base + I32_COUNT + 11,
            i64_base: self.base + I32_COUNT + F64_COUNT,
            i32_base: self.i(24),
        }
    }
    fn cw(self) -> u32 {
        self.i(8)
    }
    fn sw(self) -> u32 {
        self.i(9)
    }
    fn tags(self) -> u32 {
        self.i(10)
    }
    fn top(self) -> u32 {
        self.i(11)
    }
}

pub(super) fn append_locals(locals: &mut Vec<(u32, ValType)>) {
    locals.extend([
        (I32_COUNT, ValType::I32),
        (F64_COUNT, ValType::F64),
        (I64_COUNT, ValType::I64),
    ]);
}
fn constant(f: &mut Function, value: u32, local: u32) {
    c(f, value);
    set(f, local);
}
fn copy(f: &mut Function, source: u32, target: u32) {
    get(f, source);
    set(f, target);
}
fn load16(f: &mut Function, offset: u32) {
    get(f, FRAME);
    ins(
        f,
        W::I32Load16U(MemArg {
            align: 1,
            ..arg(offset, false)
        }),
    );
}
fn store_small(f: &mut Function, offset: u32, local: u32, byte: bool) {
    get(f, FRAME);
    get(f, local);
    ins(
        f,
        if byte {
            W::I32Store8(MemArg {
                align: 0,
                ..arg(offset, false)
            })
        } else {
            W::I32Store16(MemArg {
                align: 1,
                ..arg(offset, false)
            })
        },
    );
}
fn require(f: &mut Function, refused: &impl Fn(&mut Function)) {
    ins(f, W::I32Eqz);
    ins(f, W::If(BlockType::Empty));
    refused(f);
    ins(f, W::End);
}
fn physical(f: &mut Function, n: Locals, slot: u8) {
    get(f, n.top());
    c(f, u32::from(slot));
    ins(f, W::I32Add);
    c(f, 7);
    ins(f, W::I32And);
}
fn array_address(f: &mut Function, n: Locals, slot: u8, offset: usize, stride: u32) {
    get(f, FRAME);
    physical(f, n, slot);
    c(f, stride);
    ins(f, W::I32Mul);
    ins(f, W::I32Add);
    c(f, BASE + offset as u32);
    ins(f, W::I32Add);
}
fn rotate(f: &mut Function, n: Locals, local: u32, bits: u32, left: bool) {
    get(f, local);
    get(f, n.top());
    if bits == 16 {
        c(f, 1);
        ins(f, W::I32Shl);
    }
    ins(f, if left { W::I32Shl } else { W::I32ShrU });
    get(f, local);
    c(f, bits);
    get(f, n.top());
    if bits == 16 {
        c(f, 1);
        ins(f, W::I32Shl);
    }
    ins(f, W::I32Sub);
    ins(f, if left { W::I32ShrU } else { W::I32Shl });
    ins(f, W::I32Or);
    c(f, (1 << bits) - 1);
    ins(f, W::I32And);
}

pub(super) fn initialize(
    f: &mut Function,
    n: Locals,
    permissions: X87Procedure,
    refused: impl Fn(&mut Function),
) {
    memory_emit::preflight_with(f, memory_emit::x87_frame_layout, &refused);
    load(f, BASE + wire::MAGIC as u32, false);
    c(f, wire::MAGIC_VALUE);
    ins(f, W::I32Eq);
    require(f, &refused);
    load(f, BASE + wire::VERSION as u32, false);
    c(f, wire::VERSION_VALUE);
    ins(f, W::I32Eq);
    require(f, &refused);
    load16(f, BASE + wire::CONTROL as u32);
    set(f, n.cw());
    get(f, n.cw());
    c(f, 0x33f);
    ins(f, W::I32And);
    c(f, 0x3f);
    ins(f, W::I32Eq);
    require(f, &refused);
    load16(f, BASE + wire::INITIAL_STATUS as u32);
    set(f, n.sw());
    get(f, n.sw());
    c(f, 11);
    ins(f, W::I32ShrU);
    c(f, 7);
    ins(f, W::I32And);
    set(f, n.top());
    load16(f, BASE + wire::INITIAL_TAGS as u32);
    set(f, n.tags());
    rotate(f, n, n.tags(), 16, false);
    set(f, n.tags());
    constant(f, u32::from(permissions.read_mask), n.i(12));
    for slot in 0..8u8 {
        array_address(f, n, slot, wire::INITIAL_VALUES, 8);
        ins(f, W::F64Load(arg(0, true)));
        set(f, n.value(slot));
        if permissions.read_mask & (1 << slot) != 0 {
            physical(f, n, slot);
            set(f, n.i(u32::from(slot)));
        } else {
            constant(f, u32::from(wire::ORIGIN_ARITHMETIC), n.i(u32::from(slot)));
        }
    }
    constant(f, 1, n.i(20)); // All later side exits flush one accepted prefix.
}

pub(super) fn finish(f: &mut Function, n: Locals) {
    get(f, n.i(20));
    ins(f, W::If(BlockType::Empty));
    store_small(f, BASE + wire::STATUS as u32, n.sw(), false);
    rotate(f, n, n.tags(), 16, true);
    set(f, n.i(21));
    store_small(f, BASE + wire::TAGS as u32, n.i(21), false);
    for (local, offset) in [
        (12, wire::INITIALIZED),
        (13, wire::DIRTY_VALUES),
        (14, wire::POPPED),
    ] {
        rotate(f, n, n.i(local), 8, true);
        set(f, n.i(21));
        store_small(f, BASE + offset as u32, n.i(21), true);
    }
    store_small(f, BASE + wire::POINTER_FLAGS as u32, n.i(15), true);
    for slot in 0..8u8 {
        array_address(f, n, slot, wire::VALUES, 8);
        get(f, n.value(slot));
        ins(f, W::F64Store(arg(0, true)));
        array_address(f, n, slot, wire::ORIGINS, 1);
        get(f, n.i(u32::from(slot)));
        ins(
            f,
            W::I32Store8(MemArg {
                align: 0,
                ..arg(0, false)
            }),
        );
    }
    store(f, BASE + wire::INSTRUCTION_IP as u32, n.i(16), false);
    store_small(f, BASE + wire::INSTRUCTION_OPCODE as u32, n.i(17), false);
    // No accepted x87 instruction means the whole pointer record stays zero.
    get(f, n.i(15));
    c(f, 1);
    ins(f, W::I32And);
    ins(f, W::If(BlockType::Empty));
    load16(f, X87_SELECTORS_OFFSET + 2);
    set(f, n.i(21));
    store_small(f, BASE + wire::INSTRUCTION_CS as u32, n.i(21), false);
    ins(f, W::End);
    store(f, BASE + wire::OPERAND_OFFSET as u32, n.i(18), false);
    store_small(f, BASE + wire::OPERAND_SELECTOR as u32, n.i(19), false);
    ins(f, W::End);
}
fn readable(f: &mut Function, n: Locals, slot: u8, refused: &impl Fn(&mut Function)) {
    get(f, n.i(12));
    c(f, 1 << slot);
    ins(f, W::I32And);
    require(f, refused);
    get(f, n.tags());
    c(f, u32::from(slot) * 2);
    ins(f, W::I32ShrU);
    c(f, 3);
    ins(f, W::I32And);
    c(f, 3);
    ins(f, W::I32Ne);
    require(f, refused);
}
fn empty(f: &mut Function, n: Locals, slot: u8, refused: &impl Fn(&mut Function)) {
    get(f, n.tags());
    c(f, u32::from(slot) * 2);
    ins(f, W::I32ShrU);
    c(f, 3);
    ins(f, W::I32And);
    c(f, 3);
    ins(f, W::I32Eq);
    require(f, refused);
}
fn or_bits(f: &mut Function, local: u32, bits: u32) {
    get(f, local);
    c(f, bits);
    ins(f, W::I32Or);
    set(f, local);
}
fn tag(f: &mut Function, n: Locals, slot: u8, value: u32) {
    get(f, n.tags());
    c(f, !(3 << (slot * 2)));
    ins(f, W::I32And);
    get(f, value);
    ins(f, W::F64Const(0.0.into()));
    ins(f, W::F64Eq);
    c(f, u32::from(slot) * 2);
    ins(f, W::I32Shl);
    ins(f, W::I32Or);
    set(f, n.tags());
}
fn changed_value(f: &mut Function, n: Locals, slot: u8) {
    or_bits(f, n.i(12), 1 << slot);
    or_bits(f, n.i(13), 1 << slot);
    tag(f, n, slot, n.value(slot));
}
fn top_status(f: &mut Function, n: Locals, top: u8, clear: u32) {
    get(f, n.sw());
    c(f, !clear);
    ins(f, W::I32And);
    physical(f, n, top);
    c(f, 11);
    ins(f, W::I32Shl);
    ins(f, W::I32Or);
    set(f, n.sw());
}
fn pop(f: &mut Function, n: Locals, top: u8) {
    or_bits(f, n.tags(), 3 << (top * 2));
    or_bits(f, n.i(14), 1 << top);
    top_status(f, n, (top + 1) & 7, 0x3800);
}
fn compare(f: &mut Function, n: Locals, left: u32, right: u32) {
    // Both operands have passed finite/normal-or-zero admission. Signed zeros
    // compare equal. Clear C0/C1/C2/C3, preserve sticky exceptions and TOP.
    get(f, n.sw());
    c(f, !0x4700);
    ins(f, W::I32And);
    get(f, left);
    get(f, right);
    ins(f, W::F64Lt);
    c(f, 8);
    ins(f, W::I32Shl);
    ins(f, W::I32Or);
    get(f, left);
    get(f, right);
    ins(f, W::F64Eq);
    c(f, 14);
    ins(f, W::I32Shl);
    ins(f, W::I32Or);
    set(f, n.sw());
}
fn real32(f: &mut Function, n: Locals, refused: &impl Fn(&mut Function)) {
    // x87 refuses denormals (including the denormal operand contribution),
    // infinities and NaNs; signed zero and all normal binary32 values are exact.
    get(f, RESULT);
    c(f, 0x7fffffff);
    ins(f, W::I32And);
    set(f, n.i(21));
    get(f, n.i(21));
    ins(f, W::I32Eqz);
    get(f, n.i(21));
    c(f, 23);
    ins(f, W::I32ShrU);
    c(f, 1);
    ins(f, W::I32Sub);
    c(f, 253);
    ins(f, W::I32LeU);
    ins(f, W::I32Or);
    require(f, refused);
    get(f, RESULT);
    ins(f, W::F32ReinterpretI32);
    ins(f, W::F64PromoteF32);
    set(f, n.double(1));
}

pub(super) fn instruction(
    f: &mut Function,
    vregs: &VirtualRegisters,
    n: Locals,
    address: Address,
    instruction: X87Instruction,
    top: u8,
    refused: impl Fn(&mut Function),
) {
    let instruction = match vregs.bind(Operation::X87(instruction)) {
        Operation::X87(i) => i,
        _ => unreachable!(),
    };
    use X87Operation::*;
    let slot = |index: u8| (top + index) & 7;
    let (reads, _, _, _) = x87::effects(instruction, top);
    for i in 0..8 {
        if reads & (1 << i) != 0 {
            readable(f, n, i, &refused);
        }
    }
    let pushing = matches!(
        instruction.operation,
        LoadReal32 { .. } | LoadInteger32 { .. } | Duplicate { .. }
    );
    if pushing {
        empty(f, n, (top + 7) & 7, &refused);
    }
    if let Some(operand) = instruction.operation.address() {
        let write = matches!(instruction.operation, StoreInteger32 { .. });
        memory_emit::procedure_access(
            f,
            Width::Dword,
            operand,
            if write {
                memory_emit::Access::Write
            } else {
                memory_emit::Access::RamRead
            },
            &refused,
        );
        // Capture guest OFFSET, never the backing pointer/linear address.
        copy(f, memory_emit::EA, n.i(22));
        if !write {
            memory_emit::load_ram_value(f, Width::Dword);
        }
    }
    match instruction.operation {
        LoadReal32 { .. } | LoadInteger32 { .. } => {
            let target = (top + 7) & 7;
            let origin = if matches!(instruction.operation, LoadReal32 { .. }) {
                real32(f, n, &refused);
                wire::ORIGIN_REAL32
            } else {
                get(f, RESULT);
                ins(f, W::F64ConvertI32S);
                set(f, n.double(1));
                wire::ORIGIN_INTEGER32
            };
            copy(f, n.double(1), n.value(target));
            constant(f, u32::from(origin), n.i(u32::from(target)));
            changed_value(f, n, target);
            top_status(f, n, target, 0x3a00);
        }
        ArithmeticReal32 { operation, .. } => {
            real32(f, n, &refused);
            numeric::emit_binary(
                f,
                n.numeric(),
                operation,
                n.value(top),
                n.double(1),
                n.cw(),
                n.sw(),
                n.double(2),
                &refused,
            );
            copy(f, n.double(2), n.value(top));
            constant(f, u32::from(wire::ORIGIN_ARITHMETIC), n.i(u32::from(top)));
            changed_value(f, n, top);
        }
        ArithmeticRegister {
            index,
            operation,
            target_top,
            pop: do_pop,
        } => {
            let target = if target_top { top } else { slot(index) };
            let (lhs, rhs) = if target_top {
                (n.value(top), n.value(slot(index)))
            } else {
                (n.value(slot(index)), n.value(top))
            };
            numeric::emit_binary(
                f,
                n.numeric(),
                operation,
                lhs,
                rhs,
                n.cw(),
                n.sw(),
                n.double(2),
                &refused,
            );
            copy(f, n.double(2), n.value(target));
            constant(
                f,
                u32::from(wire::ORIGIN_ARITHMETIC),
                n.i(u32::from(target)),
            );
            changed_value(f, n, target);
            if do_pop {
                pop(f, n, top);
            }
        }
        CompareReal32 { pop: do_pop, .. } => {
            real32(f, n, &refused);
            compare(f, n, n.value(top), n.double(1));
            if do_pop {
                pop(f, n, top);
            }
        }
        CompareRegister { index, pops } => {
            compare(f, n, n.value(top), n.value(slot(index)));
            for step in 0..pops {
                pop(f, n, slot(step));
            }
        }
        StoreStatusAx => {
            copy(f, n.sw(), RESULT);
            write_register(f, Register::Ax, RESULT);
            return; // FNSTSW does not record FIP/FOP or FDP.
        }
        StoreRegister { index, pop: do_pop } => {
            let target = slot(index);
            copy(f, n.value(top), n.value(target));
            copy(f, n.i(u32::from(top)), n.i(u32::from(target)));
            get(f, n.tags());
            c(f, u32::from(top) * 2);
            ins(f, W::I32ShrU);
            c(f, 3);
            ins(f, W::I32And);
            set(f, n.i(21));
            get(f, n.tags());
            c(f, !(3 << (target * 2)));
            ins(f, W::I32And);
            get(f, n.i(21));
            c(f, u32::from(target) * 2);
            ins(f, W::I32Shl);
            ins(f, W::I32Or);
            set(f, n.tags());
            or_bits(f, n.i(12), 1 << target);
            or_bits(f, n.i(13), 1 << target);
            get(f, n.sw());
            c(f, !0x200);
            ins(f, W::I32And);
            set(f, n.sw());
            if do_pop {
                pop(f, n, top);
            }
        }
        Duplicate { index } => {
            let target = (top + 7) & 7;
            copy(f, n.value(slot(index)), n.value(target));
            copy(f, n.i(u32::from(slot(index))), n.i(u32::from(target)));
            changed_value(f, n, target);
            top_status(f, n, target, 0x3a00);
        }
        Exchange { index } => {
            let other = slot(index);
            copy(f, n.value(top), n.double(0));
            copy(f, n.value(other), n.value(top));
            copy(f, n.double(0), n.value(other));
            copy(f, n.i(u32::from(top)), n.i(21));
            copy(f, n.i(u32::from(other)), n.i(u32::from(top)));
            copy(f, n.i(21), n.i(u32::from(other)));
            // Exchange preserves original tags, including diagnostic special tags.
            get(f, n.tags());
            c(f, u32::from(top) * 2);
            ins(f, W::I32ShrU);
            c(f, 3);
            ins(f, W::I32And);
            set(f, n.i(21));
            get(f, n.tags());
            c(f, u32::from(other) * 2);
            ins(f, W::I32ShrU);
            c(f, 3);
            ins(f, W::I32And);
            set(f, n.i(23));
            get(f, n.tags());
            c(f, !((3 << (top * 2)) | (3 << (other * 2))));
            ins(f, W::I32And);
            get(f, n.i(21));
            c(f, u32::from(other) * 2);
            ins(f, W::I32Shl);
            ins(f, W::I32Or);
            get(f, n.i(23));
            c(f, u32::from(top) * 2);
            ins(f, W::I32Shl);
            ins(f, W::I32Or);
            set(f, n.tags());
            or_bits(f, n.i(13), (1 << top) | (1 << other));
            get(f, n.sw());
            c(f, !0x200);
            ins(f, W::I32And);
            set(f, n.sw());
        }
        StoreInteger32 { pop: do_pop, .. } => {
            numeric::emit_integer32(
                f,
                n.numeric(),
                n.value(top),
                n.cw(),
                n.sw(),
                RESULT,
                &refused,
            );
            memory_emit::store_result(f, Width::Dword);
            if do_pop {
                pop(f, n, top);
            }
        }
    }
    constant(f, address.offset, n.i(16));
    constant(f, u32::from(instruction.opcode), n.i(17));
    or_bits(f, n.i(15), 1);
    if let Some(operand) = instruction.operation.address() {
        copy(f, n.i(22), n.i(18));
        load16(f, X87_SELECTORS_OFFSET + (operand.segment as u32) * 2);
        set(f, n.i(19));
        or_bits(f, n.i(15), 2);
    }
}

/// All callers keep scalar code and its refusal depths unchanged.
pub(super) fn dispatch(
    f: &mut Function,
    vregs: &VirtualRegisters,
    n: Option<Locals>,
    address: Address,
    operation: Operation,
    top: &mut u8,
    refused: impl Fn(&mut Function),
) {
    if let Operation::X87(x) = operation {
        instruction(f, vregs, n.expect("x87 plan"), address, x, *top, &refused);
        *top = x87::effects(x, *top).3;
    } else {
        instruction_body(f, vregs, operation, refused);
    }
}
