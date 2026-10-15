//! Local compatibility probe. Never changes the supplied installation.
#[path = "../src/heroes_timer_audit.rs"]
mod heroes_timer_audit;
#[path = "../../common/native_input.rs"]
mod native_input;
#[cfg(feature = "profiling")]
#[path = "../src/profile.rs"]
mod profile;
#[cfg(feature = "profiling")]
#[path = "../src/wait_trace.rs"]
mod wait_trace;
use doslet_runtime::SessionStatus;
use std::{
    env,
    error::Error,
    fs,
    io::{Seek, SeekFrom, Write},
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args().skip(1);
    let path = args.next().unwrap_or_else(|| "games/CIV.EXE".into());
    let keys = args.next().unwrap_or_default();
    let budget: u64 = args.next().unwrap_or_else(|| "10000000".into()).parse()?;
    // Optional timed events: STEP:SCAN_HEX:ASCII_HEX, separated by commas.
    let schedule = args.next().unwrap_or_default();
    let mut mt32 = if let Some(directory) = args.next() {
        let directory = std::path::Path::new(&directory);
        Some(doslet_mt32::Timeline::new(
            &fs::read(directory.join("CM32L_CONTROL.ROM"))?,
            &fs::read(directory.join("CM32L_PCM.ROM"))?,
            32000,
        )?)
    } else {
        None
    };
    let capture_pcm = env::var_os("DOSLET_PROBE_PCM").is_some();
    let translation = env::var("DOSLET_CIV_MIDI_MODE")
        .ok()
        .or_else(|| capture_pcm.then(|| "auto".into()));
    let mut routing = translation.as_ref().map(|mode| {
        doslet_midi_routing::Routing::new(if mode == "mt32" {
            doslet_midi_routing::Mode::Mt32
        } else {
            doslet_midi_routing::Mode::Gm
        })
    });
    let mut gm = translation
        .as_ref()
        .map(|_| doslet_midi_synth::Timeline::new(32000).unwrap());
    let mut audio = if mt32.is_some() || gm.is_some() {
        Some(fs::File::create("/tmp/doslet-civ-mt32.wav")?)
    } else {
        None
    };
    if let Some(file) = &mut audio {
        file.write_all(&[0; 44])?;
    }
    let mut samples = [[0f32; 2]; 256];
    let mut audio_frames = 0u32;
    let mut audio_peak = 0f32;
    let mut pcm_mixer = doslet_pcm::Mixer::new(32000).unwrap();
    let mut pcm_frames = 0_u64;
    let mut pcm_nonzero = 0_u64;
    let mut schedule = schedule
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut parts = s.split(':');
            Ok((
                parts.next().ok_or("step")?.parse::<u64>()?,
                u8::from_str_radix(parts.next().ok_or("scan")?, 16)?,
                u8::from_str_radix(parts.next().ok_or("ascii")?, 16)?,
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?
        .into_iter()
        .peekable();
    // Optional mouse events STEP:X:Y:BUTTON:DOWN (-1 means movement only).
    let pointer = env::var("DOSLET_CIV_POINTER_EVENTS").unwrap_or_default();
    let mut pointer = pointer
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| {
            let mut p = s.split(':');
            Ok((
                p.next().ok_or("step")?.parse::<u64>()?,
                p.next().ok_or("x")?.parse::<u16>()?,
                p.next().ok_or("y")?.parse::<u16>()?,
                p.next().ok_or("button")?.parse::<i8>()?,
                p.next().ok_or("down")? == "1",
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?
        .into_iter()
        .peekable();
    let settings = native_input::LaunchSettings {
        heroes_timer_waits: env::var_os("DOSLET_PROBE_HEROES_TIMER_WAITS").is_some(),
        cdrom: env::var_os("DOSLET_PROBE_CDROM").map(|image| (b'D', image.into())),
        ..Default::default()
    };
    let mut session =
        native_input::load_game_with_settings(std::path::Path::new(&path), &settings)?;
    session.start();
    let mut keys = keys.bytes();
    let mut screen = [0u8; 2000];
    let mut shown = false;
    let mut midi = [0; 1024];
    let mut midi_log = fs::File::create("/tmp/doslet-civ-midi.bin")?;
    let mut decoder = doslet_midi_synth::wire::Decoder::new();
    let mut pcm = [doslet_pcm::EMPTY; 128];
    let trace_files = env::var_os("DOSLET_CIV_DRIVER_NAMES").is_some();
    let trace_mouse = env::var_os("DOSLET_PROBE_MOUSE").is_some();
    let mut mouse_calls = [0_u64; 256];
    let mut last_game_pointer = None;
    #[cfg(feature = "profiling")]
    let mut cpu_profile = env::var("DOSLET_PROBE_PROFILE_AFTER")
        .ok()
        .map(|value| {
            value
                .parse::<u64>()
                .map(|after| (after, profile::Profile::new()))
        })
        .transpose()?;
    #[cfg(feature = "profiling")]
    let mut profile_start = None;
    #[cfg(feature = "profiling")]
    let wait_trace_after = env::var("DOSLET_PROBE_WAIT_AFTER")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?;
    #[cfg(feature = "profiling")]
    let mut waits = wait_trace::WaitTrace::default();
    let trace_after = env::var("DOSLET_PROBE_TRACE_AFTER")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    let timer_after = env::var("DOSLET_PROBE_HEROES_TIMER_AFTER")
        .ok()
        .map(|value| value.parse::<u64>())
        .transpose()?;
    let mut timer_audit = heroes_timer_audit::Audit::default();
    while session.steps() < budget && session.status() == SessionStatus::Running {
        #[allow(unused_mut)]
        let mut execution_budget = 10000;
        #[cfg(feature = "profiling")]
        if let Some((after, profile)) = &mut cpu_profile
            && session.steps() >= *after
        {
            if profile_start.is_none() {
                profile_start = Some(std::time::Instant::now());
                println!(
                    "PROFILE BASELINE steps={} counters={:?}",
                    session.steps(),
                    session.execution_stats()
                );
            }
            execution_budget = profile.sample(&session);
        }
        if trace_mouse && session.registers().cs == 0x180 && session.registers().ip == 0x214098 {
            let object = session.registers().get32(doslet_x86::Gpr32::Ecx) as usize;
            if let Some(bytes) = session.ram_view(object..object + 0x338) {
                let word = |offset| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
                let pointer = (word(0x330), word(0x332));
                if last_game_pointer != Some(pointer) {
                    println!(
                        "HEROES POINTER step={} object={object:08X} position={pointer:?} divisor={}",
                        session.steps(),
                        word(0x21E)
                    );
                    last_game_pointer = Some(pointer);
                }
            }
        }
        if trace_mouse && session.registers().cs == 0xF000 && session.registers().ip == 0x33 {
            let r = session.registers();
            let ax = r.get(doslet_x86::Gpr16::Ax);
            mouse_calls[usize::from(ax as u8)] += 1;
            if mouse_calls[usize::from(ax as u8)] <= 8 {
                println!(
                    "MOUSE INT33 step={} AX={ax:04X} BX={:04X} CX={:04X} DX={:04X} SI={:04X} DI={:04X} pointer={:?}",
                    session.steps(),
                    r.get(doslet_x86::Gpr16::Bx),
                    r.get(doslet_x86::Gpr16::Cx),
                    r.get(doslet_x86::Gpr16::Dx),
                    r.get(doslet_x86::Gpr16::Si),
                    r.get(doslet_x86::Gpr16::Di),
                    session.pointer_position()
                );
            }
        }
        let tracing = trace_files && trace_after.is_none_or(|after| session.steps() >= after);
        if tracing {
            let r = session.registers();
            if r.cs == 0xF000 && r.ip == 0x2F {
                println!(
                    "MSCDEX ax={:04X} bx={:04X} cx={:04X} dx={:04X} es={:04X} si={:04X} di={:04X}",
                    r.get(doslet_x86::Gpr16::Ax),
                    r.get(doslet_x86::Gpr16::Bx),
                    r.get(doslet_x86::Gpr16::Cx),
                    r.get(doslet_x86::Gpr16::Dx),
                    r.es,
                    r.get(doslet_x86::Gpr16::Si),
                    r.get(doslet_x86::Gpr16::Di)
                );
            }
            if r.cs == 0xF000
                && r.ip == 0x21
                && matches!(r.get(doslet_x86::Gpr16::Ax) >> 8, 0x3D | 0x4B)
            {
                let address =
                    doslet_x86::linear_address(r.ds, r.get(doslet_x86::Gpr16::Dx)) as usize;
                if let Some(bytes) = session.ram_view(address..address + 128) {
                    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
                    println!("DOS OPEN {}", String::from_utf8_lossy(&bytes[..end]));
                }
            }
        }
        if trace_after.is_some()
            && session.registers().ip == 0x1E1318
            && session.registers().cs == 0x160
        {
            let stack = session.registers().get32(doslet_x86::Gpr32::Esp) as usize;
            println!(
                "ERROR HANDLER stack={:02X?}",
                session.ram_view(stack..stack + 64)
            );
            session.pause();
            break;
        }
        #[cfg(feature = "profiling")]
        let execution_started = std::time::Instant::now();
        #[cfg(feature = "profiling")]
        let tracing_waits = wait_trace_after.is_some_and(|after| session.steps() >= after);
        #[cfg(feature = "profiling")]
        let wait_before = if tracing_waits {
            execution_budget = 1;
            waits.before(&session)
        } else {
            None
        };
        if timer_after.is_some_and(|after| session.steps() >= after) {
            timer_audit.advance_one(&mut session);
        } else {
            session.advance(
                if tracing
                    || (trace_mouse && trace_after.is_none())
                    || trace_after.is_some_and(|step| session.steps() >= step)
                {
                    1
                } else {
                    execution_budget
                },
            );
        }
        #[cfg(feature = "profiling")]
        if tracing_waits {
            waits.after(&session, wait_before);
        }
        #[cfg(feature = "profiling")]
        if profile_start.is_some()
            && let Some((_, profile)) = &mut cpu_profile
        {
            profile.cpu += execution_started.elapsed();
            profile.frame(&session);
        }
        if pointer.peek().is_some_and(|e| session.steps() >= e.0) {
            let (_, x, y, button, pressed) = pointer.next().unwrap();
            session.pointer_move(x, y);
            println!(
                "GUEST POINTER position={:?} ranges={:?}",
                session.pointer_position(),
                session.pointer_ranges()
            );
            if button >= 0 {
                session.pointer_button(button as u8, pressed);
            }
            println!(
                "POINTER steps={} {x},{y} button={button} down={pressed}",
                session.steps()
            );
        }
        if schedule
            .peek()
            .is_some_and(|(step, _, _)| session.steps() >= *step)
        {
            let (_, scan, ascii) = schedule.next().unwrap();
            println!("TIMED KEY steps={} {scan:02X}:{ascii:02X}", session.steps());
            session.push_key(scan, ascii)?;
        }
        loop {
            let count = session.drain_pcm_timed(&mut pcm);
            if count == 0 {
                break;
            }
            pcm_frames += count as u64;
            pcm_nonzero += pcm[..count]
                .iter()
                .filter(|frame| frame.samples != [0; 2])
                .count() as u64;
            if audio.is_some() {
                pcm_mixer
                    .receive(&pcm[..count])
                    .map_err(|e| e.to_string())?;
            }
        }
        let count = session.drain_midi(&mut midi);
        if let Some(mt32) = &mut mt32 {
            loop {
                let count = mt32.render_until(session.emulated_nanos(), &mut samples)?;
                if count == 0 {
                    break;
                }
                pcm_mixer.mix(&mut samples[..count]);
                for frame in &samples[..count] {
                    for sample in frame {
                        audio_peak = audio_peak.max(sample.abs());
                        audio.as_mut().unwrap().write_all(
                            &((sample.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes(),
                        )?;
                    }
                }
                audio_frames += count as u32;
            }
            for byte in &midi[..count] {
                mt32.push_byte(*byte)?;
            }
        }
        if let (Some(routing), Some(gm)) = (&mut routing, &mut gm) {
            loop {
                let count = gm.render_until(session.emulated_nanos(), &mut samples)?;
                if count == 0 {
                    break;
                }
                pcm_mixer.mix(&mut samples[..count]);
                for frame in &samples[..count] {
                    for sample in frame {
                        audio_peak = audio_peak.max(sample.abs());
                        audio.as_mut().unwrap().write_all(
                            &((sample.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes(),
                        )?;
                    }
                }
                audio_frames += count as u32;
            }
            let mut error = None;
            for &byte in &midi[..count] {
                routing
                    .push(byte, doslet_midi_routing::Mode::Gm, &mut |message| {
                        for &byte in message {
                            if let Err(e) = gm.push_byte(byte) {
                                error = Some(e);
                            }
                        }
                    })
                    .map_err(|e| format!("MIDI route: {e:?}"))?;
            }
            if let Some(error) = error {
                return Err(error.into());
            }
        }
        midi_log.write_all(&midi[..count])?;
        for byte in &midi[..count] {
            if let Some(message) = decoder
                .push(*byte)
                .map_err(|error| format!("MIDI: {error:?}"))?
                && message[0] == 0xF0
            {
                println!("SYSEX steps={} {:02X?}", session.steps(), message);
            }
        }
        if session.waiting_for_key() {
            let text = session.ram_view(0xB8000..0xB8000 + 4000).unwrap();
            let changed = text
                .as_chunks::<2>()
                .0
                .iter()
                .zip(screen.iter())
                .any(|(cell, old)| cell[0] != *old);
            if changed || !shown {
                for (cell, byte) in text.as_chunks::<2>().0.iter().zip(screen.iter_mut()) {
                    *byte = cell[0];
                }
                println!("\nTEXT at {} steps:", session.steps());
                for row in screen.as_chunks::<80>().0.iter() {
                    println!("{}", String::from_utf8_lossy(row).trim_end());
                }
                shown = true;
            }
            let pointers = session.ram_view(0x41A..0x41E).unwrap();
            if pointers[..2] == pointers[2..]
                && let Some(key) = keys.next()
            {
                let ascii = if key == b'|' { 13 } else { key };
                println!("KEY {ascii:02X}");
                session.push_key(if ascii == 13 { 0x1C } else { 0 }, ascii)?;
            }
        }
    }
    if trace_mouse {
        println!(
            "MOUSE INT33 totals={:?}",
            mouse_calls
                .iter()
                .enumerate()
                .filter(|(_, count)| **count != 0)
                .collect::<Vec<_>>()
        );
    }
    #[cfg(feature = "profiling")]
    if let Some(start) = profile_start
        && let Some((_, profile)) = &cpu_profile
    {
        profile.print(&session, start.elapsed());
    }
    #[cfg(feature = "profiling")]
    if wait_trace_after.is_some() {
        let output = env::var("DOSLET_PROBE_WAIT_OUTPUT")
            .unwrap_or_else(|_| "/tmp/doslet-waits.json".into());
        waits.write(fs::File::create(&output)?)?;
        println!("WAIT AUDIT {output}");
    }
    println!(
        "{:?} {:?} steps={} mode={:?} ip={:04X}:{:X} waiting={}",
        session.status(),
        session.fault(),
        session.steps(),
        session.video_mode(),
        session.registers().cs,
        session.registers().ip,
        session.waiting_for_key()
    );
    if let Some(file) = &mut audio {
        let bytes = audio_frames.checked_mul(4).ok_or("WAVE too large")?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(b"RIFF")?;
        file.write_all(&(bytes + 36).to_le_bytes())?;
        file.write_all(b"WAVEfmt ")?;
        file.write_all(&16u32.to_le_bytes())?;
        file.write_all(&1u16.to_le_bytes())?;
        file.write_all(&2u16.to_le_bytes())?;
        file.write_all(&32000u32.to_le_bytes())?;
        file.write_all(&128000u32.to_le_bytes())?;
        file.write_all(&4u16.to_le_bytes())?;
        file.write_all(&16u16.to_le_bytes())?;
        file.write_all(b"data")?;
        file.write_all(&bytes.to_le_bytes())?;
        if let Some(mt32) = mt32 {
            println!(
                "CM-32L notes={} SysEx={} frames={} peak={audio_peak:.6}",
                mt32.note_events(),
                mt32.sysex_messages(),
                audio_frames
            );
        }
        if let (Some(routing), Some(gm)) = (routing, gm) {
            println!(
                "TRANSLATION mode={} upgraded={} notes={} stats={:?} frames={} peak={audio_peak:.6}",
                routing.mode(),
                routing.upgraded(),
                gm.note_events(),
                routing.translator().stats(),
                audio_frames
            );
        }
    }
    if timer_after.is_some() {
        timer_audit.print(&session);
    }
    if env::var_os("DOSLET_PROBE_RAM").is_some() {
        fs::write("/tmp/doslet-probe-vram.bin", session.vram_view())?;
        if let Some(ram) = session.ram_view(0..0xA0000) {
            fs::write("/tmp/doslet-probe-low.bin", ram)?;
        }
        let mut segment = 0x0EFFusize;
        let mut blocks = 0;
        let mut free = 0;
        for _ in 0..2048 {
            let Some(mcb) = session.ram_view(segment * 16..segment * 16 + 16) else {
                break;
            };
            if !matches!(mcb[0], b'M' | b'Z') {
                break;
            }
            let owner = u16::from_le_bytes([mcb[1], mcb[2]]);
            let paragraphs = u16::from_le_bytes([mcb[3], mcb[4]]);
            blocks += 1;
            if owner == 0 {
                free += usize::from(paragraphs) * 16;
            }
            if mcb[0] == b'Z' {
                break;
            }
            segment += usize::from(paragraphs) + 1;
        }
        println!("DOS MCB blocks={blocks} free_bytes={free}");
    }
    if env::var_os("DOSLET_PROBE_RAM").is_some()
        && let Some(ram) = session.ram_view(0x100000..0x1000000)
    {
        fs::write("/tmp/doslet-probe-extended.bin", ram)?;
        for (offset, window) in ram.windows(24).enumerate() {
            if window == b"Critical memory shortage" {
                let address = 0x100000u32 + offset as u32;
                println!("MEMORY ERROR STRING {address:08X}");
                for (reference, bytes) in ram.windows(4).enumerate() {
                    if bytes == address.to_le_bytes() {
                        println!("MEMORY ERROR REFERENCE {:08X}", 0x100000 + reference);
                    }
                }
            }
        }
    }
    if env::var_os("DOSLET_PROBE_FRAME").is_some() {
        let frame = session.presentation_frame();
        let mut rgba = vec![0; frame.width() * frame.height() * 4];
        frame
            .write_rgba(&mut rgba)
            .map_err(|e| format!("frame: {e:?}"))?;
        let mut file = fs::File::create("/tmp/doslet-probe.ppm")?;
        write!(file, "P6\n{} {}\n255\n", frame.width(), frame.height())?;
        for pixel in rgba.as_chunks::<4>().0.iter() {
            file.write_all(&pixel[..3])?;
        }
    }
    println!(
        "CPU {:?} flags={:08X}",
        session.registers(),
        session.flags()
    );
    println!("CONSOLE {}", String::from_utf8_lossy(session.console()));
    println!("SB PCM frames={pcm_frames} nonzero={pcm_nonzero} mixed_peak={audio_peak}");
    println!("MEMORY WAIT batched_steps={}", session.memory_wait_steps());
    if let Some(error) = session.fault() {
        println!("{:?}", session.registers());
        let start = usize::from(session.registers().ds) * 16
            + usize::from(session.registers().get(doslet_x86::Gpr16::Dx));
        println!("DS:DX {:?}", session.ram_view(start..start + 48));
        return Err(error.into());
    }
    Ok(())
}
