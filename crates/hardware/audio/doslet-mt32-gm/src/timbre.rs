//! Heuristic GM choice for timbres uploaded by the guest. Names are tried first;
//! otherwise envelope, waveform and register select a broad GM family.
use crate::Target;
use crate::memory::TIMBRE_SIZE;

const NAME: usize = 10;
const STRUCTURE_12: usize = 10;
const PARTIAL_MUTE: usize = 12;
const NO_SUSTAIN: usize = 13;
const PARTIAL: usize = 14;
const PARTIAL_SIZE: usize = 58;
const PITCH_COARSE: usize = 0;
const PITCH_KEYFOLLOW: usize = 2;
const WAVEFORM: usize = 4;
const TVA_ATTACK: usize = 49;
const TVA_SUSTAIN: usize = 57;
/// Pitch coarse value that sounds the played key (C4).
const COARSE_UNITY: i8 = 36;
/// Keyfollow index for 1:1 pitch tracking.
const KEYFOLLOW_UNITY: u8 = 11;

/// Melodic keywords; earlier entries win, so specific names precede generic ones.
const MELODIC: &[(&str, u8)] = &[
    ("harpsi", 6),
    ("bassoon", 70),
    ("englhorn", 69),
    ("englishhorn", 69),
    ("frhorn", 60),
    ("frenchhorn", 60),
    ("slap", 36),
    ("fretless", 35),
    ("synbass", 38),
    ("synthbass", 38),
    ("acoubass", 32),
    ("upright", 32),
    ("contrabass", 43),
    ("elecbass", 33),
    ("bass", 33),
    ("epiano", 4),
    ("elecpiano", 4),
    ("elpiano", 4),
    ("rhodes", 4),
    ("honky", 3),
    ("piano", 0),
    ("pipeorg", 19),
    ("church", 19),
    ("organ", 16),
    ("accord", 21),
    ("harmonica", 22),
    ("clav", 7),
    ("celest", 8),
    ("glock", 9),
    ("musicbox", 10),
    ("vibe", 11),
    ("vibra", 11),
    ("marimba", 12),
    ("xylo", 13),
    ("tubebell", 14),
    ("bell", 14),
    ("dulcimer", 15),
    ("elecgtr", 27),
    ("eguitar", 27),
    ("distort", 30),
    ("guitar", 24),
    ("gtr", 24),
    ("pizz", 45),
    ("violin", 40),
    ("fiddle", 110),
    ("viola", 41),
    ("cello", 42),
    ("string", 48),
    ("harp", 46),
    ("timp", 47),
    ("choir", 52),
    ("voice", 52),
    ("vox", 85),
    ("orch", 55),
    ("trumpet", 56),
    ("trombone", 57),
    ("tuba", 58),
    ("horn", 60),
    ("synbrass", 62),
    ("brass", 61),
    ("sax", 65),
    ("oboe", 68),
    ("clarinet", 71),
    ("piccolo", 72),
    ("flute", 73),
    ("recorder", 74),
    ("panpipe", 75),
    ("panflute", 75),
    ("shakuhachi", 77),
    ("whistle", 78),
    ("ocarina", 79),
    ("sitar", 104),
    ("banjo", 105),
    ("shamisen", 106),
    ("koto", 107),
    ("kalimba", 108),
    ("bagpipe", 109),
    ("taiko", 116),
    ("tom", 117),
    ("phone", 124),
    ("bird", 123),
    ("helicop", 125),
    ("applause", 126),
    ("gunshot", 127),
    ("pad", 89),
    ("lead", 81),
    ("pno", 0),
    ("org", 16),
    ("brs", 61),
    ("tpt", 56),
    ("trp", 56),
    ("tbn", 57),
    ("trb", 57),
    ("flt", 73),
    ("str", 48),
];
/// Unambiguous drum keywords, preferred even on melodic parts ("Bass Drum").
const DRUMS: &[(&str, u8)] = &[
    ("kick", 36),
    ("bassdrum", 36),
    ("bassdr", 36),
    ("snare", 38),
    ("rim", 37),
    ("clap", 39),
    ("openhh", 46),
    ("ophat", 46),
    ("openhat", 46),
    ("pedalhh", 44),
    ("hihat", 42),
    ("crash", 49),
    ("splash", 55),
    ("cymbal", 49),
    ("hitom", 50),
    ("lotom", 41),
    ("lowtom", 41),
    ("cowbell", 56),
    ("conga", 63),
    ("bongo", 60),
    ("timbal", 65),
    ("agogo", 67),
    ("tamb", 54),
    ("clave", 75),
    ("marac", 70),
    ("shaker", 70),
    ("triang", 81),
];
/// Drum keywords that also occur inside ordinary words; rhythm part only.
const DRUMS_WEAK: &[(&str, u8)] = &[
    ("rim", 37),
    ("hat", 42),
    ("ride", 51),
    ("cym", 49),
    ("tom", 45),
];
/// Short drum abbreviations, matched only as a whole word optionally followed
/// by digits ("BD", "SD-2", "HH1").
const DRUM_WORDS: &[(&str, u8)] = &[("bd", 36), ("sd", 38), ("hh", 42), ("oh", 46)];

