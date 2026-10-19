//! Bounded native loading of an offline-generated procedure module and sources.
use doslet_runtime::{
    Session,
    wasm_procedures::{
        Binding, CodePart, CopyKernel, FiniteComputation, FiniteLoopGuard, KernelInterrupts,
        RleKernel, bundle,
    },
};
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
const MAX_FUNCTIONS: usize = 4096;
const MAX_SOURCES: usize = 65_536;

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

fn terminals(value: &serde_json::Value, key: &str) -> Result<Vec<u32>, Box<dyn Error>> {
    let array = value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .filter(|array| array.len() <= 256)
        .ok_or("invalid finite computation terminals")?;
    array
        .iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or_else(|| "invalid finite computation terminal".into())
        })
        .collect()
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

pub fn install(
    session: &mut Session,
    path: &Path,
    interpret: bool,
    compiled_cache: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    let ProcedureBundle { manifest, module } = load(path)?;
    if word(&manifest, "memory_abi_version")? != doslet_runtime::wasm_procedures::MEMORY_ABI_VERSION
    {
        return Err(
            "AOT memory ABI differs; regenerate the procedure bundle with this compiler".into(),
        );
    }
    let entries = manifest
        .get("functions")
        .and_then(serde_json::Value::as_array)
        .ok_or("missing AOT functions")?;
    if entries.is_empty() || entries.len() > MAX_FUNCTIONS {
        return Err("invalid AOT function count".into());
    }
    let mut bindings = Vec::with_capacity(entries.len());
    let mut source_count = 0usize;
    for entry in entries {
        let sources = entry
            .get("sources")
            .and_then(serde_json::Value::as_array)
            .ok_or("missing AOT sources")?;
        source_count = source_count
            .checked_add(sources.len())
            .filter(|&n| n <= MAX_SOURCES)
            .ok_or("too many AOT sources")?;
        let mut code = Vec::with_capacity(sources.len());
        for source in sources {
            let values = source
                .get("bytes")
                .and_then(serde_json::Value::as_array)
                .ok_or("missing AOT source bytes")?;
            if !(1..=15).contains(&values.len()) {
                return Err("invalid AOT instruction length".into());
            }
            let mut expected = Vec::with_capacity(values.len());
            for value in values {
                expected.push(
                    value
                        .as_u64()
                        .and_then(|v| u8::try_from(v).ok())
                        .ok_or("invalid AOT source byte")?,
                );
            }
            code.push(CodePart {
                linear: word(source, "linear")?,
                expected,
            });
        }
        bindings.push(Binding {
            function_id: word(entry, "id")?,
            entry_ip: word(entry, "entry")?,
            maximum_instructions: word(entry, "maximum_work")?,
            kernel: match entry.get("semantic_kernel") {
                None | Some(serde_json::Value::Null) => None,
                Some(kernel)
                    if kernel.get("kind").and_then(serde_json::Value::as_str)
                        == Some("rle-decode") =>
                {
                    None
                }
                Some(kernel) => {
                    if kernel.get("kind").and_then(serde_json::Value::as_str)
                        != Some("paired-strided-copy")
                    {
                        return Err("unsupported AOT semantic kernel".into());
                    }
                    let kernel = CopyKernel {
                        stride: word(kernel, "stride")?,
                        max_work_per_call: word(kernel, "maximum_work")?,
                        prefix_nops: word(kernel, "prefix_nops")?,
                        tail_nops: word(kernel, "tail_nops")?,
                    };
                    if !kernel.is_valid() {
                        return Err("invalid AOT semantic kernel metadata".into());
                    }
                    Some(kernel)
                }
            },
            rle_kernel: match entry.get("semantic_kernel") {
                Some(kernel)
                    if kernel.get("kind").and_then(serde_json::Value::as_str)
                        == Some("rle-decode") =>
                {
                    if u64::from(word(kernel, "maximum_work")?) != RleKernel.maximum_work() {
                        return Err("invalid RLE kernel work declaration".into());
                    }
                    Some(RleKernel)
                }
                _ => None,
            },
            finite_computation: match entry.get("finite_computation") {
                None | Some(serde_json::Value::Null) => None,
                Some(finite) => Some(FiniteComputation {
                    exits: terminals(finite, "exits")?,
                    returns: terminals(finite, "returns")?,
                    guards: match finite.get("guards") {
                        None => Vec::new(),
                        Some(guards) => {
                            let guards = guards
                                .as_array()
                                .filter(|array| array.len() <= 1)
                                .ok_or("invalid finite loop guard list")?;
                            let mut result = Vec::with_capacity(guards.len());
                            for guard in guards {
                                result.push(FiniteLoopGuard {
                                    entry_ip: word(guard, "entry")?,
                                    counter: u8::try_from(word(guard, "counter")?)?,
                                    decrement: guard
                                        .get("decrement")
                                        .and_then(serde_json::Value::as_bool)
                                        .ok_or("invalid finite loop direction")?,
                                    maximum_trips: word(guard, "maximum_trips")?,
                                });
                            }
                            result
                        }
                    },
                }),
            },
            kernel_interrupts: match entry
                .get("kernel_interrupts")
                .and_then(serde_json::Value::as_str)
            {
                None | Some("defer") => KernelInterrupts::Defer,
                Some("before-irq") => KernelInterrupts::BeforeNextIrq,
                Some(_) => return Err("unsupported AOT kernel interrupt policy".into()),
            },
            code,
        });
    }
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
            entries.len(),
            path.display()
        );
    }
    Ok(())
}

#[cfg(test)]
#[path = "native_wasm_procedures_tests.rs"]
mod tests;
