use super::*;
use crate::local::tests::{Step, empty, local, occupied, reference};

fn output(seed: &X87, read: u8, write: u8, steps: &[Step]) -> ([u8; SIZE], X87) {
    let mut bytes = [0xa5; SIZE];
    assert!(seed.write_pc24_boundary(read, write, &mut bytes));
    let input: [u8; STATUS] = bytes[..STATUS].try_into().unwrap();
    let mut shadow = seed.clone();
    let mut expected = seed.clone();
    let mut region = shadow.pc24_region(read, write).unwrap();
    for (position, &step) in steps.iter().enumerate() {
        let Some(result) = local(&mut region, step) else {
            break;
        };
        assert_eq!(result, reference(&mut expected, step).unwrap());
        if !matches!(step, Step::Status) {
            region.record_instruction(0xdead0000 + position as u32, 0x23, 0xf123 ^ position as u16);
            region.record_operand(0xbeef0000 + position as u32, 0x37);
            expected.record_instruction(
                0xdead0000 + position as u32,
                0x23,
                0xf123 ^ position as u16,
            );
            expected.record_operand(0xbeef0000 + position as u32, 0x37);
        }
    }
    assert!(region.write_pc24_output(&mut bytes));
    assert_eq!(bytes[..STATUS], input);
    // The exporter does not materialize the shadow FPU either.
    assert!(shadow.bitwise_eq(seed));
    (bytes, expected)
}

#[test]
fn pc24_boundary_explicit_bytes_selected_reads_and_noop_roundtrip() {
    let mut seed = empty(2);
    seed.push(1.5).unwrap();
    let original = seed.clone();
    let mut bytes = [0xa5; SIZE];
    assert!(seed.write_pc24_boundary(0x80, 0xc0, &mut bytes));
    assert_eq!(&bytes[..4], b"X24R");
    assert_eq!(&bytes[VERSION..VERSION + 4], &[2, 0, 0, 0]);
    assert_eq!(&bytes[SIZE_FIELD..SIZE_FIELD + 4], &[192, 0, 0, 0]);
    assert_eq!(&bytes[READ_MASK..WRITE_MASK + 1], &[0x80, 0xc0]);
    assert_eq!(&bytes[CONTROL..CONTROL + 2], &0x087fu16.to_le_bytes());
    for slot in 0..8 {
        assert_eq!(
            value_at(&bytes, INITIAL_VALUES + slot * 8).to_bits(),
            if slot == 7 { 1.5f64.to_bits() } else { 0 }
        );
    }
    seed.commit_pc24_boundary(0x80, 0xc0, &bytes).unwrap();
    assert!(seed.bitwise_eq(&original));
    let wrapper = seed.pc24_boundary(0x80, 0xc0).unwrap();
    assert_eq!(wrapper.bytes(), &bytes);
    wrapper.commit().unwrap();
    assert!(seed.bitwise_eq(&original));
    {
        let mut discarded = seed.pc24_boundary(0x80, 0xc0).unwrap();
        discarded.bytes_mut()[DIRTY_VALUES] = 0xff;
    }
    assert!(seed.bitwise_eq(&original));
}

#[test]
fn pc24_boundary_all_prefixes_moves_and_stale_empty_data_match_reference() {
    let programs: [&[Step]; 3] = [
        &[
            Step::Load(0x3fc00000),
            Step::Real(0x3f800001, 1),
            Step::Real(0x3f000000, 0),
            Step::Store,
            Step::Pop,
        ],
        &[
            Step::Duplicate(0),
            Step::Exchange(1),
            Step::Real(0x3f800000, 0),
            Step::Exchange(1),
            Step::Pop,
            Step::Pop,
        ],
        &[
            Step::Top(1, 5),
            Step::Exchange(1),
            Step::Into(1, 6, true),
            Step::Duplicate(0),
            Step::Store,
            Step::Pop,
        ],
    ];
    for rc in 0..4 {
        for (index, program) in programs.into_iter().enumerate() {
            let mut seed = empty(rc);
            if index != 0 {
                seed.push(-0.0).unwrap();
            }
            if index == 2 {
                seed.push(1.5).unwrap();
            }
            let read = occupied(&seed);
            for end in 0..=program.len() {
                let (bytes, expected) = output(&seed, read, 0xff, &program[..end]);
                let mut actual = seed.clone();
                actual.commit_pc24_boundary(read, 0xff, &bytes).unwrap();
                assert!(
                    actual.bitwise_eq(&expected),
                    "program {index}, prefix {end}, rc {rc}\nactual {actual:?}\nexpected {expected:?}"
                );
            }
        }
    }
}

