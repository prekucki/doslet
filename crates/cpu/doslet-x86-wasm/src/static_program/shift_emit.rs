//! Register shifts use bounded integer expressions, including 33-bit carry
//! rings. The five-bit x86 count is kept separate from the WASM shift mask.

use super::*;
use crate::static_program::{DoubleShift, Shift, ShiftCount};

fn count(f: &mut Function, count: ShiftCount) {
    match count {
        ShiftCount::One => c(f, 1),
        ShiftCount::Immediate(n) => c(f, u32::from(n & 31)),
        ShiftCount::Cl => {
            read_register(f, Register::Cl);
            c(f, 31);
            ins(f, W::I32And);
        }
    }
    set(f, RIGHT);
}

fn masked_result(f: &mut Function, width: Width) {
    if width != Width::Dword {
        c(f, width.mask());
        ins(f, W::I32And);
    }
    set(f, RESULT);
}

fn result_flags(f: &mut Function, width: Width, clear_auxiliary: bool) {
    get(f, FLAGS);
    c(f, !(0xc5 | if clear_auxiliary { 0x10 } else { 0 }));
    ins(f, W::I32And);
    if clear_auxiliary {
        // The interpreter computes logical-result flags for ordinary shifts.
        c(f, 2);
        ins(f, W::I32Or);
    }
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
    get(f, CARRY);
    ins(f, W::I32Or);
    set(f, FLAGS);
}

// Leave the old flags (except OF) below a zero/one overflow expression.
fn overflow_start(f: &mut Function) {
    get(f, RIGHT);
    c(f, 1);
    ins(f, W::I32Eq);
    ins(f, W::If(BlockType::Empty));
    get(f, FLAGS);
    c(f, !0x800);
    ins(f, W::I32And);
}

fn overflow_end(f: &mut Function) {
    c(f, 11);
    ins(f, W::I32Shl);
    ins(f, W::I32Or);
    set(f, FLAGS);
    ins(f, W::End);
}

fn signed_left(f: &mut Function, width: Width) {
    get(f, LEFT);
    if width != Width::Dword {
        c(f, 32 - width.bits());
        ins(f, W::I32Shl);
        c(f, 32 - width.bits());
        ins(f, W::I32ShrS);
    }
}

fn simple_shift(f: &mut Function, kind: Shift, width: Width) {
    match kind {
        Shift::Shl => {
            // Widen before shifting: byte/word counts beyond the width must
            // yield zero, not wrap through WASM's five-bit shift mask.
            get(f, LEFT);
            ins(f, W::I64ExtendI32U);
            get(f, RIGHT);
            ins(f, W::I64ExtendI32U);
            ins(f, W::I64Shl);
            ins(f, W::LocalTee(WIDE));
            ins(f, W::I32WrapI64);
            masked_result(f, width);
            get(f, WIDE);
            wide(f, u64::from(width.bits()));
            ins(f, W::I64ShrU);
            ins(f, W::I32WrapI64);
        }
        Shift::Shr | Shift::Sar => {
            if kind == Shift::Sar {
                signed_left(f, width);
            } else {
                get(f, LEFT);
            }
            get(f, RIGHT);
            ins(
                f,
                if kind == Shift::Sar {
                    W::I32ShrS
                } else {
                    W::I32ShrU
                },
            );
            masked_result(f, width);
            if kind == Shift::Sar {
                signed_left(f, width);
            } else {
                get(f, LEFT);
            }
            get(f, RIGHT);
            c(f, 1);
            ins(f, W::I32Sub);
            ins(
                f,
                if kind == Shift::Sar {
                    W::I32ShrS
                } else {
                    W::I32ShrU
                },
            );
        }
        _ => unreachable!("simple shift"),
    }
    c(f, 1);
    ins(f, W::I32And);
    set(f, CARRY);
    result_flags(f, width, true);
}

