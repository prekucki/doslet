//! Doslet's own MT-32-to-GM choices. Preset names and rhythm layout follow the
//! public MT-32 owner's manual; no ROM data is embedded.

/// Built-in preset names (program 0..=127) and the chosen GM program.
pub const PRESETS: [(&str, u8); 128] = [
    ("AcouPiano1", 0),
    ("AcouPiano2", 1),
    ("AcouPiano3", 1),
    ("ElecPiano1", 4),
    ("ElecPiano2", 5),
    ("ElecPiano3", 4),
    ("ElecPiano4", 5),
    ("Honkytonk", 3),
    ("Elec Org 1", 16),
    ("Elec Org 2", 17),
    ("Elec Org 3", 18),
    ("Elec Org 4", 16),
    ("Pipe Org 1", 19),
    ("Pipe Org 2", 19),
    ("Pipe Org 3", 20),
    ("Accordion", 21),
    ("Harpsi 1", 6),
    ("Harpsi 2", 6),
    ("Harpsi 3", 6),
    ("Clavi 1", 7),
    ("Clavi 2", 7),
    ("Clavi 3", 7),
    ("Celesta 1", 8),
    ("Celesta 2", 8),
    ("Syn Brass 1", 62),
    ("Syn Brass 2", 63),
    ("Syn Brass 3", 62),
    ("Syn Brass 4", 63),
    ("Syn Bass 1", 38),
    ("Syn Bass 2", 39),
    ("Syn Bass 3", 38),
    ("Syn Bass 4", 39),
    ("Fantasy", 88),
    ("Harmo Pan", 89),
    ("Chorale", 52),
    ("Glasses", 92),
    ("Soundtrack", 97),
    ("Atmosphere", 99),
    ("Warm Bell", 98),
    ("Funny Vox", 85),
    ("Echo Bell", 102),
    ("Ice Rain", 96),
    ("Oboe 2001", 68),
    ("Echo Pan", 75),
    ("Doctor Solo", 81),
    ("Schooldaze", 100),
    ("Bellsinger", 91),
    ("Square Wave", 80),
    ("Str Sect 1", 48),
    ("Str Sect 2", 49),
    ("Str Sect 3", 48),
    ("Pizzicato", 45),
    ("Violin 1", 40),
    ("Violin 2", 40),
    ("Cello 1", 42),
    ("Cello 2", 42),
    ("Contrabass", 43),
    ("Harp 1", 46),
    ("Harp 2", 46),
    ("Guitar 1", 24),
    ("Guitar 2", 25),
    ("Elec Gtr 1", 26),
    ("Elec Gtr 2", 27),
    ("Sitar", 104),
    ("Acou Bass 1", 32),
    ("Acou Bass 2", 32),
    ("Elec Bass 1", 33),
    ("Elec Bass 2", 34),
    ("Slap Bass 1", 36),
    ("Slap Bass 2", 37),
    ("Fretless 1", 35),
    ("Fretless 2", 35),
    ("Flute 1", 73),
    ("Flute 2", 73),
    ("Piccolo 1", 72),
    ("Piccolo 2", 72),
    ("Recorder", 74),
    ("Pan Pipes", 75),
    ("Sax 1", 64),
    ("Sax 2", 65),
    ("Sax 3", 66),
    ("Sax 4", 67),
    ("Clarinet 1", 71),
    ("Clarinet 2", 71),
    ("Oboe", 68),
    ("Engl Horn", 69),
    ("Bassoon", 70),
    ("Harmonica", 22),
    ("Trumpet 1", 56),
    ("Trumpet 2", 56),
    ("Trombone 1", 57),
    ("Trombone 2", 57),
    ("Fr Horn 1", 60),
    ("Fr Horn 2", 60),
    ("Tuba", 58),
    ("Brs Sect 1", 61),
    ("Brs Sect 2", 61),
    ("Vibe 1", 11),
    ("Vibe 2", 11),
    ("Syn Mallet", 12),
    ("Windbell", 112),
    ("Glock", 9),
    ("Tube Bell", 14),
    ("Xylophone", 13),
    ("Marimba", 12),
    ("Koto", 107),
    ("Sho", 20),
    ("Shakuhachi", 77),
    ("Whistle 1", 78),
    ("Whistle 2", 78),
    ("Bottleblow", 76),
    ("Breathpipe", 79),
    ("Timpani", 47),
    ("Melodic Tom", 117),
    ("Deep Snare", 118),
    ("Elec Perc 1", 118),
    ("Elec Perc 2", 118),
    ("Taiko", 116),
    ("Taiko Rim", 115),
    ("Cymbal", DRUM),
    ("Castanets", 115),
    ("Triangle", DRUM),
    ("Orche Hit", 55),
    ("Telephone", 124),
    ("Bird Tweet", 123),
    ("One Note Jam", 87),
    ("Water Bells", 112),
    ("Jungle Tune", 108),
];
/// Marks presets that GM only offers as drum-kit notes; see [`preset_drum`].
pub const DRUM: u8 = 0x80;
/// GM drum note for presets marked [`DRUM`].
pub const fn preset_drum(program: u8) -> Option<u8> {
    match program {
        119 => Some(49),
        121 => Some(81),
        _ => None,
    }
}

