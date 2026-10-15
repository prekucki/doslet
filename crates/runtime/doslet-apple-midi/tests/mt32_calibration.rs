//! Opt-in comparison of the MT-32-to-GM translation on the Apple bank with
//! local CM-32L ROM rendering. Prints measurements; ROM bytes are only read.
#![cfg(target_os = "macos")]

use doslet_apple_midi::Timeline as Apple;
use doslet_midi_routing::{Mode, Routing};
use doslet_mt32::Timeline as Cm32l;
use doslet_mt32_gm::tables::PRESETS;

const RATE: u32 = 48000;

trait Synth {
    fn send(&mut self, message: &[u8]);
    fn render(&mut self, nanos: u64, output: &mut Vec<[f32; 2]>);
}

struct Reference(Cm32l, u64);
impl Synth for Reference {
    fn send(&mut self, message: &[u8]) {
        for &byte in message {
            self.0.push_byte(byte).unwrap();
        }
    }
    fn render(&mut self, nanos: u64, output: &mut Vec<[f32; 2]>) {
        let mut chunk = [[0.0; 2]; 256];
        self.1 += nanos;
        loop {
            let count = self.0.render_until(self.1, &mut chunk).unwrap();
            if count == 0 {
                return;
            }
            output.extend_from_slice(&chunk[..count]);
        }
    }
}

trait Gm {
    fn push(&mut self, byte: u8);
    fn render(&mut self, nanos: u64, output: &mut [[f32; 2]]) -> usize;
}
impl Gm for Apple {
    fn push(&mut self, byte: u8) {
        self.push_byte(byte).unwrap();
    }
    fn render(&mut self, nanos: u64, output: &mut [[f32; 2]]) -> usize {
        self.render_until(nanos, output).unwrap()
    }
}
impl Gm for doslet_midi_synth::Timeline {
    fn push(&mut self, byte: u8) {
        self.push_byte(byte).unwrap();
    }
    fn render(&mut self, nanos: u64, output: &mut [[f32; 2]]) -> usize {
        self.render_until(nanos, output).unwrap()
    }
}

struct Translated<S>(Routing, S, u64);
impl<S: Gm> Synth for Translated<S> {
    fn send(&mut self, message: &[u8]) {
        let synth = &mut self.1;
        for &byte in message {
            self.0
                .push(byte, Mode::Gm, &mut |gm| {
                    for &byte in gm {
                        synth.push(byte);
                    }
                })
                .unwrap();
        }
    }
    fn render(&mut self, nanos: u64, output: &mut Vec<[f32; 2]>) {
        let mut chunk = [[0.0; 2]; 512];
        self.2 += nanos;
        loop {
            let count = self.1.render(self.2, &mut chunk);
            if count == 0 {
                return;
            }
            output.extend_from_slice(&chunk[..count]);
        }
    }
}

fn roms() -> (Vec<u8>, Vec<u8>) {
    let path = std::path::PathBuf::from(
        std::env::var_os("DOSLET_MT32_ROM_DIR").expect("set DOSLET_MT32_ROM_DIR"),
    );
    (
        std::fs::read(path.join("CM32L_CONTROL.ROM")).unwrap(),
        std::fs::read(path.join("CM32L_PCM.ROM")).unwrap(),
    )
}

/// CM-32L reference, translation on the Apple bank, translation on the Rust synth.
fn synths(control: &[u8], pcm: &[u8]) -> [Box<dyn Synth>; 3] {
    [
        Box::new(Reference(Cm32l::new(control, pcm, RATE).unwrap(), 0)),
        Box::new(Translated(
            Routing::new(Mode::Mt32),
            Apple::new(RATE).unwrap(),
            0,
        )),
        Box::new(Translated(
            Routing::new(Mode::Mt32),
            doslet_midi_synth::Timeline::new(RATE).unwrap(),
            0,
        )),
    ]
}

