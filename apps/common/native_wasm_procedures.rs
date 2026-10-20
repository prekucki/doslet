//! Bounded native loading of an offline-generated procedure module and sources.
#[path = "procedure_manifest.rs"]
mod procedure_manifest;
use doslet_runtime::{Session, wasm_procedures::bundle};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs::{self, File},
    io::{Read, Write},
    path::Path,
};

const MAX_MANIFEST: usize = bundle::MAX_METADATA_BYTES;
const MAX_COMPILED: usize = 4 * MAX_WASM;
const COMPILED_MAGIC: &[u8] = b"DCW1";
const MAX_WASM: usize = bundle::MAX_CODE_BYTES;

fn read(path: &Path, limit: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(format!("{} exceeds the AOT input limit", path.display()).into());
    }
    Ok(bytes)
}

fn word(value: &serde_json::Value, key: &str) -> Result<u32, Box<dyn Error>> {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| format!("invalid AOT field {key}").into())
}

struct ProcedureBundle {
    manifest: serde_json::Value,
    module: Vec<u8>,
}

fn verify_hash(
    manifest: &serde_json::Value,
    key: &str,
    parts: &[&[u8]],
) -> Result<(), Box<dyn Error>> {
    let mut digest = Sha256::new();
    for part in parts {
        digest.update(part);
    }
    let hash = format!("{:x}", digest.finalize());
    if manifest.get(key).and_then(serde_json::Value::as_str) != Some(hash.as_str()) {
        return Err("AOT module checksum differs from its metadata".into());
    }
    Ok(())
}

fn embedded(module: Vec<u8>) -> Result<ProcedureBundle, Box<dyn Error>> {
    let data = bundle::read(&module)?.ok_or("AOT WASM module has no procedure metadata")?;
    let manifest: serde_json::Value = serde_json::from_slice(data.metadata)?;
    if word(&manifest, "version")? != 2 {
        return Err("unsupported embedded procedure bundle version".into());
    }
    // The named custom section is excluded to avoid a self-referential hash.
    // Both remaining slices are hashed in order without copying WASM code.
    verify_hash(&manifest, "code_sha256", &[data.before, data.after])?;
    Ok(ProcedureBundle { manifest, module })
}

fn load(path: &Path) -> Result<ProcedureBundle, Box<dyn Error>> {
    let input = read(path, bundle::MAX_BUNDLE_BYTES)?;
    if input.starts_with(b"\0asm") {
        return embedded(input);
    }
    if input.len() > MAX_MANIFEST {
        return Err("AOT JSON manifest exceeds its input limit".into());
    }
    let manifest: serde_json::Value = serde_json::from_slice(&input)?;
    if word(&manifest, "version")? != 1 {
        return Err("unsupported procedure bundle version".into());
    }
    let filename = manifest
        .get("wasm")
        .and_then(serde_json::Value::as_str)
        .ok_or("missing AOT module name")?;
    let relative = Path::new(filename);
    if relative.components().count() != 1
        || !matches!(
            relative.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err("AOT module must be beside its manifest".into());
    }
    let module = read(
        &path.parent().unwrap_or(Path::new(".")).join(relative),
        MAX_WASM,
    )?;
    verify_hash(&manifest, "wasm_sha256", &[&module])?;
    Ok(ProcedureBundle { manifest, module })
}

