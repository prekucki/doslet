//! Bounded native comparisons from the same portable guest state. No host I/O
//! is timed; the ZIP remains read-only and each run owns its own guest buffers.
#[allow(dead_code)]
#[path = "../../doslet/src/archive.rs"]
mod archive;
#[allow(dead_code)]
#[path = "../../common/machine_profile.rs"]
mod machine_profile;
use clap::Parser;
use doslet_runtime::{Session, SessionStatus};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

#[derive(Parser)]
struct Args {
    #[arg(long, default_value = "games/Quake.toml")]
    config: PathBuf,
    #[arg(long, default_value = "target/v86/doslet-v86.wasm")]
    module: PathBuf,
    #[arg(long, default_value_t = 100_000_000)]
    steps: u64,
    #[arg(long, default_value_t = 1)]
    repeats: usize,
    #[arg(long, default_value_t = 0)]
    warmup_steps: u64,
    #[arg(long)]
    state: Option<PathBuf>,
    #[arg(long, default_value = "interpreter,v86")]
    engines: String,
    #[arg(long)]
    host_clock: bool,
    #[arg(long, requires = "host_clock")]
    until_first_park: bool,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let module = std::fs::read(&args.module)?;
    if args.steps == 0
        || args.steps > 1_000_000_000
        || args.warmup_steps > 1_000_000_000
        || !(1..=5).contains(&args.repeats)
    {
        return Err("diagnostic bounds exceeded".into());
    }
    let spec = machine_profile::parse_machine_profile(&std::fs::read_to_string(&args.config)?)?;
    let mount = spec
        .mounts
        .iter()
        .find(|m| m.letter == b'C' && m.kind == doslet_config::SourceKind::BrowserZip)
        .ok_or("ZIP C: required")?;
    let source = Path::new(&args.config)
        .parent()
        .unwrap_or(Path::new("."))
        .join(spec.binding(&mount.source));
    let archive = archive::GameArchive::new(Arc::new(std::fs::read(source)?))?;
    let mut initial = archive.load_configured(&spec.game.executable.to_uppercase(), &spec)?;
    initial.enable_dpmi(doslet_runtime::session::dpmi::Mode::Direct)?;
    initial.set_dynamic_core(false);
    initial.set_idle_loops(false);
    initial.start();
    let state = if let Some(path) = &args.state {
        std::fs::read(path)?
    } else {
        initial.save_state()?
    };
    for engine in args.engines.split(',') {
        for repeat in 0..args.repeats {
            let mut session = Session::load_state_with_media(&state, &initial)?;
            session.set_idle_loops(false);
            if engine == "v86" {
                session.enable_v86(&module)?;
                session.set_v86_profiling(true);
                if args.host_clock {
                    session.set_v86_host_clock(true)?;
                }
            } else if engine != "interpreter" {
                return Err("unknown engine".into());
            }
            session.start();
            let mut audio = Sha256::new();
            let mut pcm = [doslet_pcm::EMPTY; 4096];
            let mut midi = [0; 4096];
            let warm_start = session.steps();
            while session.steps() - warm_start < args.warmup_steps
                && session.status() == SessionStatus::Running
            {
                session
                    .advance((args.warmup_steps - (session.steps() - warm_start)).min(4096) as u32);
                while session.drain_pcm_timed(&mut pcm) != 0 {}
                while session.drain_midi(&mut midi) != 0 {}
            }
            session.wait_for_v86_compilation()?;
            let start_steps = session.steps();
            let started = Instant::now();
            let initial_nanos = session.emulated_nanos();
            while session.steps() - start_steps < args.steps
                && session.status() == SessionStatus::Running
            {
                let budget = (args.steps - (session.steps() - start_steps)).min(4096) as u32;
                let completed = session.advance(budget);
                if args.host_clock {
                    session.advance_v86_host_time(
                        initial_nanos.saturating_add(started.elapsed().as_nanos() as u64),
                    )?;
                    if args.until_first_park
                        && session.v86_ram_waits().is_some_and(|(_, parks)| parks > 0)
                    {
                        break;
                    }
                    if completed == 0 {
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }
                }
                loop {
                    let n = session.drain_pcm_timed(&mut pcm);
                    if n == 0 {
                        break;
                    }
                    for f in &pcm[..n] {
                        audio.update(f.nanos.to_le_bytes());
                        audio.update(f.duration.to_le_bytes());
                        for sample in f.samples {
                            audio.update(sample.to_le_bytes());
                        }
                    }
                }
                loop {
                    let n = session.drain_midi(&mut midi);
                    if n == 0 {
                        break;
                    }
                    audio.update(&midi[..n]);
                }
            }
            let seconds = started.elapsed().as_secs_f64();
            let settling = Instant::now();
            session.wait_for_v86_compilation()?;
            let settle_seconds = settling.elapsed().as_secs_f64();
            let mut ram = Sha256::new();
            let memory_size = usize::from(session.hardware_config().memory_mib) * 1024 * 1024;
            for offset in (0..memory_size).step_by(4096) {
                if let Some(bytes) = session.ram_view(offset..offset + 4096) {
                    ram.update(bytes);
                }
            }
            let frame = session
                .frame()
                .map(|frame| format!("{:x}", Sha256::digest(frame.indices().collect::<Vec<_>>())));
            let vram = session
                .frame()
                .map(|frame| format!("{:x}", Sha256::digest(frame.pixels)));
            let palette = session.frame().map(|frame| {
                let mut hash = Sha256::new();
                for color in frame.palette {
                    hash.update(color);
                }
                format!("{:x}", hash.finalize())
            });
            let x87 = session
                .debug_x87()
                .map(|bytes| format!("{:x}", Sha256::digest(bytes)));
            let x87_registers = session
                .debug_x87()
                .map(|bytes| format!("{:x}", Sha256::digest(&bytes[28..])));
            let x87_environment = session
                .debug_x87()
                .map(|bytes| format!("{:02x?}", &bytes[..28]));
            println!(
                "{}",
                serde_json::json!({"engine":engine,"repeat":repeat,"warmup_steps":args.warmup_steps,"seconds":seconds,"settle_seconds":settle_seconds,"steps":session.steps()-start_steps,"guest_ns":session.emulated_nanos(),"pc":session.debug_pc(),"registers":format!("{:?}",session.registers()),"flags":session.eflags(),"ram":format!("{:x}",ram.finalize()),"frame":frame,"vram":vram,"palette":palette,"x87":x87,"x87_registers":x87_registers,"x87_environment":x87_environment,"irq_entries":session.debug_irq_entries(),"ram_wait_parks":session.v86_ram_waits(),"audio":format!("{:x}",audio.finalize()),"status":format!("{:?}",session.status()),"stats":format!("{:?}",session.v86_statistics()),"fault":format!("{:?}; {:?}",session.fault(),session.v86_last_error())})
            );
            if session.status() != SessionStatus::Running {
                return Err("guest stopped".into());
            }
        }
    }
    Ok(())
}
