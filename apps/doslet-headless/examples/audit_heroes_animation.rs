//! Read-only, version-specific HoMM intro diagnostics. Artifacts remain local.
#[path = "../src/heroes_animation_audit.rs"]
mod heroes_animation_audit;
#[path = "../../common/native_input.rs"]
mod native_input;

use doslet_runtime::{Session, SessionStatus};
use std::{error::Error, fs, io::Write, path::Path, time::Instant};

fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

fn capture(session: &Session, output: &Path) -> Result<(), Box<dyn Error>> {
    let step = session.steps();
    let frame = session.presentation_frame();
    let mut rgba = vec![0; frame.width() * frame.height() * 4];
    frame.write_rgba(&mut rgba).map_err(|e| format!("{e:?}"))?;
    let mut image = fs::File::create(output.join(format!("{step}.ppm")))?;
    write!(image, "P6\n{} {}\n255\n", frame.width(), frame.height())?;
    for pixel in rgba.as_chunks::<4>().0 {
        image.write_all(&pixel[..3])?;
    }
    let vram = session.vram_view();
    fs::write(output.join(format!("{step}-vram.bin")), vram)?;
    let descriptor = session.ram_view(0x10F15C..0x10F174).ok_or("descriptor")?;
    let width = u16::from_le_bytes(descriptor[16..18].try_into()?);
    let height = u16::from_le_bytes(descriptor[18..20].try_into()?);
    let address = u32::from_le_bytes(descriptor[20..24].try_into()?) as usize;
    let size = usize::from(width) * usize::from(height);
    let (mut source_hash, mut differences) = (0, None);
    if (width, height) == (640, 480)
        && let Some(source) = session.ram_view(address..address + size)
    {
        fs::write(output.join(format!("{step}-source.bin")), source)?;
        source_hash = hash(source);
        differences = Some(source.iter().zip(vram).filter(|(a, b)| a != b).count());
    }
    println!(
        "FRAME step={step} ns={} mode={:?} pc={:04X}:{:08X} vram={:016x} source={address:08x} {width}x{height} hash={source_hash:016x} differences={differences:?}",
        session.emulated_nanos(),
        session.video_mode(),
        session.registers().cs,
        session.registers().ip,
        hash(&vram[..307200.min(vram.len())]),
    );
    if step == 250_000_000 {
        if let Some(ram) = session.ram_view(0..0xA0000) {
            fs::write(output.join("low.bin"), ram)?;
        }
        if let Some(ram) = session.ram_view(0x100000..0x1000000) {
            fs::write(output.join("extended.bin"), ram)?;
        }
    }
    Ok(())
}

fn drain(session: &mut Session) {
    let mut midi = [0; 1024];
    let mut pcm = [doslet_pcm::EMPTY; 128];
    while session.drain_midi(&mut midi) != 0 {}
    while session.drain_pcm_timed(&mut pcm) != 0 {}
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    let executable = args
        .get(1)
        .ok_or("usage: audit_heroes_animation EXE ISO OUTPUT [reference]")?;
    let iso = args.get(2).ok_or("missing ISO")?;
    let output = Path::new(args.get(3).ok_or("missing output directory")?);
    let reference = args.get(4).is_some_and(|s| s == "reference");
    let trace = args.get(4).is_some_and(|s| s == "trace");
    fs::create_dir_all(output)?;
    let settings = native_input::LaunchSettings {
        cdrom: Some((b'D', iso.into())),
        no_dynamic_core: reference,
        ..Default::default()
    };
    let mut session = native_input::load_game_with_settings(Path::new(executable), &settings)?;
    if reference {
        session.set_decode_cache(false);
        session.set_vga_wait_acceleration(false);
        session.set_idle_loops(false);
    }
    let started = Instant::now();
    let mut audit = heroes_animation_audit::Audit::with_frames(output.join("decoded"))?;
    let mut trace_output = std::io::BufWriter::new(fs::File::create(output.join("trace.txt"))?);
    session.start();
    for checkpoint in [
        60_000_000,
        100_000_000,
        140_000_000,
        180_000_000,
        200_000_000,
        220_000_000,
        240_000_000,
        250_000_000,
        260_000_000,
        280_000_000,
        300_000_000,
    ] {
        while session.steps() < checkpoint && session.status() == SessionStatus::Running {
            let tracing = trace && (200_000_000..270_000_000).contains(&session.steps());
            if tracing {
                audit.observe(&session, &mut trace_output)?;
            }
            session.advance(if tracing {
                1
            } else {
                (checkpoint - session.steps()).min(10000) as u32
            });
            drain(&mut session);
        }
        capture(&session, output)?;
        if session.status() != SessionStatus::Running {
            return Err(
                format!("status={:?} fault={:?}", session.status(), session.fault()).into(),
            );
        }
    }
    audit.finish(&mut trace_output)?;
    println!(
        "DONE reference={reference} elapsed={:?} idle={:?}",
        started.elapsed(),
        session.idle_loop_stats()
    );
    Ok(())
}
