//! Bounded integer log2 for FYL2X. Range reduction retains all 64 input bits.
//! 256-bit squaring extracts 192 fractional bits before the final multiply;
//! the extra precision also covers cancellation for arguments close to one.
use crate::{
    ArithmeticError,
    finite::{Finite, Rounded, round},
    wide,
};

/// For positive, nonzero x and finite y. Transcendental results always use
/// extended significand precision; PC affects neither FYL2X nor range reduction.
pub(super) fn multiply_log2(
    x: Finite,
    y: Finite,
    control: u16,
) -> Result<Rounded, ArithmeticError> {
    debug_assert!(!x.negative && x.mantissa != 0);
    let extended = control | 0x300;
    if x.mantissa == 1 << 63 {
        let mut result = Finite::from_i64(i64::from(x.exponent)).multiply(y, extended)?;
        // Native FYL2X reports precision for nontrivial logarithms, including
        // powers of two whose mathematical result is exactly representable.
        result.inexact |= x.exponent != 0 && y.mantissa != 0;
        return Ok(result);
    }
    if y.mantissa == 0 {
        return round(0, 0, y.negative ^ (x.exponent < 0), 0, 1, extended);
    }
    let mut reduced = [0, 0, 0, x.mantissa]; // Q1.255, in [1, 2).
    let mut fraction = [0_u64; 4]; // Q64.192.
    for bit in (0..192).rev() {
        let product = wide::multiply(reduced, reduced);
        let greater_than_two = product[7] >> 63 != 0;
        for i in 0..4 {
            reduced[i] = if greater_than_two {
                product[i + 4]
            } else {
                (product[i + 4] << 1) | (product[i + 3] >> 63)
            };
        }
        if greater_than_two {
            fraction[bit / 64] |= 1 << (bit % 64);
        }
    }
    let negative_log = x.exponent < 0;
    let magnitude = if negative_log {
        let mut magnitude = [0, 0, 0, u64::from(x.exponent.unsigned_abs())];
        let mut borrow = false;
        for i in 0..4 {
            let (value, b1) = magnitude[i].overflowing_sub(fraction[i]);
            let (value, b2) = value.overflowing_sub(u64::from(borrow));
            magnitude[i] = value;
            borrow = b1 || b2;
        }
        magnitude
    } else {
        fraction[3] = x.exponent as u64;
        fraction
    };
    wide::rounded_product(magnitude, 192, y, y.negative ^ negative_log, extended)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wide_square_handles_cross_limb_carries_and_normalization_boundaries() {
        let value = [0, 0, 0, 1 << 63];
        assert_eq!(wide::multiply(value, value), [0, 0, 0, 0, 0, 0, 0, 1 << 62]);
        assert_eq!(
            wide::multiply([u64::MAX; 4], [u64::MAX; 4]),
            [1, 0, 0, 0, u64::MAX - 1, u64::MAX, u64::MAX, u64::MAX]
        );
    }
}
