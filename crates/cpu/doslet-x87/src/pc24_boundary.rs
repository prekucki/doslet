//! Versioned little-endian PC24 boundary ABI. No Rust layout or pointers cross
//! the boundary. The trusted compiler consumes the immutable input prefix and
//! returns one accepted numerical prefix; the owner still proves sources,
//! memory, instruction accounting and hardware grants. Detached callers must
//! keep the FPU parked until commit and pass the independently known masks.
use super::{Pc24Region, X87, fast, initial_values};

pub const SIZE: usize = 192;
pub const MAGIC_VALUE: u32 = u32::from_le_bytes(*b"X24R");
/// Version 2 adds ordered comparison condition flags to the commit contract.
pub const VERSION_VALUE: u32 = 2;
pub const MAGIC: usize = 0;
pub const VERSION: usize = 4;
pub const SIZE_FIELD: usize = 8;
pub const READ_MASK: usize = 12;
pub const WRITE_MASK: usize = 13;
pub const CONTROL: usize = 16;
pub const INITIAL_STATUS: usize = 18;
pub const INITIAL_TAGS: usize = 20;
pub const INITIAL_VALUES: usize = 24;
/// Start of mutable output. The preceding 88 bytes must remain identical.
pub const STATUS: usize = 88;
pub const TAGS: usize = 90;
pub const INITIALIZED: usize = 92;
pub const DIRTY_VALUES: usize = 93;
pub const POPPED: usize = 94;
pub const POINTER_FLAGS: usize = 95;
pub const ORIGINS: usize = 96;
pub const VALUES: usize = 104;
pub const INSTRUCTION_IP: usize = 168;
pub const INSTRUCTION_CS: usize = 172;
pub const INSTRUCTION_OPCODE: usize = 174;
pub const OPERAND_OFFSET: usize = 176;
pub const OPERAND_SELECTOR: usize = 180;
pub const RECORD_INSTRUCTION: u8 = 1;
pub const RECORD_OPERAND: u8 = 2;
pub const ORIGIN_ARITHMETIC: u8 = super::ORIGIN_ARITHMETIC;
pub const ORIGIN_REAL32: u8 = super::ORIGIN_REAL32;
pub const ORIGIN_INTEGER32: u8 = super::ORIGIN_INTEGER32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pc24BoundaryError {
    Length,
    Input,
    Reserved,
    Status,
    Masks,
    Slot,
    Pointers,
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}
fn value_at(bytes: &[u8], offset: usize) -> f64 {
    f64::from_bits(u64::from_le_bytes(
        bytes[offset..offset + 8].try_into().unwrap(),
    ))
}
fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn put_value(bytes: &mut [u8], offset: usize, value: f64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_bits().to_le_bytes());
}

impl X87 {
    /// Export only selected initial read values; unselected value fields are
    /// positive zero. Initialize a no-change output. Failure changes no bytes.
    /// Masks name physical slots and `bytes` must have exactly `SIZE` bytes.
    pub fn write_pc24_boundary(&self, read_mask: u8, write_mask: u8, bytes: &mut [u8]) -> bool {
        if bytes.len() != SIZE {
            return false;
        }
        let Some(values) = initial_values(self, read_mask) else {
            return false;
        };
        bytes.fill(0);
        put32(bytes, MAGIC, MAGIC_VALUE);
        put32(bytes, VERSION, VERSION_VALUE);
        put32(bytes, SIZE_FIELD, SIZE as u32);
        bytes[READ_MASK] = read_mask;
        bytes[WRITE_MASK] = write_mask;
        put16(bytes, CONTROL, self.control);
        put16(bytes, INITIAL_STATUS, self.status);
        put16(bytes, INITIAL_TAGS, self.tags);
        put16(bytes, STATUS, self.status);
        put16(bytes, TAGS, self.tags);
        bytes[INITIALIZED] = read_mask;
        for (slot, value) in values.into_iter().enumerate() {
            put_value(bytes, INITIAL_VALUES + slot * 8, value);
            put_value(bytes, VALUES + slot * 8, value);
            bytes[ORIGINS + slot] = if read_mask & (1 << slot) != 0 {
                slot as u8
            } else {
                ORIGIN_ARITHMETIC
            };
        }
        true
    }