/// Native code compiled from procedure modules, one file per module and engine.
/// A SHA-256 of the contents turns truncation or corruption into recompilation.
struct DiskCache<'a> {
    directory: &'a Path,
}
impl DiskCache<'_> {
    fn names(wasm: &[u8], engine: u64) -> (String, String) {
        let source = format!("{:x}", Sha256::digest(wasm));
        let name = format!("{source}-{engine:016x}.cwasm");
        (source, name)
    }
}
impl doslet_runtime::wasm_procedures::CompiledCache for DiskCache<'_> {
    fn load(&mut self, wasm: &[u8], engine: u64) -> Option<Vec<u8>> {
        let (_, name) = Self::names(wasm, engine);
        let bytes = read(&self.directory.join(name), MAX_COMPILED).ok()?;
        let (digest, compiled) = bytes.strip_prefix(COMPILED_MAGIC)?.split_at_checked(32)?;
        (Sha256::digest(compiled).as_slice() == digest).then(|| compiled.to_vec())
    }
    fn store(&mut self, wasm: &[u8], engine: u64, compiled: &[u8]) {
        let (source, name) = Self::names(wasm, engine);
        let result = (|| -> std::io::Result<()> {
            fs::create_dir_all(self.directory)?;
            // Compilations of this module by other engines are obsolete.
            for entry in fs::read_dir(self.directory)? {
                let path = entry?.path();
                if path
                    .file_name()
                    .and_then(|file| file.to_str())
                    .is_some_and(|file| {
                        file.starts_with(&source) && file.ends_with(".cwasm") && file != name
                    })
                {
                    let _ = fs::remove_file(path);
                }
            }
            let temporary = self
                .directory
                .join(format!(".{name}.{}.tmp", std::process::id()));
            let mut file = File::create(&temporary)?;
            file.write_all(COMPILED_MAGIC)?;
            file.write_all(&Sha256::digest(compiled))?;
            file.write_all(compiled)?;
            drop(file);
            fs::rename(&temporary, self.directory.join(&name))
        })();
        if let Err(error) = result {
            eprintln!(
                "[WARN doslet] Cannot cache compiled AOT code in {}: {error}",
                self.directory.display()
            );
        }
    }
}

/// What the module's bindings must have been captured under. Bindings are
/// live addresses: a module generated under another DPMI mode or layout
/// names a different copy of the code.
pub enum Expect<'a> {
    /// The session's DPMI mode; older modules carry no mode.
    Dpmi(&'a str),
    /// The key of the live layout, which the module must record.
    Layout(&'a str),
}

fn check(manifest: &serde_json::Value, expect: &Expect<'_>) -> Result<(), Box<dyn Error>> {
    let (key, expected) = match expect {
        Expect::Dpmi(dpmi) => ("dpmi", *dpmi),
        Expect::Layout(layout) => ("layout", *layout),
    };
    match manifest.get(key).map(serde_json::Value::as_str) {
        None if matches!(expect, Expect::Dpmi(_)) => Ok(()),
        Some(Some(value)) if value == expected => Ok(()),
        Some(Some(value)) => Err(format!(
            "AOT module was generated with {key} {value}; this session has {key} {expected}"
        )
        .into()),
        None => Err(format!("AOT module records no {key}").into()),
        Some(None) => Err(format!("invalid AOT field {key}").into()),
    }
}

pub fn install(
    session: &mut Session,
    path: &Path,
    expect: Expect<'_>,
    interpret: bool,
    compiled_cache: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    let ProcedureBundle { manifest, module } = load(path)?;
    check(&manifest, &expect)?;
    let mut bindings = procedure_manifest::bindings(&manifest)?;
    let entry_count = bindings.len();
    if interpret {
        bindings.retain(|binding| {
            binding.kernel.is_some()
                || binding.rle_kernel.is_some()
                || binding.finite_computation.is_some()
        });
        let count = bindings.len();
        session.install_interpreted_procedure_boundaries(bindings)?;
        eprintln!(
            "[INFO doslet] Interpreter: {} bounded procedure boundaries installed; original RAM/VRAM ownership retained ({})",
            count,
            path.display()
        );
    } else {
        match compiled_cache {
            Some(directory) => session.install_wasm_procedures_cached(
                &module,
                bindings,
                &mut DiskCache { directory },
            )?,
            None => session.install_wasm_procedures(&module, bindings)?,
        }
        eprintln!(
            "[INFO doslet] AOT-WASM: {} procedures installed; interpreter and AOT share RAM, VRAM and page generations ({})",
            entry_count,
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "native_wasm_procedures_tests.rs"]
mod tests;
