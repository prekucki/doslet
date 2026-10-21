//! Opt-in local Quake measurements of the interpreter's x87 paths. They need
//! `games/Quake.toml` and its shareware ZIP; nothing under `games` is written.
//! The demo state is kept under Cargo's target directory after the first run.
use doslet_runtime::{Session, SessionStatus};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Instant;

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
