pub const CARRY: u32 = 1;
pub const PARITY: u32 = 1 << 2;
pub const AUXILIARY: u32 = 1 << 4;
pub const ZERO: u32 = 1 << 6;
pub const SIGN: u32 = 1 << 7;
pub const TRAP: u32 = 1 << 8;
pub const INTERRUPT: u32 = 1 << 9;
pub const DIRECTION: u32 = 1 << 10;
pub const OVERFLOW: u32 = 1 << 11;
pub const RESUME: u32 = 1 << 16;
pub const VIRTUAL: u32 = 1 << 17;
pub const ALIGNMENT: u32 = 1 << 18;
pub const ID: u32 = 1 << 21;
/// Writable real-mode i386 POPF(D) flags; AC/ID/RF/VM and reserved bits are excluded.
pub const POPF_386: u32 = 0x7FD5;
const ARITHMETIC: u32 = CARRY | PARITY | AUXILIARY | ZERO | SIGN | OVERFLOW;

pub(crate) fn result_flags(previous: u32, value: u16) -> u32 {
    let mut flags = (previous & !ARITHMETIC) | 2;
    if value == 0 {
        flags |= ZERO;
    }
    if value & 0x8000 != 0 {
        flags |= SIGN;
    }
    if (value as u8).count_ones().is_multiple_of(2) {
        flags |= PARITY;
    }
    flags
}

pub(crate) fn add(previous: u32, left: u16, right: u16) -> (u16, u32) {
    let (value, carry) = left.overflowing_add(right);
    let mut flags = result_flags(previous, value);
    if carry {
        flags |= CARRY;
    }
    if (left ^ right ^ value) & 0x10 != 0 {
        flags |= AUXILIARY;
    }
    if (!(left ^ right) & (left ^ value)) & 0x8000 != 0 {
        flags |= OVERFLOW;
    }
    (value, flags)
}

pub(crate) fn sub(previous: u32, left: u16, right: u16) -> (u16, u32) {
    let (value, borrow) = left.overflowing_sub(right);
    let mut flags = result_flags(previous, value);
    if borrow {
        flags |= CARRY;
    }
    if (left ^ right ^ value) & 0x10 != 0 {
        flags |= AUXILIARY;
    }
    if ((left ^ right) & (left ^ value)) & 0x8000 != 0 {
        flags |= OVERFLOW;
    }
    (value, flags)
}

/// x86 ALU group encoding: ADD, OR, ADC, SBB, AND, SUB, XOR, CMP.
pub(crate) fn alu(previous: u32, left: u16, right: u16, operation: u8, word: bool) -> (u16, u32) {
    let mask = if word { 0xFFFF } else { 0xFF };
    let sign = if word { 0x8000 } else { 0x80 };
    let left = left & mask;
    let right = right & mask;
    let carry = u32::from(previous & CARRY != 0 && matches!(operation, 2 | 3));
    let subtract = matches!(operation, 3 | 5 | 7);
    let logical = matches!(operation, 1 | 4 | 6);
    let wide = match operation {
        1 => u32::from(left | right),
        4 => u32::from(left & right),
        6 => u32::from(left ^ right),
        _ if subtract => u32::from(left).wrapping_sub(u32::from(right) + carry),
        _ => u32::from(left) + u32::from(right) + carry,
    };
    let value = wide as u16 & mask;
    let mut result = result_flags(previous, value);
    result &= !SIGN;
    if value & sign != 0 {
        result |= SIGN;
    }
    if !logical {
        if (subtract && u32::from(left) < u32::from(right) + carry)
            || (!subtract && wide > u32::from(mask))
        {
            result |= CARRY;
        }
        if (left ^ right ^ value) & 0x10 != 0 {
            result |= AUXILIARY;
        }
        let overflow = if subtract {
            left ^ right
        } else {
            !(left ^ right)
        };
        if overflow & (left ^ value) & sign != 0 {
            result |= OVERFLOW;
        }
    }
    (value, result)
}

pub(crate) fn condition(flags: u32, code: u8) -> bool {
    let carry = flags & CARRY != 0;
    let zero = flags & ZERO != 0;
    let signed = (flags & SIGN != 0) != (flags & OVERFLOW != 0);
    let positive = match code >> 1 {
        0 => flags & OVERFLOW != 0,
        1 => carry,
        2 => zero,
        3 => carry || zero,
        4 => flags & SIGN != 0,
        5 => flags & PARITY != 0,
        6 => signed,
        _ => signed || zero,
    };
    positive != (code & 1 != 0)
}

pub(crate) fn alu32(previous: u32, left: u32, right: u32, operation: u8) -> (u32, u32) {
    let carry = u64::from(previous & CARRY != 0 && matches!(operation, 2 | 3));
    let subtract = matches!(operation, 3 | 5 | 7);
    let logical = matches!(operation, 1 | 4 | 6);
    let wide = match operation {
        1 => u64::from(left | right),
        4 => u64::from(left & right),
        6 => u64::from(left ^ right),
        _ if subtract => u64::from(left).wrapping_sub(u64::from(right) + carry),
        _ => u64::from(left) + u64::from(right) + carry,
    };
    let value = wide as u32;
    let mut result = result_flags(previous, value as u16) & !(ZERO | SIGN);
    if value == 0 {
        result |= ZERO;
    }
    if value & 0x80000000 != 0 {
        result |= SIGN;
    }
    if !logical {
        if (subtract && u64::from(left) < u64::from(right) + carry)
            || (!subtract && wide > u64::from(u32::MAX))
        {
            result |= CARRY;
        }
        if (left ^ right ^ value) & 0x10 != 0 {
            result |= AUXILIARY;
        }
        let overflow = if subtract {
            left ^ right
        } else {
            !(left ^ right)
        };
        if overflow & (left ^ value) & 0x80000000 != 0 {
            result |= OVERFLOW;
        }
    }
    (value, result)
}
