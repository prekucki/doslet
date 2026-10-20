//! Offline evidence for a speculative graph. Never used in the execution hot path.
#[path = "../../common/state_input.rs"]
mod state_input;
#[path = "support/state_probes.rs"]
mod state_probes;
#[path = "support/state_wait.rs"]
mod state_wait;
#[path = "../src/wait_trace.rs"]
mod wait_trace;

use doslet_runtime::{Session, SessionStatus};
use doslet_x86::{Gpr32, SegmentRegister};
use serde_json::json;
use std::{collections::BTreeMap, error::Error, fs, io::Read, path::Path, time::Instant};

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
struct Node {
    count: u64,
    bytes: [u8; 15],
    changed: bool,
    first: [u32; 8],
    segments: [u32; 4],
    successors: BTreeMap<u32, u64>,
}

#[derive(Default)]
struct Audio {
    frames: u64,
    nonzero: u64,
    peak_queue: u64,
    last_end: Option<u64>,
    gaps: u64,
    max_gap_ns: u64,
    reversed: u64,
    block_hash: u64,
    last_block_hash: Option<u64>,
    repeated_blocks: u64,
    samples: Vec<u8>,
}

impl Audio {
    fn drain(&mut self, s: &mut Session, scratch: &mut [doslet_pcm::Frame], generated: u64) {
        self.peak_queue = self
            .peak_queue
            .max(s.audio_status().pcm_frames - generated - self.frames);
        loop {
            let n = s.drain_pcm_timed(scratch);
            if n == 0 {
                break;
            }
            for f in &scratch[..n] {
                if let Some(end) = self.last_end {
                    let gap = f.nanos.saturating_sub(end);
                    if gap > 1 {
                        self.gaps += 1;
                        self.max_gap_ns = self.max_gap_ns.max(gap);
                    }
                    if f.nanos + 1 < end {
                        self.reversed += 1;
                    }
                }
                self.last_end = Some(f.nanos + u64::from(f.duration));
                self.frames += 1;
                self.nonzero += u64::from(f.samples != [0; 2]);
                for sample in f.samples {
                    for byte in sample.to_le_bytes() {
                        self.samples.push(byte);
                        self.block_hash =
                            (self.block_hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
                    }
                }
                if self.frames.is_multiple_of(1024) {
                    self.repeated_blocks +=
                        u64::from(self.last_block_hash == Some(self.block_hash));
                    self.last_block_hash = Some(self.block_hash);
                    self.block_hash = 0;
                }
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args: Vec<_> = std::env::args().collect();
    let loader = state_input::Loader::arguments(&mut args)?;
    if args.len() != 4 && !(6..=8).contains(&args.len()) {
        return Err(
            "usage: state_graph_trace STATE.pb.gz OUTPUT_DIRECTORY MAX_STEPS [MHZ GUEST_MS [PROBES.json [WAIT.json]]]".into(),
        );
    }
    let steps: u64 = args[3].parse()?;
    if steps > 2_500_000_000 {
        return Err("trace limit: 2.5 billion scheduler steps".into());
    }
    let out = Path::new(&args[2]);
    fs::create_dir_all(out)?;
    let mut bytes = Vec::new();
    flate2::read::GzDecoder::new(fs::File::open(&args[1])?)
        .take(1024 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 * 1024 {
        return Err("snapshot exceeds 1 GiB".into());
    }
    let mut s = loader.restore(&bytes)?;
    drop(bytes);
    s.debug_disable_accelerations();
    let guest_ms: u64 = if args.len() >= 6 {
        s.set_cpu_mhz(args[4].parse()?)?;
        args[5].parse()?
    } else {
        0
    };
    if guest_ms > 10000 {
        return Err("guest duration limit: 10 seconds".into());
    }
    // Separate mapped video from ordinary RAM. This audit exports the first 16 MiB.
    for (name, range) in [
        ("low.bin", 0..0xa0000),
        ("extended.bin", 0x100000..0x1000000),
    ] {
        fs::write(
            out.join(name),
            s.ram_view(range).ok_or("expected 16 MiB RAM")?,
        )?;
    }
    let start_ns = s.emulated_nanos();
    let start_steps = s.steps();
    let start_pc = s.debug_pc();
    let mut probes = args
        .get(6)
        .map(|path| state_probes::Probes::load(Path::new(path), &s))
        .transpose()?;
    let before_audio = s.audio_status();
    let mut waiting = args
        .get(7)
        .map(|path| state_wait::Experiment::load(Path::new(path), &s))
        .transpose()?;
    let mut executed_steps = 0_u64;
    let deadline = start_ns
        + if guest_ms == 0 {
            10_000_000_000
        } else {
            guest_ms * 1_000_000
        };
    let mut nodes = BTreeMap::<(u32, bool), Node>::new();
    let mut irqs = BTreeMap::<u32, u64>::new();
    let mut trace = wait_trace::WaitTrace::default();
    let mut pcm = [doslet_pcm::EMPTY; 1024];
    let mut midi = [0; 1024];
    // Discard the snapshot's already-produced output before measuring new frames.
    while s.drain_pcm_timed(&mut pcm) != 0 {}
    while s.drain_midi(&mut midi) != 0 {}
    let mut audio = Audio::default();
    let mut next_drain = start_ns + 1_000_000;
    let wall_start = Instant::now();
    let mut stalled = 0_u64;
    for _ in 0..steps {
        if s.emulated_nanos() >= deadline {
            break;
        }
        if !s.a20_enabled() || s.control_registers().cr0 & (1 << 31) != 0 {
            return Err("this audit requires A20 enabled and paging disabled".into());
        }
        let r = s.registers();
        let before_ns = s.emulated_nanos();
        let cs = r.segment_cache(SegmentRegister::Cs);
        let pc = s.debug_pc();
        let first = GENERAL.map(|g| r.get32(g));
        let segments = [
            SegmentRegister::Cs,
            SegmentRegister::Ds,
            SegmentRegister::Es,
            SegmentRegister::Ss,
        ]
        .map(|seg| r.segment_cache(seg).base);
        let mut code = [0; 15];
        if !s.debug_read_memory(pc, &mut code) {
            return Err("code not in readable RAM".into());
        }
        let before_irq = s.execution_stats().irq_entries;
        if let Some(waiting) = &mut waiting {
            waiting.before(&s, pc, first[4]);
        }
        let before = trace.before(&s);
        if s.advance(1) == 0 {
            if s.status() != SessionStatus::Running {
                return Err(format!("guest stopped: {:?}", s.fault()).into());
            }
            stalled += 1;
            audio.drain(&mut s, &mut pcm, before_audio.pcm_frames);
            if stalled > 1000 {
                return Err("guest stopped making progress".into());
            }
            continue;
        }
        trace.after(&s, before);
        executed_steps += 1;
        if s.status() != SessionStatus::Running {
            return Err(format!("guest stopped: {:?}", s.fault()).into());
        }
        let next = s.debug_pc();
        if let Some(probes) = &mut probes {
            probes.after(
                &s,
                pc,
                first,
                before_ns,
                before_irq != s.execution_stats().irq_entries,
            )?;
        }
        if irqs.len() >= 1024
            && !irqs.contains_key(&next)
            && before_irq != s.execution_stats().irq_entries
        {
            return Err("IRQ target bound exceeded".into());
        }
        if before_irq != s.execution_stats().irq_entries {
            *irqs.entry(next).or_default() += 1;
        } else {
            if nodes.len() >= 100_000 && !nodes.contains_key(&(pc, cs.default32)) {
                return Err("trace node bound exceeded".into());
            }
            let n = nodes.entry((pc, cs.default32)).or_default();
            if n.count == 0 {
                n.bytes = code;
                n.first = first;
                n.segments = segments;
            }
            n.changed |= n.bytes != code;
            n.count += 1;
            if n.successors.len() >= 1024 && !n.successors.contains_key(&next) {
                return Err("successor bound exceeded".into());
            }
            *n.successors.entry(next).or_default() += 1;
        }
        if let Some(waiting) = &mut waiting {
            let request = waiting
                .after(&s, pc, code, before_irq != s.execution_stats().irq_entries)
                .min(deadline.saturating_sub(s.emulated_nanos()))
                .min(next_drain.saturating_sub(s.emulated_nanos()));
            if request > 0 {
                let before_idle = s.emulated_nanos();
                let actual = s.debug_cooperative_idle(request)?;
                waiting.waited(actual);
                if let Some(probes) = &mut probes {
                    probes.hardware_after(&s, before_idle)?;
                }
            }
        }
        if s.emulated_nanos() >= next_drain {
            audio.drain(&mut s, &mut pcm, before_audio.pcm_frames);
            while s.drain_midi(&mut midi) != 0 {}
            next_drain += 1_000_000;
            if wall_start.elapsed().as_secs() > 600 {
                return Err("host audit deadline: 10 minutes".into());
            }
        }
    }
    audio.drain(&mut s, &mut pcm, before_audio.pcm_frames);
    let wall_seconds = wall_start.elapsed().as_secs_f64();
    let after_audio = s.audio_status();
    if let Some(probes) = &probes {
        probes.write(&out.join("probes.json"), start_ns)?;
    }
    fs::write(out.join("audio.s16le"), &audio.samples)?;
    let frame = s.presentation_frame();
    let mut rgba = vec![0; frame.width() * frame.height() * 4];
    frame.write_rgba(&mut rgba).map_err(|e| format!("{e:?}"))?;
    let mut encoder = png::Encoder::new(
        fs::File::create(out.join("final.png"))?,
        frame.width() as u32,
        frame.height() as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header()?.write_image_data(&rgba)?;
    let hash = |data: &[u8]| {
        data.iter().fold(0xcbf29ce484222325_u64, |h, byte| {
            (h ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
        })
    };
    let metrics = json!({
        "mhz":s.hardware_config().cpu_mhz, "requested_guest_ms":guest_ms,
        "guest_ns":s.emulated_nanos()-start_ns, "steps":s.steps()-start_steps,
        "executed_scheduler_steps":executed_steps,
        "wait_regulator":waiting.as_ref().map(state_wait::Experiment::summary),
        "duration_complete":guest_ms==0 || s.emulated_nanos()>=deadline,
        "instrumented_host_seconds":wall_seconds, "stalled_advances":stalled,
        "pcm_frames":audio.frames, "pcm_generated":after_audio.pcm_frames-before_audio.pcm_frames,
        "pcm_nonzero":audio.nonzero, "pcm_queue_peak":audio.peak_queue,
        "pcm_gaps":audio.gaps,"pcm_max_gap_ns":audio.max_gap_ns,
        "pcm_overlaps":audio.reversed,"pcm_repeated_1024_frame_blocks":audio.repeated_blocks,
        "initial_rate":before_audio.rate,"final_rate":after_audio.rate,
        "dma_checks":after_audio.dsp.block_irq_deadlines-before_audio.dsp.block_irq_deadlines,
        "dma_misses":after_audio.dsp.block_irq_deadline_misses-before_audio.dsp.block_irq_deadline_misses,
        "dma_units":after_audio.dsp.dma_units-before_audio.dsp.dma_units,
        "vram_fnv64":format!("{:016x}",hash(s.vram_view())),
        "rgba_fnv64":format!("{:016x}",hash(&rgba)),
    });
    fs::write(
        out.join("metrics.json"),
        serde_json::to_vec_pretty(&metrics)?,
    )?;
    println!("{metrics}");
    trace.write(&mut fs::File::create(out.join("wait-trace.json"))?)?;
    fs::write(
        out.join("trace.json"),
        serde_json::to_vec(&json!({
            "state":args[1], "start_pc":start_pc, "start_ns":start_ns,
            "end_ns":s.emulated_nanos(), "steps":s.steps()-start_steps,
            "executed_scheduler_steps":executed_steps,
            "irq_entries":irqs, "register_order":["eax","ecx","edx","ebx","esp","ebp","esi","edi"],
            "segment_order":["cs","ds","es","ss"],
            "nodes":nodes.iter().map(|((pc,wide),n)|json!({
                "pc":pc,"bits":if *wide {32} else {16}, "count":n.count,
                "bytes":n.bytes,"changed":n.changed,"first_registers":n.first,
                "segment_bases":n.segments,"successors":n.successors
            })).collect::<Vec<_>>()
        }))?,
    )?;
    println!(
        "{} nodes, {} scheduler steps, {} guest ns",
        nodes.len(),
        s.steps() - start_steps,
        s.emulated_nanos() - start_ns
    );
    Ok(())
}
