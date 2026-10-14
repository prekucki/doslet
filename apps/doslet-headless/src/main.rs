use doslet_executable::MzImage;
use doslet_runtime::{Session, SessionStatus};
use doslet_x86::{Gpr16, Gpr32};
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
fn run() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_else(|| "help".into());
    if command == "inspect" {
        let path = args.next().unwrap_or_else(|| "PRINCE/PRINCE.EXE".into());
        if args.next().is_some() {
            return Err("inspect accepts one executable path".into());
        }
        let bytes = fs::read(&path)?;
        let image = MzImage::parse(&bytes)?;
        println!(
            "Executable: {path}\nFile bytes: {}\nMZ load module: {} bytes\nMZ relocations: {}\nMZ entry: {:04X}:{:04X}",
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
    if !matches!(
        command.as_str(),
        "demo" | "run-com" | "run-mz" | "run-stub" | "bench"
    ) {
        println!(
            "Doslet headless\n\n  demo [--steps N] [--frame PATH.ppm]\n  run-com PATH [--steps N] [--frame PATH.ppm]\n  run-mz PATH [--steps N] [--frame PATH.ppm]\n  run-stub PATH [--steps N] [--frame PATH.ppm] [--midi-log PATH] [--midi-wave PATH.wav] (experimental game startup)\n  inspect [PATH]  (default: PRINCE/PRINCE.EXE)\n  bench [--steps N]\n\nRunner diagnostics: --trace N, --trace-tail, --peek HEXADDRESS:DECIMALLENGTH (1..128 bytes), --find-ascii PATTERN (1..128 bytes, up to 16 extended-RAM matches), --profile-cpu (requires Cargo feature profiling), --no-vga-waits (disable VGA wait recognition/batching).\nDemo runs an original COM program. PRINCE intro graphics and guest MIDI are observed; interactive sound is confirmed separately in the UI."
        );
        if command != "help" && command != "--help" && command != "-h" {
            return Err(format!("unknown command: {command}").into());
        }
        return Ok(());
    }
    let mut session = match command.as_str() {
        "run-com" => {
            let path = args.next().ok_or("run-com requires a file path")?;
            let bytes = fs::read(path)?;
            if bytes.starts_with(b"MZ") {
                return Err(
                    "run-com accepts COM only; use run-mz for standalone real-mode MZ, or inspect for LE/LX".into(),
                );
            }
            Session::new(&bytes)?
        }
        "run-mz" => {
            let path = args.next().ok_or("run-mz requires a file path")?;
            Session::from_mz(&fs::read(path)?)?
        }
        "run-stub" => {
            let path = args.next().ok_or("run-stub requires a file path")?;
            native_input::load_game(std::path::Path::new(&path))?
        }
        "bench" => Session::new(&[0x40, 0xEB, 0xFD])?,
        _ => Session::demo(),
    };
    let mut budget: u64 = if command == "bench" {
        5_000_000
    } else {
        200_000
    };
    let mut frame_path: Option<PathBuf> = None;
    let mut midi_path: Option<PathBuf> = None;
    let mut wave_path: Option<PathBuf> = None;
    let mut trace_remaining: u64 = 0;
    let mut trace_tail = false;
    #[cfg(feature = "profiling")]
    let mut profile = None;
    let mut tail: [Option<StepSnapshot>; 64] = [None; 64];
    let mut tail_count = 0usize;
    let mut peeks: Vec<(usize, usize)> = Vec::new();
    let mut finds: Vec<Vec<u8>> = Vec::new();
    while let Some(option) = args.next() {
        match option.as_str() {
            "--no-vga-waits" => session.set_vga_wait_acceleration(false),
            "--profile-cpu" => {
                #[cfg(feature = "profiling")]
                {
                    profile = Some(profile::Profile::new());
                }
                #[cfg(not(feature = "profiling"))]
                {
                    return Err("--profile-cpu requires cargo --features profiling".into());
                }
            }
            "--steps" => budget = args.next().ok_or("missing step budget")?.parse()?,
            "--trace" => trace_remaining = args.next().ok_or("missing trace limit")?.parse()?,
            "--trace-tail" => trace_tail = true,
            "--peek" => {
                let value = args.next().ok_or("missing ADDRESS:LENGTH for --peek")?;
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
            "--find-ascii" => {
                let pattern = args.next().ok_or("missing ASCII pattern")?;
                if pattern.is_empty() || pattern.len() > 128 || !pattern.is_ascii() {
                    return Err("--find-ascii requires 1..128 ASCII bytes".into());
                }
                finds.push(pattern.into_bytes());
            }
            "--midi-log" => midi_path = Some(args.next().ok_or("missing MIDI log path")?.into()),
            "--midi-wave" => wave_path = Some(args.next().ok_or("missing MIDI WAVE path")?.into()),
            "--frame" if command != "bench" => {
                frame_path = Some(args.next().ok_or("missing frame path")?.into())
            }
            _ => return Err(format!("unknown option: {option}").into()),
        }
    }
    session.start();
    let mut midi_log = midi_path
        .as_ref()
        .map(|path| fs::File::create(path).map(BufWriter::new))
        .transpose()?;
    let mut midi_bytes = [0u8; 1024];
    let mut logged_midi = 0u64;
    let mut wave = wave_path
        .as_ref()
        .map(|path| wave::Wave::new(path))
        .transpose()?;
    let start = Instant::now();
    let mut remaining = budget;
    let mut pcm = [[0i16; 2]; 128];
    let mut pcm_frames = 0u64;
    let mut pcm_peak = 0u16;
    while remaining != 0 && session.status() == SessionStatus::Running {
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
        #[cfg(feature = "profiling")]
        let cpu_start = Instant::now();
        let completed = session.advance(batch);
        #[cfg(feature = "profiling")]
        if let Some(profile) = &mut profile {
            profile.cpu += cpu_start.elapsed();
            profile.frame(&session);
        }
        remaining -= u64::from(completed);
        if let Some(wave) = &mut wave {
            wave.advance(session.emulated_nanos())?;
        }
        #[cfg(feature = "profiling")]
        let drain_midi = midi_log.is_some() || wave.is_some() || profile.is_some();
        #[cfg(not(feature = "profiling"))]
        let drain_midi = midi_log.is_some() || wave.is_some();
        if drain_midi {
            let count = session.drain_midi(&mut midi_bytes);
            if count != 0 {
                if let Some(log) = &mut midi_log {
                    write!(log, "{}\t", session.emulated_nanos())?;
                    for byte in &midi_bytes[..count] {
                        write!(log, "{byte:02X}")?;
                    }
                    writeln!(log)?;
                    logged_midi += count as u64;
                }
                if let Some(wave) = &mut wave {
                    for byte in &midi_bytes[..count] {
                        wave.timeline.push_byte(*byte)?;
                    }
                }
            }
        }
        loop {
            let count = session.drain_pcm(&mut pcm);
            pcm_frames += count as u64;
            for frame in &pcm[..count] {
                for sample in frame {
                    pcm_peak = pcm_peak.max(sample.unsigned_abs());
                }
            }
            if count == 0 {
                break;
            }
        }
    }
    let elapsed = start.elapsed();
    #[cfg(feature = "profiling")]
    if let Some(profile) = profile {
        profile.print(&session, elapsed);
    }
    if let Some(wave) = wave {
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
    if let Some(log) = &mut midi_log {
        log.flush()?;
        println!("Logged MIDI bytes: {logged_midi}");
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
    if command != "bench" {
        println!(
            "Emulated time: {} ns (functional clock)",
            session.emulated_nanos()
        );
        println!(
            "Produced PCM frames: {pcm_frames}; peak: {pcm_peak} (drained without host playback)"
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
    if command == "bench" {
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
        for index in frame.pixels {
            output.write_all(&frame.palette[usize::from(*index)][..3])?;
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
    if command != "bench" && session.status() == SessionStatus::Running {
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