/// Level trim in dB per preset, so the GM mix keeps the CM-32L balance between
/// instruments. Measured K-weighted at velocity 100 on keys 48/60/72 against
/// CM-32L v1.02 with the macOS GM bank, relative to the median preset, then
/// limited to -24..=+6 dB by an opt-in local calibration; no ROM data is
/// embedded.
pub const PRESET_TRIM_DB: [i8; 128] = [
    5, 5, 2, 0, 2, 1, 6, 5, 0, 3, 0, 2, 6, 3, 2, -3, // 0..
    5, 2, 2, 1, 1, 0, 1, -1, 4, 5, 6, 5, -2, -12, 1, -6, // 16..
    0, 2, 0, 2, 1, -1, 1, -4, 4, 4, -4, 1, -1, 6, -8, 2, // 32..
    4, 6, 6, 0, -1, 0, -2, 0, -1, -6, -6, -2, -2, -1, 6, 0, // 48..
    -4, -7, 1, 2, -1, -5, -2, -2, -3, -2, -3, 0, 0, -2, 0, 0, // 64..
    3, -1, -2, 0, -3, -1, 1, 0, 2, 1, 6, 5, 6, 6, 0, 6, // 80..
    6, 5, -2, -1, -2, -5, 3, 6, 2, -2, 0, 6, -6, -2, 4, -4, // 96..
    -1, 0, -6, -6, -3, 2, 1, -11, -20, -13, 1, -1, -24, 2, 6, 0, // 112..
];
/// Rhythm-part trim in dB: GM kits play about 2 dB above the CM-32L kit.
pub const RHYTHM_TRIM_DB: i8 = -2;
/// Headroom in dB below GM full scale, which leaves room for positive trims.
pub const HEADROOM_DB: i8 = 6;

/// Transpose in semitones for presets that sound an octave above their key on
/// the CM-32L (Celesta 1/2, Bellsinger, Piccolo 2), unlike their GM programs.
pub const fn preset_transpose(program: u8) -> i8 {
    match program {
        22 | 23 | 46 | 75 => 12,
        _ => 0,
    }
}

/// `10000 * (level / 100)^1.2`: CM-32L part level attenuates by about
/// 48·log10, whereas GM channel volume follows 40·log10.
const LEVEL_CURVE: [u16; 101] = [
    0, 40, 91, 149, 210, 275, 342, 411, 483, 556, 631, 707, 785, 864, 945, 1026, 1109, 1193, 1277,
    1363, 1450, 1537, 1625, 1714, 1804, 1895, 1986, 2078, 2171, 2264, 2358, 2453, 2548, 2644, 2740,
    2837, 2935, 3033, 3131, 3231, 3330, 3430, 3531, 3632, 3734, 3836, 3938, 4041, 4145, 4248, 4353,
    4457, 4563, 4668, 4774, 4880, 4987, 5094, 5201, 5309, 5417, 5526, 5635, 5744, 5854, 5963, 6074,
    6184, 6295, 6406, 6518, 6630, 6742, 6855, 6968, 7081, 7194, 7308, 7422, 7536, 7651, 7766, 7881,
    7996, 8112, 8228, 8344, 8461, 8578, 8695, 8812, 8930, 9048, 9166, 9284, 9403, 9522, 9641, 9760,
    9880, 10000,
];
/// `1000 * 10^(dB / 40)` for -30..=0 dB, the GM volume and velocity scale.
const DB_FACTOR: [u16; 31] = [
    178, 188, 200, 211, 224, 237, 251, 266, 282, 299, 316, 335, 355, 376, 398, 422, 447, 473, 501,
    531, 562, 596, 631, 668, 708, 750, 794, 841, 891, 944, 1000,
];

