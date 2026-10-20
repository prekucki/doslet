//! Native compilation diagnostic; accepts WASM source, never native artifacts.
#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use std::{env, fs::File, io::Read};
    let mut arguments = env::args_os().skip(1);
    let path = arguments
        .next()
        .ok_or("usage: precompile-wasm MODULE.wasm")?;
    if arguments.next().is_some() {
        return Err("usage: precompile-wasm MODULE.wasm".into());
    }
    const MAX_BYTES: u64 = 64 * 1024 * 1024;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("WASM source exceeds 64 MiB".into());
    }
    let artifact = doslet_wasm_aot::Artifact::compile(&bytes)?;
    println!(
        "Cranelift Speed / Backtracking / Inlining=Yes; WASM bytes: {}; native artifact bytes: {}; compile seconds: {:.6}; load seconds: {:.6}",
        bytes.len(),
        artifact.serialized().len(),
        artifact.compile_seconds,
        artifact.load_seconds
    );
    println!(
        "Source compiled and this process's own artifact loaded; no module execution measured."
    );
    Ok(())
}

#[cfg(target_arch = "wasm32")]
fn main() {}
