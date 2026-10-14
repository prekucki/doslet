use doslet_mt32_gm::{Target, Translator, tables};

fn translate(translator: &mut Translator, messages: &[&[u8]]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for message in messages {
        translator.translate(message, &mut |bytes: &[u8]| out.push(bytes.to_vec()));
    }
    out
}
/// Roland DT1 to the MT-32 (device 10h) with a valid checksum.
fn dt1(device: u8, address: [u8; 3], data: &[u8]) -> Vec<u8> {
    let sum: u32 = address.iter().chain(data).map(|b| u32::from(*b)).sum();
    let mut message = vec![0xF0, 0x41, device, 0x16, 0x12];
    message.extend_from_slice(&address);
    message.extend_from_slice(data);
    message.extend_from_slice(&[((128 - sum % 128) % 128) as u8, 0xF7]);
    message
}
/// One active synth partial; offsets follow the MT-32 timbre parameter map.
fn timbre(name: &str, coarse: u8, attack: u8, sustain: u8, saw: bool) -> Vec<u8> {
    let mut timbre = vec![0; 246];
    timbre[..10].copy_from_slice(format!("{name:<10}").as_bytes());
    timbre[12] = 0b0001;
    let partial = &mut timbre[14..72];
    partial[0] = coarse;
    partial[1] = 50;
    partial[2] = 11;
    partial[4] = u8::from(saw);
    partial[41] = 100;
    partial[49] = attack;
    partial[57] = sustain;
    timbre
}
fn ready() -> Translator {
    let mut translator = Translator::new();
    translator.setup(&mut |_: &[u8]| {});
    translator
}

#[test]
fn power_on_setup_routes_parts_from_channel_two_and_ignores_channel_one() {
    let mut translator = Translator::new();
    let out = translate(&mut translator, &[&[0x91, 60, 100], &[0x90, 60, 100]]);
    // Part 1 powers on with Slap Bass 1, GM 36, and a 12-semitone bend range.
    let setup = &out[..out.len() - 1];
    assert!(setup.contains(&vec![0xC0, 36]));
    for rpn in [[0xB0, 101, 0], [0xB0, 100, 0], [0xB0, 6, 12]] {
        assert!(setup.contains(&rpn.to_vec()));
    }
    assert!(setup.contains(&vec![0xB0, 10, 64]));
    assert!(setup.contains(&vec![0xB9, 91, 48]));
    assert_eq!(out.last().unwrap(), &vec![0x90, 60, 100]);
    assert!(
        !out.iter()
            .any(|m| m[0] == 0x90 && m.len() == 3 && m != out.last().unwrap())
    );
}

#[test]
fn presets_map_to_gm_and_pan_is_reversed() {
    let mut translator = ready();
    let out = translate(
        &mut translator,
        &[
            &[0xC2, 0],
            &[0xB2, 10, 0],
            &[0xB2, 10, 127],
            &[0xB2, 7, 127],
        ],
    );
    assert_eq!(
        out,
        [
            vec![0xC1, 0],
            vec![0xB1, 10, 127],
            vec![0xB1, 10, 0],
            vec![0xB1, 7, 127]
        ]
    );
    assert_eq!(tables::PRESETS[48], ("Str Sect 1", 48));
    // Triangle and Cymbal have no GM melodic program; they use drum-kit notes.
    let out = translate(
        &mut translator,
        &[&[0xC2, 121], &[0x92, 60, 90], &[0x82, 60, 0]],
    );
    assert_eq!(out, [vec![0x99, 81, 90], vec![0x89, 81, 0]]);
    assert_eq!(translator.part_target(1), Target::Drum(81));
}

