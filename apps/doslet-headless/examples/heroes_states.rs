//! Local portable fixtures and repeatable benchmarks; no window/audio device/MCP.
use state_input::native_input;
#[path = "../../common/state_input.rs"]
mod state_input;

use doslet_runtime::{Session, SessionStatus};
use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    io::{Read, Write},
    path::Path,
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

fn drain(s: &mut Session) -> (u64, u64) {
    let mut pcm = [doslet_pcm::EMPTY; 1024];
    let mut midi = [0; 1024];
    let mut frames = 0;
    let mut digest = 0_u64;
    loop {
        let n = s.drain_pcm_timed(&mut pcm);
        if n == 0 {
            break;
        }
        frames += n as u64;
        for f in &pcm[..n] {
            for sample in f.samples {
                digest = digest
                    .wrapping_mul(0x100000001b3u64)
                    .wrapping_add(sample as u16 as u64);
            }
        }
    }
    while s.drain_midi(&mut midi) != 0 {}
    (frames, digest)
}

fn advance(s: &mut Session, millis: u64) -> Result<u64> {
    let target = s.emulated_nanos() + millis * 1_000_000;
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut pcm = 0;
    while s.emulated_nanos() < target {
        s.advance(10000);
        pcm += drain(s).0;
        s.prepare_dynamic(8);
        if s.status() != SessionStatus::Running {
            return Err(format!("guest {:?}: {:?}", s.status(), s.fault()).into());
        }
        if Instant::now() > deadline {
            return Err("guest-time advance timed out".into());
        }
    }
    Ok(pcm)
}

fn click(s: &mut Session, x: f64, y: f64) -> Result<()> {
    s.pointer_relative(-1000.0, -1000.0);
    advance(s, 250)?;
    s.pointer_relative(x, y);
    advance(s, 250)?;
    s.pointer_button(0, true);
    advance(s, 250)?;
    s.pointer_button(0, false);
    advance(s, 500)?;
    Ok(())
}

fn screenshot(s: &Session, path: &Path) -> Result<()> {
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
    Ok(())
}

fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}

fn summary(s: &Session) -> Value {
    let a = s.synchronization_analysis();
    let dsp = s.audio_status().dsp;
    let r = s.registers();
    json!({"guest_ns":s.emulated_nanos(),"steps":s.steps(),"status":format!("{:?}",s.status()),
        "pc":[r.cs as u32,r.ip],"mhz":s.hardware_config().cpu_mhz,
        "vram_hash":format!("{:016x}",hash(s.vram_view())),
        "dma_checks":dsp.block_irq_deadlines,"dma_misses":dsp.block_irq_deadline_misses,
        "reductions":a.reductions,"recoveries":a.recoveries,"reason":a.reason,
        "wait_ns":a.recurrence.measured_wait_nanos,
        "candidates":a.candidates.iter().map(|c|json!({"head":c.head,"period_ns":c.period_nanos,
            "reference_ns":c.reference_period_nanos,"mean_ns":c.cadence_mean_nanos,
            "work_ns":c.work_nanos,"wait_ns":c.wait_nanos})).collect::<Vec<_>>()})
}

fn verify_resume(a: &mut Session, b: &mut Session) -> Result<()> {
    for _ in 0..100 {
        if a.advance(10000) != b.advance(10000) || !matching_output(a, b) {
            return Err("resumed output mismatch".into());
        }
    }
    let state_a = a.save_state()?;
    let state_b = b.save_state()?;
    if state_a != state_b {
        if let Some(directory) = std::env::var_os("DOSLET_STATE_DEBUG_DIR") {
            fs::write(
                Path::new(&directory).join("state-mismatch-original.pb"),
                &state_a,
            )?;
            fs::write(
                Path::new(&directory).join("state-mismatch-restored.pb"),
                &state_b,
            )?;
        }
        eprintln!("original: {}\nrestored: {}", summary(a), summary(b));
        return Err("resumed state mismatch".into());
    }
    Ok(())
}

fn matching_output(a: &mut Session, b: &mut Session) -> bool {
    let mut left = [doslet_pcm::EMPTY; 1024];
    let mut right = [doslet_pcm::EMPTY; 1024];
    loop {
        let na = a.drain_pcm_timed(&mut left);
        let nb = b.drain_pcm_timed(&mut right);
        if left[..na] != right[..nb] {
            return false;
        }
        if na == 0 {
            break;
        }
    }
    let mut left = [0; 1024];
    let mut right = [0; 1024];
    loop {
        let na = a.drain_midi(&mut left);
        let nb = b.drain_midi(&mut right);
        if left[..na] != right[..nb] {
            return false;
        }
        if na == 0 {
            return true;
        }
    }
}

