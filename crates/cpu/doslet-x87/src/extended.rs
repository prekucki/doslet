//! Little-endian x87 real80 transfers. Binary64 is only an inspection view.
use crate::{ArithmeticError, X87, finite::Finite};

pub(super) fn from_f64(value: f64) -> u128 {
    let bits = value.to_bits();
    let sign = u128::from(bits >> 63) << 79;
    if let Some(value) = Finite::from_f64(value) {
        encode(value)
    } else {
        sign | (0x7FFF_u128 << 64) | (1 << 63) | (u128::from(bits & ((1 << 52) - 1)) << 11)
    }
}

pub(super) fn encode(value: Finite) -> u128 {
    let sign = u128::from(value.negative) << 79;
    if value.mantissa == 0 {
        return sign;
    }
    if value.exponent < -16382 {
        return sign
            | u128::from(
                value
                    .mantissa
                    .checked_shr((-16382 - value.exponent) as u32)
                    .unwrap_or(0),
            );
    }
    sign | (((value.exponent + 16383) as u128) << 64) | u128::from(value.mantissa)
}
impl X87 {
    pub(super) fn raw(&self, slot: usize) -> u128 {
        self.encodings[slot]
    }
    /// Exact extended encoding, including NaN payloads, independently of PC/RC.
    pub fn store_extended(&self) -> Result<[u8; 10], ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        let raw = self.raw(((self.status >> 11) & 7) as usize).to_le_bytes();
        Ok(core::array::from_fn(|i| raw[i]))
    }
    pub fn load_extended(&mut self, bytes: [u8; 10]) -> Result<(), ArithmeticError> {
        let mut storage = [0; 16];
        storage[..10].copy_from_slice(&bytes);
        let raw = u128::from_le_bytes(storage);
        let mantissa = raw as u64;
        let exponent = ((raw >> 64) & 0x7FFF) as i32;
        let negative = raw & (1 << 79) != 0;
        let finite = if exponent < 0x7FFF && (exponent == 0 || mantissa >> 63 != 0) {
            let shift = if mantissa == 0 {
                0
            } else {
                mantissa.leading_zeros()
            };
            Some(Finite {
                negative,
                exponent: exponent.max(1) - 16383 - shift as i32,
                mantissa: mantissa << shift,
            })
        } else {
            None
        };
        let diagnostic = finite.map(Finite::to_f64).unwrap_or_else(|| {
            let fraction = raw as u64 & ((1 << 63) - 1);
            let fraction = if mantissa >> 63 == 0 {
                1 << 51
            } else {
                (fraction >> 11) | u64::from(fraction != 0)
            };
            f64::from_bits((((raw >> 79) as u64) << 63) | 0x7FF0_0000_0000_0000 | fraction)
        });
        self.push(diagnostic).map_err(ArithmeticError::Stack)?;
        let slot = ((self.status >> 11) & 7) as usize;
        self.finite[slot] = finite;
        self.encodings[slot] = raw;
        let tag = match finite {
            Some(v) if v.mantissa == 0 => 1,
            Some(_) if exponent != 0 => 0,
            _ => 2,
        };
        self.tags = (self.tags & !(3 << (slot * 2))) | (tag << (slot * 2));
        self.status &= !0x200;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_store_boundaries_match_native_formats_and_rounding() {
        for line in include_str!("native_stores.txt").lines() {
            let f: [u64; 6] = core::array::from_fn({
                let mut fields = line.split_whitespace();
                move |_| u64::from_str_radix(fields.next().unwrap(), 16).unwrap()
            });
            let mut fpu = X87::new();
            fpu.load_control(f[0] as u16);
            let mut bytes = [0; 10];
            bytes[..8].copy_from_slice(&f[2].to_le_bytes());
            bytes[8..].copy_from_slice(&(f[3] as u16).to_le_bytes());
            fpu.load_extended(bytes).unwrap();
            let result = fpu.store_float(f[1] != 0).unwrap();
            fpu.pop().unwrap();
            assert_eq!(result.to_bits(), f[4], "{line}");
            assert_eq!(fpu.status(), f[5] as u16, "{line}");
        }
    }
    #[test]
    fn scale_extrema_match_native_real80_results_and_exceptions() {
        for line in include_str!("native_ranges.txt").lines() {
            let f: [u64; 7] = core::array::from_fn({
                let mut fields = line.split_whitespace();
                move |_| u64::from_str_radix(fields.next().unwrap(), 16).unwrap()
            });
            let mut fpu = X87::new();
            fpu.load_control(f[0] as u16);
            fpu.push(f64::from_bits(f[3])).unwrap();
            let mut bytes = [0; 10];
            bytes[..8].copy_from_slice(&f[1].to_le_bytes());
            bytes[8..].copy_from_slice(&(f[2] as u16).to_le_bytes());
            fpu.load_extended(bytes).unwrap();
            fpu.scale().unwrap();
            let result = fpu.store_extended().unwrap();
            assert_eq!(
                u64::from_le_bytes(result[..8].try_into().unwrap()),
                f[4],
                "{line}"
            );
            assert_eq!(
                u16::from_le_bytes(result[8..].try_into().unwrap()),
                f[5] as u16,
                "{line}"
            );
            assert_eq!(fpu.status(), f[6] as u16, "{line}");
        }
    }
    #[test]
    fn raw_transfers_preserve_native_encodings_tags_and_status() {
        // Captured with FLD tbyte; FNSTSW; FNSTENV; FSTP tbyte on x86-64.
        for (mantissa, exponent, tag) in [
            (0_u64, 0_u16, 1),
            (1, 0, 2),
            (1 << 63, 0, 2),
            (u64::MAX, 0, 2),
            (1 << 63, 1, 0),
            (u64::MAX, 0x7FFE, 0),
            (1 << 63, 0x7FFF, 2),
            (0x8000_0000_0000_0001, 0x7FFF, 2),
            (0xC000_0000_0000_0001, 0x7FFF, 2),
            (0, 0x3FFF, 2),
        ] {
            for sign in [0, 0x8000] {
                let mut bytes = [0; 10];
                bytes[..8].copy_from_slice(&mantissa.to_le_bytes());
                bytes[8..].copy_from_slice(&(exponent | sign).to_le_bytes());
                let mut fpu = X87::new();
                fpu.load_extended(bytes).unwrap();
                assert_eq!(fpu.store_extended().unwrap(), bytes);
                assert_eq!(fpu.status(), 0x3800);
                assert_eq!(fpu.tags(), 0x3FFF | (tag << 14));
                fpu.duplicate(0).unwrap();
                assert_eq!(fpu.store_extended().unwrap(), bytes);
            }
        }
    }
    #[test]
    fn full_range_finite_arithmetic_does_not_use_the_diagnostic_binary64() {
        for exponent in [1, 100, 0x3FFF, 0x7FFD] {
            let mut bytes = [0; 10];
            bytes[..8].copy_from_slice(&(1_u64 << 63).to_le_bytes());
            bytes[8..].copy_from_slice(&(exponent as u16).to_le_bytes());
            let mut fpu = X87::new();
            fpu.load_extended(bytes).unwrap();
            fpu.duplicate(0).unwrap();
            fpu.divide_pop(1, false).unwrap();
            assert_eq!(fpu.st0(), Ok(1.));
            fpu.initialize();
            fpu.load_extended(bytes).unwrap();
            fpu.arithmetic_memory(2., 1).unwrap();
            bytes[8..].copy_from_slice(&((exponent + 1) as u16).to_le_bytes());
            assert_eq!(fpu.store_extended().unwrap(), bytes);
        }
    }
}