#[test]
fn guest_timbres_use_names_or_parameters_and_follow_memory_rewrites() {
    let mut translator = ready();
    let strings = timbre("Strings", 36, 10, 80, true);
    let patch = dt1(0x10, [0x05, 0x00, 0x00], &[2, 0, 24, 50, 12, 0, 1, 0]);
    let out = translate(
        &mut translator,
        &[&dt1(0x10, [0x08, 0x00, 0x00], &strings), &patch, &[0xC1, 0]],
    );
    assert_eq!(out, [vec![0xC0, 48]]);
    // Rewriting the memory timbre refreshes the part playing it; coarse C3
    // sounds an octave down, so GM notes are transposed.
    let bass = timbre("Bass 1", 24, 0, 80, false);
    let out = translate(
        &mut translator,
        &[&dt1(0x10, [0x08, 0x00, 0x00], &bass), &[0x91, 48, 100]],
    );
    assert_eq!(out, [vec![0xC0, 33], vec![0x90, 36, 100]]);
    // Unnamed: slow attack saw becomes strings, sustained square a lead.
    for (guest, program) in [
        (timbre("", 36, 80, 80, true), 48),
        (timbre("x9", 36, 0, 80, false), 80),
        (timbre("", 36, 0, 0, false), 4),
    ] {
        translate(&mut translator, &[&dt1(0x10, [0x08, 0x00, 0x00], &guest)]);
        assert_eq!(translator.part_target(0), Target::Program(program));
    }
    assert!(translator.stats().guest_timbre_loads >= 4);
}

#[test]
fn whole_timbre_temp_writes_replace_the_playing_preset() {
    let mut translator = ready();
    let flute = timbre("Flute", 36, 20, 80, false);
    translate(&mut translator, &[&dt1(0x10, [0x04, 0x00, 0x00], &flute)]);
    assert_eq!(translator.part_target(0), Target::Program(73));
    // A partial edit of a ROM preset cannot be interpreted and keeps the preset.
    translate(&mut translator, &[&dt1(0x10, [0x04, 0x01, 0x76], &[0, 0])]);
    assert_eq!(translator.part_target(1), Target::Program(48));
}

#[test]
fn key_shift_transposes_and_releases_follow_the_sounding_note() {
    let mut translator = ready();
    let shift = |value| dt1(0x10, [0x03, 0x00, 0x02], &[value]);
    let out = translate(
        &mut translator,
        &[&shift(36), &[0x91, 60, 100], &shift(24), &[0x81, 60, 0]],
    );
    assert_eq!(out, [vec![0x90, 72, 100], vec![0x80, 72, 0]]);
    // Channel-addressed SysEx (device = MIDI channel) writes that part's temp.
    let out = translate(
        &mut translator,
        &[&dt1(0x01, [0x00, 0x00, 0x02], &[12]), &[0x91, 60, 100]],
    );
    assert_eq!(out, [vec![0x90, 48, 100]]);
}

#[test]
fn rhythm_keys_map_by_default_and_by_rewritten_timbre() {
    let mut translator = ready();
    let out = translate(
        &mut translator,
        &[
            &[0x99, 36, 100],
            &[0x99, 73, 100],
            &[0x99, 80, 100],
            &[0x89, 36, 0],
        ],
    );
    assert_eq!(
        out,
        [vec![0x99, 36, 100], vec![0x99, 58, 100], vec![0x89, 36, 0]]
    );
    assert_eq!(translator.stats().unmapped_notes, 1);
    // Keys 41 and 60 get R05 (Acou Low Tom); key 41 keeps its own tom pitch.
    let key = |key: u8| {
        let offset = 0x90 + (usize::from(key) - 24) * 4;
        [0x03, (offset >> 7) as u8, (offset & 0x7F) as u8]
    };
    let kick = timbre("Kick", 36, 0, 0, false);
    let out = translate(
        &mut translator,
        &[
            &dt1(0x10, key(41), &[64 + 4, 100, 7, 1]),
            &dt1(0x10, key(60), &[64 + 4, 100, 7, 1]),
            &dt1(0x10, [0x08, 0x02, 0x00], &kick),
            &dt1(0x10, key(62), &[1, 100, 7, 1]),
            &[0x99, 41, 90],
            &[0x99, 60, 90],
            &[0x99, 62, 90],
        ],
    );
    assert_eq!(
        out,
        [vec![0x99, 41, 90], vec![0x99, 43, 90], vec![0x99, 36, 90]]
    );
}

