//! Warmed synthetic CPU comparison under the same Wasmtime engine/options.
//! Build the no-import Rust probe for wasm32-unknown-unknown first; see the
//! doslet-static-probe README for the exact commands and measurement boundary.

use doslet_static_probe::{
    END_OFFSET, INITIAL_GPR, INSTRUCTIONS_PER_ITERATION, MAX_ITERATIONS, SERVICE_INTERVAL,
    START_IP, fixture_program,
};
use doslet_wasm_aot::{
    shared_runtime::{SharedArtifacts, SharedRunner},
    static_program::Exit,
};
use doslet_x86_wasm::static_program::{Accounting, CompileOptions, Frame};
use std::{env, fs, time::Instant};

const DEFAULT_ITERATIONS: u32 = 500_000;
const DEFAULT_REPEATS: usize = 3;
const MAX_REPEATS: usize = 21;

#[derive(Clone, Copy)]
enum Mode {
    CacheOff,
    CacheOn,
    Counted,
    Hot,
}
impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::CacheOff => "interpreter-WASM cache off",
            Self::CacheOn => "interpreter-WASM cache on",
            Self::Counted => "static-WASM counted",
            Self::Hot => "static-WASM HOT uncounted",
        }
    }
    fn is_static(self) -> bool {
        matches!(self, Self::Counted | Self::Hot)
    }
}
struct Case {
    mode: Mode,
    runner: SharedRunner,
    handle: u32,
    seconds: Vec<f64>,
    callbacks: u64,
    services: u64,
    fallbacks: u64,
}
impl Case {
    fn new(artifacts: &SharedArtifacts, mode: Mode, repeats: usize) -> Result<Self, String> {
        let mut runner = artifacts.instantiate()?;
        let handle = runner.call_runtime::<(), u32>("probe_new", ())?;
        if handle == 0 {
            return Err("probe allocation returned a null handle".into());
        }
        Ok(Self {
            mode,
            runner,
            handle,
            seconds: Vec::with_capacity(repeats),
            callbacks: 0,
            services: 0,
            fallbacks: 0,
        })
    }
    fn reset(&mut self, iterations: u32) -> Result<(), String> {
        self.runner.call_runtime::<(u32, u32, u32), ()>(
            "probe_reset",
            (
                self.handle,
                iterations,
                u32::from(matches!(self.mode, Mode::CacheOn)),
            ),
        )
    }
    fn execute(&mut self, expected_work: u32) -> Result<(), String> {
        if self.mode.is_static() {
            let exit = self.runner.run(self.handle)?;
            if exit != Exit::Runtime {
                return Err(format!(
                    "{} stopped before endpoint: {exit:?}",
                    self.mode.name()
                ));
            }
        } else {
            let completed = self
                .runner
                .call_runtime::<u32, u32>("probe_reference", self.handle)?;
            if completed != expected_work {
                return Err(format!(
                    "reference completed {completed}, expected {expected_work}"
                ));
            }
        }
        Ok(())
    }
    fn result(&mut self, field: u32) -> Result<u64, String> {
        self.runner
            .call_runtime::<(u32, u32), u64>("probe_result", (self.handle, field))
    }
    fn inspect(&mut self, iterations: u32) -> Result<[u64; 11], String> {
        let mut architecture = [0; 11];
        for (field, value) in architecture.iter_mut().enumerate() {
            *value = self.result(field as u32)?;
        }
        let frame = self.runner.read_frame(self.handle)?;
        self.check_frame(&frame, &architecture, iterations)?;
        self.callbacks = self.result(11)?;
        self.services = self.result(12)?;
        self.fallbacks = self.result(13)?;
        if self.mode.is_static() {
            if self.callbacks != self.services + self.fallbacks
                || self.fallbacks != 1
                || self.services != u64::from(iterations / SERVICE_INTERVAL)
            {
                return Err(format!("{} callback accounting mismatch", self.mode.name()));
            }
        } else if self.callbacks != 0
            || self.result(14)? != u64::from(iterations * INSTRUCTIONS_PER_ITERATION)
        {
            return Err("reference instruction/callback accounting mismatch".into());
        }
        Ok(architecture)
    }
    fn check_frame(
        &self,
        frame: &Frame,
        architecture: &[u64; 11],
        iterations: u32,
    ) -> Result<(), String> {
        let work = u64::from(iterations) * u64::from(INSTRUCTIONS_PER_ITERATION);
        let registers_match = frame
            .gpr
            .into_iter()
            .zip(architecture)
            .all(|(a, b)| u64::from(a) == *b);
        if !registers_match
            || u64::from(frame.eflags) != architecture[8]
            || frame.address.space != 1
            || frame.address.offset != END_OFFSET
            || architecture[9] != u64::from(START_IP + END_OFFSET)
            || architecture[1] != 0
        {
            return Err(format!(
                "{} frame/CPU endpoint mismatch: {frame:?}",
                self.mode.name()
            ));
        }
        if matches!(self.mode, Mode::Hot) {
            if frame.exact_total().is_some()
                || !frame.uncounted_seen()
                || frame.remaining_budget != work + 1
            {
                return Err(
                    "HOT must omit instruction charges and expose unavailable exact total".into(),
                );
            }
        } else if frame.exact_total() != Some(work) || frame.remaining_budget != 1 {
            return Err(format!(
                "{} completed-work/budget mismatch: {frame:?}",
                self.mode.name()
            ));
        }
        Ok(())
    }
}

