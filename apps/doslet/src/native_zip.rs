//! Native ZIP adapter; archives are read with bounds and never extracted to disk.
use crate::{archive::GameArchive, native_input::LaunchSettings};
use doslet_config::{MachineSpec, SourceKind, WritePolicy};
use doslet_runtime::Session;
use std::{error::Error, fs::File, io::Read, path::Path, sync::Arc};

pub fn mount(spec: &MachineSpec) -> Option<&doslet_config::MountSpec> {
    spec.mounts
        .iter()
        .find(|m| m.letter == b'C' && m.kind == SourceKind::BrowserZip)
}

pub fn read(path: &Path) -> Result<GameArchive, Box<dyn Error>> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(crate::archive::MAX_ZIP_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    GameArchive::new(Arc::new(bytes)).map_err(Into::into)
}

pub fn select(archive: &GameArchive, program: Option<&str>) -> Result<String, String> {
    if let Some(program) = program {
        let program = program.replace('\\', "/").to_ascii_uppercase();
        if archive.programs.contains(&program) {
            return Ok(program);
        }
    } else if archive.programs.len() == 1 {
        return Ok(archive.programs[0].clone());
    }
    Err(format!(
        "Choose --program from the ZIP: {}",
        archive.programs.join(", ")
    ))
}

pub fn load_game_with_settings(
    path: &Path,
    settings: &LaunchSettings,
) -> Result<Session, Box<dyn Error>> {
    let Some((spec, mount)) = settings
        .machine_spec
        .as_ref()
        .and_then(|s| mount(s).map(|m| (s, m)))
    else {
        return crate::native_input::load_game_with_settings(path, settings);
    };
    if settings.persist || mount.policy == WritePolicy::LegacyWriteback {
        return Err(
            "ZIP sources cannot use rw; use overlay, ro, or a separate persistent save store"
                .into(),
        );
    }
    if !settings.patches.is_empty() {
        return Err("Memory patch manifests for ZIP launches are not supported yet".into());
    }
    let archive = read(Path::new(spec.binding(&mount.source)))?;
    let program = select(&archive, path.to_str())?;
    let mut session = archive.load_configured(&program, spec)?;
    session.configure_launch(&settings.command_tail, &settings.working_directory)?;
    if settings.preload_local_media
        && let Some((letter, image)) = &settings.cdrom
    {
        session = session.with_cdrom_image(*letter, std::fs::read(image)?)?;
    }
    crate::native_input::configure_runtime(&mut session, settings)?;
    Ok(session)
}

