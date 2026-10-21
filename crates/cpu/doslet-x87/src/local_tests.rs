use super::*;
use crate::{ArithmeticError, Constant};

#[derive(Clone, Copy, Debug)]
pub(super) enum Step {
    Load(u32),
    Integer(i32),
    Real(u32, u8),
    Top(u8, u8),
    Into(u8, u8, bool),
    Duplicate(u8),
    Exchange(u8),
    StoreRegister(u8, bool),
    CompareReal(u32, bool),
    CompareRegister(u8, u8),
    Status,
    Store,
    Pop,
}

pub(super) fn reference(fpu: &mut X87, step: Step) -> Result<Option<u32>, ArithmeticError> {
    match step {
        Step::Load(bits) => fpu.load_real(u64::from(bits), true)?,
        Step::Integer(value) => fpu
            .push_integer(i64::from(value))
            .map_err(ArithmeticError::Stack)?,
        Step::Real(bits, op) => fpu.arithmetic_real(u64::from(bits), true, op)?,
        Step::Top(index, op) => fpu.arithmetic_top(index, op)?,
        Step::Into(index, op, pop) => fpu.arithmetic_into(index, op, pop)?,
        Step::Duplicate(index) => fpu.duplicate(index).map_err(ArithmeticError::Stack)?,
        Step::Exchange(index) => fpu.exchange(index).map_err(ArithmeticError::Stack)?,
        Step::StoreRegister(index, pop) => fpu
            .store_register(index, pop)
            .map_err(ArithmeticError::Stack)?,
        Step::CompareReal(bits, pop) => fpu.compare_memory(f64::from(f32::from_bits(bits)), pop)?,
        Step::CompareRegister(index, pops) => fpu.compare_register(index, pops)?,
        Step::Status => return Ok(Some(u32::from(fpu.status()))),
        Step::Store => return fpu.store_integer(32).map(|value| Some(value as u32)),
        Step::Pop => fpu.pop().map_err(ArithmeticError::Stack)?,
    }
    Ok(None)
}

pub(super) fn local(region: &mut Pc24Region<'_>, step: Step) -> Option<Option<u32>> {
    let accepted = match step {
        Step::Load(bits) => region.load_real32(bits),
        Step::Integer(value) => region.load_integer32(value),
        Step::Real(bits, op) => region.arithmetic_real32(bits, op),
        Step::Top(index, op) => region.arithmetic_top(index, op),
        Step::Into(index, op, pop) => region.arithmetic_into(index, op, pop),
        Step::Duplicate(index) => region.duplicate(index),
        Step::Exchange(index) => region.exchange(index),
        Step::StoreRegister(index, pop) => region.store_register(index, pop),
        Step::CompareReal(bits, pop) => region.compare_real32(bits, pop),
        Step::CompareRegister(index, pops) => region.compare_register(index, pops),
        Step::Status => return Some(Some(u32::from(region.status()))),
        Step::Store => return region.store_integer32().map(Some),
        Step::Pop => region.pop(),
    };
    accepted.then_some(None)
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    values: [u64; 8],
    status: u16,
    tags: u16,
    initialized: u8,
    dirty: u8,
    popped: u8,
    origins: [u8; 8],
    instruction: Option<(u32, u16, u16)>,
    operand: Option<(u32, u16)>,
}
fn snapshot(region: &Pc24Region<'_>) -> Snapshot {
    Snapshot {
        values: region.values.map(f64::to_bits),
        status: region.status,
        tags: region.tags,
        initialized: region.initialized,
        dirty: region.dirty_values,
        popped: region.popped,
        origins: region.origins,
        instruction: region.instruction,
        operand: region.operand,
    }
}
pub(super) fn occupied(fpu: &X87) -> u8 {
    (0..8).fold(0, |mask, slot| {
        mask | if (fpu.tags >> (slot * 2)) & 3 != 3 {
            1 << slot
        } else {
            0
        }
    })
}

fn pointer(fpu: &mut X87, position: usize) {
    fpu.record_instruction(0x12340000 + position as u32, 0x23, 0x7ff ^ position as u16);
    fpu.record_operand(0xfedc0000 + position as u32, 0x37);
}

