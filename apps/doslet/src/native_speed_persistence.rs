//! Portable learned labels, selected by executable content and atomically saved.
use crate::speed_tiers::{Node, Profile, Tier};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    thread::{self, JoinHandle},
};

type Nodes = BTreeMap<(u32, bool), Node>;
type CachedGame = (PathBuf, [Option<u16>; 3], bool);
const LIMIT: u64 = 64 * 1024 * 1024;

pub(super) struct Persistence {
    path: PathBuf,
    identity: String,
    rates: [Option<u16>; 3],
    pub learning: bool,
    writer: Option<JoinHandle<Result<(), String>>>,
    dirty: bool,
    pub loaded: u64,
    pub writes: u64,
    pub error: Option<String>,
}
impl Persistence {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn open(
        path: &Path,
        profile: &mut Profile,
        rates: [Option<u16>; 3],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let identity = format!("profile-sha256:{}", fingerprint(path)?);
        let mut name = path.as_os_str().to_owned();
        name.push(".learned.json");
        Ok(Self::at(name.into(), identity, profile, rates))
    }
    pub fn game(
        exe: &Path,
        root: Option<&Path>,
        profile: &mut Profile,
        rates: [Option<u16>; 3],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (path, identity) = game_location(exe, root)?;
        Ok(Self::at(path, identity, profile, rates))
    }
    pub fn game_bytes(
        bytes: &[u8],
        root: Option<&Path>,
        profile: &mut Profile,
        rates: [Option<u16>; 3],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let hash = format!("{:x}", Sha256::digest(bytes));
        let (path, identity) = game_location_hash(&hash, root)?;
        Ok(Self::at(path, identity, profile, rates))
    }
    fn at(path: PathBuf, identity: String, profile: &mut Profile, rates: [Option<u16>; 3]) -> Self {
        let mut this = Self {
            path,
            identity,
            rates,
            learning: false,
            writer: None,
            dirty: false,
            loaded: 0,
            writes: 0,
            error: None,
        };
        match this.read(profile) {
            Ok(loaded) => this.loaded = loaded,
            Err(error) => this.error = Some(format!("Learned tiers: {error}; using base profile")),
        }
        this
    }
    fn read(&self, profile: &mut Profile) -> Result<u64, Box<dyn std::error::Error>> {
        let Some(value) = read_value(&self.path)? else {
            return Ok(0);
        };
        validate_identity(&value, &self.identity)?;
        let learned = Profile::from_json(&value, None, false)?;
        if learned.unknown != Tier::Grey || !learned.irq_grey {
            return Err("invalid learned policy".into());
        }
        let mut combined = profile.nodes.clone();
        let mut loaded = 0;
        for (key, node) in learned.nodes {
            if let Some(base) = combined.get(&key) {
                if base.code != node.code || base.locked || base.manual {
                    continue;
                }
                if !node.manual && base.tier != Tier::Grey && base.tier != node.tier {
                    continue;
                }
            }
            loaded += u64::from(node.tier != Tier::Grey);
            combined.insert(key, node);
        }
        if combined.len() > 100_000 {
            return Err("profile node limit".into());
        }
        profile.nodes = combined;
        Ok(loaded)
    }
    pub fn changed(&mut self) {
        self.dirty = true;
    }
    pub fn set_rates(&mut self, rates: [Option<u16>; 3]) {
        if self.rates != rates {
            self.rates = rates;
            self.changed();
        }
    }
    pub fn pending(&self) -> bool {
        self.dirty || self.writer.is_some()
    }
    pub fn poll(&mut self, profile: &Profile) {
        if self.writer.as_ref().is_some_and(|w| w.is_finished()) {
            self.join();
        }
        if !self.dirty || self.writer.is_some() {
            return;
        }
        let (path, identity, rates, learning, nodes) = (
            self.path.clone(),
            self.identity.clone(),
            self.rates,
            self.learning,
            profile.nodes.clone(),
        );
        // One writer at a time. Changes during a write coalesce into the next snapshot.
        self.dirty = false;
        match thread::Builder::new()
            .name("doslet-tier-save".into())
            .spawn(move || {
                write(&path, &identity, rates, learning, &nodes).map_err(|e| e.to_string())
            }) {
            Ok(writer) => self.writer = Some(writer),
            Err(e) => self.error = Some(format!("Cannot save learned tiers: {e}")),
        }
    }
    fn join(&mut self) {
        if let Some(writer) = self.writer.take() {
            match writer
                .join()
                .unwrap_or_else(|_| Err("writer panicked".into()))
            {
                Ok(()) => self.writes += 1,
                Err(e) => self.error = Some(format!("Cannot save learned tiers: {e}")),
            }
        }
    }
    pub fn finish(&mut self, profile: &Profile) {
        self.join();
        if self.dirty {
            self.dirty = false;
            match write(
                &self.path,
                &self.identity,
                self.rates,
                self.learning,
                &profile.nodes,
            ) {
                Ok(()) => self.writes += 1,
                Err(e) => self.error = Some(format!("Cannot save learned tiers: {e}")),
            }
        }
    }
}
pub(super) fn cached_game(
    exe: &Path,
    root: Option<&Path>,
) -> Result<Option<CachedGame>, Box<dyn std::error::Error>> {
    let (path, identity) = game_location(exe, root)?;
    let Some(value) = read_value(&path)? else {
        return Ok(None);
    };
    validate_identity(&value, &identity)?;
    let rates: [Option<u16>; 3] = serde_json::from_value(value["rates"].clone())?;
    if rates[1..].iter().any(Option::is_none)
        || rates.iter().flatten().any(|r| !(1..=1000).contains(r))
    {
        return Err("invalid saved tier rates".into());
    }
    let learning = value["learning"]
        .as_bool()
        .ok_or("missing learning setting")?;
    Ok(Some((path, rates, learning)))
}
pub(super) fn read_value(
    path: &Path,
) -> Result<Option<serde_json::Value>, Box<dyn std::error::Error>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err("learned profile exceeds 64 MiB".into());
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}
fn validate_identity(
    value: &serde_json::Value,
    identity: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    if value["learning_version"] != 1 || value["identity"].as_str() != Some(identity) {
        return Err(
            "incompatible learned profile or different executable/profile fingerprint".into(),
        );
    }
    Ok(())
}
pub(super) fn fingerprint(path: &Path) -> Result<String, std::io::Error> {
    let mut input = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = [0; 16 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn game_location(
    exe: &Path,
    root: Option<&Path>,
) -> Result<(PathBuf, String), Box<dyn std::error::Error>> {
    let hash = fingerprint(exe)?;
    game_location_hash(&hash, root)
}
fn game_location_hash(
    hash: &str,
    root: Option<&Path>,
) -> Result<(PathBuf, String), Box<dyn std::error::Error>> {
    let root = match root {
        Some(path) => path.to_owned(),
        None => default_directory()?,
    };
    Ok((
        root.join(format!("{hash}.json")),
        format!("exe-sha256:{hash}"),
    ))
}
/// Labels are cache. Earlier versions kept them as data; the first use moves
/// that directory, and a failed move keeps using it.
pub(super) fn default_directory() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dirs = crate::host_dirs::HostDirs::detect()
        .map_err(|error| format!("{error}; use --speed-tier-cache"))?;
    Ok(adopt_legacy(dirs.legacy_speed_tiers(), dirs.speed_tiers()))
}
fn adopt_legacy(legacy: PathBuf, directory: PathBuf) -> PathBuf {
    if directory.exists() || !legacy.is_dir() {
        return directory;
    }
    let moved = directory
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::rename(&legacy, &directory));
    match moved {
        Ok(()) => directory,
        Err(error) => {
            eprintln!(
                "[WARN doslet] Cannot move {} to {}: {error}",
                legacy.display(),
                directory.display()
            );
            legacy
        }
    }
}

