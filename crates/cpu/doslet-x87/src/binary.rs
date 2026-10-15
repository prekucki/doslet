//! Shared operand rules for register and memory basic arithmetic.
use crate::{ArithmeticError, X87, extended, finite::Finite};
#[derive(Clone, Copy)]
pub(super) struct Number {
    pub finite: Option<Finite>,
    pub raw: u128,
    denormal: bool,
}

impl Number {
    pub fn from_finite(value: Finite) -> Self {
        Self {
            finite: Some(value),
            raw: extended::encode(value),
            denormal: value.mantissa != 0 && value.exponent < -16382,
        }
    }
    pub fn from_f64(value: f64) -> Self {
        Self {
            finite: Finite::from_f64(value),
            raw: extended::from_f64(value),
            denormal: value.is_subnormal(),
        }
    }
    fn from_real(bits: u64, single: bool) -> Self {
        if !single {
            return Self::from_f64(f64::from_bits(bits));
        }
        let bits = bits as u32;
        let exponent = (bits >> 23) & 0xFF;
        let fraction = bits & 0x7F_FFFF;
        let mut value = Self::from_f64(f64::from(f32::from_bits(bits)));
        value.denormal = exponent == 0 && fraction != 0;
        if exponent == 0xFF {
            value.raw = (u128::from(bits >> 31) << 79)
                | (0x7FFF_u128 << 64)
                | (1 << 63)
                | (u128::from(fraction) << 40);
        }
        value
    }
    fn unsupported(self) -> bool {
        self.finite.is_none() && (self.raw as u64 >> 63 == 0 || (self.raw >> 64) & 0x7FFF != 0x7FFF)
    }
    fn negative(self) -> bool {
        self.raw & (1 << 79) != 0
    }
    fn zero(self) -> bool {
        self.finite.is_some_and(|v| v.mantissa == 0)
    }
    fn nan(self) -> bool {
        self.finite.is_none() && !self.infinity()
    }
    fn infinity(self) -> bool {
        ((self.raw >> 64) & 0x7FFF) == 0x7FFF && self.raw as u64 == 1 << 63
    }
    fn invalid(self) -> bool {
        self.nan()
            && (self.raw & (1 << 62) == 0
                || self.raw as u64 >> 63 == 0
                || (self.raw >> 64) & 0x7FFF != 0x7FFF)
    }
    fn negate(mut self) -> Self {
        self.raw ^= 1 << 79;
        if let Some(v) = &mut self.finite {
            v.negative = !v.negative;
        }
        self
    }
}
impl X87 {
    pub(super) fn special_memory(&mut self, slot: usize) -> Result<f64, ArithmeticError> {
        let number = self.number(slot);
        self.status &= !0x200;
        if number.infinity() {
            return Ok(crate::signed(f64::INFINITY, number.negative()));
        }
        let exception = u16::from(number.invalid());
        self.status |= exception;
        if exception & !self.control != 0 {
            self.status |= 0x8080;
            return Err(ArithmeticError::UnmaskedException(exception));
        }
        let raw = if number.unsupported() {
            (0xFFFF_u128 << 64) | (3 << 62)
        } else {
            number.raw
        };
        Ok(f64::from_bits(
            (((raw >> 79) as u64) << 63)
                | 0x7FF8_0000_0000_0000
                | ((raw as u64 & ((1 << 63) - 1)) >> 11),
        ))
    }
    pub(super) fn check_denormals(&mut self, slots: &[usize]) -> Result<(), ArithmeticError> {
        if slots.iter().any(|&slot| self.number(slot).denormal) {
            self.status |= 2;
            if self.control & 2 == 0 {
                self.status |= 0x8080;
                return Err(ArithmeticError::UnmaskedException(2));
            }
        }
        Ok(())
    }