/// Re-enter from the same owner for every possible prefix, including a prefix
/// immediately before a refusal. Every accepted operation is checked against
/// the integer/extended reference, never against another fast path.
fn prefixes(seed: &X87, steps: &[Step]) -> usize {
    let mut accepted = 0;
    for end in 0..=steps.len() {
        let mut actual = seed.clone();
        let mut expected = seed.clone();
        let mask = occupied(seed);
        let mut region = actual.pc24_region(mask, 0xff).expect("valid region seed");
        for (position, &step) in steps[..end].iter().enumerate() {
            let before = snapshot(&region);
            let Some(value) = local(&mut region, step) else {
                assert_eq!(snapshot(&region), before, "refused {step:?}");
                break;
            };
            assert_eq!(value, reference(&mut expected, step).unwrap(), "{step:?}");
            if !matches!(step, Step::Status) {
                region.record_instruction(
                    0x12340000 + position as u32,
                    0x23,
                    0x7ff ^ position as u16,
                );
                region.record_operand(0xfedc0000 + position as u32, 0x37);
                pointer(&mut expected, position);
            }
            accepted += 1;
        }
        // Even accepted local work has not touched the parked original owner.
        assert!(region.owner.bitwise_eq(seed));
        region.commit();
        assert!(
            actual.bitwise_eq(&expected),
            "prefix {end}, steps {steps:?}\nactual {actual:?}\nexpected {expected:?}"
        );
    }
    accepted
}

pub(super) fn empty(rc: u16) -> X87 {
    let mut fpu = X87::new();
    // Empty slots can retain nonfinite diagnostics, noncanonical zero finite
    // exponents and raw encodings. Unselected data must survive every commit.
    for value in [
        f64::NAN,
        f64::INFINITY,
        -0.0,
        3.5,
        -7.0,
        0.0,
        1e200,
        -1e-200,
    ] {
        fpu.push(value).unwrap();
    }
    fpu.initialize();
    fpu.load_control(0x007f | (rc << 10));
    fpu.restore_status(0x4500);
    fpu.record_instruction(0xabcdef, 0x10, 0x456);
    fpu.record_operand(0x987654, 0x20);
    fpu
}

#[test]
fn pc24_region_real32_sequence_and_all_prefixes_preserve_complete_state() {
    for rc in 0..4 {
        for value in [
            0u32, 0x80000000, 0x3f800000, 0x3f000000, 0xbf000000, 0x4b800001, 0xcb800001,
        ] {
            let seed = empty(rc);
            assert!(
                prefixes(
                    &seed,
                    &[
                        Step::Load(value),
                        Step::Real(0x3f800001, 1),
                        Step::Real(0x3f000000, 0),
                        Step::Store,
                        Step::Pop,
                    ]
                ) > 0
            );
        }
    }
}

#[test]
fn pc24_region_rounding_and_retained_zero_origins_match_reference() {
    for rc in 0..4 {
        for status in [0x4500, 0xc7ff] {
            for bits in [0, 0x80000000] {
                let mut seed = empty(rc);
                seed.restore_status(status);
                for operation in [0, 1, 4, 5, 6, 7] {
                    prefixes(
                        &seed,
                        &[Step::Load(bits), Step::Real(bits, operation), Step::Pop],
                    );
                }
                prefixes(
                    &seed,
                    &[
                        Step::Load(bits),
                        Step::Duplicate(0),
                        Step::Exchange(1),
                        Step::Pop,
                        Step::Pop,
                    ],
                );
            }
        }
        // Imported zeros keep their operation-specific retained exponent when
        // moved, even if the original physical source is subsequently changed.
        for kind in 0..3 {
            let mut seed = empty(rc);
            match kind {
                0 => seed.push(-0.0).unwrap(),
                1 => seed.load_real(0x80000000, true).unwrap(),
                _ => seed.load_constant(Constant::Zero).unwrap(),
            }
            prefixes(
                &seed,
                &[
                    Step::Duplicate(0),
                    Step::Exchange(1),
                    Step::Real(0x3f800000, 0),
                    Step::Exchange(1),
                    Step::Pop,
                    Step::Pop,
                ],
            );
        }
    }
}

