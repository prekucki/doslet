use std::{env, error::Error, fs, path::PathBuf};

#[path = "src/engine_config.rs"]
mod engine_config;
#[path = "../../../tools/v86-build/src/module.rs"]
mod module;

fn main() -> Result<(), Box<dyn Error>> {
    if module::link_if_requested()? {
        return Ok(());
    }
    println!("cargo:rerun-if-env-changed=CLANG");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/engine_config.rs");
    println!("cargo:rerun-if-changed=../../../tools/v86-build/src/module.rs");
    if env::var("CARGO_CFG_TARGET_ARCH")? == "wasm32" {
        return Ok(());
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .join("../../..")
        .canonicalize()?;
    for path in [
        "vendor/v86/rust",
        "vendor/v86/softfloat",
        "vendor/v86/Cargo.toml",
        "vendor/v86/Cargo.lock",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(path).display());
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let wasm = module::build(&root, &out)?;
    let mut config = engine_config::config();
    // Explicit target uses baseline ISA features, never the build host's CPU.
    // The artifact travels with the binary and the matching Wasmtime version.
    config.target(&env::var("TARGET")?)?;
    let engine = wasmtime::Engine::new(&config)?;
    let compiled = engine.precompile_module(&fs::read(wasm)?)?;
    fs::write(out.join("doslet-v86.cwasm"), compiled)?;
    Ok(())
}
