//! Reproducible boot-to-menu audit, without snapshots, window or host audio.
#[path = "../../common/native_input.rs"]
mod native_input;

use doslet_runtime::{Session, SessionStatus};
use doslet_x86::Gpr32;
use serde_json::json;
use std::{error::Error, fs, io::Write, path::Path, time::Instant};

fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

fn screenshot(s: &Session, path: &Path) -> Result<u64, Box<dyn Error>> {
    let frame = s.presentation_frame();
    let mut rgba = vec![0; frame.width() * frame.height() * 4];
    frame.write_rgba(&mut rgba).map_err(|e| format!("{e:?}"))?;
    let mut encoder = png::Encoder::new(
        fs::File::create(path)?,
        frame.width() as u32,
        frame.height() as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    Ok(hash(&rgba))
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    if !(7..=9).contains(&args.len()) {
        return Err(
            "usage: startup_timing EXE ISO OUT MHZ GUEST_SECONDS MARKERS.json [ESC_GUEST_MS|- [CAPTURE_GUEST_MS]]"
                .into(),
        );
    }
    let mhz = args[4].parse()?;
    let seconds: u64 = args[5].parse()?;
    if !(1..=90).contains(&seconds) {
        return Err("guest seconds must be 1..90".into());
    }
    let markers: serde_json::Value = serde_json::from_slice(&fs::read(&args[6])?)?;
    let number = |name| -> Result<u32, Box<dyn Error>> {
        Ok(u32::try_from(
            markers[name].as_u64().ok_or("missing marker integer")?,
        )?)
    };
    let head = number("menu_head")?;
    let callback_offset = number("callback_offset")?;
    let callback = number("callback")?;
    let mut escape: Option<u64> = args
        .get(7)
        .filter(|s| *s != "-")
        .map(|s| s.parse())
        .transpose()?;
    let escape_requested = escape;
    let capture: Option<u64> = args.get(8).map(|s| s.parse()).transpose()?;
    if capture.is_some_and(|ms| ms > seconds * 1000) || escape.is_some_and(|ms| ms > seconds * 1000)
    {
        return Err("capture and Esc must fall within the run".into());
    }
    let mut captured_at = None;
    let out = Path::new(&args[3]);
    fs::create_dir_all(out)?;
    let settings = native_input::LaunchSettings {
        hardware: doslet_config::HardwareConfig {
            cpu_mhz: mhz,
            mpu401: false,
            ..Default::default()
        },
        no_dynamic_core: true,
        cdrom: Some((b'D', args[2].clone().into())),
        ..Default::default()
    };
    let mut s = native_input::load_game_with_settings(Path::new(&args[1]), &settings)?;
    s.debug_disable_accelerations();
    s.start();
    let t0 = s.emulated_nanos();
    let start = Instant::now();
    let mut timeline = Vec::new();
    let mut next_sample = 100_000_000;
    let mut next_picture = 1_000_000_000;
    let mut menu_first = None;
    let mut menu_samples = 0;
    let mut pcm = [doslet_pcm::EMPTY; 1024];
    let mut midi = [0; 1024];
    let mut audio = fs::File::create(out.join("audio.s16le"))?;
    let mut pcm_frames = 0_u64;
    let mut pcm_nonzero = 0_u64;
    let mut pcm_gaps = 0;
    let mut pcm_end: Option<u64> = None;
    let mut sample_bytes = [0; 4096];
    let mut escaped_at = None;
    while s.emulated_nanos() - t0 < seconds * 1_000_000_000 {
        let elapsed = s.emulated_nanos() - t0;
        if escape.is_some_and(|ms| elapsed >= ms * 1_000_000) {
            s.push_key(1, 0x1b)?;
            escape = None;
            escaped_at = Some(elapsed);
        }
        // The menu arrival is a sampled upper bound, not a breakpoint timestamp.
        if s.debug_pc() == head {
            let mut bytes = [0; 4];
            if let Some(address) = s.registers().get32(Gpr32::Esp).checked_add(callback_offset)
                && s.debug_read_memory(address, &mut bytes)
                && u32::from_le_bytes(bytes) == callback
            {
                menu_first.get_or_insert(elapsed);
                menu_samples += 1;
            }
        }
        s.advance(256);
        if s.status() != SessionStatus::Running {
            return Err(format!("guest stopped: {:?}", s.fault()).into());
        }
        loop {
            let n = s.drain_pcm_timed(&mut pcm);
            if n == 0 {
                break;
            }
            for (i, f) in pcm[..n].iter().enumerate() {
                if pcm_end.is_some_and(|end| f.nanos > end + 1 || f.nanos + 1 < end) {
                    pcm_gaps += 1;
                }
                pcm_end = Some(f.nanos + u64::from(f.duration));
                pcm_nonzero += u64::from(f.samples != [0; 2]);
                sample_bytes[4 * i..4 * i + 2].copy_from_slice(&f.samples[0].to_le_bytes());
                sample_bytes[4 * i + 2..4 * i + 4].copy_from_slice(&f.samples[1].to_le_bytes());
            }
            pcm_frames += n as u64;
            audio.write_all(&sample_bytes[..4 * n])?;
        }
        while s.drain_midi(&mut midi) != 0 {}
        let elapsed = s.emulated_nanos() - t0;
        if captured_at.is_none() && capture.is_some_and(|ms| elapsed >= ms * 1_000_000) {
            let bytes = s.save_state()?;
            let mut gzip = flate2::write::GzEncoder::new(
                fs::File::create(out.join("capture.pb.gz"))?,
                flate2::Compression::fast(),
            );
            gzip.write_all(&bytes)?;
            gzip.finish()?.sync_all()?;
            captured_at = Some(elapsed);
        }
        if elapsed >= next_sample {
            let dsp = s.audio_status().dsp;
            timeline.push(
                json!({"guest_ns":elapsed,"pc":s.debug_pc(),"steps":s.steps(),
                "pcm_frames":pcm_frames,"pcm_nonzero":pcm_nonzero,
                "dma_checks":dsp.block_irq_deadlines,"dma_misses":dsp.block_irq_deadline_misses,
                "vram_fnv64":format!("{:016x}",hash(s.vram_view()))}),
            );
            next_sample += 100_000_000;
            if start.elapsed().as_secs() > 180 {
                return Err("startup host deadline: 180 seconds".into());
            }
        }
        if elapsed >= next_picture {
            screenshot(
                &s,
                &out.join(format!("{:02}.png", next_picture / 1_000_000_000)),
            )?;
            next_picture += 1_000_000_000;
        }
    }
    let rgba = screenshot(&s, &out.join("final.png"))?;
    let report = json!({"mhz":mhz,"requested_seconds":seconds,"guest_ns":s.emulated_nanos()-t0,
        "instrumented_host_seconds":start.elapsed().as_secs_f64(),"steps":s.steps(),
        "executable_fnv64":format!("{:016x}",hash(&fs::read(&args[1])?)),
        "interpreter":true,"accelerations":false,"esc_requested_ms":escape_requested,"esc_at_ns":escaped_at,
        "capture_requested_ms":capture,"captured_at_ns":captured_at,
        "first_sampled_menu_ns":menu_first,"menu_samples":menu_samples,
        "pcm_frames":pcm_frames,"pcm_nonzero":pcm_nonzero,"pcm_timestamp_discontinuities":pcm_gaps,
        "final_rgba_fnv64":format!("{rgba:016x}"),"timeline":timeline});
    fs::write(
        out.join("startup.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!(
        "{}",
        json!({"mhz":mhz,"first_sampled_menu_ns":menu_first,"menu_samples":menu_samples,
        "guest_seconds":seconds,"host_seconds":start.elapsed().as_secs_f64()})
    );
    Ok(())
}
