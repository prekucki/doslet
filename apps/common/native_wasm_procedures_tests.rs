use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};

const EMPTY_MODULE: &[u8] = b"\0asm\x01\0\0\0";

struct Files(std::path::PathBuf);
impl Files {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "doslet-procedure-bundle-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn metadata(code: &[u8]) -> serde_json::Value {
    json!({"version":2,"memory_abi_version":doslet_runtime::wasm_procedures::MEMORY_ABI_VERSION,
        "code_sha256":format!("{:x}",Sha256::digest(code)),"functions":[]})
}

#[test]
fn self_contained_module_loads_without_a_sidecar_by_content() {
    let files = Files::new();
    let expected = metadata(EMPTY_MODULE);
    let module = bundle::append(EMPTY_MODULE, &serde_json::to_vec(&expected).unwrap()).unwrap();
    let path = files.write("single.bin", &module);
    let loaded = load(&path).unwrap();
    assert_eq!(loaded.module, module);
    assert_eq!(loaded.manifest, expected);
    assert_eq!(std::fs::read_dir(&files.0).unwrap().count(), 1);
}

#[test]
fn module_must_match_the_dpmi_mode_or_layout_that_placed_its_code() {
    let mut manifest = metadata(EMPTY_MODULE);
    // Modules generated before the mode was recorded remain loadable.
    check(&manifest, &Expect::Dpmi("direct")).unwrap();
    // Cached modules must name the layout they were bound at.
    assert!(check(&manifest, &Expect::Layout("le-1e8000")).is_err());
    manifest["dpmi"] = json!("direct");
    manifest["layout"] = json!("le-1e8000");
    check(&manifest, &Expect::Dpmi("direct")).unwrap();
    check(&manifest, &Expect::Layout("le-1e8000")).unwrap();
    let error = check(&manifest, &Expect::Dpmi("off"))
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("dpmi direct") && error.contains("dpmi off"),
        "{error}"
    );
    let error = check(&manifest, &Expect::Layout("le-19e000"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("le-1e8000"), "{error}");
    manifest["dpmi"] = json!(1);
    assert!(check(&manifest, &Expect::Dpmi("direct")).is_err());
}

#[test]
fn embedded_hash_covers_code_after_the_metadata_section() {
    // An unrelated custom section follows the metadata. Hash both code pieces
    // in their original order, without including the metadata's own envelope.
    let suffix = [0, 5, 1, b'x', 1, 2, 3];
    let mut code = EMPTY_MODULE.to_vec();
    code.extend_from_slice(&suffix);
    let mut module =
        bundle::append(EMPTY_MODULE, &serde_json::to_vec(&metadata(&code)).unwrap()).unwrap();
    module.extend_from_slice(&suffix);
    assert!(embedded(module.clone()).is_ok());
    *module.last_mut().unwrap() ^= 1;
    assert!(
        embedded(module)
            .err()
            .unwrap()
            .to_string()
            .contains("checksum")
    );
}

#[test]
fn embedded_metadata_requires_its_version_and_code_hash() {
    for value in [
        json!({"version":1}),
        json!({"version":3}),
        json!({"version":2}),
        json!({"version":2,"code_sha256":"bad"}),
    ] {
        let module = bundle::append(EMPTY_MODULE, &serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(embedded(module).is_err());
    }
    assert!(
        embedded(EMPTY_MODULE.to_vec())
            .err()
            .unwrap()
            .to_string()
            .contains("no procedure metadata")
    );
}

#[test]
fn original_json_and_wasm_pair_remains_readable() {
    let files = Files::new();
    files.write("original.wasm", EMPTY_MODULE);
    let expected = json!({"version":1,"wasm":"original.wasm",
        "wasm_sha256":format!("{:x}",Sha256::digest(EMPTY_MODULE)),"functions":[]});
    let path = files.write("original.json", &serde_json::to_vec(&expected).unwrap());
    let loaded = load(&path).unwrap();
    assert_eq!(loaded.manifest, expected);
    assert_eq!(loaded.module, EMPTY_MODULE);
    files.write("original.wasm", b"corrupt");
    assert!(load(&path).err().unwrap().to_string().contains("checksum"));
}

#[test]
fn legacy_manifest_cannot_select_a_module_outside_its_directory() {
    let files = Files::new();
    for name in [
        "../outside.wasm",
        "/absolute.wasm",
        "directory/file.wasm",
        ".",
    ] {
        let path = files.write(
            "invalid.json",
            &serde_json::to_vec(&json!({"version":1,"wasm":name})).unwrap(),
        );
        assert!(
            load(&path)
                .err()
                .unwrap()
                .to_string()
                .contains("beside its manifest")
        );
    }
}

#[test]
fn compiled_code_cache_verifies_contents_and_keeps_one_engine_per_module() {
    use doslet_runtime::wasm_procedures::CompiledCache;
    let files = Files::new();
    let directory = files.0.join("compiled");
    let mut cache = DiskCache {
        directory: &directory,
    };
    assert!(cache.load(EMPTY_MODULE, 1).is_none());
    cache.store(EMPTY_MODULE, 1, b"native");
    assert_eq!(cache.load(EMPTY_MODULE, 1).as_deref(), Some(&b"native"[..]));
    assert!(cache.load(EMPTY_MODULE, 2).is_none());
    cache.store(EMPTY_MODULE, 2, b"newer");
    assert!(cache.load(EMPTY_MODULE, 1).is_none());
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    let path = directory.join(DiskCache::names(EMPTY_MODULE, 2).1);
    let mut bytes = std::fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(cache.load(EMPTY_MODULE, 2).is_none());
}
