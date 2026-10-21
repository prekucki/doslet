//! Exact binary64 fast paths for common 387 operations.
//!
//! Every `fast_*` method leaves the same state as its reference method:
//! register values (including the diagnostic binary64 view), retained finite
//! data, raw encodings, tags, status with C1, and the returned value. Outside
//! its domain it calls that reference method, which also reports every error.
//!
//! Arithmetic domain: PC=24 or PC=53, any RC, all exceptions masked, and
//! operands that are zero or normal with at most 53 significant bits and an
//! unbiased exponent of magnitude at most `LIMIT`, so each is an exact
//! binary64 value. The host computes s = RN53(x) of the exact result x and,
//! when needed, the exact sign of x − s: TwoSum for sums, Dekker's product
//! without FMA for products, and Dekker products of the residual a − q·b for
//! quotients. `LIMIT` keeps every operand, split, product, quotient and error
//! term a normal binary64 value, which these error-free transformations need;
//! results then never overflow, underflow or become denormal.
//!
//! Since |x − s| ≤ ulp(s)/2, x lies in [s − ulp/2, s) or (s, s + ulp/2]
//! (or [s − ulp/4, s) at a binade start). For P ≤ 53, these intervals
//! contain no P-bit grid point and, apart from their outer ends, no P-bit
//! midpoint. Those ends are midpoints only for P = 53, where binary64 RN has
//! already chosen the correct even neighbour. So x rounds like the 64-bit
//! significand of s, decreased by one unit when x < s in magnitude, plus a
//! sticky bit: `round` applies the reference rounding rule to that value.
//! Under P = 24 the sign matters only when s itself is a 24-bit grid point or
//! midpoint (`needs_error`).
//!
//! Loads, integer loads, register duplication and real32/64 stores have no
//! precision dependence; their fast paths cover zero and normal values (and
//! in-range store results), reproducing the reference's retained data and
//! tags, including those of zeros.
use crate::{
    X87, extended,
    finite::{Finite, Rounded},
};

const LIMIT: u32 = 448;
/// Fast arithmetic results, so tests can check that they exercised the path.
#[cfg(test)]
static HITS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
const FRACTION: u64 = (1 << 52) - 1;
/// Binary64 operations round once, without excess precision or contraction.
const NATIVE: bool = cfg!(any(
    target_arch = "x86_64",
    target_arch = "aarch64",
    target_arch = "wasm32",
    all(target_arch = "x86", target_feature = "sse2")
));

/// PC=24 or PC=53 with every exception masked.
#[inline(always)]
fn precision(control: u16) -> Option<u32> {
    if !NATIVE || control & 0x3F != 0x3F {
        return None;
    }
    match (control >> 8) & 3 {
        0 => Some(24),
        2 => Some(53),
        _ => None,
    }
}

/// The exact binary64 value of a retained register in the fast domain.
#[inline(always)]
fn exact(value: Finite) -> Option<f64> {
    if value.mantissa == 0 {
        return Some(signed_zero(value.negative));
    }
    if value.mantissa >> 63 == 0
        || value.mantissa & 0x7FF != 0
        || value.exponent.unsigned_abs() > LIMIT
    {
        return None;
    }
    Some(f64::from_bits(
        (u64::from(value.negative) << 63)
            | (((value.exponent + 1023) as u64) << 52)
            | ((value.mantissa >> 11) & FRACTION),
    ))
}

/// A real32/64 memory operand that is zero or normal within `LIMIT`.
#[inline(always)]
fn real(bits: u64, single: bool) -> Option<f64> {
    if single {
        let bits = bits as u32;
        let exponent = (bits >> 23) & 0xFF;
        if (exponent == 0 && bits << 1 != 0) || exponent == 0xFF {
            return None;
        }
        Some(f64::from(f32::from_bits(bits)))
    } else {
        let exponent = ((bits >> 52) & 0x7FF) as i32;
        if bits << 1 == 0 {
            return Some(f64::from_bits(bits));
        }
        if exponent == 0 || (exponent - 1023).unsigned_abs() > LIMIT {
            return None;
        }
        Some(f64::from_bits(bits))
    }
}

