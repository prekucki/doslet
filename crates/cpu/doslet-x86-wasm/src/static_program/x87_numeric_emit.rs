//! Inline masked-PC24 arithmetic. No calls, loads or architectural effects
//! precede a refused operation. The enclosing procedure owns stack, memory,
//! pointer metadata and instruction accounting.

use super::{Function, W, c, get, ins, set, wide};
use wasm_encoder::BlockType;

/// Scratch ranges are disjoint from inputs, outputs and retained stack locals.
#[derive(Clone, Copy)]
pub(super) struct NumericLocals {
    pub f64_base: u32,
    pub i64_base: u32,
    pub i32_base: u32,
}

impl NumericLocals {
    pub const F64_COUNT: u32 = 12;
    pub const I64_COUNT: u32 = 5;
    pub const I32_COUNT: u32 = 6;
    fn d(self, n: u32) -> u32 {
        self.f64_base + n
    }
    fn q(self, n: u32) -> u32 {
        self.i64_base + n
    }
    fn i(self, n: u32) -> u32 {
        self.i32_base + n
    }
}

fn number(f: &mut Function, value: f64) {
    ins(f, W::F64Const(value.into()));
}

fn copy(f: &mut Function, from: u32, to: u32) {
    get(f, from);
    set(f, to);
}

fn binary(f: &mut Function, left: u32, right: u32, op: W<'static>, out: u32) {
    get(f, left);
    get(f, right);
    ins(f, op);
    set(f, out);
}

/// True for signed zero or normal f64 with unbiased exponent in [-448,448].
fn domain(f: &mut Function, value: u32, scratch: u32) {
    get(f, value);
    ins(f, W::I64ReinterpretF64);
    wide(f, 0x7fff_ffff_ffff_ffff);
    ins(f, W::I64And);
    ins(f, W::LocalTee(scratch));
    ins(f, W::I64Eqz);
    get(f, scratch);
    wide(f, 52);
    ins(f, W::I64ShrU);
    wide(f, 1023 - 448);
    ins(f, W::I64Sub);
    wide(f, 896);
    ins(f, W::I64LeU);
    ins(f, W::I32Or);
}

fn split(f: &mut Function, value: u32, high: u32, low: u32, temp: u32) {
    number(f, 134_217_729.0);
    get(f, value);
    ins(f, W::F64Mul);
    set(f, temp);
    get(f, temp);
    get(f, temp);
    get(f, value);
    ins(f, W::F64Sub);
    ins(f, W::F64Sub);
    set(f, high);
    binary(f, value, high, W::F64Sub, low);
}

/// Inputs may be a,b or q,b; d3..d9 are scratch, d10=product,d11=residual.
fn product(f: &mut Function, n: NumericLocals, a: u32, b: u32) {
    binary(f, a, b, W::F64Mul, n.d(10));
    split(f, a, n.d(3), n.d(4), n.d(7));
    split(f, b, n.d(5), n.d(6), n.d(7));
    get(f, n.d(3));
    get(f, n.d(5));
    ins(f, W::F64Mul);
    get(f, n.d(10));
    ins(f, W::F64Sub);
    get(f, n.d(3));
    get(f, n.d(6));
    ins(f, W::F64Mul);
    ins(f, W::F64Add);
    get(f, n.d(4));
    get(f, n.d(5));
    ins(f, W::F64Mul);
    ins(f, W::F64Add);
    get(f, n.d(4));
    get(f, n.d(6));
    ins(f, W::F64Mul);
    ins(f, W::F64Add);
    set(f, n.d(11));
}

fn sign(f: &mut Function, value: u32, out: u32) {
    get(f, value);
    number(f, 0.0);
    ins(f, W::F64Gt);
    get(f, value);
    number(f, 0.0);
    ins(f, W::F64Lt);
    ins(f, W::I32Sub);
    set(f, out);
}

/// Boolean on stack: RC is toward negative infinity.
fn down(f: &mut Function, control: u32) {
    get(f, control);
    c(f, 0xc00);
    ins(f, W::I32And);
    c(f, 0x400);
    ins(f, W::I32Eq);
}

