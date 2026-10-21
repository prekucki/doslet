//! Opt-in local Quake measurements of the interpreter's x87 paths. They need
//! `games/Quake.toml` and its shareware ZIP; nothing under `games` is written.
//! The demo state is kept under Cargo's target directory after the first run.
use doslet_runtime::{Session, SessionStatus};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Instant;

#[path = "quake_x87/instruction_runs.rs"]
mod instruction_runs;

/// Quake v1.06 waits for a key after its "MSCDEX not loaded" notice and then
/// offers joystick calibration: space at 3 s, ESC at 12 s (seconds, HID usage,
/// pressed) reach its demo behind the main menu by about 15 s.
const KEYS: [(u64, u16, bool); 4] = [
    (3, 0x2c, true),
    (4, 0x2c, false),
    (12, 0x29, true),
    (13, 0x29, false),
];

fn local_game() -> (PathBuf, crate::native_input::LaunchSettings) {
    use clap::Parser;
    let profile = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../games/Quake.toml");
    let cli = crate::cli::Cli::try_parse_from([
        "doslet",
        "--config",
        profile.to_str().unwrap(),
        "--no-aot",
        "--dpmi",
        "direct",
    ])
    .unwrap()
    .resolve()
    .unwrap();
    let mut launch = cli.launch;
    launch.persist = false;
    launch.preload_local_media = true;
    (cli.game.unwrap(), launch)
}

fn drain(session: &mut Session, audio: Option<&mut Sha256>) {
    let mut pcm = [doslet_pcm::EMPTY; 1024];
    let mut midi = [0; 1024];
    let mut audio = audio;
    loop {
        let count = session.drain_pcm_timed(&mut pcm);
        if count == 0 {
            break;
        }
        if let Some(audio) = audio.as_deref_mut() {
            for frame in &pcm[..count] {
                audio.update(frame.nanos.to_le_bytes());
                audio.update(frame.duration.to_le_bytes());
                for sample in frame.samples {
                    audio.update(sample.to_le_bytes());
                }
            }
        }
    }
    loop {
        let count = session.drain_midi(&mut midi);
        if count == 0 {
            break;
        }
        if let Some(audio) = audio.as_deref_mut() {
            audio.update([0xff]);
            audio.update(&midi[..count]);
        }
    }
}

/// The demo's state at 16 guest seconds and a session supplying its media.
/// `QUAKE_DEMO_STATE` overrides the cache file.
fn demo_state() -> (Vec<u8>, Session) {
    let (game, launch) = local_game();
    let media = crate::native_zip::load_game_with_settings(&game, &launch).unwrap();
    let path = std::env::var_os("QUAKE_DEMO_STATE").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/quake-x87-demo.state"),
        PathBuf::from,
    );
    if let Ok(state) = std::fs::read(&path) {
        return (state, media);
    }
    let mut session = crate::native_zip::load_game_with_settings(&game, &launch).unwrap();
    session.start();
    let mut pressed = 0;
    while session.emulated_nanos() < 16_000_000_000 {
        assert_eq!(
            session.status(),
            SessionStatus::Running,
            "{:?}",
            session.fault()
        );
        session.advance(10_000);
        drain(&mut session, None);
        if let Some(&(second, usage, down)) = KEYS.get(pressed)
            && session.emulated_nanos() >= second * 1_000_000_000
        {
            session.physical_key(usage, down).unwrap();
            pressed += 1;
        }
    }
    let state = session.save_state().unwrap();
    std::fs::write(&path, &state).unwrap();
    eprintln!("saved {}", path.display());
    (state, media)
}

fn advance_exactly(session: &mut Session, steps: u64, mut audio: Option<&mut Sha256>) {
    let target = session.steps() + steps;
    while session.steps() < target {
        assert_eq!(
            session.status(),
            SessionStatus::Running,
            "{:?}",
            session.fault()
        );
        session.advance((target - session.steps()).min(10_000) as u32);
        drain(session, audio.as_deref_mut());
    }
    assert_eq!(session.steps(), target);
}

/// Variant 0 is the byte-fetching reference x87 handler; 1 batches x87 forms
/// with the reference arithmetic; 2 (the default) adds the binary64 paths.
fn configure(session: &mut Session, variant: usize) {
    session.set_x87_batching(variant != 0);
    session.set_x87_fast_paths(variant == 2);
}

