//! Exact x87 helper calls across WASM instances versus one merged module.
//! Emit the generated module first; merge externally (never from build.rs).
#[path = "x87_helpers/native.rs"]
mod native;
#[path = "x87_helpers/program.rs"]
mod program;

use doslet_static_probe::x87::{Kernel, MAX_ITERATIONS, checksum};
use doslet_wasm_aot::{
    Artifact,
    shared_runtime::{SharedArtifacts, SharedRunner},
    static_program::Exit,
};
use std::{env, fs, time::Instant};

#[derive(Clone, Copy)]
enum Mode {
    Reference,
    RustFast,
    Imported,
    Merged,
    Locals(u32),
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Reference => "Rust reference math in WASM",
            Self::RustFast => "Rust exact fast loop in WASM",
            Self::Imported => "generated WASM / imported Rust helpers",
            Self::Merged => "generated WASM / merged Rust helpers",
            Self::Locals(1) => "PC24 f64 locals / 1 iteration per region",
            Self::Locals(4) => "PC24 f64 locals / 4 iterations per region",
            Self::Locals(16) => "PC24 f64 locals / 16 iterations per region",
            Self::Locals(64) => "PC24 f64 locals / 64 iterations per region",
            Self::Locals(_) => unreachable!("fixed diagnostic batch sizes"),
        }
    }
    fn supports(self, control: u16) -> bool {
        !matches!(self, Self::Locals(_)) || control & 0x300 == 0
    }
}
struct Case {
    mode: Mode,
    runner: SharedRunner,
    handle: u32,
    seconds: Vec<f64>,
}
impl Case {
    fn new(mode: Mode, mut runner: SharedRunner) -> Result<Self, String> {
        let handle = runner.call_runtime::<(), u32>("x87_probe_new", ())?;
        if handle == 0 {
            return Err("null x87 probe handle".into());
        }
        Ok(Self {
            mode,
            runner,
            handle,
            seconds: Vec::new(),
        })
    }
    fn reset(&mut self, control: u16, iterations: u32) -> Result<(), String> {
        self.runner.call_runtime(
            "x87_probe_reset",
            (self.handle, u32::from(control), iterations),
        )
    }
    fn run(&mut self) -> Result<(), String> {
        match self.mode {
            Mode::Locals(batch) => {
                let accepted = self
                    .runner
                    .call_runtime::<(u32, u32), u32>("x87_probe_locals", (self.handle, batch))?;
                if accepted == 0 {
                    return Err(format!("{} refused fixture", self.mode.name()));
                }
            }
            Mode::Reference | Mode::RustFast => {
                let accepted = self.runner.call_runtime::<(u32, u32), u32>(
                    "x87_probe_reference",
                    (self.handle, u32::from(matches!(self.mode, Mode::RustFast))),
                )?;
                if accepted == 0 {
                    return Err(format!("{} refused fixture", self.mode.name()));
                }
            }
            _ => {
                if self.runner.run(self.handle)? != Exit::Runtime {
                    return Err(format!("{} refused fixture", self.mode.name()));
                }
            }
        }
        Ok(())
    }
    fn state(&mut self) -> Result<Vec<u8>, String> {
        let descriptor = self
            .runner
            .call_runtime::<u32, u64>("x87_probe_state", self.handle)?;
        let pointer = (descriptor >> 32) as u32;
        let length = descriptor as u32;
        if !(16..=16384).contains(&length) {
            return Err("invalid FPU snapshot extent".into());
        }
        Ok(self.runner.memory_view(pointer, length)?.to_vec())
    }
}

fn verify_frame(state: &[u8], control: u16, iterations: u32) -> Result<(), String> {
    let frame: Vec<_> = state[state.len() - 16..]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| u32::from_le_bytes(b))
        .collect();
    // All fixture results lie strictly between 3 and 3.5, so only round-up
    // converts to 4. This independently checks integer work and all four RCs.
    let integer = if (control >> 10) & 3 == 2 { 4 } else { 3 };
    let expected = (0..iterations).fold(0, |previous, i| checksum(previous, integer, i));
    if frame != [iterations, expected, iterations, 0] {
        return Err(format!("incorrect integer frame: {frame:x?}"));
    }
    Ok(())
}
fn median(values: &[f64]) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n.is_multiple_of(2) {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    } else {
        values[n / 2]
    }
}
fn number<T: std::str::FromStr>(
    value: Option<String>,
    default: T,
    name: &str,
) -> Result<T, String> {
    value.map_or(Ok(default), |s| {
        s.parse().map_err(|_| format!("invalid {name}: {s}"))
    })
}

