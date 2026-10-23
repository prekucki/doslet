//! Rebuild the pinned upstream CPU and JIT without a Node runtime.
use boa_engine::{Context, Source};
use std::{env, error::Error, fs, path::PathBuf, process::Command};

fn checked(command: &mut Command) -> Result<(), Box<dyn Error>> {
    let status = command.status()?;
    if !status.success() {
        return Err(format!("command failed: {command:?}: {status}").into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    // rustc invokes this executable as its linker; preserve upstream's state
    // area below the global base and expose the table used by generated code.
    if args.first().is_some_and(|arg| arg == "-flavor") {
        let sysroot = Command::new("rustc")
            .args(["--print", "sysroot"])
            .output()?;
        let version = Command::new("rustc").args(["-vV"]).output()?;
        let version = String::from_utf8(version.stdout)?;
        let host = version
            .lines()
            .find_map(|line| line.strip_prefix("host: "))
            .ok_or("missing rustc host")?;
        let linker = PathBuf::from(String::from_utf8(sysroot.stdout)?.trim())
            .join("lib/rustlib")
            .join(host)
            .join("bin/rust-lld");
        let args = args
            .iter()
            .filter(|arg| !matches!(arg.to_str(), Some("--stack-first" | "--export-table")));
        return checked(Command::new(linker).args(args).arg("--no-stack-first"));
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let vendor = root.join("vendor/v86");
    let generated = vendor.join("rust/gen");
    fs::create_dir_all(&generated)?;
    for kind in ["interpreter", "jit", "analyzer"] {
        let mut context = Context::default();
        let script = format!(
            r#"
            const outputs = [];
            function assert(value, message) {{ if(!value) throw new Error(message || 'assertion failed'); }}
            function hex(n, pad) {{ return n.toString(16).toUpperCase().padStart(pad || 0, '0'); }}
            function get_switch_value(_) {{ return null; }}
            function get_switch_exist(_) {{ return true; }}
            const OUT_DIR = '';
            function finalize_table_rust(_, name, contents) {{ outputs.push([name, contents]); }}
            const x86_table = (() => {{ {} }})();
            const rust_ast = (() => {{ {}; return {{ print_syntax_tree }}; }})();
            {};
            JSON.stringify(outputs);
        "#,
            fs::read_to_string(vendor.join("gen/x86_table.js"))?,
            fs::read_to_string(vendor.join("gen/rust_ast.js"))?,
            fs::read_to_string(vendor.join(format!("gen/generate_{kind}.js")))?
        );
        let value = context
            .eval(Source::from_bytes(&script))
            .map_err(|error| format!("{kind} generator: {error}"))?;
        let output = value
            .to_string(&mut context)
            .map_err(|error| error.to_string())?
            .to_std_string_escaped();
        let outputs: Vec<(String, String)> = serde_json::from_str(&output)?;
        for (name, contents) in outputs {
            if !matches!(
                name.as_str(),
                "interpreter.rs"
                    | "interpreter0f.rs"
                    | "jit.rs"
                    | "jit0f.rs"
                    | "analyzer.rs"
                    | "analyzer0f.rs"
            ) {
                return Err("unexpected generated file".into());
            }
            fs::write(generated.join(name), contents)?;
        }
    }
    let out = root.join("target/v86");
    fs::create_dir_all(&out)?;
    checked(
        Command::new("clang")
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
            .arg(out.join("softfloat.o")),
    )?;
    let linker = env::current_exe()?;
    checked(
        Command::new("cargo")
            .args(["rustc", "--manifest-path"])
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
            .args(["--", "-C"])
            .arg(format!("linker={}", linker.display()))
            .args([
                "-C",
                "link-arg=--import-table",
                "-C",
                "link-arg=--global-base=4096",
                "-C",
            ])
            .arg(format!("link-arg={}", out.join("softfloat.o").display()))
            .args(["-C", "target-feature=+bulk-memory,+multivalue,+simd128"]),
    )?;
    fs::copy(
        out.join("build/wasm32-unknown-unknown/release/doslet_v86_module.wasm"),
        out.join("doslet-v86.wasm"),
    )?;
    println!("Built {}", out.join("doslet-v86.wasm").display());
    Ok(())
}
