//! Quake, a DJGPP program, through the desktop application's own worker path.
use super::*;
use clap::Parser;

#[test]
#[cfg(feature = "v86")]
#[ignore = "requires local Quake and the v86 module; no window or host audio"]
fn quake_v86_host_clock_uses_the_app_worker() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = root.join("games/Quake.toml");
    let module = root.join("target/v86/doslet-v86.wasm");
    let mut cli = crate::cli::Cli::parse_from([
        "doslet",
        "--config",
        config.to_str().unwrap(),
        "--v86",
        "--v86-host-clock",
        "--v86-module",
        module.to_str().unwrap(),
        "--no-audio",
        "--drive-mode",
        "overlay",
    ]);
    cli.launch.preload_local_media = true;
    let mut app = Doslet::from_cli(cli).unwrap();
    assert!(app.session.v86_host_clock_enabled());
    assert!(app.aot.is_none());
    assert!(!app.session.dynamic_core_enabled());
    let host = run(&mut app, 16);
    eprintln!(
        "v86 host clock: guest={} host={host:?} steps={} stats={:?}; error={:?}",
        app.session.emulated_nanos(),
        app.session.steps(),
        app.session.v86_statistics(),
        app.notice
    );
    assert_eq!(
        app.session.status(),
        SessionStatus::Running,
        "{:?}; {:?}",
        app.session.fault(),
        app.session.v86_last_error()
    );
    assert!(app.session.emulated_nanos() >= 16_000_000_000);
    assert!(host >= Duration::from_secs(15) && host < Duration::from_secs(30));
    assert!(app.session.v86_statistics().unwrap().jit_entries > 0);
    let frame = app.session.frame().expect("Quake scanout");
    assert_eq!((frame.width, frame.height), (320, 200));
    let rgba: Vec<_> = frame
        .indices()
        .flat_map(|p| frame.palette[usize::from(p)])
        .collect();
    let file = std::fs::File::create(root.join("target/v86-quake-host-clock.png")).unwrap();
    let mut encoder = png::Encoder::new(file, frame.width as u32, frame.height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&rgba)
        .unwrap();
}

#[test]
#[cfg(feature = "v86")]
#[ignore = "requires local Quake and the v86 module; no window or host audio"]
fn quake_v86_boots_with_rust_devices() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config = root.join("games/Quake.toml");
    let module = root.join("target/v86/doslet-v86.wasm");
    let cli = crate::cli::Cli::parse_from([
        "doslet",
        "--config",
        config.to_str().unwrap(),
        "--v86",
        "--v86-instruction-clock",
        "--v86-module",
        module.to_str().unwrap(),
        "--no-audio",
        "--drive-mode",
        "overlay",
    ])
    .resolve()
    .unwrap();
    let mut launch = cli.launch;
    launch.preload_local_media = true;
    let mut session =
        crate::native_zip::load_game_with_settings(&cli.game.unwrap(), &launch).unwrap();
    session.start();
    let start = Instant::now();
    let mut pressed = 0;
    while session.emulated_nanos() < 16_000_000_000 && start.elapsed() < Duration::from_secs(120) {
        assert_eq!(
            session.status(),
            SessionStatus::Running,
            "{:?}; {:?}; PC={:x}; console={}",
            session.fault(),
            session.v86_last_error(),
            session.debug_pc(),
            String::from_utf8_lossy(session.console())
        );
        session.advance(4096);
        let mut pcm = [doslet_pcm::EMPTY; 2048];
        while session.drain_pcm_timed(&mut pcm) != 0 {}
        let mut midi = [0; 1024];
        while session.drain_midi(&mut midi) != 0 {}
        if let Some(&(second, usage, down)) = crate::native_aot::QUAKE_KEYS.get(pressed)
            && session.emulated_nanos() >= second * 1_000_000_000
        {
            session.physical_key(usage, down).unwrap();
            pressed += 1;
        }
    }
    eprintln!(
        "v86 time={} steps={} stats={:?}; console={}",
        session.emulated_nanos(),
        session.steps(),
        session.v86_statistics(),
        String::from_utf8_lossy(session.console())
    );
    assert_eq!(
        session.status(),
        SessionStatus::Running,
        "{:?}; {:?}",
        session.fault(),
        session.v86_last_error()
    );
    assert!(
        session.emulated_nanos() >= 16_000_000_000,
        "guest did not reach the bounded boot target"
    );
    let stats = session.v86_statistics().unwrap();
    assert!(stats.jit_entries > 0, "{stats:?}");
    let frame = session.frame().expect("Quake graphics scanout");
    let rgba: Vec<_> = frame
        .indices()
        .flat_map(|pixel| frame.palette[usize::from(pixel)])
        .collect();
    let file = std::fs::File::create(root.join("target/v86-quake-boot.png")).unwrap();
    let mut encoder = png::Encoder::new(file, frame.width as u32, frame.height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&rgba)
        .unwrap();
}

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
