//! Per-user native directories. Linux and BSD follow the XDG Base Directory
//! specification; macOS and Windows use their platform conventions.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostDirs {
    /// User-edited settings such as machine profiles.
    pub config: PathBuf,
    /// Game saves and save exports.
    pub data: PathBuf,
    /// Execution profiles and generated or compiled code; safe to delete.
    pub cache: PathBuf,
}
impl HostDirs {
    pub fn detect() -> Result<Self, String> {
        // Tests never read or write the user's real directories.
        if cfg!(test) {
            let home =
                std::env::temp_dir().join(format!("doslet-test-home-{}", std::process::id()));
            return Self::resolve("linux", |key| (key == "HOME").then(|| home.clone().into()));
        }
        Self::resolve(std::env::consts::OS, |key| std::env::var_os(key))
    }
    fn resolve(os: &str, env: impl Fn(&str) -> Option<OsString>) -> Result<Self, String> {
        // The specification requires absolute values; relative ones are ignored.
        let absolute = |key: &str| {
            env(key)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
        };
        let home = || absolute("HOME").ok_or("HOME is unset or relative");
        let xdg = |key: &str, fallback: &str| {
            absolute(key).map_or_else(|| home().map(|h| h.join(fallback)), Ok)
        };
        let (config, data, cache) = match os {
            "windows" => {
                let local = absolute("LOCALAPPDATA").ok_or("LOCALAPPDATA unavailable")?;
                (
                    absolute("APPDATA").ok_or("APPDATA unavailable")?,
                    local.clone(),
                    local.join("doslet"),
                )
            }
            "macos" => {
                let library = home()?.join("Library");
                let support = library.join("Application Support");
                (support.clone(), support, library.join("Caches"))
            }
            _ => (
                xdg("XDG_CONFIG_HOME", ".config")?,
                xdg("XDG_DATA_HOME", ".local/share")?,
                xdg("XDG_CACHE_HOME", ".cache")?,
            ),
        };
        Ok(Self {
            config: config.join("doslet"),
            data: data.join("doslet"),
            // Windows keeps its cache beside, not inside, the local data.
            cache: cache.join(if os == "windows" { "cache" } else { "doslet" }),
        })
    }
    pub fn profiles(&self) -> PathBuf {
        self.config.join("profiles")
    }
    #[allow(dead_code)]
    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.toml")
    }
    pub fn saves(&self) -> PathBuf {
        self.data.join("saves")
    }
    #[allow(dead_code)]
    pub fn autosave_file(&self, store_name: &str) -> PathBuf {
        self.saves().join(store_name).join("autosave.state.gz")
    }
    pub fn save_exports(&self) -> PathBuf {
        self.data.join("exports")
    }
    /// Learned and exported speed-tier labels.
    pub fn speed_tiers(&self) -> PathBuf {
        self.cache.join("speed-tiers")
    }
    /// The data location used before speed tiers became cache.
    pub fn legacy_speed_tiers(&self) -> PathBuf {
        self.data.join("speed-tiers")
    }
    /// Dynamic-core profiles, one per executable SHA-256.
    pub fn core_profiles(&self) -> PathBuf {
        self.cache.join("core-profiles")
    }
    /// Generated procedure modules, one directory per executable SHA-256.
    pub fn aot_modules(&self) -> PathBuf {
        self.cache.join("aot")
    }
    /// One procedure module per executable and live layout of its LE
    /// objects (`le_layout::Layout::key`). A DOS extender loads the same code
    /// at other addresses with a DPMI host or another memory configuration.
    #[allow(dead_code, reason = "the desktop resolves layouts while a game runs")]
    pub fn aot_module(&self, executable_sha256: &str, layout: &str) -> PathBuf {
        self.aot_modules()
            .join(executable_sha256)
            .join(layout)
            .join("procedures.wasm")
    }
    /// Native code compiled from procedure modules.
    pub fn aot_compiled(&self) -> PathBuf {
        self.cache.join("aot-compiled")
    }
}

