//! Setup-only discovery of existing DOSBox ROMs. No files are copied or changed.
use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
};

const CONTROL: &str = "CM32L_CONTROL.ROM";
const PCM: &str = "CM32L_PCM.ROM";

pub fn paths(directory: &Path) -> Result<(PathBuf, PathBuf), String> {
    fn find(directory: &Path, name: &str) -> Option<PathBuf> {
        let exact = directory.join(name);
        if exact.is_file() {
            return Some(exact);
        }
        fs::read_dir(directory)
            .ok()?
            .take(512)
            .filter_map(Result::ok)
            .find(|entry| entry.file_name().eq_ignore_ascii_case(name) && entry.path().is_file())
            .map(|entry| entry.path())
    }
    let find = |name| {
        find(directory, name).ok_or_else(|| {
            format!(
                "midi.rom_directory is missing {name}: {}",
                directory.display()
            )
        })
    };
    Ok((find(CONTROL)?, find(PCM)?))
}

pub fn read(directory: &Path) -> Result<(Vec<u8>, Vec<u8>), String> {
    fn bounded(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        fs::File::open(path)?
            .take(limit + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ROM exceeds supported size",
            ));
        }
        Ok(bytes)
    }
    let (control, pcm) = paths(directory)?;
    Ok((
        bounded(&control, 128 * 1024).map_err(|e| e.to_string())?,
        bounded(&pcm, 1024 * 1024).map_err(|e| e.to_string())?,
    ))
}