/// `cargo test -p doslet --release --locked --bin doslet quake_x87_throughput -- --ignored --nocapture`
/// `QUAKE_BENCH_STEPS` (default 400,000,000) sets the replay length.
#[test]
#[ignore = "requires the local Quake profile; host timing measurement"]
fn quake_x87_throughput() {
    let (state, media) = demo_state();
    let steps: u64 = std::env::var("QUAKE_BENCH_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(400_000_000);
    let variants = [
        "x87 reference handler",
        "x87 batched, reference arithmetic",
        "x87 batched with binary64 fast paths",
    ];
    let mut best = vec![f64::MAX; variants.len()];
    let mut guest = 0.0;
    // Each variant replays the same steps alone, in rotating order, twice;
    // the faster run counts.
    for round in 0..2 {
        for slot in 0..variants.len() {
            let variant = (slot + round) % variants.len();
            let mut session = Session::load_state_with_media(&state, &media).unwrap();
            configure(&mut session, variant);
            session.start();
            let start = session.emulated_nanos();
            let started = Instant::now();
            advance_exactly(&mut session, steps, None);
            best[variant] = best[variant].min(started.elapsed().as_secs_f64());
            guest = (session.emulated_nanos() - start) as f64 / 1e9;
        }
    }
    for (variant, seconds) in variants.iter().zip(best) {
        eprintln!(
            "{variant}: {seconds:.2} s for {steps} steps, {guest:.2} guest s ({:.3} guest s per host s, {:.1} ns/step)",
            guest / seconds,
            seconds * 1e9 / steps as f64
        );
    }
}

/// Replay the demo with each variant and compare full portable states and
/// drained PCM/MIDI at checkpoints.
/// `cargo test -p doslet --release --locked --bin doslet quake_x87_replay -- --ignored --nocapture`
/// `QUAKE_COMPARE_STEPS` (default 400,000,000) bounds the replay.
#[test]
#[ignore = "requires the local Quake profile"]
fn quake_x87_replay_matches_the_reference_handler() {
    let (state, media) = demo_state();
    let steps: u64 = std::env::var("QUAKE_COMPARE_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(400_000_000);
    let checkpoints = 8;
    let mut sessions: Vec<_> = [0, 2]
        .into_iter()
        .map(|variant| {
            let mut session = Session::load_state_with_media(&state, &media).unwrap();
            configure(&mut session, variant);
            session.start();
            (session, Sha256::new())
        })
        .collect();
    for _ in 0..checkpoints {
        for (session, audio) in &mut sessions {
            advance_exactly(session, steps / checkpoints, Some(audio));
        }
        let states: Vec<_> = sessions
            .iter()
            .map(|(session, _)| session.save_state().unwrap())
            .collect();
        let audio: Vec<_> = sessions
            .iter()
            .map(|(_, audio)| audio.clone().finalize())
            .collect();
        let (reference, _) = &sessions[0];
        eprintln!(
            "step {}: {:.2} guest s, state {} bytes, states equal {}, audio equal {}",
            reference.steps(),
            reference.emulated_nanos() as f64 / 1e9,
            states[0].len(),
            states[0] == states[1],
            audio[0] == audio[1]
        );
        assert!(states[0] == states[1], "full portable state differs");
        assert!(audio[0] == audio[1], "drained audio differs");
    }
}

fn benchmark_setting(name: &str, default: u64, allow_zero: bool) -> u64 {
    match std::env::var(name) {
        Ok(value) => {
            let parsed = value
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("{name} must be an unsigned integer, got {value:?}"));
            assert!(allow_zero || parsed != 0, "{name} must be positive");
            parsed
        }
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => panic!("cannot read {name}: {error}"),
    }
}

fn code_guards_session(state: &[u8], media: &Session, enabled: bool) -> Session {
    let mut session = Session::load_state_with_media(state, media).unwrap();
    // Both sides retain the current exact x87 batching and arithmetic paths.
    configure(&mut session, 2);
    session.set_dynamic_core(false);
    session.set_selective_code_guards(enabled);
    session.start();
    session
}

fn borrowed_bus_session(state: &[u8], media: &Session, enabled: bool) -> Session {
    let mut session = code_guards_session(state, media, true);
    session.set_borrowed_interpreter_bus(enabled);
    session
}

type ReplaySetup = fn(&[u8], &Session, bool) -> Session;

fn median(samples: &[f64]) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_unstable_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    }
}

