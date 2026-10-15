//! Normal finite arithmetic with a 64-bit significand. Integer intermediates
//! preserve x87 precision independently of the host's floating-point unit.
use crate::ArithmeticError;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Finite {
    pub negative: bool,
    pub exponent: i32,
    pub mantissa: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct Rounded {
    pub value: Finite,
    pub inexact: bool,
    pub up: bool,
}

impl Finite {
    pub fn from_i64(value: i64) -> Self {
        let magnitude = value.unsigned_abs();
        let shift = magnitude.leading_zeros();
        Self {
            negative: value < 0,
            exponent: if magnitude == 0 { 0 } else { 63 - shift as i32 },
            mantissa: if magnitude == 0 {
                0
            } else {
                magnitude << shift
            },
        }
    }
    pub fn compare(self, other: Self) -> core::cmp::Ordering {
        use core::cmp::Ordering;
        if self.mantissa == 0 && other.mantissa == 0 {
            return Ordering::Equal;
        }
        if self.negative != other.negative {
            return if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let magnitude = if self.mantissa == 0 {
            Ordering::Less
        } else if other.mantissa == 0 {
            Ordering::Greater
        } else {
            self.exponent
                .cmp(&other.exponent)
                .then(self.mantissa.cmp(&other.mantissa))
        };
        if self.negative {
            magnitude.reverse()
        } else {
            magnitude
        }
    }
    pub fn from_f64(value: f64) -> Option<Self> {
        if !value.is_finite() {
            return None;
        }
        let bits = value.to_bits();
        let exponent = ((bits >> 52) & 0x7FF) as i32;
        let mantissa = (bits & 0xF_FFFF_FFFF_FFFF) | if exponent == 0 { 0 } else { 1 << 52 };
        let shift = mantissa.leading_zeros();
        Some(Self {
            negative: value.is_sign_negative(),
            exponent: exponent.max(1) - 1023 + 11 - shift as i32,
            mantissa: if mantissa == 0 { 0 } else { mantissa << shift },
        })
    }

    pub fn to_f64(self) -> f64 {
        if self.mantissa == 0 {
            return f64::from_bits(u64::from(self.negative) << 63);
        }
        // Arithmetic below currently accepts normal binary64 exponent range.
        let rounded = round(self.mantissa, self.exponent, self.negative, 0, 1, 0x027F)
            .expect("finite arithmetic result is within the supported exponent range")
            .value;
        let bits = (u64::from(rounded.negative) << 63)
            | (((rounded.exponent + 1023) as u64) << 52)
            | ((rounded.mantissa >> 11) & 0xF_FFFF_FFFF_FFFF);
        f64::from_bits(bits)
    }
    pub fn memory_round(self, single: bool, control: u16) -> Result<Rounded, ArithmeticError> {
        let result = round(
            self.mantissa,
            self.exponent,
            self.negative,
            0,
            1,
            (control & !0x300) | if single { 0 } else { 0x200 },
        )?;
        if single && result.value.mantissa != 0 && !(-126..=127).contains(&result.value.exponent) {
            return Err(ArithmeticError::Unsupported);
        }
        Ok(result)
    }
    pub fn integer_round(self, control: u16) -> Rounded {
        if self.mantissa == 0 || self.exponent >= 63 {
            return Rounded {
                value: self,
                inexact: false,
                up: false,
            };
        }
        let (kept, inexact, halfway) = if self.exponent < 0 {
            (
                0,
                true,
                if self.exponent == -1 {
                    self.mantissa.cmp(&(1 << 63))
                } else {
                    core::cmp::Ordering::Less
                },
            )
        } else {
            let discard = 63 - self.exponent as u32;
            let low = self.mantissa & ((1_u64 << discard) - 1);
            (
                self.mantissa >> discard,
                low != 0,
                low.cmp(&(1_u64 << (discard - 1))),
            )
        };
        let up = inexact
            && match (control >> 10) & 3 {
                0 => halfway.is_gt() || (halfway.is_eq() && kept & 1 != 0),
                1 => self.negative,
                2 => !self.negative,
                _ => false,
            };
        let magnitude = kept + u64::from(up);
        let shift = magnitude.leading_zeros();
        Rounded {
            value: Self {
                negative: self.negative,
                exponent: if magnitude == 0 { 0 } else { 63 - shift as i32 },
                mantissa: if magnitude == 0 {
                    0
                } else {
                    magnitude << shift
                },
            },
            inexact,
            up,
        }
    }

    pub fn divide(self, other: Self, control: u16) -> Result<Rounded, ArithmeticError> {
        if other.mantissa == 0 {
            return Err(ArithmeticError::Unsupported);
        }
        let shift = if self.mantissa < other.mantissa {
            64
        } else {
            63
        };
        let numerator = u128::from(self.mantissa) << shift;
        let denominator = u128::from(other.mantissa);
        round(
            (numerator / denominator) as u64,
            self.exponent - other.exponent + 63 - shift,
            self.negative ^ other.negative,
            numerator % denominator,
            denominator,
            control,
        )
    }

    pub fn multiply(self, other: Self, control: u16) -> Result<Rounded, ArithmeticError> {
        if self.mantissa == 0 || other.mantissa == 0 {
            return round(0, 0, self.negative ^ other.negative, 0, 1, control);
        }
        let product = u128::from(self.mantissa) * u128::from(other.mantissa);
        let shift = 64 - product.leading_zeros(); // Normal product shifts by 63 or 64.
        round(
            (product >> shift) as u64,
            self.exponent + other.exponent + shift as i32 - 63,
            self.negative ^ other.negative,
            product & ((1u128 << shift) - 1),
            1u128 << shift,
            control,
        )
    }

    pub fn subtract(self, mut other: Self, control: u16) -> Result<Rounded, ArithmeticError> {
        other.negative = !other.negative;
        if self.mantissa == 0 && other.mantissa != 0 {
            return round(
                other.mantissa,
                other.exponent,
                other.negative,
                0,
                1,
                control,
            );
        }
        if other.mantissa == 0 && self.mantissa != 0 {
            return round(self.mantissa, self.exponent, self.negative, 0, 1, control);
        }
        let exponent = self.exponent.max(other.exponent);
        let align = |value: Self| {
            let shift = (exponent - value.exponent) as u32;
            let wide = u128::from(value.mantissa) << 63;
            if shift == 0 {
                wide
            } else if shift >= 128 {
                u128::from(wide != 0)
            } else {
                (wide >> shift) | u128::from(wide & ((1u128 << shift) - 1) != 0)
            }
        };
        let lhs = align(self);
        let rhs = align(other);
        let (magnitude, negative) = if self.negative == other.negative {
            (lhs + rhs, self.negative)
        } else if lhs > rhs {
            (lhs - rhs, self.negative)
        } else if rhs > lhs {
            (rhs - lhs, other.negative)
        } else {
            (0, (control >> 10) & 3 == 1)
        };
        if magnitude == 0 {
            return round(0, 0, negative, 0, 1, control);
        }
        let highest = 127 - magnitude.leading_zeros();
        let result_exponent = exponent + highest as i32 - 126;
        if highest <= 63 {
            round(
                (magnitude << (63 - highest)) as u64,
                result_exponent,
                negative,
                0,
                1,
                control,
            )
        } else {
            let shift = highest - 63;
            round(
                (magnitude >> shift) as u64,
                result_exponent,
                negative,
                magnitude & ((1u128 << shift) - 1),
                1u128 << shift,
                control,
            )
        }
    }
}

fn round(
    mantissa: u64,
    mut exponent: i32,
    negative: bool,
    remainder: u128,
    denominator: u128,
    control: u16,
) -> Result<Rounded, ArithmeticError> {
    let precision = match (control >> 8) & 3 {
        0 => 24,
        2 => 53,
        3 => 64,
        _ => return Err(ArithmeticError::Unsupported),
    };
    let discard = 64 - precision;
    let low = mantissa & ((1u64 << discard) - 1);
    let inexact = low != 0 || remainder != 0;
    let halfway = if discard == 0 {
        (remainder * 2).cmp(&denominator)
    } else {
        low.cmp(&(1 << (discard - 1)))
            .then_with(|| remainder.cmp(&0))
    };
    let kept = mantissa >> discard;
    let up = inexact
        && match (control >> 10) & 3 {
            0 => halfway.is_gt() || (halfway.is_eq() && kept & 1 != 0),
            1 => negative,
            2 => !negative,
            _ => false,
        };
    let rounded = u128::from(kept) + u128::from(up);
    let mantissa = if rounded >= (1u128 << precision) {
        exponent += 1;
        1 << 63
    } else {
        (rounded as u64) << discard
    };
    if mantissa != 0 && !(-1022..=1022).contains(&exponent) {
        // Extended exponent range, subnormal results and overflow remain explicit.
        // Reserve the upper binary64 binade to keep diagnostic conversion finite.
        return Err(ArithmeticError::Unsupported);
    }
    Ok(Rounded {
        value: Finite {
            negative,
            exponent,
            mantissa,
        },
        inexact,
        up,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn arithmetic_matches_captured_native_x87_results() {
        for line in include_str!("native_vectors.txt")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let mut fields = line
                .split_whitespace()
                .map(|value| u64::from_str_radix(value, 16).unwrap());
            let control = fields.next().unwrap() as u16;
            let lhs = Finite::from_f64(f64::from_bits(fields.next().unwrap())).unwrap();
            let rhs = Finite::from_f64(f64::from_bits(fields.next().unwrap())).unwrap();
            let operation = fields.next().unwrap();
            let mantissa = fields.next().unwrap();
            let sign_exponent = fields.next().unwrap();
            let status = fields.next().unwrap();
            let result = match operation {
                0 => lhs.divide(rhs, control),
                1 => lhs.multiply(rhs, control),
                _ => lhs.subtract(rhs, control),
            }
            .unwrap();
            assert_eq!(result.value.mantissa, mantissa, "{line}");
            if mantissa != 0 {
                assert_eq!(
                    result.value.exponent,
                    (sign_exponent & 0x7FFF) as i32 - 16383,
                    "{line}"
                );
            }
            assert_eq!(result.value.negative, sign_exponent & 0x8000 != 0, "{line}");
            assert_eq!(result.inexact, status & 0x20 != 0, "{line}");
            assert_eq!(result.up, status & 0x200 != 0, "{line}");
        }
    }
}
