//! Compare a local CIV directory with the browser ZIP without changing either.
#[path = "../src/archive.rs"]
mod archive;
#[path = "../../common/native_input.rs"]
mod native_input;

use doslet_runtime::{Session, SessionStatus};
use std::{error::Error, fs, path::Path, sync::Arc};

fn drain(session: &mut Session, output: &mut Vec<u8>) {
    let mut bytes = [0; 4096];
    loop {
        let count = session.drain_midi(&mut bytes);
        if count == 0 {
            break;
        }
        output.extend_from_slice(&bytes[..count]);
    }
    let mut pcm = [[0; 2]; 256];
    while session.drain_pcm(&mut pcm) != 0 {}
}

fn main() -> Result<(), Box<dyn Error>> {
    let budget = std::env::args()
        .nth(1)
        .map_or(Ok(30_000_000_u64), |value| value.parse())?;
    let zip = archive::GameArchive::new(Arc::new(fs::read("target/civ-web/bundle/CIV.zip")?))?;
    let mut directory = native_input::load_game(Path::new("games/civ/CIV.EXE"))?;
    let mut packed = zip.load_configured("CIV.EXE", &doslet_config::MachineSpec::default())?;
    directory.set_dynamic_core(false);
    packed.set_dynamic_core(false);
    directory.start();
    packed.start();
    let mut keys = b"151".iter();
    let (mut directory_midi, mut packed_midi) = (Vec::new(), Vec::new());
    while directory.steps() < budget && directory.status() == SessionStatus::Running {
        if directory.waiting_for_key() && packed.waiting_for_key() {
            if let Some(&key) = keys.next() {
                directory.push_key(0, key)?;
                packed.push_key(0, key)?;
            }
        }
        let count = ((budget - directory.steps()).min(10_000)) as u32;
        let a = directory.advance(count);
        let b = packed.advance(count);
        drain(&mut directory, &mut directory_midi);
        drain(&mut packed, &mut packed_midi);
        if a != b
            || directory.status() != packed.status()
            || directory.registers() != packed.registers()
            || directory_midi != packed_midi
        {
            return Err(format!("directory/ZIP diverged at steps {} / {}: executed {a}/{b}, status {:?}/{:?}, MIDI bytes {}/{}", directory.steps(), packed.steps(), directory.status(), packed.status(), directory_midi.len(), packed_midi.len()).into());
        }
        if a == 0 {
            return Err("guest made no progress".into());
        }
    }
    println!(
        "matched steps={} status={:?} MIDI bytes={} directory files={:?} ZIP files={:?}",
        directory.steps(),
        directory.status(),
        directory_midi.len(),
        directory.file_access_stats(),
        packed.file_access_stats()
    );
    Ok(())
}