/// GM target and semitone transposition for a timbre on a melodic part.
pub fn melodic(timbre: &[u8]) -> (Target, i8) {
    let timbre = &timbre[..TIMBRE_SIZE];
    let Some(main) = first_partial(timbre) else {
        return (Target::Silent, 0);
    };
    let (name, len) = normalize(timbre);
    if let Some(note) = find(&name[..len], DRUMS) {
        return (Target::Drum(note), 0);
    }
    if let Some(program) = find(&name[..len], MELODIC) {
        return (Target::Program(program), transpose(main));
    }
    if let Some(note) = drum_word(timbre) {
        return (Target::Drum(note), 0);
    }
    (Target::Program(family(timbre, main)), transpose(main))
}
/// GM drum note for a guest timbre on the rhythm part, recognized by name only.
pub fn drum(timbre: &[u8]) -> Option<u8> {
    first_partial(timbre)?;
    let (name, len) = normalize(timbre);
    find(&name[..len], DRUMS)
        .or_else(|| find(&name[..len], DRUMS_WEAK))
        .or_else(|| drum_word(timbre))
}
fn find(name: &[u8], table: &[(&str, u8)]) -> Option<u8> {
    let found = table.iter().find(|(keyword, _)| {
        name.windows(keyword.len())
            .any(|window| window == keyword.as_bytes())
    });
    found.map(|(_, value)| *value)
}
fn drum_word(timbre: &[u8]) -> Option<u8> {
    let raw = &timbre[..NAME];
    raw.split(|b| !b.is_ascii_alphanumeric())
        .find_map(|word| {
            let letters = word.iter().take_while(|b| b.is_ascii_alphabetic()).count();
            let digits_only = word[letters..].iter().all(u8::is_ascii_digit);
            DRUM_WORDS
                .iter()
                .find(|(k, _)| digits_only && word[..letters].eq_ignore_ascii_case(k.as_bytes()))
        })
        .map(|(_, note)| *note)
}
fn first_partial(timbre: &[u8]) -> Option<&[u8]> {
    let active = timbre[PARTIAL_MUTE] & 15;
    let index = (0..4).find(|i| active & (1 << i) != 0)?;
    let start = PARTIAL + index * PARTIAL_SIZE;
    Some(&timbre[start..start + PARTIAL_SIZE])
}
fn transpose(partial: &[u8]) -> i8 {
    if partial[PITCH_KEYFOLLOW] != KEYFOLLOW_UNITY {
        return 0;
    }
    partial[PITCH_COARSE].min(96) as i8 - COARSE_UNITY
}
/// Broad family from the first active partial when the name says nothing.
fn family(timbre: &[u8], partial: &[u8]) -> u8 {
    let pcm = uses_pcm(timbre);
    let decaying = timbre[NO_SUSTAIN] != 0 || partial[TVA_SUSTAIN] < 30;
    let slow = partial[TVA_ATTACK] > 45;
    let saw = partial[WAVEFORM] & 1 != 0;
    if partial[PITCH_COARSE] <= COARSE_UNITY as u8 - 12 {
        return if pcm { 33 } else { 38 };
    }
    match (slow, decaying, pcm, saw) {
        (true, _, _, true) => 48,
        (true, _, _, false) => 89,
        (false, true, true, _) => 0,
        (false, true, false, _) => 4,
        (false, false, true, _) => 48,
        (false, false, false, true) => 62,
        (false, false, false, false) => 80,
    }
}
/// True when an active partial is PCM according to its pair structure.
fn uses_pcm(timbre: &[u8]) -> bool {
    const FIRST: u16 = 1 << 2 | 1 << 3 | 1 << 5 | 1 << 6 | 1 << 8 | 1 << 10 | 1 << 12;
    const SECOND: u16 = 1 << 4 | 1 << 5 | 1 << 6 | 1 << 8 | 1 << 11 | 1 << 12;
    let active = timbre[PARTIAL_MUTE];
    (0..2).any(|pair| {
        let structure = 1u16 << timbre[STRUCTURE_12 + pair].min(12);
        (active >> (pair * 2) & 1 != 0 && structure & FIRST != 0)
            || (active >> (pair * 2 + 1) & 1 != 0 && structure & SECOND != 0)
    })
}
/// Lower-case alphanumeric name without separators ("Str.Sect 1" -> "strsect1").
fn normalize(timbre: &[u8]) -> ([u8; NAME], usize) {
    let mut name = [0; NAME];
    let mut len = 0;
    for &byte in timbre[..NAME].iter().filter(|b| b.is_ascii_alphanumeric()) {
        name[len] = byte.to_ascii_lowercase();
        len += 1;
    }
    (name, len)
}