/// A store named without a path component lives in the per-user save directory.
/// Explicit paths, including `./name`, keep their meaning relative to the caller.
pub fn store_directory(
    location: &str,
    dirs: impl FnOnce() -> Result<HostDirs, String>,
) -> Result<PathBuf, String> {
    let path = Path::new(location);
    let mut components = path.components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(name)), None) => Ok(dirs()?.saves().join(name)),
        (Some(_), _) => Ok(path.to_owned()),
        (None, _) => Err("save store needs a name or directory".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn env<'a>(pairs: &'a [(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        |key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| (*value).into())
        }
    }
    #[test]
    fn xdg_overrides_defaults_and_rejects_relative_values() {
        let dirs = HostDirs::resolve(
            "linux",
            env(&[
                ("HOME", "/home/user"),
                ("XDG_CONFIG_HOME", "/settings"),
                ("XDG_DATA_HOME", "relative"),
                ("XDG_CACHE_HOME", "/scratch"),
            ]),
        )
        .unwrap();
        assert_eq!(dirs.config, Path::new("/settings/doslet"));
        assert_eq!(dirs.data, Path::new("/home/user/.local/share/doslet"));
        assert_eq!(dirs.speed_tiers(), Path::new("/scratch/doslet/speed-tiers"));
        assert_eq!(
            HostDirs::resolve("freebsd", env(&[("HOME", "/u")])).unwrap(),
            HostDirs {
                config: "/u/.config/doslet".into(),
                data: "/u/.local/share/doslet".into(),
                cache: "/u/.cache/doslet".into(),
            }
        );
        assert!(HostDirs::resolve("linux", env(&[("HOME", "home")])).is_err());
        assert!(
            HostDirs::resolve(
                "linux",
                env(&[
                    ("XDG_CONFIG_HOME", "/c"),
                    ("XDG_DATA_HOME", "/d"),
                    ("XDG_CACHE_HOME", "/k")
                ])
            )
            .is_ok()
        );
    }
    #[test]
    fn platform_conventions_outside_xdg() {
        let mac = HostDirs::resolve("macos", env(&[("HOME", "/Users/u")])).unwrap();
        assert_eq!(
            mac.data,
            Path::new("/Users/u/Library/Application Support/doslet")
        );
        assert_eq!(mac.cache, Path::new("/Users/u/Library/Caches/doslet"));
        let windows = HostDirs::resolve(
            "windows",
            env(&[("APPDATA", "/Roaming"), ("LOCALAPPDATA", "/Local")]),
        )
        .unwrap();
        assert_eq!(windows.config, Path::new("/Roaming/doslet"));
        assert_eq!(windows.saves(), Path::new("/Local/doslet/saves"));
        assert_eq!(
            windows.speed_tiers(),
            Path::new("/Local/doslet/cache/speed-tiers")
        );
    }
    #[test]
    fn tests_use_a_private_home() {
        let dirs = HostDirs::detect().unwrap();
        assert!(dirs.cache.starts_with(std::env::temp_dir()));
        assert!(dirs.data.starts_with(std::env::temp_dir()));
    }
    #[test]
    fn bare_store_names_use_the_save_directory() {
        let dirs = || {
            Ok(HostDirs {
                config: "/c".into(),
                data: "/d".into(),
                cache: "/k".into(),
            })
        };
        assert_eq!(
            store_directory("prince", dirs).unwrap(),
            Path::new("/d/saves/prince")
        );
        assert_eq!(
            store_directory("./prince", dirs).unwrap(),
            Path::new("./prince")
        );
        assert_eq!(
            store_directory("/x/saves", dirs).unwrap(),
            Path::new("/x/saves")
        );
        assert_eq!(store_directory("a/b", dirs).unwrap(), Path::new("a/b"));
        assert_eq!(store_directory("..", dirs).unwrap(), Path::new(".."));
        assert!(store_directory("", dirs).is_err());
    }
}
