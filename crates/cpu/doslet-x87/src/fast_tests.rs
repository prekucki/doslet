//! Differential tests: every fast path against its reference method, comparing
//! results and complete state bit for bit (values, retained data, encodings,
//! tags, status and pointers).
use super::*;
use crate::{ArithmeticError, Constant};
use core::sync::atomic::Ordering;

/// SplitMix64: deterministic, dependency-free test operands.
struct Random(u64);
impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
    fn chance(&mut self, numerator: u64, denominator: u64) -> bool {
        self.below(denominator) < numerator
    }
}

fn pow2(exponent: i32) -> f64 {
    f64::from_bits(((exponent + 1023) as u64) << 52)
}

/// A significand with few or many set bits, so ties and exact results occur.
fn fraction(random: &mut Random, width: u32) -> u64 {
    let bits = match random.below(4) {
        0 => random.below(u64::from(width) + 1) as u32,
        1 => width.min(1 + random.below(13) as u32),
        _ => width,
    };
    let value = random.next() & ((1 << width) - 1);
    if bits == 0 {
        0
    } else {
        value & !((1_u64 << (width - bits)) - 1)
    }
}

fn real32(random: &mut Random) -> u64 {
    let sign = u32::from(random.chance(1, 2)) << 31;
    let exponent = match random.below(16) {
        0 => 0,
        1 => 0xFF,
        2 => 1 + random.below(254) as u32,
        3 => [1, 2, 253, 254][random.below(4) as usize],
        _ => (127 + random.below(41) as i32 - 20) as u32,
    };
    let fraction = if exponent == 0xFF && random.chance(1, 3) {
        0
    } else {
        fraction(random, 23) as u32
    };
    u64::from(sign | (exponent << 23) | fraction)
}

fn real64(random: &mut Random) -> u64 {
    let sign = u64::from(random.chance(1, 2)) << 63;
    let limit = i64::from(LIMIT);
    let exponent = match random.below(16) {
        0 => 0,
        1 => 0x7FF,
        2 => 1 + random.below(2046) as i64,
        3 => 1023 + [-limit - 1, -limit, limit, limit + 1][random.below(4) as usize],
        4 => [1, 2, 2045, 2046][random.below(4) as usize],
        _ => 1023 + random.below(81) as i64 - 40,
    } as u64;
    let fraction = if exponent == 0x7FF && random.chance(1, 3) {
        0
    } else {
        fraction(random, 52)
    };
    sign | (exponent << 52) | fraction
}

fn integer(random: &mut Random) -> i64 {
    match random.below(6) {
        0 => 0,
        1 => random.next() as i64,
        2 => [i64::MIN, i64::MAX, 1 << 53, -(1 << 53), (1 << 53) - 1][random.below(5) as usize],
        3 => i64::from(random.next() as i32),
        _ => random.below(2001) as i64 - 1000,
    }
}

const OPERATIONS: [u8; 6] = [0, 1, 4, 5, 6, 7];

/// Push one register through a randomly chosen loading path.
fn push(fpu: &mut X87, random: &mut Random) {
    let control = fpu.control();
    let _ = match random.below(12) {
        0..=2 => fpu.load_real(real32(random), true),
        3 | 4 => fpu.load_real(real64(random), false),
        5 => fpu
            .push_integer(integer(random))
            .map_err(ArithmeticError::Stack),
        6 | 7 => {
            // Up to 64 significant bits, in or beyond the binary64 range,
            // including pseudo-denormals, unnormals and extreme exponents.
            let mantissa = match random.below(8) {
                0 => fraction(random, 63),
                _ => (1 << 63) | fraction(random, 63),
            };
            let exponent = match random.below(4) {
                0 => random.below(0x8000),
                1 => 16383 + random.below(2400) - 1200,
                _ => 16383 + random.below(81) - 40,
            };
            let sign = u64::from(random.chance(1, 2)) << 15;
            let mut bytes = [0; 10];
            bytes[..8].copy_from_slice(&mantissa.to_le_bytes());
            bytes[8..].copy_from_slice(&((exponent | sign) as u16).to_le_bytes());
            fpu.load_extended(bytes)
        }
        8 => {
            let constants = [
                Constant::One,
                Constant::Log2Ten,
                Constant::Log2E,
                Constant::Pi,
                Constant::Log10Two,
                Constant::LnTwo,
                Constant::Zero,
            ];
            fpu.load_constant(constants[random.below(7) as usize])
                .map_err(ArithmeticError::Stack)
        }
        _ => {
            // A previous result under another PC/RC, such as PC=64 data
            // persisting into PC=24 code.
            let _ = fpu.load_real(real32(random), true);
            fpu.load_control(
                0x7F | ((random.below(4) as u16) << 8) | ((random.below(4) as u16) << 10),
            );
            let _ = if random.chance(1, 2) {
                fpu.arithmetic_real(real64(random), false, OPERATIONS[random.below(6) as usize])
            } else {
                fpu.arithmetic_integer(
                    i64::from(integer(random) as i32),
                    OPERATIONS[random.below(6) as usize],
                )
            };
            Ok(())
        }
    };
    fpu.load_control(control);
}

