//! Deterministic randomized differential checks for local PC24 regions.
//!
//! This is a standalone WASM diagnostic, not CPU instruction decoding or
//! guest-memory/device admission. Every accepted region operation is compared
//! with the ordinary retained-state X87 method before the next boundary.

use doslet_state::{State, prost::Message};
use doslet_x87::{Pc24Region, X87};

const MAX_BATCHES: u32 = 4096;
const MAX_OPERATIONS_PER_BATCH: u32 = 32;
const LOAD: u8 = 0;
const MEMORY: u8 = 1;
const TOP: u8 = 2;
const INTO: u8 = 3;
const DUPLICATE: u8 = 4;
const EXCHANGE: u8 = 5;
const STORE_POP: u8 = 6;
const REFUSAL: u8 = 7;

#[derive(Clone, Copy)]
enum Action {
    Load(u32),
    Memory(u32, u8),
    Top(u8, u8),
    Into(u8, u8, bool),
    Duplicate(u8),
    Exchange(u8),
    StorePop,
    Refusal,
}

#[derive(Clone, Copy)]
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 32) as u32
    }

    fn real32(&mut self) -> u32 {
        let bits = self.next();
        // Signed normal values in [0.5, 4), keeping most generated arithmetic
        // comfortably inside the PC24 local-domain bound.
        (bits & 0x807f_ffff) | (((126 + (bits >> 28) % 3) & 0xff) << 23)
    }
}

fn serialized_equal(left: &X87, right: &X87) -> bool {
    left.save().encode_to_vec() == right.save().encode_to_vec()
}

fn initial(control: u16, values: [u32; 2], sequence: &mut u32) -> Option<X87> {
    let mut fpu = X87::new();
    fpu.load_control(control);
    for bits in values {
        let ip = 0x1000_u32.checked_add((*sequence).checked_mul(4)?)?;
        let operand = 0x2000_u32.checked_add((*sequence).checked_mul(4)?)?;
        fpu.load_real(u64::from(bits), true).ok()?;
        fpu.record_instruction(ip, 0, 0x106);
        fpu.record_operand(operand, 0);
        *sequence = (*sequence).checked_add(1)?;
    }
    Some(fpu)
}

fn nonempty_physical_slots(fpu: &X87) -> u8 {
    let tags = fpu.tags();
    let mut mask = 0;
    for slot in 0..8 {
        if (tags >> (slot * 2)) & 3 != 3 {
            mask |= 1 << slot;
        }
    }
    mask
}

fn active_logical_slots(fpu: &X87) -> ([u8; 8], usize) {
    let mut slots = [0; 8];
    let mut count = 0;
    for index in 0..8 {
        if !fpu.is_empty(index) {
            slots[count] = index;
            count += 1;
        }
    }
    (slots, count)
}

fn action(fpu: &X87, rng: &mut Lcg) -> Action {
    let bits = rng.next();
    let (active, count) = active_logical_slots(fpu);
    let stack_push_slot_empty = fpu.is_empty(7);
    match (bits % 8) as u8 {
        LOAD if stack_push_slot_empty => Action::Load(rng.real32()),
        MEMORY => {
            const OPS: [u8; 6] = [0, 1, 4, 5, 6, 7];
            Action::Memory(rng.real32(), OPS[(bits as usize >> 3) % OPS.len()])
        }
        TOP if count != 0 => {
            const OPS: [u8; 6] = [0, 1, 4, 5, 6, 7];
            Action::Top(
                active[(bits as usize >> 4) % count],
                OPS[(bits as usize >> 8) % OPS.len()],
            )
        }
        INTO if count != 0 => {
            const OPS: [u8; 6] = [0, 1, 4, 5, 6, 7];
            Action::Into(
                active[(bits as usize >> 4) % count],
                OPS[(bits as usize >> 8) % OPS.len()],
                bits & 0x1000 != 0,
            )
        }
        DUPLICATE if stack_push_slot_empty && count != 0 => {
            Action::Duplicate(active[(bits as usize >> 5) % count])
        }
        EXCHANGE if count != 0 => Action::Exchange(active[(bits as usize >> 5) % count]),
        STORE_POP => Action::StorePop,
        REFUSAL => Action::Refusal,
        // Keep the generated stream active when an operation is unavailable
        // for the current stack shape.
        _ if count != 0 => {
            const OPS: [u8; 6] = [0, 1, 4, 5, 6, 7];
            Action::Top(
                active[(bits as usize >> 4) % count],
                OPS[(bits as usize >> 8) % OPS.len()],
            )
        }
        _ => Action::Load(rng.real32()),
    }
}

fn metadata(sequence: u32) -> Option<(u32, u32)> {
    let ip = 0x1000_u32.checked_add(sequence.checked_mul(4)?)?;
    let operand = 0x2000_u32.checked_add(sequence.checked_mul(4)?)?;
    Some((ip, operand))
}

fn opcode(action: Action) -> u16 {
    match action {
        Action::Load(_) => 0x106,
        Action::Memory(_, operation) => 0x800 | (u16::from(operation) << 3) | 6,
        Action::Top(index, operation) => 0x900 | (u16::from(operation) << 3) | u16::from(index),
        Action::Into(index, operation, _) => 0xa00 | (u16::from(operation) << 3) | u16::from(index),
        Action::Duplicate(index) => 0xb00 | u16::from(index),
        Action::Exchange(index) => 0xc00 | u16::from(index),
        Action::StorePop => 0x31e,
        Action::Refusal => 0x806,
    }
}

