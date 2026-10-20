//! Bounded, same-state acceptance measurement of one statically converted EXE.
//! Explicit layouts are declarations; actual code/context admission remains live.
#[allow(dead_code)]
#[path = "../../doslet/src/archive.rs"]
mod archive;
#[allow(dead_code)]
#[path = "../../common/machine_profile.rs"]
mod machine_profile;

use doslet_aot::profile::{Profile, ProfileLimits};
use doslet_config::{SourceKind, WritePolicy};
use doslet_runtime::Session;
use doslet_static_game_probe::{Probe, metadata, prepare};
use doslet_wasm_aot::shared_runtime::{SharedArtifacts, SharedRunner};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap, error::Error, fs, io::Read, path::Path, sync::Arc, time::Instant,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const CHUNK: u64 = 10_000;

fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(format!("{} exceeds diagnostic input bound", path.display()).into());
    }
    Ok(bytes)
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

struct Driver {
    runner: SharedRunner,
    handle: u32,
    frame: u32,
}
impl Driver {
    fn new(artifacts: &SharedArtifacts, bundle: &[u8]) -> Result<Self> {
        let mut runner = artifacts.instantiate()?;
        let len = u32::try_from(bundle.len())?;
        let input = runner.call_runtime::<u32, u32>("static_probe_input", len)?;
        runner.write_bytes(input, bundle)?;
        let handle = runner.call_runtime::<(u32, u32), u32>("static_probe_new", (input, len))?;
        let frame = runner.call_runtime::<u32, u32>("static_probe_frame", handle)?;
        Ok(Self {
            runner,
            handle,
            frame,
        })
    }
    fn reset(&mut self, cache: bool) -> Result<()> {
        self.runner
            .call_runtime::<(u32, u32), ()>("static_probe_reset", (self.handle, cache as u32))?;
        Ok(())
    }
    fn set_fallback_diagnostics(&mut self, enabled: bool) -> Result<()> {
        self.runner.call_runtime::<(u32, u32), ()>(
            "static_probe_diagnose_fallbacks",
            (self.handle, enabled as u32),
        )?;
        Ok(())
    }
    fn output(&mut self, kind: u32) -> Result<Vec<u8>> {
        let packed = self
            .runner
            .call_runtime::<(u32, u32), u64>("static_probe_output", (self.handle, kind))?;
        Ok(self
            .runner
            .memory_view(packed as u32, (packed >> 32) as u32)?
            .to_vec())
    }
    fn drain(&mut self) -> Result<()> {
        self.runner
            .call_runtime::<u32, ()>("static_probe_drain", self.handle)?;
        Ok(())
    }
    /// Time begin/run/finish together; serialization and audio draining follow.
    fn static_chunk(&mut self, budget: u64) -> Result<(u64, f64)> {
        let now = Instant::now();
        self.runner
            .call_runtime::<(u32, u64), ()>("static_probe_begin", (self.handle, budget))?;
        self.runner.run(self.frame)?;
        self.runner
            .call_runtime::<u32, ()>("static_probe_finish", self.handle)?;
        let seconds = now.elapsed().as_secs_f64();
        let completed = self.runner.read_frame(self.frame)?.charged_work;
        if completed == 0 || completed > budget + 256 {
            return Err(format!("static chunk made invalid progress: {completed}/{budget}").into());
        }
        Ok((completed, seconds))
    }
    fn reference_chunk(&mut self, budget: u64) -> Result<f64> {
        let budget = u32::try_from(budget)?;
        let now = Instant::now();
        let done = self
            .runner
            .call_runtime::<(u32, u32), u32>("static_probe_reference", (self.handle, budget))?;
        let seconds = now.elapsed().as_secs_f64();
        if done != budget {
            return Err(format!("reference stopped after {done}/{budget} instructions").into());
        }
        Ok(seconds)
    }
    fn close(mut self) -> Result<()> {
        self.runner
            .call_runtime::<u32, ()>("static_probe_free", self.handle)?;
        Ok(())
    }
}

