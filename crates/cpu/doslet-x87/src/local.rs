//! Guarded PC24 regions with binary64 locals and explicit retained-state commit.
//!
//! This is an opt-in numerical building block, not a CPU execution engine. The
//! owner still proves memory accesses, records instruction/operand pointers,
//! charges guest work and stops before every hardware/control boundary. Failed
//! numerical operations have no effects; commit the accepted prefix before
//! interpreting the refused instruction. Dropping a region discards its work.
use crate::{X87, extended, fast, finite::Finite};

#[path = "pc24_boundary.rs"]
pub mod boundary;

const ORIGIN_ARITHMETIC: u8 = 8;
const ORIGIN_REAL32: u8 = 9;
const ORIGIN_INTEGER32: u8 = 10;

#[inline(always)]
fn initial_values(owner: &X87, read_mask: u8) -> Option<[f64; 8]> {
    if fast::precision(owner.control) != Some(24) {
        return None;
    }
    let mut values = [0.0; 8];
    for (slot, value) in values.iter_mut().enumerate() {
        if read_mask & (1 << slot) != 0 {
            let raw = owner.encodings[slot];
            if (owner.tags >> (slot * 2)) & 3 == 3 || ((raw >> 64) & 0x7fff == 0 && raw as u64 != 0)
            {
                return None;
            }
            *value = fast::exact(owner.finite[slot]?)?;
            if owner.values[slot].to_bits() != value.to_bits() {
                return None;
            }
        }
    }
    Some(values)
}

/// One correctly rounded PC24 result and the precision/C1 contributions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pc24Result {
    pub value: f64,
    pub inexact: bool,
    /// Rounding increased the magnitude, matching x87 C1.
    pub up: bool,
}

#[inline(always)]
fn domain(value: f64) -> bool {
    let bits = value.to_bits() & 0x7fff_ffff_ffff_ffff;
    let exponent = (bits >> 52) as i32;
    bits == 0 || (exponent != 0 && (exponent - 1023).unsigned_abs() <= fast::LIMIT)
}

/// Round using the same RN53 value and exact residual sign as `fast::round`.
/// PC24 grid neighbours differ by 2^29 in binary64 encoding, including at a
/// binade boundary. No retained Finite or real80 encoding is formed here.
#[inline(always)]
fn round24(value: f64, toward: i32, control: u16) -> Pc24Result {
    let bits = value.to_bits();
    let sign = bits & (1 << 63);
    let magnitude = bits & !(1 << 63);
    if magnitude == 0 {
        return Pc24Result {
            value,
            inexact: false,
            up: false,
        };
    }
    let away = if sign == 0 { toward } else { -toward };
    let step = 1u64 << 29;
    let low = magnitude & (step - 1);
    let inexact = low != 0 || away != 0;
    let up = inexact
        && match (control >> 10) & 3 {
            0 if low == 0 => away < 0,
            0 => {
                low > step / 2
                    || (low == step / 2 && (away > 0 || (away == 0 && magnitude & step != 0)))
            }
            1 => sign != 0,
            2 => sign == 0,
            _ => false,
        };
    let floor = (magnitude & !(step - 1)) - if low == 0 && away < 0 { step } else { 0 };
    Pc24Result {
        value: f64::from_bits(sign | (floor + if up { step } else { 0 })),
        inexact,
        up,
    }
}

#[inline(always)]
fn binary(lhs: f64, rhs: f64, operation: u8, control: u16) -> Option<Pc24Result> {
    let (value, toward) = fast::arithmetic_components(lhs, rhs, operation, control, 24)?;
    Some(round24(value, toward, control))
}

/// Pure local arithmetic for generated regions. Admits masked PC24 controls
/// and normal/zero binary64 operands with exponents in ±448. Operations are
/// 0 add, 1 multiply, 4 subtract, 5 reverse subtract, 6 divide, 7 reverse divide.
/// A zero divisor or unsupported domain refuses without changing any state.
#[inline(always)]
pub fn pc24_binary(lhs: f64, rhs: f64, operation: u8, control: u16) -> Option<Pc24Result> {
    if fast::precision(control) != Some(24) || !domain(lhs) || !domain(rhs) {
        return None;
    }
    binary(lhs, rhs, operation, control)
}