/// Exact zero for add/sub, including matching negative zeros and cancellation.
fn sum_zero(f: &mut Function, n: NumericLocals, control: u32) {
    // A+b has already produced zero. Two equal signed zeros retain that sign;
    // every other cancellation uses RC to choose its sign.
    get(f, n.d(0));
    ins(f, W::I64ReinterpretF64);
    get(f, n.d(1));
    ins(f, W::I64ReinterpretF64);
    ins(f, W::I64And);
    wide(f, 1 << 63);
    ins(f, W::I64And);
    get(f, n.d(0));
    ins(f, W::I64ReinterpretF64);
    get(f, n.d(1));
    ins(f, W::I64ReinterpretF64);
    ins(f, W::I64Xor);
    wide(f, 1 << 63);
    ins(f, W::I64And);
    down(f, control);
    ins(f, W::I64ExtendI32U);
    wide(f, 63);
    ins(f, W::I64Shl);
    ins(f, W::I64And);
    ins(f, W::I64Or);
    ins(f, W::F64ReinterpretI64);
    set(f, n.d(2));
}

fn residual(f: &mut Function, n: NumericLocals, operation: u8) {
    match operation {
        0 | 4 => {
            // Knuth TwoSum residual, with subtraction normalized to addition.
            binary(f, n.d(2), n.d(0), W::F64Sub, n.d(3));
            get(f, n.d(0));
            get(f, n.d(2));
            get(f, n.d(3));
            ins(f, W::F64Sub);
            ins(f, W::F64Sub);
            get(f, n.d(1));
            get(f, n.d(3));
            ins(f, W::F64Sub);
            ins(f, W::F64Add);
            set(f, n.d(11));
            sign(f, n.d(11), n.i(0));
        }
        1 => {
            product(f, n, n.d(0), n.d(1));
            sign(f, n.d(11), n.i(0));
        }
        6 => {
            product(f, n, n.d(2), n.d(1));
            binary(f, n.d(0), n.d(10), W::F64Sub, n.d(3));
            get(f, n.d(3));
            get(f, n.d(11));
            ins(f, W::F64Gt);
            get(f, n.d(3));
            get(f, n.d(11));
            ins(f, W::F64Lt);
            ins(f, W::I32Sub);
            set(f, n.i(0));
            get(f, n.d(1));
            number(f, 0.0);
            ins(f, W::F64Lt);
            ins(f, W::If(BlockType::Empty));
            c(f, 0);
            get(f, n.i(0));
            ins(f, W::I32Sub);
            set(f, n.i(0));
            ins(f, W::End);
        }
        _ => unreachable!("validated arithmetic opcode"),
    }
}

