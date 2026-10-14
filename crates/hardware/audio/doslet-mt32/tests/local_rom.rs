use doslet_mt32::{Error, Timeline};

#[test]
fn invalid_roms_and_rates_are_rejected_before_parsing() {
    assert!(matches!(
        Timeline::new(&[], &[], 48000),
        Err(Error::UnsupportedRom)
    ));
    assert!(matches!(Timeline::new(&[], &[], 0), Err(Error::Rate)));
}

#[test]
#[ignore = "requires DOSLET_MT32_ROM_DIR with local CM-32L ROMs"]
fn local_rom_notes_sysex_rendering_and_chunk_boundaries() {
    let path = std::path::PathBuf::from(
        std::env::var_os("DOSLET_MT32_ROM_DIR").expect("set DOSLET_MT32_ROM_DIR"),
    );
    let control = std::fs::read(path.join("CM32L_CONTROL.ROM")).unwrap();
    let pcm = std::fs::read(path.join("CM32L_PCM.ROM")).unwrap();
    for rate in [22050, 32000, 44100, 48000, 96000, 192000] {
        let mut one = Timeline::new(&control, &pcm, rate).unwrap();
        let mut chunks = Timeline::new(&control, &pcm, rate).unwrap();
        for engine in [&mut one, &mut chunks] {
            // Valid Roland DT1 master-volume write, then a channel-2 note.
            for byte in [
                0xF0, 0x41, 0x10, 0x16, 0x12, 0x10, 0, 0x16, 100, 0x76, 0xF7, 0x91, 60, 100,
            ] {
                engine.push_byte(byte).unwrap();
            }
        }
        let mut a = Vec::new();
        let mut b = Vec::new();
        let mut output = [[0.0; 2]; 256];
        for (engine, results, size) in [(&mut one, &mut a, 256), (&mut chunks, &mut b, 7)] {
            loop {
                let n = engine
                    .render_until(250_000_000, &mut output[..size])
                    .unwrap();
                if n == 0 {
                    break;
                }
                results.extend_from_slice(&output[..n]);
            }
        }
        assert_eq!(a, b);
        assert_eq!(a.len(), rate as usize / 4);
        assert!(a.iter().flatten().any(|sample| sample.abs() > 0.001));
        assert_eq!(one.note_events(), 1);
        assert_eq!(one.sysex_messages(), 1);
        let measured = allocation_counter::measure(|| {
            one.render_until(255_000_000, &mut output).unwrap();
        });
        assert_eq!(measured.count_total, 0, "render rate {rate}");
        assert!(matches!(
            one.render_until(0, &mut output),
            Err(Error::TimeReversed)
        ));
    }
}

/// Compares the ROM-free MT-32-to-GM tables with CM-32L v1.02: preset order,
/// power-on programs and the default rhythm layout. ROM bytes are only read.
#[test]
#[ignore = "requires DOSLET_MT32_ROM_DIR with local CM-32L ROMs"]
fn local_rom_matches_gm_translation_tables() {
    use doslet_mt32_gm::tables;
    let path = std::path::PathBuf::from(
        std::env::var_os("DOSLET_MT32_ROM_DIR").expect("set DOSLET_MT32_ROM_DIR"),
    );
    let control = std::fs::read(path.join("CM32L_CONTROL.ROM")).unwrap();
    let pcm = std::fs::read(path.join("CM32L_PCM.ROM")).unwrap();
    Timeline::new(&control, &pcm, 48000).expect("CM-32L v1.02 ROM pair");
    let rom = moont::cm32l::Rom::new(&control, &pcm).unwrap();
    let mut timbres = Box::new([[0u8; 256]; 256]);
    rom.fill_raw_timbres(&mut timbres);
    let normalize = |name: &[u8]| -> Vec<u8> {
        name.iter()
            .filter(|b| b.is_ascii_alphanumeric())
            .map(u8::to_ascii_lowercase)
            .collect()
    };
    for (program, (name, _)) in tables::PRESETS.iter().enumerate() {
        assert_eq!(
            normalize(&timbres[program][..10]),
            normalize(name.as_bytes()),
            "program {program}"
        );
    }
    let programs = rom.default_programs().map(|program| program as u8);
    assert_eq!(programs, tables::POWER_ON_PROGRAMS);
    // CM-32L v1.02 keeps 85 four-byte rhythm keys (24..=108) at 8580h.
    for index in 0..85 {
        let key = 24 + index as u8;
        let timbre = control[0x8580 + index * 4];
        let rhythm = timbre.checked_sub(64).map(usize::from).filter(|r| *r < 64);
        let name = rhythm.map(|r| String::from_utf8_lossy(&timbres[192 + r][..10]).into_owned());
        match rhythm {
            Some(r) if r < tables::RHYTHM_TIMBRES.len() => {
                let note = tables::default_rhythm_note(key);
                let note = note.unwrap_or_else(|| panic!("key {key}: {name:?} has no GM note"));
                assert!(
                    tables::RHYTHM_TIMBRES[r].contains(&note),
                    "key {key}: R{:02} {name:?} -> {note}",
                    r + 1
                );
            }
            _ => assert_eq!(
                tables::default_rhythm_note(key),
                None,
                "key {key}: {name:?}"
            ),
        }
    }
}

#[test]
#[ignore = "requires DOSLET_MT32_ROM_DIR with local CM-32L ROMs"]
fn local_rom_gm_wrapper_plays_channel_one_and_rebases_without_rendering_past_time() {
    let path = std::path::PathBuf::from(std::env::var_os("DOSLET_MT32_ROM_DIR").unwrap());
    let control = std::fs::read(path.join("CM32L_CONTROL.ROM")).unwrap();
    let pcm = std::fs::read(path.join("CM32L_PCM.ROM")).unwrap();
    let mut engine = Timeline::with_gm(&control, &pcm, 48000, true).unwrap();
    engine.rebase(10_000_000_000);
    for byte in [0xC0, 0, 0x90, 60, 100] {
        engine.push_byte(byte).unwrap();
    }
    let mut frames = [[0.; 2]; 256];
    let mut peak = 0f32;
    while engine.render_until(10_250_000_000, &mut frames).unwrap() != 0 {
        for sample in frames.iter().flatten() {
            peak = peak.max(sample.abs());
        }
    }
    assert!(peak > 0.001);
    assert_eq!(engine.note_events(), 1);
    let measured = allocation_counter::measure(|| {
        engine.render_until(10_251_000_000, &mut frames).unwrap();
    });
    assert_eq!(measured.count_total, 0);
}