/// Compare the current exact interpreter with selective code guards, without
/// instruction profiling. Each pair restores the same state and alternates
/// baseline/candidate order; loading and warmup are outside the timed region.
/// The timed region includes execution, PC devices and PCM/MIDI draining, and
/// excludes state hashing, audio synthesis, presentation and the GUI.
///
/// `cargo test -p doslet --release --locked --bin doslet quake_x87_paired_throughput -- --ignored --nocapture --test-threads 1`
/// `QUAKE_BENCH_STEPS` defaults to 400M measured steps per sample;
/// `QUAKE_BENCH_WARMUP_STEPS` to 10M untimed steps (zero disables warmup);
/// `QUAKE_BENCH_ROUNDS` to 6 pairs (at least 2; even counts balance order).
#[test]
#[ignore = "requires the local Quake profile; repeated paired host timing measurement"]
fn quake_x87_paired_throughput() {
    paired_throughput(
        code_guards_session,
        ["baseline (all code guards)", "selective code guards"],
    );
}

/// Compare ordinary PC bus execution with a bounded native RAM borrow.
#[test]
#[ignore = "requires the local Quake profile; repeated paired host timing measurement"]
fn quake_borrowed_bus_throughput() {
    paired_throughput(
        borrowed_bus_session,
        ["ordinary PC bus", "borrowed RAM bus"],
    );
}

fn paired_throughput(setup: ReplaySetup, names: [&str; 2]) {
    let steps = benchmark_setting("QUAKE_BENCH_STEPS", 400_000_000, false);
    let warmup = benchmark_setting("QUAKE_BENCH_WARMUP_STEPS", 10_000_000, true);
    let rounds = usize::try_from(benchmark_setting("QUAKE_BENCH_ROUNDS", 6, false))
        .expect("QUAKE_BENCH_ROUNDS exceeds the host address range");
    assert!(rounds >= 2, "QUAKE_BENCH_ROUNDS must be at least 2");
    let (state, media) = demo_state();
    eprintln!(
        "paired Quake: source state sha256={:x}; {rounds} pairs; {warmup} warmup + {steps} timed steps per sample",
        Sha256::digest(&state)
    );
    eprintln!(
        "boundary: interpreter + PC devices + PCM/MIDI draining; no profiling, synthesis, presentation or GUI"
    );
    let mut samples = [Vec::with_capacity(rounds), Vec::with_capacity(rounds)];
    let mut paired_speedups = Vec::with_capacity(rounds);
    let mut expected_window = None;
    let mut expected_warmup: Option<Vec<u8>> = None;
    for round in 0..rounds {
        let order = [round % 2, 1 - round % 2];
        let mut pair = [0.0; 2];
        for variant in order {
            let mut session = setup(&state, &media, variant == 1);
            advance_exactly(&mut session, warmup, None);
            // Serialization only borrows the session. Compare outside timing,
            // so every sample starts with identical architectural state as well
            // as the same clocks. Both sides incur the same serialization work.
            {
                let warmed = session.save_state().unwrap();
                if let Some(expected) = &expected_warmup {
                    assert!(expected == &warmed, "post-warmup portable state differs");
                } else {
                    expected_warmup = Some(warmed);
                }
            }
            let start = (session.steps(), session.emulated_nanos());
            let started = Instant::now();
            advance_exactly(&mut session, steps, None);
            let seconds = started.elapsed().as_secs_f64();
            let end = (session.steps(), session.emulated_nanos());
            let window = (start, end);
            assert_eq!(
                *expected_window.get_or_insert(window),
                window,
                "timing samples must cover the same guest window"
            );
            let guest = (end.1 - start.1) as f64 / 1e9;
            eprintln!(
                "pair {} {}: {seconds:.6} host s, {guest:.6} guest s, {:.3} ns/step, {:.3} guest s/host s",
                round + 1,
                names[variant],
                seconds * 1e9 / steps as f64,
                guest / seconds
            );
            pair[variant] = seconds;
            samples[variant].push(seconds);
        }
        let speedup = pair[0] / pair[1];
        paired_speedups.push(speedup);
        eprintln!("pair {} baseline/candidate: {speedup:.5}x", round + 1);
    }
    for (name, values) in names.iter().zip(&samples) {
        eprintln!(
            "{name}: median {:.6} host s ({:.3} ns/step), range {:.6}..{:.6} s, n={rounds}",
            median(values),
            median(values) * 1e9 / steps as f64,
            values.iter().copied().fold(f64::INFINITY, f64::min),
            values.iter().copied().fold(0.0, f64::max)
        );
    }
    eprintln!(
        "baseline/candidate: median paired speedup {:.5}x; ratio of medians {:.5}x",
        median(&paired_speedups),
        median(&samples[0]) / median(&samples[1])
    );
}