#[inline(always)]
fn signed_zero(negative: bool) -> f64 {
    f64::from_bits(u64::from(negative) << 63)
}

/// Finite form of a nonzero normal binary64 value.
#[inline(always)]
fn finite(value: f64) -> Finite {
    let bits = value.to_bits();
    Finite {
        negative: bits >> 63 != 0,
        exponent: ((bits >> 52) & 0x7FF) as i32 - 1023,
        mantissa: ((bits & FRACTION) | (1 << 52)) << 11,
    }
}

/// Binary64 view of a rounded result with at most 53 bits in range.
#[inline(always)]
fn binary64(value: Finite) -> f64 {
    if value.mantissa == 0 {
        signed_zero(value.negative)
    } else {
        f64::from_bits(
            (u64::from(value.negative) << 63)
                | (((value.exponent + 1023) as u64) << 52)
                | ((value.mantissa >> 11) & FRACTION),
        )
    }
}

#[inline(always)]
fn sign(value: f64) -> i32 {
    if value > 0.0 {
        1
    } else if value < 0.0 {
        -1
    } else {
        0
    }
}

/// Knuth's TwoSum: a + b = s + e exactly.
#[inline(always)]
fn two_sum(a: f64, b: f64) -> (f64, f64) {
    let s = a + b;
    let bb = s - a;
    (s, (a - (s - bb)) + (b - bb))
}

/// Veltkamp splitting into two halves of at most 26 significant bits.
#[inline(always)]
fn split(a: f64) -> (f64, f64) {
    let c = 134_217_729.0 * a; // 2^27 + 1
    let high = c - (c - a);
    (high, a - high)
}

/// Dekker's product: a · b = p + e exactly.
#[inline(always)]
fn two_product(a: f64, b: f64) -> (f64, f64) {
    let p = a * b;
    let (ah, al) = split(a);
    let (bh, bl) = split(b);
    (p, ((ah * bh - p) + ah * bl + al * bh) + al * bl)
}

/// The reference rounding of x with RN53(x) = s (normal, nonzero) and
/// `toward` = sign(x − s), at `precision` bits with the RC of `control`.
#[inline(always)]
fn round(s: f64, toward: i32, control: u16, precision: u32) -> Rounded {
    let Finite {
        negative,
        mut exponent,
        mantissa,
    } = finite(s);
    // Direction of x relative to s in magnitude.
    let away = if negative { -toward } else { toward };
    let (mantissa, sticky) = match away {
        0 => (mantissa, false),
        1 => (mantissa, true),
        _ if mantissa == 1 << 63 => {
            exponent -= 1;
            (u64::MAX, true)
        }
        _ => (mantissa - 1, true),
    };
    let discard = 64 - precision;
    let low = mantissa & ((1 << discard) - 1);
    let kept = mantissa >> discard;
    let half = 1 << (discard - 1);
    let inexact = low != 0 || sticky;
    let up = inexact
        && match (control >> 10) & 3 {
            0 => low > half || (low == half && (sticky || kept & 1 != 0)),
            1 => negative,
            2 => !negative,
            _ => false,
        };
    let rounded = kept + u64::from(up);
    let (mantissa, exponent) = if rounded >> precision != 0 {
        (1 << 63, exponent + 1)
    } else {
        (rounded << discard, exponent)
    };
    Rounded {
        value: Finite {
            negative,
            exponent,
            mantissa,
        },
        inexact,
        up,
        exceptions: 0,
    }
}

/// The reference result of an exactly zero arithmetic result.
#[inline(always)]
fn zero(negative: bool) -> Rounded {
    Rounded {
        value: Finite {
            negative,
            exponent: 0,
            mantissa: 0,
        },
        inexact: false,
        up: false,
        exceptions: 0,
    }
}