#[test]
fn pc24_boundary_fild32_retains_integer_precision_and_zero_provenance() {
    for rc in 0..4 {
        for integer in [i32::MIN, -16_777_217, -1, 0, 1, 16_777_217, i32::MAX] {
            let seed = empty(rc);
            let steps = [Step::Integer(integer), Step::Store, Step::Pop];
            for end in 0..=steps.len() {
                let (bytes, expected) = output(&seed, 0, 0x80, &steps[..end]);
                if end != 0 {
                    assert_eq!(bytes[ORIGINS + 7], ORIGIN_INTEGER32);
                    assert_eq!(value_at(&bytes, VALUES + 7 * 8), f64::from(integer));
                }
                let mut actual = seed.clone();
                actual.commit_pc24_boundary(0, 0x80, &bytes).unwrap();
                assert!(actual.bitwise_eq(&expected));
            }
            let (bytes, expected) = output(
                &seed,
                0,
                0x80,
                &[
                    Step::Integer(integer),
                    Step::Real(0x3f800000, 1),
                    Step::Store,
                    Step::Pop,
                ],
            );
            let mut actual = seed.clone();
            actual.commit_pc24_boundary(0, 0x80, &bytes).unwrap();
            assert!(actual.bitwise_eq(&expected));
        }
    }
}

fn rejected(seed: &X87, read: u8, write: u8, bytes: &[u8]) {
    let mut owner = seed.clone();
    assert!(owner.commit_pc24_boundary(read, write, bytes).is_err());
    assert!(owner.bitwise_eq(seed));
}

#[test]
fn pc24_boundary_invalid_headers_permissions_state_and_values_are_transactional() {
    let seed = empty(0);
    let (valid, _) = output(
        &seed,
        0,
        0x80,
        &[
            Step::Load(0x3fc00000),
            Step::Real(0x3f800001, 1),
            Step::Store,
            Step::Pop,
        ],
    );
    for offset in 0..STATUS {
        let mut invalid = valid;
        invalid[offset] ^= 1;
        rejected(&seed, 0, 0x80, &invalid);
    }
    for offset in 182..SIZE {
        let mut invalid = valid;
        invalid[offset] = 1;
        rejected(&seed, 0, 0x80, &invalid);
    }
    for (offset, mask) in [
        (STATUS + 1, 0x80),
        (TAGS, 1),
        (DIRTY_VALUES, 1),
        (POPPED, 1),
        (INITIALIZED, 1),
        (POINTER_FLAGS, 4),
        (INSTRUCTION_OPCODE + 1, 8),
    ] {
        let mut invalid = valid;
        invalid[offset] ^= mask;
        rejected(&seed, 0, 0x80, &invalid);
    }
    for origin in [0, 7, 11, 255] {
        let mut invalid = valid;
        invalid[ORIGINS + 7] = origin;
        rejected(&seed, 0, 0x80, &invalid);
    }
    for value in [
        f64::NAN,
        f64::INFINITY,
        f64::from_bits(1),
        1e300,
        1.00000000001,
    ] {
        let mut invalid = valid;
        put_value(&mut invalid, VALUES + 7 * 8, value);
        rejected(&seed, 0, 0x80, &invalid);
    }
    let mut invalid = valid;
    put_value(&mut invalid, VALUES, 1.0);
    rejected(&seed, 0, 0x80, &invalid); // Untouched/unimported slot must stay canonical.
    for (origin, value) in [(ORIGIN_REAL32, 1.00000000001), (ORIGIN_INTEGER32, 1.5)] {
        let mut invalid = valid;
        invalid[ORIGINS + 7] = origin;
        put_value(&mut invalid, VALUES + 7 * 8, value);
        rejected(&seed, 0, 0x80, &invalid);
    }
    let mut invalid = valid;
    invalid[POINTER_FLAGS] &= !RECORD_INSTRUCTION;
    rejected(&seed, 0, 0x80, &invalid);
    let mut invalid = valid;
    invalid[POINTER_FLAGS] &= !RECORD_OPERAND;
    rejected(&seed, 0, 0x80, &invalid);
    rejected(&seed, 0, 0x80, &valid[..SIZE - 1]);
    rejected(&seed, 0, 0, &valid); // Independent permission mask cannot be widened.
    let mut changed = seed.clone();
    changed.restore_status(changed.status() ^ 0x100);
    rejected(&changed, 0, 0x80, &valid); // Live owner no longer matches the input.
}

