//! Extended-range finite arithmetic with a 64-bit significand. Integer intermediates
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
    pub exceptions: u16,
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
        let rounded = round_format(
            self.mantissa,
            self.exponent,
            self.negative,
            0,
            1,
            0x027F,
            53,
            -1022,
            1023,
        )
        .value;
        let sign = u64::from(rounded.negative) << 63;
        let bits = if rounded.mantissa == 0 {
            sign
        } else if rounded.exponent > 1023 {
            sign | 0x7FF0_0000_0000_0000
        } else if rounded.exponent < -1022 {
            sign | (rounded.mantissa >> (-1022 - rounded.exponent + 11))
        } else {
            sign | (((rounded.exponent + 1023) as u64) << 52)
                | ((rounded.mantissa >> 11) & 0xF_FFFF_FFFF_FFFF)
        };
        f64::from_bits(bits)
    }
    pub fn memory_round(self, single: bool, control: u16) -> Result<Rounded, ArithmeticError> {
        let result = round_format(
            self.mantissa,
            self.exponent,
            self.negative,
            0,
            1,
            control,
            if single { 24 } else { 53 },
            if single { -126 } else { -1022 },
            if single { 127 } else { 1023 },
        );
        Ok(result)
    }
    pub fn integer_round(self, control: u16) -> Rounded {
        if self.mantissa == 0 || self.exponent >= 63 {
            return Rounded {
                value: self,
                inexact: false,
                up: false,
                exceptions: 0,
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
            exceptions: 0,
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

pub(super) fn round(
    mantissa: u64,
    exponent: i32,
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
    Ok(round_format(
        mantissa,
        exponent,
        negative,
        remainder,
        denominator,
        control,
        precision,
        -16382,
        16383,
    ))
}

// Normalized significand plus an exact fractional remainder. Keeping the
// exponent independent of the host format is essential for extended values.
#[allow(clippy::too_many_arguments)]
fn round_format(
    mantissa: u64,
    mut exponent: i32,
    negative: bool,
    remainder: u128,
    denominator: u128,
    control: u16,
    precision: u32,
    minimum: i32,
    maximum: i32,
) -> Rounded {
    // x87 detects tininess after rounding to the selected significand width,
    // before the additional rounding needed for a subnormal exponent. This
    // differs from simply testing the final (possibly normal) destination.
    let tiny = exponent < minimum
        && round_format(
            mantissa,
            exponent,
            negative,
            remainder,
            denominator,
            control,
            precision,
            i32::MIN,
            i32::MAX,
        )
        .value
        .exponent
            < minimum;
    let discard = (64 - precision) + minimum.saturating_sub(exponent).max(0) as u32;
    let (kept, low, halfway) = if discard == 0 {
        // Compare 2*r with d without overflowing a 128-bit intermediate.
        (mantissa, 0, remainder.cmp(&(denominator - remainder)))
    } else if discard < 64 {
        let low = mantissa & ((1_u64 << discard) - 1);
        (
            mantissa >> discard,
            low,
            low.cmp(&(1_u64 << (discard - 1)))
                .then_with(|| remainder.cmp(&0)),
        )
    } else if discard == 64 {
        (
            0,
            mantissa,
            mantissa.cmp(&(1_u64 << 63)).then_with(|| remainder.cmp(&0)),
        )
    } else {
        (0, mantissa, core::cmp::Ordering::Less)
    };
    let inexact = low != 0 || remainder != 0;
    let mut up = inexact
        && match (control >> 10) & 3 {
            0 => halfway.is_gt() || (halfway.is_eq() && kept & 1 != 0),
            1 => negative,
            2 => !negative,
            _ => false,
        };
    let rounded = u128::from(kept) + u128::from(up);
    let (mantissa, new_exponent) = if rounded == 0 {
        (0, 0)
    } else {
        let highest = 127 - rounded.leading_zeros();
        (
            (if highest > 63 {
                rounded >> (highest - 63)
            } else {
                rounded << (63 - highest)
            }) as u64,
            exponent + discard as i32 + highest as i32 - 63,
        )
    };
    exponent = new_exponent;
    let mut exceptions = if tiny && inexact { 0x10 } else { 0 };
    let mut mantissa = mantissa;
    let mut inexact = inexact;
    if mantissa != 0 && exponent > maximum {
        exceptions |= 8;
        inexact = true;
        let infinity = match (control >> 10) & 3 {
            0 => true,
            1 => negative,
            2 => !negative,
            _ => false,
        };
        up = infinity;
        exponent = maximum + i32::from(infinity);
        mantissa = if infinity {
            1 << 63
        } else {
            u64::MAX << (64 - precision)
        };
    }
    Rounded {
        value: Finite {
            negative,
            exponent,
            mantissa,
        },
        inexact,
        up,
        exceptions,
    }
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