fn main() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let path = args.next().ok_or("usage: compare-x87-helpers emit PROGRAM.wasm | native [iterations] [repeats] | RUNTIME.wasm MERGED.wasm [iterations] [repeats]")?;
    if path == "native" {
        return native::run(args);
    }
    if path == "emit" {
        let output = args.next().ok_or("emit needs a destination WASM path")?;
        if args.next().is_some() {
            return Err("unexpected emit arguments".into());
        }
        fs::write(&output, program::emit()).map_err(|e| format!("write {output}: {e}"))?;
        println!(
            "wrote {output}; imports memory, static_x87_operation and x87_probe_frame from doslet_static"
        );
        return Ok(());
    }
    let merged_path = args.next().ok_or("missing merged module path")?;
    let iterations = number(args.next(), 100_000u32, "iterations")?;
    let repeats = number(args.next(), 5usize, "repeats")?;
    if args.next().is_some()
        || !(1..=MAX_ITERATIONS).contains(&iterations)
        || !(1..=21).contains(&repeats)
    {
        return Err("iterations/repeats outside 1..=2000000 / 1..=21".into());
    }
    let runtime = fs::read(&path).map_err(|e| format!("read {path}: {e}"))?;
    let merged = fs::read(&merged_path).map_err(|e| format!("read {merged_path}: {e}"))?;
    let separate = SharedArtifacts::compile(&runtime, &program::emit())?;
    let merged = Artifact::compile(&merged)?;
    let mut cases = [
        Case::new(Mode::Reference, separate.instantiate()?)?,
        Case::new(Mode::RustFast, separate.instantiate()?)?,
        Case::new(Mode::Imported, separate.instantiate()?)?,
        Case::new(Mode::Merged, SharedRunner::instantiate_merged(&merged)?)?,
        Case::new(Mode::Locals(1), separate.instantiate()?)?,
        Case::new(Mode::Locals(4), separate.instantiate()?)?,
        Case::new(Mode::Locals(16), separate.instantiate()?)?,
        Case::new(Mode::Locals(64), separate.instantiate()?)?,
    ];
    for rounding in 0..4u32 {
        for seed in [1u64, 0x913a_845b_c70d_2fe1, u64::MAX] {
            let valid = cases[0].runner.call_runtime::<(u64, u32, u32), u32>(
                "x87_probe_check_locals",
                (seed, 0x7f | (rounding << 10), 256),
            )?;
            if valid != 1 {
                return Err(format!(
                    "WASM local random program differs: seed={seed:x}, RC={rounding}"
                ));
            }
        }
    }
    println!(
        "Actual WASM local-region random programs: 3 seeds x 4 RC x 256 batches, up to 32 operations per batch; complete retained FPU state equal at every exit/refusal."
    );
    // Short complete-state checks cover every implemented PC/RC combination
    // and loop boundaries before measuring the common Quake PC=24/chop mode.
    for precision in [0u16, 2, 3] {
        for rounding in 0..4 {
            let control = 0x7f | (precision << 8) | (rounding << 10);
            for count in [1, 2, 17, 257] {
                let mut reference = Kernel::default();
                reference.reset(control, count);
                assert!(reference.run(false));
                let expected = reference.state().to_vec();
                verify_frame(&expected, control, count)?;
                for case in &mut cases {
                    if !case.mode.supports(control) {
                        continue;
                    }
                    case.reset(control, count)?;
                    case.run()?;
                    if case.state()? != expected {
                        return Err(format!(
                            "{} differs at CW={control:04x}, iterations={count}",
                            case.mode.name()
                        ));
                    }
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
        case.reset(control, iterations)?;
        case.run()?;
        if case.state()? != expected {
            return Err(format!("{} warmup mismatch", case.mode.name()));
        }
    }
    for repeat in 0..repeats {
        for offset in 0..cases.len() {
            let index = (offset + repeat) % cases.len();
            let case = &mut cases[index];
            case.reset(control, iterations)?;
            let start = Instant::now();
            case.run()?;
            let seconds = start.elapsed().as_secs_f64();
            if case.state()? != expected {
                return Err(format!("{} timed repeat mismatch", case.mode.name()));
            }
            case.seconds.push(seconds);
            println!(
                "repeat {} {}: {:.6} host s",
                repeat + 1,
                case.mode.name(),
                seconds
            );
        }
    }
    println!(
        "Exact FLD32/FMUL32/FADD32/FISTP32 plus integer loop; {iterations} iterations, CW={control:04x}; full portable FPU state and integer frame equal in every run"
    );
    println!(
        "Wasmtime45.0.3 / Cranelift Speed / Backtracking / inlining Yes; {repeats} rotated samples after warmup. Setup/reset/compilation/snapshot checks excluded. No guest RAM copies or host execution callbacks."
    );
    println!(
        "Local-region samples include FPU enter/commit at every batch boundary, including stale empty-slot data. Locals use PC24 only; this fixture does not include production source/memory/device admission or native-host FPU transfer."
    );
    for case in &cases {
        println!(
            "{}: median {:.6} s ({:.3} ns/iteration), range {:.6}..{:.6}",
            case.mode.name(),
            median(&case.seconds),
            median(&case.seconds) * 1e9 / f64::from(iterations),
            case.seconds.iter().copied().fold(f64::INFINITY, f64::min),
            case.seconds.iter().copied().fold(0.0, f64::max)
        );
    }
    println!(
        "imported/merged median ratio {:.4}x; synthetic proof, not Quake FPS or browser evidence",
        median(&cases[2].seconds) / median(&cases[3].seconds)
    );
    for case in &mut cases {
        case.runner
            .call_runtime::<u32, ()>("x87_probe_free", case.handle)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn generated_helper_module_is_valid_wasm() {
        super::Artifact::compile(&super::program::emit()).unwrap();
    }
}