fn write(
    path: &Path,
    identity: &str,
    rates: [Option<u16>; 3],
    learning: bool,
    nodes: &Nodes,
) -> Result<(), Box<dyn std::error::Error>> {
    let rows = node_rows(nodes);
    let bytes = serde_json::to_vec(&serde_json::json!({"learning_version":1,
        "identity":identity, "rates":rates, "learning":learning, "unknown":"grey", "irq_policy":"static-grey", "nodes":rows}))?;
    write_bytes(path, &bytes)
}
fn node_rows(nodes: &Nodes) -> Vec<serde_json::Value> {
    nodes.iter().map(|(&(pc, wide), n)| serde_json::json!({
        "pc":pc, "bits":if wide {32} else {16}, "bytes":n.code, "tier":n.tier.name(),
        "reason":if n.manual {"manual"} else if n.tier == Tier::Irq {"interrupt-frame"} else if n.locked {"code-conflict"} else {"learned"}
    })).collect()
}
/// Export a standalone profile. Existing files are never replaced by Save as.
pub(super) fn export_new(
    path: &Path,
    rates: [Option<u16>; 3],
    nodes: &Nodes,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes = serde_json::to_vec_pretty(&serde_json::json!({"version":1,
        "rates":rates, "learning":false, "unknown":"grey", "irq_policy":"static-grey", "nodes":node_rows(nodes)}))?;
    if bytes.len() as u64 > LIMIT {
        return Err("profile exceeds 64 MiB".into());
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error.into());
    }
    Ok(())
}
fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    if bytes.len() as u64 > LIMIT {
        return Err("learned profile exceeds 64 MiB".into());
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary = PathBuf::from(temporary);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        #[cfg(unix)]
        File::open(
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )?
        .sync_all()?;
        Ok::<_, std::io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_data_directory_moves_once_into_the_cache() {
        let f = Fixture::new();
        let legacy = f.0.join("data/speed-tiers");
        let directory = f.0.join("cache/speed-tiers");
        fs::create_dir_all(legacy.join("profiles")).unwrap();
        fs::write(legacy.join("game.json"), b"{}").unwrap();
        assert_eq!(adopt_legacy(legacy.clone(), directory.clone()), directory);
        assert_eq!(fs::read(directory.join("game.json")).unwrap(), b"{}");
        assert!(directory.join("profiles").is_dir() && !legacy.exists());
        fs::create_dir_all(&legacy).unwrap();
        assert_eq!(adopt_legacy(legacy.clone(), directory.clone()), directory);
        assert!(legacy.exists());
    }
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "doslet-tier-store-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("GAME.EXE"), b"first executable").unwrap();
            Self(path)
        }
        fn exe(&self) -> PathBuf {
            self.0.join("GAME.EXE")
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn profile() -> Profile {
        Profile::from_json(
            &serde_json::json!({"unknown":"grey", "irq_policy":"static-grey", "nodes":[{
                "pc":256,"bits":16,"bytes":vec![0x90;15],"tier":"grey"
            }]}),
            None,
            false,
        )
        .unwrap()
    }
    #[test]
    fn saves_latest_labels_and_restores_after_executable_moves() {
        let f = Fixture::new();
        let mut current = profile();
        let mut store = Persistence::game(
            &f.exe(),
            Some(&f.0),
            &mut current,
            [Some(500), Some(8), Some(100)],
        )
        .unwrap();
        store.learning = true;
        current.nodes.get_mut(&(256, false)).unwrap().tier = Tier::Fast;
        current.nodes.get_mut(&(256, false)).unwrap().manual = true;
        store.changed();
        store.poll(&current);
        for (address, tier) in [(257, Tier::Slow), (258, Tier::Irq)] {
            current.nodes.insert(
                (address, false),
                Node {
                    code: [0x90; 15],
                    tier,
                    locked: tier == Tier::Irq,
                    manual: false,
                },
            );
        }
        store.changed();
        store.finish(&current);
        assert!(!store.pending());
        assert_eq!(store.writes, 2);
        assert!(store.error.is_none());
        let renamed = f.0.join("RENAMED.EXE");
        fs::copy(f.exe(), &renamed).unwrap();
        let mut restored = profile();
        let next = Persistence::game(
            &renamed,
            Some(&f.0),
            &mut restored,
            [Some(500), Some(8), Some(100)],
        )
        .unwrap();
        assert_eq!(next.loaded, 3);
        assert!(restored.nodes[&(256, false)].tier == Tier::Fast);
        assert!(restored.nodes[&(256, false)].manual);
        assert!(restored.nodes[&(257, false)].tier == Tier::Slow);
        assert!(restored.nodes[&(258, false)].tier == Tier::Irq);
        let (_, rates, learn) = cached_game(&renamed, Some(&f.0)).unwrap().unwrap();
        assert_eq!(rates, [Some(500), Some(8), Some(100)]);
        assert!(learn);
        fs::write(&renamed, b"different executable").unwrap();
        assert!(cached_game(&renamed, Some(&f.0)).unwrap().is_none());
    }
    #[test]
    fn manual_corrections_override_ordinary_base_labels_including_grey() {
        let f = Fixture::new();
        let mut current = profile();
        let mut store = Persistence::game(
            &f.exe(),
            Some(&f.0),
            &mut current,
            [Some(60), Some(8), Some(16)],
        )
        .unwrap();
        current.nodes.get_mut(&(256, false)).unwrap().manual = true;
        store.changed();
        store.finish(&current);
        let mut base = profile();
        base.nodes.get_mut(&(256, false)).unwrap().tier = Tier::Slow;
        let next = Persistence::game(
            &f.exe(),
            Some(&f.0),
            &mut base,
            [Some(60), Some(8), Some(16)],
        )
        .unwrap();
        assert!(next.error.is_none());
        assert!(base.nodes[&(256, false)].tier == Tier::Grey);
        assert!(base.nodes[&(256, false)].manual);
    }
    #[test]
    fn invalid_data_falls_back_without_partial_installation_or_writes() {
        let f = Fixture::new();
        let mut base = profile();
        let (path, identity) = game_location(&f.exe(), Some(&f.0)).unwrap();
        for value in [
            serde_json::json!({"learning_version":1,"identity":"wrong"}),
            serde_json::json!({"learning_version":1,"identity":identity,"nodes":[{"pc":2}]}),
        ] {
            let bytes = serde_json::to_vec(&value).unwrap();
            fs::write(&path, &bytes).unwrap();
            let mut store =
                Persistence::game(&f.exe(), Some(&f.0), &mut base, [None, Some(8), Some(133)])
                    .unwrap();
            assert!(store.error.is_some());
            assert_eq!(base.nodes.len(), 1);
            assert!(base.nodes[&(256, false)].tier == Tier::Grey);
            store.poll(&base);
            store.finish(&base);
            assert_eq!(fs::read(&path).unwrap(), bytes);
        }
    }
    #[test]
    fn save_failure_is_reported_once_without_an_unbounded_retry_loop() {
        let f = Fixture::new();
        let mut current = profile();
        let blocked = f.0.join("file-not-directory");
        fs::write(&blocked, b"x").unwrap();
        let mut store = Persistence::game(
            &f.exe(),
            Some(&blocked),
            &mut current,
            [None, Some(8), Some(133)],
        )
        .unwrap();
        store.error = None;
        store.changed();
        store.poll(&current);
        store.finish(&current);
        assert!(store.error.take().is_some());
        assert!(!store.pending());
        store.poll(&current);
        assert!(store.error.is_none());
        assert_eq!(store.writes, 0);
    }
}