fn dt1(address: [u8; 3], data: &[u8]) -> Vec<u8> {
    let mut message = vec![0xF0, 0x41, 0x10, 0x16, 0x12];
    message.extend_from_slice(&address);
    message.extend_from_slice(data);
    let sum = message[5..].iter().fold(0u8, |sum, b| sum.wrapping_add(*b));
    message.extend_from_slice(&[(128 - (sum & 127)) & 127, 0xF7]);
    message
}

/// Mean square after ITU-R BS.1770 K-weighting (48 kHz coefficients).
fn loudness(samples: &[[f32; 2]]) -> f64 {
    let stages = [
        (
            [
                1.535_124_859_586_97,
                -2.691_696_189_406_38,
                1.198_392_810_852_85,
            ],
            [-1.690_659_293_182_41, 0.732_480_774_215_85],
        ),
        (
            [1.0, -2.0, 1.0],
            [-1.990_047_454_833_98, 0.990_072_250_366_21],
        ),
    ];
    let mut total = 0.0;
    for channel in 0..2 {
        let mut signal: Vec<f64> = samples.iter().map(|s| f64::from(s[channel])).collect();
        for (b, a) in stages {
            let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
            for value in &mut signal {
                let x = *value;
                let y = b[0] * x + b[1] * x1 + b[2] * x2 - a[0] * y1 - a[1] * y2;
                (x2, x1, y2, y1) = (x1, x, y1, y);
                *value = y;
            }
        }
        total += signal.iter().map(|v| v * v).sum::<f64>() / signal.len() as f64;
    }
    total
}

fn db(power: f64) -> f64 {
    10.0 * power.max(1e-12).log10()
}

/// YIN fundamental estimate in Hz, or None for unpitched sound.
fn pitch(samples: &[[f32; 2]]) -> Option<f64> {
    let mono: Vec<f64> = samples
        .iter()
        .map(|s| f64::from(s[0] + s[1]) * 0.5)
        .collect();
    let window = 2048;
    let (min_lag, max_lag) = (RATE as usize / 2000, RATE as usize / 30);
    if mono.len() < window + max_lag {
        return None;
    }
    let difference: Vec<f64> = (0..=max_lag)
        .map(|lag| (0..window).map(|i| (mono[i] - mono[i + lag]).powi(2)).sum())
        .collect();
    let mut running = 0.0;
    let normalized: Vec<f64> = difference
        .iter()
        .enumerate()
        .map(|(lag, d)| {
            if lag == 0 {
                return 1.0;
            }
            running += d;
            d * lag as f64 / running.max(1e-12)
        })
        .collect();
    let mut lag = min_lag;
    while lag < max_lag {
        if normalized[lag] < 0.15 {
            while lag + 1 < max_lag && normalized[lag + 1] < normalized[lag] {
                lag += 1;
            }
            return Some(f64::from(RATE) / lag as f64);
        }
        lag += 1;
    }
    None
}

const NOTE: u64 = 600_000_000;
const TAIL: u64 = 1_000_000_000;

/// Sustained and post-release audio of one note on part 1 (channel 2).
fn play(synth: &mut dyn Synth, key: u8) -> (Vec<[f32; 2]>, Vec<[f32; 2]>) {
    let mut held = Vec::new();
    let mut tail = Vec::new();
    synth.send(&[0x91, key, 100]);
    synth.render(NOTE, &mut held);
    synth.send(&[0x81, key, 0]);
    synth.render(TAIL, &mut tail);
    (held, tail)
}

fn setup(synth: &mut dyn Synth, program: u8, reverb: [u8; 3]) {
    let mut sink = Vec::new();
    synth.send(&dt1([0x10, 0, 1], &reverb));
    synth.send(&[0xC1, program, 0xB1, 7, 100, 0xB1, 10, 64]);
    synth.render(200_000_000, &mut sink);
}

const SKIP: usize = RATE as usize / 20;

