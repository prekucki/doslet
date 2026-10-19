//! Same saved game window: the complete interpreter/runtime runs under Wasmtime.
#[allow(dead_code)]
#[path = "../../common/machine_profile.rs"]
mod machine_profile;
use doslet_wasm_aot::probe::GameArtifacts;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    time::Instant,
};

fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    let option = |key: &str| {
        args.iter()
            .position(|s| s == key)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let config = PathBuf::from(option("--config").ok_or("--config required")?);
    let fixture = PathBuf::from(
        option("--fixture").ok_or("--fixture DIR with initial.state/profile/game.wasm required")?,
    );
    let probe = PathBuf::from(
        option("--probe")
            .unwrap_or("target/wasm32-unknown-unknown/web/doslet_wasm_game_probe.wasm".into()),
    );
    let out = PathBuf::from(option("--out").unwrap_or("target/rott-wasmtime-interpreter".into()));
    let steps: u64 = option("--steps").unwrap_or("2000000".into()).parse()?;
    let repeats: usize = option("--repeats").unwrap_or("3".into()).parse()?;
    if steps == 0 || steps > 10_000_000 || repeats == 0 || repeats > 5 {
        return Err("diagnostic bounds exceeded".into());
    }
    fs::create_dir_all(&out)?;
    let toml = fs::read(&config)?;
    let spec = machine_profile::parse_machine_profile(std::str::from_utf8(&toml)?)?;
    let mount = spec
        .mounts
        .iter()
        .find(|m| m.letter == b'C' && m.kind == doslet_config::SourceKind::BrowserZip)
        .ok_or("requires ZIP C:")?;
    let zip = fs::read(
        config
            .parent()
            .unwrap_or(Path::new("."))
            .join(spec.binding(&mount.source)),
    )?;
    let initial = fs::read(fixture.join("initial.state"))?;
    let profile = fs::read(fixture.join("observed-speed-tiers.json"))?;
    let generated = fs::read(fixture.join("game.wasm"))?;
    let probe_bytes = fs::read(&probe)?;
    let parts = [&zip, &toml, &initial, &profile];
    let mut bundle = Vec::new();
    for part in parts {
        bundle.extend_from_slice(&u32::try_from(part.len())?.to_le_bytes());
    }
    for part in parts {
        bundle.extend_from_slice(part);
    }
    let mut oracle = doslet_wasm_game_probe::Probe::new(&bundle)?;
    oracle.reset(false)?;
    let mut native_seconds = 0.0;
    let mut done = 0;
    while done < steps {
        let budget = (steps - done).min(10_000) as u32;
        let now = Instant::now();
        let n = oracle.run_reference(budget);
        native_seconds += now.elapsed().as_secs_f64();
        if n != budget {
            return Err("native oracle stopped".into());
        }
        done += u64::from(n);
        oracle.drain();
    }
    oracle.prepare_output(0);
    let expected_state = oracle.output().to_vec();
    fs::write(out.join("reference-final.state"), &expected_state)?;
    oracle.prepare_output(1);
    let expected_audio = oracle.output().to_vec();
    drop(oracle);
    eprintln!("native oracle={native_seconds:.6}s");
    let artifacts = GameArtifacts::compile(&probe_bytes, &generated)?;
    eprintln!(
        "AOT interpreter={:.6}s translated={:.6}s",
        artifacts.probe.compile_seconds, artifacts.translated.compile_seconds
    );
    fs::write(out.join("runtime.cwasm"), artifacts.probe.serialized())?;
    fs::write(out.join("game.cwasm"), artifacts.translated.serialized())?;
    let variants = if args.iter().any(|s| s == "--reference-only") {
        2
    } else {
        3
    };
    let mut measurements = Vec::new();
    for repeat in 0..repeats {
        let order: Vec<_> = if repeat % 2 == 0 {
            (0..variants).collect()
        } else {
            (0..variants).rev().collect()
        };
        for variant in order {
            let translated = variant == 2;
            let cache = variant == 1;
            let name = [
                "interpreter-wasm",
                "interpreter-wasm-cached",
                "translated-with-wasm-runtime",
            ][variant];
            let setup = Instant::now();
            let mut runner = artifacts.instantiate(&bundle, translated)?;
            let setup_seconds = setup.elapsed().as_secs_f64();
            for pass in 0..2 {
                runner.reset(cache)?;
                let mut seconds = 0.0;
                let mut done = 0;
                while done < steps {
                    let budget = (steps - done).min(10_000) as u32;
                    let now = Instant::now();
                    let n = runner.run(budget)?;
                    seconds += now.elapsed().as_secs_f64();
                    if n != budget {
                        return Err(format!("{name} stopped at {done}: {n}/{budget}").into());
                    }
                    done += u64::from(n);
                    runner.drain()?;
                }
                let state = runner.output(0)?;
                let audio = runner.output(1)?;
                if expected_state != state {
                    fs::write(out.join(format!("diverged-{name}.state")), &state)?;
                    return Err(format!("complete state differs for {name} pass={pass}").into());
                }
                if expected_audio != audio {
                    return Err(format!("audio differs for {name}").into());
                }
                let stats: serde_json::Value = serde_json::from_slice(&runner.output(3)?)?;
                eprintln!(
                    "{name} repeat={repeat} pass={pass} seconds={seconds:.6} state/audio exact; {stats}"
                );
                if pass == 1 {
                    measurements.push(json!({"name":name,"repeat":repeat,"steps":steps,"warmup_passes":1,"seconds":seconds,"setup_seconds":setup_seconds,"state_equal":true,"audio_equal":true,"state_sha256":hash(&state),"audio_sha256":hash(&audio),"stats":stats}));
                    fs::write(
                        out.join("measurements.json"),
                        serde_json::to_vec_pretty(&measurements)?,
                    )?;
                }
            }
        }
    }
    let report = json!({"config":config,"fixture":fixture,"probe":probe,"probe_sha256":hash(&probe_bytes),"initial_state_sha256":hash(&initial),"profile_sha256":hash(&profile),"generated_sha256":hash(&generated),
        "build":"Cargo profile web (opt-level=3, thin LTO, codegen-units=1), wasm32-unknown-unknown",
        "optimization":"Wasmtime 45.0.3 AOT: Cranelift Speed + Backtracking + inlining",
        "interpreter_aot_compile_seconds":artifacts.probe.compile_seconds,"interpreter_aot_load_seconds":artifacts.probe.load_seconds,
        "translated_aot_compile_seconds":artifacts.translated.compile_seconds,"translated_aot_load_seconds":artifacts.translated.load_seconds,
        "measurements":measurements,"native_oracle_seconds":native_seconds,
        "boundary":"Complete interpreter, runtime, devices and fallback execute in WASM under Wasmtime; native poll trampoline copies 65,600-byte scratch via a staging buffer (four copies per poll); no guest RAM/VRAM transport; setup, drain/hash and serialization excluded; one full warmup before resetting guest state; not a browser measurement"});
    fs::write(out.join("report.json"), serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
