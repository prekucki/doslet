//! Trunk post-build hook. Rust tools only; no separate JS build system.
use std::{
    env, fs,
    hash::{Hash, Hasher},
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

/// wasm-bindgen renames the Rust function table. Export a stable alias for
/// v86's generated functions, distinct from the externref table it also adds.
fn export_v86_table(wasm: &[u8]) -> Result<Vec<u8>, &'static str> {
    if wasm.get(..8) != Some(b"\0asm\x01\0\0\0".as_slice()) {
        return Err("invalid WASM");
    }
    let mut out = wasm[..8].to_vec();
    let mut cursor = 8;
    let mut found = false;
    while cursor < wasm.len() {
        let start = cursor;
        let id = wasm[cursor];
        cursor += 1;
        let len = read_u32_leb(wasm, &mut cursor)? as usize;
        let end = cursor.checked_add(len).ok_or("section overflow")?;
        let payload = wasm.get(cursor..end).ok_or("truncated section")?;
        if id == 7 {
            let mut sub = 0;
            let count = read_u32_leb(payload, &mut sub)?;
            let entries = sub;
            let mut table = false;
            for _ in 0..count {
                let name_len = read_u32_leb(payload, &mut sub)? as usize;
                sub = sub.checked_add(name_len).ok_or("export overflow")?;
                let kind = *payload.get(sub).ok_or("truncated export")?;
                sub += 1;
                let index = read_u32_leb(payload, &mut sub)?;
                table |= kind == 1 && index == 0;
            }
            if !table {
                return Err("v86 worker has no exported Rust function table");
            }
            let name = b"doslet_v86_table";
            let mut exports = Vec::new();
            write_u32_leb(&mut exports, count + 1);
            exports.extend_from_slice(&payload[entries..]);
            write_u32_leb(&mut exports, name.len() as u32);
            exports.extend_from_slice(name);
            exports.extend_from_slice(&[1, 0]);
            out.push(7);
            write_u32_leb(&mut out, exports.len() as u32);
            out.extend_from_slice(&exports);
            found = true;
        } else {
            out.extend_from_slice(&wasm[start..end]);
        }
        cursor = end;
    }
    if !found {
        return Err("v86 worker has no export section");
    }
    Ok(out)
}

fn add_compilation_hints_to_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = fs::read(path)?;
    let updated = inject_compilation_hints(&bytes)?;
    fs::write(path, updated)?;
    Ok(())
}

