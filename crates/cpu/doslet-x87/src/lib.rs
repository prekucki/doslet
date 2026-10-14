//! Independent x87 stack/control state and selected finite arithmetic forms.
#![no_std]
mod finite;
use finite::{Finite, Rounded};
#[derive(Debug)]
pub struct X87 {
    control: u16,
    status: u16,
    tags: u16,
    values: [f64; 8],
    finite: [Option<Finite>; 8],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackError {
    Overflow,
    Empty,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithmeticError {
    Stack(StackError),
    UnmaskedException(u16),
    /// Unsupported exponent range, special operand or conversion.
    Unsupported,
}
impl Default for X87 {
    fn default() -> Self {
        Self::new()
    }
}
impl X87 {
    pub const fn new() -> Self {
        Self {
            control: 0x037F,
            status: 0,
            tags: 0xFFFF,
            values: [0.0; 8],
            finite: [None; 8],
        }
    }
    pub fn initialize(&mut self) {
        *self = Self::new();
    }
    pub fn control(&self) -> u16 {
        self.control
    }
    pub fn status(&self) -> u16 {
        self.status
    }
    pub fn tags(&self) -> u16 {
        self.tags
    }
    pub fn load_control(&mut self, value: u16) {
        self.control = value;
    }
    pub fn clear_exceptions(&mut self) {
        self.status &= !0x80FF;
    }
    pub fn push(&mut self, value: f64) -> Result<(), StackError> {
        let top = ((self.status >> 11).wrapping_sub(1) & 7) as usize;
        if (self.tags >> (top * 2)) & 3 != 3 {
            self.status |= 0x241; // IE, SF, C1; exception delivery is pending.
            return Err(StackError::Overflow);
        }
        self.status = (self.status & !0x3800) | ((top as u16) << 11);
        let tag = if value == 0.0 {
            1
        } else if value.is_finite() {
            0
        } else {
            2
        };
        self.tags = (self.tags & !(3 << (top * 2))) | (tag << (top * 2));
        self.values[top] = value;
        self.finite[top] = Finite::from_f64(value);
        Ok(())
    }
    pub fn st0(&self) -> Result<f64, StackError> {
        self.st(0)
    }
    pub fn st(&self, index: u8) -> Result<f64, StackError> {
        let slot = (((self.status >> 11) + u16::from(index)) & 7) as usize;
        if index > 7 || (self.tags >> (slot * 2)) & 3 == 3 {
            return Err(StackError::Empty);
        }
        Ok(self.values[slot])
    }
    /// Exact binary64 conversion for the currently supported memory-store forms.
    pub fn store_value(&self) -> Result<f64, ArithmeticError> {
        let value = self.st0().map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        if let Some(finite) = self.finite[top]
            && !finite.compare(Finite::from_f64(value).unwrap()).is_eq()
        {
            return Err(ArithmeticError::Unsupported);
        }
        Ok(value)
    }
    pub fn duplicate(&mut self, index: u8) -> Result<(), StackError> {
        let value = self.st(index)?;
        let slot = (((self.status >> 11) + u16::from(index)) & 7) as usize;
        let finite = self.finite[slot];
        self.push(value)?;
        self.finite[((self.status >> 11) & 7) as usize] = finite;
        Ok(())
    }
    /// FXCH exchanges values and tags without changing TOP.
    pub fn exchange(&mut self, index: u8) -> Result<(), StackError> {
        self.st0()?;
        self.st(index)?;
        let top = ((self.status >> 11) & 7) as usize;
        let slot = (top + usize::from(index)) & 7;
        let top_tag = (self.tags >> (top * 2)) & 3;
        let slot_tag = (self.tags >> (slot * 2)) & 3;
        self.values.swap(top, slot);
        self.finite.swap(top, slot);
        self.tags = (self.tags & !((3 << (top * 2)) | (3 << (slot * 2))))
            | (slot_tag << (top * 2))
            | (top_tag << (slot * 2));
        self.status &= !0x200;
        Ok(())
    }
    pub fn change_sign(&mut self) -> Result<(), StackError> {
        self.st0()?;
        let slot = ((self.status >> 11) & 7) as usize;
        self.values[slot] = -self.values[slot];
        if let Some(value) = &mut self.finite[slot] {
            value.negative = !value.negative;
        }
        self.status &= !0x200;
        Ok(())
    }
    /// FCOMPP uses IEEE ordering, including the 387's signed infinities.
    pub fn compare_pop_twice(&mut self) -> Result<(), ArithmeticError> {
        let lhs = self.st0().map_err(ArithmeticError::Stack)?;
        let rhs = self.st(1).map_err(ArithmeticError::Stack)?;
        let condition = if lhs.is_nan() || rhs.is_nan() {
            self.status |= 1;
            if self.control & 1 == 0 {
                self.status |= 0x8080;
                return Err(ArithmeticError::UnmaskedException(1));
            }
            0x4500
        } else if self.order(0, 1).is_lt() {
            0x100
        } else if self.order(0, 1).is_eq() {
            0x4000
        } else {
            0
        };
        self.status = (self.status & !0x4700) | condition;
        self.pop().map_err(ArithmeticError::Stack)?;
        self.pop().map_err(ArithmeticError::Stack)
    }
    fn order(&self, left: u8, right: u8) -> core::cmp::Ordering {
        let top = ((self.status >> 11) & 7) as usize;
        let lhs = (top + usize::from(left)) & 7;
        let rhs = (top + usize::from(right)) & 7;
        match (self.finite[lhs], self.finite[rhs]) {
            (Some(a), Some(b)) => a.compare(b),
            _ => self.values[lhs].partial_cmp(&self.values[rhs]).unwrap(),
        }
    }
    /// FDIVP ST(i), ST(0), with guest precision and rounding control.
    pub fn divide_pop(&mut self, index: u8, reverse: bool) -> Result<(), ArithmeticError> {
        self.divide_registers(index, 0, reverse, true)
    }
    /// FDIV/FDIVR ST(0), ST(i), with the same exponent limits as FDIVP.
    pub fn divide(&mut self, index: u8, reverse: bool) -> Result<(), ArithmeticError> {
        self.divide_registers(0, index, reverse, false)
    }
    fn divide_registers(
        &mut self,
        destination_index: u8,
        source_index: u8,
        reverse: bool,
        pop: bool,
    ) -> Result<(), ArithmeticError> {
        let source = self.st(source_index).map_err(ArithmeticError::Stack)?;
        let destination = self.st(destination_index).map_err(ArithmeticError::Stack)?;
        let (numerator, denominator) = if reverse {
            (source, destination)
        } else {
            (destination, source)
        };
        if numerator.is_nan() || denominator.is_nan() {
            return Err(ArithmeticError::Unsupported);
        }
        let negative = numerator.is_sign_negative() ^ denominator.is_sign_negative();
        let destination_slot = (((self.status >> 11) + u16::from(destination_index)) & 7) as usize;
        let source_slot = (((self.status >> 11) + u16::from(source_index)) & 7) as usize;
        if numerator != 0.0
            && denominator != 0.0
            && numerator.is_finite()
            && denominator.is_finite()
        {
            let destination = self.finite[destination_slot].unwrap();
            let source = self.finite[source_slot].unwrap();
            let result = if reverse {
                source.divide(destination, self.control)?
            } else {
                destination.divide(source, self.control)?
            };
            return self.store_rounded(destination_slot, result, pop);
        }
        let (value, exception) = if (numerator == 0.0 && denominator == 0.0)
            || (numerator.is_infinite() && denominator.is_infinite())
        {
            (f64::from_bits(0xFFF8_0000_0000_0000), 1)
        } else if denominator == 0.0 && numerator.is_finite() {
            (signed(f64::INFINITY, negative), 4)
        } else if numerator.is_infinite() {
            (signed(f64::INFINITY, negative), 0)
        } else if denominator.is_infinite() || numerator == 0.0 {
            (signed(0.0, negative), 0)
        } else {
            return Err(ArithmeticError::Unsupported);
        };
        self.status = (self.status & !0x200) | exception; // C1=0, sticky exceptions.
        if exception & !self.control != 0 {
            self.status |= 0x8080; // ES/B, without changing the stack or destination.
            return Err(ArithmeticError::UnmaskedException(exception));
        }
        let slot = (((self.status >> 11) + u16::from(destination_index)) & 7) as usize;
        let tag = if value == 0.0 {
            1
        } else if value.is_finite() {
            0
        } else {
            2
        };
        self.values[slot] = value;
        self.finite[slot] = Finite::from_f64(value);
        self.tags = (self.tags & !(3 << (slot * 2))) | (tag << (slot * 2));
        if pop {
            self.pop().map_err(ArithmeticError::Stack)?;
        }
        Ok(())
    }
    fn store_rounded(
        &mut self,
        slot: usize,
        result: Rounded,
        pop: bool,
    ) -> Result<(), ArithmeticError> {
        let exception = if result.inexact { 0x20 } else { 0 };
        self.status = (self.status & !0x200) | exception | if result.up { 0x200 } else { 0 };
        if exception & !self.control != 0 {
            self.status |= 0x8080;
            return Err(ArithmeticError::UnmaskedException(exception));
        }
        self.values[slot] = result.value.to_f64();
        self.finite[slot] = Some(result.value);
        let tag = u16::from(result.value.mantissa == 0);
        self.tags = (self.tags & !(3 << (slot * 2))) | (tag << (slot * 2));
        if pop {
            self.pop().map_err(ArithmeticError::Stack)?;
        }
        Ok(())
    }
    /// FMULP or FSUBP/FSUBRP ST(i), ST(0), for finite normal results.
    pub fn arithmetic_pop(
        &mut self,
        index: u8,
        multiply: bool,
        reverse: bool,
    ) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        self.st(index).map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        let slot = (top + usize::from(index)) & 7;
        let source = self.finite[top].ok_or(ArithmeticError::Unsupported)?;
        let destination = self.finite[slot].ok_or(ArithmeticError::Unsupported)?;
        let result = if multiply {
            destination.multiply(source, self.control)?
        } else if reverse {
            source.subtract(destination, self.control)?
        } else {
            destination.subtract(source, self.control)?
        };
        self.store_rounded(slot, result, true)
    }
    /// FCOMP with a binary32/64 memory operand.
    pub fn compare_memory_pop(&mut self, rhs: f64) -> Result<(), ArithmeticError> {
        let lhs = self.st0().map_err(ArithmeticError::Stack)?;
        if !lhs.is_finite() || !rhs.is_finite() {
            return Err(ArithmeticError::Unsupported);
        }
        let top = ((self.status >> 11) & 7) as usize;
        let order = self.finite[top]
            .unwrap()
            .compare(Finite::from_f64(rhs).unwrap());
        let condition = if order.is_lt() {
            0x100
        } else if order.is_eq() {
            0x4000
        } else {
            0
        };
        self.status = (self.status & !0x4700) | condition;
        self.pop().map_err(ArithmeticError::Stack)
    }
    pub fn pop(&mut self) -> Result<(), StackError> {
        self.st0()?;
        let top = ((self.status >> 11) & 7) as usize;
        self.tags |= 3 << (top * 2);
        self.finite[top] = None;
        self.status = (self.status & !0x3800) | ((((top + 1) & 7) as u16) << 11);
        Ok(())
    }
}
fn signed(value: f64, negative: bool) -> f64 {
    if negative { -value } else { value }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pentium_fdiv_probe_retains_extended_intermediate_precision() {
        let mut fpu = X87::new();
        fpu.push(4_195_835.0).unwrap();
        fpu.duplicate(0).unwrap();
        fpu.push(3_145_727.0).unwrap();
        fpu.exchange(1).unwrap();
        fpu.divide(1, false).unwrap();
        // The quotient cannot be stored exactly as binary64, but must retain
        // its 64-bit significand through duplication and arithmetic.
        assert_eq!(fpu.store_value(), Err(ArithmeticError::Unsupported));
        fpu.duplicate(0).unwrap();
        fpu.pop().unwrap();
        fpu.arithmetic_pop(1, true, false).unwrap();
        fpu.arithmetic_pop(1, false, false).unwrap();
        assert_eq!(fpu.st0().unwrap().to_bits(), 0); // Matches native x87.
        fpu.compare_memory_pop(1.0).unwrap();
        assert_eq!(fpu.status() & 0x4500, 0x100);
        assert_eq!(fpu.tags(), 0xFFFF);
    }
    #[test]
    fn exchange_wraps_physical_stack_and_moves_tags_with_values() {
        let mut fpu = X87::new();
        for value in [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, f64::INFINITY] {
            fpu.push(value).unwrap();
        }
        fpu.status = 7 << 11; // ST(1) wraps from physical slot seven to zero.
        fpu.status |= 0x4700;
        let status = fpu.status();
        fpu.exchange(1).unwrap();
        assert_eq!(fpu.st0(), Ok(f64::INFINITY));
        assert_eq!(fpu.st(1), Ok(0.0));
        assert_eq!(fpu.tags() & 3, 1);
        assert_eq!((fpu.tags() >> 14) & 3, 2);
        assert_eq!(fpu.status(), status & !0x200);
        let tags = fpu.tags();
        fpu.exchange(0).unwrap();
        assert_eq!(fpu.tags(), tags);
        assert_eq!(fpu.st0(), Ok(f64::INFINITY));
        fpu.pop().unwrap();
        fpu.exchange(7).unwrap_err(); // Empty wrapped slot must not change ST(0).
        assert_eq!(fpu.st0(), Ok(0.0));
    }
    #[test]
    fn register_division_preserves_top_and_source_for_both_directions() {
        for (reverse, expected) in [(false, 4.0), (true, 0.25)] {
            let mut fpu = X87::new();
            fpu.push(2.0).unwrap();
            fpu.push(8.0).unwrap();
            let status = fpu.status();
            let tags = fpu.tags();
            fpu.divide(1, reverse).unwrap();
            assert_eq!(fpu.st0(), Ok(expected));
            assert_eq!(fpu.st(1), Ok(2.0));
            assert_eq!(fpu.status(), status);
            assert_eq!(fpu.tags(), tags);
        }
    }
    #[test]
    fn exchange_empty_or_invalid_operand_preserves_state() {
        let mut fpu = X87::new();
        assert_eq!(fpu.exchange(1), Err(StackError::Empty));
        fpu.push(-0.0).unwrap();
        let tags = fpu.tags();
        let status = fpu.status();
        for index in [1, 7, 8, 255] {
            assert_eq!(fpu.exchange(index), Err(StackError::Empty));
            assert_eq!(fpu.tags(), tags);
            assert_eq!(fpu.status(), status);
            assert_eq!(fpu.st0().unwrap().to_bits(), (-0.0f64).to_bits());
        }
    }
    #[test]
    fn infinity_probe_orders_387_signed_infinities_and_empties_the_stack() {
        let mut fpu = X87::new();
        fpu.push(1.0).unwrap();
        fpu.push(0.0).unwrap();
        fpu.divide_pop(1, false).unwrap();
        assert_eq!(fpu.st0(), Ok(f64::INFINITY));
        assert_eq!(fpu.status() & 0xFF, 4);
        fpu.duplicate(0).unwrap();
        fpu.change_sign().unwrap();
        fpu.compare_pop_twice().unwrap();
        assert_eq!(fpu.status() & 0x4700, 0x100);
        assert_eq!(fpu.tags(), 0xFFFF);
        assert_eq!(fpu.st0(), Err(StackError::Empty));
    }
    #[test]
    fn division_exceptions_and_inexact_results_preserve_the_operand_stack() {
        let mut fpu = X87::new();
        fpu.load_control(0x037B); // Unmask division by zero.
        fpu.push(-1.0).unwrap();
        fpu.push(-0.0).unwrap();
        let tags = fpu.tags();
        assert_eq!(
            fpu.divide_pop(1, false),
            Err(ArithmeticError::UnmaskedException(4))
        );
        assert_eq!(fpu.tags(), tags);
        assert_eq!(fpu.status() & 0x8084, 0x8084);
        assert_eq!(fpu.st(1), Ok(-1.0));
        fpu.initialize();
        fpu.load_control(0x035F); // Unmask inexact result.
        fpu.push(1.0).unwrap();
        fpu.push(3.0).unwrap();
        assert_eq!(
            fpu.divide_pop(1, false),
            Err(ArithmeticError::UnmaskedException(0x20))
        );
        assert_eq!(fpu.st0(), Ok(3.0));
        assert_eq!(fpu.st(1), Ok(1.0));
        fpu.initialize();
        fpu.push(0.0).unwrap();
        fpu.push(0.0).unwrap();
        fpu.divide_pop(1, false).unwrap();
        assert_eq!(fpu.st0().unwrap().to_bits(), 0xFFF8_0000_0000_0000);
        assert_eq!(fpu.status() & 1, 1);
    }
    #[test]
    fn exact_division_honors_precision_without_host_rounding_assumptions() {
        let mut fpu = X87::new();
        fpu.push(-9.0).unwrap();
        fpu.push(1.5).unwrap();
        fpu.divide_pop(1, false).unwrap();
        assert_eq!(fpu.st0(), Ok(-6.0));
        assert_eq!(fpu.status() & 0xFF, 0);
        fpu.initialize();
        fpu.load_control(0x007F); // PC24 cannot exactly hold this quotient.
        fpu.push(16_777_217.0).unwrap();
        fpu.push(1.0).unwrap();
        fpu.divide_pop(1, false).unwrap();
        assert_eq!(fpu.st0(), Ok(16_777_216.0));
        assert_eq!(fpu.status() & 0x20, 0x20);
    }
    #[test]
    fn floating_constants_update_top_tags_and_empty_stack_state() {
        let mut fpu = X87::new();
        fpu.push(0.0).unwrap();
        assert_eq!(
            (fpu.st0().unwrap(), fpu.status(), fpu.tags()),
            (0.0, 0x3800, 0x7FFF)
        );
        fpu.push(1.0).unwrap();
        assert_eq!(fpu.st0(), Ok(1.0));
        fpu.pop().unwrap();
        assert_eq!(fpu.st0(), Ok(0.0));
        fpu.pop().unwrap();
        assert_eq!(fpu.st0(), Err(StackError::Empty));
        assert_eq!(fpu.tags(), 0xFFFF);
    }
    #[test]
    fn initialization_resets_control_status_and_empty_tags() {
        let mut fpu = X87::new();
        fpu.load_control(0x027F);
        fpu.status = 0x83FF;
        fpu.tags = 0;
        fpu.initialize();
        assert_eq!(
            (fpu.control(), fpu.status(), fpu.tags()),
            (0x037F, 0, 0xFFFF)
        );
    }
    #[test]
    fn clex_clears_exceptions_and_preserves_conditions_and_top() {
        let mut fpu = X87::new();
        fpu.status = 0xFFFF;
        fpu.clear_exceptions();
        assert_eq!(fpu.status(), 0x7F00);
    }
}
