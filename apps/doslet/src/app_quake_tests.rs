//! Real-game diagnostics through the desktop application's own worker path.
use super::*;
use clap::Parser;

#[test]
#[cfg(feature = "v86")]
#[ignore = "requires local ROTT and a headless wgpu adapter; exercises desktop input dispatch"]
fn local_rott_v86_desktop_keyboard_probe() {
    use iced::keyboard::{
        Event, Key, Location, Modifiers,
        key::{Code, Named, Physical},
    };
    use iced::{Font, Pixels, Point, Size};
    use iced_runtime::core::{clipboard, mouse, renderer};
    use iced_runtime::user_interface::{Cache, UserInterface};
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let profile = std::env::var_os("DOSLET_ROTT_PROFILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("games/ROTT.toml"));
    let mut app = crate::desktop::Doslet::with_engine(move || {
        let mut cli = crate::cli::Cli::parse_from([
            "doslet",
            "--config",
            profile.to_str().unwrap(),
            "--v86",
            "--no-audio",
            "--drive-mode",
            "overlay",
        ]);
        cli.load_state = std::env::var_os("DOSLET_ROTT_STATE").map(std::path::PathBuf::from);
        eprintln!(
            "ROTT desktop profile={} state={:?}",
            profile.display(),
            cli.load_state
        );
        let mut engine = Doslet::from_cli(cli).map_err(|e| e.to_string())?;
        if let Some(store) = std::env::var_os("DOSLET_ROTT_STORE") {
            let mut payload =
                crate::native_store_format::read_committed(std::path::Path::new(&store))
                    .map_err(|error| format!("read-only ROTT store: {error:?}"))?
                    .ok_or("ROTT store has no committed overlay")?;
            if std::env::var_os("DOSLET_ROTT_DISABLE_JOYSTICK").is_some() {
                // Diagnostic only: edit the imported copy, never the user's store.
                let setting = b"JoystickEnabled    1";
                let offset = payload
                    .windows(setting.len())
                    .position(|bytes| bytes == setting)
                    .ok_or("ROTT overlay has no enabled joystick setting")?;
                payload[offset + setting.len() - 1] = b'0';
            }
            let mut session = crate::native_zip::load_game_with_settings(
                engine.source.as_deref().unwrap(),
                &engine.launch,
            )
            .map_err(|error| error.to_string())?;
            session
                .import_overlay(&payload)
                .map_err(|error| format!("ROTT overlay: {error:?}"))?;
            engine.storage = Some(
                crate::native_storage::NativeStorage::prepare_mut(&mut session, &engine.launch)
                    .map_err(|error| error.to_string())?,
            );
            session.start();
            engine.session = session.into();
        }
        Ok(engine)
    });
    app.from_library = true;
    let mut renderer = iced::futures::executor::block_on(
        <iced::Renderer as renderer::Headless>::new(Font::DEFAULT, Pixels(16.0), Some("wgpu")),
    )
    .expect("headless renderer");
    let mut cache = Cache::default();
    let size = Size::new(960.0, 640.0);
    let cursor = mouse::Cursor::Available(Point::new(480.0, 320.0));
    let advance = |app: &mut crate::desktop::Doslet, millis| {
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(millis) {
            let _ = app.update(Message::Tick);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            app.session.status(),
            SessionStatus::Running,
            "{:?}",
            app.session.fault()
        );
    };
    advance(&mut app, 10000);
    let escapes = std::env::var("DOSLET_ROTT_MENU_ESCAPES")
        .map(|value| value.parse::<usize>().unwrap())
        .unwrap_or(3);
    for (i, (named, code)) in std::iter::repeat_n((Named::Escape, Code::Escape), escapes)
        .chain([
            (Named::ArrowDown, Code::ArrowDown),
            (Named::ArrowDown, Code::ArrowDown),
            (Named::ArrowUp, Code::ArrowUp),
            (Named::ArrowDown, Code::ArrowDown),
            (Named::ArrowUp, Code::ArrowUp),
        ])
        .enumerate()
    {
        for pressed in [true, false] {
            let key = Key::Named(named);
            let event = if pressed {
                Event::KeyPressed {
                    key: key.clone(),
                    modified_key: key,
                    physical_key: Physical::Code(code),
                    location: Location::Standard,
                    modifiers: Modifiers::empty(),
                    text: None,
                    repeat: false,
                }
            } else {
                Event::KeyReleased {
                    key: key.clone(),
                    modified_key: key,
                    physical_key: Physical::Code(code),
                    location: Location::Standard,
                    modifiers: Modifiers::empty(),
                }
            };
            let mut ui = UserInterface::build(app.view(), size, cache, &mut renderer);
            let mut messages = Vec::new();
            let (_, statuses) = ui.update(
                &[iced::Event::Keyboard(event.clone())],
                cursor,
                &mut renderer,
                &mut clipboard::Null,
                &mut messages,
            );
            cache = ui.into_cache();
            eprintln!("desktop key={named:?} pressed={pressed} status={statuses:?}");
            assert_eq!(
                statuses,
                [iced::event::Status::Ignored],
                "game keys must reach the worker"
            );
            for message in messages {
                let _ = app.update(message);
            }
            let _ = app.update(if statuses[0] == iced::event::Status::Ignored {
                Message::Keyboard(event)
            } else {
                Message::KeyboardCaptured(event)
            });
            advance(&mut app, if pressed { 150 } else { 650 });
        }
        if let Some(iced::widget::image::Handle::Rgba {
            width,
            height,
            pixels,
            ..
        }) = &app.display.image
        {
            image::RgbaImage::from_raw(*width, *height, pixels.to_vec())
                .unwrap()
                .save(root.join(format!("target/rott-v86-desktop-{i}.png")))
                .unwrap();
        }
    }
}

#[test]
#[cfg(feature = "v86")]
#[ignore = "requires local ROTT; bounded keyboard diagnostic without window or host audio"]
fn local_rott_v86_keyboard_probe() {
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
            let cli = crate::cli::Cli::parse_from([
                "doslet",
                "--config",
                root.join("games/ROTT.toml").to_str().unwrap(),
                "--v86",
                "--no-audio",
                "--drive-mode",
                "overlay",
            ])
            .resolve()
            .unwrap();
            let mut app = Doslet::new();
            app.launch = cli.launch;
            app.launch.persist = false;
            app.launch.preload_local_media = true;
            app.configurator
                .set_active(app.launch.machine_spec.clone().unwrap());
            app.init_game_session(cli.game.unwrap()).unwrap();
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(8) {
                let _ = app.worker_tick();
                assert_eq!(
                    app.session.status(),
                    SessionStatus::Running,
                    "{:?}",
                    app.session.fault()
                );
            }
            let mut key_state = None;
            for (usage, make) in [(41, &[0x01][..]), (81, &[0xe0, 0x50]), (20, &[0x10])] {
                for pressed in [true, false] {
                    app.session.physical_key(usage, pressed).unwrap();
                    let mut reads = Vec::new();
                    for _ in 0..20000 {
                        let nanos = app.session.emulated_nanos() + 1000;
                        app.session.advance_v86_host_time(nanos).unwrap();
                        let pc = app.session.debug_pc();
                        let mut code = [0; 2];
                        assert!(app.session.debug_read_memory(pc, &mut code));
                        let dx = app.session.registers().get(doslet_x86::Gpr16::Dx);
                        app.session.advance(1);
                        if code.starts_with(&[0xe4, 0x60])
                            || (code.first() == Some(&0xec) && dx == 0x60)
                        {
                            let ax = app.session.registers().get(doslet_x86::Gpr16::Ax);
                            reads.push(ax as u8);
                            if usage == 41 && pressed {
                                let isr = app
                                    .session
                                    .ram_view(pc as usize..pc as usize + 384)
                                    .unwrap();
                                // Locate this build's Keystate[ECX] = 1 store in
                                // the live ISR, without a fixed load address.
                                let store = isr
                                    .windows(11)
                                    .find(|bytes| {
                                        bytes[..3] == [0xc7, 0x04, 0x8d]
                                            && bytes[7..] == [1, 0, 0, 0]
                                    })
                                    .expect("ROTT keyboard ISR Keystate store");
                                key_state =
                                    Some(u32::from_le_bytes(store[3..7].try_into().unwrap())
                                        as usize);
                            }
                        }
                        assert_eq!(
                            app.session.status(),
                            SessionStatus::Running,
                            "{:?}",
                            app.session.fault()
                        );
                    }
                    eprintln!(
                        "ROTT keyboard usage={usage} pressed={pressed} reads={reads:02x?} pc={:x}",
                        app.session.debug_pc()
                    );
                    let mut expected = make.to_vec();
                    if !pressed {
                        *expected.last_mut().unwrap() |= 0x80;
                    }
                    assert_eq!(reads, expected, "game must read every keyboard byte");
                }
            }
            use iced::keyboard::{
                Event, Key, Location, Modifiers,
                key::{Code, Named, Physical},
            };
            let base = key_state.unwrap();
            for (key, code, scan) in [
                (Named::ArrowDown, Code::ArrowDown, 0x50),
                (Named::ArrowUp, Code::ArrowUp, 0x48),
                (Named::ArrowLeft, Code::ArrowLeft, 0x4b),
                (Named::ArrowRight, Code::ArrowRight, 0x4d),
            ] {
                for pressed in [true, false] {
                    let key = Key::Named(key);
                    let event = if pressed {
                        Event::KeyPressed {
                            key: key.clone(),
                            modified_key: key,
                            physical_key: Physical::Code(code),
                            location: Location::Standard,
                            modifiers: Modifiers::empty(),
                            text: None,
                            repeat: false,
                        }
                    } else {
                        Event::KeyReleased {
                            key: key.clone(),
                            modified_key: key,
                            physical_key: Physical::Code(code),
                            location: Location::Standard,
                            modifiers: Modifiers::empty(),
                        }
                    };
                    let _ = app.update(Message::Keyboard(event));
                    let began = Instant::now();
                    while began.elapsed() < Duration::from_millis(150) {
                        let _ = app.worker_tick();
                    }
                    let actual = app
                        .session
                        .ram_view(base + scan * 4..base + scan * 4 + 4)
                        .unwrap();
                    eprintln!(
                        "ROTT normal worker scan={scan:x} pressed={pressed} Keystate={actual:?}"
                    );
                    assert_eq!(actual, u32::from(pressed).to_le_bytes());
                }
            }
        })
        .unwrap()
        .join()
        .unwrap();
}

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
