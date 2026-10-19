//! Real local-game profile census and bounded interpreter/WASM comparison.
//! Guest writes stay in a private overlay. No game/profile files are updated.
#[allow(dead_code)]
#[path = "../../doslet/src/archive.rs"]
mod archive;
#[allow(dead_code)]
#[path = "../../common/machine_profile.rs"]
mod machine_profile;

use doslet_config::{SourceKind, WritePolicy};
use doslet_runtime::{Session, SessionStatus};
use doslet_x86::translation;
use doslet_x86_wasm::{Translation, host::Host, native::Runner};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    error::Error,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

#[path = "../../common/wasm_game.rs"]
mod wasm_game;
use wasm_game::{Game, setup};
enum BenchRunner {
    Wasmi(Box<Runner<Game>>),
    #[cfg(feature = "wasm-aot-test")]
    Aot(doslet_wasm_aot::Runner<Game>),
}
impl BenchRunner {
    fn host(&self) -> &Host<Game> {
        match self {
            Self::Wasmi(r) => r.host(),
            #[cfg(feature = "wasm-aot-test")]
            Self::Aot(r) => r.host(),
        }
    }
    fn host_mut(&mut self) -> &mut Host<Game> {
        match self {
            Self::Wasmi(r) => r.host_mut(),
            #[cfg(feature = "wasm-aot-test")]
            Self::Aot(r) => r.host_mut(),
        }
    }
    fn run(&mut self, budget: u32) -> Result<u32, String> {
        match self {
            Self::Wasmi(r) => r.run(budget),
            #[cfg(feature = "wasm-aot-test")]
            Self::Aot(r) => r.run(budget),
        }
    }
}
#[derive(Default)]
struct Census {
    instructions: u64,
    code16: u64,
    code32: u64,
    unavailable: u64,
    undecoded: u64,
    decoded_widths: [u64; 3],
    narrow_registers: [u64; 2],
    current_registers_and_branches: u64,
}
struct Node {
    bytes: [u8; 15],
    count: u64,
    changed: bool,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn input(path: &Path, limit: u64) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut data = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err("input too large".into());
    }
    Ok(data)
}
fn advance(s: &mut Session, steps: u64) -> Result<(), Box<dyn Error>> {
    let end = s.steps() + steps;
    while s.steps() < end && s.status() == SessionStatus::Running {
        let done = s.advance((end - s.steps()).min(10_000) as u32);
        if done == 0 {
            return Err(format!("no progress: {:?}", s.fault()).into());
        }
        drain(s);
    }
    if s.steps() != end {
        return Err(format!(
            "guest stopped early: {:?} {:?}\n{}",
            s.status(),
            s.fault(),
            String::from_utf8_lossy(s.console())
        )
        .into());
    }
    Ok(())
}
fn drain(s: &mut Session) {
    let mut pcm = [[0; 2]; 2048];
    while s.drain_pcm(&mut pcm) != 0 {}
    let mut midi = [0; 2048];
    while s.drain_midi(&mut midi) != 0 {}
}
fn screenshot(s: &Session, path: &Path) -> Result<(), Box<dyn Error>> {
    if let Some(frame) = s.frame() {
        let mut pixels = vec![0; frame.width * frame.height * 4];
        frame
            .write_rgba(&mut pixels)
            .map_err(|e| format!("frame conversion: {e:?}"))?;
        let mut encoder = png::Encoder::new(
            fs::File::create(path)?,
            frame.width as u32,
            frame.height as u32,
        );
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header()?.write_image_data(&pixels)?;
    }
    Ok(())
}
fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let option = |key: &str| {
        args.iter()
            .position(|s| s == key)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let config = PathBuf::from(option("--config").ok_or("--config FILE required")?);
    let engine = option("--engine").unwrap_or("wasmi".into());
    match engine.as_str() {
        "wasmi" => {}
        #[cfg(feature = "wasm-aot-test")]
        "wasmtime" => {}
        _ => return Err("--engine must be wasmi or wasmtime (requires wasm-aot-test)".into()),
    }
    let out = PathBuf::from(option("--out").unwrap_or("target/rott-wasm-comparison".into()));
    let warmup: u64 = option("--warmup").unwrap_or("0".into()).parse()?;
    let training: u64 = option("--train-steps")
        .unwrap_or("1000000".into())
        .parse()?;
    let steps: u64 = option("--steps").unwrap_or("100000".into()).parse()?;
    let repeats: usize = option("--repeats").unwrap_or("3".into()).parse()?;
    if training == 0
        || training > 50_000_000
        || steps == 0
        || steps > 10_000_000
        || repeats == 0
        || repeats > 5
        || warmup > 100_000_000
    {
        return Err("requested comparison exceeds diagnostic bounds".into());
    }
    fs::create_dir_all(&out)?;
    let mut spec = machine_profile::parse_machine_profile(&fs::read_to_string(&config)?)?;
    let mount = spec
        .mounts
        .iter()
        .find(|m| m.letter == b'C' && m.kind == SourceKind::BrowserZip)
        .ok_or("this diagnostic currently requires a ZIP C: profile")?;
    let source = config
        .parent()
        .unwrap_or(Path::new("."))
        .join(spec.binding(&mount.source));
    // Host persistence remains disabled even when the live launch profile uses it.
    for mount in &mut spec.mounts {
        mount.policy = WritePolicy::Temporary;
    }
    let game = archive::GameArchive::new(Arc::new(input(&source, archive::MAX_ZIP_BYTES as u64)?))?;
    let wanted = spec.game.executable.replace('\\', "/").to_ascii_uppercase();
    let selected = game
        .programs
        .iter()
        .find(|p| *p == &wanted)
        .or_else(|| {
            game.programs
                .iter()
                .find(|p| p.ends_with(&format!("/{wanted}")))
        })
        .ok_or("profile EXE not present in ZIP")?;
    let exe = game.program_bytes(selected)?;
    let exe_hash = hash(&exe);
    let mut session = game.load_configured(selected, &spec)?;
    setup(&mut session);
    advance(&mut session, warmup)?;
    let observation_start = session.capture_working_state()?;
    let start_steps = session.steps();
    let mut census = Census::default();
    let mut nodes = BTreeMap::<(u32, bool), Node>::new();
    let collect = Instant::now();
    for _ in 0..training {
        if session.status() != SessionStatus::Running {
            break;
        }
        if let Some(i) = session.debug_observed_step() {
            census.instructions += 1;
            if i.code32 {
                census.code32 += 1;
            } else {
                census.code16 += 1;
            }
            let code = &i.bytes[..usize::from(i.available)];
            if i.available == 0 {
                census.unavailable += 1;
            }
            if let Some(shape) = translation::inspect(code, i.code32) {
                // Decoder width is reported separately from mode and from branches.
                if !shape.branch {
                    census.decoded_widths[match shape.width {
                        1 => 0,
                        2 => 1,
                        _ => 2,
                    }] += 1;
                }
                if i.code32 && shape.register_family && shape.width < 4 {
                    census.narrow_registers[usize::from(shape.width == 2)] += 1;
                }
            } else {
                census.undecoded += 1;
            }
            if i.code32 && translation::decode(code).is_some() {
                census.current_registers_and_branches += 1;
            }
            if i.available == 15
                && (nodes.len() < 100_000 || nodes.contains_key(&(i.linear, i.code32)))
            {
                let entry = nodes.entry((i.linear, i.code32)).or_insert(Node {
                    bytes: i.bytes,
                    count: 0,
                    changed: false,
                });
                entry.count += 1;
                entry.changed |= entry.bytes != i.bytes;
            }
        }
        if session.steps().is_multiple_of(2048) {
            drain(&mut session);
        }
    }
    drain(&mut session);
    let collection_seconds = collect.elapsed().as_secs_f64();
    let rows: Vec<_> = nodes.iter().map(|(&(pc, wide), n)| json!({"pc":pc,"bits":if wide {32} else {16},"bytes":n.bytes,"tier":"grey","count":n.count,"reason":if n.changed {"code-conflict"} else {"observed"}})).collect();
    let observed = serde_json::to_vec(
        &json!({"learning_version":1,"identity":format!("exe-sha256:{exe_hash}"),"unknown":"grey","irq_policy":"static-grey","nodes":rows}),
    )?;
    fs::write(out.join("observed-speed-tiers.json"), &observed)?;
    let default_cache = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")))
        .map(|p| p.join(format!("doslet/speed-tiers/{exe_hash}.json")));
    let existing = option("--profile")
        .map(PathBuf::from)
        .or_else(|| default_cache.filter(|p| p.is_file()));
    let (profile, profile_source) = if let Some(path) = existing {
        (input(&path, 64 * 1024 * 1024)?, path.display().to_string())
    } else {
        (observed, "recorded from this real run".into())
    };
    let translation = Translation::from_bytes(&profile)?;
    let emit_start = Instant::now();
    let wasm_bytes = translation.wasm();
    let module_emit_seconds = emit_start.elapsed().as_secs_f64();
    fs::write(out.join("game.wasm"), &wasm_bytes)?;
    #[cfg(feature = "wasm-aot-test")]
    let aot = if engine == "wasmtime" {
        let aot = doslet_wasm_aot::Artifact::compile(&wasm_bytes)?;
        fs::write(out.join("game.cwasm"), aot.serialized())?;
        eprintln!(
            "AOT Cranelift Speed: compile={:.6}s load={:.6}s bytes={}",
            aot.compile_seconds,
            aot.load_seconds,
            aot.serialized().len()
        );
        Some(aot)
    } else {
        None
    };
    #[cfg(feature = "wasm-aot-test")]
    let (aot_compile_seconds, aot_load_seconds) = aot
        .as_ref()
        .map_or((0.0, 0.0), |a| (a.compile_seconds, a.load_seconds));
    #[cfg(not(feature = "wasm-aot-test"))]
    let (aot_compile_seconds, aot_load_seconds) = (0.0, 0.0);
    let profile_report = format!("{:?}", translation.report());
    eprintln!(
        "collected {} instructions, mode16={} mode32={}, widths={:?}; source={profile_source}; {profile_report}; guest={:?} fault={:?}",
        census.instructions,
        census.code16,
        census.code32,
        census.decoded_widths,
        session.status(),
        session.fault()
    );
    eprintln!("console: {}", String::from_utf8_lossy(session.console()));
    if session.status() != SessionStatus::Running {
        return Err("guest stopped during observation".into());
    }
    let following = args.iter().any(|a| a == "--following-window");
    let snapshot = if following {
        session.capture_working_state()?
    } else {
        observation_start
    };
    fs::write(out.join("initial.state"), snapshot.restore()?.save_state()?)?;
    if args.iter().any(|a| a == "--prepare-only") {
        return Ok(());
    }
    let mut measurements = Vec::new();
    for repeat in 0..repeats {
        let native_start = Instant::now();
        let host = Host::new(
            Translation::from_bytes(&profile)?,
            Game {
                session: snapshot.restore()?,
            },
        );
        let mut runner = match engine.as_str() {
            "wasmi" => BenchRunner::Wasmi(Box::new(Runner::new(host)?)),
            #[cfg(feature = "wasm-aot-test")]
            "wasmtime" => BenchRunner::Aot(aot.as_ref().unwrap().instantiate(host)?),
            _ => unreachable!(),
        };
        setup(&mut runner.host_mut().runtime_mut().session);
        let preparation_seconds = native_start.elapsed().as_secs_f64();
        // Run the full window once, then restore only guest state while retaining
        // this compiled module/instance and its warmed execution machinery.
        for pass in 0..2 {
            let mut reference = snapshot.restore()?;
            setup(&mut reference);
            let mut fresh = snapshot.restore()?;
            setup(&mut fresh);
            runner.host_mut().replace_runtime(Game { session: fresh })?;
            let mut reference_seconds = 0.0;
            let mut wasm_seconds = 0.0;
            let mut done = 0;
            let mut pcm_equal = true;
            let mut midi_equal = true;
            let mut pcm_a = [[0i16; 2]; 2048];
            let mut pcm_b = [[0i16; 2]; 2048];
            let mut midi_a = [0u8; 2048];
            let mut midi_b = [0u8; 2048];
            while done < steps {
                let budget = (steps - done).min(10_000) as u32;
                let (a, b) = if repeat % 2 == 0 {
                    let now = Instant::now();
                    let a = reference.advance(budget);
                    reference_seconds += now.elapsed().as_secs_f64();
                    let now = Instant::now();
                    let b = runner.run(budget)?;
                    wasm_seconds += now.elapsed().as_secs_f64();
                    (a, b)
                } else {
                    let now = Instant::now();
                    let b = runner.run(budget)?;
                    wasm_seconds += now.elapsed().as_secs_f64();
                    let now = Instant::now();
                    let a = reference.advance(budget);
                    reference_seconds += now.elapsed().as_secs_f64();
                    (a, b)
                };
                let actual = &mut runner.host_mut().runtime_mut().session;
                if a != b
                    || a == 0
                    || !actual
                        .debug_cpu()
                        .matches(&reference.debug_cpu().snapshot())
                    || actual.emulated_nanos() != reference.emulated_nanos()
                {
                    return Err(format!("divergence after {done}: counts {a}/{b}, PCs {:x}/{:x}, time {}/{}, faults {:?}/{:?}", reference.debug_pc(), actual.debug_pc(), reference.emulated_nanos(), actual.emulated_nanos(), reference.fault(), actual.fault()).into());
                }
                for range in [
                    0..0xa0000,
                    0xc0000..if actual.a20_enabled() {
                        actual.hardware_config().memory_bytes()
                    } else {
                        0x100000
                    },
                ] {
                    if actual.ram_view(range.clone()) != reference.ram_view(range) {
                        return Err(format!("RAM divergence after {done}").into());
                    }
                }
                if actual.vram_view() != reference.vram_view() {
                    return Err("VRAM divergence".into());
                }
                loop {
                    let n = reference.drain_pcm(&mut pcm_a);
                    let m = actual.drain_pcm(&mut pcm_b);
                    pcm_equal &= n == m && pcm_a[..n] == pcm_b[..m];
                    if n == 0 && m == 0 {
                        break;
                    }
                }
                loop {
                    let n = reference.drain_midi(&mut midi_a);
                    let m = actual.drain_midi(&mut midi_b);
                    midi_equal &= n == m && midi_a[..n] == midi_b[..m];
                    if n == 0 && m == 0 {
                        break;
                    }
                }
                done += u64::from(a);
                if !pcm_equal || !midi_equal {
                    return Err("audio divergence".into());
                }
            }
            let actual = &runner.host().runtime().session;
            let state_equal = actual.save_state()? == reference.save_state()?;
            if !state_equal {
                return Err("complete serialized guest state diverged".into());
            }
            if pass == 0 {
                eprintln!(
                    "warmup engine={engine} repeat={repeat}: reference={reference_seconds:.6}s wasm={wasm_seconds:.6}s; exact state/audio match"
                );
                continue;
            }
            if repeat == 0 {
                screenshot(actual, &out.join("wasm.png"))?;
                screenshot(&reference, &out.join("reference.png"))?;
            }
            let stats = runner.host().stats();
            let row = json!({"repeat":repeat,"engine":engine,"warmup_passes":1,"steps":done,"reference_seconds":reference_seconds,"wasm_seconds":wasm_seconds,
            "wasmi_seconds":if engine == "wasmi" {Some(wasm_seconds)} else {None},
            "preparation_seconds":preparation_seconds,"translated_steps":stats.translated_steps,"interpreted_steps":stats.interpreted_steps,
            "polls":stats.polls,"grants":stats.grants,"state_bytes_equal":state_equal,"pcm_equal":pcm_equal,"midi_equal":midi_equal,
            "zero_budget":stats.zero_budget,"rejected_context":stats.rejected_context,"missing_code":stats.missing_code,
            "context":format!("CS={:?}, cr0={:x}, flags={:x}",actual.registers().segment_cache(doslet_x86::SegmentRegister::Cs),actual.control_registers().cr0,actual.eflags()),
            "guest_nanos":actual.emulated_nanos(),"final_pc":actual.debug_pc(),"status":format!("{:?}",actual.status())});
            eprintln!("{row}");
            measurements.push(row);
            fs::write(
                out.join("measurements.json"),
                serde_json::to_vec_pretty(&measurements)?,
            )?;
        }
    }
    let report = json!({"config":config,"exe_sha256":exe_hash,"profile_source":profile_source,"profile_sha256":hash(&profile),
        "profile_report":profile_report,"warmup_steps":warmup,"observation_steps":session.steps()-start_steps,"collection_seconds":collection_seconds,
        "measurement_start_steps":if following {session.steps()} else {start_steps},
        "cpu_instructions_or_rep_iterations":census.instructions,"code16":census.code16,"code32":census.code32,
        "decoded_nonbranch_widths_8_16_32":census.decoded_widths,"undecoded":census.undecoded,"unavailable_code":census.unavailable,
        "current_scope_instructions":census.current_registers_and_branches,"additional_register_scope_8_16":census.narrow_registers,
        "observed_unique_sites":nodes.len(),"measurements":measurements,"engine":engine,
        "optimization":if engine=="wasmtime" {"Cranelift Speed + backtracking + inlining"} else {"WASM interpreter"},
        "module_emit_seconds":module_emit_seconds,"aot_compile_seconds":aot_compile_seconds,"aot_load_seconds":aot_load_seconds,
        "boundary":"native execution: Wasmi interpreter or Wasmtime AOT; not browser JIT performance; one complete warmup pass per instance, then guest reset; decode widths exclude unknown decoder forms"});
    fs::write(out.join("report.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
