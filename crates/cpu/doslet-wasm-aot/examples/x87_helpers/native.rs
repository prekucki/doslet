//! Native counterpart of the WASM local-region experiment. No CPU/device or
//! cross-engine ABI overhead is included; FPU import/commit is measured.
use super::{Kernel, MAX_ITERATIONS, median, number, verify_frame};
use std::time::Instant;

enum Operation {
    Reference,
    Fast,
    Locals(u32),
}
struct Case {
    name: &'static str,
    operation: Operation,
    kernel: Kernel,
    seconds: Vec<f64>,
}
impl Case {
    fn new(name: &'static str, operation: Operation) -> Self {
        Self {
            name,
            operation,
            kernel: Kernel::default(),
            seconds: Vec::new(),
        }
    }
    fn run(&mut self) -> Result<(), String> {
        let accepted = match self.operation {
            Operation::Reference => self.kernel.run(false),
            Operation::Fast => self.kernel.run(true),
            Operation::Locals(batch) => self.kernel.run_locals(batch),
        };
        if accepted {
            Ok(())
        } else {
            Err(format!("{} declined the diagnostic loop", self.name))
        }
    }
}

pub fn run(mut args: impl Iterator<Item = String>) -> Result<(), String> {
    let iterations = number(args.next(), 100_000u32, "iterations")?;
    let repeats = number(args.next(), 5usize, "repeats")?;
    if args.next().is_some()
        || !(1..=MAX_ITERATIONS).contains(&iterations)
        || !(1..=21).contains(&repeats)
    {
        return Err("iterations/repeats outside 1..=2000000 / 1..=21".into());
    }
    for rounding in 0..4 {
        for seed in [1u64, 0x913a_845b_c70d_2fe1, u64::MAX] {
            if !doslet_static_probe::x87::local_programs::check(seed, 0x7f | (rounding << 10), 256)
            {
                return Err(format!(
                    "Native local random program differs: seed={seed:x}, RC={rounding}"
                ));
            }
        }
    }
    let mut cases = [
        Case::new("reference", Operation::Reference),
        Case::new("existing exact fast operations", Operation::Fast),
        Case::new("PC24 locals / batch 1", Operation::Locals(1)),
        Case::new("PC24 locals / batch 4", Operation::Locals(4)),
        Case::new("PC24 locals / batch 16", Operation::Locals(16)),
        Case::new("PC24 locals / batch 64", Operation::Locals(64)),
    ];
    // Correctness across all PC24 rounding controls before timing truncate.
    for rounding in 0..4 {
        let control = 0x7f | (rounding << 10);
        for count in [1, 2, 17, 257] {
            let mut reference = Kernel::default();
            reference.reset(control, count);
            assert!(reference.run(false));
            let expected = reference.state().to_vec();
            verify_frame(&expected, control, count)?;
            for case in &mut cases {
                case.kernel.reset(control, count);
                case.run()?;
                if case.kernel.state() != expected {
                    return Err(format!(
                        "{} differs at CW={control:x}, count={count}",
                        case.name
                    ));
                }
            }
        }
    }
    let control = 0x0c7f;
    let mut reference = Kernel::default();
    reference.reset(control, iterations);
    assert!(reference.run(false));
    let expected = reference.state().to_vec();
    verify_frame(&expected, control, iterations)?;
    for case in &mut cases {
        case.kernel.reset(control, iterations);
        case.run()?;
        if case.kernel.state() != expected {
            return Err(format!("{} warmup mismatch", case.name));
        }
    }
    for repeat in 0..repeats {
        for offset in 0..cases.len() {
            let index = (offset + repeat) % cases.len();
            let case = &mut cases[index];
            case.kernel.reset(control, iterations);
            let start = Instant::now();
            case.run()?;
            let seconds = start.elapsed().as_secs_f64();
            if case.kernel.state() != expected {
                return Err(format!("{} timed repeat mismatch", case.name));
            }
            case.seconds.push(seconds);
            println!("repeat {} {}: {seconds:.6} host s", repeat + 1, case.name);
        }
    }
    println!(
        "Native {}; {iterations} iterations of FLD32/FMUL32/FADD32/FISTP32 plus integer loop; CW={control:04x}; {repeats} rotated samples after warmup",
        std::env::consts::ARCH
    );
    println!(
        "Every run matches full serialized FPU state and the integer frame. Locals include enter/commit per batch. Compilation/reset/snapshot checks excluded. Synthetic component measurement: no guest CPU decode, RAM/device proofs or cross-engine FPU transfer; not Quake FPS or browser evidence."
    );
    for case in &cases {
        println!(
            "{}: median {:.6} s ({:.3} ns/iteration), range {:.6}..{:.6}",
            case.name,
            median(&case.seconds),
            median(&case.seconds) * 1e9 / f64::from(iterations),
            case.seconds.iter().copied().fold(f64::INFINITY, f64::min),
            case.seconds.iter().copied().fold(0.0, f64::max)
        );
    }
    Ok(())
}
