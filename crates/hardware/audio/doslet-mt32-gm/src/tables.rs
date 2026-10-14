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