fn compare_candidate_checkpoint(sessions: &[(Session, Sha256)], checkpoint: u64) {
    let states: Vec<_> = sessions
        .iter()
        .map(|(session, _)| session.save_state().unwrap())
        .collect();
    let audio: Vec<_> = sessions
        .iter()
        .map(|(_, audio)| audio.clone().finalize())
        .collect();
    let (reference, _) = &sessions[0];
    eprintln!(
        "checkpoint {checkpoint}: step {}, {:.6} guest s, state {} bytes, states equal {}, audio equal {}",
        reference.steps(),
        reference.emulated_nanos() as f64 / 1e9,
        states[0].len(),
        states[0] == states[1],
        audio[0] == audio[1]
    );
    assert!(states[0] == states[1], "full portable state differs");
    assert!(audio[0] == audio[1], "drained PCM/MIDI differs");
}

/// Validate selective code guards separately from timing, including complete
/// portable states and drained PCM/MIDI at eight checkpoints. The replay uses
/// the benchmark's warmup and `QUAKE_COMPARE_STEPS` (default 400M)
/// measured steps. Audio is compared across warmup too.
/// `cargo test -p doslet --release --locked --bin doslet quake_x87_code_guards_replay -- --ignored --nocapture --test-threads 1`
#[test]
#[ignore = "requires the local Quake profile"]
fn quake_x87_code_guards_replay() {
    compare_replay(code_guards_session, "selective code guards");
}

/// Compare complete portable state and PCM/MIDI across native bus policies.
#[test]
#[ignore = "requires the local Quake profile"]
fn quake_borrowed_bus_replay() {
    compare_replay(borrowed_bus_session, "borrowed RAM bus");
}

fn compare_replay(setup: ReplaySetup, label: &str) {
    let steps = benchmark_setting("QUAKE_COMPARE_STEPS", 400_000_000, false);
    let warmup = benchmark_setting("QUAKE_BENCH_WARMUP_STEPS", 10_000_000, true);
    let (state, media) = demo_state();
    eprintln!(
        "Quake {label} differential: source state sha256={:x}; {warmup} warmup + {steps} replay steps",
        Sha256::digest(&state)
    );
    let mut sessions: Vec<_> = [false, true]
        .into_iter()
        .map(|enabled| {
            let mut session = setup(&state, &media, enabled);
            let mut audio = Sha256::new();
            advance_exactly(&mut session, warmup, Some(&mut audio));
            (session, audio)
        })
        .collect();
    compare_candidate_checkpoint(&sessions, 0);
    let checkpoints = 8;
    for checkpoint in 0..checkpoints {
        let chunk = steps / checkpoints + u64::from(checkpoint < steps % checkpoints);
        for (session, audio) in &mut sessions {
            advance_exactly(session, chunk, Some(audio));
        }
        compare_candidate_checkpoint(&sessions, checkpoint + 1);
    }
}

const TIMEDEMO_MARKER: &[u8] = b"DOSLET_TIMEDEMO_START";
const TIMEDEMO_CHUNK: u32 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq)]
struct TimedemoReport {
    frames: u64,
    seconds: f64,
    fps: f64,
}

fn timedemo_report(log: &[u8]) -> Option<TimedemoReport> {
    log.split(|byte| *byte == b'\n').find_map(|line| {
        let line = std::str::from_utf8(line).ok()?;
        let mut words = line.split_ascii_whitespace();
        let frames = words.next()?.parse::<u64>().ok()?;
        if words.next()? != "frames" {
            return None;
        }
        let seconds = words.next()?.parse::<f64>().ok()?;
        if words.next()? != "seconds" {
            return None;
        }
        let fps = words.next()?.parse::<f64>().ok()?;
        if words.next()? != "fps"
            || frames == 0
            || !seconds.is_finite()
            || seconds <= 0.0
            || !fps.is_finite()
            || fps <= 0.0
        {
            return None;
        }
        Some(TimedemoReport {
            frames,
            seconds,
            fps,
        })
    })
}

