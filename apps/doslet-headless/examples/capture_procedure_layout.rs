//! Capture live DOS-flat LE bindings and a matching portable starting state.
//! Guest media is read-only. Preferred image addresses never select bindings.
#[allow(dead_code)]
#[path = "../../common/procedure_input.rs"]
mod procedure_input;

#[allow(dead_code)]
#[path = "../../common/procedure_cache.rs"]
mod procedure_cache;

use doslet_aot::{ImageLimits, SymbolicImage};
use doslet_executable::{LeImage, LeLimits};
use doslet_runtime::SessionStatus;
use doslet_x86::SegmentRegister;
use procedure_cache::{anchors, discover};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{error::Error, fs, path::Path, time::Instant};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn main() -> Result<()> {
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    // DOS4GW places code differently with a DPMI host. Capture the layout the
    // desktop uses by default; `--dpmi off` reproduces captures made without one.
    let dpmi = match args.iter().position(|arg| arg == "--dpmi") {
        Some(index) if index + 1 < args.len() => {
            let mode = args.remove(index + 1);
            args.remove(index);
            match mode.to_str() {
                Some("off") => None,
                Some("reflect") => Some(doslet_runtime::session::dpmi::Mode::Reflect),
                Some("direct") => Some(doslet_runtime::session::dpmi::Mode::Direct),
                _ => return Err("--dpmi must be off, reflect or direct".into()),
            }
        }
        Some(_) => return Err("--dpmi requires off, reflect or direct".into()),
        None => Some(doslet_runtime::session::dpmi::Mode::Direct),
    };
    let dpmi_name = match dpmi {
        None => "off",
        Some(doslet_runtime::session::dpmi::Mode::Reflect) => "reflect",
        Some(doslet_runtime::session::dpmi::Mode::Direct) => "direct",
    };
    if !(2..=3).contains(&args.len()) {
        return Err(
            "usage: capture_procedure_layout CONFIG.toml OUTPUT_DIR [MAX_STEPS] [--dpmi off|reflect|direct]"
                .into(),
        );
    }
    let limit: u64 = args
        .get(2)
        .map(|v| -> Result<u64> {
            v.to_str()
                .ok_or("invalid steps")?
                .parse::<u64>()
                .map_err(Into::into)
        })
        .transpose()?
        .unwrap_or(20_000_000);
    if limit == 0 || limit > 100_000_000 {
        return Err("max steps must be 1..100000000".into());
    }
    let input = procedure_input::load(Path::new(&args[0]))?;
    let le = LeImage::discover(&input.exe, LeLimits::default())?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    for object in image.objects() {
        eprintln!(
            "object{}: {:?}, {} bytes, executable={}, preferred={:#x}",
            object.object_number(),
            object.default_bitness(),
            object.bytes().len(),
            object.metadata().is_executable(),
            object.metadata().preferred_base,
        );
    }
    let anchors = anchors(&image);
    let mut session = input.media;
    if let Some(mode) = dpmi {
        session.enable_dpmi(mode)?;
    }
    session.set_dynamic_core(false);
    session.set_decode_cache(true);
    session.set_idle_loops(false);
    session.set_vga_wait_acceleration(false);
    session.set_adaptive_cpu(false);
    session.set_adaptive_idle(false);
    session.start();
    let start = Instant::now();
    let mut next_probe = 0;
    let mut last_reason = String::new();
    let mut saved_diagnostic = false;
    let mut pcm = [doslet_pcm::EMPTY; 4096];
    let mut midi = [0; 4096];
    while session.steps() < limit && start.elapsed().as_secs() < 180 {
        session.advance(4096.min((limit - session.steps()) as u32));
        while session.drain_pcm_timed(&mut pcm) != 0 {}
        while session.drain_midi(&mut midi) != 0 {}
        if session.status() != SessionStatus::Running {
            return Err(format!("guest {:?}: {:?}", session.status(), session.fault()).into());
        }
        let cs = session.registers().segment_cache(SegmentRegister::Cs);
        if session.steps() < next_probe
            || !cs.default32
            || cs.base != 0
            || cs.limit != u32::MAX
            || session.control_registers().cr0 & 0x8000_0001 != 1
        {
            continue;
        }
        next_probe = session.steps() + 100_000;
        match discover(&session, &image, &anchors) {
            Ok(bindings) => {
                let directory = Path::new(&args[1]);
                fs::create_dir_all(directory)?;
                let rows: Vec<_> = bindings
                    .iter()
                    .map(|binding| {
                        let mut row = json!({
                            "object_number": binding.object_number,
                            "linear_base": binding.linear_base,
                            "selector": binding.normal.map(|selector| selector.value),
                        });
                        if let Some(alias) = binding.alias {
                            row["alias_selector"] = json!(alias.value);
                        }
                        row
                    })
                    .collect();
                fs::write(
                    directory.join("bindings.json"),
                    serde_json::to_vec_pretty(&json!({"objects":rows,"dpmi":dpmi_name}))?,
                )?;
                // Capture ran with raw polling for reproducible layout probes.
                // The exported native test starts with normal bounded VGA waits.
                session.set_vga_wait_acceleration(true);
                // Module generation needs only the bindings; states cannot yet
                // capture the DPMI host.
                match session.save_state() {
                    Ok(state) => fs::write(directory.join("start.state"), state)?,
                    Err(error) => eprintln!("No start.state under DPMI {dpmi_name}: {error:?}"),
                }
                let frame = session.presentation_frame();
                let mut rgba = vec![0; frame.width() * frame.height() * 4];
                frame.write_rgba(&mut rgba).map_err(|e| format!("{e:?}"))?;
                let mut png = png::Encoder::new(
                    fs::File::create(directory.join("start.png"))?,
                    frame.width() as u32,
                    frame.height() as u32,
                );
                png.set_color(png::ColorType::Rgba);
                png.set_depth(png::BitDepth::Eight);
                png.write_header()?.write_image_data(&rgba)?;
                let code_fixups = image
                    .relocations()
                    .filter(|r| {
                        image
                            .object(r.source.object_number)
                            .is_some_and(|object| object.metadata().is_executable())
                    })
                    .count();
                fs::write(
                    directory.join("layout-evidence.json"),
                    serde_json::to_vec_pretty(&json!({
                        "exe_sha256": format!("{:x}", Sha256::digest(&input.exe)),
                        "steps": session.steps(),
                        "pc": session.debug_pc(),
                        "vga_wait_acceleration": true,
                        "dpmi": dpmi_name,
                        "anchors": anchors.len(),
                        "code_fixups_verified": code_fixups,
                        "bindings": rows,
                    }))?,
                )?;
                println!(
                    "Captured {} live object bindings (DPMI {dpmi_name}) at step{} PC{:#x} in {}",
                    bindings.len(),
                    session.steps(),
                    session.debug_pc(),
                    directory.display()
                );
                return Ok(());
            }
            Err(error) => {
                if !saved_diagnostic {
                    let directory = Path::new(&args[1]);
                    fs::create_dir_all(directory)?;
                    if let Ok(state) = session.save_state() {
                        fs::write(directory.join("diagnostic.state"), state)?;
                    }
                    saved_diagnostic = true;
                }
                let reason = error.to_string();
                if reason != last_reason {
                    eprintln!(
                        "step{} PC{:#x}: {reason}",
                        session.steps(),
                        session.debug_pc()
                    );
                    last_reason = reason;
                }
            }
        }
    }
    Err(format!(
        "live bindings not established after {} steps: {last_reason}",
        session.steps()
    )
    .into())
}
