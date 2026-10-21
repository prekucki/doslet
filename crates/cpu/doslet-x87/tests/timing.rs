//! Host cost of individual FPU operations, without CPU decode or dispatch.
//! `cargo test -p doslet-x87 --release --locked --test timing -- --ignored --nocapture`
use doslet_x87::X87;
use std::hint::black_box;
use std::time::Instant;

const ITERATIONS: u32 = 5_000_000;

fn measure(name: &str, control: u16, mut operation: impl FnMut(&mut X87, u32)) {
    let mut fpu = X87::new();
    fpu.load_control(control);
    // Two operands stay on the stack; each operation leaves its depth unchanged.
    fpu.load_real(1.25_f32.to_bits().into(), true).unwrap();
    fpu.load_real(0.75_f32.to_bits().into(), true).unwrap();
    for i in 0..10_000 {
        operation(&mut fpu, i);
    }
    let started = Instant::now();
    for i in 0..ITERATIONS {
        operation(black_box(&mut fpu), i);
    }
    let elapsed = started.elapsed();
    black_box(&fpu);
    eprintln!(
        "CW {control:04X} {name}: {:.1} ns",
        elapsed.as_nanos() as f64 / f64::from(ITERATIONS)
    );
}

#[test]
#[ignore = "host timing measurement"]
fn fpu_operation_costs() {
    let values = [1.5_f32, 0.375, 3.0e3, -7.25, 1.0e-3, 2.5e5, 0.1, -0.2];
    let bits = |i: u32| u64::from(values[(i & 7) as usize].to_bits());
    for control in [0x0C7F, 0x027F, 0x037F] {
        measure("FLD m32 + FSTP ST(0)", control, |fpu, i| {
            fpu.load_real(bits(i), true).unwrap();
            fpu.store_register(0, true).unwrap();
        });
        measure("FILD m32 + FSTP ST(0)", control, |fpu, i| {
            fpu.push_integer(i64::from(i as i32)).unwrap();
            fpu.store_register(0, true).unwrap();
        });
        measure("FLD ST(1) + FSTP ST(0)", control, |fpu, _| {
            fpu.duplicate(1).unwrap();
            fpu.store_register(0, true).unwrap();
        });
        measure("FXCH ST(1)", control, |fpu, _| fpu.exchange(1).unwrap());
        measure("FMUL m32 + FDIV m32 (restore)", control, |fpu, i| {
            fpu.arithmetic_real(bits(i), true, 1).unwrap();
            fpu.arithmetic_real(bits(i), true, 6).unwrap();
        });
        measure("FADD m32 + FSUB m32", control, |fpu, i| {
            fpu.arithmetic_real(bits(i), true, 0).unwrap();
            fpu.arithmetic_real(bits(i), true, 4).unwrap();
        });
        measure("FMUL ST(1) + FDIV ST(1)", control, |fpu, _| {
            fpu.arithmetic_register(1, 1).unwrap();
            fpu.divide(1, false).unwrap();
        });
        measure("FST m32", control, |fpu, _| {
            black_box(fpu.store_float(true).unwrap());
        });
        measure("FIST m32", control, |fpu, _| {
            black_box(fpu.store_integer(32).unwrap());
        });
        measure("FCOM ST(1)", control, |fpu, _| {
            fpu.compare_register(1, 0).unwrap()
        });
        measure("fast FLD m32 + FSTP ST(0)", control, |fpu, i| {
            fpu.fast_load_real(bits(i), true).unwrap();
            fpu.store_register(0, true).unwrap();
        });
        measure("fast FILD m32 + FSTP ST(0)", control, |fpu, i| {
            fpu.fast_push_integer(i64::from(i as i32)).unwrap();
            fpu.store_register(0, true).unwrap();
        });
        measure("fast FLD ST(1) + FSTP ST(0)", control, |fpu, _| {
            fpu.fast_duplicate(1).unwrap();
            fpu.store_register(0, true).unwrap();
        });
        measure("fast FMUL m32 + FDIV m32", control, |fpu, i| {
            fpu.fast_arithmetic_real(bits(i), true, 1).unwrap();
            fpu.fast_arithmetic_real(bits(i), true, 6).unwrap();
        });
        measure("fast FADD m32 + FSUB m32", control, |fpu, i| {
            fpu.fast_arithmetic_real(bits(i), true, 0).unwrap();
            fpu.fast_arithmetic_real(bits(i), true, 4).unwrap();
        });
        measure("fast FMUL ST(1) + FDIV ST(1)", control, |fpu, _| {
            fpu.fast_arithmetic_top(1, 1).unwrap();
            fpu.fast_arithmetic_top(1, 6).unwrap();
        });
        measure("fast FST m32", control, |fpu, _| {
            black_box(fpu.fast_store_float(true).unwrap());
        });
        measure("FSQRT + FMUL ST(0)", control, |fpu, _| {
            fpu.absolute().unwrap();
            fpu.square_root().unwrap();
            fpu.arithmetic_register(0, 1).unwrap();
        });
    }
}