pub fn bind_game(
    controller: &mut crate::native_speed_tiers::Controller,
    path: &Path,
    settings: &LaunchSettings,
    directory: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    if let Some((spec, mount)) = settings
        .machine_spec
        .as_ref()
        .and_then(|s| mount(s).map(|m| (s, m)))
    {
        let archive = read(Path::new(spec.binding(&mount.source)))?;
        let program = select(&archive, path.to_str())?;
        let bytes = archive.program_bytes(&program)?;
        controller.bind_game_bytes(&bytes, directory)
    } else {
        controller.bind_game(path, directory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cli::Cli, profile::tests::Fixture};
    use clap::Parser;
    use zip::CompressionMethod;

    const EXIT: &[u8] = &[0xb8, 7, 0x4c, 0xcd, 0x21];

    #[test]
    fn archived_program_uses_the_same_fingerprint_as_host_binary() {
        let fixture = Fixture::new("version=2");
        let path = zip(&fixture, &[("GAME/PLAY.COM", EXIT)]);
        let cli = Cli::parse_from(["doslet", "--zip", path.to_str().unwrap(), "--no-audio"])
            .resolve()
            .unwrap();
        let mut archived = crate::native_speed_tiers::Controller::untrained();
        bind_game(
            &mut archived,
            cli.game.as_deref().unwrap(),
            &cli.launch,
            Some(&fixture.root),
        )
        .unwrap();
        let host = fixture.root.join("renamed.com");
        std::fs::write(&host, EXIT).unwrap();
        let mut unpacked = crate::native_speed_tiers::Controller::untrained();
        unpacked.bind_game(&host, Some(&fixture.root)).unwrap();
        assert_eq!(archived.profile_info().path, unpacked.profile_info().path);
        std::fs::write(&host, [0x90, 0xc3]).unwrap();
        let mut changed = crate::native_speed_tiers::Controller::untrained();
        changed.bind_game(&host, Some(&fixture.root)).unwrap();
        assert_ne!(archived.profile_info().path, changed.profile_info().path);
        assert!(
            bind_game(
                &mut archived,
                Path::new("MISSING.COM"),
                &cli.launch,
                Some(&fixture.root)
            )
            .is_err()
        );
    }

    fn zip(fixture: &Fixture, files: &[(&str, &[u8])]) -> std::path::PathBuf {
        let path = fixture.root.join("game.zip");
        std::fs::write(
            &path,
            crate::archive::tests::pack(files, CompressionMethod::Deflated).as_slice(),
        )
        .unwrap();
        path
    }

    #[test]
    fn zip_cli_selects_program_case_insensitively_and_reset_keeps_hardware() {
        let fixture = Fixture::new("version=2");
        let path = zip(&fixture, &[("GAME/PLAY.COM", EXIT), ("SETUP.COM", EXIT)]);
        let cli = Cli::parse_from([
            "doslet",
            "--zip",
            path.to_str().unwrap(),
            "--program",
            "game\\play.com",
            "--no-audio",
            "--memory-mib",
            "8",
            "--cpu-mhz",
            "66",
            "--no-dynamic-core",
        ])
        .resolve()
        .unwrap();
        assert_eq!(cli.game.as_deref(), Some(Path::new("GAME/PLAY.COM")));
        for _ in 0..2 {
            let mut session =
                load_game_with_settings(cli.game.as_deref().unwrap(), &cli.launch).unwrap();
            assert_eq!(session.hardware_config().memory_mib, 8);
            assert_eq!(session.hardware_config().cpu_mhz, 66);
            session.start();
            session.advance(100);
            assert_eq!(session.status(), doslet_runtime::SessionStatus::Exited(7));
        }
    }

    #[test]
    fn desktop_zip_launch_and_reset_keep_the_archive_recipe() {
        let fixture = Fixture::new("version=2");
        let path = zip(&fixture, &[("PLAY.COM", EXIT)]);
        let cli = Cli::parse_from([
            "doslet",
            "--zip",
            path.to_str().unwrap(),
            "--no-audio",
            "--no-speed-tiers",
            "--cpu-mhz",
            "66",
        ]);
        let mut app = crate::app::Doslet::from_cli(cli).unwrap();
        app.session.advance(100);
        assert_eq!(
            app.session.status(),
            doslet_runtime::SessionStatus::Exited(7)
        );
        let _ = app.update(crate::app::Message::Reset);
        assert_eq!(app.session.steps(), 0);
        assert_eq!(app.session.hardware_config().cpu_mhz, 66);
        app.session.advance(100);
        assert_eq!(
            app.session.status(),
            doslet_runtime::SessionStatus::Exited(7)
        );
    }

    #[test]
    fn ambiguous_or_missing_program_and_writeback_are_rejected() {
        let fixture = Fixture::new("version=2");
        let path = zip(&fixture, &[("PLAY.COM", EXIT), ("SETUP.COM", EXIT)]);
        for extra in [
            vec![],
            vec!["--program", "missing.exe"],
            vec!["--program", "PLAY.COM", "--drive-mode", "rw"],
        ] {
            let mut args = vec!["doslet", "--zip", path.to_str().unwrap(), "--no-audio"];
            args.extend(extra);
            let error = Cli::parse_from(args).resolve().unwrap_err().to_string();
            assert!(
                error.contains("PLAY.COM") || error.contains("cannot use rw"),
                "{error}"
            );
        }
    }

    #[test]
    fn zip_profile_resolves_paths_bindings_and_round_trips() {
        for source in [
            "source='../game.zip'",
            "source='game'\n[[bindings]]\nid='game'\nkind='zip'\nlocation='../game.zip'",
        ] {
            let fixture = Fixture::new(&format!(
                "version=2\n[game]\nexecutable='play.com'\n[[drives]]\nletter='C'\nkind='zip'\npolicy='temporary'\n{source}\n"
            ));
            zip(&fixture, &[("PLAY.COM", EXIT)]);
            let cli = fixture.cli(&["--no-audio"]).resolve().unwrap();
            let spec = cli.launch.machine_spec.as_ref().unwrap();
            let text = crate::machine_profile::export_machine_profile(spec);
            assert_eq!(
                crate::machine_profile::parse_machine_profile(&text).unwrap(),
                *spec
            );
            let mut session =
                load_game_with_settings(cli.game.as_deref().unwrap(), &cli.launch).unwrap();
            session.start();
            session.advance(100);
            assert_eq!(session.status(), doslet_runtime::SessionStatus::Exited(7));
        }
    }

    #[test]
    fn sole_program_is_selected_automatically_and_cli_options_conflict() {
        let fixture = Fixture::new("version=2");
        let path = zip(&fixture, &[("PLAY.COM", EXIT)]);
        let cli = Cli::parse_from(["doslet", "--zip", path.to_str().unwrap()])
            .resolve()
            .unwrap();
        assert_eq!(cli.game.as_deref(), Some(Path::new("PLAY.COM")));
        assert!(Cli::try_parse_from(["doslet", "--program", "PLAY.COM"]).is_err());
        assert!(
            Cli::try_parse_from(["doslet", "--zip", "game.zip", "--game", "PLAY.COM"]).is_err()
        );
    }

    #[test]
    #[ignore = "requires the user's local ROTT archive; bounded compatibility probe"]
    fn local_rott_zip_startup() {
        let cli = Cli::parse_from([
            "doslet",
            "--config",
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../games/ROTT-sound.toml"),
            "--no-audio",
            "--no-speed-tiers",
        ])
        .resolve()
        .unwrap();
        let mut session =
            load_game_with_settings(cli.game.as_deref().unwrap(), &cli.launch).unwrap();
        session
            .set_keyboard_mode(doslet_config::KeyboardMode::At)
            .unwrap();
        session.start();
        session.capture_audio_sources(true);
        let mut pcm = [[0; 2]; 1024];
        let mut midi = [0; 1024];
        let mut source = [doslet_pcm::SourceFrame::default(); 1024];
        let mut fm_peak = 0_u16;
        let mut fm_frames = 0;
        let mut drain = |session: &mut Session| {
            while session.drain_pcm(&mut pcm) != 0 {}
            session.drain_midi(&mut midi);
            while session.drain_audio_source(false, &mut source) != 0 {}
            loop {
                let count = session.drain_audio_source(true, &mut source);
                if count == 0 {
                    break;
                }
                fm_frames += count;
                for frame in &source[..count] {
                    for sample in frame.samples {
                        fm_peak = fm_peak.max(sample.unsigned_abs());
                    }
                }
            }
        };
        while session.steps() < 100_000_000
            && session.status() == doslet_runtime::SessionStatus::Running
        {
            session.advance(10_000);
            drain(&mut session);
        }
        // Configure PCM first (required by ROTT's SB FM driver), then choose
        // Sound Blaster music and open the music-volume playback screen.
        for usage in [
            0, 40, 81, 81, 40, 40, 40, 40, 40, 40, 0, 0, 0, 0, 81, 40, 81, 81, 40, 0, 0, 0, 81, 81,
            40, 0, 0,
            // Leave music volume, select Wave Blaster and exercise register 83h.
            41, 82, 82, 40, 81, 81, 81, 81, 81, 81, 40, 40, 0, 0,
        ] {
            for (pressed, steps) in [(true, 400_000), (false, 5_000_000)] {
                if usage != 0 {
                    session.physical_key(usage, pressed).unwrap();
                }
                let end = session.steps() + steps;
                while session.steps() < end
                    && session.status() == doslet_runtime::SessionStatus::Running
                {
                    session.advance(10_000);
                    drain(&mut session);
                }
            }
            println!("ROTT key {usage}: {:?}", session.audio_status());
        }
        println!("Pure FM: {fm_frames} frames, peak {fm_peak}");
        println!("ROTT audio: {:?}", session.audio_status());
        println!(
            "ROTT: steps={}, status={:?}, fault={:?}, mode={:?}, BX={:04x}, CX={:04x}, VGA={:x?}\n{}",
            session.steps(),
            session.status(),
            session.fault(),
            session.video_mode(),
            session.registers().get(doslet_x86::Gpr16::Bx),
            session.registers().get(doslet_x86::Gpr16::Cx),
            session.video_memory_configuration(),
            String::from_utf8_lossy(session.console())
        );
        if let Some(frame) = session.frame() {
            let mut rgba = vec![0; frame.width * frame.height * 4];
            frame.write_rgba(&mut rgba).unwrap();
            let path =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/rott-sound-mode-x.png");
            let mut encoder = png::Encoder::new(
                File::create(&path).unwrap(),
                frame.width as u32,
                frame.height as u32,
            );
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&rgba)
                .unwrap();
            println!(
                "ROTT frame: {}x{} {}",
                frame.width,
                frame.height,
                path.display()
            );
        }
        assert!(session.steps() > 0);
        let console = String::from_utf8_lossy(session.console());
        assert!(console.contains("SD_SetupFXCard: Fx ok."), "{console}");
        assert!(console.contains("SD_Startup: Fx ok."), "{console}");
        assert_eq!(session.fault(), None);
        assert!(fm_peak > 400, "pure FM peak {fm_peak}");
        assert!(session.audio_status().opl_key_on_writes > 0);
        assert_eq!(session.audio_status().irq_enabled_sources, 3);
        assert!(session.midi_stats().1 > 0, "Wave Blaster must emit MIDI");
        assert_eq!(&session.audio_status().fm_levels[..2], &[0xf8, 0xf8]);
        assert!(
            !matches!(
                session.fault(),
                Some(doslet_runtime::RuntimeError::UnsupportedService {
                    vector: 0x10,
                    ax: 0x0100
                })
            ),
            "ROTT must pass its cursor-hide call"
        );
    }
}
