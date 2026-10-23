use clap::{Args, Parser, Subcommand};
use doslet_executable::MzImage;
use doslet_runtime::{Session, SessionStatus};
use doslet_x86::{Gpr16, Gpr32};
#[cfg(test)]
mod cache_tests;
use doslet_debugger as debugger;
#[path = "../../common/native_input.rs"]
mod native_input;
#[cfg(feature = "profiling")]
mod profile;
mod wave;
use std::{
    error::Error,
    fs,
    io::{BufWriter, Write},
    path::PathBuf,
    time::Instant,
};

#[derive(Parser)]
#[command(
    about = "Run Doslet without a graphical frontend",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Demo(RunOptions),
    RunCom {
        path: PathBuf,
        #[command(flatten)]
        options: RunOptions,
    },
    RunMz {
        path: PathBuf,
        #[command(flatten)]
        options: RunOptions,
    },
    RunStub {
        path: PathBuf,
        #[command(flatten)]
        options: RunOptions,
    },
    Inspect {
        #[arg(default_value = "PRINCE/PRINCE.EXE")]
        path: PathBuf,
    },
    Bench(RunOptions),
}

#[derive(Args)]
struct RunOptions {
    #[arg(long)]
    steps: Option<u64>,
    #[arg(long)]
    frame: Option<PathBuf>,
    #[arg(long)]
    midi_log: Option<PathBuf>,
    #[arg(long)]
    midi_wave: Option<PathBuf>,
    #[arg(long)]
    gdb: Option<std::net::SocketAddr>,
    #[arg(long)]
    gdb_after: Option<u64>,
    #[arg(long)]
    cdrom: Option<PathBuf>,
    #[arg(long)]
    key_at: Vec<String>,
    #[arg(long)]
    no_vga_waits: bool,
    #[arg(long)]
    no_idle_loops: bool,
    #[arg(long)]
    no_decode_cache: bool,
    #[arg(long)]
    no_dynamic_core: bool,
    #[arg(long)]
    core_profile: Option<PathBuf>,
    #[arg(long)]
    profile_cpu: bool,
    #[arg(long)]
    trace: Option<u64>,
    #[arg(long)]
    trace_tail: bool,
    #[arg(long)]
    peek: Vec<String>,
    #[arg(long)]
    find_ascii: Vec<String>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("doslet: {error}");
        std::process::exit(1);
    }
}