/// Read only new bytes from Quake's private RAM overlay. No DOS file handle is
/// opened or moved, and no host game file or persistent save store is changed.
fn refresh_timedemo_log(session: &Session, log: &mut Vec<u8>) -> bool {
    let Some(file) = session
        .visible_drive_files()
        .find(|file| file.name().eq_ignore_ascii_case(b"ID1\\QCONSOLE.LOG"))
    else {
        return false;
    };
    let previous = log.len();
    let length = file.len() as usize;
    assert!(
        length >= previous,
        "Quake unexpectedly truncated qconsole.log"
    );
    assert!(
        length <= 1024 * 1024,
        "Quake console exceeded its overlay budget"
    );
    if length == previous {
        return false;
    }
    log.resize(length, 0);
    file.read_into(previous as u32, &mut log[previous..]);
    true
}

fn advance_until_timedemo_log(
    session: &mut Session,
    log: &mut Vec<u8>,
    max_steps: u64,
    timeout_seconds: u64,
    ready: impl Fn(&[u8]) -> bool,
) {
    let start = session.steps();
    let started = Instant::now();
    loop {
        assert_eq!(
            session.status(),
            SessionStatus::Running,
            "Quake stopped: {:?}; DOS console: {}; qconsole.log: {}",
            session.fault(),
            String::from_utf8_lossy(session.console()),
            String::from_utf8_lossy(log)
        );
        assert!(
            session.steps() - start < max_steps && started.elapsed().as_secs() < timeout_seconds,
            "Quake timedemo exceeded its step/host-time bound; DOS console: {}; qconsole.log: {}",
            String::from_utf8_lossy(session.console()),
            String::from_utf8_lossy(log)
        );
        session
            .advance((max_steps - (session.steps() - start)).min(u64::from(TIMEDEMO_CHUNK)) as u32);
        drain(session, None);
        if refresh_timedemo_log(session, log) && ready(log) {
            return;
        }
    }
}