fn local_apply(region: &mut Pc24Region<'_>, action: Action, ip: u32, operand: u32) -> Option<u32> {
    let value = match action {
        Action::Load(bits) => {
            if !region.load_real32(bits) {
                return None;
            }
            0
        }
        Action::Memory(bits, operation) => {
            if !region.arithmetic_real32(bits, operation) {
                return None;
            }
            0
        }
        Action::Top(index, operation) => {
            if !region.arithmetic_top(index, operation) {
                return None;
            }
            0
        }
        Action::Into(index, operation, pop) => {
            if !region.arithmetic_into(index, operation, pop) {
                return None;
            }
            0
        }
        Action::Duplicate(index) => {
            if !region.duplicate(index) {
                return None;
            }
            0
        }
        Action::Exchange(index) => {
            if !region.exchange(index) {
                return None;
            }
            0
        }
        Action::StorePop => region.store_integer32()?,
        Action::Refusal => {
            // Infinity must decline transactionally; the reference execution
            // is intentionally skipped and the owner resumes from the prefix.
            if !region.arithmetic_real32(0x7f80_0000, 6) {
                return None;
            }
            0
        }
    };
    region.record_instruction(ip, 0, opcode(action));
    if matches!(
        action,
        Action::Load(_) | Action::Memory(_, _) | Action::StorePop
    ) {
        region.record_operand(operand, 0);
    }
    if matches!(action, Action::StorePop) && !region.pop() {
        return None;
    }
    Some(value)
}

fn reference_apply(fpu: &mut X87, action: Action, ip: u32, operand: u32) -> Option<u32> {
    let value = match action {
        Action::Load(bits) => {
            fpu.load_real(u64::from(bits), true).ok()?;
            0
        }
        Action::Memory(bits, operation) => {
            fpu.arithmetic_real(u64::from(bits), true, operation).ok()?;
            0
        }
        Action::Top(index, operation) => {
            fpu.arithmetic_top(index, operation).ok()?;
            0
        }
        Action::Into(index, operation, pop) => {
            fpu.arithmetic_into(index, operation, pop).ok()?;
            0
        }
        Action::Duplicate(index) => {
            fpu.duplicate(index).ok()?;
            0
        }
        Action::Exchange(index) => {
            fpu.exchange(index).ok()?;
            0
        }
        Action::StorePop => fpu.store_integer(32).ok()? as u32,
        Action::Refusal => return None,
    };
    fpu.record_instruction(ip, 0, opcode(action));
    if matches!(
        action,
        Action::Load(_) | Action::Memory(_, _) | Action::StorePop
    ) {
        fpu.record_operand(operand, 0);
    }
    if matches!(action, Action::StorePop) {
        fpu.pop().ok()?;
    }
    Some(value)
}

fn reseed(control: u16, rng: &mut Lcg, sequence: &mut u32) -> Option<(X87, X87)> {
    let values = [rng.real32(), rng.real32()];
    let base = *sequence;
    let local = initial(control, values, sequence)?;
    let mut reference_sequence = base;
    let reference = initial(control, values, &mut reference_sequence)?;
    Some((local, reference))
}

/// Compare bounded random PC24 operation batches against the retained-state
/// implementation. Returns false on any refusal mismatch, state difference,
/// unsupported input contract, or invalid batch bound.
pub fn check(seed: u64, control: u16, batches: u32) -> bool {
    if !(1..=MAX_BATCHES).contains(&batches) || control & 0x3f != 0x3f || (control >> 8) & 3 != 0 {
        return false;
    }
    let mut rng = Lcg(seed);
    let mut sequence = 0;
    for _ in 0..batches {
        let Some((mut local, mut reference)) = reseed(control, &mut rng, &mut sequence) else {
            return false;
        };
        let read_mask = nonempty_physical_slots(&local);
        let Some(mut region) = local.pc24_region(read_mask, u8::MAX) else {
            return false;
        };
        let count = 1 + rng.next() % MAX_OPERATIONS_PER_BATCH;
        let mut valid = true;
        for operation_index in 0..count {
            let selected = if operation_index == 0 {
                Action::Memory(1.0f32.to_bits(), 0)
            } else {
                action(&reference, &mut rng)
            };
            let Some((ip, operand)) = metadata(sequence) else {
                valid = false;
                break;
            };
            let Some(next_sequence) = sequence.checked_add(1) else {
                valid = false;
                break;
            };
            sequence = next_sequence;
            let Some(local_value) = local_apply(&mut region, selected, ip, operand) else {
                if operation_index == 0 {
                    valid = false;
                }
                break;
            };
            let Some(reference_value) = reference_apply(&mut reference, selected, ip, operand)
            else {
                valid = false;
                break;
            };
            if local_value != reference_value {
                valid = false;
                break;
            }
        }
        region.commit();
        if !valid || !serialized_equal(&local, &reference) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn deterministic_programs_match_reference_at_region_exits() {
        for rounding in 0..4 {
            for seed in [1u64, 0x913a_845b_c70d_2fe1, u64::MAX] {
                assert!(
                    check(seed, 0x7f | (rounding << 10), 256),
                    "seed={seed:x}, RC={rounding}"
                );
            }
        }
        assert!(!check(1, 0x7f, 0));
        assert!(!check(1, 0x7f, 4097));
        assert!(!check(1, 0x37f, 256));
        assert!(!check(1, 0x7e, 256));
    }
}
