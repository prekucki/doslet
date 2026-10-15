//! Compare interpretation, batching alone, and cold/profile-assisted JIT.
//! Successive windows preserve each session's guest and compiler state.
//! Host-only diagnostics; the original game files are never written.
#[path = "../../common/native_input.rs"]
mod native_input;

use doslet_runtime::{Session, SessionStatus};
use std::{error::Error, path::Path, time::Duration, time::Instant};

const WINDOW_STEPS: u32 = 100_000_000;
const WINDOWS: usize = 10;
const ROUNDS: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Interpreter,
    BatchingOnly,
    Cold,
    Profile,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Interpreter => "interpreter",
            Self::BatchingOnly => "batching",
            Self::Cold => "cold",
            Self::Profile => "profile",
        }
    }

    fn compiles(self) -> bool {
        matches!(self, Self::Cold | Self::Profile)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Stream {
    hash: u64,
    bytes: u64,
}
impl Stream {
    fn new() -> Self {
        Self {
            hash: 0xcbf29ce484222325,
            bytes: 0,
        }
    }
    fn update(&mut self, byte: u8) {
        self.hash = (self.hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        self.bytes += 1;
    }
}

struct Runner {
    session: Session,
    mode: Mode,
    midi: Stream,
    pcm: Stream,
}
impl Runner {
    fn new(path: &Path, mode: Mode, profile: &Path) -> Result<Self, Box<dyn Error>> {
        let mut session = native_input::load_game(path)?;
        let dynamic = mode != Mode::Interpreter;
        session.set_dynamic_core(dynamic);
        assert_eq!(session.dynamic_core_enabled(), dynamic, "JIT unavailable");
        if mode == Mode::Profile {
            native_input::load_core_profile(&mut session, profile);
            assert!(
                session.dynamic_stats().compiled_blocks > 0,
                "profile not prepared"
            );
        }
        session.start();
        Ok(Self {
            session,
            mode,
            midi: Stream::new(),
            pcm: Stream::new(),
        })
    }

    fn window(&mut self, round: usize, window: usize) {
        let before = self.session.dynamic_stats();
        let mut execution = Duration::ZERO;
        let mut preparation = Duration::ZERO;
        let mut midi = [0; 1024];
        let mut pcm = [[0; 2]; 128];
        let start = Instant::now();
        for _ in 0..WINDOW_STEPS / 10_000 {
            // Batching uses identical dynamic dispatch/discovery, but skipping
            // all preparation guarantees that no native block can be executed.
            if self.mode.compiles() {
                let compile = Instant::now();
                self.session.prepare_dynamic(2);
                preparation += compile.elapsed();
            }
            let cpu = Instant::now();
            let done = self.session.advance(10_000);
            execution += cpu.elapsed();
            assert_eq!(done, 10_000, "{:?}", self.session.fault());
            assert_eq!(self.session.status(), SessionStatus::Running);
            loop {
                let count = self.session.drain_midi(&mut midi);
                if count == 0 {
                    break;
                }
                for &byte in &midi[..count] {
                    self.midi.update(byte);
                }
            }
            loop {
                let count = self.session.drain_pcm(&mut pcm);
                if count == 0 {
                    break;
                }
                for frame in &pcm[..count] {
                    for sample in frame {
                        for byte in sample.to_le_bytes() {
                            self.pcm.update(byte);
                        }
                    }
                }
            }
        }
        let total = start.elapsed();
        let after = self.session.dynamic_stats();
        assert_eq!(
            after.compilation_failures,
            0,
            "{:?}",
            self.session.dynamic_error()
        );
        if !self.mode.compiles() {
            assert_eq!(after.compiled_blocks, 0, "unexpected native compilation");
            assert_eq!(after.executed_steps, 0, "unexpected native execution");
            assert_eq!(after.executed_blocks, 0, "unexpected native block entry");
        }
        println!(
            "{},{},{},{},{:.6},{:.6},{:.6},{},{},{},{},{},{},{},{},{}",
            round + 1,
            window + 1,
            self.mode.name(),
            self.session.steps(),
            execution.as_secs_f64(),
            preparation.as_secs_f64(),
            total.as_secs_f64(),
            after.compiled_blocks - before.compiled_blocks,
            after.compiled_blocks,
            after.executed_steps - before.executed_steps,
            after.executed_blocks - before.executed_blocks,
            after.invalidations - before.invalidations,
            after.execution_batches - before.execution_batches,
            after.batched_interpreter_steps - before.batched_interpreter_steps,
            after.ram_trace_steps - before.ram_trace_steps,
            after.ram_trace_entries - before.ram_trace_entries,
        );
    }
}

fn compare(reference: &Runner, native: &Runner) {
    let a = &reference.session;
    let b = &native.session;
    assert_eq!(a.registers(), b.registers());
    assert_eq!(a.eflags(), b.eflags());
    assert_eq!(a.control_registers(), b.control_registers());
    assert_eq!(a.emulated_nanos(), b.emulated_nanos());
    assert_eq!(a.steps(), b.steps());
    assert_eq!(a.a20_enabled(), b.a20_enabled());
    assert_eq!(a.cpu_halted(), b.cpu_halted());
    assert_eq!(a.midi_stats(), b.midi_stats());
    assert_eq!(a.file_access_stats(), b.file_access_stats());
    assert_eq!(a.video_mode(), b.video_mode());
    assert_eq!(a.loaded_overlays(), b.loaded_overlays());
    assert_eq!(a.console(), b.console());
    assert_eq!(reference.midi, native.midi);
    assert_eq!(reference.pcm, native.pcm);
    #[cfg(feature = "profiling")]
    assert_eq!(a.execution_stats(), b.execution_stats());
    assert_eq!(a.vram_view(), b.vram_view());
    assert_eq!(a.ram_view(0..0xa0000), b.ram_view(0..0xa0000));
    let end = if a.a20_enabled() {
        16 * 1024 * 1024
    } else {
        0x100000
    };
    assert_eq!(a.ram_view(0xb0000..end), b.ram_view(0xb0000..end));
    match (a.frame(), b.frame()) {
        (Some(a), Some(b)) => {
            assert_eq!(
                (a.width, a.height, a.revision),
                (b.width, b.height, b.revision)
            );
            assert_eq!(a.pixels, b.pixels);
            assert_eq!(a.palette, b.palette);
        }
        (None, None) => {}
        _ => panic!("scanout differs"),
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(2..=4).contains(&args.len()) {
        return Err(
            "usage: bench_dynamic_warmup EXE EXISTING_CORE_PROFILE [WINDOWS:1..10] [profile-pair]"
                .into(),
        );
    }
    let windows = match args.get(2) {
        Some(value) => value.to_str().ok_or("WINDOWS must be UTF-8")?.parse()?,
        None => WINDOWS,
    };
    if !(1..=WINDOWS).contains(&windows) {
        return Err("WINDOWS must be between 1 and 10 (100 million steps each)".into());
    }
    let modes: &[Mode] = match args.get(3) {
        None => &[
            Mode::Interpreter,
            Mode::BatchingOnly,
            Mode::Cold,
            Mode::Profile,
        ],
        Some(value) if value == "profile-pair" => &[Mode::Interpreter, Mode::Profile],
        _ => return Err("the optional mode selector must be profile-pair".into()),
    };
    let path = Path::new(&args[0]);
    let profile = Path::new(&args[1]);
    if !profile.is_file() {
        return Err("an existing observed-block profile is required".into());
    }
    println!(
        "round,window,mode,end_steps,execution_s,preparation_s,total_s,new_blocks,compiled_blocks,native_steps,native_blocks,invalidations,execution_batches,batched_interpreter_steps,ram_trace_steps,ram_trace_entries"
    );
    for round in 0..ROUNDS {
        let mut runners: Vec<_> = modes
            .iter()
            .map(|&mode| Runner::new(path, mode, profile))
            .collect::<Result<_, _>>()?;
        for window in 0..windows {
            // Four rounds balance each selected mode's position. Sessions are
            // never reset between their windows.
            for position in 0..runners.len() {
                let index = (round + window + position) % runners.len();
                runners[index].window(round, window);
            }
            for runner in &runners[1..] {
                compare(&runners[0], runner);
            }
        }
        eprintln!(
            "Round {}: all {} selected modes match state/RAM/VRAM/scanout and MIDI/PCM hashes through {} steps",
            round + 1,
            runners.len(),
            windows as u64 * u64::from(WINDOW_STEPS)
        );
    }
    Ok(())
}