/// Quake's own timedemo frame count and guest-clock FPS, with a separate
/// unpaced host FPS measurement. Boots once, then repeats from an identical
/// in-memory state at the beginning of timedemo's second frame. No window or
/// host audio device is opened; PCM/MIDI are drained, not synthesized.
///
/// The original id Software `cl_demo.c` excludes the first frame/loading and
/// starts its timer at frame start+1. `cmd.c` makes `wait` defer the following
/// `echo` to that frame; `host.c` executes commands before reading demo messages.
/// Thus the marker is close to the game timer boundary, but host FPS includes
/// the remaining echo/log work and summary logging. Each boundary is observed
/// within 10,000 guest steps. This does not measure iced/GPU presentation FPS.
///
/// `cargo test -p doslet --release --locked --bin doslet quake_timedemo_fps -- --ignored --nocapture --test-threads 1`
/// `QUAKE_TIMEDEMO_MHZ` defaults to the profile's MHz; `QUAKE_TIMEDEMO_ROUNDS`
/// defaults to 3. Per-stage bounds: `QUAKE_TIMEDEMO_BOOT_STEPS` (4B),
/// `QUAKE_TIMEDEMO_MAX_STEPS` (20B) and `QUAKE_TIMEDEMO_TIMEOUT_SECS` (180).
/// `QUAKE_TIMEDEMO_BORROWED_BUS=0` selects the ordinary reference RAM bus.
#[test]
#[ignore = "requires the local Quake shareware profile; repeated full demo1 timedemo"]
fn quake_timedemo_fps() {
    let (game, mut launch) = local_game();
    let borrowed_bus = benchmark_setting("QUAKE_TIMEDEMO_BORROWED_BUS", 1, true);
    assert!(
        borrowed_bus <= 1,
        "QUAKE_TIMEDEMO_BORROWED_BUS must be 0 or 1"
    );
    let mhz = benchmark_setting(
        "QUAKE_TIMEDEMO_MHZ",
        u64::from(launch.hardware.cpu_mhz),
        false,
    );
    assert!((1..=1000).contains(&mhz), "Quake MHz must be in 1..=1000");
    let rounds = benchmark_setting("QUAKE_TIMEDEMO_ROUNDS", 3, false);
    let boot_steps = benchmark_setting("QUAKE_TIMEDEMO_BOOT_STEPS", 4_000_000_000, false);
    let max_steps = benchmark_setting("QUAKE_TIMEDEMO_MAX_STEPS", 20_000_000_000, false);
    let timeout = benchmark_setting("QUAKE_TIMEDEMO_TIMEOUT_SECS", 180, false);
    // These switches skip physical joystick calibration and the absent CD-ROM
    // notice. Digital Sound Blaster audio remains enabled in the guest.
    launch.command_tail =
        b"-condebug -nojoy -nocdaudio +timedemo demo1 +wait +echo DOSLET_TIMEDEMO_START".to_vec();
    launch.no_dynamic_core = true;
    let mut media = crate::native_zip::load_game_with_settings(&game, &launch).unwrap();
    media.set_dynamic_core(false);
    media.set_cpu_mhz(mhz as u16).unwrap();
    configure(&mut media, 2);
    media.set_borrowed_interpreter_bus(borrowed_bus == 1);
    media.start();
    let mut initial_log = Vec::new();
    advance_until_timedemo_log(&mut media, &mut initial_log, boot_steps, timeout, |log| {
        log.windows(TIMEDEMO_MARKER.len())
            .any(|part| part == TIMEDEMO_MARKER)
    });
    assert!(
        timedemo_report(&initial_log).is_none(),
        "timedemo finished before its marker"
    );
    let frame = media.frame().expect("Quake did not select a graphics mode");
    let dimensions = (frame.width, frame.height);
    assert_eq!(
        dimensions,
        (320, 200),
        "the local Quake benchmark must use mode 0"
    );
    let state = media.save_state().unwrap();
    eprintln!(
        "Quake demo1 timedemo: {mhz} nominal MHz, {}x{}, borrowed bus={borrowed_bus}, {rounds} repeats, start state sha256={:x}",
        dimensions.0,
        dimensions.1,
        Sha256::digest(&state)
    );
    eprintln!(
        "boundary: marker near second-frame game timer start to summary; polling <= {TIMEDEMO_CHUNK} guest steps; interpreter + devices + PCM/MIDI drain + log inspection; no synthesis or GUI"
    );
    let mut expected_report = None;
    let mut host_fps = Vec::new();
    for round in 0..rounds {
        let mut session = Session::load_state_with_media(&state, &media).unwrap();
        session.set_dynamic_core(false);
        configure(&mut session, 2);
        session.set_borrowed_interpreter_bus(borrowed_bus == 1);
        session.start();
        let mut log = initial_log.clone();
        let start = (session.steps(), session.emulated_nanos());
        let started = Instant::now();
        advance_until_timedemo_log(&mut session, &mut log, max_steps, timeout, |log| {
            timedemo_report(log).is_some()
        });
        let host_seconds = started.elapsed().as_secs_f64();
        let report = timedemo_report(&log).expect("Quake did not print a timedemo result");
        assert_eq!(
            *expected_report.get_or_insert(report),
            report,
            "identical restored timedemos must report identical results"
        );
        let guest_seconds = (session.emulated_nanos() - start.1) as f64 / 1e9;
        let steps = session.steps() - start.0;
        let fps = report.frames as f64 / host_seconds;
        host_fps.push(fps);
        eprintln!(
            "repeat {}: game reports {} frames / {:.1} guest s = {:.1} guest FPS; host {host_seconds:.6} s = {fps:.3} host FPS; sampled window {guest_seconds:.6} guest s, {steps} steps ({:.1} steps/frame)",
            round + 1,
            report.frames,
            report.seconds,
            report.fps,
            steps as f64 / report.frames as f64
        );
    }
    eprintln!(
        "median unpaced host FPS {:.3}, range {:.3}..{:.3}; profile budget {:.3} million scheduling steps/guest s (4 nominal clocks/step)",
        median(&host_fps),
        host_fps.iter().copied().fold(f64::INFINITY, f64::min),
        host_fps.iter().copied().fold(0.0, f64::max),
        mhz as f64 / 4.0
    );
}