/// Exclusive, allocation-free region. Masks name physical register slots,
/// independent of TOP. Read bits import initial nonempty values; write bits
/// authorize value/tag changes. Unselected initial state remains untouched.
/// Zero/normal FLD32, six memory/register arithmetic forms, valid FIST32,
/// duplicate/exchange and ordinary successful pops are supported. Refusals never call reference
/// operations and never modify the region or its parked owner.
pub struct Pc24Region<'a> {
    owner: &'a mut X87,
    values: [f64; 8],
    control: u16,
    status: u16,
    tags: u16,
    initialized: u8,
    writable: u8,
    dirty_values: u8,
    popped: u8,
    /// Original physical source, or 8=arithmetic, 9=FLD32, 10=FILD32.
    origins: [u8; 8],
    instruction: Option<(u32, u16, u16)>,
    operand: Option<(u32, u16)>,
}

impl X87 {
    /// Enter without modifying the FPU. Unmasked exceptions, PC53/64, empty
    /// declared reads, special/denormal retained data and wider domains refuse.
    /// Keep all memory and CPU admission outside this numerical API.
    #[inline(always)]
    pub fn pc24_region(&mut self, read_mask: u8, write_mask: u8) -> Option<Pc24Region<'_>> {
        let values = initial_values(self, read_mask)?;
        let origins = core::array::from_fn(|slot| {
            if read_mask & (1 << slot) != 0 {
                slot as u8
            } else {
                ORIGIN_ARITHMETIC
            }
        });
        Some(Pc24Region {
            control: self.control,
            status: self.status,
            tags: self.tags,
            owner: self,
            values,
            initialized: read_mask,
            writable: write_mask,
            dirty_values: 0,
            popped: 0,
            origins,
            instruction: None,
            operand: None,
        })
    }
}