#[derive(Clone, Copy)]
struct StepSnapshot {
    step: u64,
    cs: u16,
    ip: u32,
    general: [u32; 8],
    flags: u32,
}
impl StepSnapshot {
    fn capture(session: &Session) -> Self {
        let r = session.registers();
        Self {
            step: session.steps(),
            cs: r.cs,
            ip: r.ip,
            flags: session.eflags(),
            general: [
                Gpr32::Eax,
                Gpr32::Ecx,
                Gpr32::Edx,
                Gpr32::Ebx,
                Gpr32::Esp,
                Gpr32::Ebp,
                Gpr32::Esi,
                Gpr32::Edi,
            ]
            .map(|reg| r.get32(reg)),
        }
    }
    fn print(self) {
        println!(
            "{:08} {:04X}:{:08X} EAX={:08X} ECX={:08X} EDX={:08X} EBX={:08X} ESP={:08X} EBP={:08X} ESI={:08X} EDI={:08X} FLAGS={:08X}",
            self.step,
            self.cs,
            self.ip,
            self.general[0],
            self.general[1],
            self.general[2],
            self.general[3],
            self.general[4],
            self.general[5],
            self.general[6],
            self.general[7],
            self.flags
        );
    }
}
/// Host-side output and scripted input shared by ordinary and GDB execution.
struct RunOutput {
    midi_log: Option<BufWriter<fs::File>>,
    wave: Option<wave::Wave>,
    drain_midi: bool,
    logged_midi: u64,
    pcm_frames: u64,
    pcm_peak: u16,
    midi_bytes: [u8; 1024],
    pcm: [[i16; 2]; 128],
    keys: std::collections::VecDeque<(u64, u8, u8)>,
}
impl RunOutput {
    fn after_step(&mut self, session: &mut Session) -> Result<(), Box<dyn Error>> {
        while self
            .keys
            .front()
            .is_some_and(|event| event.0 <= session.steps())
        {
            let (_, scan, ascii) = self.keys.pop_front().unwrap();
            session.push_key(scan, ascii)?;
        }
        if let Some(wave) = &mut self.wave {
            wave.advance(session.emulated_nanos())?;
        }
        if self.drain_midi {
            loop {
                let count = session.drain_midi(&mut self.midi_bytes);
                if count == 0 {
                    break;
                }
                if let Some(log) = &mut self.midi_log {
                    write!(log, "{}\t", session.emulated_nanos())?;
                    for byte in &self.midi_bytes[..count] {
                        write!(log, "{byte:02X}")?;
                    }
                    writeln!(log)?;
                    self.logged_midi += count as u64;
                }
                if let Some(wave) = &mut self.wave {
                    for byte in &self.midi_bytes[..count] {
                        wave.timeline.push_byte(*byte)?;
                    }
                }
            }
        }
        loop {
            let count = session.drain_pcm(&mut self.pcm);
            self.pcm_frames += count as u64;
            for frame in &self.pcm[..count] {
                for sample in frame {
                    self.pcm_peak = self.pcm_peak.max(sample.unsigned_abs());
                }
            }
            if count == 0 {
                break;
            }
        }
        Ok(())
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let cli = Cli::parse();
    if let Command::Inspect { path } = &cli.command {
        let bytes = fs::read(path)?;
        let image = MzImage::parse(&bytes)?;
        println!(
            "Executable: {}\nFile bytes: {}\nMZ load module: {} bytes\nMZ relocations: {}\nMZ entry: {:04X}:{:04X}",
            path.display(),
            bytes.len(),
            image.load_module.len(),
            image.header.relocation_count,
            image.header.initial_cs,
            image.header.initial_ip
        );
        if let Some(ext) = image.extended {
            println!(
                "Extended format: {}\nHeader offset: {:#X}\nCPU type code: {}\nEntry object: {}\nEntry offset: {:#X}\nExecution support: experimental guest-extender startup via run-stub; game compatibility pending",
                std::str::from_utf8(&ext.signature)?,
                ext.file_offset,
                ext.cpu_type,
                ext.entry_object,
                ext.entry_offset
            );
        } else {
            println!(
                "Execution support: standalone real-mode MZ loading; selected i386 instructions"
            );
        }
        return Ok(());
    }
    let (mut session, options, default_budget, bench) = match cli.command {
        Command::RunCom { path, options } => {
            let bytes = fs::read(path)?;
            if bytes.starts_with(b"MZ") {
                return Err(
                    "run-com accepts COM only; use run-mz for standalone real-mode MZ, or inspect for LE/LX".into(),
                );
            }
            (Session::new(&bytes)?, options, 200_000, false)
        }
        Command::RunMz { path, options } => {
            (Session::from_mz(&fs::read(path)?)?, options, 200_000, false)
        }
        Command::RunStub { path, options } => {
            (native_input::load_game(&path)?, options, 200_000, false)
        }
        Command::Bench(options) => (Session::new(&[0x40, 0xEB, 0xFD])?, options, 5_000_000, true),
        Command::Demo(options) => (Session::demo(), options, 200_000, false),
        Command::Inspect { .. } => unreachable!(),
    };
    let budget = options.steps.unwrap_or(default_budget);
    let frame_path = options.frame;
    if bench && frame_path.is_some() {
        return Err("--frame is unavailable for bench".into());
    }
    let core_profile = options.core_profile;
    let midi_path = options.midi_log;
    let wave_path = options.midi_wave;
    let mut gdb_address = options.gdb;
    let gdb_after = options.gdb_after.unwrap_or(0);
    let mut keys = std::collections::VecDeque::new();
    let mut trace_remaining = options.trace.unwrap_or(0);
    let trace_tail = options.trace_tail;
    #[cfg(feature = "profiling")]
    let mut profile = options.profile_cpu.then(profile::Profile::new);
    #[cfg(not(feature = "profiling"))]
    if options.profile_cpu {
        return Err("--profile-cpu requires cargo --features profiling".into());
    }
    let mut tail: [Option<StepSnapshot>; 64] = [None; 64];
    let mut tail_count = 0usize;
    let mut peeks: Vec<(usize, usize)> = Vec::new();
    let mut finds: Vec<Vec<u8>> = Vec::new();
    if let Some(path) = options.cdrom {
        session = session.with_cdrom_image(b'D', fs::read(path)?)?;
    }
    for value in options.key_at {
        let fields: Vec<_> = value.split(':').collect();
        if fields.len() != 3 {
            return Err("--key-at requires STEP:SCAN_HEX:ASCII_HEX".into());
        }
        keys.push_back((
            fields[0].parse()?,
            u8::from_str_radix(fields[1], 16)?,
            u8::from_str_radix(fields[2], 16)?,
        ));
    }
    if options.no_vga_waits {
        session.set_vga_wait_acceleration(false);
    }
    if options.no_idle_loops {
        session.set_idle_loops(false);
    }
    if options.no_decode_cache {
        session.set_decode_cache(false);
    }
    if options.no_dynamic_core {
        session.set_dynamic_core(false);
    }
    for value in options.peek {
        let (address, length) = value
            .split_once(':')
            .ok_or("--peek requires hexadecimal ADDRESS:decimal LENGTH")?;
        let address = usize::from_str_radix(address.trim_start_matches("0x"), 16)?;
        let length: usize = length.parse()?;
        if !(1..=128).contains(&length) {
            return Err("--peek length must be 1..128".into());
        }
        peeks.push((address, length));
    }
    for pattern in options.find_ascii {
        if pattern.is_empty() || pattern.len() > 128 || !pattern.is_ascii() {
            return Err("--find-ascii requires 1..128 ASCII bytes".into());
        }
        finds.push(pattern.into_bytes());
    }
    if gdb_after != 0 && gdb_address.is_none() {
        return Err("--gdb-after requires --gdb IP:PORT".into());
    }
    if gdb_address.is_some() && gdb_after >= budget {
        return Err("--gdb-after must be less than --steps".into());
    }
    #[cfg(feature = "profiling")]
    if gdb_address.is_some() && profile.is_some() {
        return Err("--profile-cpu cannot be combined with --gdb".into());
    }
    keys.make_contiguous().sort_by_key(|event| event.0);
    if let Some(path) = &core_profile {
        native_input::load_core_profile(&mut session, path);
    }
    session.start();
    let midi_log = midi_path
        .as_ref()
        .map(|path| fs::File::create(path).map(BufWriter::new))
        .transpose()?;
    let wave = wave_path
        .as_ref()
        .map(|path| wave::Wave::new(path))
        .transpose()?;
    #[cfg(feature = "profiling")]
    let drain_midi =
        midi_log.is_some() || wave.is_some() || profile.is_some() || gdb_address.is_some();
    #[cfg(not(feature = "profiling"))]
    let drain_midi = midi_log.is_some() || wave.is_some() || gdb_address.is_some();
    let mut output = RunOutput {
        midi_log,
        wave,
        drain_midi,
        keys,
        logged_midi: 0,
        pcm_frames: 0,
        pcm_peak: 0,
        midi_bytes: [0; 1024],
        pcm: [[0; 2]; 128],
    };
    let start = Instant::now();
    let mut remaining = budget;
    output.after_step(&mut session)?;
    while remaining != 0 && session.status() == SessionStatus::Running {
        if session.steps() >= gdb_after
            && let Some(address) = gdb_address.take()
        {
            if !debugger::serve(&mut session, address, budget, &mut |s| output.after_step(s))? {
                break;
            }
            remaining = budget.saturating_sub(session.steps());
            continue;
        }
        if trace_tail {
            tail[tail_count % tail.len()] = Some(StepSnapshot::capture(&session));
            tail_count += 1;
        }
        #[cfg(feature = "profiling")]
        let batch_limit = profile.as_mut().map_or(10_000, |p| p.sample(&session));
        #[cfg(not(feature = "profiling"))]
        let batch_limit: u32 = 10_000;
        let batch = if trace_remaining != 0 {
            let r = session.registers();
            println!(
                "{:08} {:04X}:{:04X} AX={:04X} BX={:04X} CX={:04X} DX={:04X} DS={:04X} ES={:04X} SS:SP={:04X}:{:04X} FLAGS={:04X}",
                session.steps(),
                r.cs,
                r.ip,
                r.get(Gpr16::Ax),
                r.get(Gpr16::Bx),
                r.get(Gpr16::Cx),
                r.get(Gpr16::Dx),
                r.ds,
                r.es,
                r.ss,
                r.get(Gpr16::Sp),
                session.flags()
            );
            trace_remaining -= 1;
            1
        } else if trace_tail {
            1
        } else {
            remaining.min(u64::from(batch_limit)) as u32
        };
        let boundary =
            output
                .keys
                .front()
                .map_or(budget, |event| event.0)
                .min(if gdb_address.is_some() {
                    gdb_after
                } else {
                    budget
                });
        let batch = u64::from(batch).min(boundary.saturating_sub(session.steps())) as u32;
        session.prepare_dynamic(2);
        #[cfg(feature = "profiling")]
        let cpu_start = Instant::now();
        let completed = session.advance(batch);
        #[cfg(feature = "profiling")]
        if let Some(profile) = &mut profile {
            profile.cpu += cpu_start.elapsed();
            profile.frame(&session);
        }
        remaining = remaining.saturating_sub(u64::from(completed));
        output.after_step(&mut session)?;
    }
    let elapsed = start.elapsed();
    if let Some(path) = &core_profile {
        native_input::save_core_profile(&session, path);
    }
    if session.dynamic_core_enabled() {
        println!("Native dynamic core: {:?}", session.dynamic_stats());
        if let Some(error) = session.dynamic_error() {
            eprintln!("JIT preparation stopped: {error}; interpreter fallback active");
        }
    }
    #[cfg(feature = "profiling")]
    if let Some(profile) = profile {
        profile.print(&session, elapsed);
    }
    if let Some(wave) = output.wave {
        println!(
            "Synthesized MIDI note-ons: {}; peak: {:.4}; nonzero samples: {}",
            wave.timeline.note_events(),
            wave.peak,
            wave.nonzero
        );
        wave.finish()?;
        println!(
            "MIDI audio written: {}",
            wave_path.as_ref().unwrap().display()
        );
    }
    if let Some(log) = &mut output.midi_log {
        log.flush()?;
        println!("Logged MIDI bytes: {}", output.logged_midi);
    }
    for pattern in finds {
        if let Some(ram) = session.ram_view(0x100000..0x1000000) {
            let addresses: Vec<_> = ram
                .windows(pattern.len())
                .enumerate()
                .filter(|(_, bytes)| *bytes == pattern.as_slice())
                .take(16)
                .map(|(offset, _)| format!("{:08X}", offset + 0x100000))
                .collect();
            println!(
                "Find {} in extended RAM: {addresses:?}",
                String::from_utf8_lossy(&pattern)
            );
        }
    }
    for (address, length) in peeks {
        println!(
            "Memory {address:08X}: {:02X?}",
            session.ram_view(address..address.checked_add(length).ok_or("peek overflow")?)
        );
    }
    if trace_tail {
        for index in tail_count.saturating_sub(tail.len())..tail_count {
            if let Some(snapshot) = tail[index % tail.len()] {
                snapshot.print();
            }
        }
    }
    println!(
        "Status: {:?}\nExecution steps: {}",
        session.status(),
        session.steps()
    );
    if !bench {
        println!(
            "Emulated time: {} ns (functional clock)",
            session.emulated_nanos()
        );
        println!(
            "Produced PCM frames: {}; peak: {} (drained without host playback)",
            output.pcm_frames, output.pcm_peak
        );
        println!("MPU commands/bytes/UART: {:?}", session.midi_stats());
        let (opens, reads, midi) = session.file_access_stats();
        println!(
            "DOS opens/reads: {opens}/{reads}; last MIDI file: {}",
            midi.map_or_else(|| "none".into(), String::from_utf8_lossy)
        );
        if let Some((path, mode, error)) = session.midi_file_request() {
            println!(
                "Last MIDI open request: {} mode={mode:02X} DOS error={error}",
                String::from_utf8_lossy(path)
            );
        }
        println!(
            "Video mode: {:?}\nPending MIDI bytes: {}",
            session.video_mode(),
            session.pending_midi()
        );
        if let Some(frame) = session.frame() {
            println!(
                "Graphics frame: {}x{}; nonzero indexed pixels: {}",
                frame.width,
                frame.height,
                frame.pixels.iter().filter(|pixel| **pixel != 0).count()
            );
        }
    }
    if session.loaded_overlays() != 0 {
        println!("Loaded MZ overlays: {}", session.loaded_overlays());
    }
    if bench {
        println!(
            "Elapsed: {:.3}s\nExecution steps/s: {:.0} (host throughput, not emulated MHz)",
            elapsed.as_secs_f64(),
            session.steps() as f64 / elapsed.as_secs_f64()
        );
    }
    if !session.console().is_empty() {
        println!("DOS output: {}", String::from_utf8_lossy(session.console()));
    }
    if let Some(path) = frame_path {
        let frame = session
            .frame()
            .ok_or("guest has not entered a supported video mode")?;
        let mut output = BufWriter::new(fs::File::create(&path)?);
        write!(output, "P6\n{} {}\n255\n", frame.width, frame.height)?;
        for index in frame.indices() {
            output.write_all(&frame.palette[usize::from(index)][..3])?;
        }
        output.flush()?;
        println!("Frame written: {}", path.display());
    }
    if let Some(error) = session.fault() {
        println!("Fault registers: {:?}", session.registers());
        println!(
            "Control registers: {:?}\nEFLAGS: {:08X}\nA20 enabled: {}",
            session.control_registers(),
            session.eflags(),
            session.a20_enabled()
        );
        if let doslet_runtime::RuntimeError::UnsupportedService { vector: 0x21, ax } = error
            && matches!(ax >> 8, 0x3D | 0x43 | 0x4B)
        {
            let r = session.registers();
            let address = doslet_x86::real_address(r.ds, r.get(Gpr16::Dx)) as usize;
            let mut name = [0; 128];
            let mut length = 0;
            for byte in &mut name {
                let Some(view) = session.ram_view(address + length..address + length + 1) else {
                    break;
                };
                *byte = view[0];
                if *byte == 0 {
                    break;
                }
                length += 1;
            }
            println!(
                "DOS requested path: {}",
                String::from_utf8_lossy(&name[..length])
            );
            if ax == 0x4B03 {
                let parameter = doslet_x86::real_address(r.es, r.get(Gpr16::Bx)) as usize;
                println!(
                    "Overlay parameter bytes: {:02X?}",
                    session.ram_view(parameter..parameter + 4)
                );
            }
        }
        return Err(error.into());
    }
    if !bench && session.status() == SessionStatus::Running {
        println!(
            "Budget registers: {:?}\nControl registers: {:?}",
            session.registers(),
            session.control_registers()
        );
        println!("A20 enabled: {}", session.a20_enabled());
        return Err("execution step budget exhausted".into());
    }
    Ok(())
}