#[test]
#[ignore = "requires DOSLET_MT32_ROM_DIR with local CM-32L ROMs; prints a report"]
fn local_rom_preset_loudness_and_octave() {
    let (control, pcm) = roms();
    println!(
        "program name         gm  dB ref-apple ref-rust | semitones vs key ref/apple/rust @48 @60 @72"
    );
    let mut offsets = [Vec::new(), Vec::new()];
    for (program, (name, gm)) in PRESETS.iter().enumerate() {
        let mut level = [0.0; 3];
        let mut pitches = [[None; 3]; 3];
        for (index, synth) in synths(&control, &pcm).iter_mut().enumerate() {
            setup(synth.as_mut(), program as u8, [0, 0, 0]);
            for (k, key) in [48u8, 60, 72].into_iter().enumerate() {
                let (held, _) = play(synth.as_mut(), key);
                level[index] += loudness(&held[SKIP..]);
                let nominal = 440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0);
                pitches[index][k] =
                    pitch(&held[SKIP * 2..]).map(|f| (12.0 * (f / nominal).log2()).round() as i32);
            }
        }
        let apple = db(level[0]) - db(level[1]);
        let rust = db(level[0]) - db(level[2]);
        offsets[0].push(apple);
        offsets[1].push(rust);
        let semitones = (0..3)
            .map(|k| {
                (0..3)
                    .map(|i| pitches[i][k].map_or("-".into(), |s| format!("{s:+}")))
                    .collect::<Vec<_>>()
                    .join("/")
            })
            .collect::<Vec<_>>()
            .join("  ");
        println!("{program:3} {name:<14} {gm:3} {apple:+8.1} {rust:+8.1} | {semitones}");
    }
    for (label, values) in ["apple", "rust"].iter().zip(&mut offsets) {
        values.sort_by(f64::total_cmp);
        let median = values[64];
        let spread = (values.iter().map(|v| (v - median).powi(2)).sum::<f64>() / 128.0).sqrt();
        println!("{label}: median {median:+.1} dB, rms deviation {spread:.1} dB");
    }
}

#[test]
#[ignore = "requires DOSLET_MT32_ROM_DIR with local CM-32L ROMs; prints a report"]
fn local_rom_part_level_curve() {
    let (control, pcm) = roms();
    for program in [0u8, 48, 66] {
        let mut reference = [0.0; 3];
        for level in [127u8, 100, 80, 60, 40, 20] {
            let mut measured = [0.0; 3];
            for (index, synth) in synths(&control, &pcm).iter_mut().enumerate() {
                setup(synth.as_mut(), program, [0, 0, 0]);
                synth.send(&[0xB1, 7, level]);
                let (held, _) = play(synth.as_mut(), 60);
                measured[index] = db(loudness(&held[SKIP..]));
            }
            if level == 127 {
                reference = measured;
            }
            println!(
                "program {program:3} CC7 {level:3}: ref {:+6.1} apple {:+6.1} rust {:+6.1} dB",
                measured[0] - reference[0],
                measured[1] - reference[1],
                measured[2] - reference[2]
            );
        }
    }
}

#[test]
#[ignore = "requires DOSLET_MT32_ROM_DIR with local CM-32L ROMs; prints a report"]
fn local_rom_reverb_tail() {
    let (control, pcm) = roms();
    // Tail energy relative to the sustained note: dry release is included in
    // both, so compare against level 0 of the same synth.
    for program in [0u8, 48, 52] {
        for mode in 0..4u8 {
            for time in [0u8, 5, 7] {
                let mut line = format!("program {program:3} mode {mode} time {time}:");
                for level in 0..8u8 {
                    let mut ratio = [0.0; 3];
                    for (index, synth) in synths(&control, &pcm).iter_mut().enumerate() {
                        setup(synth.as_mut(), program, [mode, time, level]);
                        let (held, tail) = play(synth.as_mut(), 60);
                        let start = RATE as usize * 3 / 20;
                        ratio[index] = db(loudness(&tail[start..])) - db(loudness(&held[SKIP..]));
                    }
                    line += &format!(" {level}:{:+.0}/{:+.0}", ratio[0], ratio[1]);
                    let _ = ratio[2];
                }
                println!("{line}");
            }
        }
    }
}