/// Whether rounding RN53(x) = s to `precision` needs the sign of x − s. For
/// P = 24 that is only when s is a 24-bit grid point or midpoint (its low 29
/// significand bits are 0 or 2^28). Otherwise those points are at least one
/// binary64 ulp from s, so x lies in the same open rounding interval as s,
/// rounds the same way and is inexact.
#[inline(always)]
fn needs_error(s: f64, precision: u32) -> bool {
    let low = s.to_bits() & ((1 << 29) - 1);
    precision == 53 || low == 0 || low == 1 << 28
}

/// `a op b` in the encoding of `X87::binary`: 0 add, 1 multiply, 4 a − b,
/// 5 b − a, 6 a / b, 7 b / a. None for a zero divisor.
#[inline(always)]
fn arithmetic(a: f64, b: f64, operation: u8, control: u16, precision: u32) -> Option<Rounded> {
    let (a, b, operation) = match operation {
        5 => (b, a, 4),
        7 => (b, a, 6),
        _ => (a, b, operation),
    };
    let down = (control >> 10) & 3 == 1;
    Some(match operation {
        0 | 4 => {
            let b = if operation == 4 { -b } else { b };
            if a == 0.0 && b == 0.0 {
                let (left, right) = (a.is_sign_negative(), b.is_sign_negative());
                return Some(zero(if left == right { left } else { down }));
            }
            let s = a + b;
            if s == 0.0 {
                // Exact cancellation of nonzero operands; no underflow here.
                return Some(zero(down));
            }
            let toward = if needs_error(s, precision) {
                sign(two_sum(a, b).1)
            } else {
                0
            };
            round(s, toward, control, precision)
        }
        1 => {
            if a == 0.0 || b == 0.0 {
                return Some(zero(a.is_sign_negative() != b.is_sign_negative()));
            }
            let p = a * b;
            let toward = if needs_error(p, precision) {
                sign(two_product(a, b).1)
            } else {
                0
            };
            round(p, toward, control, precision)
        }
        6 => {
            if b == 0.0 {
                return None;
            }
            if a == 0.0 {
                return Some(zero(a.is_sign_negative() != b.is_sign_negative()));
            }
            let q = a / b;
            let toward = if needs_error(q, precision) {
                let (p, e) = two_product(q, b);
                // a − p is exact (Sterbenz), so sign(a − q·b) = sign((a − p) − e),
                // and a/b − q = (a − q·b)/b.
                let d = a - p;
                let residual = if d > e {
                    1
                } else if d < e {
                    -1
                } else {
                    0
                };
                if b < 0.0 { -residual } else { residual }
            } else {
                0
            };
            round(q, toward, control, precision)
        }
        _ => return None,
    })
}

impl X87 {
    #[inline(always)]
    fn top(&self) -> usize {
        ((self.status >> 11) & 7) as usize
    }
    #[inline(always)]
    fn empty_slot(&self, slot: usize) -> bool {
        (self.tags >> (slot * 2)) & 3 == 3
    }
    /// The exact binary64 operand of a nonempty register in the fast domain.
    /// Its raw encoding must not be denormal, as `binary` would report.
    #[inline(always)]
    fn fast_operand(&self, slot: usize) -> Option<f64> {
        let raw = self.encodings[slot];
        if self.empty_slot(slot) || ((raw >> 64) & 0x7FFF == 0 && raw as u64 != 0) {
            return None;
        }
        exact(self.finite[slot]?)
    }
    /// Commit a fast arithmetic result exactly as `store_rounded` does for an
    /// in-range result under masked exceptions.
    #[inline(always)]
    fn fast_store(&mut self, slot: usize, result: Rounded, pop: bool) {
        #[cfg(test)]
        HITS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        self.status = (self.status & !0x200)
            | if result.inexact { 0x20 } else { 0 }
            | if result.up { 0x200 } else { 0 };
        let value = result.value;
        self.values[slot] = binary64(value);
        self.finite[slot] = Some(value);
        self.encodings[slot] = extended::encode(value);
        let tag = u16::from(value.mantissa == 0);
        self.tags = (self.tags & !(3 << (slot * 2))) | (tag << (slot * 2));
        if pop {
            let top = self.top();
            self.tags |= 3 << (top * 2);
            self.finite[top] = None;
            self.status = (self.status & !0x3800) | ((((top + 1) & 7) as u16) << 11);
        }
    }
    /// Push a value as `push` followed by retained-data replacement does.
    #[inline(always)]
    fn fast_push(&mut self, value: f64, finite: Finite, raw: u128, tag: u16) {
        let top = (self.top() + 7) & 7;
        self.status = (self.status & !0x3A00) | ((top as u16) << 11);
        self.tags = (self.tags & !(3 << (top * 2))) | (tag << (top * 2));
        self.values[top] = value;
        self.finite[top] = Some(finite);
        self.encodings[top] = raw;
    }