#[test]
fn pc24_region_register_arithmetic_aliases_pop_and_copy_cycles_match_reference() {
    for rc in 0..4 {
        for operation in [0, 1, 4, 5, 6, 7] {
            for index in [0, 1, 2] {
                for pop in [false, true] {
                    let mut seed = empty(rc);
                    for value in [1.0000000596046448, -1.0000001192092896, 1.5] {
                        seed.push(value).unwrap();
                    }
                    prefixes(
                        &seed,
                        &[
                            Step::Top(index, operation),
                            Step::Exchange(2),
                            Step::Into(index, operation, pop),
                            Step::Duplicate(0),
                            Step::Exchange(1),
                        ],
                    );
                }
            }
        }
    }
}

#[test]
fn pc24_region_integer_conversion_limits_ties_signed_zero_and_no_early_pop() {
    for rc in 0..4 {
        for value in [
            -2147483649.0,
            -2147483648.5,
            -2147483648.0,
            -1.5,
            -0.5,
            -0.0,
            0.0,
            0.5,
            1.5,
            2147483647.0,
            2147483647.5,
            2147483648.0,
        ] {
            let mut actual = empty(rc);
            actual.push(value).unwrap();
            let seed = actual.clone();
            let mut expected = actual.clone();
            let reference = expected.store_integer(32).unwrap() as u32;
            let valid = expected.status & 1 == 0;
            let mut region = actual.pc24_region(0x80, 0x80).unwrap();
            let before = snapshot(&region);
            let result = region.store_integer32();
            if valid {
                assert_eq!(result, Some(reference));
                region.commit();
                assert!(actual.bitwise_eq(&expected)); // FIST alone leaves TOP/tag/value intact.
            } else {
                assert_eq!(result, None);
                assert_eq!(snapshot(&region), before);
                region.commit();
                assert!(actual.bitwise_eq(&seed));
            }
        }
    }
}

#[test]
fn pc24_region_refusals_masks_domains_and_dropped_work_are_transactional() {
    for control in [0x037f, 0x027f, 0x017f, 0x007e, 0x005f] {
        let mut fpu = empty(0);
        fpu.load_control(control);
        let before = fpu.clone();
        assert!(fpu.pc24_region(0, 0xff).is_none());
        assert!(fpu.bitwise_eq(&before));
    }
    for value in [f64::NAN, f64::INFINITY, f64::from_bits(1), 1e200, 1e-200] {
        let mut fpu = empty(0);
        fpu.push(value).unwrap();
        let before = fpu.clone();
        assert!(fpu.pc24_region(0x80, 0xff).is_none());
        assert!(fpu.bitwise_eq(&before));
    }
    let mut fpu = empty(0);
    assert!(fpu.pc24_region(1, 0xff).is_none()); // Declared empty initial read.
    let seed = fpu.clone();
    {
        let mut region = fpu.pc24_region(0, 0x80).unwrap();
        for step in [
            Step::Pop,
            Step::Store,
            Step::Top(0, 0),
            Step::Duplicate(0),
            Step::Exchange(0),
            Step::Load(1),
            Step::Load(0x7f800000),
            Step::Load(0x7fc00000),
        ] {
            let before = snapshot(&region);
            assert!(local(&mut region, step).is_none());
            assert_eq!(snapshot(&region), before);
        }
        assert!(region.load_real32(0x3f800000));
        for step in [
            Step::Load(0x3f800000),
            Step::Duplicate(0),
            Step::Exchange(1),
            Step::Top(8, 0),
            Step::Into(8, 0, true),
            Step::Real(0, 6),
            Step::Real(1, 0),
            Step::Real(0x3f800000, 2),
        ] {
            let before = snapshot(&region);
            assert!(local(&mut region, step).is_none(), "{step:?}");
            assert_eq!(snapshot(&region), before);
        }
        region.record_instruction(1, 2, 3);
        region.record_operand(4, 5);
        // Explicitly discard all local work and pending pointers.
    }
    assert!(fpu.bitwise_eq(&seed));
    let mut imported = empty(0);
    imported.push(1.0).unwrap();
    let before = imported.clone();
    let mut read_only = imported.pc24_region(0x80, 0).unwrap();
    assert!(!read_only.arithmetic_real32(0x3f800000, 0));
    assert!(!read_only.pop());
    read_only.commit();
    assert!(imported.bitwise_eq(&before));
}

