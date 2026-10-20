//! Bounded local-game recurrence evidence and proof-based idle A/B comparison.
//! This diagnostic never authorizes omission from CFG/profile labels alone.
#[cfg(all(
    feature = "wasm-static-test",
    feature = "save-state",
    feature = "profiling"
))]
#[allow(dead_code)]
#[path = "../../doslet/src/archive.rs"]
mod archive;
#[cfg(all(
    feature = "wasm-static-test",
    feature = "save-state",
    feature = "profiling"
))]
#[allow(dead_code)]
#[path = "../../common/machine_profile.rs"]
mod machine_profile;

#[cfg(all(
    feature = "wasm-static-test",
    feature = "save-state",
    feature = "profiling"
))]
mod diagnostic {
    use doslet_aot::{
        DefaultBitness, ImageLimits, ObjectAddress, SymbolicImage,
        cfg::{CfgLimits, EdgeKind, EdgeTarget, StaticCfg},
    };
    use doslet_config::{SourceKind, WritePolicy};
    use doslet_executable::{LeImage, LeLimits};
    use doslet_runtime::{Session, SessionStatus};
    use doslet_x86::analysis::{self, Effect, Flow, Operand};
    use serde_json::{Value, json};
    use std::{
        collections::BTreeMap, error::Error, fs, io::Read, path::Path, sync::Arc, time::Instant,
    };

