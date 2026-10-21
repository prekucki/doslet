//! Trunk post-build hook. Rust tools only; no separate JS build system.
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

const COMPILATION_HINTS_SECTION: &[u8] = b"metadata.code.compilation_hints";
/// `strategy = kEager (2) | (baseline_tier = kOptimized (3) << 2) | (top_tier = kOptimized (3) << 4)`.
const EAGER_OPTIMIZED_HINT: u8 = 0x3e;

fn run(command: &mut Command) -> Result<(), Box<dyn std::error::Error>> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("{command:?} failed: {status}").into());
    }
    Ok(())
}

fn read_u32_leb(bytes: &[u8], cursor: &mut usize) -> Result<u32, &'static str> {
    let mut result = 0u32;
    let mut shift = 0u32;
    loop {
        let &byte = bytes.get(*cursor).ok_or("truncated LEB128")?;
        *cursor += 1;
        if shift == 28 && (byte & 0xf0) != 0 {
            return Err("overflowing u32 LEB128");
        }
        result |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift >= 35 {
            return Err("invalid u32 LEB128");
        }
    }
}

fn write_u32_leb(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn skip_limits(bytes: &[u8], cursor: &mut usize) -> Result<(), &'static str> {
    let &flags = bytes.get(*cursor).ok_or("truncated limits")?;
    *cursor += 1;
    let _ = read_u32_leb(bytes, cursor)?;
    if flags & 1 != 0 {
        let _ = read_u32_leb(bytes, cursor)?;
    }
    Ok(())
}

fn inject_compilation_hints(wasm: &[u8]) -> Result<Vec<u8>, &'static str> {
    if wasm.get(..8) != Some(b"\0asm\x01\0\0\0".as_slice()) {
        return Err("invalid WebAssembly header");
    }
    let mut imported_funcs = 0u32;
    let mut defined_funcs = 0u32;
    let mut out = Vec::with_capacity(wasm.len() + 1024);
    out.extend_from_slice(&wasm[..8]);
    let mut cursor = 8;
    while cursor < wasm.len() {
        let section_start = cursor;
        let id = wasm[cursor];
        cursor += 1;
        let len = read_u32_leb(wasm, &mut cursor)? as usize;
        let section_end = cursor.checked_add(len).ok_or("truncated section")?;
        let payload = wasm.get(cursor..section_end).ok_or("truncated section")?;
        cursor = section_end;

        if id == 0 {
            let mut sub = 0;
            let name_len = read_u32_leb(payload, &mut sub)? as usize;
            let name_end = sub.checked_add(name_len).ok_or("truncated custom name")?;
            let name = payload.get(sub..name_end).ok_or("truncated custom name")?;
            if name == COMPILATION_HINTS_SECTION {
                continue;
            }
        } else if id == 2 {
            let mut sub = 0;
            let count = read_u32_leb(payload, &mut sub)?;
            for _ in 0..count {
                let mod_len = read_u32_leb(payload, &mut sub)? as usize;
                sub = sub.checked_add(mod_len).ok_or("truncated import module")?;
                let field_len = read_u32_leb(payload, &mut sub)? as usize;
                sub = sub.checked_add(field_len).ok_or("truncated import name")?;
                let &kind = payload.get(sub).ok_or("truncated import kind")?;
                sub += 1;
                match kind {
                    0 => {
                        let _ = read_u32_leb(payload, &mut sub)?;
                        imported_funcs += 1;
                    }
                    1 => {
                        sub = sub.checked_add(1).ok_or("truncated table import")?;
                        skip_limits(payload, &mut sub)?;
                    }
                    2 => skip_limits(payload, &mut sub)?,
                    3 => {
                        sub = sub.checked_add(2).ok_or("truncated global import")?;
                    }
                    4 => {
                        sub = sub.checked_add(1).ok_or("truncated tag import")?;
                        let _ = read_u32_leb(payload, &mut sub)?;
                    }
                    _ => return Err("unsupported import descriptor kind"),
                }
            }
        } else if id == 3 {
            let mut sub = 0;
            defined_funcs = read_u32_leb(payload, &mut sub)?;
        } else if id == 10 && defined_funcs > 0 {
            let mut custom_payload = Vec::with_capacity(
                1 + COMPILATION_HINTS_SECTION.len() + 5 + defined_funcs as usize * 9,
            );
            write_u32_leb(&mut custom_payload, COMPILATION_HINTS_SECTION.len() as u32);
            custom_payload.extend_from_slice(COMPILATION_HINTS_SECTION);
            write_u32_leb(&mut custom_payload, defined_funcs);
            for offset in 0..defined_funcs {
                write_u32_leb(&mut custom_payload, imported_funcs + offset);
                custom_payload.extend_from_slice(&[1, 0, 1, EAGER_OPTIMIZED_HINT]);
            }
            out.push(0);
            write_u32_leb(&mut out, custom_payload.len() as u32);
            out.extend_from_slice(&custom_payload);
        }

        out.extend_from_slice(&wasm[section_start..section_end]);
    }
    Ok(out)
}