fn save(s: &mut Session, path: &Path) -> Result<()> {
    let bytes = s.save_state()?;
    let mut metadata = summary(s);
    let mut restored = Session::load_state_with_media(&bytes, s)?;
    let png_tmp = path.with_extension("tmp.png");
    screenshot(s, &png_tmp)?;
    verify_resume(s, &mut restored)?;
    metadata["verified_original_resume_steps"] = json!(1_000_000);
    // A failed write never replaces an existing fixture.
    let tmp = path.with_extension("tmp");
    let mut gzip =
        flate2::write::GzEncoder::new(fs::File::create(&tmp)?, flate2::Compression::fast());
    gzip.write_all(&bytes)?;
    gzip.finish()?.sync_all()?;
    fs::rename(tmp, path)?;
    fs::rename(png_tmp, path.with_extension("png"))?;
    metadata["protobuf_bytes"] = json!(bytes.len());
    metadata["protobuf_hash"] = json!(format!("{:016x}", hash(&bytes)));
    fs::write(
        path.with_extension("json"),
        serde_json::to_vec_pretty(&metadata)?,
    )?;
    println!("saved {}: {metadata}", path.display());
    Ok(())
}

fn load(path: &Path, loader: &state_input::Loader) -> Result<Session> {
    loader.restore(&read_state(path)?)
}