#[test]
fn pc24_boundary_export_refusals_and_sticky_precision_preserve_state() {
    let mut seed = empty(0);
    let mut bytes = [0xa5; SIZE];
    assert!(!seed.write_pc24_boundary(1, 0xff, &mut bytes));
    assert_eq!(bytes, [0xa5; SIZE]);
    assert!(!seed.write_pc24_boundary(0, 0xff, &mut bytes[..SIZE - 1]));
    assert_eq!(bytes, [0xa5; SIZE]);
    seed.load_control(0x037f);
    assert!(!seed.write_pc24_boundary(0, 0xff, &mut bytes));
    assert_eq!(bytes, [0xa5; SIZE]);
    seed.load_control(0x007f);
    seed.restore_status(seed.status() | 0x20);
    assert!(seed.write_pc24_boundary(0, 0xff, &mut bytes));
    bytes[STATUS] &= !0x20;
    rejected(&seed, 0, 0xff, &bytes);
}

#[test]
fn pc24_boundary_comparison_and_register_copy_prefixes_preserve_complete_state() {
    for rc in 0..4 {
        for pops in 0..=2 {
            for (lhs, rhs) in [(1.0, 2.0), (2.0, 1.0), (-0.0, 0.0)] {
                let mut seed = empty(rc);
                seed.rotate_top(true);
                seed.push(rhs).unwrap();
                seed.push(lhs).unwrap();
                let read = occupied(&seed);
                let steps = [
                    Step::CompareRegister(1, pops),
                    Step::Status,
                    Step::StoreRegister(7, true),
                    Step::Status,
                ];
                for end in 0..=steps.len() {
                    let (bytes, expected) = output(&seed, read, 0xff, &steps[..end]);
                    let mut actual = seed.clone();
                    actual.commit_pc24_boundary(read, 0xff, &bytes).unwrap();
                    assert!(actual.bitwise_eq(&expected));
                    if end == 2 {
                        assert_eq!(u32_at(&bytes, INSTRUCTION_IP), 0xdead0000); // FNSTSW kept compare FIP.
                    }
                }
            }
        }
    }
}

#[test]
fn pc24_boundary_ordered_condition_changes_are_allowed_but_forgeries_are_rejected() {
    let mut seed = empty(0);
    seed.restore_status(seed.status() | 0x20);
    let mut original = [0; SIZE];
    assert!(seed.write_pc24_boundary(0, 0, &mut original));
    for condition in [0, 0x100, 0x4000] {
        let mut bytes = original;
        let status = (seed.status() & !0x4700) | condition;
        put16(&mut bytes, STATUS, status);
        let mut actual = seed.clone();
        actual.commit_pc24_boundary(0, 0, &bytes).unwrap();
        let mut expected = seed.clone();
        expected.restore_status(status);
        assert!(actual.bitwise_eq(&expected));
        bytes[STATUS] ^= 0x20; // Sticky precision cannot be cleared by comparison.
        rejected(&seed, 0, 0, &bytes);
    }
    for bit in [1u16, 2, 4, 8, 0x10, 0x40, 0x80, 0x8000] {
        let mut bytes = original;
        put16(&mut bytes, STATUS, seed.status() ^ bit);
        rejected(&seed, 0, 0, &bytes);
    }
    for condition in [0x400, 0x500, 0x4100, 0x4400] {
        let mut bytes = original;
        put16(&mut bytes, STATUS, (seed.status() & !0x4500) | condition);
        rejected(&seed, 0, 0, &bytes);
    }
    let mut old = original;
    put32(&mut old, VERSION, 1);
    rejected(&seed, 0, 0, &old);
    // An inherited unordered/diagnostic condition can remain unchanged when
    // there was no comparison. Guarded comparisons cannot newly create it.
    let mut clean = seed.clone();
    clean.restore_status(clean.status() & !0x4500);
    let mut bytes = [0; SIZE];
    assert!(clean.write_pc24_boundary(0, 0, &mut bytes));
    put16(&mut bytes, STATUS, clean.status() | 0x4500);
    rejected(&clean, 0, 0, &bytes);
}