impl Pc24Region<'_> {
    #[inline(always)]
    fn top(&self) -> usize {
        ((self.status >> 11) & 7) as usize
    }

    #[inline(always)]
    fn readable(&self, slot: usize) -> bool {
        self.initialized & (1 << slot) != 0 && (self.tags >> (slot * 2)) & 3 != 3
    }

    /// FLD real32. The write mask must include the empty physical push slot.
    #[inline(always)]
    pub fn load_real32(&mut self, bits: u32) -> bool {
        let Some(value) = fast::real(u64::from(bits), true) else {
            return false;
        };
        self.push_local(value, ORIGIN_REAL32)
    }

    /// FILD dword retains all 32 signed integer bits independently of PC24.
    #[inline(always)]
    pub fn load_integer32(&mut self, value: i32) -> bool {
        self.push_local(f64::from(value), ORIGIN_INTEGER32)
    }

    #[inline(always)]
    fn push_local(&mut self, value: f64, origin: u8) -> bool {
        let slot = (self.top() + 7) & 7;
        let bit = 1 << slot;
        if self.writable & bit == 0 || (self.tags >> (slot * 2)) & 3 != 3 {
            return false;
        }
        self.values[slot] = value;
        self.initialized |= bit;
        self.dirty_values |= bit;
        self.origins[slot] = origin;
        self.tags = (self.tags & !(3 << (slot * 2))) | (u16::from(value == 0.0) << (slot * 2));
        self.status = (self.status & !0x3a00) | ((slot as u16) << 11);
        true
    }

    /// Memory-real32 arithmetic on ST0. Source access must already be proved.
    #[inline(always)]
    pub fn arithmetic_real32(&mut self, bits: u32, operation: u8) -> bool {
        let slot = self.top();
        let bit = 1 << slot;
        if !self.readable(slot) || self.writable & bit == 0 || !domain(self.values[slot]) {
            return false;
        }
        let Some(rhs) = fast::real(u64::from(bits), true) else {
            return false;
        };
        let Some(result) = binary(self.values[slot], rhs, operation, self.control) else {
            return false;
        };
        self.store_result(slot, result);
        true
    }

    #[inline(always)]
    fn store_result(&mut self, slot: usize, result: Pc24Result) {
        self.values[slot] = result.value;
        self.dirty_values |= 1 << slot;
        self.origins[slot] = ORIGIN_ARITHMETIC;
        self.tags =
            (self.tags & !(3 << (slot * 2))) | (u16::from(result.value == 0.0) << (slot * 2));
        self.status = (self.status & !0x200)
            | if result.inexact { 0x20 } else { 0 }
            | if result.up { 0x200 } else { 0 };
    }

    /// ST0 = ST0 op ST(index), with the same six opcodes as `pc24_binary`.
    #[inline(always)]
    pub fn arithmetic_top(&mut self, index: u8, operation: u8) -> bool {
        if index > 7 {
            return false;
        }
        let top = self.top();
        let source = (top + usize::from(index)) & 7;
        if !self.readable(top)
            || !self.readable(source)
            || self.writable & (1 << top) == 0
            || !domain(self.values[top])
            || !domain(self.values[source])
        {
            return false;
        }
        let Some(result) = binary(
            self.values[top],
            self.values[source],
            operation,
            self.control,
        ) else {
            return false;
        };
        self.store_result(top, result);
        true
    }

    /// ST(index) = ST(index) op ST0, optionally popping ST0 afterward.
    #[inline(always)]
    pub fn arithmetic_into(&mut self, index: u8, operation: u8, pop: bool) -> bool {
        if index > 7 {
            return false;
        }
        let top = self.top();
        let destination = (top + usize::from(index)) & 7;
        if !self.readable(top)
            || !self.readable(destination)
            || self.writable & (1 << destination) == 0
            || (pop && self.writable & (1 << top) == 0)
            || !domain(self.values[top])
            || !domain(self.values[destination])
        {
            return false;
        }
        let Some(result) = binary(
            self.values[destination],
            self.values[top],
            operation,
            self.control,
        ) else {
            return false;
        };
        self.store_result(destination, result);
        if pop {
            self.pop();
        }
        true
    }

    /// FLD ST(index), retaining the original source encoding without rounding.
    #[inline(always)]
    pub fn duplicate(&mut self, index: u8) -> bool {
        if index > 7 {
            return false;
        }
        let source = (self.top() + usize::from(index)) & 7;
        let slot = (self.top() + 7) & 7;
        let bit = 1 << slot;
        if !self.readable(source) || self.writable & bit == 0 || (self.tags >> (slot * 2)) & 3 != 3
        {
            return false;
        }
        self.values[slot] = self.values[source];
        self.origins[slot] = self.origins[source];
        self.initialized |= bit;
        self.dirty_values |= bit;
        self.tags =
            (self.tags & !(3 << (slot * 2))) | (u16::from(self.values[slot] == 0.0) << (slot * 2));
        self.status = (self.status & !0x3a00) | ((slot as u16) << 11);
        true
    }

    /// FXCH exchanges complete retained values and tags without changing TOP.
    #[inline(always)]
    pub fn exchange(&mut self, index: u8) -> bool {
        if index > 7 {
            return false;
        }
        let top = self.top();
        let slot = (top + usize::from(index)) & 7;
        let bits = (1 << top) | (1 << slot);
        if !self.readable(top) || !self.readable(slot) || self.writable & bits != bits {
            return false;
        }
        let top_tag = (self.tags >> (top * 2)) & 3;
        let slot_tag = (self.tags >> (slot * 2)) & 3;
        self.values.swap(top, slot);
        self.origins.swap(top, slot);
        self.dirty_values |= bits;
        self.tags = (self.tags & !((3 << (top * 2)) | (3 << (slot * 2))))
            | (slot_tag << (top * 2))
            | (top_tag << (slot * 2));
        self.status &= !0x200;
        true
    }

    /// FST/FSTP ST(index) copies the complete source without rounding. Unlike
    /// FXCH, the destination may be empty. Check both writes before changing
    /// anything when a pop follows the copy.
    #[inline(always)]
    pub fn store_register(&mut self, index: u8, pop: bool) -> bool {
        if index > 7 {
            return false;
        }
        let top = self.top();
        let destination = (top + usize::from(index)) & 7;
        let bit = 1 << destination;
        if !self.readable(top)
            || self.writable & bit == 0
            || (pop && self.writable & (1 << top) == 0)
        {
            return false;
        }
        let tag = (self.tags >> (top * 2)) & 3;
        self.values[destination] = self.values[top];
        self.origins[destination] = self.origins[top];
        self.initialized |= bit;
        self.dirty_values |= bit;
        self.tags = (self.tags & !(3 << (destination * 2))) | (tag << (destination * 2));
        self.status &= !0x200;
        if pop {
            self.pop();
        }
        true
    }

    /// Ordered FCOM/FCOMP real32. Special and denormal memory operands refuse
    /// before condition flags or stack state change.
    #[inline(always)]
    pub fn compare_real32(&mut self, bits: u32, pop: bool) -> bool {
        let top = self.top();
        if !self.readable(top) {
            return false;
        }
        let Some(rhs) = fast::real(u64::from(bits), true) else {
            return false;
        };
        self.compare_values(self.values[top], rhs, u8::from(pop))
    }

    /// Ordered FCOM/FCOMP ST(index), or FCOMPP for index=1 and pops=2.
    /// Every source and pop is checked before updating C0/C1/C2/C3.
    #[inline(always)]
    pub fn compare_register(&mut self, index: u8, pops: u8) -> bool {
        if index > 7 || pops > 2 || (pops == 2 && index != 1) {
            return false;
        }
        let top = self.top();
        let source = (top + usize::from(index)) & 7;
        if !self.readable(top) || !self.readable(source) {
            return false;
        }
        self.compare_values(self.values[top], self.values[source], pops)
    }

    #[inline(always)]
    fn compare_values(&mut self, lhs: f64, rhs: f64, pops: u8) -> bool {
        let top = self.top();
        for offset in 0..pops {
            let slot = (top + usize::from(offset)) & 7;
            if !self.readable(slot) || self.writable & (1 << slot) == 0 {
                return false;
            }
        }
        let condition = if lhs < rhs {
            0x100
        } else if lhs == rhs {
            0x4000
        } else {
            0
        };
        self.status = (self.status & !0x4700) | condition;
        for _ in 0..pops {
            self.pop();
        }
        true
    }

    /// FNSTSW reads the full status word without changing FPU state or
    /// environment pointers. The CPU adapter merges it into AX separately.
    #[inline(always)]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// FIST real signed dword, without a pop or memory write. Invalid/range
    /// cases refuse transactionally for reference fallback. Prove the store
    /// first, write the accepted result, then call `pop` for FISTP.
    #[inline(always)]
    pub fn store_integer32(&mut self) -> Option<u32> {
        let slot = self.top();
        if !self.readable(slot) {
            return None;
        }
        let (value, inexact, up) = integer32(self.values[slot], self.control)?;
        self.status =
            (self.status & !0x200) | if inexact { 0x20 } else { 0 } | if up { 0x200 } else { 0 };
        Some(value)
    }

    /// Pop after a successful guest store. Diagnostic values and raw encodings
    /// remain stale in the empty slot exactly as in `X87::pop`.
    #[inline(always)]
    pub fn pop(&mut self) -> bool {
        let slot = self.top();
        let bit = 1 << slot;
        if !self.readable(slot) || self.writable & bit == 0 {
            return false;
        }
        self.tags |= 3 << (slot * 2);
        self.popped |= bit;
        self.status = (self.status & !0x3800) | ((((slot + 1) & 7) as u16) << 11);
        true
    }

    /// Record only after the corresponding numerical instruction is accepted.
    #[inline(always)]
    pub fn record_instruction(&mut self, ip: u32, cs: u16, opcode: u16) {
        self.instruction = Some((ip, cs, opcode));
    }
    /// Protected-mode offsets/selectors, or real-mode linear pointers, as in X87.
    #[inline(always)]
    pub fn record_operand(&mut self, offset: u32, selector: u16) {
        self.operand = Some((offset, selector));
    }

    /// Materialize the accepted prefix once, including stale popped-slot data.
    /// The exclusive borrow prevents owner changes between entry and commit.
    #[inline(always)]
    pub fn commit(self) {
        // Snapshot only when a stack move copied an original slot. Resolve all
        // such sources before any destination overwrites the parked owner.
        let originals =
            if (0..8).any(|slot| self.dirty_values & (1 << slot) != 0 && self.origins[slot] < 8) {
                Some((self.owner.values, self.owner.finite, self.owner.encodings))
            } else {
                None
            };
        for slot in 0..8 {
            let bit = 1 << slot;
            if self.dirty_values & bit != 0 {
                if let Some((values, retained, raw)) = &originals
                    && self.origins[slot] < 8
                {
                    let source = usize::from(self.origins[slot]);
                    self.owner.values[slot] = values[source];
                    self.owner.encodings[slot] = raw[source];
                    self.owner.finite[slot] = if (self.tags >> (slot * 2)) & 3 == 3 {
                        None
                    } else {
                        retained[source]
                    };
                    continue;
                }
                let value = self.values[slot];
                let bits = value.to_bits();
                let zero = bits << 1 == 0;
                let finite = Finite {
                    negative: bits >> 63 != 0,
                    exponent: if zero {
                        if self.origins[slot] == ORIGIN_REAL32 {
                            -16382
                        } else {
                            0
                        }
                    } else {
                        ((bits >> 52) & 0x7ff) as i32 - 1023
                    },
                    mantissa: if zero {
                        0
                    } else {
                        ((bits & ((1 << 52) - 1)) | (1 << 52)) << 11
                    },
                };
                self.owner.values[slot] = value;
                self.owner.encodings[slot] = extended::encode(finite);
                self.owner.finite[slot] = if (self.tags >> (slot * 2)) & 3 == 3 {
                    None
                } else {
                    Some(finite)
                };
            } else if self.popped & bit != 0 {
                self.owner.finite[slot] = None;
            }
        }
        self.owner.status = self.status;
        self.owner.tags = self.tags;
        if let Some((ip, cs, opcode)) = self.instruction {
            self.owner.record_instruction(ip, cs, opcode);
        }
        if let Some((offset, selector)) = self.operand {
            self.owner.record_operand(offset, selector);
        }
    }
}