#[test]
fn channel_assignment_moves_parts_and_releases_their_notes() {
    let mut translator = ready();
    let assign = dt1(0x10, [0x10, 0x00, 0x0D], &[0, 1, 2, 3, 4, 5, 6, 7, 9]);
    let out = translate(
        &mut translator,
        &[
            &[0x91, 60, 100],
            &assign,
            &[0x90, 62, 100],
            &[0x91, 64, 100],
        ],
    );
    assert_eq!(
        out,
        [
            vec![0x90, 60, 100],
            vec![0x80, 60, 64],
            vec![0x90, 62, 100],
            vec![0x91, 64, 100]
        ]
    );
    // A part can be switched off entirely (channel value 16).
    let off = dt1(0x10, [0x10, 0x00, 0x0D], &[16]);
    assert!(
        translate(&mut translator, &[&off, &[0x90, 62, 100]])
            .iter()
            .all(|m| m[0] != 0x90)
    );
}

#[test]
fn master_volume_reverb_and_bend_range_refresh_gm_controls() {
    let mut translator = ready();
    let out = translate(&mut translator, &[&dt1(0x10, [0x10, 0x00, 0x16], &[50])]);
    // Output level 80 at master volume 50 is 50/127 on every used channel.
    assert_eq!(out.len(), 9);
    assert!(out.contains(&vec![0xB0, 7, 50]) && out.contains(&vec![0xB9, 7, 50]));
    let out = translate(&mut translator, &[&dt1(0x10, [0x10, 0x00, 0x03], &[7])]);
    assert!(out.contains(&vec![0xB0, 91, 112]) && out.contains(&vec![0xB9, 91, 112]));
    let out = translate(
        &mut translator,
        &[&[0xB1, 101, 0], &[0xB1, 100, 0], &[0xB1, 6, 2]],
    );
    assert_eq!(out[2], [0xB0, 6, 2]);
}

#[test]
fn invalid_and_foreign_sysex_are_counted_not_applied() {
    let mut translator = ready();
    let mut corrupt = dt1(0x10, [0x10, 0x00, 0x16], &[10]);
    let checksum = corrupt.len() - 2;
    corrupt[checksum] ^= 1;
    let gm_on = [0xF0, 0x7E, 0x7F, 0x09, 0x01, 0xF7];
    let out = translate(
        &mut translator,
        &[
            &corrupt,
            &gm_on,
            &dt1(0x10, [0x30, 0x00, 0x00], &[1]),
            &[0xF8],
            &[0xFF],
        ],
    );
    assert!(out.is_empty());
    let stats = translator.stats();
    assert_eq!(
        (stats.sysex, stats.checksum_errors, stats.ignored_sysex),
        (3, 1, 2)
    );
}

#[test]
fn reset_and_display_follow_the_device() {
    let mut translator = ready();
    translate(
        &mut translator,
        &[
            &dt1(0x10, [0x20, 0x00, 0x00], b"Hello MT-32"),
            &[0xC1, 0],
            &[0x91, 60, 1],
        ],
    );
    assert!(translator.display().starts_with(b"Hello MT-32"));
    let out = translate(&mut translator, &[&dt1(0x10, [0x7F, 0x00, 0x00], &[0])]);
    assert_eq!(out[0], [0x80, 60, 64]);
    assert!(out.contains(&vec![0xC0, 36]));
    assert_eq!(translator.stats().sysex, 2);
}

#[test]
fn translation_after_setup_allocates_nothing() {
    let mut translator = ready();
    let strings = dt1(
        0x10,
        [0x08, 0x00, 0x00],
        &timbre("Strings", 36, 10, 80, true),
    );
    let messages: [&[u8]; 6] = [
        &strings,
        &[0xC1, 64],
        &[0x91, 60, 100],
        &[0xE1, 0, 64],
        &[0x99, 36, 100],
        &[0xB1, 123, 0],
    ];
    let mut bytes = 0;
    let measured = allocation_counter::measure(|| {
        for message in messages {
            translator.translate(message, &mut |out: &[u8]| bytes += out.len());
        }
    });
    assert_eq!(measured.count_total, 0);
    assert!(bytes > 0);
}