/// All PC/RC combinations, with every exception masked or some unmasked.
fn controls() -> impl Iterator<Item = u16> {
    (0..16_u16).flat_map(|pcrc| {
        [0x3F_u16, 0x1F, 0x37, 0x3E, 0x00]
            .into_iter()
            .map(move |masks| 0x40 | masks | ((pcrc & 3) << 8) | ((pcrc >> 2) << 10))
    })
}

fn check<T: PartialEq + core::fmt::Debug>(
    name: &str,
    state: &X87,
    reference: impl FnOnce(&mut X87) -> T,
    fast: impl FnOnce(&mut X87) -> T,
) {
    let (mut expected, mut actual) = (state.clone(), state.clone());
    let expected_result = reference(&mut expected);
    let actual_result = fast(&mut actual);
    assert_eq!(actual_result, expected_result, "{name}: {state:?}");
    assert!(
        actual.bitwise_eq(&expected),
        "{name}: {state:?}\nexpected {expected:?}\nactual   {actual:?}"
    );
}

fn bits(result: Result<f64, ArithmeticError>) -> Result<u64, ArithmeticError> {
    result.map(f64::to_bits)
}

/// A randomized stack whose registers come from different loading paths.
fn state(random: &mut Random, control: u16) -> X87 {
    let mut fpu = X87::new();
    fpu.load_control(control);
    let depth = [1, 2, 2, 2, 3, 7, 8][random.below(7) as usize];
    for _ in 0..depth {
        push(&mut fpu, random);
    }
    if random.chance(1, 8) {
        fpu.free(random.below(8) as u8);
    }
    fpu
}

fn compare_all(random: &mut Random, cases: u32) {
    for control in controls() {
        for _ in 0..cases {
            let fpu = state(random, control);
            let operation = OPERATIONS[random.below(6) as usize];
            let index = random.below(8) as u8;
            let pop = random.chance(1, 2);
            let (real, single) = if random.chance(1, 2) {
                (real32(random), true)
            } else {
                (real64(random), false)
            };
            let value = match random.below(3) {
                0 => i64::from(random.next() as i16),
                1 => i64::from(random.next() as i32),
                _ => random.below(201) as i64 - 100,
            };
            check(
                "arithmetic_real",
                &fpu,
                |f| f.arithmetic_real(real, single, operation),
                |f| f.fast_arithmetic_real(real, single, operation),
            );
            check(
                "arithmetic_integer",
                &fpu,
                |f| f.arithmetic_integer(value, operation),
                |f| f.fast_arithmetic_integer(value, operation),
            );
            check(
                "arithmetic_top",
                &fpu,
                |f| f.arithmetic_top(index, operation),
                |f| f.fast_arithmetic_top(index, operation),
            );
            check(
                "arithmetic_into",
                &fpu,
                |f| f.arithmetic_into(index, operation, pop),
                |f| f.fast_arithmetic_into(index, operation, pop),
            );
            check(
                "load_real",
                &fpu,
                |f| f.load_real(real, single),
                |f| f.fast_load_real(real, single),
            );
            let pushed = integer(random);
            check(
                "push_integer",
                &fpu,
                |f| f.push_integer(pushed),
                |f| f.fast_push_integer(pushed),
            );
            check(
                "duplicate",
                &fpu,
                |f| f.duplicate(index),
                |f| f.fast_duplicate(index),
            );
            check(
                "store_float",
                &fpu,
                |f| bits(f.store_float(single)),
                |f| bits(f.fast_store_float(single)),
            );
        }
    }
}