fn rotate(f: &mut Function, kind: Shift, width: Width) {
    let through_carry = matches!(kind, Shift::Rcl | Shift::Rcr);
    let left = matches!(kind, Shift::Rol | Shift::Rcl);
    let ring_bits = width.bits() + u32::from(through_carry);
    get(f, LEFT);
    ins(f, W::I64ExtendI32U);
    if through_carry {
        get(f, FLAGS);
        c(f, 1);
        ins(f, W::I32And);
        ins(f, W::I64ExtendI32U);
        wide(f, u64::from(width.bits()));
        ins(f, W::I64Shl);
        ins(f, W::I64Or);
    }
    set(f, WIDE);
    // Preserve the original masked count for OF; a full byte/word ring still
    // updates CF in the reference CPU even though the destination is unchanged.
    get(f, RIGHT);
    c(f, ring_bits);
    ins(f, W::I32RemU);
    set(f, RESULT);
    get(f, WIDE);
    get(f, RESULT);
    ins(f, W::I64ExtendI32U);
    ins(f, if left { W::I64Shl } else { W::I64ShrU });
    get(f, WIDE);
    c(f, ring_bits);
    get(f, RESULT);
    ins(f, W::I32Sub);
    ins(f, W::I64ExtendI32U);
    ins(f, if left { W::I64ShrU } else { W::I64Shl });
    ins(f, W::I64Or);
    wide(f, (1u64 << ring_bits) - 1);
    ins(f, W::I64And);
    ins(f, W::LocalTee(WIDE));
    ins(f, W::I32WrapI64);
    masked_result(f, width);
    get(f, WIDE);
    wide(
        f,
        u64::from(if through_carry {
            width.bits()
        } else if left {
            0
        } else {
            width.bits() - 1
        }),
    );
    ins(f, W::I64ShrU);
    ins(f, W::I32WrapI64);
    c(f, 1);
    ins(f, W::I32And);
    set(f, CARRY);
    get(f, FLAGS);
    c(f, !1);
    ins(f, W::I32And);
    get(f, CARRY);
    ins(f, W::I32Or);
    set(f, FLAGS);
}

pub(super) fn shift(f: &mut Function, kind: Shift, target: Register, n: ShiftCount) {
    let width = target.width();
    read_register(f, target);
    set(f, LEFT);
    count(f, n);
    get(f, RIGHT);
    ins(f, W::If(BlockType::Empty));
    if matches!(kind, Shift::Shl | Shift::Shr | Shift::Sar) {
        simple_shift(f, kind, width);
    } else {
        rotate(f, kind, width);
    }
    overflow_start(f);
    match kind {
        Shift::Shl | Shift::Rol | Shift::Rcl => {
            get(f, RESULT);
            c(f, width.bits() - 1);
            ins(f, W::I32ShrU);
            get(f, CARRY);
            ins(f, W::I32Xor);
        }
        Shift::Ror | Shift::Rcr => {
            get(f, RESULT);
            c(f, width.bits() - 1);
            ins(f, W::I32ShrU);
            get(f, RESULT);
            c(f, width.bits() - 2);
            ins(f, W::I32ShrU);
            ins(f, W::I32Xor);
            c(f, 1);
            ins(f, W::I32And);
        }
        Shift::Shr => {
            get(f, LEFT);
            c(f, width.bits() - 1);
            ins(f, W::I32ShrU);
        }
        Shift::Sar => c(f, 0),
    }
    overflow_end(f);
    write_register(f, target, RESULT);
    ins(f, W::End);
}

pub(super) fn double_shift(
    f: &mut Function,
    kind: DoubleShift,
    target: Register,
    source: Register,
    n: ShiftCount,
) {
    let width = target.width();
    read_register(f, target);
    set(f, LEFT);
    read_register(f, source);
    set(f, RESULT);
    count(f, n);
    get(f, RIGHT);
    ins(f, W::If(BlockType::Empty));
    let left = kind == DoubleShift::Left;
    get(f, if left { LEFT } else { RESULT });
    ins(f, W::I64ExtendI32U);
    wide(f, u64::from(width.bits()));
    ins(f, W::I64Shl);
    get(f, if left { RESULT } else { LEFT });
    ins(f, W::I64ExtendI32U);
    ins(f, W::I64Or);
    get(f, RIGHT);
    ins(f, W::I64ExtendI32U);
    ins(f, if left { W::I64Shl } else { W::I64ShrU });
    if left {
        wide(f, u64::from(width.bits()));
        ins(f, W::I64ShrU);
    }
    ins(f, W::I32WrapI64);
    masked_result(f, width);
    get(f, LEFT);
    if left {
        c(f, width.bits());
        get(f, RIGHT);
    } else {
        get(f, RIGHT);
        c(f, 1);
    }
    ins(f, W::I32Sub);
    ins(f, W::I32ShrU);
    c(f, 1);
    ins(f, W::I32And);
    set(f, CARRY);
    result_flags(f, width, false);
    overflow_start(f);
    get(f, LEFT);
    get(f, RESULT);
    ins(f, W::I32Xor);
    c(f, width.bits() - 1);
    ins(f, W::I32ShrU);
    overflow_end(f);
    write_register(f, target, RESULT);
    ins(f, W::End);
}