    pub fn load_real(&mut self, bits: u64, single: bool) -> Result<(), ArithmeticError> {
        let mut number = Number::from_real(bits, single);
        let exception = if number.invalid() {
            1
        } else if number.denormal {
            2
        } else {
            0
        };
        self.status |= exception;
        if exception & !self.control != 0 {
            self.status |= 0x8080;
            return Err(ArithmeticError::UnmaskedException(exception));
        }
        if number.nan() {
            number.raw |= 1 << 62;
        }
        let storage = number.raw.to_le_bytes();
        self.load_extended(core::array::from_fn(|i| storage[i]))
    }
    pub fn arithmetic_real(
        &mut self,
        bits: u64,
        single: bool,
        operation: u8,
    ) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        self.binary(
            top,
            self.number(top),
            Number::from_real(bits, single),
            operation,
            false,
        )
    }
    pub fn compare_real(
        &mut self,
        bits: u64,
        single: bool,
        pop: bool,
    ) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        self.compare_number(
            self.number(top),
            Number::from_real(bits, single),
            u8::from(pop),
        )
    }

    pub(super) fn number(&self, slot: usize) -> Number {
        Number {
            finite: self.finite[slot],
            raw: self.raw(slot),
            denormal: (self.raw(slot) >> 64) & 0x7FFF == 0 && self.raw(slot) as u64 != 0,
        }
    }
    pub(super) fn binary(
        &mut self,
        slot: usize,
        mut a: Number,
        mut b: Number,
        mut operation: u8,
        pop: bool,
    ) -> Result<(), ArithmeticError> {
        if !matches!(operation, 0 | 1 | 4 | 5 | 6 | 7) {
            return Err(ArithmeticError::Unsupported);
        }
        if a.nan() || b.nan() {
            let invalid = a.invalid() || b.invalid();
            let mut raw = if a.unsupported() || b.unsupported() {
                (0xFFFF_u128 << 64) | (3 << 62)
            } else if !a.nan() {
                b.raw
            } else if !b.nan() {
                a.raw
            } else if a.invalid() != b.invalid() {
                if a.invalid() { b.raw } else { a.raw }
            } else if (a.raw as u64)
                .cmp(&(b.raw as u64))
                .then_with(|| b.negative().cmp(&a.negative()))
                .is_ge()
            {
                a.raw
            } else {
                b.raw
            };
            raw |= 1 << 62;
            self.store_special(slot, f64::NAN, u16::from(invalid), pop)?;
            self.encodings[slot] = raw;
            self.values[slot] = f64::from_bits(
                (((raw >> 79) as u64) << 63)
                    | 0x7FF8_0000_0000_0000
                    | ((raw as u64 & ((1 << 63) - 1)) >> 11),
            );
            return Ok(());
        }
        let denormal = a.denormal || b.denormal;
        if denormal {
            self.status |= 2;
            if self.control & 2 == 0 {
                self.status |= 0x8080;
                return Err(ArithmeticError::UnmaskedException(2));
            }
        }
        if operation == 5 || operation == 7 {
            core::mem::swap(&mut a, &mut b);
            operation -= 1;
        }
        if operation == 4 {
            b = b.negate();
            operation = 0;
        }
        let negative = a.negative() ^ b.negative();
        if (operation == 0 && a.infinity() && b.infinity() && a.negative() != b.negative())
            || (operation == 1 && ((a.infinity() && b.zero()) || (b.infinity() && a.zero())))
            || (operation == 6 && ((a.zero() && b.zero()) || (a.infinity() && b.infinity())))
        {
            return self.store_special(slot, f64::from_bits(0xFFF8_0000_0000_0000), 1, pop);
        }
        if operation == 0 && (a.infinity() || b.infinity()) {
            return self.store_special(
                slot,
                crate::signed(
                    f64::INFINITY,
                    if a.infinity() {
                        a.negative()
                    } else {
                        b.negative()
                    },
                ),
                0,
                pop,
            );
        }
        if operation == 1 && (a.infinity() || b.infinity()) || operation == 6 && a.infinity() {
            return self.store_special(slot, crate::signed(f64::INFINITY, negative), 0, pop);
        }
        if operation == 6 && b.zero() {
            return self.store_special(slot, crate::signed(f64::INFINITY, negative), 4, pop);
        }
        if operation == 6 && b.infinity() {
            return self.store_special(slot, crate::signed(0., negative), 0, pop);
        }
        let a = a.finite.unwrap();
        let mut b = b.finite.unwrap();
        let result = match operation {
            0 => {
                b.negative = !b.negative;
                a.subtract(b, self.control)?
            }
            1 => a.multiply(b, self.control)?,
            _ => a.divide(b, self.control)?,
        };
        self.store_rounded(slot, result, pop)
    }
    pub(super) fn compare_number(
        &mut self,
        a: Number,
        b: Number,
        pops: u8,
    ) -> Result<(), ArithmeticError> {
        self.compare_operands(a, b, pops, false)
    }
    pub(super) fn compare_quiet(
        &mut self,
        a: Number,
        b: Number,
        pops: u8,
    ) -> Result<(), ArithmeticError> {
        self.compare_operands(a, b, pops, true)
    }
    fn compare_operands(
        &mut self,
        a: Number,
        b: Number,
        pops: u8,
        quiet: bool,
    ) -> Result<(), ArithmeticError> {
        let order = if a.nan() || b.nan() {
            if !quiet || a.invalid() || b.invalid() {
                self.status |= 1;
                if self.control & 1 == 0 {
                    self.status |= 0x8080;
                    return Err(ArithmeticError::UnmaskedException(1));
                }
            }
            None
        } else {
            if a.denormal || b.denormal {
                self.status |= 2;
                if self.control & 2 == 0 {
                    self.status |= 0x8080;
                    return Err(ArithmeticError::UnmaskedException(2));
                }
            }
            Some(match (a.finite, b.finite) {
                (Some(a), Some(b)) => a.compare(b),
                _ => {
                    use core::cmp::Ordering::*;
                    if a.negative() != b.negative() {
                        if a.negative() { Less } else { Greater }
                    } else {
                        let magnitude = a.infinity().cmp(&b.infinity());
                        if a.negative() {
                            magnitude.reverse()
                        } else {
                            magnitude
                        }
                    }
                }
            })
        };
        let condition = match order {
            None => 0x4500,
            Some(core::cmp::Ordering::Less) => 0x100,
            Some(core::cmp::Ordering::Equal) => 0x4000,
            _ => 0,
        };
        self.status = (self.status & !0x4700) | condition;
        for _ in 0..pops {
            self.pop().map_err(ArithmeticError::Stack)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn memory_and_register_zero_infinity_and_nan_rules_agree() {
        for operation in [0, 1, 4, 5, 6, 7] {
            for lhs in [0., -0., 1., -2., f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
                for rhs in [0., -0., 1., -2., f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
                    let mut memory = X87::new();
                    memory.push(lhs).unwrap();
                    memory.arithmetic_memory(rhs, operation).unwrap();
                    let mut register = X87::new();
                    register.push(rhs).unwrap();
                    register.push(lhs).unwrap();
                    register.arithmetic_register(1, operation).unwrap();
                    assert_eq!(
                        memory.store_extended().unwrap(),
                        register.store_extended().unwrap(),
                        "{operation} {lhs} {rhs}"
                    );
                    assert_eq!(memory.status() & 0x7FF, register.status() & 0x7FF);
                }
            }
        }
    }
    #[test]
    fn real_loads_report_source_denormals_and_quiet_signaling_nan_payloads() {
        for (bits, single) in [(1_u64, true), (1, false)] {
            let mut fpu = X87::new();
            fpu.load_real(bits, single).unwrap();
            assert_eq!(fpu.status() & 0x3F, 2);
            assert_ne!(fpu.st0().unwrap(), 0.);
            let mut fpu = X87::new();
            fpu.load_control(0x37D);
            assert_eq!(
                fpu.load_real(bits, single),
                Err(ArithmeticError::UnmaskedException(2))
            );
            assert_eq!(fpu.tags(), 0xFFFF);
        }
        let mut fpu = X87::new();
        fpu.load_real(0x7F80_0001, true).unwrap();
        assert_eq!(fpu.status() & 0x3F, 1);
        assert_eq!(fpu.raw(7), (0x7FFF_u128 << 64) | (3 << 62) | (1 << 40));
        fpu.initialize();
        fpu.load_real(0x7FF0_0000_0000_0001, false).unwrap();
        assert_eq!(fpu.raw(7), (0x7FFF_u128 << 64) | (3 << 62) | (1 << 11));
    }
    #[test]
    fn quiet_comparisons_distinguish_signaling_and_quiet_real80_nans() {
        for signaling in [false, true] {
            let raw = (0x7FFF_u128 << 64) | (1 << 63) | if signaling { 1 } else { (1 << 62) | 1 };
            let storage = raw.to_le_bytes();
            let bytes = core::array::from_fn(|i| storage[i]);
            let mut fpu = X87::new();
            fpu.push(1.).unwrap();
            fpu.load_extended(bytes).unwrap();
            fpu.unordered_compare(1, 2).unwrap();
            assert_eq!(fpu.status() & 0x4501, 0x4500 | u16::from(signaling));
            assert_eq!(fpu.tags(), 0xFFFF);
        }
    }
    #[test]
    fn native_unmasked_precision_probe_completes_faddp_before_reporting() {
        let mut fpu = X87::new();
        fpu.push(1.).unwrap();
        fpu.push(1. / 3.).unwrap();
        fpu.load_control(0x005F);
        assert_eq!(
            fpu.add_pop(1),
            Err(ArithmeticError::UnmaskedException(0x20))
        );
        assert_eq!(fpu.status(), 0xBAA0);
        assert_eq!(fpu.raw(7), (0x3FFF_u128 << 64) | 0xAAAA_AB00_0000_0000);
        assert_eq!(fpu.st(1), Err(crate::StackError::Empty));
    }
}
