//! Portable bounded fixed-point range reduction and series. All intermediate
//! arrays are stack owned. Transcendental results ignore PC and honor RC.
use crate::{
    ArithmeticError, X87,
    finite::{Finite, Rounded, round},
    wide,
};
type Fixed = [u64; 4]; // Q64.192
const ONE: Fixed = [0, 0, 0, 1];
// floor((pi/2) * 2^192), generated using the Chudnovsky series.
const PI_2: Fixed = [
    0x5204_9C11_14CF_98E8,
    0x898C_C517_01B8_39A2,
    0x921F_B544_42D1_8469,
    1,
];
const PI_4: Fixed = [
    0x2902_4E08_8A67_CC74,
    0xC4C6_628B_80DC_1CD1,
    0xC90F_DAA2_2168_C234,
    0,
];
const LOG2_E: Fixed = [
    0xD6AE_F551_BAD2_B4B1,
    0x7D0F_FDA0_D23A_7D11,
    0x7154_7652_B82F_E177,
    1,
];
fn compare(a: Fixed, b: Fixed) -> core::cmp::Ordering {
    a.iter().rev().cmp(b.iter().rev())
}
fn add(a: Fixed, b: Fixed) -> Fixed {
    let mut carry = 0_u128;
    core::array::from_fn(|i| {
        let sum = u128::from(a[i]) + u128::from(b[i]) + carry;
        carry = sum >> 64;
        sum as u64
    })
}

