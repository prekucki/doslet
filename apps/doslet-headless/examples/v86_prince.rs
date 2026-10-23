//! Read-only local PRINCE startup probe; guest writes remain in an overlay.
#[allow(dead_code)]
#[path = "../../common/native_input.rs"]
mod native_input;
use clap::{Parser, ValueEnum};
use doslet_runtime::SessionStatus;
use std::time::{Duration, Instant};

#[derive(Clone, Copy, ValueEnum)]
enum DpmiArg {
    Off,
    Direct,
    Reflect,
}

#[derive(Parser)]
struct Args {
    #[arg(value_enum, default_value_t = DpmiArg::Off)]
    dpmi: DpmiArg,
    #[arg(long)]
    menu: bool,
    #[arg(long)]
    long: bool,
    #[arg(long)]
    trace: bool,
    #[arg(long)]
    trace_ss: bool,
    #[arg(long)]
    frame: Option<std::path::PathBuf>,
    #[arg(long)]
    duration: Option<u64>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let menu = args.menu;
    let trace = args.trace;
    let trace_ss = args.trace_ss;
    let duration = args
        .duration
        .unwrap_or(if menu || args.long { 36 } else { 20 })
        .clamp(1, 300);
    let dpmi = match args.dpmi {
        DpmiArg::Direct => Some(doslet_runtime::session::dpmi::Mode::Direct),
        DpmiArg::Reflect => Some(doslet_runtime::session::dpmi::Mode::Reflect),
        DpmiArg::Off => None,
    };
    let mut session = native_input::load_game_with_settings(
        std::path::Path::new("games/PRINCE/PRINCE.EXE"),
        &native_input::LaunchSettings {
            hardware: doslet_config::HardwareConfig {
                keyboard: doslet_config::KeyboardMode::At,
                ..Default::default()
            },
            v86_module: Some("target/v86/doslet-v86.wasm".into()),
            v86_host_clock: true,
            dpmi,
            ..Default::default()
        },
    )?;
    session.start();
    let started = Instant::now();
    let mut pcm = [doslet_pcm::EMPTY; 4096];
    let mut midi = [0; 4096];
    let clicks = [(15_000, 320, 240), (18_000, 320, 150), (31_000, 320, 455)];
    let mut click = 0;
    let mut pressed = false;
    let mut trace_left = 400;
    while session.status() == SessionStatus::Running
        && started.elapsed() < Duration::from_secs(duration)
    {
        if menu && click < clicks.len() {
            let (at, x, y) = clicks[click];
            if !pressed && started.elapsed() >= Duration::from_millis(at) {
                eprintln!(
                    "PRINCE click {click} at {x},{y}; pc={:x}",
                    session.debug_pc()
                );
                session.pointer_move(x, y);
                session.pointer_button(0, true);
                pressed = true;
            } else if pressed && started.elapsed() >= Duration::from_millis(at + 100) {
                session.pointer_button(0, false);
                pressed = false;
                click += 1;
            }
        }
        session.advance_v86_host_time(started.elapsed().as_nanos() as u64)?;
        let tracing = trace
            && menu
            && (pressed || click > 0)
            && session.registers().cs == 0x15f
            && (0x600..0x820).contains(&session.registers().ip);
        if tracing && trace_left > 0 {
            trace_left -= 1;
            let cpu = session.debug_cpu().context();
            let sp = cpu.caches[2].base.wrapping_add(cpu.general[4]);
            eprintln!(
                "PRINCE trace ip={:x} flags={:x} gpr={:x?} ds={:x} es={:x} ss={:x} stack={:02x?}",
                cpu.ip,
                cpu.flags,
                cpu.general,
                cpu.caches[3].selector,
                cpu.caches[0].selector,
                cpu.caches[2].selector,
                session.ram_view(sp as usize..sp as usize + 48)
            );
        }
        let before = trace_ss.then(|| session.debug_cpu().context());
        session.advance(if menu && (pressed || click > 0) {
            1
        } else {
            4096
        });
        if let Some(before) = before {
            let after = session.debug_cpu().context();
            if before.caches[2].selector != after.caches[2].selector
                && (before.caches[2].selector == 0x127 || after.caches[2].selector == 0x127)
            {
                eprintln!(
                    "SS transition {:04x}:{:08x} ss={:04x} esp={:08x} -> {:04x}:{:08x} ss={:04x} esp={:08x}",
                    before.caches[1].selector,
                    before.ip,
                    before.caches[2].selector,
                    before.general[4],
                    after.caches[1].selector,
                    after.ip,
                    after.caches[2].selector,
                    after.general[4]
                );
            }
        }
        while session.drain_pcm_timed(&mut pcm) != 0 {}
        while session.drain_midi(&mut midi) != 0 {}
    }
    let context = session.debug_cpu().context();
    eprintln!(
        "status={:?} steps={} fault={:?} error={:?} regs={:?} pc={:x} flags={:x} console={}",
        session.status(),
        session.steps(),
        session.fault(),
        session.v86_last_error(),
        session.registers(),
        session.debug_pc(),
        session.eflags(),
        String::from_utf8_lossy(session.console())
    );
    if let Some(path) = &args.frame {
        let frame = session.presentation_frame();
        let (width, height) = (frame.width(), frame.height());
        let mut rgba = vec![0; width * height * 4];
        frame
            .write_rgba(&mut rgba)
            .expect("exact presentation dimensions");
        let mut encoder =
            png::Encoder::new(std::fs::File::create(path)?, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&rgba)?;
        eprintln!("frame={} {width}x{height}", path.display());
    }
    if trace {
        for (i, s) in context.caches.iter().enumerate() {
            eprintln!(
                "seg{i}: selector={:x} base={:x} limit={:x} access={:x} flags={:x} usable={} d32={}",
                s.selector, s.base, s.limit, s.access, s.flags, s.usable, s.default32
            );
        }
        for selector in [0x0127u32, 0x012b, 0x015f] {
            if let Some(desc) = session.ram_view(
                (0x101000 + (selector & !7)) as usize..(0x101008 + (selector & !7)) as usize,
            ) {
                let base = u32::from_le_bytes([desc[2], desc[3], desc[4], desc[7]]);
                eprintln!("selector {selector:04x}: desc={desc:02x?} base={base:x}");
                if selector == 0x15f {
                    eprintln!(
                        "015f:0700 handler={:02x?}",
                        session.ram_view(base as usize + 0x700..base as usize + 0x820)
                    );
                    eprintln!(
                        "015f:07f0 code={:02x?}",
                        session.ram_view(base as usize + 0x7f0..base as usize + 0x840)
                    );
                } else if selector == 0x12b {
                    eprintln!(
                        "012b:35c0 code={:02x?}",
                        session.ram_view(base as usize + 0x35c0..base as usize + 0x35f0)
                    );
                }
            }
        }
        eprintln!(
            "exception stack={:02x?}",
            session.ram_view(0x11bf80..0x11c000)
        );
    }
    if session.status() != SessionStatus::Running {
        return Err("PRINCE startup failed".into());
    }
    Ok(())
}