/// Per-mille GM volume or velocity factor for `db` (clamped to -30..=0).
pub const fn db_factor(db: i8) -> u16 {
    let db = if db < -30 {
        -30
    } else if db > 0 {
        0
    } else {
        db
    };
    DB_FACTOR[(db + 30) as usize]
}

/// GM channel volume (CC7) for part output `level` and system master volume,
/// both 0..=100, plus a trim in dB. Master volume is linear in amplitude.
pub fn part_volume(level: u8, master: u8, trim_db: i8) -> u8 {
    let level = u64::from(LEVEL_CURVE[usize::from(level.min(100))]);
    let master = u64::from((u32::from(master.min(100)) * 10_000).isqrt());
    let gain = u64::from(db_factor(trim_db.saturating_sub(HEADROOM_DB)));
    let scale = 10_000 * 1_000 * 1_000;
    ((127 * level * master * gain + scale / 2) / scale).min(127) as u8
}

/// GM reverb send per MT-32 reverb level step, by mode (Room, Hall, Plate,
/// Tap delay) and reverb time 0..=7. Matches the CM-32L reverb energy after
/// note release on the macOS GM bank; Room at time 5 keeps 16 per step.
const REVERB_SEND: [[u8; 8]; 4] = [
    [4, 5, 6, 10, 12, 16, 22, 38],
    [7, 8, 10, 13, 16, 18, 27, 39],
    [4, 4, 5, 6, 9, 18, 22, 45],
    [2, 3, 3, 7, 7, 13, 12, 15],
];

/// GM reverb send (CC91) for MT-32 reverb mode, time and level (0..=7).
pub const fn reverb_send(mode: u8, time: u8, level: u8) -> u8 {
    let level = if level > 7 { 7 } else { level };
    let send = level as u16 * REVERB_SEND[(mode & 3) as usize][(time & 7) as usize] as u16;
    if send > 127 { 127 } else { send as u8 }
}

/// Power-on programs of melodic parts 1..=8. The local-ROM test in
/// `doslet-mt32` compares them with CM-32L v1.02.
pub const POWER_ON_PROGRAMS: [u8; 8] = [68, 48, 95, 78, 41, 3, 110, 122];

/// GM drum notes for rhythm timbres R01..=R30. The first note is used when a
/// timbre moves to another key; the others keep their own keys (tom pitches).
pub const RHYTHM_TIMBRES: [&[u8]; 30] = [
    &[36, 35], // Acou BD
    &[38],     // Acou SD
    &[50, 48], // Acou Hi Tom
    &[47, 45], // Acou Mid Tom
    &[43, 41], // Acou Low Tom
    &[40],     // Elec SD
    &[42],     // Closed HiHat
    &[46],     // Open HiHat 1
    &[49],     // Crash Cym
    &[51],     // Ride Cym
    &[37],     // Rim Shot
    &[39],     // Hand Clap
    &[56],     // Cowbell
    &[62],     // Mute Hi Conga
    &[63],     // High Conga
    &[64],     // Low Conga
    &[65],     // High Timbale
    &[66],     // Low Timbale
    &[60],     // High Bongo
    &[61],     // Low Bongo
    &[67],     // High Agogo
    &[68],     // Low Agogo
    &[54],     // Tambourine
    &[75],     // Claves
    &[70],     // Maracas
    &[72],     // Samba Whistle Long
    &[71],     // Samba Whistle Short
    &[69],     // Cabasa
    &[58],     // Quijada
    &[44],     // Open HiHat 2
];

/// GM drum note for a key whose rhythm setup the guest has not changed. The
/// MT-32 kit is GM-compatible on 35..=75; CM-32L effects above 75 have no GM
/// drum equivalent yet.
pub const fn default_rhythm_note(key: u8) -> Option<u8> {
    match key {
        35..=51 | 54 | 56 | 60..=72 | 75 => Some(key),
        73 => Some(58),
        _ => None,
    }
}

/// GM drum note for rhythm timbre `timbre` (R01 = 0) placed on `key`.
pub fn rhythm_timbre_note(timbre: u8, key: u8) -> Option<u8> {
    let notes = RHYTHM_TIMBRES.get(usize::from(timbre))?;
    match default_rhythm_note(key) {
        Some(note) if notes.contains(&note) => Some(note),
        _ => Some(notes[0]),
    }
}
