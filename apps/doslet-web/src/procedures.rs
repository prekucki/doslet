//! Preserve the native procedure_memory module and its embedded metadata.
//!
//! Offline bindings do not prove a browser loader layout. The browser must
//! verify every live source and capability before executing these procedures.
use doslet_x86_wasm::static_program::{memory, procedure_memory::bundle};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub struct Prepared {
    pub module: Vec<u8>,
    pub entries: usize,
}

/// Use exactly the native application's executable-specific cache: one
/// module per live layout, of which the newest bound under the bundle's DPMI
/// host (`dpmi`, as in its profile) and not declined. DOS4GW places code
/// elsewhere with another host. An explicit observed-code profile selects the
/// register backend instead.
pub fn cached_path(executable_sha256: &str, dpmi: &str) -> crate::Result<Option<PathBuf>> {
    let Ok(dirs) = crate::host_dirs::HostDirs::detect() else {
        return Ok(None);
    };
    newest_module(&dirs.aot_modules().join(executable_sha256), dpmi)
}

fn newest_module(directory: &Path, dpmi: &str) -> crate::Result<Option<PathBuf>> {
    let Ok(layouts) = std::fs::read_dir(directory) else {
        return Ok(None);
    };
    let mut newest = None;
    for layout in layouts {
        let path = layout?.path().join("procedures.wasm");
        if !path.is_file() || path.with_file_name("unavailable.txt").exists() {
            continue;
        }
        let module = crate::read(&path, bundle::MAX_BUNDLE_BYTES)?;
        let Some(embedded) = bundle::read(&module)? else {
            continue;
        };
        let metadata: Value = serde_json::from_slice(embedded.metadata)?;
        if metadata.get("dpmi").and_then(Value::as_str) != Some(dpmi) {
            continue;
        }
        let modified = std::fs::metadata(&path)?.modified()?;
        if newest.as_ref().is_none_or(|(time, _)| modified > *time) {
            newest = Some((modified, path));
        }
    }
    Ok(newest.map(|(_, path)| path))
}

pub fn load(path: &Path, executable_sha256: &str, dpmi: &str) -> crate::Result<Prepared> {
    prepare(
        crate::read(path, bundle::MAX_BUNDLE_BYTES)?,
        executable_sha256,
        dpmi,
    )
}

fn word(value: &Value, key: &str) -> crate::Result<u32> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .ok_or_else(|| format!("invalid procedure bundle field {key}").into())
}