    /// `load_real` (FLD real32/real64).
    pub fn fast_load_real(
        &mut self,
        bits: u64,
        single: bool,
    ) -> Result<(), crate::ArithmeticError> {
        let top = (self.top() + 7) & 7;
        if self.empty_slot(top) {
            let (negative, exponent, mantissa) = if single {
                let bits = bits as u32;
                (
                    bits >> 31 != 0,
                    ((bits >> 23) & 0xFF) as i32,
                    u64::from(bits & 0x7F_FFFF),
                )
            } else {
                (
                    bits >> 63 != 0,
                    ((bits >> 52) & 0x7FF) as i32,
                    bits & FRACTION,
                )
            };
            let (bias, maximum, width) = if single {
                (127, 0xFF, 23)
            } else {
                (1023, 0x7FF, 52)
            };
            if exponent == 0 && mantissa == 0 {
                // Decoded from the raw real80 zero: exponent field 0.
                let value = Finite {
                    negative,
                    exponent: -16382,
                    mantissa: 0,
                };
                self.fast_push(signed_zero(negative), value, extended::encode(value), 1);
                return Ok(());
            }
            if exponent != 0 && exponent != maximum {
                let value = Finite {
                    negative,
                    exponent: exponent - bias,
                    mantissa: (mantissa | (1 << width)) << (63 - width),
                };
                let diagnostic = if single {
                    f64::from(f32::from_bits(bits as u32))
                } else {
                    f64::from_bits(bits)
                };
                self.fast_push(diagnostic, value, extended::encode(value), 0);
                return Ok(());
            }
        }
        self.load_real(bits, single)
    }

    /// `push_integer` (FILD).
    pub fn fast_push_integer(&mut self, value: i64) -> Result<(), crate::StackError> {
        let top = (self.top() + 7) & 7;
        if self.empty_slot(top) && value.unsigned_abs() < 1 << 53 {
            let finite = Finite::from_i64(value);
            self.fast_push(
                value as f64,
                finite,
                extended::encode(finite),
                u16::from(value == 0),
            );
            return Ok(());
        }
        self.push_integer(value)
    }

    /// `duplicate` (FLD ST(i)). The tag follows the diagnostic value, as `push`.
    pub fn fast_duplicate(&mut self, index: u8) -> Result<(), crate::StackError> {
        let source = (self.top() + usize::from(index & 7)) & 7;
        let top = (self.top() + 7) & 7;
        if index <= 7 && !self.empty_slot(source) && self.empty_slot(top) {
            let value = self.values[source];
            let tag = if value == 0.0 {
                1
            } else if value.is_finite() {
                0
            } else {
                2
            };
            let finite = self.finite[source];
            let raw = self.encodings[source];
            self.status = (self.status & !0x3A00) | ((top as u16) << 11);
            self.tags = (self.tags & !(3 << (top * 2))) | (tag << (top * 2));
            self.values[top] = value;
            self.finite[top] = finite;
            self.encodings[top] = raw;
            return Ok(());
        }
        self.duplicate(index)
    }