fn main() -> Result<()> {
    let mut options = BTreeMap::new();
    let mut diagnose_fallbacks = false;
    let mut memory_write_helper = false;
    let mut args = std::env::args().skip(1);
    while let Some(key) = args.next() {
        if key == "--memory-write-helper" {
            if memory_write_helper {
                return Err("duplicate option".into());
            }
            memory_write_helper = true;
            continue;
        }
        if key == "--diagnose-fallbacks" {
            if diagnose_fallbacks {
                return Err("duplicate option".into());
            }
            diagnose_fallbacks = true;
            continue;
        }
        if !matches!(
            key.as_str(),
            "--config"
                | "--state"
                | "--bindings"
                | "--profile-bindings"
                | "--profile"
                | "--base-profile"
                | "--probe"
                | "--out"
                | "--steps"
                | "--repeats"
                | "--event-waits"
                | "--seek-wait"
        ) {
            return Err(format!("unknown option {key}").into());
        }
        let value = args.next().ok_or("each option requires a value")?;
        if options.insert(key, value).is_some() {
            return Err("duplicate option".into());
        }
    }
    let required = |key: &str| -> Result<&str> {
        options
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| format!("required: {key}").into())
    };
    let config = Path::new(required("--config")?);
    let state_path = Path::new(required("--state")?);
    let binding_path = Path::new(required("--bindings")?);
    let probe_path = Path::new(
        options
            .get("--probe")
            .map(String::as_str)
            .unwrap_or("target/wasm32-unknown-unknown/release/doslet_static_game_probe.wasm"),
    );
    let out = Path::new(required("--out")?);
    let steps: u64 = options
        .get("--steps")
        .map(String::as_str)
        .unwrap_or("100000")
        .parse()?;
    let repeats: usize = options
        .get("--repeats")
        .map(String::as_str)
        .unwrap_or("3")
        .parse()?;
    if steps == 0 || steps > 10_000_000 || repeats == 0 || repeats > 5 {
        return Err("diagnostic bounds exceeded".into());
    }
    let profile_count = ["--profile", "--profile-bindings", "--base-profile"]
        .iter()
        .filter(|key| options.contains_key(**key))
        .count();
    if profile_count != 0 && profile_count != 3 {
        return Err(
            "profile requires --profile, --profile-bindings and --base-profile together".into(),
        );
    }
    let config_bytes = read(config, 1024 * 1024)?;
    let mut spec = machine_profile::parse_machine_profile(std::str::from_utf8(&config_bytes)?)?;
    let mount = spec
        .mounts
        .iter()
        .find(|mount| mount.letter == b'C' && mount.kind == SourceKind::BrowserZip)
        .ok_or("ZIP C: required")?;
    let zip_path = config
        .parent()
        .unwrap_or(Path::new("."))
        .join(spec.binding(&mount.source));
    let zip = read(&zip_path, archive::MAX_ZIP_BYTES)?;
    for mount in &mut spec.mounts {
        mount.policy = WritePolicy::Temporary;
    }
    let archive = archive::GameArchive::new(Arc::new(zip.clone()))?;
    let wanted = spec.game.executable.replace('\\', "/").to_ascii_uppercase();
    let selected = archive
        .programs
        .iter()
        .find(|program| **program == wanted || program.ends_with(&format!("/{wanted}")))
        .ok_or("configured executable absent")?;
    let exe = archive.program_bytes(selected)?;
    let media = archive.load_configured(selected, &spec)?;
    let mut initial = read(state_path, 256 * 1024 * 1024)?;
    let mut session = Session::load_state_with_media(&initial, &media)?;
    let binding_bytes = read(binding_path, 64 * 1024)?;
    let bindings = prepare::bindings_from_json(&exe, &binding_bytes)?;
    let profile_bytes = options
        .get("--profile")
        .map(|path| read(Path::new(path), ProfileLimits::default().max_input_bytes))
        .transpose()?;
    let profile = profile_bytes
        .as_deref()
        .map(|bytes| Profile::parse(bytes, ProfileLimits::default()))
        .transpose()?;
    let profile_binding_bytes = options
        .get("--profile-bindings")
        .map(|path| read(Path::new(path), 64 * 1024))
        .transpose()?;
    let profile_bindings = profile_binding_bytes
        .as_deref()
        .map(|bytes| prepare::bindings_from_json(&exe, bytes))
        .transpose()?;
    let base_bytes = options
        .get("--base-profile")
        .map(|path| read(Path::new(path), ProfileLimits::default().max_input_bytes))
        .transpose()?;
    let expected_identity = base_bytes
        .as_deref()
        .map(|bytes| format!("profile-sha256:{}", hash(bytes)));
    let profile_input = profile.as_ref().map(|profile| prepare::ProfileInput {
        profile,
        source_bindings: profile_bindings.as_deref().expect("validated options"),
        expected_identity: expected_identity.as_deref(),
    });
    let wait_policy = match options
        .get("--event-waits")
        .map(String::as_str)
        .unwrap_or("off")
    {
        "off" => prepare::WaitPolicy::Off,
        "static" => prepare::WaitPolicy::Static,
        "external" => prepare::WaitPolicy::External,
        "cold" => prepare::WaitPolicy::Cold,
        _ => return Err("--event-waits must be off, static, external or cold".into()),
    };
    let mut prepared = prepare::prepare_with_memory_write_helper(
        &exe,
        &session,
        &bindings,
        profile_input,
        wait_policy,
        memory_write_helper,
    )?;
    let seek_limit: u64 = options
        .get("--seek-wait")
        .map(String::as_str)
        .unwrap_or("0")
        .parse()?;
    if seek_limit > 100_000_000 {
        return Err("wait seek exceeds 100M instructions".into());
    }
    let mut seek_steps = 0u64;
    if seek_limit != 0 {
        let heads: Vec<_> = prepared
            .metadata
            .regions
            .iter()
            .filter(|region| region.wait_proof_steps != 0)
            .map(|region| {
                let object = prepared
                    .metadata
                    .objects
                    .iter()
                    .find(|object| object.space == region.entry.space)
                    .expect("validated region object");
                object.linear_base + region.entry.offset
            })
            .collect();
        if heads.is_empty() {
            return Err("wait seek requires selected wait candidates".into());
        }
        session.set_dynamic_core(false);
        session.set_decode_cache(true);
        session.set_idle_loops(false);
        session.set_vga_wait_acceleration(false);
        session.set_synchronization_analysis(false);
        session.set_adaptive_idle(false);
        session.set_idle_observation(false);
        session.start();
        let wall = Instant::now();
        while !heads.contains(&session.debug_pc()) {
            if seek_steps >= seek_limit || wall.elapsed().as_secs() > 60 {
                return Err(format!("wait head not reached after {seek_steps} steps, PC={:#x}; no guest state was injected", session.debug_pc()).into());
            }
            if session.advance(1) != 1 {
                return Err(format!(
                    "wait seek stopped: {:?} {:?}",
                    session.status(),
                    session.fault()
                )
                .into());
            }
            seek_steps += 1;
            if seek_steps.is_multiple_of(4096) {
                let mut pcm = [[0; 2]; 2048];
                let mut midi = [0; 2048];
                while session.drain_pcm(&mut pcm) != 0 {}
                while session.drain_midi(&mut midi) != 0 {}
            }
        }
        eprintln!(
            "natural wait head reached after {seek_steps} instructions at {:#x}",
            session.debug_pc()
        );
        initial = session.save_state()?;
        prepared = prepare::prepare_with_memory_write_helper(
            &exe,
            &session,
            &bindings,
            profile_input,
            wait_policy,
            memory_write_helper,
        )?;
    }
    eprintln!(
        "static conversion: {:?}; profile {:?}",
        prepared.report, prepared.guidance_report
    );
    eprintln!(
        "event-wait policy {wait_policy:?}: {} selected candidates",
        prepared.wait_candidates
    );
    let metadata = metadata::encode(&prepared.metadata)?;
    let parts = [&zip[..], &config_bytes, &initial, &metadata];
    let mut bundle = Vec::new();
    for part in parts {
        bundle.extend_from_slice(&u32::try_from(part.len())?.to_le_bytes());
    }
    for part in parts {
        bundle.extend_from_slice(part);
    }
    let probe_bytes = read(probe_path, 64 * 1024 * 1024)?;
    // Retain offline outputs even when an engine rejects the generated module.
    fs::create_dir_all(out)?;
    fs::write(out.join("initial.state"), &initial)?;
    fs::write(out.join("game.wasm"), &prepared.wasm)?;
    fs::write(out.join("metadata.bin"), &metadata)?;
    let artifacts = SharedArtifacts::compile(&probe_bytes, &prepared.wasm)?;
    eprintln!(
        "AOT runtime={:.3}s program={:.3}s",
        artifacts.runtime.compile_seconds, artifacts.program.compile_seconds
    );
    fs::write(out.join("game.cwasm"), artifacts.program.serialized())?;

    // Establish the actual soft-budget endpoint, retaining every audio boundary.
    // Each later pass must reproduce these chunks; reference execution then
    // consumes those exact instruction counts rather than the nominal budget.
    let mut pilot = Driver::new(&artifacts, &bundle)?;
    if diagnose_fallbacks {
        eprintln!("fallback diagnostics enabled; static timings include instrumentation");
        pilot.set_fallback_diagnostics(true)?;
    }
    pilot.reset(false)?;
    let mut chunks = Vec::new();
    let mut done = 0u64;
    while done < steps {
        let budget = (steps - done).min(CHUNK);
        let (completed, _) = pilot.static_chunk(budget)?;
        chunks.push((budget, completed));
        done += completed;
        pilot.drain()?;
    }
    let pilot_stats: Value = serde_json::from_slice(&pilot.output(3)?)?;
    eprintln!("pilot actual instructions={done}; {pilot_stats}");
    pilot.close()?;
    let mut oracle = Probe::new(&bundle)?;
    oracle.reset(false)?;
    for &(_, completed) in &chunks {
        if oracle.run_reference(u32::try_from(completed)?)? != completed as u32 {
            return Err("native reference stopped".into());
        }
        oracle.drain()?;
    }
    oracle.prepare_output(0)?;
    let expected_state = oracle.output().to_vec();
    oracle.prepare_output(1)?;
    let expected_audio = oracle.output().to_vec();
    fs::write(out.join("reference-final.state"), &expected_state)?;
    drop(oracle);

    let mut measurements = Vec::new();
    for repeat in 0..repeats {
        for index in 0..6 {
            let choice = (index + repeat) % 6;
            if choice < 3 {
                let name = [
                    "interpreter-native",
                    "interpreter-native-cached",
                    "interpreter-native-cached-idle",
                ][choice];
                let mut native = Probe::new(&bundle)?;
                for pass in 0..2 {
                    native.reset(choice != 0)?;
                    native.set_reference_idle(choice == 2)?;
                    let mut seconds = 0.0;
                    for &(_, completed) in &chunks {
                        let budget = u32::try_from(completed)?;
                        let now = Instant::now();
                        let actual = native.run_reference(budget)?;
                        seconds += now.elapsed().as_secs_f64();
                        if actual != budget {
                            return Err(format!("{name} stopped").into());
                        }
                        native.drain()?;
                    }
                    native.set_reference_idle(false)?;
                    native.prepare_output(0)?;
                    if native.output() != expected_state {
                        return Err(format!("full saved state differs: {name}").into());
                    }
                    native.prepare_output(1)?;
                    if native.output() != expected_audio {
                        return Err(format!("PCM/MIDI differs: {name}").into());
                    }
                    native.prepare_output(3)?;
                    let stats: Value = serde_json::from_slice(native.output())?;
                    eprintln!(
                        "{name} repeat={repeat} pass={pass} {seconds:.6}s state/audio exact; {stats}"
                    );
                    if pass == 1 {
                        measurements.push(json!({"name":name,"repeat":repeat,"seconds":seconds,"actual_instructions":done,"state_equal":true,"audio_equal":true,"stats":stats,"final_linear_pc":native.session().debug_pc()}));
                    }
                }
                continue;
            }
            let variant = choice - 3;
            let name = ["interpreter-wasm", "interpreter-wasm-cached", "static-wasm"][variant];
            let mut driver = Driver::new(&artifacts, &bundle)?;
            if diagnose_fallbacks && variant == 2 {
                driver.set_fallback_diagnostics(true)?;
            }
            // One complete untimed pass warms code before an identical reset.
            for pass in 0..2 {
                driver.reset(variant == 1)?;
                let mut seconds = 0.0;
                for &(budget, expected_completed) in &chunks {
                    if variant == 2 {
                        let (completed, elapsed) = driver.static_chunk(budget)?;
                        if completed != expected_completed {
                            return Err("static soft-budget endpoint changed between passes".into());
                        }
                        seconds += elapsed;
                    } else {
                        seconds += driver.reference_chunk(expected_completed)?;
                    }
                    driver.drain()?;
                }
                let stats: Value = serde_json::from_slice(&driver.output(3)?)?;
                let state = driver.output(0)?;
                let audio = driver.output(1)?;
                if state != expected_state {
                    fs::write(out.join(format!("diverged-{name}.state")), &state)?;
                    return Err(format!(
                        "full saved state differs: {name} repeat={repeat} pass={pass}; {stats}"
                    )
                    .into());
                }
                if audio != expected_audio {
                    return Err(format!("PCM/MIDI differs: {name}").into());
                }
                eprintln!(
                    "{name} repeat={repeat} pass={pass} {seconds:.6}s state/audio exact; {stats}"
                );
                if pass == 1 {
                    let address = driver.runner.read_frame(driver.frame)?.address;
                    measurements.push(json!({"name":name,"repeat":repeat,"seconds":seconds,"instrumented":diagnose_fallbacks && variant == 2,"actual_instructions":done,"state_equal":true,"audio_equal":true,"stats":stats,"final_address":{"object":address.space,"offset":address.offset}}));
                }
            }
            driver.close()?;
        }
    }
    let report = json!({
        "config":config,"initial_state":state_path,"probe":probe_path,"requested_instructions":steps,"actual_instructions":done,
        "seek_instructions":seek_steps,
        "executable_sha256":hash(&exe),"initial_state_sha256":hash(&initial),"runtime_wasm_sha256":hash(&probe_bytes),"program_wasm_sha256":hash(&prepared.wasm),
        "declared_binding_sha256":hash(&binding_bytes),"profile_sha256":profile_bytes.as_deref().map(hash),"profile_source_binding_sha256":profile_binding_bytes.as_deref().map(hash),"profile_identity":expected_identity,
        "lowering":format!("{:?}",prepared.report),"guidance":format!("{:?}",prepared.guidance_report),"profile_rebased":prepared.rebased_guidance,
        "tiers":{"hot":prepared.hot_instructions,"cold":prepared.cold_instructions,"irq":prepared.irq_instructions},
        "wait_policy":format!("{wait_policy:?}"),"wait_candidates":prepared.wait_candidates,
        "wait_evidence":{"external_memory":prepared.wait_evidence[0],"local_progress":prepared.wait_evidence[1],"uncertain":prepared.wait_evidence[2]},
        "runtime_aot_seconds":artifacts.runtime.compile_seconds,"program_aot_seconds":artifacts.program.compile_seconds,
        "fallback_diagnostics":diagnose_fallbacks,
        "memory_write_helper":memory_write_helper,
        "timing_note": if diagnose_fallbacks { "Static WASM times include fallback classification and source-byte reads; instrumented diagnostic results are not baseline performance measurements." } else { "Fallback diagnostics disabled." },
        "state_sha256":hash(&expected_state),"audio_sha256":hash(&expected_audio),"measurements":measurements,
        "optimization":"Wasmtime AOT, Cranelift Speed, Backtracking, inlining",
        "native_build":"Cargo release: opt-level 3, thin LTO, codegen-units 1; actual Rust Session/interpreter",
        "boundary":"One static EXE module, shared WASM runtime memory and direct WASM callbacks. Live region admission validates code bytes, CS context and hard device horizon. All tiers counted; adaptive/speed-tier acceleration disabled. Optional wait candidates start the ordinary CPU/RAM recurrence proof; only proven passes may be omitted, bounded before hardware events. Setup/optional natural wait seek/reset, AOT, audio drain/hash and full-state serialization excluded. Identical actual instruction and drain boundaries, one full warmup per variant. Explicit load/selector declarations are not live descriptor proofs. Native oracle plus both interpreter variants compare complete saved guest state and PCM/MIDI hashes. Not a browser or interactive gameplay measurement."
    });
    fs::write(out.join("report.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