#[inline(always)]
fn integer32(value: f64, control: u16) -> Option<(u32, bool, bool)> {
    let bits = value.to_bits();
    let negative = bits >> 63 != 0;
    let magnitude = bits & !(1 << 63);
    if magnitude == 0 {
        return Some((0, false, false));
    }
    let exponent = (magnitude >> 52) as i32 - 1023;
    if exponent > 31 {
        return None;
    }
    let (kept, low, half) = if exponent < 0 {
        (0, magnitude, 0x3fe0_0000_0000_0000)
    } else {
        let shift = 52 - exponent as u32;
        let mantissa = (magnitude & ((1 << 52) - 1)) | (1 << 52);
        (
            mantissa >> shift,
            mantissa & ((1 << shift) - 1),
            1 << (shift - 1),
        )
    };
    let inexact = low != 0;
    let up = inexact
        && match (control >> 10) & 3 {
            0 => low > half || (low == half && kept & 1 != 0),
            1 => negative,
            2 => !negative,
            _ => false,
        };
    let rounded = kept + u64::from(up);
    if rounded > 1 << 31 || (rounded == 1 << 31 && !negative) {
        return None;
    }
    let result = rounded as u32;
    Some((
        if negative {
            result.wrapping_neg()
        } else {
            result
        },
        inexact,
        up,
    ))
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