    /// Validate the entire returned prefix before modifying any FPU field.
    /// Expected masks are independent of the writable byte buffer. This proves
    /// representation/admission consistency, not a foreign compiler's numerical
    /// derivation or guest work count. The owner must remain parked meanwhile.
    pub fn commit_pc24_boundary(
        &mut self,
        read_mask: u8,
        write_mask: u8,
        bytes: &[u8],
    ) -> Result<(), Pc24BoundaryError> {
        if bytes.len() != SIZE {
            return Err(Pc24BoundaryError::Length);
        }
        let mut initial = [0; SIZE];
        if !self.write_pc24_boundary(read_mask, write_mask, &mut initial)
            || bytes[..STATUS] != initial[..STATUS]
        {
            return Err(Pc24BoundaryError::Input);
        }
        if bytes[182..].iter().any(|&value| value != 0) {
            return Err(Pc24BoundaryError::Reserved);
        }
        let status = u16_at(bytes, STATUS);
        let condition = status & 0x4500;
        if (status ^ self.status) & !0x7f20 != 0
            || status & self.status & 0x20 != self.status & 0x20
            || (condition != self.status & 0x4500 && !matches!(condition, 0 | 0x100 | 0x4000))
        {
            return Err(Pc24BoundaryError::Status);
        }
        let tags = u16_at(bytes, TAGS);
        let initialized = bytes[INITIALIZED];
        let dirty = bytes[DIRTY_VALUES];
        let popped = bytes[POPPED];
        if dirty & !write_mask != 0
            || popped & !write_mask != 0
            || initialized != read_mask | dirty
            || popped & !initialized != 0
        {
            return Err(Pc24BoundaryError::Masks);
        }
        let values = core::array::from_fn::<_, 8, _>(|slot| value_at(bytes, VALUES + slot * 8));
        let origins = core::array::from_fn::<_, 8, _>(|slot| bytes[ORIGINS + slot]);
        for slot in 0..8 {
            let bit = 1 << slot;
            let tag = (tags >> (slot * 2)) & 3;
            let old_tag = (self.tags >> (slot * 2)) & 3;
            if dirty & bit == 0 {
                if origins[slot] != initial[ORIGINS + slot]
                    || values[slot].to_bits() != value_at(&initial, VALUES + slot * 8).to_bits()
                    || tag != if popped & bit != 0 { 3 } else { old_tag }
                {
                    return Err(Pc24BoundaryError::Slot);
                }
                continue;
            }
            let origin = origins[slot];
            let value = values[slot];
            if origin < 8 {
                if read_mask & (1 << origin) == 0
                    || value.to_bits()
                        != value_at(&initial, INITIAL_VALUES + usize::from(origin) * 8).to_bits()
                {
                    return Err(Pc24BoundaryError::Slot);
                }
            } else if !valid_new_value(value, origin) {
                return Err(Pc24BoundaryError::Slot);
            }
            if tag == 3 {
                if popped & bit == 0 {
                    return Err(Pc24BoundaryError::Slot);
                }
            } else {
                let numeric_tag = u16::from(value == 0.0);
                let source_tag = if origin < 8 {
                    (self.tags >> (usize::from(origin) * 2)) & 3
                } else {
                    numeric_tag
                };
                if tag != numeric_tag && tag != source_tag {
                    return Err(Pc24BoundaryError::Slot);
                }
            }
        }
        let pointer_flags = bytes[POINTER_FLAGS];
        if pointer_flags & !(RECORD_INSTRUCTION | RECORD_OPERAND) != 0
            || u16_at(bytes, INSTRUCTION_OPCODE) > 0x7ff
            || (pointer_flags & RECORD_INSTRUCTION == 0
                && bytes[INSTRUCTION_IP..OPERAND_OFFSET]
                    .iter()
                    .any(|&value| value != 0))
            || (pointer_flags & RECORD_OPERAND == 0
                && bytes[OPERAND_OFFSET..182].iter().any(|&value| value != 0))
        {
            return Err(Pc24BoundaryError::Pointers);
        }
        // Everything is valid; reuse the local commit rules, including original
        // raw/Finite copies and stale data retained after a pop.
        let mut region = self
            .pc24_region(read_mask, write_mask)
            .ok_or(Pc24BoundaryError::Input)?;
        region.values = values;
        region.origins = origins;
        region.status = status;
        region.tags = tags;
        region.initialized = initialized;
        region.dirty_values = dirty;
        region.popped = popped;
        if pointer_flags & RECORD_INSTRUCTION != 0 {
            region.record_instruction(
                u32_at(bytes, INSTRUCTION_IP),
                u16_at(bytes, INSTRUCTION_CS),
                u16_at(bytes, INSTRUCTION_OPCODE),
            );
        }
        if pointer_flags & RECORD_OPERAND != 0 {
            region.record_operand(
                u32_at(bytes, OPERAND_OFFSET),
                u16_at(bytes, OPERAND_SELECTOR),
            );
        }
        region.commit();
        Ok(())
    }