fn parse<T: std::str::FromStr>(value: Option<String>, default: T, name: &str) -> Result<T, String> {
    value.map_or(Ok(default), |s| {
        s.parse().map_err(|_| format!("invalid {name}: {s}"))
    })
}
fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        (values[middle - 1] + values[middle]) / 2.0
    } else {
        values[middle]
    }
}
fn verify_fixture(state: &[u64; 11], iterations: u32, ram_hash: u64) -> Result<(), String> {
    // Independent algebraic oracle for the entire final architectural state.
    // A complete cycle of 256 successive byte values XORs to zero.
    let mut gpr = INITIAL_GPR;
    gpr[0] = gpr[0].wrapping_add(iterations);
    gpr[1] = 0;
    gpr[2] =
        (gpr[2] & 0xffff_0000) | u32::from((gpr[2] as u16).wrapping_add((iterations * 3) as u16));
    let mut bh = (gpr[3] >> 8) as u8;
    for i in 1..=iterations % 256 {
        bh ^= INITIAL_GPR[0].wrapping_add(i) as u8;
    }
    gpr[3] = (gpr[3] & !0xff00) | (u32::from(bh) << 8);
    if !gpr.into_iter().zip(state).all(|(a, b)| u64::from(a) == *b)
        || state[8] != 0x246
        || state[9] != u64::from(START_IP + END_OFFSET)
        || state[10] != ram_hash
    {
        return Err(format!(
            "fixture oracle mismatch after {iterations} iterations: {state:x?}"
        ));
    }
    Ok(())
}
fn main() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let path = args
        .next()
        .ok_or("usage: compare-static-cpu RUNTIME.wasm [iterations:1..2000000] [repeats:1..21]")?;
    let iterations = parse(args.next(), DEFAULT_ITERATIONS, "iterations")?;
    let repeats = parse(args.next(), DEFAULT_REPEATS, "repeats")?;
    if args.next().is_some()
        || !(1..=MAX_ITERATIONS).contains(&iterations)
        || !(1..=MAX_REPEATS).contains(&repeats)
    {
        return Err("iterations/repeats outside documented bounds or unexpected arguments".into());
    }
    let runtime = fs::read(&path).map_err(|e| format!("read {path}: {e}"))?;
    let options = CompileOptions {
        service_interval_regions: SERVICE_INTERVAL,
        ..CompileOptions::default()
    };
    let start = Instant::now();
    let counted = fixture_program(Accounting::Counted)
        .compile(&options)
        .map_err(|e| e.to_string())?;
    let hot = fixture_program(Accounting::UncountedHot)
        .compile(&options)
        .map_err(|e| e.to_string())?;
    let generation = start.elapsed().as_secs_f64();
    let counted_artifacts = SharedArtifacts::compile(&runtime, counted.wasm())?;
    let hot_artifacts = SharedArtifacts::compile(&runtime, hot.wasm())?;
    let mut cases = [
        Case::new(&counted_artifacts, Mode::CacheOff, repeats)?,
        Case::new(&counted_artifacts, Mode::CacheOn, repeats)?,
        Case::new(&counted_artifacts, Mode::Counted, repeats)?,
        Case::new(&hot_artifacts, Mode::Hot, repeats)?,
    ];
    let initial_ram_hash = cases[0].result(10)?;
    // Exercise both sides of the structural checkpoint before the long warm-up.
    for boundary in [
        1,
        2,
        SERVICE_INTERVAL - 1,
        SERVICE_INTERVAL,
        SERVICE_INTERVAL + 1,
    ] {
        for case in &mut cases {
            case.reset(boundary)?;
            case.execute(boundary * INSTRUCTIONS_PER_ITERATION)?;
            verify_fixture(&case.inspect(boundary)?, boundary, initial_ram_hash)?;
        }
    }
    let expected_work = iterations * INSTRUCTIONS_PER_ITERATION;
    let mut expected = None;
    // Populate CPU decoder and engine caches before measuring. Each case keeps
    // its own live CPU, so cache-on reset preserves its warmed decode cache.
    for case in &mut cases {
        case.reset(iterations)?;
        case.execute(expected_work)?;
        let state = case.inspect(iterations)?;
        verify_fixture(&state, iterations, initial_ram_hash)?;
        if expected.is_some_and(|e| e != state) {
            return Err(format!(
                "{} warm-up architectural mismatch",
                case.mode.name()
            ));
        }
        expected = Some(state);
    }
    for repeat in 0..repeats {
        // Rotate order to avoid giving every case the same thermal/order bias.
        for offset in 0..cases.len() {
            let index = (repeat + offset) % cases.len();
            let case = &mut cases[index];
            case.reset(iterations)?;
            let start = Instant::now();
            case.execute(expected_work)?;
            let seconds = start.elapsed().as_secs_f64();
            let state = case.inspect(iterations)?;
            verify_fixture(&state, iterations, initial_ram_hash)?;
            if Some(state) != expected {
                return Err(format!(
                    "{} repeat {} architectural mismatch",
                    case.mode.name(),
                    repeat + 1
                ));
            }
            case.seconds.push(seconds);
        }
    }
    println!(
        "Synthetic protected-mode register loop: {iterations} iterations, {expected_work} guest instructions per run; HLT endpoint excluded"
    );
    println!(
        "Runtime: actual Rust doslet-x86 compiled to wasm32-unknown-unknown; Wasmtime 45.0.3 / Cranelift Speed / Backtracking / inlining Yes"
    );
    println!(
        "One untimed warm-up per case; {repeats} timed runs, rotated order; service every {SERVICE_INTERVAL} regions"
    );
    println!(
        "Untimed checkpoint-boundary acceptance passed at 1, 2, 63, 64 and 65 iterations, including an independent register/flags oracle and unchanged RAM"
    );
    println!(
        "Setup, reset, compile/load and state/RAM-hash verification are outside execution timing"
    );
    println!(
        "Generation {:.3} ms; counted runtime/program compile {:.3}/{:.3} ms, load {:.3}/{:.3} ms; HOT runtime/program compile {:.3}/{:.3} ms, load {:.3}/{:.3} ms",
        generation * 1000.0,
        counted_artifacts.runtime.compile_seconds * 1000.0,
        counted_artifacts.program.compile_seconds * 1000.0,
        counted_artifacts.runtime.load_seconds * 1000.0,
        counted_artifacts.program.load_seconds * 1000.0,
        hot_artifacts.runtime.compile_seconds * 1000.0,
        hot_artifacts.program.compile_seconds * 1000.0,
        hot_artifacts.runtime.load_seconds * 1000.0,
        hot_artifacts.program.load_seconds * 1000.0
    );
    let mut medians = [0.0; 4];
    for (i, case) in cases.iter_mut().enumerate() {
        medians[i] = median(&mut case.seconds);
        println!(
            "{}: median {:.3} ms, range {:.3}..{:.3} ms; callbacks {} (service {}, endpoint {}){}",
            case.mode.name(),
            medians[i] * 1000.0,
            case.seconds[0] * 1000.0,
            case.seconds[case.seconds.len() - 1] * 1000.0,
            case.callbacks,
            case.services,
            case.fallbacks,
            if matches!(case.mode, Mode::Hot) {
                "; exact instruction total unavailable"
            } else {
                ""
            }
        );
    }
    println!(
        "Static counted/HOT speedup versus interpreter cache on: {:.2}x / {:.2}x",
        medians[1] / medians[2],
        medians[1] / medians[3]
    );
    let state = expected.expect("four warmed cases");
    println!(
        "Equivalent every run: GPR8, EFLAGS={:#x}, EIP={:#x}, complete 64-KiB guest-RAM FNV-1a={:#018x}; counted work={expected_work}",
        state[8], state[9], state[10]
    );
    println!(
        "Shared runtime memory and direct WASM-to-WASM callback; no native execution callback or guest-RAM copies. Synthetic result does not establish game speed, device correctness or browser performance."
    );
    for case in &mut cases {
        case.runner
            .call_runtime::<u32, ()>("probe_free", case.handle)?;
    }
    Ok(())
}