#[test]
fn fast_paths_match_the_reference_on_randomized_states() {
    let hits = HITS.load(Ordering::Relaxed);
    compare_all(&mut Random(0x5EED_0087), 300);
    // 80 control words × 300 states × 4 arithmetic forms; at least an
    // eighth of them must have taken the binary64 path.
    assert!(HITS.load(Ordering::Relaxed) - hits > 12_000);
}

/// `cargo test -p doslet-x87 --release --locked fast_paths_match_extended -- --ignored`
#[test]
#[ignore = "long randomized differential run"]
fn fast_paths_match_extended_randomized_runs() {
    for seed in 0..64 {
        compare_all(&mut Random(seed), 4_000);
    }
}

fn real80(value: f64) -> [u8; 10] {
    let raw = extended::from_f64(value).to_le_bytes();
    core::array::from_fn(|i| raw[i])
}

fn with(control: u16, values: &[f64]) -> X87 {
    let mut fpu = X87::new();
    fpu.load_control(control);
    for &value in values {
        fpu.load_extended(real80(value)).unwrap();
    }
    fpu
}

/// Exact ties, one unit beyond 53 bits around them, binade starts, signed
/// zeros and the exponent limit, for both precisions and every rounding mode.
#[test]
fn fast_arithmetic_matches_at_rounding_boundaries() {
    let mut cases = 0;
    for control in controls() {
        for precision in [24, 53] {
            let ulp = pow2(1 - precision);
            for base in [1.0, 1.5, 1.0 + ulp, 2.0 - ulp, 1.25, 3.0 - 2.0 * ulp] {
                for base in [base, -base] {
                    // Sums: base ± k·ulp/2, with or without a tiny offset.
                    for k in [1.0, 2.0, 3.0] {
                        for tiny in [0.0, pow2(-60), -pow2(-60)] {
                            let addend = k * ulp / 2.0 + tiny;
                            for addend in [addend, -addend] {
                                let fpu = with(control, &[base]);
                                check(
                                    "tie sum",
                                    &fpu,
                                    |f| f.arithmetic_memory(addend, 0),
                                    |f| f.fast_arithmetic_real(addend.to_bits(), false, 0),
                                );
                                let fpu = with(control, &[addend, base]);
                                for operation in [0, 4, 5] {
                                    check(
                                        "tie register sum",
                                        &fpu,
                                        |f| f.arithmetic_top(1, operation),
                                        |f| f.fast_arithmetic_top(1, operation),
                                    );
                                }
                                cases += 4;
                            }
                        }
                    }
                    // Products and quotients of short significands near midpoints.
                    let half = pow2(-(precision / 2) - 1);
                    for factor in [1.0 + half, 1.0 - half / 2.0, 1.0 + 3.0 * half, 3.0, 0.75] {
                        let fpu = with(control, &[factor, base]);
                        for operation in [1, 6, 7] {
                            check(
                                "tie product",
                                &fpu,
                                |f| f.arithmetic_top(1, operation),
                                |f| f.fast_arithmetic_top(1, operation),
                            );
                            check(
                                "tie product into",
                                &fpu,
                                |f| f.arithmetic_into(1, operation, true),
                                |f| f.fast_arithmetic_into(1, operation, true),
                            );
                            cases += 2;
                        }
                    }
                }
            }
        }
        for (a, b) in [
            (0.0, 0.0),
            (0.0, -0.0),
            (-0.0, 0.0),
            (-0.0, -0.0),
            (1.5, -1.5),
            (-2.0, 2.0),
            (0.0, 3.0),
            (-0.0, -3.0),
        ] {
            let fpu = with(control, &[b, a]);
            for operation in OPERATIONS {
                check(
                    "zero",
                    &fpu,
                    |f| f.arithmetic_top(1, operation),
                    |f| f.fast_arithmetic_top(1, operation),
                );
                check(
                    "zero memory",
                    &fpu,
                    |f| f.arithmetic_memory(b, operation),
                    |f| f.fast_arithmetic_real(b.to_bits(), false, operation),
                );
                check(
                    "zero integer",
                    &fpu,
                    |f| f.arithmetic_integer(0, operation),
                    |f| f.fast_arithmetic_integer(0, operation),
                );
                cases += 3;
            }
        }
        let limit = LIMIT as i32;
        for exponent in [limit / 2, limit - 1, limit, limit + 1, 1023] {
            for value in [
                pow2(exponent),
                pow2(-exponent) * 1.75,
                pow2(exponent) * (2.0 - pow2(-52)),
            ] {
                for other in [value * 1.5, 1.0 / value, -value] {
                    let fpu = with(control, &[other, value]);
                    for operation in OPERATIONS {
                        check(
                            "range",
                            &fpu,
                            |f| f.arithmetic_top(1, operation),
                            |f| f.fast_arithmetic_top(1, operation),
                        );
                        cases += 1;
                    }
                }
            }
        }
    }
    assert!(cases > 50_000, "{cases}");
}

