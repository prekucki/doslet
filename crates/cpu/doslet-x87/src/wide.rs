//! Fixed-size intermediates shared by bounded transcendental kernels.
use crate::{
    ArithmeticError,
    finite::{Finite, Rounded, round},
};

pub(super) fn multiply(lhs: [u64; 4], rhs: [u64; 4]) -> [u64; 8] {
    let mut result = [0_u64; 8];
    for (i, &lhs) in lhs.iter().enumerate() {
        let mut carry = 0_u128;
        for (j, &rhs) in rhs.iter().enumerate() {
            let product = u128::from(lhs) * u128::from(rhs) + u128::from(result[i + j]) + carry;
            result[i + j] = product as u64;
            carry = product >> 64;
        }
        result[i + 4] = carry as u64;
    }
    result
}

pub(super) fn multiply_scalar(value: [u64; 4], scalar: u64) -> [u64; 5] {
    let mut product = [0; 5];
    let mut carry = 0_u128;
    for (i, &word) in value.iter().enumerate() {
        let value = u128::from(word) * u128::from(scalar) + carry;
        product[i] = value as u64;
        carry = value >> 64;
    }
    product[4] = carry as u64;
    product
}

pub(super) fn rounded_product(
    factor: [u64; 4],
    fractional_bits: i32,
    value: Finite,
    negative: bool,
    control: u16,
) -> Result<Rounded, ArithmeticError> {
    let product = multiply_scalar(factor, value.mantissa);
    let highest = (0..320)
        .rev()
        .find(|&bit| product[bit / 64] & (1 << (bit % 64)) != 0)
        .ok_or(ArithmeticError::Unsupported)?;
    // The kernels retain at least 128 significant fixed-point bits even for
    // their smallest nonzero factors; 64 result and 127 guard bits fit here.
    debug_assert!(highest >= 190);
    let bit_at = |bit: usize| (product[bit / 64] >> (bit % 64)) & 1;
    let mut mantissa = 0_u64;
    for offset in 0..64 {
        mantissa = (mantissa << 1) | bit_at(highest - offset);
    }
    let mut remainder = 0_u128;
    for offset in 64..191 {
        remainder = (remainder << 1) | u128::from(bit_at(highest - offset));
    }
    // Both callers use this path only for irrational nonzero results.
    remainder |= 1;
    round(
        mantissa,
        highest as i32 + value.exponent - 63 - fractional_bits,
        negative,
        remainder,
        1_u128 << 127,
        control | 0x300,
    )
}
