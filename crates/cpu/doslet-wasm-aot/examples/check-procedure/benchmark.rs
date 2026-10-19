//! Warmed synthetic procedure timings. All construction and checks are untimed.
use super::*;
use std::{hint::black_box, time::Instant};

const INVOCATIONS: usize = 262_144;
const BATCH: usize = 64;
const REPEATS: usize = 7;
const BATCH_ENTRY: u32 = 0x2000;
const INITIAL: [u32; 8] = [
    0x12345678, 0xa5, 0x76543210, 0x89abcdef, STACK, 0x33334444, 0x55556666, 0x77778888,
];
const FLAGS: u32 = 0x2d7;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Outcome {
    gpr: [u32; 8],
    flags: u32,
    work: u64,
}

fn expanded_program() -> ProcedureProgram {
    let mut program = program();
    program.functions.push(ProcedureFunction {
        id: ProcedureId(3),
        operations: vec![P::Call(ProcedureId(0)); BATCH],
    });
    program
}

struct Native {
    cpu: Cpu,
    bus: Bus,
}
impl Native {
    fn new(cached: bool) -> Result<Self, String> {
        let (mut cpu, mut bus) = reference()?;
        // A batch body contains precisely 64 CALL A instructions and one RET.
        for index in 0..BATCH {
            let at = BATCH_ENTRY + index as u32 * 5;
            let displacement = START.wrapping_sub(at + 5);
            bus.0[at as usize] = 0xe8;
            bus.0[at as usize + 1..at as usize + 5].copy_from_slice(&displacement.to_le_bytes());
        }
        bus.0[BATCH_ENTRY as usize + BATCH * 5] = 0xc3;
        cpu.set_decode_cache(cached);
        Ok(Self { cpu, bus })
    }
    fn reset(&mut self) {
        for (register, value) in REGS.into_iter().zip(INITIAL) {
            self.cpu.registers.set32(register, value);
        }
        self.cpu.flags = FLAGS;
        self.bus.0[STACK as usize..STACK as usize + 4].copy_from_slice(&RETURN.to_le_bytes());
    }
    fn execute(&mut self, batched: bool) -> Result<Outcome, String> {
        let (entry, iterations, work) = if batched {
            (BATCH_ENTRY, INVOCATIONS / BATCH, 1281)
        } else {
            (START, INVOCATIONS, 19)
        };
        for _ in 0..iterations {
            self.cpu.registers.ip = entry;
            self.cpu.registers.set32(Gpr32::Esp, STACK);
            for _ in 0..work {
                self.cpu.step(&mut self.bus).map_err(|e| e.to_string())?;
            }
            if self.cpu.registers.ip != RETURN {
                return Err("timed reference return mismatch".into());
            }
        }
        let mut gpr = core::array::from_fn(|i| self.cpu.registers.get32(REGS[i]));
        gpr[4] = gpr[4].checked_sub(4).ok_or("reference stack underflow")?;
        Ok(Outcome {
            gpr,
            flags: self.cpu.flags,
            work: iterations as u64 * work,
        })
    }
}

fn generated(
    runner: &mut doslet_wasm_aot::procedure::ProcedureRunner,
    batched: bool,
) -> Result<Outcome, String> {
    let mut gpr = INITIAL;
    let mut flags = FLAGS;
    let mut work = 0;
    for _ in 0..if batched {
        INVOCATIONS / BATCH
    } else {
        INVOCATIONS
    } {
        let result = runner.call(gpr, flags)?;
        gpr = result.gpr;
        flags = result.eflags;
        work += result.completed_instructions;
    }
    Ok(Outcome { gpr, flags, work })
}

pub(super) fn run() -> Result<(), String> {
    let compiled = expanded_program()
        .compile(&ProcedureCompileOptions::default())
        .map_err(|e| e.to_string())?;
    let artifact = ProcedureArtifact::compile(&compiled)?;
    let mut small = artifact.instantiate(ProcedureId(0))?;
    let mut large = artifact.instantiate(ProcedureId(3))?;
    let mut cached = Native::new(true)?;
    let mut uncached = Native::new(false)?;
    let names = [
        "native interpreter cached /19",
        "native interpreter uncached /19",
        "Wasmtime AOT /19",
        "native interpreter cached /1281",
        "native interpreter uncached /1281",
        "Wasmtime AOT /1281",
    ];
    let mut timings: [Vec<f64>; 6] = core::array::from_fn(|_| Vec::with_capacity(REPEATS));
    let mut expected: Option<([u32; 8], u32)> = None;
    // A full warmup of each case. Measurement order rotates on every repeat.
    for repetition in 0..=REPEATS {
        for position in 0..names.len() {
            let variant = (position + repetition) % names.len();
            let batched = variant >= 3;
            // Reset registers/outer return slot while retaining warmed decode caches.
            cached.reset();
            uncached.reset();
            let start = Instant::now();
            let result = match variant {
                0 | 3 => cached.execute(batched)?,
                1 | 4 => uncached.execute(batched)?,
                2 => generated(&mut small, false)?,
                5 => generated(&mut large, true)?,
                _ => unreachable!(),
            };
            let seconds = start.elapsed().as_secs_f64();
            black_box(result);
            let final_state = (result.gpr, result.flags);
            if *expected.get_or_insert(final_state) != final_state {
                return Err(format!(
                    "benchmark register/flag mismatch: {}",
                    names[variant]
                ));
            }
            let wanted = if batched {
                (INVOCATIONS / BATCH) as u64 * 1281
            } else {
                INVOCATIONS as u64 * 19
            };
            if result.work != wanted {
                return Err(format!("benchmark work mismatch: {}", names[variant]));
            }
            if repetition != 0 {
                timings[variant].push(seconds);
            }
        }
    }
    println!(
        "BENCH: {INVOCATIONS} state-dependent A invocations; full warmup, {REPEATS} rotated repeats; setup/AOT/reset/validation excluded. Register/flags and actual work checked after every case."
    );
    let mut medians = [0.0; 6];
    for (variant, samples) in timings.iter_mut().enumerate() {
        samples.sort_unstable_by(f64::total_cmp);
        let median = samples[REPEATS / 2];
        medians[variant] = median;
        println!(
            "{}: median_ms={:.3} range_ms={:.3}..{:.3} ns_per_A={:.3} samples_ms={:?}",
            names[variant],
            median * 1000.0,
            samples[0] * 1000.0,
            samples[REPEATS - 1] * 1000.0,
            median * 1e9 / INVOCATIONS as f64,
            samples.iter().map(|v| v * 1000.0).collect::<Vec<_>>()
        );
    }
    println!(
        "RATIOS vs cached native: single={:.3}x, batch64={:.3}x; AOT batch64 relative throughput={:.3}x",
        medians[0] / medians[2],
        medians[3] / medians[5],
        medians[2] / medians[5]
    );
    println!(
        "Boundary: native Rust interpreter versus native Cranelift execution of register-only WASM. Counts include conceptual guest CALL/RET; generated code has no guest stack effects. No source guards, memory materialization, device time, interpreter entry adapter, browser or real-game timing is measured. Batch64 executes 64 CALL A plus RET, not a guest loop."
    );
    Ok(())
}
