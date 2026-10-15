//! F2XM1 on [-1, 1] with Q2.254 integer series evaluation. Factoring out x
//! avoids catastrophic cancellation and loss of tiny input values.
use crate::{
    ArithmeticError,
    finite::{Finite, Rounded},
    wide,
};
const ONE: [u64; 4] = [0, 0, 0, 1 << 62];
// floor(ln(2) * 2^254), independent of the guest-rounded FLDLN2 constant.
const LN_2: [u64; 4] = [
    0x6283_45D6_E2EA_BE8A,
    0xD03C_D0C9_9CA6_2D8B,
    0xF278_ECE6_00FC_BDAB,
    0x2C5C_85FD_F473_DE6A,
];
fn multiply(lhs: [u64; 4], rhs: [u64; 4]) -> [u64; 4] {
    let product = wide::multiply(lhs, rhs);
    core::array::from_fn(|i| (product[i + 3] >> 62) | (product[i + 4] << 2))
}
fn divide_small(value: &mut [u64; 4], divisor: u64) {
    let mut remainder = 0_u128;
    for word in value.iter_mut().rev() {
        let numerator = (remainder << 64) | u128::from(*word);
        *word = (numerator / u128::from(divisor)) as u64;
        remainder = numerator % u128::from(divisor);
    }
}
pub(super) fn exp2_minus_one(x: Finite, control: u16) -> Result<Rounded, ArithmeticError> {
    if x.mantissa == 0 {
        return Ok(Rounded {
            value: x,
            inexact: false,
            up: false,
            exceptions: 0,
        });
    }
    let mut absolute = x;
    absolute.negative = false;
    match absolute.compare(Finite::from_i64(1)) {
        core::cmp::Ordering::Greater => return Err(ArithmeticError::Unsupported),
        core::cmp::Ordering::Equal => {
            return Ok(Rounded {
                value: Finite::from_f64(if x.negative { -0.5 } else { 1.0 }).unwrap(),
                inexact: true,
                up: false,
                exceptions: 0,
            });
        }
        _ => {}
    }
    let product = wide::multiply_scalar(LN_2, x.mantissa);
    let shift = (63 - x.exponent) as usize;
    let t = core::array::from_fn(|i| {
        let word = i + shift / 64;
        let bits = shift % 64;
        let low = product.get(word).copied().unwrap_or(0) >> bits;
        let high = if bits == 0 {
            0
        } else {
            product.get(word + 1).copied().unwrap_or(0) << (64 - bits)
        };
        low | high
    });
    // (exp(t)-1)/t = 1 + t/2! + t^2/3! + ...; |t| <= ln(2).
    let mut sum = ONE;
    let mut term = ONE;
    for n in 1..=96 {
        term = multiply(term, t);
        divide_small(&mut term, n + 1);
        if term == [0; 4] {
            break;
        }
        if x.negative && n & 1 != 0 {
            let mut borrow = false;
            for i in 0..4 {
                let (value, b1) = sum[i].overflowing_sub(term[i]);
                let (value, b2) = value.overflowing_sub(u64::from(borrow));
                sum[i] = value;
                borrow = b1 || b2;
            }
        } else {
            let mut carry = 0_u128;
            for i in 0..4 {
                let value = u128::from(sum[i]) + u128::from(term[i]) + carry;
                sum[i] = value as u64;
                carry = value >> 64;
            }
        }
    }
    wide::rounded_product(multiply(LN_2, sum), 254, x, x.negative, control)
}
