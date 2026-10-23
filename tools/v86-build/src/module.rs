//! Shared standalone WASM build for the native embedder and table generator.
use std::{
    env,
    error::Error,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
};

fn checked(command: &mut Command) -> Result<(), Box<dyn Error>> {
    if !command.status()?.success() {
        return Err(format!("command failed: {command:?}").into());
    }
    Ok(())
}

/// This executable doubles as rustc's linker wrapper. Keep v86's register
/// page below globals and the stack, and import the generated-code table.
pub fn link_if_requested() -> Result<bool, Box<dyn Error>> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if !args.first().is_some_and(|arg| arg == "-flavor") {
        return Ok(false);
    }
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let sysroot = Command::new(&rustc).args(["--print", "sysroot"]).output()?;
    let version = Command::new(&rustc).arg("-vV").output()?;
    let version = String::from_utf8(version.stdout)?;
    let host = version
        .lines()
        .find_map(|s| s.strip_prefix("host: "))
        .ok_or("missing rustc host")?;
    let linker = PathBuf::from(String::from_utf8(sysroot.stdout)?.trim())
        .join("lib/rustlib")
        .join(host)
        .join("bin")
        .join(format!("rust-lld{}", env::consts::EXE_SUFFIX));
    checked(
        Command::new(linker)
            .args(
                args.iter().filter(|arg| {
                    !matches!(arg.to_str(), Some("--stack-first" | "--export-table"))
                }),
            )
            .arg("--no-stack-first"),
    )?;
    Ok(true)
}

pub fn build(root: &Path, out: &Path) -> Result<PathBuf, Box<dyn Error>> {
    fs::create_dir_all(out)?;
    let vendor = root.join("vendor/v86");
    let object = out.join("softfloat.o");
    checked(
        Command::new(env::var_os("CLANG").unwrap_or_else(|| "clang".into()))
            .args([
                "-c",
                "--target=wasm32",
                "-O3",
                "-flto",
                "-nostdlib",
                "-fvisibility=hidden",
                "-ffunction-sections",
                "-fdata-sections",
                "-DSOFTFLOAT_FAST_INT64",
                "-DINLINE_LEVEL=5",
                "-DSOFTFLOAT_FAST_DIV32TO16",
                "-DSOFTFLOAT_FAST_DIV64TO32",
            ])
            .arg(vendor.join("softfloat/softfloat.c"))
            .arg("-o")
            .arg(&object),
    )?;
    // Cargo does not track the contents of an object passed via link-arg or
    // this linker wrapper. Include both in rustc's input fingerprint so a
    // SoftFloat-only edit cannot reuse a stale linked WASM module.
    let mut inputs = std::collections::hash_map::DefaultHasher::new();
    fs::read(&object)?.hash(&mut inputs);
    include_bytes!("module.rs").hash(&mut inputs);
    let mut cargo = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    cargo
        .current_dir(&vendor)
        .args(["rustc", "--crate-type", "cdylib", "--manifest-path"])
        .arg(vendor.join("Cargo.toml"))
        .args([
            "--release",
            "--locked",
            "--offline",
            "--target",
            "wasm32-unknown-unknown",
            "--target-dir",
        ])
        .arg(out.join("build"))
        // Native target-cpu/linker settings must not leak into the WASM build.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .env_remove("CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS")
        // cargo clippy's wrapper applies to the workspace, not this separately
        // pinned upstream package and its generated instruction tables.
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CLIPPY_ARGS")
        .args(["--", "-C"])
        .arg(format!("linker={}", env::current_exe()?.display()))
        .args([
            "-C",
            "link-arg=--import-table",
            "-C",
            "link-arg=--global-base=4096",
            "-C",
        ])
        .arg(format!("link-arg={}", object.display()))
        .arg("--cfg")
        .arg(format!("doslet_build_inputs=\"{:016x}\"", inputs.finish()))
        .args(["-C", "target-feature=+bulk-memory,+multivalue,+simd128"]);
    checked(&mut cargo)?;
    let wasm = out.join("doslet-v86.wasm");
    fs::copy(
        out.join("build/wasm32-unknown-unknown/release/doslet_v86_module.wasm"),
        &wasm,
    )?;
    Ok(wasm)
}
