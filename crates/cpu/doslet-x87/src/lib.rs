//! Independent 387 stack/control state and bounded portable numerical kernels.
#![no_std]
mod binary;
mod environment;
mod exponential;
mod extended;
mod finite;
mod logarithm;
mod operations;
mod transcendental;
mod wide;
use finite::{Finite, Rounded};
#[derive(Clone, Debug, PartialEq)]
pub struct X87 {
    control: u16,
    status: u16,
    tags: u16,
    values: [f64; 8],
    finite: [Option<Finite>; 8],
    encodings: [u128; 8],
    pointers: environment::Pointers,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackError {
    Overflow,
    Empty,
}
/// Constants loaded from the x87's internal constant table, rounded to 64
/// significand bits using RC. PC does not affect constant loads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Constant {
    One,
    Log2Ten,
    Log2E,
    Pi,
    Log10Two,
    LnTwo,
    Zero,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArithmeticError {
    Stack(StackError),
    UnmaskedException(u16),
    /// Unsupported numeric domain, special operand or exception completion.
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
            encodings: [0; 8],
            pointers: environment::Pointers::new(),
        }
    }
    pub fn initialize(&mut self) {
        // FINIT empties the stack by changing tags; it does not erase the
        // physical data registers. FNSAVE uses the same initialization.
        self.control = 0x037F;
        self.status = 0;
        self.tags = 0xFFFF;
        self.pointers = environment::Pointers::new();
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
        self.encodings[top] = extended::from_f64(value);
        Ok(())
    }
    pub fn st0(&self) -> Result<f64, StackError> {
        self.st(0)
    }
    /// FLD1/FLDL2T/FLDL2E/FLDPI/FLDLG2/FLDLN2/FLDZ. Rounding these constants
    /// does not raise precision or set C1, including with unmasked precision.
    pub fn load_constant(&mut self, constant: Constant) -> Result<(), StackError> {
        let (floor, exponent, nearest_up) = match constant {
            Constant::One => (0x8000_0000_0000_0000_u64, 0, false),
            Constant::Log2Ten => (0xD49A_784B_CD1B_8AFE, 1, false),
            Constant::Log2E => (0xB8AA_3B29_5C17_F0BB, 0, true),
            Constant::Pi => (0xC90F_DAA2_2168_C234, 1, true),
            Constant::Log10Two => (0x9A20_9A84_FBCF_F798, -2, true),
            Constant::LnTwo => (0xB172_17F7_D1CF_79AB, -1, true),
            Constant::Zero => (0, 0, false),
        };
        let inexact = !matches!(constant, Constant::One | Constant::Zero);
        let up = inexact
            && match (self.control >> 10) & 3 {
                0 => nearest_up,
                2 => true,
                _ => false, // All constants are nonnegative: down equals truncate.
            };
        let value = Finite {
            negative: false,
            exponent,
            mantissa: floor + u64::from(up),
        };
        self.push(value.to_f64())?;
        self.finite[((self.status >> 11) & 7) as usize] = Some(value);
        self.encodings[((self.status >> 11) & 7) as usize] = extended::encode(value);
        self.status &= !0x200;
        Ok(())
    }
    /// FILD converts all signed integer bits exactly, independently of PC/RC.
    pub fn push_integer(&mut self, value: i64) -> Result<(), StackError> {
        let finite = Finite::from_i64(value);
        self.push(finite.to_f64())?;
        self.finite[((self.status >> 11) & 7) as usize] = Some(finite);
        self.encodings[((self.status >> 11) & 7) as usize] = extended::encode(finite);
        self.status &= !0x200;
        Ok(())
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
            && !Finite::from_f64(value).is_some_and(|v| finite.compare(v).is_eq())
        {
            return Err(ArithmeticError::Unsupported);
        }
        Ok(value)
    }
    /// FST/FSTP rounds to its memory format using RC, including range limits.
    /// PC does not select the precision of a memory store.
    pub fn store_float(&mut self, single: bool) -> Result<f64, ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        let slot = ((self.status >> 11) & 7) as usize;
        let Some(value) = self.finite[slot] else {
            return self.special_memory(slot);
        };
        let result = value.memory_round(single, self.control)?;
        let exception = result.exceptions | if result.inexact { 0x20 } else { 0 };
        self.status = (self.status & !0x200) | exception | if result.up { 0x200 } else { 0 };
        if exception & !self.control != 0 {
            self.status |= 0x8080;
            return Err(ArithmeticError::UnmaskedException(exception));
        }
        Ok(
            if result.value.mantissa != 0 && result.value.exponent > if single { 127 } else { 1023 }
            {
                signed(f64::INFINITY, result.value.negative)
            } else {
                result.value.to_f64()
            },
        )
    }
    /// Basic arithmetic with a binary64 memory source.
    pub fn arithmetic_memory(&mut self, value: f64, operation: u8) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        self.binary(
            top,
            self.number(top),
            binary::Number::from_f64(value),
            operation,
            false,
        )
    }
    pub fn arithmetic_integer(&mut self, value: i64, operation: u8) -> Result<(), ArithmeticError> {
        self.arithmetic_finite(Finite::from_i64(value), operation)
    }
    pub fn arithmetic_register(&mut self, index: u8, operation: u8) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        self.st(index).map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        self.binary(
            top,
            self.number(top),
            self.number((top + usize::from(index)) & 7),
            operation,
            false,
        )
    }
    fn arithmetic_finite(&mut self, rhs: Finite, operation: u8) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        self.binary(
            top,
            self.number(top),
            binary::Number::from_finite(rhs),
            operation,
            false,
        )
    }
    pub fn round_integer(&mut self) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        let slot = ((self.status >> 11) & 7) as usize;
        let Some(value) = self.finite[slot] else {
            let number = self.number(slot);
            return self.binary(
                slot,
                number,
                binary::Number::from_finite(Finite::from_i64(0)),
                0,
                false,
            );
        };
        self.store_rounded(slot, value.integer_round(self.control), false)
    }
    /// FYL2X: ST(1) * log2(ST(0)), followed by one pop on successful completion.
    /// Normal finite inputs use bounded integer range reduction. Nonfinite
    /// operands remain explicit until the general special-value model exists.
    pub fn y_log2_x(&mut self) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        self.st(1).map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        let slot = (top + 1) & 7;
        let x = self.finite[top].ok_or(ArithmeticError::Unsupported)?;
        let y = self.finite[slot].ok_or(ArithmeticError::Unsupported)?;
        if x.mantissa == 0 || x.negative {
            let invalid = x.mantissa != 0 || y.mantissa == 0;
            let value = if invalid {
                f64::from_bits(0xFFF8_0000_0000_0000)
            } else {
                signed(f64::INFINITY, !y.negative)
            };
            return self.store_special(slot, value, if invalid { 1 } else { 4 }, true);
        }
        let result = logarithm::multiply_log2(x, y, self.control)?;
        self.store_rounded(slot, result, true)
    }
    /// F2XM1: 2^ST(0)-1 on the documented [-1, 1] input domain.
    pub fn exp2_minus_one(&mut self) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        let slot = ((self.status >> 11) & 7) as usize;
        let x = self.finite[slot].ok_or(ArithmeticError::Unsupported)?;
        let result = exponential::exp2_minus_one(x, self.control)?;
        self.store_rounded(slot, result, false)
    }
    /// FSCALE preserves ST(1) and the full ST(0) significand. The scale is
    /// truncated toward zero independently of RC; PC does not round the result.
    pub fn scale(&mut self) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        self.st(1).map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        let mut value = self.finite[top].ok_or(ArithmeticError::Unsupported)?;
        let scale = self.finite[(top + 1) & 7].ok_or(ArithmeticError::Unsupported)?;
        self.check_denormals(&[top, (top + 1) & 7])?;
        if value.mantissa != 0 && scale.mantissa != 0 && scale.exponent >= 0 {
            let magnitude = if scale.exponent >= 16 {
                65536
            } else {
                (scale.mantissa >> (63 - scale.exponent)) as i32
            };
            value.exponent += if scale.negative {
                -magnitude
            } else {
                magnitude
            };
        }
        let rounded = finite::round(
            value.mantissa,
            value.exponent,
            value.negative,
            0,
            1,
            self.control | 0x300,
        )?;
        self.store_rounded(top, rounded, false)
    }
    /// FIST/FISTP returns the encoded signed integer, or masked integer indefinite.
    pub fn store_integer(&mut self, bits: u8) -> Result<u64, ArithmeticError> {
        if !matches!(bits, 16 | 32 | 64) {
            return Err(ArithmeticError::Unsupported);
        }
        self.st0().map_err(ArithmeticError::Stack)?;
        let slot = ((self.status >> 11) & 7) as usize;
        let rounded = self.finite[slot].map(|value| value.integer_round(self.control));
        let limit = 1_u64 << (bits - 1);
        let magnitude = rounded.and_then(|result| {
            let value = result.value;
            if value.mantissa == 0 {
                Some(0)
            } else if (0..=63).contains(&value.exponent) {
                Some(value.mantissa >> (63 - value.exponent))
            } else {
                None
            }
        });
        let valid = rounded.zip(magnitude).filter(|(result, magnitude)| {
            *magnitude < limit || (*magnitude == limit && result.value.negative)
        });
        let (value, exception, up) = if let Some((result, magnitude)) = valid {
            (
                if result.value.negative {
                    magnitude.wrapping_neg()
                } else {
                    magnitude
                },
                if result.inexact { 0x20 } else { 0 },
                result.up,
            )
        } else {
            (limit, 1, false)
        };
        self.status = (self.status & !0x200) | exception | if up { 0x200 } else { 0 };
        if exception & !self.control != 0 {
            self.status |= 0x8080;
            return Err(ArithmeticError::UnmaskedException(exception));
        }
        Ok(value)
    }
    pub fn duplicate(&mut self, index: u8) -> Result<(), StackError> {
        let value = self.st(index)?;
        let slot = (((self.status >> 11) + u16::from(index)) & 7) as usize;
        let finite = self.finite[slot];
        let special = self.encodings[slot];
        self.push(value)?;
        self.encodings[((self.status >> 11) & 7) as usize] = special;
        self.finite[((self.status >> 11) & 7) as usize] = finite;
        self.status &= !0x200;
        Ok(())
    }
    /// FST/FSTP ST(i) copies the full retained value and tag without rounding.
    /// The destination may be empty. Read/validate the source before any write.
    pub fn store_register(&mut self, index: u8, pop: bool) -> Result<(), StackError> {
        if index > 7 {
            return Err(StackError::Empty);
        }
        let value = self.st0()?;
        let top = ((self.status >> 11) & 7) as usize;
        let destination = (top + usize::from(index)) & 7;
        let tag = (self.tags >> (top * 2)) & 3;
        self.values[destination] = value;
        self.finite[destination] = self.finite[top];
        self.encodings[destination] = self.encodings[top];
        self.tags = (self.tags & !(3 << (destination * 2))) | (tag << (destination * 2));
        self.status &= !0x200; // C1: no format conversion or rounding.
        if pop {
            self.pop()?;
        }
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
        self.encodings.swap(top, slot);
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
        self.encodings[slot] ^= 1_u128 << 79;
        if let Some(value) = &mut self.finite[slot] {
            value.negative = !value.negative;
        }
        self.status &= !0x200;
        Ok(())
    }
    pub fn absolute(&mut self) -> Result<(), StackError> {
        self.st0()?;
        let slot = ((self.status >> 11) & 7) as usize;
        self.values[slot] = self.values[slot].abs();
        self.encodings[slot] &= !(1_u128 << 79);
        if let Some(value) = &mut self.finite[slot] {
            value.negative = false;
        }
        self.status &= !0x200;
        Ok(())
    }
    /// FFREE only changes the tag, including an already empty register.
    pub fn free(&mut self, index: u8) {
        let slot = (((self.status >> 11) + u16::from(index & 7)) & 7) as usize;
        self.tags |= 3 << (slot * 2);
    }
    /// FINCSTP/FDECSTP rotate TOP without changing physical values or tags.
    pub fn rotate_top(&mut self, increment: bool) {
        let top = (self.status >> 11) & 7;
        let next = if increment {
            top.wrapping_add(1)
        } else {
            top.wrapping_sub(1)
        } & 7;
        self.status = (self.status & !0x3A00) | (next << 11);
    }
    /// FCOMPP uses IEEE ordering, including the 387's signed infinities.
    pub fn compare_pop_twice(&mut self) -> Result<(), ArithmeticError> {
        self.compare_register(1, 2)
    }
    /// FTST compares ST(0) with positive zero, preserving every stack value.
    /// As with FCOM, either kind of NaN raises invalid operation.
    pub fn test_zero(&mut self) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        self.compare_number(
            self.number(top),
            binary::Number::from_finite(Finite::from_i64(0)),
            0,
        )
    }
    /// Ordered FCOM/FCOMP/FCOMPP, retaining exact finite integer comparisons.
    pub fn compare_register(&mut self, index: u8, pops: u8) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        self.st(index).map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        self.compare_number(
            self.number(top),
            self.number((top + usize::from(index)) & 7),
            pops,
        )
    }
    /// FDIVP ST(i), ST(0), with guest precision and rounding control.
    pub fn divide_pop(&mut self, index: u8, reverse: bool) -> Result<(), ArithmeticError> {
        self.divide_registers(index, 0, reverse, true)
    }
    /// FDIV/FDIVR ST(0), ST(i), using retained extended operands.
    pub fn divide(&mut self, index: u8, reverse: bool) -> Result<(), ArithmeticError> {
        self.divide_registers(0, index, reverse, false)
    }
    pub fn divide_destination(&mut self, index: u8, reverse: bool) -> Result<(), ArithmeticError> {
        self.divide_registers(index, 0, reverse, false)
    }
    fn divide_registers(
        &mut self,
        destination_index: u8,
        source_index: u8,
        reverse: bool,
        pop: bool,
    ) -> Result<(), ArithmeticError> {
        self.st(source_index).map_err(ArithmeticError::Stack)?;
        self.st(destination_index).map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        let destination = (top + usize::from(destination_index)) & 7;
        self.binary(
            destination,
            self.number(destination),
            self.number((top + usize::from(source_index)) & 7),
            if reverse { 7 } else { 6 },
            pop,
        )
    }
    fn store_special(
        &mut self,
        slot: usize,
        value: f64,
        exception: u16,
        pop: bool,
    ) -> Result<(), ArithmeticError> {
        self.status = (self.status & !0x200) | exception; // C1=0, sticky exceptions.
        if exception & !self.control != 0 {
            self.status |= 0x8080; // ES/B, without changing the stack or destination.
            return Err(ArithmeticError::UnmaskedException(exception));
        }
        let tag = if value == 0.0 {
            1
        } else if value.is_finite() {
            0
        } else {
            2
        };
        self.values[slot] = value;
        self.finite[slot] = Finite::from_f64(value);
        self.encodings[slot] = extended::from_f64(value);
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
        let exception = result.exceptions | if result.inexact { 0x20 } else { 0 };
        self.status = (self.status & !0x200) | exception | if result.up { 0x200 } else { 0 };
        let pending = exception & !self.control;
        if pending != 0 {
            self.status |= 0x8080;
        }
        // Precision is a post-completion exception for a register destination.
        // Range traps still stop explicitly until wrapped-result delivery exists.
        if pending & !0x20 != 0 {
            return Err(ArithmeticError::UnmaskedException(exception));
        }
        let infinity = result.value.mantissa != 0 && result.value.exponent > 16383;
        self.values[slot] = if infinity {
            signed(f64::INFINITY, result.value.negative)
        } else {
            result.value.to_f64()
        };
        self.finite[slot] = if infinity { None } else { Some(result.value) };
        self.encodings[slot] = if infinity {
            extended::from_f64(self.values[slot])
        } else {
            extended::encode(result.value)
        };
        let tag = if infinity || (result.value.mantissa != 0 && result.value.exponent < -16382) {
            2
        } else {
            u16::from(result.value.mantissa == 0)
        };
        self.tags = (self.tags & !(3 << (slot * 2))) | (tag << (slot * 2));
        if pop {
            self.pop().map_err(ArithmeticError::Stack)?;
        }
        if pending != 0 {
            Err(ArithmeticError::UnmaskedException(exception))
        } else {
            Ok(())
        }
    }
    /// FADDP ST(i), ST(0), retaining extended integer precision.
    pub fn add_pop(&mut self, index: u8) -> Result<(), ArithmeticError> {
        self.add_destination(index, true)
    }
    /// FADD ST(i), ST(0), retaining TOP and the source register.
    pub fn add_register(&mut self, index: u8) -> Result<(), ArithmeticError> {
        self.add_destination(index, false)
    }
    fn add_destination(&mut self, index: u8, pop: bool) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        self.st(index).map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        let slot = (top + usize::from(index)) & 7;
        self.binary(slot, self.number(slot), self.number(top), 0, pop)
    }
    /// FMULP or FSUBP/FSUBRP ST(i), ST(0).
    pub fn arithmetic_pop(
        &mut self,
        index: u8,
        multiply: bool,
        reverse: bool,
    ) -> Result<(), ArithmeticError> {
        self.arithmetic_destination(index, multiply, reverse, true)
    }
    /// FMUL/FSUB/FSUBR with ST(i) destination, optionally popping ST(0).
    pub fn arithmetic_destination(
        &mut self,
        index: u8,
        multiply: bool,
        reverse: bool,
        pop: bool,
    ) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        self.st(index).map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        let slot = (top + usize::from(index)) & 7;
        self.binary(
            slot,
            self.number(slot),
            self.number(top),
            if multiply {
                1
            } else if reverse {
                5
            } else {
                4
            },
            pop,
        )
    }
    /// FCOMP with a binary32/64 memory operand.
    pub fn compare_memory_pop(&mut self, rhs: f64) -> Result<(), ArithmeticError> {
        self.compare_memory(rhs, true)
    }
    pub fn compare_memory(&mut self, rhs: f64, pop: bool) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        self.compare_number(
            self.number(top),
            binary::Number::from_f64(rhs),
            u8::from(pop),
        )
    }
    pub fn compare_integer(&mut self, rhs: i64, pop: bool) -> Result<(), ArithmeticError> {
        self.compare_finite(Finite::from_i64(rhs), pop)
    }
    fn compare_finite(&mut self, rhs: Finite, pop: bool) -> Result<(), ArithmeticError> {
        self.st0().map_err(ArithmeticError::Stack)?;
        let top = ((self.status >> 11) & 7) as usize;
        self.compare_number(
            self.number(top),
            binary::Number::from_finite(rhs),
            u8::from(pop),
        )
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
    fn fscale_matches_native_retained_bits_and_truncates_independently_of_pc_rc() {
        let mut cases = 0;
        for line in include_str!("native_scales.txt")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let mut fields = line
                .split_whitespace()
                .map(|field| u64::from_str_radix(field, 16).unwrap());
            let control = fields.next().unwrap() as u16;
            let x_mantissa = fields.next().unwrap();
            let x_sign_exponent = fields.next().unwrap();
            let y = f64::from_bits(fields.next().unwrap());
            let mantissa = fields.next().unwrap();
            let sign_exponent = fields.next().unwrap();
            let status = fields.next().unwrap() as u16;
            let x = Finite {
                negative: x_sign_exponent & 0x8000 != 0,
                exponent: (x_sign_exponent & 0x7FFF) as i32 - 16383,
                mantissa: x_mantissa,
            };
            let mut fpu = X87::new();
            fpu.load_control(control);
            fpu.push(y).unwrap();
            fpu.push(if x.mantissa == 0 {
                signed(0., x.negative)
            } else {
                x.to_f64()
            })
            .unwrap();
            fpu.finite[6] = Some(x);
            fpu.scale().unwrap();
            let actual = fpu.finite[6].unwrap();
            assert_eq!(actual.mantissa, mantissa, "{line}");
            assert_eq!(actual.negative, sign_exponent & 0x8000 != 0);
            if mantissa != 0 {
                assert_eq!(actual.exponent, (sign_exponent & 0x7FFF) as i32 - 16383);
            }
            assert_eq!(fpu.status(), status);
            assert_eq!(fpu.st(1), Ok(y));
            cases += 1;
        }
        assert_eq!(cases, 300);
    }
    #[test]
    fn fscale_handles_overflow_underflow_and_preserves_signed_zero() {
        for scale in [-1e20, 1e20] {
            let mut fpu = X87::new();
            fpu.push(scale).unwrap();
            fpu.push(1.).unwrap();
            fpu.scale().unwrap();
            assert_eq!(fpu.st(1), Ok(scale));
            assert_eq!(fpu.status() & 0x38, if scale < 0. { 0x30 } else { 0x28 });
        }
        let mut fpu = X87::new();
        fpu.push(1e20).unwrap();
        fpu.push(-0.).unwrap();
        fpu.scale().unwrap();
        assert_eq!(fpu.st0().unwrap().to_bits(), (-0.0_f64).to_bits());
    }
    #[test]
    fn f2xm1_matches_native_values_including_tiny_inputs_without_cancellation() {
        let mut cases = 0;
        for line in include_str!("native_exponentials.txt")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let mut fields = line
                .split_whitespace()
                .map(|field| u64::from_str_radix(field, 16).unwrap());
            let control = fields.next().unwrap() as u16;
            let x = f64::from_bits(fields.next().unwrap());
            let mantissa = fields.next().unwrap();
            let sign_exponent = fields.next().unwrap();
            let status = fields.next().unwrap() as u16;
            let mut fpu = X87::new();
            fpu.load_control(control);
            fpu.push(x).unwrap();
            fpu.exp2_minus_one().unwrap();
            let actual = fpu.finite[7].unwrap();
            assert_eq!(actual.negative, sign_exponent & 0x8000 != 0);
            if mantissa == 0 {
                assert_eq!(actual.mantissa, 0);
            } else {
                let rank = |exponent: u64, significand: u64| {
                    (u128::from(exponent) << 63) + u128::from(significand & !(1 << 63))
                };
                assert!(
                    rank((actual.exponent + 16383) as u64, actual.mantissa)
                        .abs_diff(rank(sign_exponent & 0x7FFF, mantissa))
                        <= 2,
                    "{line}: {:016X}",
                    actual.mantissa
                );
            }
            assert_eq!(fpu.status() & 0x3820, status & 0x3820);
            cases += 1;
        }
        assert_eq!(cases, 144);
    }
    #[test]
    fn f2xm1_rejects_out_of_domain_and_completes_unmasked_precision() {
        for value in [-2., 2., f64::INFINITY, f64::NAN] {
            let mut fpu = X87::new();
            fpu.push(value).unwrap();
            let status = fpu.status();
            assert_eq!(fpu.exp2_minus_one(), Err(ArithmeticError::Unsupported));
            assert_eq!(fpu.st0().unwrap().to_bits(), value.to_bits());
            assert_eq!(fpu.status(), status);
        }
        let mut fpu = X87::new();
        fpu.push(0.5).unwrap();
        fpu.load_control(0x035F);
        assert_eq!(
            fpu.exp2_minus_one(),
            Err(ArithmeticError::UnmaskedException(0x20))
        );
        assert!((fpu.st0().unwrap() - (core::f64::consts::SQRT_2 - 1.)).abs() < 1e-15);
        assert_eq!(fpu.status() & 0x3800, 0x3800);
    }
    #[test]
    fn audit_frndint_upper_binade_preserves_the_exact_value() {
        let mut fpu = X87::new();
        fpu.push(f64::MAX).unwrap();
        let status = fpu.status();
        let tags = fpu.tags();
        assert_eq!(fpu.round_integer(), Ok(()));
        assert_eq!(fpu.st0(), Ok(f64::MAX));
        assert_eq!(fpu.status(), status);
        assert_eq!(fpu.tags(), tags);
    }
    #[test]
    fn audit_fld_register_clears_c1_but_preserves_precision_status() {
        let mut fpu = X87::new();
        fpu.push(1.75).unwrap();
        assert_eq!(fpu.store_integer(16), Ok(2));
        assert_eq!(fpu.status(), 0x3A20);
        fpu.duplicate(0).unwrap();
        assert_eq!(fpu.status(), 0x3020); // Captured native FLD ST(0).
        assert_eq!(fpu.st0(), Ok(1.75));
        assert_eq!(fpu.st(1), Ok(1.75));
    }
    #[test]
    fn fyl2x_matches_native_values_within_two_extended_ulps() {
        let mut cases = 0;
        for line in include_str!("native_logarithms.txt")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let mut fields = line
                .split_whitespace()
                .map(|field| u64::from_str_radix(field, 16).unwrap());
            let control = fields.next().unwrap() as u16;
            let x = f64::from_bits(fields.next().unwrap());
            let y_mantissa = fields.next().unwrap();
            let y_sign_exponent = fields.next().unwrap();
            let expected_mantissa = fields.next().unwrap();
            let expected_sign_exponent = fields.next().unwrap();
            let expected_status = fields.next().unwrap() as u16;
            let y = Finite {
                negative: y_sign_exponent & 0x8000 != 0,
                exponent: (y_sign_exponent & 0x7FFF) as i32 - 16383,
                mantissa: y_mantissa,
            };
            let mut fpu = X87::new();
            fpu.load_control(control);
            fpu.push(y.to_f64()).unwrap();
            fpu.finite[7] = Some(y);
            fpu.push(x).unwrap();
            fpu.y_log2_x().unwrap();
            let actual = fpu.finite[7].unwrap();
            assert_eq!(
                actual.negative,
                expected_sign_exponent & 0x8000 != 0,
                "{line}"
            );
            if expected_mantissa != 0 {
                let rank = |exponent: u64, mantissa: u64| {
                    (u128::from(exponent) << 63) + u128::from(mantissa & !(1 << 63))
                };
                let expected_rank = rank(expected_sign_exponent & 0x7FFF, expected_mantissa);
                let actual_rank = rank((actual.exponent + 16383) as u64, actual.mantissa);
                assert!(
                    actual_rank.abs_diff(expected_rank) <= 2,
                    "{line}: {:016X} exp={}",
                    actual.mantissa,
                    actual.exponent
                );
            } else {
                assert_eq!(actual.mantissa, 0);
            }
            assert_eq!(fpu.status() & 0x3820, expected_status & 0x3820, "{line}");
            assert_eq!(
                fpu.tags(),
                if expected_mantissa == 0 {
                    0x7FFF
                } else {
                    0x3FFF
                }
            );
            cases += 1;
        }
        assert_eq!(cases, 540);
    }
    #[test]
    fn fyl2x_preserves_inputs_on_unmasked_exceptions_and_handles_zero_and_negative_domain() {
        for (x, y, exception) in [(0., 1., 4), (-1., 1., 1), (0., 0., 1)] {
            let mut fpu = X87::new();
            fpu.push(y).unwrap();
            fpu.push(x).unwrap();
            fpu.load_control(0x037F & !exception);
            let tags = fpu.tags();
            let values = fpu.values;
            assert_eq!(
                fpu.y_log2_x(),
                Err(ArithmeticError::UnmaskedException(exception))
            );
            assert_eq!(fpu.status() & 0x3800, 0x3000);
            assert_eq!(fpu.tags(), tags);
            assert_eq!(fpu.values, values);
        }
        for (x, y, negative, invalid) in [
            (0., 1., true, false),
            (-0., -1., false, false),
            (-1., 1., true, true),
            (0., 0., true, true),
        ] {
            let mut fpu = X87::new();
            fpu.push(y).unwrap();
            fpu.push(x).unwrap();
            fpu.y_log2_x().unwrap();
            assert_eq!(fpu.status() & 0x3800, 0x3800);
            assert_eq!(fpu.st0().unwrap().is_nan(), invalid);
            assert_eq!(fpu.st0().unwrap().is_sign_negative(), negative);
            assert_eq!(fpu.status() & 5, if invalid { 1 } else { 4 });
        }
    }
    #[test]
    fn constants_match_native_x87_at_every_rounding_and_precision_setting() {
        use Constant::*;
        for line in include_str!("native_constants.txt")
            .lines()
            .filter(|line| !line.starts_with('#'))
        {
            let mut fields = line
                .split_whitespace()
                .map(|field| u64::from_str_radix(field, 16).unwrap());
            let control = fields.next().unwrap() as u16;
            let opcode = fields.next().unwrap() as usize;
            let mantissa = fields.next().unwrap();
            let exponent = fields.next().unwrap() as i32;
            let status = fields.next().unwrap() as u16;
            for precision in [0, 0x100, 0x200, 0x300] {
                for precision_masked in [false, true] {
                    let mut fpu = X87::new();
                    let control =
                        (control & !0x320) | precision | if precision_masked { 0x20 } else { 0 };
                    fpu.load_control(control);
                    fpu.load_constant(
                        [One, Log2Ten, Log2E, Pi, Log10Two, LnTwo, Zero][opcode - 0xE8],
                    )
                    .unwrap();
                    let retained = fpu.finite[7].unwrap();
                    assert_eq!(retained.mantissa, mantissa, "{line}; CW={control:04X}");
                    if mantissa != 0 {
                        assert_eq!(retained.exponent + 16383, exponent);
                    }
                    assert!(!retained.negative);
                    assert_eq!(fpu.status(), status);
                    assert_eq!(fpu.tags(), if mantissa == 0 { 0x7FFF } else { 0x3FFF });
                    assert_eq!(fpu.control(), control);
                }
            }
        }
    }
    #[test]
    fn constants_clear_c1_preserve_other_conditions_and_do_not_overwrite_full_stack() {
        let mut fpu = X87::new();
        fpu.status = 0x4720;
        fpu.load_constant(Constant::LnTwo).unwrap();
        assert_eq!(fpu.status(), 0x7D20); // TOP=7, C1 cleared; C0/C2/C3/PE retained.
        for _ in 0..7 {
            fpu.push_integer(17).unwrap();
        }
        let tags = fpu.tags();
        let top = fpu.status() & 0x3800;
        let values = fpu.values;
        assert_eq!(fpu.load_constant(Constant::Pi), Err(StackError::Overflow));
        assert_eq!(fpu.tags(), tags);
        assert_eq!(fpu.status() & 0x3800, top);
        assert_eq!(fpu.values, values);
    }
    #[test]
    fn register_stores_copy_tags_wrap_top_and_do_not_write_on_empty_source() {
        for (value, tag) in [(0.0, 1), (-0.0, 1), (2.5, 0), (f64::INFINITY, 2)] {
            let mut fpu = X87::new();
            fpu.push(value).unwrap(); // Physical slot 7; ST(1) is empty slot 0.
            fpu.status |= 0x4700;
            let status = fpu.status();
            fpu.store_register(1, true).unwrap();
            assert_eq!(fpu.status(), status & !(0x3800 | 0x200));
            assert_eq!(fpu.tags(), 0xFFFC | tag);
            assert_eq!(fpu.st0().unwrap().to_bits(), value.to_bits());
            assert_eq!(fpu.st(7), Err(StackError::Empty));
            fpu.pop().unwrap();
            let status = fpu.status();
            let tags = fpu.tags();
            assert_eq!(fpu.store_register(3, true), Err(StackError::Empty));
            assert_eq!(fpu.status(), status);
            assert_eq!(fpu.tags(), tags);
        }
        let mut fpu = X87::new();
        fpu.push(4.0).unwrap();
        fpu.push(7.0).unwrap();
        fpu.store_register(1, false).unwrap();
        assert_eq!(fpu.st0(), Ok(7.0));
        assert_eq!(fpu.st(1), Ok(7.0));
    }
    #[test]
    fn integer_stores_check_signed_limits_rounding_and_masked_invalid_results() {
        for bits in [16, 32, 64] {
            let min = if bits == 64 {
                i64::MIN
            } else {
                -(1_i64 << (bits - 1))
            };
            let max = if bits == 64 {
                i64::MAX
            } else {
                (1_i64 << (bits - 1)) - 1
            };
            for value in [0, min, max, -1] {
                let mut fpu = X87::new();
                fpu.push_integer(value).unwrap();
                assert_eq!(fpu.store_integer(bits), Ok(value as u64));
                assert_eq!(fpu.status() & 0x21, 0);
            }
            let mut fpu = X87::new();
            fpu.push((1_u64 << (bits - 1)) as f64).unwrap();
            assert_eq!(fpu.store_integer(bits), Ok(1_u64 << (bits - 1)));
            assert_eq!(fpu.status() & 1, 1);
            fpu.load_control(0x037E);
            assert_eq!(
                fpu.store_integer(bits),
                Err(ArithmeticError::UnmaskedException(1))
            );
        }
        for (rc, positive, negative) in [(0, 2_i64, -2_i64), (1, 1, -2), (2, 2, -1), (3, 1, -1)] {
            for (value, expected) in [(1.5, positive), (-1.5, negative)] {
                let mut fpu = X87::new();
                fpu.push(value).unwrap();
                fpu.load_control(0x037F | (rc << 10));
                assert_eq!(fpu.store_integer(32), Ok(expected as u64));
                assert_eq!(fpu.st0(), Ok(value));
                assert_ne!(fpu.status() & 0x20, 0);
            }
        }
    }
    #[test]
    fn round_integer_handles_ties_signs_large_values_and_all_rounding_modes() {
        for precision in [0, 2, 3] {
            for (rounding, expected) in [
                (0, [0.0, 2.0, 2.0]),
                (1, [0.0, 1.0, 2.0]),
                (2, [1.0, 2.0, 3.0]),
                (3, [0.0, 1.0, 2.0]),
            ] {
                for (index, value) in [0.5, 1.5, 2.5].into_iter().enumerate() {
                    for negative in [false, true] {
                        let mut fpu = X87::new();
                        let value = if negative { -value } else { value };
                        fpu.push(value).unwrap();
                        // Swap down/up for negative magnitudes.
                        let rc = if negative && rounding == 1 {
                            2
                        } else if negative && rounding == 2 {
                            1
                        } else {
                            rounding
                        };
                        fpu.load_control(0x007F | (precision << 8) | (rc << 10));
                        fpu.round_integer().unwrap();
                        let expected: f64 = if negative {
                            -expected[index]
                        } else {
                            expected[index]
                        };
                        assert_eq!(fpu.st0().unwrap().to_bits(), expected.to_bits());
                        assert_ne!(fpu.status() & 0x20, 0);
                    }
                }
            }
        }
        let mut fpu = X87::new();
        fpu.push_integer(i64::MAX).unwrap();
        fpu.round_integer().unwrap();
        assert_eq!(fpu.finite[7].unwrap().mantissa, (i64::MAX as u64) << 1);
        assert_eq!(fpu.status() & 0x20, 0);
        let mut fpu = X87::new();
        fpu.push(0.25).unwrap();
        fpu.load_control(0x035F);
        assert_eq!(
            fpu.round_integer(),
            Err(ArithmeticError::UnmaskedException(0x20))
        );
        assert_eq!(fpu.st0(), Ok(0.));
    }
    #[test]
    fn memory_stores_use_format_precision_and_guest_rounding_without_changing_st0() {
        for precision in [0, 2, 3] {
            for rounding in 0..4 {
                for sign in [1.0, -1.0] {
                    let mut fpu = X87::new();
                    let original = sign * (1.0 + 1.0 / 16777216.0);
                    fpu.push(original).unwrap();
                    fpu.load_control(0x007F | (precision << 8) | (rounding << 10));
                    let up = (rounding == 2 && sign > 0.0) || (rounding == 1 && sign < 0.0);
                    let expected = sign * if up { 1.0 + 1.0 / 8388608.0 } else { 1.0 };
                    assert_eq!(fpu.store_float(true), Ok(expected));
                    assert_eq!(fpu.st0(), Ok(original));
                    assert_eq!(fpu.status() & 0x220, 0x20 | if up { 0x200 } else { 0 });
                }
            }
        }
        let mut fpu = X87::new();
        fpu.push_integer((1_i64 << 53) + 1).unwrap();
        assert_eq!(fpu.store_float(false), Ok((1_i64 << 53) as f64));
        fpu.load_control(0x035F); // Unmasked precision exception.
        assert_eq!(
            fpu.store_float(false),
            Err(ArithmeticError::UnmaskedException(0x20))
        );
        assert_eq!(fpu.status() & 0x8080, 0x8080);
        assert_eq!(fpu.finite[7].unwrap().mantissa, ((1_u64 << 53) + 1) << 10);
    }
    #[test]
    fn memory_arithmetic_operations_keep_top_and_apply_operand_order() {
        for (operation, expected) in [(0, 6.0), (1, 8.0), (4, -2.0), (5, 2.0), (6, 0.5), (7, 2.0)] {
            let mut fpu = X87::new();
            fpu.push_integer(2).unwrap();
            fpu.arithmetic_memory(4.0, operation).unwrap();
            assert_eq!(fpu.store_value(), Ok(expected));
            assert_eq!(fpu.status() & 0x3800, 0x3800);
            assert_eq!(fpu.status() & 0x20, 0);
        }
    }
    #[test]
    fn integer_load_retains_all_bits_independently_of_precision_and_rounding() {
        for precision in [0, 2, 3] {
            for rounding in 0..4 {
                for value in [0, 1, -1, i64::MIN, i64::MAX, (1_i64 << 53) + 1] {
                    let mut fpu = X87::new();
                    fpu.load_control(0x007F | (precision << 8) | (rounding << 10));
                    fpu.push_integer(value).unwrap();
                    let finite = fpu.finite[7].unwrap();
                    let magnitude = if finite.mantissa == 0 {
                        0
                    } else {
                        finite.mantissa >> (63 - finite.exponent)
                    };
                    assert_eq!(magnitude, value.unsigned_abs());
                    assert_eq!(finite.negative, value < 0);
                    assert_eq!(fpu.status() & 0x3AFF, 0x3800);
                }
            }
        }
        let mut fpu = X87::new();
        fpu.push_integer(i64::MAX).unwrap();
        fpu.push_integer(i64::MAX - 1).unwrap();
        fpu.arithmetic_pop(1, false, false).unwrap();
        assert_eq!(fpu.store_value(), Ok(1.0));
        let mut full = X87::new();
        for _ in 0..8 {
            full.push_integer(1).unwrap();
        }
        assert_eq!(full.push_integer(2), Err(StackError::Overflow));
        assert_eq!(full.status() & 0x200, 0x200);
    }
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
    fn division_precompletion_and_postcompletion_exceptions_have_distinct_stack_effects() {
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
        assert_eq!(fpu.st0(), Ok(1.0 / 3.0));
        assert_eq!(fpu.st(1), Err(StackError::Empty));
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