fn prepare(module: Vec<u8>, executable_sha256: &str, dpmi: &str) -> crate::Result<Prepared> {
    let embedded = bundle::read(&module)?.ok_or("WASM module has no procedure metadata")?;
    let metadata: Value = serde_json::from_slice(embedded.metadata)?;
    if word(&metadata, "version")? != 2 {
        return Err("unsupported embedded procedure bundle version".into());
    }
    if word(&metadata, "memory_abi_version")? != memory::VERSION {
        return Err(
            "procedure memory ABI differs; regenerate the bundle with this compiler".into(),
        );
    }
    if metadata.get("exe_sha256").and_then(Value::as_str) != Some(executable_sha256) {
        return Err("procedure bundle identity does not match the selected executable".into());
    }
    // Older modules carry no mode; their live source checks still apply.
    if let Some(mode) = metadata.get("dpmi") {
        let mode = mode.as_str().ok_or("invalid procedure bundle field dpmi")?;
        if mode != dpmi {
            return Err(format!(
                "procedure bundle was generated with DPMI {mode}; this bundle uses DPMI {dpmi}"
            )
            .into());
        }
    }
    let mut digest = Sha256::new();
    digest.update(embedded.before);
    digest.update(embedded.after);
    let code_hash = format!("{:x}", digest.finalize());
    if metadata.get("code_sha256").and_then(Value::as_str) != Some(code_hash.as_str()) {
        return Err("procedure module checksum differs from its metadata".into());
    }
    let entries = metadata
        .get("functions")
        .and_then(Value::as_array)
        .filter(|entries| !entries.is_empty() && entries.len() <= 4096)
        .ok_or("invalid procedure bundle function count")?;
    let mut ids = std::collections::BTreeSet::new();
    let mut addresses = std::collections::BTreeSet::new();
    let mut total_sources = 0usize;
    for entry in entries {
        if !ids.insert(word(entry, "id")?) || !addresses.insert(word(entry, "entry")?) {
            return Err("duplicate procedure function id or entry".into());
        }
        if word(entry, "maximum_work")? == 0 {
            return Err("procedure maximum work must be positive".into());
        }
        let sources = entry
            .get("sources")
            .and_then(Value::as_array)
            .filter(|sources| !sources.is_empty())
            .ok_or("missing procedure sources")?;
        total_sources = total_sources
            .checked_add(sources.len())
            .filter(|&count| count <= 65_536)
            .ok_or("too many procedure sources")?;
        for source in sources {
            let bytes = source
                .get("bytes")
                .and_then(Value::as_array)
                .filter(|bytes| (1..=15).contains(&bytes.len()))
                .ok_or("invalid procedure instruction length")?;
            if bytes.iter().any(|byte| {
                byte.as_u64()
                    .and_then(|byte| u8::try_from(byte).ok())
                    .is_none()
            }) {
                return Err("invalid procedure source byte".into());
            }
            if u64::from(word(source, "linear")?) + bytes.len() as u64 > 1u64 << 32 {
                return Err("procedure source address overflow".into());
            }
        }
    }
    let entries = entries.len();
    Ok(Prepared { module, entries })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // The packager checks framing and authenticated metadata; executable WASM
    // sections and live source proofs remain the browser engine's responsibility.
    const MODULE: &[u8] = b"\0asm\x01\0\0\0";
    fn metadata() -> Value {
        json!({"version":2,"memory_abi_version":memory::VERSION,
            "exe_sha256":"test-executable","code_sha256":crate::hash(MODULE),
            "functions":[{"id":7,"entry":0x20000,"maximum_work":2,
                "sources":[{"linear":0x20000,"bytes":[0x40,0xc3]}]}]})
    }
    fn packaged(metadata: &Value) -> Vec<u8> {
        bundle::append(MODULE, &serde_json::to_vec(metadata).unwrap()).unwrap()
    }
    #[test]
    fn preserves_native_bundle_bytes_and_rejects_different_identity_or_code() {
        let module = packaged(&metadata());
        let prepared = prepare(module.clone(), "test-executable", "direct").unwrap();
        assert_eq!(prepared.module, module);
        assert_eq!(prepared.entries, 1);
        assert!(prepare(module, "other-executable", "direct").is_err());
        let mut metadata = metadata();
        metadata["code_sha256"] = json!("wrong");
        assert!(prepare(packaged(&metadata), "test-executable", "direct").is_err());
    }
    #[test]
    fn accepts_only_the_copy_bound_under_the_bundle_dpmi() {
        let mut metadata = metadata();
        metadata["dpmi"] = json!("direct");
        assert!(prepare(packaged(&metadata), "test-executable", "direct").is_ok());
        metadata["dpmi"] = json!("off");
        let error = prepare(packaged(&metadata), "test-executable", "direct")
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("DPMI off"), "{error}");
        // A profile naming machine.dpmi = "off" packages the copy bound there.
        assert!(prepare(packaged(&metadata), "test-executable", "off").is_ok());
        metadata["dpmi"] = json!(1);
        assert!(prepare(packaged(&metadata), "test-executable", "direct").is_err());
    }
    #[test]
    fn picks_a_cached_layout_bound_under_the_bundle_dpmi() {
        let directory =
            std::env::temp_dir().join(format!("doslet-web-layouts-{}", std::process::id()));
        let write = |layout: &str, dpmi: &str, declined: bool| {
            let mut metadata = metadata();
            metadata["dpmi"] = json!(dpmi);
            let path = directory.join(layout).join("procedures.wasm");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, packaged(&metadata)).unwrap();
            if declined {
                std::fs::write(path.with_file_name("unavailable.txt"), "short calls").unwrap();
            }
            path
        };
        assert_eq!(newest_module(&directory, "direct").unwrap(), None);
        let off = write("le-19e000-240000-00000000", "off", false);
        write("le-1e8000-28b000-11111111", "direct", true);
        assert_eq!(newest_module(&directory, "direct").unwrap(), None);
        assert_eq!(newest_module(&directory, "off").unwrap(), Some(off));
        let direct = write("le-1e8000-28b000-22222222", "direct", false);
        assert_eq!(newest_module(&directory, "direct").unwrap(), Some(direct));
        std::fs::remove_dir_all(&directory).unwrap();
    }
    #[test]
    fn rejects_unsupported_or_unbounded_metadata_before_packaging() {
        for change in [
            ("version", json!(1)),
            ("memory_abi_version", json!(memory::VERSION + 1)),
            ("functions", json!([])),
        ] {
            let mut metadata = metadata();
            metadata[change.0] = change.1;
            assert!(prepare(packaged(&metadata), "test-executable", "direct").is_err());
        }
        let mut invalid = metadata();
        invalid["functions"][0]["sources"][0]["bytes"] = json!([256]);
        assert!(prepare(packaged(&invalid), "test-executable", "direct").is_err());
        invalid = metadata();
        invalid["functions"][0]["sources"][0]["linear"] = json!(u32::MAX);
        assert!(prepare(packaged(&invalid), "test-executable", "direct").is_err());
        invalid = metadata();
        let duplicate = invalid["functions"][0].clone();
        invalid["functions"].as_array_mut().unwrap().push(duplicate);
        assert!(prepare(packaged(&invalid), "test-executable", "direct").is_err());
    }
}