    const LIMIT: u64 = 50_000_000;
    const FNV: u64 = 0xcbf29ce484222325;
    fn hash(mut h: u64, bytes: &[u8]) -> u64 {
        for &b in bytes {
            h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
        }
        h
    }
    fn read(path: &Path, limit: usize) -> Result<Vec<u8>, Box<dyn Error>> {
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err("input exceeds diagnostic limit".into());
        }
        Ok(bytes)
    }
    #[derive(Default)]
    struct Audio {
        pcm: u64,
        midi: u64,
        frames: u64,
        midi_bytes: u64,
    }
    impl Audio {
        fn new() -> Self {
            Self {
                pcm: FNV,
                midi: FNV,
                ..Self::default()
            }
        }
        fn drain(&mut self, s: &mut Session) -> Result<(), Box<dyn Error>> {
            let mut pcm = [doslet_pcm::EMPTY; 1024];
            let mut midi = [0; 4096];
            // Every execution sub-batch is bounded to 4096 steps. A fixed
            // output-drain bound also protects diagnostics from API regressions.
            for i in 0..4096 {
                let n = s.drain_pcm_timed(&mut pcm);
                if n == 0 {
                    break;
                }
                if i == 4095 {
                    return Err("PCM drain bound".into());
                }
                self.frames += n as u64;
                for f in &pcm[..n] {
                    self.pcm = hash(self.pcm, &f.nanos.to_le_bytes());
                    self.pcm = hash(self.pcm, &f.duration.to_le_bytes());
                    for sample in f.samples {
                        self.pcm = hash(self.pcm, &sample.to_le_bytes());
                    }
                }
            }
            for i in 0..4096 {
                let n = s.drain_midi(&mut midi);
                if n == 0 {
                    break;
                }
                if i == 4095 {
                    return Err("MIDI drain bound".into());
                }
                self.midi_bytes += n as u64;
                self.midi = hash(self.midi, &midi[..n]);
            }
            Ok(())
        }
        fn json(&self) -> Value {
            json!({"pcm_timed_fnv64":format!("{:016x}",self.pcm), "pcm_frames":self.frames, "midi_fnv64":format!("{:016x}",self.midi), "midi_bytes":self.midi_bytes})
        }
    }
    fn setup(s: &mut Session, idle: bool) {
        s.set_dynamic_core(false);
        s.set_decode_cache(false);
        s.set_idle_loops(false);
        s.set_vga_wait_acceleration(false);
        s.set_synchronization_analysis(false);
        s.set_adaptive_idle(false);
        s.set_idle_observation(false);
        s.set_idle_loops(idle);
        s.start();
    }
    fn advance(s: &mut Session, steps: u64, audio: &mut Audio) -> Result<(), Box<dyn Error>> {
        let end = s.steps().checked_add(steps).ok_or("step overflow")?;
        let wall = Instant::now();
        while s.steps() < end {
            let n = s.advance((end - s.steps()).min(4096) as u32);
            audio.drain(s)?;
            if n == 0 || s.status() != SessionStatus::Running {
                return Err(format!("no progress: {:?} {:?}", s.status(), s.fault()).into());
            }
            if wall.elapsed().as_secs() > 120 {
                return Err("120s host limit".into());
            }
        }
        Ok(())
    }
    fn state(s: &Session) -> Result<Value, Box<dyn Error>> {
        let frame = s.presentation_frame();
        let mut rgba = vec![0; frame.width() * frame.height() * 4];
        frame.write_rgba(&mut rgba).map_err(|e| format!("{e:?}"))?;
        Ok(
            json!({"steps":s.steps(), "guest_ns":s.emulated_nanos(), "pc":s.debug_pc(), "cpu_mhz":s.hardware_config().cpu_mhz,
            "ram_low_fnv64":format!("{:016x}",hash(FNV,s.ram_view(0..0xa0000).ok_or("low RAM unavailable")?)),
            "ram_high_fnv64":format!("{:016x}",hash(FNV,s.ram_view(0xc0000..s.hardware_config().memory_bytes()).ok_or("high RAM unavailable")?)),
            "vram_fnv64":format!("{:016x}",hash(FNV,s.vram_view())), "rgba_fnv64":format!("{:016x}",hash(FNV,&rgba)),
            "audio_status":format!("{:?}",s.audio_status()), "execution_stats":format!("{:?}",s.execution_stats())}),
        )
    }
    #[derive(Default)]
    struct Node {
        count: u64,
        effect: Option<analysis::Decoded>,
        changed: bool,
        successors: BTreeMap<u32, u64>,
    }
    fn location(pc: u32, bases: &[(u32, u32, u32)]) -> Value {
        bases
            .iter()
            .find_map(|&(object, base, size)| {
                pc.checked_sub(base)
                    .filter(|&offset| offset < size)
                    .map(|offset| json!({"object":object,"offset":offset,"linear":pc}))
            })
            .unwrap_or_else(|| json!({"linear":pc}))
    }
    fn operand(o: Operand, s: &Session, width: u8, bases: &[(u32, u32, u32)]) -> Value {
        match o {
            Operand::Absolute { segment, address } => {
                let linear = s
                    .registers()
                    .segment_cache(segment)
                    .base
                    .checked_add(address);
                let value = linear
                    .and_then(|linear| {
                        s.ram_view(linear as usize..linear as usize + usize::from(width))
                    })
                    .map(|b| {
                        b.iter()
                            .enumerate()
                            .fold(0u32, |a, (i, &b)| a | (u32::from(b) << (i * 8)))
                    });
                json!({"memory":linear.map(|p|location(p,bases)),"segment":format!("{segment:?}"),"width":width,"final_value":value})
            }
            _ => json!({"operand":format!("{o:?}")}),
        }
    }
    fn effect(d: Option<analysis::Decoded>, s: &Session, bases: &[(u32, u32, u32)]) -> Value {
        let Some(d) = d else {
            return json!({"unknown":true});
        };
        let detail = match d.effect {
            Effect::Move { target, source } => {
                json!({"move":{"target":operand(target,s,d.width,bases),"source":operand(source,s,d.width,bases)}})
            }
            Effect::Compare { left, right, test } => {
                json!({"compare":{"left":operand(left,s,d.width,bases),"right":operand(right,s,d.width,bases),"test":test}})
            }
            other => json!({"effect":format!("{other:?}")}),
        };
        json!({"length":d.length,"width":d.width,"flow":format!("{:?}",d.flow),"detail":detail})
    }
    fn trace(
        s: &mut Session,
        steps: u64,
        bases: &[(u32, u32, u32)],
    ) -> Result<Value, Box<dyn Error>> {
        let start = s.steps();
        let start_ns = s.emulated_nanos();
        let mut nodes = BTreeMap::<(u32, bool), Node>::new();
        let mut audio = Audio::new();
        let wall = Instant::now();
        let mut stalled = 0;
        let mut iterations = 0u64;
        while s.steps() - start < steps {
            iterations += 1;
            if iterations.is_multiple_of(4096) {
                audio.drain(s)?;
                if wall.elapsed().as_secs() > 120 {
                    return Err("trace 120s limit".into());
                }
            }
            let before = s.steps();
            let Some(i) = s.debug_observed_step() else {
                if s.status() != SessionStatus::Running {
                    return Err(format!("trace stopped: {:?}", s.fault()).into());
                }
                if s.steps() == before {
                    stalled += 1;
                } else {
                    stalled = 0;
                }
                if stalled > 1024 || wall.elapsed().as_secs() > 120 {
                    return Err("trace progress bound".into());
                }
                continue;
            };
            if nodes.len() >= 100_000 && !nodes.contains_key(&(i.linear, i.code32)) {
                return Err("trace node bound".into());
            }
            let decoded = i
                .code32
                .then(|| analysis::decode(&i.bytes[..usize::from(i.available)], i.ip))
                .flatten();
            let n = nodes.entry((i.linear, i.code32)).or_default();
            if n.count == 0 {
                n.effect = decoded;
            } else {
                n.changed |= n.effect != decoded;
            }
            n.count += 1;
            if n.successors.len() >= 1024 {
                return Err("successor bound".into());
            }
            *n.successors.entry(s.debug_pc()).or_default() += 1;
        }
        audio.drain(s)?;
        let mut ranked: Vec<_> = nodes.iter().collect();
        ranked.sort_by_key(|(_, n)| std::cmp::Reverse(n.count));
        let backedges:Vec<_>=nodes.iter().filter_map(|(&(pc,wide),n)|{
            let d=n.effect?;
            let Flow::Branch{target,condition:Some(condition)}=d.flow else{return None;};
            if !wide || target>pc || pc-target>256 || n.count<2 {return None;}
            let taken=n.successors.get(&target).copied().unwrap_or(0);
            Some(json!({"head":location(target,bases),"branch":location(pc,bases),"condition":condition,"executions":n.count,"taken":taken,"fallthrough":n.successors.get(&(pc+u32::from(d.length))).copied().unwrap_or(0),
                "body":nodes.range((target,true)..=(pc,true)).filter(|((_,w),_)|*w).map(|(&(pc,_),n)|json!({"at":location(pc,bases),"executions":n.count,"changed":n.changed,"decoded":effect(n.effect,s,bases)})).collect::<Vec<_>>() }))
        }).collect();
        Ok(
            json!({"steps":s.steps()-start,"guest_ns":s.emulated_nanos()-start_ns,"host_seconds":wall.elapsed().as_secs_f64(),"nodes":nodes.len(),"audio":audio.json(),"short_conditional_backedges":backedges,
            "top_nodes":ranked.iter().take(40).map(|((pc,_),n)|json!({"at":location(*pc,bases),"count":n.count,"changed":n.changed,"decoded":effect(n.effect,s,bases),"successors":n.successors})).collect::<Vec<_>>(),"final":state(s)?}),
        )
    }
    fn static_candidates(
        image: &SymbolicImage,
        s: &Session,
        bases: &[(u32, u32, u32)],
        entries: &[ObjectAddress],
    ) -> Result<Value, Box<dyn Error>> {
        let cfg = StaticCfg::analyze(image, entries, CfgLimits::default())?;
        let mut candidates = Vec::new();
        for edge in cfg.edges() {
            let EdgeTarget::Address(head) = edge.target else {
                continue;
            };
            if edge.kind != EdgeKind::Branch
                || head.object_number != edge.source.object_number
                || head.offset > edge.source.offset
                || edge.source.offset - head.offset > 96
            {
                continue;
            }
            let Some(&(_, base, _)) = bases.iter().find(|&&(n, _, _)| n == head.object_number)
            else {
                continue;
            };
            let mut body = Vec::new();
            let mut loads = 0;
            let mut compares = 0;
            let mut unknown = false;
            for node in cfg.nodes().iter().filter(|n| {
                n.address.object_number == head.object_number
                    && n.address.offset >= head.offset
                    && n.address.offset <= edge.source.offset
            }) {
                if node.bitness != DefaultBitness::Bits32 {
                    unknown = true;
                    break;
                }
                let Some(pc) = base.checked_add(node.address.offset) else {
                    unknown = true;
                    break;
                };
                let d = s
                    .debug_code_bytes(pc, 15)
                    .and_then(|b| analysis::decode(b, pc));
                match d.map(|d| d.effect) {
                    Some(Effect::Move {
                        source: Operand::Absolute { .. },
                        ..
                    }) => loads += 1,
                    Some(Effect::Compare { .. }) => compares += 1,
                    Some(Effect::Branch | Effect::Move { .. } | Effect::RegisterWork) => {}
                    _ => {
                        unknown = true;
                        break;
                    }
                }
                body.push(json!({"at":location(pc,bases),"decoded":effect(d,s,bases)}));
                if body.len() > 32 {
                    unknown = true;
                    break;
                }
            }
            if !unknown && loads > 0 && compares > 0 {
                if candidates.len() >= 512 {
                    return Err("static candidate bound".into());
                }
                candidates.push(json!({"head":location(base+head.offset,bases),"branch":location(base+edge.source.offset,bases),"body":body}));
            }
        }
        let mut trusted = Vec::new();
        for entry in entries {
            let Some(&(_, base, size)) = bases.iter().find(|&&(n, _, _)| n == entry.object_number)
            else {
                continue;
            };
            let mut pc = base + entry.offset;
            let mut body = Vec::new();
            for _ in 0..40 {
                if pc >= base + size {
                    break;
                }
                let d = s
                    .debug_code_bytes(pc, 15)
                    .and_then(|b| analysis::decode(b, pc));
                body.push(json!({"at":location(pc,bases),"decoded":effect(d,s,bases)}));
                let Some(d) = d else {
                    break;
                };
                if matches!(d.flow, Flow::Return | Flow::InterruptReturn) {
                    break;
                }
                pc += u32::from(d.length);
            }
            trusted
                .push(json!({"entry":location(base+entry.offset,bases),"linear_inventory":body}));
        }
        Ok(
            json!({"cfg_nodes":cfg.nodes().len(),"candidates":candidates,"trusted_entry_linear_inventories":trusted,"authority":"Candidate inventory only; current loaded instruction summaries and caller-declared bases are not runtime recurrence proofs."}),
        )
    }
    pub fn main() -> Result<(), Box<dyn Error>> {
        let args: Vec<_> = std::env::args().skip(1).collect();
        if !(8..=9).contains(&args.len()) {
            return Err("usage: inspect_event_wait CONFIG STATE OUT.json PAIR_STEPS TRACE_STEPS WARMUP_STEPS BASES WORKLOAD_LABEL [TRUSTED_ENTRIES] (BASES and ENTRIES: object=hexoffset,... )".into());
        }
        let pair: u64 = args[3].parse()?;
        let traced: u64 = args[4].parse()?;
        let warmup: u64 = args[5].parse()?;
        if pair == 0
            || pair > LIMIT
            || traced > LIMIT
            || warmup > LIMIT
            || pair
                .checked_mul(2)
                .and_then(|n| n.checked_add(traced))
                .and_then(|n| n.checked_add(warmup))
                .is_none_or(|n| n > LIMIT)
        {
            return Err("total diagnostic budget exceeds 50M steps".into());
        }
        let config = Path::new(&args[0]);
        let mut spec = super::machine_profile::parse_machine_profile(std::str::from_utf8(&read(
            config,
            1024 * 1024,
        )?)?)?;
        let mount = spec
            .mounts
            .iter()
            .find(|m| m.letter == b'C' && m.kind == SourceKind::BrowserZip)
            .ok_or("ZIP C: required")?;
        let source = config
            .parent()
            .unwrap_or(Path::new("."))
            .join(spec.binding(&mount.source));
        for mount in &mut spec.mounts {
            mount.policy = WritePolicy::Temporary;
        }
        let archive = super::archive::GameArchive::new(Arc::new(read(
            &source,
            super::archive::MAX_ZIP_BYTES,
        )?))?;
        let wanted = spec.game.executable.replace('\\', "/").to_ascii_uppercase();
        let selected = archive
            .programs
            .iter()
            .find(|p| **p == wanted || p.ends_with(&format!("/{wanted}")))
            .ok_or("profile EXE missing")?;
        let exe = archive.program_bytes(selected)?;
        let le = LeImage::discover(&exe, LeLimits::default())?;
        let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
        let mut bases = Vec::new();
        for entry in args[6].split(',') {
            if bases.len() >= 64 {
                return Err("object binding limit: 64".into());
            }
            let (object, base) = entry.split_once('=').ok_or("BASES format")?;
            let object = object.parse()?;
            let base = u32::from_str_radix(base.trim_start_matches("0x"), 16)?;
            let size = image
                .object(object)
                .ok_or("unknown object")?
                .metadata()
                .virtual_size;
            if base.checked_add(size).is_none()
                || bases.iter().any(|&(number, other_base, other_size)| {
                    number == object || (base < other_base + other_size && other_base < base + size)
                })
            {
                return Err("object bindings wrap, overlap or repeat an object".into());
            }
            bases.push((object, base, size));
        }
        let media = archive.load_configured(selected, &spec)?;
        let mut entries = Vec::new();
        if let Some(supplied) = args.get(8) {
            for entry in supplied.split(',') {
                if entries.len() >= 64 {
                    return Err("trusted entry limit: 64".into());
                }
                let (object, offset) = entry.split_once('=').ok_or("ENTRIES format")?;
                let address = ObjectAddress {
                    object_number: object.parse()?,
                    offset: u32::from_str_radix(offset.trim_start_matches("0x"), 16)?,
                };
                if !bases.iter().any(|&(number, _, size)| {
                    number == address.object_number && address.offset < size
                }) {
                    return Err("trusted entry must lie in a bound object".into());
                }
                entries.push(address);
            }
        }
        let bytes = read(Path::new(&args[1]), 256 * 1024 * 1024)?;
        let mut initial = Session::load_state_with_media(&bytes, &media)?;
        setup(&mut initial, false);
        let mut discarded = Audio::new();
        discarded.drain(&mut initial)?;
        advance(&mut initial, warmup, &mut discarded)?;
        let snapshot = initial.save_state()?;
        let initial_state = state(&initial)?;
        let candidates = static_candidates(&image, &initial, &bases, &entries)?;
        let mut off = Session::load_state_with_media(&snapshot, &media)?;
        let mut on = Session::load_state_with_media(&snapshot, &media)?;
        setup(&mut off, false);
        setup(&mut on, true);
        let mut off_audio = Audio::new();
        let mut on_audio = Audio::new();
        let wall = Instant::now();
        advance(&mut off, pair, &mut off_audio)?;
        let off_seconds = wall.elapsed().as_secs_f64();
        let wall = Instant::now();
        advance(&mut on, pair, &mut on_audio)?;
        let on_seconds = wall.elapsed().as_secs_f64();
        let on_idle = format!("{:?}", on.idle_loop_stats());
        let off_idle = format!("{:?}", off.idle_loop_stats());
        let cpu_equal = off.debug_cpu().snapshot() == on.debug_cpu().snapshot();
        let audio_status_equal = off.audio_status() == on.audio_status();
        let off_state = state(&off)?;
        let on_state = state(&on)?;
        // Host idle policy is explicitly serialized. Normalize it only after
        // measuring so byte equality compares architectural/device/runtime state.
        on.set_idle_loops(false);
        let state_equal = off.save_state()? == on.save_state()?;
        let trace = trace(&mut initial, traced, &bases)?;
        let report = json!({"workload":args[7],"input_state":args[1],"le_header":format!("{:?}",le.header()),"bases_declared_not_inferred":bases,"initial":initial_state,"static":candidates,
            "pair_steps":pair,"off":{"host_seconds":off_seconds,"idle":off_idle,"audio":off_audio.json(),"final":off_state},
            "on":{"host_seconds":on_seconds,"idle":on_idle,"audio":on_audio.json(),"final":on_state},
            "normalized_saved_state_equal":state_equal,"architectural_cpu_equal":cpu_equal,"audio_status_equal":audio_status_equal,
            "streamed_output_equal":off_audio.json()==on_audio.json(),"trace":trace});
        fs::write(&args[2], serde_json::to_vec_pretty(&report)?)?;
        println!(
            "state_equal={state_equal} cpu_equal={cpu_equal} audio_status_equal={audio_status_equal} output_equal={} off={off_seconds:.6}s on={on_seconds:.6}s idle={on_idle}; report={}",
            off_audio.json() == on_audio.json(),
            args[2]
        );
        Ok(())
    }
}

#[cfg(all(
    feature = "wasm-static-test",
    feature = "save-state",
    feature = "profiling"
))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    diagnostic::main()
}

#[cfg(not(all(
    feature = "wasm-static-test",
    feature = "save-state",
    feature = "profiling"
)))]
fn main() {
    eprintln!("inspect_event_wait requires --features wasm-static-test,save-state,profiling");
}