/// Up to 64 significant bits under PC=24/53 must still round only once.
#[test]
fn wide_significands_round_once_under_reduced_precision() {
    let mut random = Random(0xD00B1E);
    for control in [
        0x007F_u16, 0x047F, 0x087F, 0x0C7F, 0x027F, 0x067F, 0x0A7F, 0x0E7F,
    ] {
        for _ in 0..5_000 {
            let mut fpu = X87::new();
            fpu.load_control(control);
            let mantissa = (1 << 63) | random.next();
            let mantissa = if random.chance(1, 2) {
                mantissa & !0x7FF
            } else {
                mantissa
            };
            let mut bytes = [0; 10];
            bytes[..8].copy_from_slice(&mantissa.to_le_bytes());
            bytes[8..].copy_from_slice(&(16380_u16 + (random.below(9) as u16)).to_le_bytes());
            fpu.load_extended(bytes).unwrap();
            let other = real64(&mut random);
            for operation in OPERATIONS {
                check(
                    "wide",
                    &fpu,
                    |f| f.arithmetic_real(other, false, operation),
                    |f| f.fast_arithmetic_real(other, false, operation),
                );
            }
        }
    }
}

/// Real32/64 stores near the format's normal range limits and halfway
/// between neighbours, for every rounding mode and significand width.
#[test]
fn fast_stores_match_at_format_limits() {
    let mut cases = 0;
    for control in controls() {
        for (bias, width) in [(127_i32, 24_u32), (1023, 53)] {
            for exponent in [
                -bias - 1,
                -bias + 1,
                -bias + 2,
                0,
                bias - 2,
                bias - 1,
                bias,
                bias + 1,
            ] {
                let discard = 64 - width;
                for mantissa in [
                    u64::MAX,
                    u64::MAX << discard,
                    (u64::MAX << discard) | (1 << (discard - 1)),
                    (1 << 63) | (1 << (discard - 1)),
                    (1 << 63) | (3 << (discard - 1)),
                    (1 << 63) | ((1 << (discard - 1)) - 1),
                    1 << 63,
                ] {
                    for sign in [0_u16, 0x8000] {
                        let mut bytes = [0; 10];
                        bytes[..8].copy_from_slice(&mantissa.to_le_bytes());
                        bytes[8..]
                            .copy_from_slice(&(sign | (exponent + 16383) as u16).to_le_bytes());
                        let mut fpu = X87::new();
                        fpu.load_control(control);
                        fpu.load_extended(bytes).unwrap();
                        for single in [true, false] {
                            check(
                                "store limits",
                                &fpu,
                                |f| bits(f.store_float(single)),
                                |f| bits(f.fast_store_float(single)),
                            );
                            cases += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(cases > 10_000, "{cases}");
}