fn add_compilation_hints_to_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    let updated = inject_compilation_hints(&bytes)?;
    fs::write(path, updated)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let trunk_stage = PathBuf::from(env::var("TRUNK_STAGING_DIR")?);
    let stage = trunk_stage.join("browser");
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
        let output = stage.join(format!("{name}_bg.wasm"));
        if env::var("TRUNK_PROFILE").is_ok_and(|profile| profile == "release") {
            println!("{name}: wasm-opt -O3");
            run(Command::new("wasm-opt")
                .arg(&output)
                .args(["-O3", "-o"])
                .arg(&output))?;
        }
        add_compilation_hints_to_file(&output)?;
    }
    for entry in fs::read_dir(&trunk_stage)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "wasm") {
            add_compilation_hints_to_file(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_eager_optimized_compilation_hints_before_code_section_idempotently() {
        // Minimal valid WASM module with 1 imported func (idx 0) and 2 defined funcs (idx 1, 2).
        let module = [
            0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, // header
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // TypeSection: 1 type () -> ()
            0x02, 0x07, 0x01, 0x01, b'a', 0x01, b'b', 0x00,
            0x00, // ImportSection: 1 func import
            0x03, 0x03, 0x02, 0x00, 0x00, // FunctionSection: 2 defined functions
            0x0a, 0x07, 0x02, 0x02, 0x00, 0x0b, 0x02, 0x00,
            0x0b, // CodeSection: 2 empty bodies
        ];
        let injected = inject_compilation_hints(&module).unwrap();
        let injected_twice = inject_compilation_hints(&injected).unwrap();
        assert_eq!(injected, injected_twice);

        let mut cursor = 8;
        let mut custom_payload = None;
        let mut saw_code = false;
        while cursor < injected.len() {
            let id = injected[cursor];
            cursor += 1;
            let len = read_u32_leb(&injected, &mut cursor).unwrap() as usize;
            let payload = &injected[cursor..cursor + len];
            cursor += len;
            if id == 0 {
                assert!(!saw_code, "compilation hints must precede CodeSection");
                custom_payload = Some(payload);
            } else if id == 10 {
                saw_code = true;
            }
        }
        let payload = custom_payload.expect("missing compilation hints section");
        let mut sub = 0;
        let name_len = read_u32_leb(payload, &mut sub).unwrap() as usize;
        assert_eq!(&payload[sub..sub + name_len], COMPILATION_HINTS_SECTION);
        sub += name_len;
        assert_eq!(
            &payload[sub..],
            &[
                2, // 2 defined functions
                1,
                1,
                0,
                1,
                EAGER_OPTIMIZED_HINT, // func 1
                2,
                1,
                0,
                1,
                EAGER_OPTIMIZED_HINT, // func 2
            ]
        );
    }
}