/// Set inexact/up locals and round s to a PC24 grid point. The error sign is
/// only needed on grid points or midpoints; all other low bits prove inexact.
fn round(f: &mut Function, n: NumericLocals, control: u32) {
    get(f, n.d(2));
    ins(f, W::I64ReinterpretF64);
    ins(f, W::LocalTee(n.q(0)));
    wide(f, 0x7fff_ffff_ffff_ffff);
    ins(f, W::I64And);
    set(f, n.q(1));
    get(f, n.q(0));
    wide(f, 1 << 63);
    ins(f, W::I64And);
    set(f, n.q(4));
    copy(f, n.i(0), n.i(1));
    get(f, n.q(4));
    ins(f, W::I64Eqz);
    ins(f, W::I32Eqz);
    ins(f, W::If(BlockType::Empty));
    c(f, 0);
    get(f, n.i(1));
    ins(f, W::I32Sub);
    set(f, n.i(1));
    ins(f, W::End);
    get(f, n.q(1));
    wide(f, (1 << 29) - 1);
    ins(f, W::I64And);
    set(f, n.q(2));
    get(f, n.q(2));
    ins(f, W::I64Eqz);
    ins(f, W::I32Eqz);
    get(f, n.i(1));
    c(f, 0);
    ins(f, W::I32Ne);
    ins(f, W::I32Or);
    set(f, n.i(2));
    get(f, control);
    c(f, 10);
    ins(f, W::I32ShrU);
    c(f, 3);
    ins(f, W::I32And);
    set(f, n.i(4));
    get(f, n.i(4));
    ins(f, W::I32Eqz);
    ins(f, W::If(BlockType::Result(wasm_encoder::ValType::I32)));
    get(f, n.q(2));
    ins(f, W::I64Eqz);
    ins(f, W::If(BlockType::Result(wasm_encoder::ValType::I32)));
    get(f, n.i(1));
    c(f, 0);
    ins(f, W::I32LtS);
    ins(f, W::Else);
    get(f, n.q(2));
    wide(f, 1 << 28);
    ins(f, W::I64GtU);
    get(f, n.q(2));
    wide(f, 1 << 28);
    ins(f, W::I64Eq);
    get(f, n.i(1));
    c(f, 0);
    ins(f, W::I32GtS);
    get(f, n.i(1));
    ins(f, W::I32Eqz);
    get(f, n.q(1));
    wide(f, 1 << 29);
    ins(f, W::I64And);
    ins(f, W::I64Eqz);
    ins(f, W::I32Eqz);
    ins(f, W::I32And);
    ins(f, W::I32Or);
    ins(f, W::I32And);
    ins(f, W::I32Or);
    ins(f, W::End);
    ins(f, W::Else);
    get(f, n.i(4));
    c(f, 1);
    ins(f, W::I32Eq);
    get(f, n.q(4));
    ins(f, W::I64Eqz);
    ins(f, W::I32Eqz);
    ins(f, W::I32And);
    get(f, n.i(4));
    c(f, 2);
    ins(f, W::I32Eq);
    get(f, n.q(4));
    ins(f, W::I64Eqz);
    ins(f, W::I32And);
    ins(f, W::I32Or);
    ins(f, W::End);
    get(f, n.i(2));
    ins(f, W::I32And);
    set(f, n.i(3));
    get(f, n.q(1));
    wide(f, !((1 << 29) - 1));
    ins(f, W::I64And);
    get(f, n.q(2));
    ins(f, W::I64Eqz);
    get(f, n.i(1));
    c(f, 0);
    ins(f, W::I32LtS);
    ins(f, W::I32And);
    ins(f, W::I64ExtendI32U);
    wide(f, 29);
    ins(f, W::I64Shl);
    ins(f, W::I64Sub);
    get(f, n.i(3));
    ins(f, W::I64ExtendI32U);
    wide(f, 29);
    ins(f, W::I64Shl);
    ins(f, W::I64Add);
    get(f, n.q(4));
    ins(f, W::I64Or);
    ins(f, W::F64ReinterpretI64);
    set(f, n.d(2));
}

fn status(f: &mut Function, n: NumericLocals, status: u32) {
    get(f, status);
    c(f, !0x200);
    ins(f, W::I32And);
    get(f, n.i(2));
    c(f, 5);
    ins(f, W::I32Shl);
    ins(f, W::I32Or);
    get(f, n.i(3));
    c(f, 9);
    ins(f, W::I32Shl);
    ins(f, W::I32Or);
    set(f, status);
}