fn candidates(os: &str, env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Vec<PathBuf> {
    let home = env("HOME").filter(|v| !v.is_empty()).map(PathBuf::from);
    let mut dirs = Vec::new();
    let mut add = |path: PathBuf| {
        if !dirs.contains(&path) {
            dirs.push(path);
        }
    };
    match os {
        "windows" => {
            for key in ["LOCALAPPDATA", "APPDATA"] {
                if let Some(base) = env(key).filter(|v| !v.is_empty()) {
                    add(PathBuf::from(base).join("DOSBox/mt32-roms"));
                }
            }
        }
        "macos" => {
            if let Some(home) = &home {
                add(home.join("Library/Preferences/DOSBox/mt32-roms"));
                add(home.join("Library/Audio/Sounds/MT32-Roms"));
            }
            add(PathBuf::from("/usr/local/share/mt32-rom-data"));
            add(PathBuf::from("/usr/share/mt32-rom-data"));
        }
        _ => {
            let xdg = |key: &str, fallback: &str| {
                env(key)
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .or_else(|| home.as_ref().map(|h| h.join(fallback)))
            };
            if let Some(base) = xdg("XDG_DATA_HOME", ".local/share") {
                add(base.join("dosbox/mt32-roms"));
                add(base.join("mt32-rom-data"));
            }
            if let Some(base) = xdg("XDG_CONFIG_HOME", ".config") {
                add(base.join("dosbox/mt32-roms"));
            }
            let bases = env("XDG_DATA_DIRS")
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
            for base in std::env::split_paths(&bases)
                .filter(|p| p.is_absolute())
                .take(32)
            {
                add(base.join("mt32-rom-data"));
            }
        }
    }
    if let Some(home) = home {
        add(home.join(".dosbox/mt32-roms"));
        add(home.join(".dosbox"));
    }
    dirs
}

fn detect_in(dirs: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    dirs.into_iter().find(|directory| {
        read(directory)
            .is_ok_and(|(control, pcm)| doslet_mt32::validate_roms(&control, &pcm).is_ok())
    })
}

pub fn detect() -> Option<PathBuf> {
    detect_in(candidates(std::env::consts::OS, |key| {
        std::env::var_os(key)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{cli::Cli, profile::tests::Fixture};
    use clap::Parser;

    #[test]
    fn standard_locations_use_xdg_overrides_without_changing_process_environment() {
        let dirs = candidates("linux", |key| match key {
            "HOME" => Some("/user".into()),
            "XDG_CONFIG_HOME" => Some("/settings".into()),
            "XDG_DATA_HOME" => Some("/data".into()),
            "XDG_DATA_DIRS" => Some("/system:/extra".into()),
            _ => None,
        });
        assert_eq!(
            dirs,
            [
                "/data/dosbox/mt32-roms",
                "/data/mt32-rom-data",
                "/settings/dosbox/mt32-roms",
                "/system/mt32-rom-data",
                "/extra/mt32-rom-data",
                "/user/.dosbox/mt32-roms",
                "/user/.dosbox"
            ]
            .map(PathBuf::from)
        );
        let dirs = candidates("linux", |key| match key {
            "HOME" => Some("/user".into()),
            "XDG_CONFIG_HOME" => Some("relative-is-ignored".into()),
            _ => None,
        });
        assert!(dirs.contains(&PathBuf::from("/user/.config/dosbox/mt32-roms")));
        let mac = candidates("macos", |key| (key == "HOME").then(|| "/user".into()));
        assert_eq!(
            mac[0],
            Path::new("/user/Library/Preferences/DOSBox/mt32-roms")
        );
        let windows = candidates("windows", |key| {
            (key == "LOCALAPPDATA").then(|| "C:/Users/user/AppData/Local".into())
        });
        assert_eq!(
            windows[0],
            Path::new("C:/Users/user/AppData/Local/DOSBox/mt32-roms")
        );
    }

    #[test]
    fn lookup_accepts_lowercase_and_rejects_missing_invalid_or_oversized_roms() {
        let fixture = Fixture::new("version=2");
        let dir = fixture.root.join("roms");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("cm32l_control.rom"), b"invalid control").unwrap();
        assert!(paths(&dir).is_err());
        fs::write(dir.join("cm32l_pcm.rom"), b"invalid pcm").unwrap();
        let (control, pcm) = read(&dir).unwrap();
        assert_eq!(control, b"invalid control");
        assert_eq!(pcm, b"invalid pcm");
        assert!(detect_in([fixture.root.join("missing"), dir.clone()]).is_none());
        fs::File::create(dir.join("cm32l_pcm.rom"))
            .unwrap()
            .set_len(1024 * 1024 + 1)
            .unwrap();
        assert!(read(&dir).unwrap_err().contains("exceeds supported size"));
    }

    #[test]
    fn discovery_is_optional_and_explicit_paths_and_outputs_take_precedence() {
        let fixture = Fixture::new("version=2\n[output]\nmidi_output='rust'");
        let dir = fixture.root.join("roms");
        fs::create_dir(&dir).unwrap();
        for name in [CONTROL, PCM] {
            fs::write(dir.join(name), []).unwrap();
        }
        let cli = Cli::parse_from(["doslet"])
            .resolve_with_roms(|| Some(dir.clone()))
            .unwrap();
        assert_eq!(cli.mt32_rom_dir, Some(dir.clone()));
        assert_eq!(cli.midi_output.as_deref(), Some("moont"));
        let defaults = Fixture::new("version=2");
        let cli = defaults
            .cli(&[])
            .resolve_with_roms(|| Some(dir.clone()))
            .unwrap();
        assert_eq!(cli.midi_output.as_deref(), Some("moont"));
        let cli = fixture
            .cli(&[])
            .resolve_with_roms(|| Some(dir.clone()))
            .unwrap();
        assert_eq!(cli.midi_output.as_deref(), Some("rust"));
        let cli = Cli::parse_from(["doslet", "--midi-output", "rust"])
            .resolve_with_roms(|| Some(dir.clone()))
            .unwrap();
        assert_eq!(cli.midi_output.as_deref(), Some("rust"));
        let cli = Cli::parse_from(["doslet", "--mt32-rom-dir", dir.to_str().unwrap()])
            .resolve_with_roms(|| panic!("manual path must bypass discovery"))
            .unwrap();
        assert_eq!(cli.mt32_rom_dir, Some(dir));
        let cli = Cli::parse_from(["doslet", "--no-audio"])
            .resolve_with_roms(|| panic!("no-audio must bypass discovery"))
            .unwrap();
        assert!(cli.mt32_rom_dir.is_none());
        let cli = Cli::parse_from(["doslet"])
            .resolve_with_roms(|| None)
            .unwrap();
        assert!(cli.mt32_rom_dir.is_none());
        assert!(cli.midi_output.is_none());
    }

    #[test]
    #[ignore = "requires local DOSBox CM-32L v1.02 ROMs"]
    fn local_dosbox_roms_are_detected_and_synthesizer_initializes() {
        let directory = detect().expect("supported local DOSBox ROM pair");
        let (control, pcm) = read(&directory).unwrap();
        doslet_mt32::Timeline::new(&control, &pcm, 48000).unwrap();
        let cli = Cli::parse_from(["doslet"]).resolve().unwrap();
        assert_eq!(cli.mt32_rom_dir.as_ref(), Some(&directory));
        assert_eq!(cli.midi_output.as_deref(), Some("moont"));
        let spec = cli.launch.machine_spec.unwrap();
        assert_eq!(spec.output.midi_output, "moont");
        assert_eq!(spec.output.rom_binding.as_deref(), directory.to_str());
        println!("Detected and initialized CM-32L: {}", directory.display());
    }
}