fn read_state(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(path)?)
        .take(1024 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn bench_second(s: &mut Session, scroll: bool, second: u64) -> Result<u64> {
    if !scroll {
        return advance(s, 1000);
    }
    // The pointer starts at the map center. Briefly touch alternating edges;
    // keep the viewport near the discovered terrain instead of crossing the map.
    let dx = if second.is_multiple_of(2) {
        -318.0
    } else {
        317.0
    };
    s.pointer_relative(dx, 0.0);
    let pcm = advance(s, 100)?;
    s.pointer_relative(-dx, 0.0);
    Ok(pcm + advance(s, 900)?)
}

fn play(s: &mut Session, actions: &Value) -> Result<()> {
    for a in actions.as_array().ok_or("actions must be an array")? {
        if let Some(ms) = a.get("wait_ms").and_then(Value::as_u64) {
            advance(s, ms)?;
        } else if let Some(p) = a.get("click").and_then(Value::as_array) {
            if p.len() != 2 {
                return Err("click requires x,y".into());
            }
            click(s, p[0].as_f64().ok_or("x")?, p[1].as_f64().ok_or("y")?)?;
        } else if let Some(p) = a.get("move").and_then(Value::as_array) {
            if p.len() != 2 {
                return Err("move requires dx,dy".into());
            }
            s.pointer_relative(p[0].as_f64().ok_or("dx")?, p[1].as_f64().ok_or("dy")?);
            advance(s, 250)?;
        } else if a.get("press") == Some(&Value::Bool(true)) {
            s.pointer_button(0, true);
            advance(s, 250)?;
            s.pointer_button(0, false);
            advance(s, 500)?;
        } else if let Some(key) = a.get("key").and_then(Value::as_array) {
            if key.len() != 2 {
                return Err("key requires scan,ascii".into());
            }
            let scan = u8::try_from(key[0].as_u64().ok_or("scan")?)?;
            let ascii = u8::try_from(key[1].as_u64().ok_or("ascii")?)?;
            s.push_key(scan, ascii)?;
            advance(s, 500)?;
        } else {
            return Err(format!("unknown action: {a}").into());
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let loader = state_input::Loader::arguments(&mut args)?;
    match args.first().map(String::as_str) {
        Some("capture-menu") if args.len() == 4 => {
            let settings = native_input::LaunchSettings {
                hardware: doslet_config::HardwareConfig {
                    mpu401: false,
                    ..Default::default()
                },
                no_dynamic_core: true,
                cdrom: Some((b'D', args[2].clone().into())),
                ..Default::default()
            };
            let mut s = native_input::load_game_with_settings(Path::new(&args[1]), &settings)?;
            s.start();
            let deadline = Instant::now() + Duration::from_secs(120);
            while s.steps() < 350_000_000 {
                let next = if s.steps() < 100_000_000 {
                    100_000_000
                } else {
                    350_000_000
                };
                s.advance((next - s.steps()).min(10000) as u32);
                drain(&mut s);
                if s.steps() == 100_000_000 {
                    s.push_key(1, 0x1b)?;
                }
                if s.status() != SessionStatus::Running || Instant::now() > deadline {
                    return Err(format!("boot failed: {:?}", s.fault()).into());
                }
            }
            s.set_dynamic_core(true);
            s.set_cpu_mhz(133)?;
            advance(&mut s, 1000)?;
            save(&mut s, Path::new(&args[3]))?;
        }
        Some("convert") if args.len() == 3 => {
            let mut s = load(Path::new(&args[1]), &loader)?;
            save(&mut s, Path::new(&args[2]))?;
        }
        Some("play") if args.len() == 4 => {
            let mut s = load(Path::new(&args[1]), &loader)?;
            play(&mut s, &serde_json::from_slice(&fs::read(&args[2])?)?)?;
            save(&mut s, Path::new(&args[3]))?;
        }
        Some("verify") if args.len() == 2 => {
            let mut a = load(Path::new(&args[1]), &loader)?;
            advance(&mut a, 250)?;
            let saved = a.save_state()?;
            let mut b = Session::load_state_with_media(&saved, &a)?;
            if saved != b.save_state()? {
                return Err("initial round-trip mismatch".into());
            }
            drop(saved);
            verify_resume(&mut a, &mut b)?;
            println!(
                "verified identical full state and PCM after 1,000,000 steps: {}",
                args[1]
            );
        }
        Some("bench") if (3..=5).contains(&args.len()) => {
            let path = Path::new(&args[1]);
            let seconds: u64 = args[2].parse()?;
            if !(1..=300).contains(&seconds) {
                return Err("seconds must be 1..300".into());
            }
            let repeats: usize = args.get(3).map(|x| x.parse()).transpose()?.unwrap_or(3);
            if !(1..=10).contains(&repeats) {
                return Err("repeats must be 1..10".into());
            }
            let scroll = match args.get(4).map(String::as_str) {
                None => false,
                Some("scroll") => true,
                _ => return Err("workload must be scroll or omitted".into()),
            };
            let bytes = read_state(path)?;
            let fixture_hash = format!("{:016x}", hash(&bytes));
            let executable_hash = format!("{:016x}", hash(&fs::read(std::env::current_exe()?)?));
            let mut results = Vec::new();
            for repeat in 0..repeats {
                for adaptive in if repeat % 2 == 0 {
                    [false, true]
                } else {
                    [true, false]
                } {
                    let mut s = loader.restore(&bytes)?;
                    let initial = summary(&s);
                    s.set_synchronization_analysis(adaptive);
                    if adaptive {
                        s.set_adaptive_cpu(true);
                    }
                    for second in 0..5 {
                        bench_second(&mut s, scroll, second)?;
                    }
                    let measured_initial = summary(&s);
                    let before = s.audio_status().dsp;
                    let guest_start = s.emulated_nanos();
                    let start = Instant::now();
                    let mut pcm = 0;
                    let mut timeline = Vec::new();
                    for second in 1..=seconds {
                        pcm += bench_second(&mut s, scroll, second)?;
                        timeline.push(json!({"second":second,"mhz":s.hardware_config().cpu_mhz}));
                    }
                    let elapsed = start.elapsed().as_secs_f64();
                    let after = s.audio_status().dsp;
                    let guest_seconds = (s.emulated_nanos() - guest_start) as f64 / 1e9;
                    let row = json!({"repeat":repeat,"adaptive":adaptive,"host_seconds":elapsed,
                        "fixture_fnv64":fixture_hash,"executable_fnv64":executable_hash,
                        "guest_seconds":guest_seconds,"host_seconds_per_guest_second":elapsed/guest_seconds,
                        "warmup_guest_seconds":5,"scroll":scroll,"measured_initial":measured_initial,
                        "initial":initial,"final":summary(&s),"pcm_frames":pcm,"timeline":timeline,
                        "dma_checks":after.block_irq_deadlines-before.block_irq_deadlines,
                        "dma_misses":after.block_irq_deadline_misses-before.block_irq_deadline_misses});
                    println!("{row}");
                    results.push(row);
                    screenshot(
                        &s,
                        &path.with_extension(if scroll {
                            "scroll-final.png"
                        } else {
                            "bench-final.png"
                        }),
                    )?;
                }
            }
            fs::write(
                path.with_extension(if scroll {
                    "scroll.bench.json"
                } else {
                    "bench.json"
                }),
                serde_json::to_vec_pretty(&results)?,
            )?;
        }
        _ => {
            return Err(concat!(
                "usage: heroes_states capture-menu EXE ISO OUTPUT.pb.gz | ",
                "play STATE ACTIONS.json OUTPUT.pb.gz | verify STATE | ",
                "bench STATE SECONDS [REPEATS] [scroll]"
            )
            .into());
        }
    }
    Ok(())
}