fn subtract(a: Fixed, b: Fixed) -> Fixed {
    let mut borrow = false;
    core::array::from_fn(|i| {
        let (v, b1) = a[i].overflowing_sub(b[i]);
        let (v, b2) = v.overflowing_sub(u64::from(borrow));
        borrow = b1 || b2;
        v
    })
}
fn multiply(a: Fixed, b: Fixed) -> Fixed {
    let p = wide::multiply(a, b);
    [p[3], p[4], p[5], p[6]]
}
fn divide_small(mut a: Fixed, b: u64) -> Fixed {
    let mut remainder = 0_u128;
    for limb in a.iter_mut().rev() {
        let v = (remainder << 64) | u128::from(*limb);
        *limb = (v / u128::from(b)) as u64;
        remainder = v % u128::from(b);
    }
    a
}
fn shift_left(a: Fixed) -> Fixed {
    core::array::from_fn(|i| (a[i] << 1) | if i == 0 { 0 } else { a[i - 1] >> 63 })
}
fn from_finite(a: Finite) -> Fixed {
    let shift = 192 + a.exponent - 63;
    let mut result = [0; 4];
    for bit in 0..64 {
        let destination = bit + shift;
        if (0..256).contains(&destination) && a.mantissa & (1_u64 << bit) != 0 {
            result[destination as usize / 64] |= 1_u64 << (destination % 64);
        }
    }
    result
}
fn modulo(numerator: Fixed, denominator: Fixed) -> (u64, Fixed) {
    let mut remainder = [0; 4];
    let mut quotient = 0_u64;
    for bit in (0..256).rev() {
        remainder = shift_left(remainder);
        remainder[0] |= (numerator[bit / 64] >> (bit % 64)) & 1;
        if compare(remainder, denominator).is_ge() {
            remainder = subtract(remainder, denominator);
            if bit < 64 {
                quotient |= 1 << bit;
            }
        }
    }
    (quotient, remainder)
}
// The callers divide positive magnitudes no larger than the denominator.
fn ratio(numerator: Fixed, denominator: Fixed) -> Fixed {
    let mut remainder = numerator;
    let mut result = [0; 4];
    if compare(remainder, denominator).is_ge() {
        remainder = subtract(remainder, denominator);
        result[3] = 1;
    }
    for bit in (0..192).rev() {
        remainder = shift_left(remainder);
        if compare(remainder, denominator).is_ge() {
            remainder = subtract(remainder, denominator);
            result[bit / 64] |= 1 << (bit % 64);
        }
    }
    result
}
fn output(a: Fixed, negative: bool, control: u16) -> Result<Rounded, ArithmeticError> {
    wide::rounded_product(a, 192, Finite::from_i64(1), negative, control)
}
fn sin_cos(x: Finite, control: u16) -> Result<(Rounded, Rounded), ArithmeticError> {
    if x.mantissa == 0 {
        return Ok((
            round(0, 0, x.negative, 0, 1, control | 0x300)?,
            round(1 << 63, 0, false, 0, 1, control | 0x300)?,
        ));
    }
    let small = x.exponent < -1;
    let (quadrant, mut r) = if small {
        (0, from_finite(x))
    } else {
        modulo(from_finite(x), PI_2)
    };
    let r_negative = compare(r, PI_4).is_gt();
    let quadrant = if r_negative {
        r = subtract(PI_2, r);
        quadrant + 1
    } else {
        quadrant
    };
    let square = multiply(r, r);
    let mut sinc = ONE;
    let mut cosine = ONE;
    let mut sin_term = ONE;
    let mut cos_term = ONE;
    for n in 1..=64_u64 {
        sin_term = divide_small(multiply(sin_term, square), (2 * n) * (2 * n + 1));
        cos_term = divide_small(multiply(cos_term, square), (2 * n - 1) * (2 * n));
        if sin_term == [0; 4] && cos_term == [0; 4] {
            break;
        }
        if n & 1 != 0 {
            sinc = subtract(sinc, sin_term);
            cosine = subtract(cosine, cos_term);
        } else {
            sinc = add(sinc, sin_term);
            cosine = add(cosine, cos_term);
        }
    }
    if small {
        // A correction below the fixed-point grid is still negative. Retain
        // its direction for directed rounding of tiny nonzero arguments.
        if sinc == ONE {
            sinc = subtract(ONE, [1, 0, 0, 0]);
        }
        if cosine == ONE {
            cosine = subtract(ONE, [1, 0, 0, 0]);
        }
        return Ok((
            wide::rounded_product(sinc, 192, x, x.negative, control)?,
            output(cosine, false, control)?,
        ));
    }
    let sine = multiply(r, sinc);
    let (sin, cos, sin_negative, cos_negative) = match quadrant & 3 {
        0 => (sine, cosine, r_negative, false),
        1 => (cosine, sine, false, !r_negative),
        2 => (sine, cosine, !r_negative, true),
        _ => (cosine, sine, true, r_negative),
    };
    Ok((
        output(sin, sin_negative ^ x.negative, control)?,
        output(cos, cos_negative, control)?,
    ))
}
fn arctangent(t: Fixed) -> Fixed {
    // atan(t) = pi/4 - atan((1-t)/(1+t)) for t > tan(pi/8).
    // Using the simpler cutoff 1/2 bounds the series argument by 1/2.
    let transformed = compare(t, [0, 0, 1 << 63, 0]).is_gt();
    let t = if transformed {
        ratio(subtract(ONE, t), add(ONE, t))
    } else {
        t
    };
    let square = multiply(t, t);
    let mut power = t;
    let mut sum = t;
    for n in 1..=192_u64 {
        power = multiply(power, square);
        let term = divide_small(power, 2 * n + 1);
        if term == [0; 4] {
            break;
        }
        if n & 1 != 0 {
            sum = subtract(sum, term);
        } else {
            sum = add(sum, term);
        }
    }
    if transformed {
        subtract(PI_4, sum)
    } else {
        sum
    }
}
impl X87 {
    /// kind: 0=FSIN, 1=FCOS, 2=FSINCOS, 3=FPTAN. Out-of-range finite
    /// inputs leave ST(0)/TOP untouched and set C2 as required by the ISA.
    pub fn trigonometric(&mut self, kind: u8) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        if self.finite[top].is_some_and(|x| x.mantissa != 0 && x.exponent >= 63) {
            self.status |= 0x400;
            return Ok(());
        }
        if kind >= 2 && (self.tags >> (((top + 7) & 7) * 2)) & 3 != 3 {
            return Err(ArithmeticError::Stack(crate::StackError::Overflow));
        }
        let Some(x) = self.finite[top] else {
            if self.values[top].is_infinite() {
                self.store_special(top, f64::from_bits(0xFFF8_0000_0000_0000), 1, false)?;
            } else {
                self.binary(
                    top,
                    self.number(top),
                    crate::binary::Number::from_finite(Finite::from_i64(0)),
                    0,
                    false,
                )?;
            }
            self.status &= !0x400;
            if kind >= 2 {
                self.duplicate(0).map_err(ArithmeticError::Stack)?;
            }
            return Ok(());
        };
        self.check_denormals(&[top])?;
        let (sine, cosine) = sin_cos(x, self.control)?;
        self.status &= !0x400;
        match kind {
            0 => self.store_rounded(top, sine, false),
            1 => self.store_rounded(top, cosine, false),
            2 => {
                let pending = self.store_rounded(top, sine, false);
                if let Err(error) = pending
                    && error != ArithmeticError::UnmaskedException(0x20)
                {
                    return Err(error);
                }
                self.push(0.).map_err(ArithmeticError::Stack)?;
                self.store_rounded((top + 7) & 7, cosine, false)?;
                pending
            }
            3 => {
                let mut tangent = sine.value.divide(cosine.value, self.control | 0x300)?;
                tangent.inexact |= x.mantissa != 0;
                let pending = self.store_rounded(top, tangent, false);
                if let Err(error) = pending
                    && error != ArithmeticError::UnmaskedException(0x20)
                {
                    return Err(error);
                }
                self.push_integer(1).map_err(ArithmeticError::Stack)?;
                pending
            }
            _ => Err(ArithmeticError::Unsupported),
        }
    }
    pub fn arctangent_pop(&mut self) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        self.st(1).map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        let slot = (top + 1) & 7;
        let x = self.finite[top].ok_or(ArithmeticError::Unsupported)?;
        let y = self.finite[slot].ok_or(ArithmeticError::Unsupported)?;
        if y.mantissa == 0 && !x.negative {
            return self.store_special(slot, crate::signed(0., y.negative), 0, true);
        }
        let magnitude = |v: Finite| Finite {
            negative: false,
            ..v
        };
        let swap = magnitude(y).compare(magnitude(x)).is_gt();
        let (smaller, larger) = if swap {
            (magnitude(x), magnitude(y))
        } else {
            (magnitude(y), magnitude(x))
        };
        let mut angle = if smaller.mantissa == 0 {
            [0; 4]
        } else {
            let distance = larger.exponent - smaller.exponent;
            if distance > 128 && !swap && !x.negative {
                let mut r = smaller.divide(larger, self.control | 0x300)?;
                r.value.negative = y.negative;
                r.inexact = true;
                return self.store_rounded(slot, r, true);
            }
            let a = from_finite(Finite {
                exponent: -distance,
                ..smaller
            });
            let b = from_finite(Finite {
                exponent: 0,
                ..larger
            });
            arctangent(ratio(a, b))
        };
        if swap {
            angle = subtract(PI_2, angle);
        }
        if x.negative {
            angle = subtract(add(PI_2, PI_2), angle);
        }
        self.store_rounded(slot, output(angle, y.negative, self.control)?, true)
    }
    pub fn y_log2_x_plus_one(&mut self) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        self.st(1).map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        let slot = (top + 1) & 7;
        let x = self.finite[top].ok_or(ArithmeticError::Unsupported)?;
        let y = self.finite[slot].ok_or(ArithmeticError::Unsupported)?;
        let magnitude = Finite {
            negative: false,
            ..x
        };
        if x.negative && magnitude.compare(Finite::from_i64(1)).is_ge() {
            if magnitude.compare(Finite::from_i64(1)).is_eq() && y.mantissa != 0 {
                return self.store_special(
                    slot,
                    crate::signed(f64::INFINITY, !y.negative),
                    4,
                    true,
                );
            }
            return self.store_special(slot, f64::from_bits(0xFFF8_0000_0000_0000), 1, true);
        }
        self.check_denormals(&[top, slot])?;
        if x.mantissa == 0 || y.mantissa == 0 {
            return self.store_special(slot, crate::signed(0., x.negative ^ y.negative), 0, true);
        }
        if x.exponent >= -1 {
            // Large inputs do not lose cancellation bits in 1+x; retain the
            // logarithm's extended operand precision.
            let one = Finite::from_i64(1);
            let mut neg_x = x;
            neg_x.negative = !neg_x.negative;
            let argument = one.subtract(neg_x, self.control | 0x300)?.value;
            if argument.negative || argument.mantissa == 0 {
                return self.store_special(slot, f64::from_bits(0xFFF8_0000_0000_0000), 1, true);
            }
            return self.store_rounded(
                slot,
                crate::logarithm::multiply_log2(argument, y, self.control)?,
                true,
            );
        }
        // log(1+x)/x = 1 - x/2 + x^2/3 - ... avoids cancellation near zero.
        let t = from_finite(x);
        let mut power = ONE;
        let mut factor = ONE;
        for n in 1..=256_u64 {
            power = multiply(power, t);
            let term = divide_small(power, n + 1);
            if term == [0; 4] {
                break;
            }
            if !x.negative && n & 1 != 0 {
                factor = subtract(factor, term);
            } else {
                factor = add(factor, term);
            }
        }
        let result = wide::rounded_product_pair(
            multiply(LOG2_E, factor),
            192,
            x,
            y,
            x.negative ^ y.negative,
            self.control,
        )?;
        self.store_rounded(slot, result, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn log1p_multiplies_extreme_operands_before_rounding_the_result() {
        // x=2^-16445, y=2^16383: rounding log2(1+x) to real80 first
        // would discard significant bits before multiplication by y.
        let mut fpu = X87::new();
        for raw in [(0x7FFE_u128 << 64) | (1 << 63), 1] {
            let bytes = raw.to_le_bytes();
            fpu.load_extended(core::array::from_fn(|i| bytes[i]))
                .unwrap();
        }
        fpu.y_log2_x_plus_one().unwrap();
        let actual = fpu.store_extended().unwrap();
        assert_eq!(fpu.status() & 0x3F, 0x22); // DE and PE, no intermediate underflow.
        let mut expected = X87::new();
        expected.push_integer(-62).unwrap();
        expected.load_constant(crate::Constant::Log2E).unwrap();
        expected.scale().unwrap();
        assert_eq!(actual, expected.store_extended().unwrap());
    }
    #[test]
    fn two_result_trig_specials_match_native_stack_payloads_and_flags() {
        // Native FPTAN/FSINCOS return two identical NaNs, not NaN and one.
        for kind in [2, 3] {
            for signaling in [false, true] {
                let raw = (0x7FFF_u128 << 64) | (1 << 63) | 1 | if signaling { 0 } else { 1 << 62 };
                let storage = raw.to_le_bytes();
                let mut fpu = X87::new();
                fpu.load_extended(core::array::from_fn(|i| storage[i]))
                    .unwrap();
                fpu.trigonometric(kind).unwrap();
                assert_eq!(fpu.status(), 0x3000 | u16::from(signaling));
                assert_eq!(fpu.raw(6), raw | (1 << 62));
                assert_eq!(fpu.raw(7), raw | (1 << 62));
            }
        }
        for kind in [2, 3] {
            let mut fpu = X87::new();
            fpu.push(f64::INFINITY).unwrap();
            fpu.trigonometric(kind).unwrap();
            assert_eq!(fpu.status(), 0x3001);
            assert_eq!(fpu.raw(6), (0xFFFF_u128 << 64) | (3 << 62));
            assert_eq!(fpu.raw(6), fpu.raw(7));
        }
    }
    #[test]
    fn two_result_instructions_complete_both_results_on_unmasked_precision() {
        for kind in [2, 3] {
            let mut fpu = X87::new();
            fpu.push(0.5).unwrap();
            fpu.load_control(0x35F);
            assert_eq!(
                fpu.trigonometric(kind),
                Err(ArithmeticError::UnmaskedException(0x20))
            );
            assert_eq!(fpu.status() & 0x3800, 0x3000);
            assert!(fpu.st(1).is_ok());
            if kind == 3 {
                assert_eq!(fpu.st0(), Ok(1.));
            }
        }
    }
    #[test]
    fn transcendental_results_match_sampled_native_x87_values() {
        let mut count = 0;
        for line in include_str!("native_trigonometry.txt")
            .lines()
            .filter(|l| !l.starts_with('#'))
        {
            let mut fields = line
                .split_whitespace()
                .map(|v| u64::from_str_radix(v, 16).unwrap());
            let cw = fields.next().unwrap() as u16;
            let op = fields.next().unwrap();
            let x = f64::from_bits(fields.next().unwrap());
            let y = f64::from_bits(fields.next().unwrap());
            let mantissa = fields.next().unwrap();
            let sign_exponent = fields.next().unwrap();
            let sw = fields.next().unwrap() as u16;
            // Intel documents a limited-precision pi constant in hardware
            // range reduction. Large arguments are tested separately for
            // stack/C2 semantics, not equality to that approximation.
            if x.abs() > 1e6 && op < 3 {
                continue;
            }
            let mut fpu = X87::new();
            fpu.load_control(cw);
            fpu.push(y).unwrap();
            fpu.push(x).unwrap();
            match op {
                0 => fpu.trigonometric(0),
                1 => fpu.trigonometric(1),
                2 => fpu
                    .trigonometric(3)
                    .and_then(|_| fpu.pop().map_err(ArithmeticError::Stack)),
                3 => fpu.arctangent_pop(),
                _ => fpu.y_log2_x_plus_one(),
            }
            .unwrap();
            let actual = fpu.store_extended().unwrap();
            let actual_mantissa = u64::from_le_bytes(actual[..8].try_into().unwrap());
            let actual_exponent = u16::from_le_bytes(actual[8..].try_into().unwrap());
            assert_eq!(
                actual_exponent & 0x8000,
                sign_exponent as u16 & 0x8000,
                "{line}"
            );
            let rank = |exponent: u64, mantissa: u64| {
                (u128::from(exponent & 0x7FFF) << 63) + u128::from(mantissa & !(1 << 63))
            };
            assert!(
                rank(u64::from(actual_exponent), actual_mantissa)
                    .abs_diff(rank(sign_exponent, mantissa))
                    <= 4,
                "{line}: got {actual_mantissa:016X} {actual_exponent:04X}"
            );
            assert_eq!(fpu.status() & 0x3C20, sw & 0x3C20, "{line}");
            count += 1;
        }
        assert_eq!(count, 1944);
    }
    #[test]
    fn trig_range_and_stack_effects_are_explicit() {
        for kind in 0..4 {
            let mut fpu = X87::new();
            fpu.push(9223372036854775808.).unwrap();
            let raw = fpu.store_extended().unwrap();
            let tags = fpu.tags();
            fpu.trigonometric(kind).unwrap();
            assert_eq!(fpu.status() & 0x400, 0x400);
            assert_eq!(fpu.tags(), tags);
            assert_eq!(fpu.store_extended().unwrap(), raw);
        }
        let mut fpu = X87::new();
        fpu.push(-0.).unwrap();
        fpu.trigonometric(2).unwrap();
        assert_eq!(fpu.st0(), Ok(1.));
        assert_eq!(fpu.st(1).unwrap().to_bits(), (-0_f64).to_bits());
    }
}
