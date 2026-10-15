//! Bounded integer algorithms for root, remainder and stack utilities.
use crate::{
    ArithmeticError, X87,
    finite::{self, Finite},
};

impl X87 {
    /// FXAM classifies the physical register without raising a stack exception.
    pub fn examine(&mut self) {
        let slot = ((self.status >> 11) & 7) as usize;
        let raw = self.raw(slot);
        let condition = if (self.tags >> (slot * 2)) & 3 == 3 {
            0x4100
        } else if raw as u64 >> 63 == 0 && (raw >> 64) & 0x7FFF != 0 {
            0
        } else if let Some(v) = self.finite[slot] {
            if v.mantissa == 0 {
                0x4000
            } else if (raw >> 64) & 0x7FFF == 0 {
                0x4400
            } else {
                0x400
            }
        } else if raw as u64 == 1 << 63 {
            0x500
        } else {
            0x100
        };
        self.status =
            (self.status & !0x4700) | condition | if raw & (1 << 79) != 0 { 0x200 } else { 0 };
    }
    /// FUCOM only raises invalid for signaling NaNs; quiet NaNs are unordered.
    pub fn unordered_compare(&mut self, index: u8, pops: u8) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        self.st(index).map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        self.compare_quiet(
            self.number(top),
            self.number((top + usize::from(index)) & 7),
            pops,
        )
    }
    pub fn square_root(&mut self) -> Result<(), ArithmeticError> {
        let diagnostic = self.st0().map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        let Some(x) = self.finite[top] else {
            if diagnostic == f64::INFINITY {
                self.status &= !0x200;
                return Ok(());
            }
            if diagnostic.is_nan() {
                return self.binary(
                    top,
                    self.number(top),
                    crate::binary::Number::from_finite(Finite::from_i64(0)),
                    0,
                    false,
                );
            }
            return self.store_special(top, f64::from_bits(0xFFF8_0000_0000_0000), 1, false);
        };
        if x.mantissa == 0 {
            self.status &= !0x200;
            return Ok(());
        }
        if x.negative {
            return self.store_special(top, f64::from_bits(0xFFF8_0000_0000_0000), 1, false);
        }
        self.check_denormals(&[top])?;
        let numerator = u128::from(x.mantissa) << (63 + (x.exponent & 1));
        let mut remainder = numerator;
        let mut root = 0_u128;
        let mut bit = 1_u128 << 126;
        // Restoring square root: 64 iterations, no host floating point.
        while bit != 0 {
            if remainder >= root + bit {
                remainder -= root + bit;
                root = (root >> 1) + bit;
            } else {
                root >>= 1;
            }
            bit >>= 2;
        }
        let result = finite::round(
            root as u64,
            x.exponent.div_euclid(2),
            false,
            remainder,
            2 * root + 1,
            self.control,
        )?;
        self.store_rounded(top, result, false)
    }
    pub fn extract(&mut self) -> Result<(), ArithmeticError> {
        let diagnostic = self.st0().map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        // Check space before modifying either result.
        if (self.tags >> (((top + 7) & 7) * 2)) & 3 != 3 {
            return Err(ArithmeticError::Stack(crate::StackError::Overflow));
        }
        let Some(mut value) = self.finite[top] else {
            if diagnostic.is_nan() {
                self.binary(
                    top,
                    self.number(top),
                    crate::binary::Number::from_finite(Finite::from_i64(0)),
                    0,
                    false,
                )?;
                self.duplicate(0).map_err(ArithmeticError::Stack)?;
                return Ok(());
            }
            self.store_special(top, f64::INFINITY, 0, false)?;
            self.push(diagnostic).map_err(ArithmeticError::Stack)?;
            return Ok(());
        };
        self.check_denormals(&[top])?;
        if value.mantissa == 0 {
            self.store_special(top, f64::NEG_INFINITY, 4, false)?;
            self.push(diagnostic).map_err(ArithmeticError::Stack)?;
        } else {
            let exponent = value.exponent;
            value.exponent = 0;
            self.store_special(top, f64::from(exponent), 0, false)?;
            self.push(value.to_f64()).map_err(ArithmeticError::Stack)?;
            let slot = ((self.status >> 11) & 7) as usize;
            self.finite[slot] = Some(value);
            self.encodings[slot] = crate::extended::encode(value);
        }
        self.status &= !0x200;
        Ok(())
    }
    /// FPREM/FPREM1 reduce at most 63 quotient bits per visit. C2 requests
    /// another guest instruction for larger exponent differences.
    pub fn remainder(&mut self, nearest: bool) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        self.st(1).map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        let Some(x) = self.finite[top] else {
            return self.store_special(top, f64::from_bits(0xFFF8_0000_0000_0000), 1, false);
        };
        let Some(y) = self.finite[(top + 1) & 7] else {
            if self.values[(top + 1) & 7].is_infinite() {
                self.status &= !0x4700;
                return Ok(());
            }
            return self.store_special(top, f64::NAN, 0, false);
        };
        if y.mantissa == 0 {
            return self.store_special(top, f64::from_bits(0xFFF8_0000_0000_0000), 1, false);
        }
        self.check_denormals(&[top, (top + 1) & 7])?;
        let difference = x.exponent - y.exponent;
        if x.mantissa == 0 || difference < if nearest { -1 } else { 0 } {
            self.status &= !0x4700;
            return Ok(());
        }
        let partial = difference >= 64;
        let reduction = if partial { difference - 32 } else { 0 };
        let (numerator, denominator, exponent) = if difference == -1 {
            (
                u128::from(x.mantissa),
                u128::from(y.mantissa) << 1,
                x.exponent,
            )
        } else {
            (
                u128::from(x.mantissa) << (difference - reduction),
                u128::from(y.mantissa),
                y.exponent + reduction,
            )
        };
        let mut quotient = numerator / denominator;
        let mut remainder = numerator % denominator;
        let up = nearest
            && !partial
            && (remainder > denominator - remainder
                || (remainder == denominator - remainder && quotient & 1 != 0));
        if up {
            quotient += 1;
            remainder = denominator - remainder;
        }
        let highest = if remainder == 0 {
            0
        } else {
            127 - remainder.leading_zeros()
        };
        let value = Finite {
            negative: x.negative ^ up,
            exponent: exponent + highest as i32 - 63,
            mantissa: if remainder == 0 {
                0
            } else {
                (remainder << (63 - highest)) as u64
            },
        };
        self.store_rounded(
            top,
            finite::round(
                value.mantissa,
                value.exponent,
                value.negative,
                0,
                1,
                self.control | 0x300,
            )?,
            false,
        )?;
        if partial {
            self.status |= 0x400;
        } else {
            self.status = (self.status & !0x4700)
                | (((quotient as u16 >> 2) & 1) << 8)
                | (((quotient as u16 >> 1) & 1) << 14)
                | ((quotient as u16 & 1) << 9);
        }
        Ok(())
    }
    pub fn load_bcd(&mut self, bytes: [u8; 10]) -> Result<(), ArithmeticError> {
        let mut magnitude = 0_i64;
        for byte in bytes[..9].iter().rev() {
            // Invalid BCD digits are architecturally undefined, keep them explicit.
            if byte & 15 > 9 || byte >> 4 > 9 {
                return Err(ArithmeticError::Unsupported);
            }
            magnitude = magnitude * 100 + i64::from(byte >> 4) * 10 + i64::from(byte & 15);
        }
        self.push_integer(if bytes[9] & 0x80 != 0 {
            -magnitude
        } else {
            magnitude
        })
        .map_err(ArithmeticError::Stack)?;
        if magnitude == 0 && bytes[9] & 0x80 != 0 {
            self.change_sign().map_err(ArithmeticError::Stack)?;
        }
        Ok(())
    }
    pub fn store_bcd(&mut self) -> Result<[u8; 10], ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        let slot = ((self.status >> 11) & 7) as usize;
        let rounded = self.finite[slot].map(|v| v.integer_round(self.control));
        let magnitude = rounded
            .and_then(|r| {
                let v = r.value;
                if v.mantissa == 0 {
                    Some(0)
                } else if (0..=59).contains(&v.exponent) {
                    Some(v.mantissa >> (63 - v.exponent))
                } else {
                    None
                }
            })
            .filter(|v| *v < 1_000_000_000_000_000_000);
        let Some(mut magnitude) = magnitude else {
            self.status = (self.status & !0x200) | 1;
            if self.control & 1 == 0 {
                self.status |= 0x8080;
                return Err(ArithmeticError::UnmaskedException(1));
            }
            return Ok([0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]);
        };
        let rounded = rounded.unwrap();
        let exception = if rounded.inexact { 0x20 } else { 0 };
        self.status = (self.status & !0x200) | exception | if rounded.up { 0x200 } else { 0 };
        if exception & !self.control != 0 {
            self.status |= 0x8080;
            return Err(ArithmeticError::UnmaskedException(exception));
        }
        let mut bytes = [0; 10];
        for byte in &mut bytes[..9] {
            *byte = (magnitude % 10) as u8;
            magnitude /= 10;
            *byte |= ((magnitude % 10) as u8) << 4;
            magnitude /= 10;
        }
        bytes[9] = u8::from(rounded.value.negative) << 7;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn utilities_match_native_results_and_defined_condition_flags() {
        let mut count = 0;
        for line in include_str!("native_utilities.txt")
            .lines()
            .filter(|l| !l.starts_with('#'))
        {
            let f: [u64; 7] = core::array::from_fn({
                let mut fields = line.split_whitespace();
                move |_| u64::from_str_radix(fields.next().unwrap(), 16).unwrap()
            });
            let mut fpu = X87::new();
            fpu.load_control(f[0] as u16);
            fpu.push(f64::from_bits(f[3])).unwrap();
            fpu.push(f64::from_bits(f[2])).unwrap();
            if f[1] == 0 {
                fpu.square_root().unwrap();
            } else {
                for _ in 0..1024 {
                    fpu.remainder(f[1] == 2).unwrap();
                    if fpu.status() & 0x400 == 0 {
                        break;
                    }
                }
                assert_eq!(fpu.status() & 0x400, 0, "{line}");
            }
            let actual = fpu.store_extended().unwrap();
            assert_eq!(
                u64::from_le_bytes(actual[..8].try_into().unwrap()),
                f[4],
                "{line}"
            );
            assert_eq!(
                u16::from_le_bytes(actual[8..].try_into().unwrap()),
                f[5] as u16,
                "{line}"
            );
            let mask = if f[1] == 0 { 0x3A3D } else { 0x7F3D }; // DE from FLD m64 is outside this arithmetic-only probe.
            assert_eq!(fpu.status() & mask, f[6] as u16 & mask, "{line}");
            count += 1;
        }
        assert_eq!(count, 456);
    }
    #[test]
    fn packed_bcd_retains_18_digits_and_negative_zero() {
        for integer in [
            0,
            1,
            -1,
            999_999_999_999_999_999_i64,
            -999_999_999_999_999_999,
        ] {
            let mut fpu = X87::new();
            fpu.push_integer(integer).unwrap();
            let bytes = fpu.store_bcd().unwrap();
            fpu.initialize();
            fpu.load_bcd(bytes).unwrap();
            assert_eq!(fpu.store_integer(64).unwrap() as i64, integer);
        }
        let mut fpu = X87::new();
        fpu.push(-0.).unwrap();
        let bytes = fpu.store_bcd().unwrap();
        assert_eq!(bytes[9], 0x80);
        fpu.initialize();
        fpu.load_bcd(bytes).unwrap();
        assert_eq!(fpu.st0().unwrap().to_bits(), (-0_f64).to_bits());
        fpu.initialize();
        fpu.push(1e18).unwrap();
        assert_eq!(
            fpu.store_bcd().unwrap(),
            [0, 0, 0, 0, 0, 0, 0, 0xC0, 0xFF, 0xFF]
        );
        assert_eq!(fpu.status() & 0x21, 1);
    }
}
