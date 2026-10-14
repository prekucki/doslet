//! Read-only SMF inventory and executable string evidence; never plays/copies assets.
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::Path,
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(len).ok_or("offset overflow")?;
        let bytes = self.data.get(self.pos..end).ok_or("truncated SMF")?;
        self.pos = end;
        Ok(bytes)
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into()?))
    }
    fn vlq(&mut self) -> Result<u32> {
        let mut value = 0;
        for _ in 0..4 {
            let byte = self.byte()?;
            value = (value << 7) | u32::from(byte & 127);
            if byte & 128 == 0 {
                return Ok(value);
            }
        }
        Err("overlong SMF VLQ".into())
    }
}
#[derive(Clone, Copy)]
enum Kind {
    Midi(u8, u8, u8),
    Tempo(u32),
}
struct Event {
    tick: u64,
    kind: Kind,
}
struct Song {
    format: u16,
    tracks: u16,
    division: u16,
    end: u64,
    events: Vec<Event>,
    sysex: BTreeMap<String, usize>,
    names: Vec<String>,
}
fn parse(data: &[u8]) -> Result<Song> {
    let mut r = Reader { data, pos: 0 };
    if r.take(4)? != b"MThd" {
        return Err("missing MThd".into());
    }
    let header_len = r.u32()? as usize;
    let mut header = Reader {
        data: r.take(header_len)?,
        pos: 0,
    };
    let format = header.u16()?;
    let tracks = header.u16()?;
    let division = header.u16()?;
    if format > 1 || tracks == 0 || division == 0 || division & 0x8000 != 0 {
        return Err("only synchronous format 0/1 with PPQN is supported".into());
    }
    let mut song = Song {
        format,
        tracks,
        division,
        end: 0,
        events: Vec::new(),
        sysex: BTreeMap::new(),
        names: Vec::new(),
    };
    for _ in 0..tracks {
        if r.take(4)? != b"MTrk" {
            return Err("missing MTrk".into());
        }
        let length = r.u32()? as usize;
        let mut t = Reader {
            data: r.take(length)?,
            pos: 0,
        };
        let mut tick = 0u64;
        let mut running = 0;
        while t.pos < t.data.len() {
            tick = tick
                .checked_add(u64::from(t.vlq()?))
                .ok_or("tick overflow")?;
            let first = t.byte()?;
            let status = if first & 128 != 0 {
                first
            } else {
                if running == 0 {
                    return Err("data without running status".into());
                }
                t.pos -= 1;
                running
            };
            match status {
                0x80..=0xEF => {
                    running = status;
                    let a = t.byte()?;
                    let b = if matches!(status & 0xF0, 0xC0 | 0xD0) {
                        0
                    } else {
                        t.byte()?
                    };
                    if a >= 128 || b >= 128 {
                        return Err("invalid MIDI data byte".into());
                    }
                    song.events.push(Event {
                        tick,
                        kind: Kind::Midi(status, a, b),
                    });
                }
                0xFF => {
                    running = 0;
                    let kind = t.byte()?;
                    let length = t.vlq()? as usize;
                    let bytes = t.take(length)?;
                    if matches!(kind, 3 | 4) {
                        song.names
                            .push(String::from_utf8_lossy(bytes).chars().take(80).collect());
                    }
                    if kind == 0x51 {
                        if bytes.len() != 3 {
                            return Err("bad tempo length".into());
                        }
                        let tempo = u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]);
                        if tempo == 0 {
                            return Err("zero tempo".into());
                        }
                        song.events.push(Event {
                            tick,
                            kind: Kind::Tempo(tempo),
                        });
                    }
                    if kind == 0x2F {
                        break;
                    }
                }
                0xF0 | 0xF7 => {
                    running = 0;
                    let length = t.vlq()? as usize;
                    let bytes = t.take(length)?;
                    let name = if bytes.starts_with(&[0x7E]) && bytes.get(2) == Some(&9) {
                        "universal GM message".to_owned()
                    } else if bytes.first() == Some(&0x41) && bytes.get(2) == Some(&0x16) {
                        "Roland MT-32 model 16h".to_owned()
                    } else if bytes.first() == Some(&0x41) && bytes.get(2) == Some(&0x42) {
                        "Roland GS model 42h".to_owned()
                    } else {
                        format!(
                            "status={status:02X}, manufacturer={:02X?}, len={length}",
                            bytes.first()
                        )
                    };
                    *song.sysex.entry(name).or_default() += 1;
                }
                _ => return Err(format!("unsupported SMF status {status:02X}").into()),
            }
        }
        song.end = song.end.max(tick);
    }
    song.events.sort_by_key(|event| event.tick);
    Ok(song)
}
fn duration_seconds(song: &Song) -> f64 {
    let mut tempo = 500_000u64;
    let mut micros_numerator = 0u128;
    let mut last_tick = 0;
    for event in &song.events {
        micros_numerator += u128::from(event.tick - last_tick) * u128::from(tempo);
        last_tick = event.tick;
        if let Kind::Tempo(value) = event.kind {
            tempo = u64::from(value);
        }
    }
    micros_numerator += u128::from(song.end - last_tick) * u128::from(tempo);
    micros_numerator as f64 / f64::from(song.division) / 1_000_000.0
}
fn summarize(name: &str, song: Song) {
    let seconds = duration_seconds(&song);
    let mut programs = [0; 16];
    let mut notes: BTreeMap<(u8, u8), u64> = BTreeMap::new();
    let mut controls: BTreeSet<u8> = BTreeSet::new();
    let mut banks = BTreeSet::new();
    let mut held = [[false; 128]; 16];
    let mut keys = 0;
    let mut peak_keys = 0;
    let mut bends = 0;
    for event in song.events {
        match event.kind {
            Kind::Tempo(_) => {}
            Kind::Midi(status, a, b) => {
                let channel = usize::from(status & 15);
                match status & 0xF0 {
                    0xC0 => programs[channel] = a,
                    0xB0 => {
                        controls.insert(a);
                        if matches!(a, 0 | 32) {
                            banks.insert((channel + 1, a, b));
                        }
                    }
                    0xE0 => bends += 1,
                    0x90 if b != 0 => {
                        *notes.entry((channel as u8, programs[channel])).or_default() += 1;
                        if !held[channel][usize::from(a)] {
                            held[channel][usize::from(a)] = true;
                            keys += 1;
                            peak_keys = peak_keys.max(keys);
                        }
                    }
                    0x80 | 0x90 => {
                        if held[channel][usize::from(a)] {
                            held[channel][usize::from(a)] = false;
                            keys -= 1;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    println!(
        "{name}: format={} tracks={} PPQN={} seconds={seconds:.3} note_ons={} peak_held_keys={peak_keys} bends={bends} CC={controls:?} SysEx={:?}",
        song.format,
        song.tracks,
        song.division,
        notes.values().sum::<u64>(),
        song.sysex
    );
    println!(
        "  channel/program/note-ons (channel=1-based, program=0-based): {:?}",
        notes
            .into_iter()
            .map(|((ch, prog), n)| (ch + 1, prog, n))
            .collect::<Vec<_>>()
    );
    println!("  bank-select (channel/controller/value): {banks:?}");
    println!("  track/instrument names: {:?}", song.names);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn smf(tracks: &[&[u8]]) -> Vec<u8> {
        let mut data = b"MThd\0\0\0\x06\0\x01".to_vec();
        data.extend_from_slice(&(tracks.len() as u16).to_be_bytes());
        data.extend_from_slice(&240u16.to_be_bytes());
        for track in tracks {
            data.extend_from_slice(b"MTrk");
            data.extend_from_slice(&(track.len() as u32).to_be_bytes());
            data.extend_from_slice(track);
        }
        data
    }
    #[test]
    fn tempo_track_controls_synchronous_duration_and_running_status() {
        let data = smf(&[
            &[
                0, 0xFF, 0x51, 3, 7, 0xA1, 0x20, 120, 0xFF, 0x51, 3, 3, 0xD0, 0x90, 120, 0xFF,
                0x2F, 0,
            ],
            &[
                0, 0xC1, 73, 0, 0x91, 60, 100, 120, 61, 100, 120, 0x81, 60, 0, 0, 61, 0, 0, 0xFF,
                0x2F, 0,
            ],
        ]);
        let song = parse(&data).unwrap();
        assert_eq!(song.end, 240);
        assert_eq!(duration_seconds(&song), 0.375);
        assert_eq!(
            song.events
                .iter()
                .filter(|e| matches!(e.kind, Kind::Midi(0x91, _, 100)))
                .count(),
            2
        );
        assert!(song.events.windows(2).all(|w| w[0].tick <= w[1].tick));
    }
    #[test]
    fn recognizes_mt32_sysex_without_exposing_patch_data() {
        let song = parse(&smf(&[&[
            0, 0xF0, 5, 0x41, 0x10, 0x16, 0x12, 0xF7, 0, 0xFF, 0x2F, 0,
        ]]))
        .unwrap();
        assert_eq!(song.sysex.get("Roland MT-32 model 16h"), Some(&1));
        assert!(song.events.is_empty());
    }
    #[test]
    fn truncated_and_invalid_data_produce_explicit_errors() {
        assert!(parse(b"MThd").is_err());
        assert!(parse(&smf(&[&[0, 60, 100]])).is_err());
        assert!(parse(&smf(&[&[0x80, 0x80, 0x80, 0x80, 0, 0xFF, 0x2F, 0]])).is_err());
        assert!(parse(&smf(&[&[0, 0xFF, 0x51, 3, 0, 0, 0]])).is_err());
    }
}
fn trace(path: &str) -> Result<()> {
    let mut decoder = doslet_midi_synth::wire::Decoder::new();
    let mut programs = [0u8; 16];
    let mut notes: BTreeMap<(u8, u8), u64> = BTreeMap::new();
    let mut changes: BTreeSet<(u8, u8)> = BTreeSet::new();
    let mut sysex = 0;
    let mut banks = BTreeSet::new();
    for line in fs::read_to_string(path)?.lines() {
        let (_, hex) = line
            .split_once(' ')
            .or_else(|| line.split_once('\t'))
            .or_else(|| line.split_once("\\t"))
            .ok_or("bad trace line")?;
        if hex.len() % 2 != 0 {
            return Err("odd trace byte count".into());
        }
        for pair in hex.as_bytes().chunks_exact(2) {
            let byte = u8::from_str_radix(std::str::from_utf8(pair)?, 16)?;
            if let Some(message) = decoder.push(byte).map_err(|e| format!("wire: {e:?}"))? {
                let channel = usize::from(message[0] & 15);
                match message[0] & 0xF0 {
                    0xC0 => {
                        programs[channel] = message[1];
                        changes.insert((channel as u8 + 1, message[1]));
                    }
                    0x90 if message[2] != 0 => {
                        *notes
                            .entry((channel as u8 + 1, programs[channel]))
                            .or_default() += 1;
                    }
                    0xF0 if message[0] == 0xF0 => sysex += 1,
                    0xB0 if matches!(message[1], 0 | 32) => {
                        banks.insert((channel + 1, message[1], message[2]));
                    }
                    _ => {}
                }
            }
        }
    }
    println!(
        "Guest UART trace {path}: program changes={changes:?}; note_ons={notes:?}; bank-select={banks:?}; SysEx={sysex}"
    );
    Ok(())
}
fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let directory = args.next().unwrap_or_else(|| "PRINCE".into());
    let root = Path::new(&directory);
    let mut paths = fs::read_dir(root)?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    for path in &paths {
        if path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("mid"))
        {
            let song = parse(&fs::read(path)?).map_err(|e| format!("{}: {e}", path.display()))?;
            summarize(&path.file_name().unwrap().to_string_lossy(), song);
        }
    }
    for path in paths.iter().filter(|path| {
        path.extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("exe") || extension.eq_ignore_ascii_case("386")
        })
    }) {
        let filename = path.file_name().unwrap().to_string_lossy();
        let data = fs::read(path)?;
        println!("{filename}: printable string evidence (file offsets, not code addresses)");
        let mut start = 0;
        for end in 0..=data.len() {
            if end < data.len() && (32..127).contains(&data[end]) {
                continue;
            }
            if end - start >= 4 {
                let text = std::str::from_utf8(&data[start..end])?;
                let lower = text.to_ascii_lowercase();
                if [
                    ".mid",
                    "mpu",
                    "mt32",
                    "mt-32",
                    "general midi",
                    "roland",
                    "jvsound",
                    "midi=",
                    "jvs",
                    "watcom",
                    "rational",
                    "copyright",
                    "sound drive",
                    "sound library",
                    "human machine",
                    "miles",
                    "fastgraph",
                    "genus",
                    "graphics",
                    "library",
                    "linker",
                    "version",
                    "exception",
                    "svga",
                    "vesa",
                    "jared",
                    "voyetra",
                    "fat man",
                    "timbre",
                    "c/386",
                    "1.97",
                    "flashtek",
                    "zortech",
                    "symantec",
                    "dosx",
                    "x-32",
                ]
                .iter()
                .any(|term| lower.contains(term) && (*term != "mpu" || !lower.contains("computer")))
                {
                    println!("  {start:08X}: {text}");
                }
            }
            start = end + 1;
        }
    }
    if let Some(path) = args.next() {
        trace(&path)?;
    }
    Ok(())
}
