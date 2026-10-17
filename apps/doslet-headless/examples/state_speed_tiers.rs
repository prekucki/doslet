#[allow(dead_code)]
#[path = "../../common/speed_tiers.rs"]
mod speed_tiers;
#[path = "../../common/state_input.rs"]
mod state_input;
use speed_tiers::{Profile, Tier, charge_cpu_time};
// Offline speed-tier experiment: measured execution time for MAX, fixed
// nominal rates for cold/grey PCs. Probes/serialization are outside MAX timing.
#[path = "support/state_probes.rs"]
mod state_probes;
use doslet_runtime::{Session, SessionStatus};
use doslet_x86::Gpr32;
use serde_json::json;
use std::{error::Error, fs, io::Read, path::Path, time::Instant};

const GENERAL: [Gpr32; 8] = [
    Gpr32::Eax,
    Gpr32::Ecx,
    Gpr32::Edx,
    Gpr32::Ebx,
    Gpr32::Esp,
    Gpr32::Ebp,
    Gpr32::Esi,
    Gpr32::Edi,
];

#[derive(Default)]
struct Audio {
    bytes: Vec<u8>,
    frames: u64,
    nonzero: u64,
    end: Option<u64>,
    gaps: u64,
    overlaps: u64,
}
impl Audio {
    fn drain(&mut self, s: &mut Session) {
        let mut scratch = [doslet_pcm::EMPTY; 1024];
        loop {
            let n = s.drain_pcm_timed(&mut scratch);
            if n == 0 {
                break;
            }
            for f in &scratch[..n] {
                if let Some(end) = self.end {
                    self.gaps += u64::from(f.nanos > end + 1);
                    self.overlaps += u64::from(f.nanos + 1 < end);
                }
                self.end = Some(f.nanos + u64::from(f.duration));
                self.nonzero += u64::from(f.samples != [0; 2]);
                self.frames += 1;
                for sample in f.samples {
                    self.bytes.extend_from_slice(&sample.to_le_bytes());
                }
            }
        }
        let mut midi = [0; 1024];
        while s.drain_midi(&mut midi) != 0 {}
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args: Vec<_> = std::env::args().collect();
    let loader = state_input::Loader::arguments(&mut args)?;
    if args.len() != 8 && args.len() != 9 {
        return Err("usage: state_speed_tiers STATE OUT PROFILE PROBES SLOW_MHZ GUEST_MS FAST(max|1..1000) [jit|interpreter]".into());
    }
    let slow_mhz: u64 = args[5].parse()?;
    let guest_ms: u64 = args[6].parse()?;
    let fast_mhz: Option<u64> = if args[7] == "max" {
        None
    } else {
        Some(args[7].parse()?)
    };
    let jit = match args.get(8).map(String::as_str) {
        None | Some("jit") => true,
        Some("interpreter") => false,
        _ => return Err("engine must be jit or interpreter".into()),
    };
    if !(1..=1000).contains(&slow_mhz)
        || !(1..=10000).contains(&guest_ms)
        || fast_mhz.is_some_and(|m| !(1..=1000).contains(&m))
    {
        return Err("invalid time/rate bound".into());
    }
    let out = Path::new(&args[2]);
    fs::create_dir_all(out)?;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(&args[1])?)
        .take(1024 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 * 1024 {
        return Err("state exceeds 1 GiB".into());
    }
    let mut s = loader.restore(&bytes)?;
    drop(bytes);
    s.debug_disable_accelerations();
    s.set_dynamic_core(jit);
    let mut profile = Profile::load(Path::new(&args[3]), Some(Path::new(&args[4])))?;
    if !profile.irq_grey {
        return Err("regenerate the profile with irq_policy=static-grey".into());
    }
    let mut probes = state_probes::Probes::load(Path::new(&args[4]), &s)?;
    let mut discarded = Audio::default();
    discarded.drain(&mut s);
    drop(discarded);
    let initial_audio = s.audio_status();
    let start_ns = s.emulated_nanos();
    let end_ns = start_ns + guest_ms * 1_000_000;
    let mut audio = Audio::default();
    let mut fast_steps = 0_u64;
    let mut slow_steps = 0_u64;
    let mut fast_cpu_ns = 0_u64;
    let mut slow_cpu_ns = 0_u64;
    let mut fast_charge = 0_u64;
    let mut slow_charge = 0_u64;
    let mut fast_fraction = 0_u64;
    let mut slow_fraction = 0_u64;
    let mut grey_steps = 0_u64;
    let mut grey_cpu_ns = 0_u64;
    let mut grey_charge = 0_u64;
    let mut grey_fraction = 0_u64;
    let mut max_cpu_batch_ns = 0_u64;
    let mut long_batches = 0_u64;
    let mut invalidations = 0_u64;
    let mut batches = 0_u64;
    let mut irq_entries = 0_u64;
    let mut next_drain = start_ns + 1_000_000;
    let mut next_compile = start_ns;
    let mut time_truncated = 0_u64;
    let start_wall = Instant::now();
    while s.emulated_nanos() < end_ns {
        if !s.a20_enabled() || s.control_registers().cr0 & (1 << 31) != 0 {
            return Err("experiment requires A20 and no paging".into());
        }
        let before_ns = s.emulated_nanos();
        let pc = s.debug_pc();
        let registers = GENERAL.map(|g| s.registers().get32(g));
        let irq_before = s.execution_stats().irq_entries;
        let pending = s.debug_irq_pending();
        let mut plan = profile.plan(&s);
        invalidations += u64::from(plan.invalid);
        if pending {
            plan.steps = 1;
            plan.tier = Tier::Grey;
        }
        if plan.tier != Tier::Fast {
            // Keep the slower batch below known interrupt release boundaries.
            if let Some(horizon) = s.debug_service_horizon() {
                let rate = if matches!(plan.tier, Tier::Grey | Tier::Irq) {
                    133
                } else {
                    slow_mhz
                };
                plan.steps = plan.steps.min((horizon * rate / 4000).clamp(1, 64) as u32);
            }
        }
        // Only execution enters MAX accounting. Code checks, probes, PNG hashes
        // and compilation do not turn instrumentation overhead into guest work.
        let cpu_start = Instant::now();
        let completed = s.debug_external_advance(plan.steps);
        let cpu_ns = u64::try_from(cpu_start.elapsed().as_nanos())?.max(1);
        if completed == 0 || s.status() != SessionStatus::Running {
            return Err(format!("execution stopped: {:?}", s.fault()).into());
        }
        max_cpu_batch_ns = max_cpu_batch_ns.max(cpu_ns);
        long_batches += u64::from(cpu_ns > 100_000);
        batches += 1;
        let irq = s.execution_stats().irq_entries != irq_before;
        if irq {
            if completed != 1 {
                return Err("IRQ entered inside a multi-step diagnostic batch".into());
            }
            irq_entries += 1;
        }
        let (rate, fraction) = match plan.tier {
            Tier::Slow => (Some(slow_mhz), &mut slow_fraction),
            Tier::Grey | Tier::Irq => (Some(133), &mut grey_fraction),
            Tier::Fast => (fast_mhz, &mut fast_fraction),
        };
        // Grey is the explicitly requested fixed 133-MHz emulated reference,
        // including when diagnostic dispatch cannot sustain it in wall time.
        // MAX and cold retain the measured host limit. This is an offline
        // scheduling experiment, not a claim of real-time host throughput.
        let charged = charge_cpu_time(
            rate,
            completed,
            cpu_ns,
            fraction,
            fast_mhz.is_none() && !matches!(plan.tier, Tier::Grey | Tier::Irq),
        );
        let (steps, cost, time) = match plan.tier {
            Tier::Slow => (&mut slow_steps, &mut slow_cpu_ns, &mut slow_charge),
            Tier::Grey | Tier::Irq => (&mut grey_steps, &mut grey_cpu_ns, &mut grey_charge),
            Tier::Fast => (&mut fast_steps, &mut fast_cpu_ns, &mut fast_charge),
        };
        *steps += u64::from(completed);
        *cost += cpu_ns;
        *time += charged;
        let mut remaining = charged.min(end_ns - s.emulated_nanos());
        time_truncated += charged - remaining;
        while remaining > 0 {
            let elapsed = s.debug_advance_device_time(
                remaining.min(next_drain.saturating_sub(s.emulated_nanos()).max(1)),
            )?;
            if elapsed == 0 {
                return Err("device clock stopped".into());
            }
            remaining -= elapsed;
            if s.emulated_nanos() >= next_drain {
                audio.drain(&mut s);
                next_drain += 1_000_000;
            }
        }
        if completed == 1 {
            probes.after(&s, pc, registers, before_ns, irq)?;
        } else {
            probes.hardware_after(&s, before_ns)?;
        }
        if jit && s.emulated_nanos() >= next_compile {
            s.prepare_dynamic(8);
            next_compile = s.emulated_nanos() + 10_000_000;
        }
        if batches.is_multiple_of(100_000) && start_wall.elapsed().as_secs() > 180 {
            return Err("experiment exceeded 180 host seconds".into());
        }
    }
    audio.drain(&mut s);
    probes.write(&out.join("probes.json"), start_ns)?;
    fs::write(out.join("audio.s16le"), &audio.bytes)?;
    let f = s.presentation_frame();
    let mut rgba = vec![0; f.width() * f.height() * 4];
    f.write_rgba(&mut rgba).map_err(|e| format!("{e:?}"))?;
    let mut encoder = png::Encoder::new(
        fs::File::create(out.join("final.png"))?,
        f.width() as u32,
        f.height() as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    let a = s.audio_status();
    let metrics = json!({"experiment":"speed-tiers","fast":args[7],"slow_mhz":slow_mhz,"grey_mhz":133,
        "grey_clock":"fixed nominal, independent of host cost",
        "profile":args[3],"unknown":profile.unknown.name(),
        "classified_slow_nodes":profile.nodes.values().filter(|n| n.tier == Tier::Slow).count(),
        "classified_grey_nodes":profile.nodes.values().filter(|n| n.tier == Tier::Grey).count(),
        "mhz":s.hardware_config().cpu_mhz,"mhz_is_metadata_only":true,"jit":jit,
        "requested_guest_ms":guest_ms,"guest_ns":s.emulated_nanos()-start_ns,"duration_complete":true,
        "executed_scheduler_steps":fast_steps+slow_steps+grey_steps,"fast_steps":fast_steps,"slow_steps":slow_steps,
        "grey_steps":grey_steps,"grey_cpu_ns":grey_cpu_ns,"grey_charged_ns":grey_charge,
        "fast_cpu_ns":fast_cpu_ns,"slow_cpu_ns":slow_cpu_ns,
        "fast_charged_ns":fast_charge,"slow_charged_ns":slow_charge,
        "fast_measured_equivalent_mhz":4000.0*fast_steps as f64/fast_cpu_ns.max(1) as f64,
        "max_cpu_batch_ns":max_cpu_batch_ns,"batches_over_100us":long_batches,"batches":batches,
        "irq_entries":irq_entries,"code_invalidations":invalidations,"irq_policy":"static-grey",
        "end_time_truncated_ns":time_truncated,"instrumented_host_seconds":start_wall.elapsed().as_secs_f64(),
        "pcm_frames":audio.frames,"pcm_generated":a.pcm_frames-initial_audio.pcm_frames,
        "pcm_nonzero":audio.nonzero,"pcm_gaps":audio.gaps,"pcm_overlaps":audio.overlaps,
        "dma_checks":a.dsp.block_irq_deadlines-initial_audio.dsp.block_irq_deadlines,
        "dma_misses":a.dsp.block_irq_deadline_misses-initial_audio.dsp.block_irq_deadline_misses});
    fs::write(
        out.join("metrics.json"),
        serde_json::to_vec_pretty(&metrics)?,
    )?;
    println!("{metrics}");
    Ok(())
}
