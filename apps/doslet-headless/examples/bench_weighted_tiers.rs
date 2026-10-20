//! Same portable state, ordinary/weighted clocks, no instruction profiler or audio device.
#[allow(dead_code)]
#[path = "../../doslet/src/cpu_config.rs"]
mod cpu_config;
#[allow(dead_code)]
#[path = "../../common/host_dirs.rs"]
mod host_dirs;
#[allow(dead_code)]
#[path = "../../doslet/src/native_speed_tiers.rs"]
mod native_speed_tiers;
#[allow(dead_code)]
#[path = "../../common/speed_tiers.rs"]
mod speed_tiers;
#[path = "../../common/state_input.rs"]
mod state_input;
use doslet_runtime::SessionStatus;
#[cfg(test)]
use state_input::native_input;
use std::{error::Error, fs, io::Read, path::Path, time::Instant};
fn hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100000001b3)
    })
}
fn main() -> Result<(), Box<dyn Error>> {
    let mut a: Vec<_> = std::env::args().collect();
    let loader = state_input::Loader::arguments(&mut a)?;
    if a.len() != 9 && a.len() != 10 {
        return Err("usage: bench_weighted_tiers STATE PROFILE|- GUEST_MS HOT|max GREY COLD jit|interpreter OUT [STEPS]".into());
    }
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(&a[1])?)
        .take(1024 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 * 1024 {
        return Err("state too large".into());
    }
    let mut s = loader.restore(&bytes)?;
    drop(bytes);
    let jit = match a[7].as_str() {
        "jit" => true,
        "interpreter" => false,
        _ => return Err("invalid engine".into()),
    };
    s.set_dynamic_core(jit);
    s.set_synchronization_analysis(false);
    s.set_adaptive_idle(false);
    let grey = a[5].parse()?;
    s.set_cpu_mhz(grey)?;
    let mut controller = if a[2] == "-" {
        None
    } else {
        Some(native_speed_tiers::Controller::load(
            Path::new(&a[2]),
            a[6].parse()?,
            if a[4] == "max" {
                None
            } else {
                Some(a[4].parse()?)
            },
            grey,
        )?)
    };
    if let Some(c) = &mut controller {
        c.enable(&mut s);
    }
    s.start();
    let before = s.emulated_nanos();
    let steps = s.steps();
    let end = before + a[3].parse::<u64>()? * 1_000_000;
    let max_steps = a.get(9).map_or(Ok(u64::MAX), |v| v.parse())?;
    let initial_audio = s.audio_status();
    let mut pcm = [[0; 2]; 4096];
    let mut midi = [0; 4096];
    while s.drain_pcm(&mut pcm) != 0 {}
    while s.drain_midi(&mut midi) != 0 {}
    let mut pcm_hash = 0xcbf29ce484222325u64;
    let mut frames = 0u64;
    let start = Instant::now();
    let mut compile_at = before;
    let mut compiler_ns = 0u128;
    while s.emulated_nanos() < end
        && s.steps() - steps < max_steps
        && s.status() == SessionStatus::Running
    {
        let budget = (max_steps - (s.steps() - steps)).min(4096) as u32;
        let n = if let Some(c) = &mut controller {
            c.advance(&mut s, budget, end)?
        } else {
            s.advance(budget)
        };
        if n == 0 && controller.as_ref().is_none_or(|c| !c.has_pending_time()) {
            return Err(format!("no progress: {:?}", s.fault()).into());
        }
        loop {
            let n = s.drain_pcm(&mut pcm);
            if n == 0 {
                break;
            }
            frames += n as u64;
            for frame in &pcm[..n] {
                for sample in frame {
                    for b in sample.to_le_bytes() {
                        pcm_hash = (pcm_hash ^ u64::from(b)).wrapping_mul(0x100000001b3);
                    }
                }
            }
        }
        while s.drain_midi(&mut midi) != 0 {}
        if jit && s.emulated_nanos() >= compile_at {
            let t = Instant::now();
            s.prepare_dynamic(8);
            compiler_ns += t.elapsed().as_nanos();
            compile_at = s.emulated_nanos() + 10_000_000;
        }
        if start.elapsed().as_secs() > 120 {
            return Err("120s host limit".into());
        }
    }
    if s.status() != SessionStatus::Running {
        return Err(format!("{:?}", s.fault()).into());
    }
    let host = start.elapsed().as_secs_f64();
    let audio = s.audio_status();
    let r = s.registers();
    let result = serde_json::json!({"state":a[1],"profile":a[2],"jit":jit,"hot":a[4],"grey":grey,"cold":a[6],
        "host_seconds":host,"compiler_seconds":compiler_ns as f64/1e9,"guest_ns":s.emulated_nanos()-before,"steps":s.steps()-steps,
        "host_per_guest":host / ((s.emulated_nanos()-before) as f64/1e9),"tiers":s.execution_tier_stats(),
        "idle_skipped":s.idle_loop_stats().skipped_steps,"jit_steps":s.dynamic_stats().executed_steps,
        "pcm_frames":frames,"pcm_hash":pcm_hash,"dma_misses":audio.dsp.block_irq_deadline_misses-initial_audio.dsp.block_irq_deadline_misses,
        "dma_checks":audio.dsp.block_irq_deadlines-initial_audio.dsp.block_irq_deadlines,
        "pc":s.debug_pc(),"flags":s.eflags(),"registers":([doslet_x86::Gpr32::Eax,doslet_x86::Gpr32::Ecx,doslet_x86::Gpr32::Edx,doslet_x86::Gpr32::Ebx,doslet_x86::Gpr32::Esp,doslet_x86::Gpr32::Ebp,doslet_x86::Gpr32::Esi,doslet_x86::Gpr32::Edi].map(|g|r.get32(g))),
        "ram":hash(s.ram_view(0..0xA0000).unwrap()),"extended":hash(s.ram_view(0x100000..s.hardware_config().memory_bytes()).unwrap()),"vram":hash(s.vram_view())});
    fs::write(&a[8], serde_json::to_vec_pretty(&result)?)?;
    let frame = s.presentation_frame();
    let mut rgba = vec![0; frame.width() * frame.height() * 4];
    frame.write_rgba(&mut rgba).map_err(|e| format!("{e:?}"))?;
    let mut encoder = png::Encoder::new(
        fs::File::create(format!("{}.png", a[8]))?,
        frame.width() as u32,
        frame.height() as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    println!("{result}");
    Ok(())
}