#[test]
#[ignore = "requires DOSLET_MT32_ROM_DIR with local CM-32L ROMs; prints CSV"]
fn local_rom_reverb_csv() {
    let (control, pcm) = roms();
    let ratio = |synth: &mut dyn Synth| {
        let (held, tail) = play(synth, 60);
        db(loudness(&tail[RATE as usize * 3 / 20..])) - db(loudness(&held[SKIP..]))
    };
    for program in [0u8, 48, 52, 61, 73] {
        for mode in 0..4u8 {
            for time in 0..8u8 {
                for level in 0..8u8 {
                    let mut synth = Reference(Cm32l::new(&control, &pcm, RATE).unwrap(), 0);
                    setup(&mut synth, program, [mode, time, level]);
                    println!(
                        "ref,{program},{mode},{time},{level},{:.2}",
                        ratio(&mut synth)
                    );
                }
            }
        }
        for send in (0..=127u8).step_by(8).chain([127]) {
            let mut synth = Translated(Routing::new(Mode::Mt32), Apple::new(RATE).unwrap(), 0);
            setup(&mut synth, program, [0, 0, 0]);
            // Part 1 receives on MIDI channel 2 and plays on GM channel 1.
            for byte in [0xB0, 91, send] {
                synth.1.push(byte);
            }
            synth.render(100_000_000, &mut Vec::new());
            println!("apple,{program},{send},{:.2}", ratio(&mut synth));
        }
    }
}

#[test]
#[ignore = "requires DOSLET_MT32_ROM_DIR with local CM-32L ROMs; prints a report"]
fn local_rom_rhythm_loudness() {
    let (control, pcm) = roms();
    let mut total = [0.0; 3];
    for key in 35..=76u8 {
        let mut level = [0.0; 3];
        for (index, synth) in synths(&control, &pcm).iter_mut().enumerate() {
            let mut sink = Vec::new();
            synth.send(&dt1([0x10, 0, 1], &[0, 0, 0]));
            synth.render(200_000_000, &mut sink);
            let mut held = Vec::new();
            synth.send(&[0x99, key, 100]);
            synth.render(400_000_000, &mut held);
            synth.send(&[0x89, key, 0]);
            level[index] = loudness(&held);
            total[index] += level[index];
        }
        println!(
            "key {key}: ref-apple {:+.1} ref-rust {:+.1}",
            db(level[0]) - db(level[1]),
            db(level[0]) - db(level[2])
        );
    }
    println!(
        "kit: ref-apple {:+.1} ref-rust {:+.1}",
        db(total[0]) - db(total[1]),
        db(total[0]) - db(total[2])
    );
}

#[test]
#[ignore = "requires DOSLET_MT32_ROM_DIR with local CM-32L ROMs; prints a report"]
fn local_rom_master_volume_curve() {
    let (control, pcm) = roms();
    let mut full = 0.0;
    for master in [100u8, 80, 60, 40, 20, 10] {
        let mut synth = Reference(Cm32l::new(&control, &pcm, RATE).unwrap(), 0);
        setup(&mut synth, 0, [0, 0, 0]);
        synth.send(&dt1([0x10, 0, 0x16], &[master]));
        let (held, _) = play(&mut synth, 60);
        let level = db(loudness(&held[SKIP..]));
        if master == 100 {
            full = level;
        }
        println!("master {master:3}: ref {:+6.1} dB", level - full);
    }
}

