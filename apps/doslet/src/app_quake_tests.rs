//! Quake, a DJGPP program, through the desktop application's own worker path.
use super::*;
use clap::Parser;

/// Run the app from boot to `seconds` of guest time, pressing Quake's keys,
/// and return the host time it took.
fn run(app: &mut Doslet, seconds: u64) -> Duration {
    let mut pressed = 0;
    let started = std::time::Instant::now();
    while app.session.emulated_nanos() < seconds * 1_000_000_000
        && started.elapsed() < Duration::from_secs(10 * seconds)
    {
        if let Some(&(second, usage, down)) = crate::native_aot::QUAKE_KEYS.get(pressed)
            && app.session.emulated_nanos() >= second * 1_000_000_000
        {
            app.session.physical_key(usage, down).unwrap();
            pressed += 1;
        }
        let _ = app.worker_tick();
        std::thread::sleep(app.worker_delay(true).min(Duration::from_millis(4)));
    }
    started.elapsed()
}

/// `cargo test -p doslet --release --locked --bin doslet quake_app -- --ignored --nocapture`
/// `QUAKE_SECONDS` (default 40) sets the guest time of each run.
#[test]
#[ignore = "requires the local Quake profile; runs it twice without a window or audio device"]
fn quake_app_follows_its_coff_layout_with_and_without_aot() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let seconds: u64 = std::env::var("QUAKE_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(40);
    let modules = crate::native_aot::test_cache_directory("app-quake");
    let mut results = Vec::new();
    for aot in [true, false] {
        let config = root.join("games/Quake.toml");
        // Overlay writes keep the profile's save store closed.
        let mut arguments = vec![
            "doslet",
            "--config",
            config.to_str().unwrap(),
            "--dpmi",
            "direct",
            "--no-audio",
            "--no-speed-tiers",
            "--drive-mode",
            "overlay",
        ];
        if !aot {
            arguments.push("--no-aot");
        }
        let mut app = Doslet::from_cli(crate::cli::Cli::parse_from(arguments)).unwrap();
        assert!(app.layout.is_some(), "Quake is a watched COFF program");
        assert_eq!(app.aot.is_some(), aot);
        if let Some(watched) = &app.layout {
            // The test never writes the user's AOT cache.
            app.aot = aot.then(|| {
                crate::native_aot::Aot::new(
                    "QUAKE.EXE".into(),
                    watched.exe().clone(),
                    modules.clone(),
                    None,
                )
            });
        }
        let host = run(&mut app, seconds);
        let guest = app.session.emulated_nanos() as f64 / 1e9;
        let (_, layout) = app.live_layout.clone().expect("live COFF layout");
        eprintln!(
            "AOT {aot}: {guest:.1} guest s in {:.1} host s ({:.3} guest s per host s), layout {}, AOT active {}, stats {:?}",
            host.as_secs_f64(),
            guest / host.as_secs_f64(),
            layout.key(),
            app.session.wasm_procedures_active(),
            app.session.wasm_procedure_stats()
        );
        assert_eq!(app.session.status(), SessionStatus::Running);
        assert!(guest >= seconds as f64, "{guest} guest seconds");
        assert!(layout.text.is_some());
        assert!(layout.key().starts_with("coff-"));
        if aot {
            // Every compiled layout was judged; this Quake keeps none.
            let module = crate::native_aot::module_path(&modules, &layout);
            assert!(module.is_file());
            let reason = std::fs::read_to_string(crate::native_aot::failure_marker(&module))
                .unwrap_or_default();
            eprintln!("AOT outcome for {}: {}", layout.key(), reason.trim());
        } else {
            assert!(app.session.wasm_procedure_stats().is_none());
        }
        results.push((aot, guest / host.as_secs_f64()));
    }
    eprintln!("guest seconds per host second (AOT, ratio): {results:?}");
    let _ = std::fs::remove_dir_all(&modules);
}