fn next(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

#[test]
fn pc24_region_random_programs_and_pure_rounding_match_integer_reference() {
    let mut random = 0x913a_845b_c70d_2fe1;
    let operations = [0, 1, 4, 5, 6, 7];
    for rc in 0..4 {
        let control = 0x007f | (rc << 10);
        for _ in 0..4000 {
            let a = next(&mut random);
            let b = next(&mut random);
            let lhs = f64::from_bits(
                (a & ((1 << 63) | ((1 << 52) - 1)))
                    | ((1023 + ((a >> 32) % 897) as i64 - 448) as u64) << 52,
            );
            let rhs = f64::from_bits(
                (b & ((1 << 63) | ((1 << 52) - 1)))
                    | ((1023 + ((b >> 32) % 897) as i64 - 448) as u64) << 52,
            );
            let op = operations[(a % 6) as usize];
            let Some(result) = pc24_binary(lhs, rhs, op, control) else {
                panic!("guarded normal operands");
            };
            let mut reference = X87::new();
            reference.load_control(control);
            reference.push(rhs).unwrap();
            reference.push(lhs).unwrap();
            reference.arithmetic_top(1, op).unwrap();
            assert_eq!(result.value.to_bits(), reference.st0().unwrap().to_bits());
            assert_eq!(
                (result.inexact, result.up),
                (reference.status & 0x20 != 0, reference.status & 0x200 != 0)
            );
        }
        for _ in 0..64 {
            let mut seed = empty(rc);
            for _ in 0..3 {
                let bits = next(&mut random);
                seed.push(((bits as i32 % 1000) as f64) / 16.0).unwrap();
            }
            let mut steps = [Step::Store; 24];
            for step in &mut steps {
                let bits = next(&mut random);
                let value = (((bits >> 32) as i32 % 1000) as f32 / 16.0).to_bits();
                *step = match bits % 9 {
                    0 => Step::Load(value),
                    1 | 2 => Step::Real(value, operations[(bits >> 16) as usize % 6]),
                    3 => Step::Top(
                        (bits >> 24) as u8 & 3,
                        operations[(bits >> 16) as usize % 6],
                    ),
                    4 => Step::Into(
                        (bits >> 24) as u8 & 3,
                        operations[(bits >> 16) as usize % 6],
                        bits & 0x100 != 0,
                    ),
                    5 => Step::Duplicate((bits >> 24) as u8 & 3),
                    6 => Step::Exchange((bits >> 24) as u8 & 3),
                    7 => Step::Store,
                    _ => Step::Pop,
                };
            }
            prefixes(&seed, &steps);
        }
    }
}

#[test]
fn pc24_region_midpoints_binade_boundaries_and_residuals_match_reference() {
    let pow2 = |exponent: i32| f64::from_bits(((exponent + 1023) as u64) << 52);
    let ulp = pow2(-23);
    for rc in 0..4 {
        for base in [1.0, 1.5, 1.0 + ulp, 2.0 - ulp, 1.25, 3.0 - 2.0 * ulp] {
            for base in [base, -base] {
                for k in [1.0, 2.0, 3.0] {
                    for tiny in [0.0, pow2(-60), -pow2(-60)] {
                        let other = k * ulp / 2.0 + tiny;
                        for other in [other, -other] {
                            for op in [0, 1, 4, 5, 6, 7] {
                                let mut actual = X87::new();
                                actual.load_control(0x007f | (rc << 10));
                                actual.push(other).unwrap();
                                actual.push(base).unwrap();
                                let mut expected = actual.clone();
                                expected.arithmetic_top(1, op).unwrap();
                                let result = pc24_binary(base, other, op, actual.control).unwrap();
                                assert_eq!(
                                    result.value.to_bits(),
                                    expected.st0().unwrap().to_bits()
                                );
                                assert_eq!(
                                    (result.inexact, result.up),
                                    (expected.status & 0x20 != 0, expected.status & 0x200 != 0)
                                );
                                let mut region = actual.pc24_region(0xc0, 0xc0).unwrap();
                                assert!(region.arithmetic_top(1, op));
                                region.commit();
                                assert!(
                                    actual.bitwise_eq(&expected),
                                    "base={base:?}, other={other:?}, op={op}, rc={rc}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn pc24_region_ordered_comparisons_pops_status_and_wrapping_top_match_reference() {
    for rc in 0..4 {
        for rotation in 0..8 {
            for lhs in [-0.0, 0.0, 1.0, -2.0, 2147483647.0] {
                for rhs in [-0.0, 0.0, 1.0, -2.0, 2147483647.0] {
                    let mut seed = empty(rc);
                    for _ in 0..rotation {
                        seed.rotate_top(true);
                    }
                    seed.push(rhs).unwrap();
                    seed.push(lhs).unwrap();
                    seed.restore_status(seed.status() | 0xc7ff);
                    for pops in 0..=2 {
                        prefixes(&seed, &[Step::CompareRegister(1, pops), Step::Status]);
                    }
                    for pop in [false, true] {
                        prefixes(
                            &seed,
                            &[Step::CompareReal((rhs as f32).to_bits(), pop), Step::Status],
                        );
                    }
                }
            }
        }
    }
    let mut owner = empty(0);
    let before = owner.clone();
    let region = owner.pc24_region(0, 0).unwrap();
    assert_eq!(region.status(), before.status());
    region.commit();
    assert!(owner.bitwise_eq(&before)); // FNSTSW alone changes no pointer or FPU field.
}

#[test]
fn pc24_region_register_stores_preserve_raw_zeros_integer_bits_and_empty_destinations() {
    for rc in 0..4 {
        for rotation in 0..8 {
            for kind in 0..4 {
                let mut seed = empty(rc);
                for _ in 0..rotation {
                    seed.rotate_top(true);
                }
                seed.push(7.0).unwrap();
                match kind {
                    0 => seed.push(-0.0).unwrap(),
                    1 => seed.load_real(0x80000000, true).unwrap(),
                    2 => seed.load_constant(Constant::Zero).unwrap(),
                    _ => seed.push_integer(16777217).unwrap(),
                }
                seed.restore_status(seed.status() | 0x200);
                for index in [0, 1, 7] {
                    for pop in [false, true] {
                        prefixes(
                            &seed,
                            &[
                                Step::StoreRegister(index, pop),
                                Step::Status,
                                Step::Real(0x3f800000, 0),
                                Step::StoreRegister(1, true),
                                Step::Status,
                            ],
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn pc24_region_comparison_and_register_store_refusals_precede_every_mutation() {
    let mut owner = empty(0);
    owner.push(2.0).unwrap();
    owner.push(1.0).unwrap();
    let before = owner.clone();
    let mut region = owner.pc24_region(0xc0, 0x40).unwrap();
    for step in [
        Step::CompareRegister(1, 2),
        Step::CompareRegister(0, 2),
        Step::CompareRegister(8, 0),
        Step::CompareRegister(1, 3),
        Step::StoreRegister(1, true),
        Step::StoreRegister(8, false),
        Step::CompareReal(1, false),
        Step::CompareReal(0x7f800000, false),
        Step::CompareReal(0x7fc00000, true),
    ] {
        let original = snapshot(&region);
        assert!(local(&mut region, step).is_none(), "{step:?}");
        assert_eq!(snapshot(&region), original);
    }
    region.commit();
    assert!(owner.bitwise_eq(&before));
    let mut region = owner.pc24_region(0xc0, 0).unwrap();
    let original = snapshot(&region);
    assert!(!region.compare_real32(0x3f800000, true));
    assert_eq!(snapshot(&region), original);
    assert!(region.compare_register(1, 0)); // Comparison without pop needs no slot writes.
    region.commit();
    let mut expected = before;
    expected.compare_register(1, 0).unwrap();
    assert!(owner.bitwise_eq(&expected));
}
