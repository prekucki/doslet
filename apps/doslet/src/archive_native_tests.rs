#[test]
#[ignore = "requires local games/ADLIBDEM.zip"]
fn local_adlibdem_zip_smoke() {
    use clap::Parser;
    use std::path::Path;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let config_path = repository.join("games/ADLIBDEM.toml");
    let cli = crate::cli::Cli::parse_from([
        "doslet",
        "--config",
        config_path.to_str().unwrap(),
        "--no-audio",
    ])
    .resolve()
    .unwrap();
    let mut session =
        crate::native_zip::load_game_with_settings(cli.game.as_deref().unwrap(), &cli.launch)
            .unwrap();
    session.start();
    let mut pcm = [[0; 2]; 128];
    let mut pcm_peak = 0i32;
    while session.steps() < 4_500_000 && session.status() == doslet_runtime::SessionStatus::Running
    {
        if session.steps() == 2_000_000 {
            session.push_key(0x1C, b'\r').unwrap();
        }
        session.advance(10_000);
        loop {
            let n = session.drain_pcm(&mut pcm);
            if n == 0 {
                break;
            }
            for frame in &pcm[..n] {
                pcm_peak = pcm_peak
                    .max(i32::from(frame[0]).abs())
                    .max(i32::from(frame[1]).abs());
            }
        }
    }
    assert_eq!(session.status(), doslet_runtime::SessionStatus::Running);
    assert_eq!(session.video_mode_label().to_string(), "320×200×2 CGA");
    let frame = session.presentation_frame();
    assert_eq!((frame.width(), frame.height()), (320, 200));
    let mut pixels = vec![0; 320 * 200 * 4];
    frame.write_rgba(&mut pixels).unwrap();
    assert!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[..3] != [0, 0, 0])
    );
    assert!(pcm_peak > 0, "expected OPL2 FM audio from ADLIBDEM");
    // Navigate to section 3 page 4 (resource 3004), which switches to 640x200 CGA mode 06h.
    session.push_key(0x04, b'3').unwrap();
    for _ in 0..4 {
        let target = session.steps() + 1_500_000;
        while session.steps() < target && session.status() == doslet_runtime::SessionStatus::Running
        {
            session.advance(10_000);
            while session.drain_pcm(&mut pcm) != 0 {}
        }
        session.push_key(0x4D, 0).unwrap();
    }
    let target = session.steps() + 1_500_000;
    while session.steps() < target && session.status() == doslet_runtime::SessionStatus::Running {
        session.advance(10_000);
        while session.drain_pcm(&mut pcm) != 0 {}
    }
    assert_eq!(session.status(), doslet_runtime::SessionStatus::Running);
    assert_eq!(session.video_mode_label().to_string(), "640×200×1 CGA");
    let frame = session.presentation_frame();
    assert_eq!((frame.width(), frame.height()), (640, 200));
}
