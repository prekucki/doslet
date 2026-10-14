//! Trunk post-build hook. Rust tools only; no separate JS build system.
use std::{env, fs, path::PathBuf, process::Command};
fn run(command: &mut Command) -> Result<(), Box<dyn std::error::Error>> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("{command:?} failed: {status}").into());
    }
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let stage = PathBuf::from(env::var("TRUNK_STAGING_DIR")?).join("browser");
    fs::create_dir_all(&stage)?;
    let mut cargo = Command::new("cargo");
    cargo.current_dir(&root).args([
        "build",
        "--locked",
        "--target",
        "wasm32-unknown-unknown",
        "--profile",
        "web",
        "-p",
        "doslet-web-worker",
        "-p",
        "doslet-web-audio",
    ]);
    run(&mut cargo)?;
    for name in ["doslet_web_worker", "doslet_web_audio"] {
        let wasm = root.join(format!("target/wasm32-unknown-unknown/web/{name}.wasm"));
        run(Command::new("wasm-bindgen")
            .arg(wasm)
            .args(["--target", "web", "--no-typescript", "--out-dir"])
            .arg(&stage))?;
        if env::var("TRUNK_PROFILE").is_ok_and(|profile| profile == "release") {
            println!("{name}: wasm-opt -O3");
            let output = stage.join(format!("{name}_bg.wasm"));
            run(Command::new("wasm-opt")
                .arg(&output)
                .args(["-O3", "-o"])
                .arg(&output))?;
        }
    }
    Ok(())
}
