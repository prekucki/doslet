//! Fixed-step local-game A/B runner. Files stay in the RAM write overlay.
#[path = "../../common/native_input.rs"]
mod native_input;

use doslet_runtime::SessionStatus;
use doslet_x86::Gpr32;
use std::{error::Error, path::Path, time::Instant};

fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let path = args
        .first()
        .ok_or("usage: bench_interpreter_game EXE STEPS [ISO] [reference]")?;
    let steps: u64 = args.get(1).ok_or("missing STEPS")?.parse()?;
    let settings = native_input::LaunchSettings {
        hardware: doslet_config::HardwareConfig {
            cpu_mhz: 50,
            ..Default::default()
        },
        no_dynamic_core: true,
        cdrom: args
            .get(2)
            .filter(|s| s.as_str() != "-")
            .map(|p| (b'D', p.into())),
        ..Default::default()
    };
    let mut session = native_input::load_game_with_settings(Path::new(path), &settings)?;
    if args.get(3).is_some_and(|s| s == "reference") {
        session.set_decode_cache(false);
    }
    session.start();
    let mut midi = [0; 4096];
    let mut pcm = [[0; 2]; 4096];
    let mut midi_hash = 0xcbf29ce484222325_u64;
    let start = Instant::now();
    while session.steps() < steps && session.status() == SessionStatus::Running {
        session.advance((steps - session.steps()).min(10_000) as u32);
        loop {
            let count = session.drain_midi(&mut midi);
            for byte in &midi[..count] {
                midi_hash = (midi_hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
            }
            if count == 0 {
                break;
            }
        }
        while session.drain_pcm(&mut pcm) != 0 {}
    }
    let elapsed = start.elapsed();
    eprintln!(
        "mode={:?} midi={:?} console={}",
        session.video_mode(),
        session.midi_stats(),
        String::from_utf8_lossy(session.console())
    );
    assert_eq!(session.steps(), steps, "{:?}", session.fault());
    let r = session.registers();
    let registers = [
        Gpr32::Eax,
        Gpr32::Ecx,
        Gpr32::Edx,
        Gpr32::Ebx,
        Gpr32::Esp,
        Gpr32::Ebp,
        Gpr32::Esi,
        Gpr32::Edi,
    ]
    .map(|g| r.get32(g));
    let segments = [r.cs, r.ds, r.es, r.ss, r.fs, r.gs];
    println!(
        "time={elapsed:?} ns/step={:.3}",
        elapsed.as_nanos() as f64 / steps as f64
    );
    println!(
        "state steps={} ns={} regs={registers:x?} segments={segments:x?} ip={:x} flags={:x} ram_low={:016x} ram_high={:016x} vram={:016x} midi={midi_hash:016x}",
        session.steps(),
        session.emulated_nanos(),
        r.ip,
        session.eflags(),
        hash(session.ram_view(0..0xA0000).unwrap()),
        hash(
            session
                .ram_view(0xC0000..session.hardware_config().memory_bytes())
                .unwrap()
        ),
        hash(session.vram_view())
    );
    if let Some(frame) = session.frame() {
        println!(
            "frame={:016x} palette={:016x}",
            hash(frame.pixels),
            hash(frame.palette.as_flattened())
        );
    }
    Ok(())
}