/// Emit one operation with the same exact domain/result as x87::pc24_binary.
/// `control` is a previously admitted masked-PC24 CW. A refusing closure must
/// leave the generated function; only scratch locals have changed at refusal.
#[expect(clippy::too_many_arguments)]
pub(super) fn emit_binary(
    f: &mut Function,
    n: NumericLocals,
    operation: u8,
    lhs: u32,
    rhs: u32,
    control: u32,
    status_local: u32,
    output: u32,
    refused: impl Fn(&mut Function),
) {
    assert!(matches!(operation, 0 | 1 | 4 | 5 | 6 | 7));
    domain(f, lhs, n.q(0));
    domain(f, rhs, n.q(0));
    ins(f, W::I32And);
    ins(f, W::I32Eqz);
    ins(f, W::If(BlockType::Empty));
    refused(f);
    ins(f, W::End);
    let (lhs, rhs, operation) = match operation {
        5 => (rhs, lhs, 4),
        7 => (rhs, lhs, 6),
        _ => (lhs, rhs, operation),
    };
    copy(f, lhs, n.d(0));
    get(f, rhs);
    if operation == 4 {
        ins(f, W::F64Neg);
    }
    set(f, n.d(1));
    if operation == 6 {
        get(f, n.d(1));
        number(f, 0.0);
        ins(f, W::F64Eq);
        ins(f, W::If(BlockType::Empty));
        refused(f);
        ins(f, W::End);
    }
    let op = match operation {
        0 | 4 => W::F64Add,
        1 => W::F64Mul,
        6 => W::F64Div,
        _ => unreachable!(),
    };
    binary(f, n.d(0), n.d(1), op, n.d(2));
    c(f, 0);
    set(f, n.i(0));
    c(f, 0);
    set(f, n.i(2));
    c(f, 0);
    set(f, n.i(3));
    get(f, n.d(2));
    number(f, 0.0);
    ins(f, W::F64Eq);
    ins(f, W::If(BlockType::Empty));
    if matches!(operation, 0 | 4) {
        sum_zero(f, n, control);
    }
    ins(f, W::Else);
    get(f, n.d(2));
    ins(f, W::I64ReinterpretF64);
    wide(f, (1 << 29) - 1);
    ins(f, W::I64And);
    ins(f, W::LocalTee(n.q(0)));
    ins(f, W::I64Eqz);
    get(f, n.q(0));
    wide(f, 1 << 28);
    ins(f, W::I64Eq);
    ins(f, W::I32Or);
    ins(f, W::If(BlockType::Empty));
    residual(f, n, operation);
    ins(f, W::End);
    round(f, n, control);
    ins(f, W::End);
    status(f, n, status_local);
    copy(f, n.d(2), output);
}

/// Integer store conversion without a pop or a memory effect. The enclosing
/// instruction proves its destination first, then stores and pops on success.
pub(super) fn emit_integer32(
    f: &mut Function,
    n: NumericLocals,
    value: u32,
    control: u32,
    status_local: u32,
    output: u32,
    refused: impl Fn(&mut Function),
) {
    get(f, control);
    c(f, 10);
    ins(f, W::I32ShrU);
    c(f, 3);
    ins(f, W::I32And);
    set(f, n.i(4));
    get(f, n.i(4));
    ins(f, W::I32Eqz);
    ins(f, W::If(BlockType::Result(wasm_encoder::ValType::F64)));
    get(f, value);
    ins(f, W::F64Nearest);
    ins(f, W::Else);
    get(f, n.i(4));
    c(f, 1);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Result(wasm_encoder::ValType::F64)));
    get(f, value);
    ins(f, W::F64Floor);
    ins(f, W::Else);
    get(f, n.i(4));
    c(f, 2);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Result(wasm_encoder::ValType::F64)));
    get(f, value);
    ins(f, W::F64Ceil);
    ins(f, W::Else);
    get(f, value);
    ins(f, W::F64Trunc);
    ins(f, W::End);
    ins(f, W::End);
    ins(f, W::End);
    set(f, n.d(2));
    get(f, n.d(2));
    number(f, -2_147_483_648.0);
    ins(f, W::F64Ge);
    get(f, n.d(2));
    number(f, 2_147_483_647.0);
    ins(f, W::F64Le);
    ins(f, W::I32And);
    ins(f, W::I32Eqz);
    ins(f, W::If(BlockType::Empty));
    refused(f);
    ins(f, W::End);
    get(f, value);
    get(f, n.d(2));
    ins(f, W::F64Ne);
    set(f, n.i(2));
    get(f, n.d(2));
    ins(f, W::F64Abs);
    get(f, value);
    ins(f, W::F64Abs);
    ins(f, W::F64Gt);
    set(f, n.i(3));
    status(f, n, status_local);
    get(f, n.d(2));
    ins(f, W::I32TruncF64S);
    set(f, output);
}

#[cfg(test)]
#[path = "x87_numeric_tests.rs"]
mod tests;