    /// `store_float` (FST/FSTP real32/real64), including its returned value.
    pub fn fast_store_float(&mut self, single: bool) -> Result<f64, crate::ArithmeticError> {
        let top = self.top();
        if NATIVE
            && !self.empty_slot(top)
            && self.control & 0x3F == 0x3F
            && let Some(value) = self.finite[top]
        {
            let (precision, minimum, maximum) = if single {
                (24, -126, 126)
            } else {
                (53, -1022, 1022)
            };
            if value.mantissa == 0 {
                self.status &= !0x200;
                return Ok(signed_zero(value.negative));
            }
            if value.mantissa >> 63 != 0 && (minimum..=maximum).contains(&value.exponent) {
                let discard = 64 - precision;
                let low = value.mantissa & ((1 << discard) - 1);
                let kept = value.mantissa >> discard;
                let half = 1 << (discard - 1);
                let inexact = low != 0;
                let up = inexact
                    && match (self.control >> 10) & 3 {
                        0 => low > half || (low == half && kept & 1 != 0),
                        1 => value.negative,
                        2 => !value.negative,
                        _ => false,
                    };
                let rounded = kept + u64::from(up);
                let result = if rounded >> precision != 0 {
                    Finite {
                        negative: value.negative,
                        exponent: value.exponent + 1,
                        mantissa: 1 << 63,
                    }
                } else {
                    Finite {
                        negative: value.negative,
                        exponent: value.exponent,
                        mantissa: rounded << discard,
                    }
                };
                self.status = (self.status & !0x200)
                    | if inexact { 0x20 } else { 0 }
                    | if up { 0x200 } else { 0 };
                return Ok(binary64(result));
            }
        }
        self.store_float(single)
    }

    /// `arithmetic_real` (D8/DC memory arithmetic).
    pub fn fast_arithmetic_real(
        &mut self,
        bits: u64,
        single: bool,
        operation: u8,
    ) -> Result<(), crate::ArithmeticError> {
        let top = self.top();
        if let Some(precision) = precision(self.control)
            && let Some(a) = self.fast_operand(top)
            && let Some(b) = real(bits, single)
            && let Some(result) = arithmetic(a, b, operation, self.control, precision)
        {
            self.fast_store(top, result, false);
            return Ok(());
        }
        self.arithmetic_real(bits, single, operation)
    }

    /// `arithmetic_integer` (DA/DE memory arithmetic).
    pub fn fast_arithmetic_integer(
        &mut self,
        value: i64,
        operation: u8,
    ) -> Result<(), crate::ArithmeticError> {
        let top = self.top();
        if let Some(precision) = precision(self.control)
            && value.unsigned_abs() < 1 << 53
            && let Some(a) = self.fast_operand(top)
            && let Some(result) = arithmetic(a, value as f64, operation, self.control, precision)
        {
            self.fast_store(top, result, false);
            return Ok(());
        }
        self.arithmetic_integer(value, operation)
    }

    /// `arithmetic_top` (D8 register arithmetic).
    pub fn fast_arithmetic_top(
        &mut self,
        index: u8,
        operation: u8,
    ) -> Result<(), crate::ArithmeticError> {
        let top = self.top();
        if index <= 7
            && let Some(precision) = precision(self.control)
            && let Some(a) = self.fast_operand(top)
            && let Some(b) = self.fast_operand((top + usize::from(index)) & 7)
            && let Some(result) = arithmetic(a, b, operation, self.control, precision)
        {
            self.fast_store(top, result, false);
            return Ok(());
        }
        self.arithmetic_top(index, operation)
    }

    /// `arithmetic_into` (DC/DE register arithmetic).
    pub fn fast_arithmetic_into(
        &mut self,
        index: u8,
        operation: u8,
        pop: bool,
    ) -> Result<(), crate::ArithmeticError> {
        let top = self.top();
        let slot = (top + usize::from(index)) & 7;
        if index <= 7
            && let Some(precision) = precision(self.control)
            && let Some(a) = self.fast_operand(slot)
            && let Some(b) = self.fast_operand(top)
            && let Some(result) = arithmetic(a, b, operation, self.control, precision)
        {
            self.fast_store(slot, result, pop);
            return Ok(());
        }
        self.arithmetic_into(index, operation, pop)
    }
}

#[cfg(test)]
#[path = "fast_tests.rs"]
mod tests;
