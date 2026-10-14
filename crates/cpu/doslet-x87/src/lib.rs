//! Independent x87 stack/control state and selected exact division forms.
#![no_std]
#[derive(Debug)]
pub struct X87 {
    control: u16,
    status: u16,
    tags: u16,
    values: [f64; 8],
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
    /// Extended precision, rounding or a nonfinite operand needs further support.
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
    pub fn duplicate(&mut self, index: u8) -> Result<(), StackError> {
        self.push(self.st(index)?)
    }
    pub fn change_sign(&mut self) -> Result<(), StackError> {
        self.st0()?;
        let slot = ((self.status >> 11) & 7) as usize;
        self.values[slot] = -self.values[slot];
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
        } else if lhs < rhs {
            0x100
        } else if lhs == rhs {
            0x4000
        } else {
            0
        };
        self.status = (self.status & !0x4700) | condition;
        self.pop().map_err(ArithmeticError::Stack)?;
        self.pop().map_err(ArithmeticError::Stack)
    }
    /// FDIVP ST(i), ST(0). Exact finite quotients and masked zero/invalid
    /// results are supported. Inexact/extended-range arithmetic stops explicitly.
    pub fn divide_pop(&mut self, index: u8, reverse: bool) -> Result<(), ArithmeticError> {
        let source = self.st0().map_err(ArithmeticError::Stack)?;
        let destination = self.st(index).map_err(ArithmeticError::Stack)?;
        let (numerator, denominator) = if reverse {
            (source, destination)
        } else {
            (destination, source)
        };
        if numerator.is_nan() || denominator.is_nan() {
            return Err(ArithmeticError::Unsupported);
        }
        let negative = numerator.is_sign_negative() ^ denominator.is_sign_negative();
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
            let value = numerator / denominator;
            let precision = match (self.control >> 8) & 3 {
                0 => 24,
                2 => 53,
                3 => 64,
                _ => return Err(ArithmeticError::Unsupported),
            };
            if !value.is_finite()
                || value == 0.0
                || !exact_quotient(numerator, denominator, value)
                || significant_bits(value) > precision
            {
                return Err(ArithmeticError::Unsupported);
            }
            (value, 0)
        };
        self.status = (self.status & !0x200) | exception; // C1=0, sticky exceptions.
        if exception & !self.control != 0 {
            self.status |= 0x8080; // ES/B, without changing the stack or destination.
            return Err(ArithmeticError::UnmaskedException(exception));
        }
        let slot = (((self.status >> 11) + u16::from(index)) & 7) as usize;
        let tag = if value == 0.0 {
            1
        } else if value.is_finite() {
            0
        } else {
            2
        };
        self.values[slot] = value;
        self.tags = (self.tags & !(3 << (slot * 2))) | (tag << (slot * 2));
        self.pop().map_err(ArithmeticError::Stack)
    }
    pub fn pop(&mut self) -> Result<(), StackError> {
        self.st0()?;
        let top = ((self.status >> 11) & 7) as usize;
        self.tags |= 3 << (top * 2);
        self.status = (self.status & !0x3800) | ((((top + 1) & 7) as u16) << 11);
        Ok(())
    }
}
fn signed(value: f64, negative: bool) -> f64 {
    if negative { -value } else { value }
}
fn parts(value: f64) -> (u128, i32) {
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7FF) as i32;
    let significand = (bits & 0xF_FFFF_FFFF_FFFF) | if exponent == 0 { 0 } else { 1 << 52 };
    let shift = significand.trailing_zeros();
    (
        u128::from(significand >> shift),
        exponent.max(1) - 1075 + shift as i32,
    )
}
fn significant_bits(value: f64) -> u32 {
    128 - parts(value).0.leading_zeros()
}
fn exact_quotient(numerator: f64, denominator: f64, quotient: f64) -> bool {
    let (n, ne) = parts(numerator);
    let (d, de) = parts(denominator);
    let (q, qe) = parts(quotient);
    let product = d * q;
    let shift = product.trailing_zeros();
    n == product >> shift && ne == de + qe + shift as i32
}
#[cfg(test)]
mod tests {
    use super::*;
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
        fpu.push(1.0).unwrap();
        fpu.push(3.0).unwrap();
        assert_eq!(fpu.divide_pop(1, false), Err(ArithmeticError::Unsupported));
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
        assert_eq!(fpu.divide_pop(1, false), Err(ArithmeticError::Unsupported));
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