fn v86_exports(
    directory: &Path,
    names: &mut std::collections::BTreeSet<String>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            v86_exports(&path, names)?;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let source = fs::read_to_string(path)?;
            for item in source.split("#[no_mangle]").skip(1) {
                if let Some((_, signature)) = item.split_once("fn ") {
                    let name = signature.split('(').next().unwrap_or("").trim();
                    if !name.is_empty()
                        && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
                    {
                        names.insert(name.to_owned());
                    }
                }
            }
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "-flavor") {
        // Reserve v86's fixed register page before globals, stack and heap.
        let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let sysroot = Command::new(&rustc).args(["--print", "sysroot"]).output()?;
        let version = Command::new(&rustc).arg("-vV").output()?;
        let version = String::from_utf8(version.stdout)?;
        let host = version
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .ok_or("missing rustc host")?;
        let linker = PathBuf::from(String::from_utf8(sysroot.stdout)?.trim())
            .join("lib/rustlib")
            .join(host)
            .join(format!("bin/rust-lld{}", env::consts::EXE_SUFFIX));
        let mut exports = std::collections::BTreeSet::new();
        v86_exports(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/v86/rust"),
            &mut exports,
        )?;
        let mut command = Command::new(linker);
        command
            .args(
                args.iter()
                    .filter(|arg| arg.to_str() != Some("--stack-first")),
            )
            .args([
                "--no-stack-first",
                "--global-base=4096",
                "--export-table",
                "--growable-table",
            ]);
        // JIT modules need upstream's C-ABI helpers, not every public Rust
        // symbol (which would retain unprocessed wasm-bindgen intrinsics).
        for name in exports {
            command.arg(format!("--export-if-defined={name}"));
        }
        return run(&mut command);
    }
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
    let v86 = env::var("DOSLET_WEB_MODE").is_ok_and(|mode| mode == "v86");
    if v86 {
        // The pinned core's SoftFloat implementation is compiled with Clang;
        // instruction tables are checked in, so no JavaScript build runtime runs.
        let object = root.join("target/v86/browser-softfloat.o");
        fs::create_dir_all(object.parent().unwrap())?;
        run(
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
                .arg(root.join("vendor/v86/softfloat/softfloat.c"))
                .arg("-o")
                .arg(&object),
        )?;
        // Cargo tracks linker arguments, not the contents of a linked object
        // or this linker wrapper. Make both part of its compilation cache key.
        let mut inputs = std::collections::hash_map::DefaultHasher::new();
        fs::read(&object)?.hash(&mut inputs);
        include_bytes!("main.rs").hash(&mut inputs);
        let mut worker = Command::new("cargo");
        worker
            .current_dir(&root)
            .args([
                "rustc",
                "--locked",
                "--target",
                "wasm32-unknown-unknown",
                "--profile",
                "web",
                "-p",
                "doslet-web-worker",
                "--features",
                "v86",
                "--",
                "-C",
            ])
            .arg(format!("linker={}", env::current_exe()?.display()))
            .arg("-C")
            .arg(format!("link-arg={}", object.display()))
            .args(["-C", "link-arg=--export=doslet_run", "--cfg"])
            .arg(format!("doslet_build_inputs=\"{:016x}\"", inputs.finish()));
        run(&mut worker)?;
        run(Command::new("cargo").current_dir(&root).args([
            "build",
            "--locked",
            "--target",
            "wasm32-unknown-unknown",
            "--profile",
            "web",
            "-p",
            "doslet-web-audio",
        ]))?;
        fs::write(stage.join("v86-runtime.json"), "{\"abi\":4}\n")?;
    } else {
        run(&mut cargo)?;
    }
    for name in ["doslet_web_worker", "doslet_web_audio"] {
        let mut wasm = root.join(format!("target/wasm32-unknown-unknown/web/{name}.wasm"));
        if v86 && name == "doslet_web_worker" {
            let input = root.join("target/v86/browser-bindgen/doslet_web_worker.wasm");
            fs::create_dir_all(input.parent().unwrap())?;
            fs::write(&input, export_v86_table(&fs::read(&wasm)?)?)?;
            wasm = input;
        }
        run(Command::new("wasm-bindgen")
            .arg(wasm)
            .args(["--target", "web", "--no-typescript", "--out-dir"])
            .arg(&stage))?;
        let output = stage.join(format!("{name}_bg.wasm"));
        if env::var("TRUNK_PROFILE").is_ok_and(|profile| profile == "release") {
            println!("{name}: wasm-opt -O3");
            let mut optimize = Command::new("wasm-opt");
            optimize.arg(&output).args(["-O3", "-o"]).arg(&output);
            if v86 && name == "doslet_web_worker" {
                optimize.arg("--enable-exception-handling");
            }
            run(&mut optimize)?;
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
    fn v86_alias_selects_function_table_and_rejects_missing_table() {
        let mut module = b"\0asm\x01\0\0\0".to_vec();
        // Two exports: Rust's function table 0 and wasm-bindgen externrefs 1.
        module.extend_from_slice(&[7, 9, 2, 1, b'f', 1, 0, 1, b'e', 1, 1]);
        let aliased = export_v86_table(&module).unwrap();
        assert!(aliased.ends_with(b"\x10doslet_v86_table\x01\x00"));
        assert_eq!(aliased[10], 3);
        assert!(export_v86_table(b"\0asm\x01\0\0\0").is_err());
        module[14] = 1;
        assert!(export_v86_table(&module).is_err());
    }

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