/// Uploads each CM-32L ROM preset timbre as guest timbre memory 0 and plays it
/// through patch temp, as games do. Prints the GM choice, the level difference
/// and the parameters a ROM-free estimate may use (CSV after "guest,").
#[test]
#[ignore = "requires DOSLET_MT32_ROM_DIR with local CM-32L ROMs; prints CSV"]
fn local_rom_guest_timbres() {
    let (control, pcm) = roms();
    let rom = moont::cm32l::Rom::new(&control, &pcm).unwrap();
    let mut timbres = Box::new([[0u8; 256]; 256]);
    rom.fill_raw_timbres(&mut timbres);
    for program in 0..128usize {
        let timbre = &timbres[program][..246];
        let mut level = [0.0; 3];
        let mut pitches = [None; 3];
        for (index, synth) in synths(&control, &pcm).iter_mut().enumerate() {
            let mut sink = Vec::new();
            synth.send(&dt1([0x10, 0, 1], &[0, 0, 0]));
            synth.send(&dt1([0x08, 0, 0], timbre));
            // Group 2 (memory) timbre 0, key shift 0, fine 50, bend 12, poly,
            // reverb off, output level 100, centre pan.
            synth.send(&dt1([0x03, 0, 0], &[2, 0, 24, 50, 12, 0, 0, 0, 100, 7]));
            synth.render(200_000_000, &mut sink);
            for key in [48u8, 60, 72] {
                let (held, _) = play(synth.as_mut(), key);
                level[index] += loudness(&held[SKIP..]);
                if key == 60 {
                    pitches[index] = pitch(&held[SKIP * 2..])
                        .map(|f| (12.0 * (f / 261.625_565).log2()).round() as i32);
                }
            }
        }
        let target = {
            let mut routing = Routing::new(Mode::Mt32);
            for message in [
                dt1([0x08, 0, 0], timbre),
                dt1([0x03, 0, 0], &[2, 0, 24, 50, 12, 0, 0, 0, 100, 7]),
            ] {
                for byte in message {
                    routing.push(byte, Mode::Gm, &mut |_| {}).unwrap();
                }
            }
            format!("{:?}", routing.translator().part_target(0)).replace(',', ";")
        };
        let pitch = |p: Option<i32>| p.map_or("".into(), |p| p.to_string());
        let partials: Vec<String> = (0..4)
            .map(|i| {
                let p = &timbre[14 + i * 58..14 + (i + 1) * 58];
                // coarse, keyfollow, waveform, pcm wave, TVA level, velocity
                // sensitivity, bias levels, sustain level, TVF cutoff
                format!(
                    "{};{};{};{};{};{};{};{};{};{}",
                    p[0], p[2], p[4], p[5], p[41], p[42], p[44], p[46], p[57], p[23]
                )
            })
            .collect();
        println!(
            "guest,{program},{},{target},{:.2},{:.2},{:.2},{},{},{},{},{},{},{}",
            String::from_utf8_lossy(&timbre[..10]).trim(),
            db(level[0]),
            db(level[1]),
            db(level[2]),
            pitch(pitches[0]),
            pitch(pitches[1]),
            timbre[10],
            timbre[11],
            timbre[12],
            timbre[13],
            partials.join(",")
        );
    }
}

/// Absolute K-weighted level of every GM program on the Apple bank at CC7 127,
/// velocity 100, keys 48/60/72 (CSV after "gm,"). Notes play on channel 1.
#[test]
#[ignore = "macOS GM bank report"]
fn gm_program_levels() {
    struct Direct(Apple, u64);
    impl Synth for Direct {
        fn send(&mut self, message: &[u8]) {
            for &byte in message {
                self.0.push(byte);
            }
        }
        fn render(&mut self, nanos: u64, output: &mut Vec<[f32; 2]>) {
            let mut chunk = [[0.0; 2]; 512];
            self.1 += nanos;
            loop {
                let count = Gm::render(&mut self.0, self.1, &mut chunk);
                if count == 0 {
                    return;
                }
                output.extend_from_slice(&chunk[..count]);
            }
        }
    }
    for program in 0..128u8 {
        let mut synth = Direct(Apple::new(RATE).unwrap(), 0);
        synth.send(&[0xC1, program, 0xB1, 7, 127, 0xB1, 91, 0, 0xB1, 93, 0]);
        synth.render(200_000_000, &mut Vec::new());
        let mut level = 0.0;
        for key in [48u8, 60, 72] {
            let (held, _) = play(&mut synth, key);
            level += loudness(&held[SKIP..]);
        }
        println!("gm,{program},{:.2}", db(level));
    }
}
