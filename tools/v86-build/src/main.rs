//! Rebuild the pinned upstream CPU and JIT without a Node runtime.
use boa_engine::{Context, Source};
use std::{env, error::Error, fs, path::PathBuf};

mod module;

fn main() -> Result<(), Box<dyn Error>> {
    if module::link_if_requested()? {
        return Ok(());
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
    let wasm = module::build(&root, &root.join("target/v86"))?;
    println!("Built {}", wasm.display());
    Ok(())
}