    /// Borrowed convenience wrapper when the caller can keep exclusive FPU
    /// access for the entire foreign invocation.
    pub fn pc24_boundary(&mut self, read_mask: u8, write_mask: u8) -> Option<Pc24Boundary<'_>> {
        let mut bytes = [0; SIZE];
        if !self.write_pc24_boundary(read_mask, write_mask, &mut bytes) {
            return None;
        }
        Some(Pc24Boundary {
            owner: self,
            read_mask,
            write_mask,
            bytes,
        })
    }
}

fn valid_new_value(value: f64, origin: u8) -> bool {
    let magnitude = value.to_bits() & 0x7fff_ffff_ffff_ffff;
    let exponent = (magnitude >> 52) as i32;
    if magnitude != 0 && (exponent == 0 || (exponent - 1023).unsigned_abs() > 2 * fast::LIMIT + 2) {
        return false;
    }
    match origin {
        ORIGIN_ARITHMETIC => magnitude & ((1 << 29) - 1) == 0,
        ORIGIN_REAL32 => fast::real(u64::from((value as f32).to_bits()), true)
            .is_some_and(|roundtrip| roundtrip.to_bits() == value.to_bits()),
        ORIGIN_INTEGER32 => f64::from(value as i32).to_bits() == value.to_bits(),
        _ => false,
    }
}

impl Pc24Region<'_> {
    /// Export the accepted local prefix to the mutable output suffix of a
    /// prepared boundary. The immutable input prefix is never changed. Useful
    /// for shared Rust/WASM kernels; generated code may write these fields
    /// directly using the same public constants. Wrong lengths change nothing.
    pub fn write_pc24_output(&self, bytes: &mut [u8]) -> bool {
        if bytes.len() != SIZE {
            return false;
        }
        bytes[STATUS..].fill(0);
        put16(bytes, STATUS, self.status);
        put16(bytes, TAGS, self.tags);
        bytes[INITIALIZED] = self.initialized;
        bytes[DIRTY_VALUES] = self.dirty_values;
        bytes[POPPED] = self.popped;
        for slot in 0..8 {
            bytes[ORIGINS + slot] = self.origins[slot];
            put_value(bytes, VALUES + slot * 8, self.values[slot]);
        }
        if let Some((ip, cs, opcode)) = self.instruction {
            bytes[POINTER_FLAGS] |= RECORD_INSTRUCTION;
            put32(bytes, INSTRUCTION_IP, ip);
            put16(bytes, INSTRUCTION_CS, cs);
            put16(bytes, INSTRUCTION_OPCODE, opcode & 0x7ff);
        }
        if let Some((offset, selector)) = self.operand {
            bytes[POINTER_FLAGS] |= RECORD_OPERAND;
            put32(bytes, OPERAND_OFFSET, offset);
            put16(bytes, OPERAND_SELECTOR, selector);
        }
        true
    }
}

pub struct Pc24Boundary<'a> {
    owner: &'a mut X87,
    read_mask: u8,
    write_mask: u8,
    bytes: [u8; SIZE],
}
impl Pc24Boundary<'_> {
    pub fn bytes(&self) -> &[u8; SIZE] {
        &self.bytes
    }
    pub fn bytes_mut(&mut self) -> &mut [u8; SIZE] {
        &mut self.bytes
    }
    pub fn commit(self) -> Result<(), Pc24BoundaryError> {
        self.owner
            .commit_pc24_boundary(self.read_mask, self.write_mask, &self.bytes)
    }
}

#[cfg(test)]
#[path = "pc24_boundary_tests.rs"]
mod tests;
