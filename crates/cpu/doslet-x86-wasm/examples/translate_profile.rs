//! cargo run -p doslet-x86-wasm --example translate_profile -- input.core output.wasm
use doslet_x86_wasm::Translation;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: translate_profile INPUT.json|INPUT.dsj OUTPUT.wasm".into());
    }
    use std::io::Read;
    let mut profile = Vec::new();
    std::fs::File::open(&args[0])?
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut profile)?;
    let translation = Translation::from_bytes(&profile)?;
    let wasm = translation.wasm();
    std::fs::write(&args[1], &wasm)?;
    println!(
        "{} translated blocks, {} WASM bytes; interpreter fallback required",
        translation.blocks().len(),
        wasm.len()
    );
    println!("{:?}", translation.report());
    Ok(())
}
