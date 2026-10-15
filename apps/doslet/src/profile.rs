//! Native launch policy. TOML and host paths never enter emulation libraries.
use crate::cli::{Cli, DriveMode, MidiMode};
use std::{
    error::Error,
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item, TableLike};

#[derive(Default)]
struct Profile {
    name: Option<String>,
    executable: Option<PathBuf>,
    arguments: Vec<String>,
    working_directory: Option<String>,
    root: Option<PathBuf>,
    cdrom: Option<(u8, PathBuf)>,
    writable: bool,
    drive_mode: Option<DriveMode>,
    game_mode: Option<MidiMode>,
    host_mode: Option<MidiMode>,
    output: Option<String>,
    rom_directory: Option<PathBuf>,
    audio: Option<bool>,
}

fn fields(table: &dyn TableLike, allowed: &[&str], context: &str) -> Result<(), String> {
    for (key, _) in table.iter() {
        if !allowed.contains(&key) {
            return Err(format!("unknown field {context}{key}"));
        }
    }
    Ok(())
}
fn section<'a>(table: &'a dyn TableLike, key: &str) -> Result<Option<&'a dyn TableLike>, String> {
    table
        .get(key)
        .map(|item| {
            item.as_table_like()
                .ok_or_else(|| format!("{key} must be a table"))
        })
        .transpose()
}
fn string(table: &dyn TableLike, key: &str, context: &str) -> Result<Option<String>, String> {
    table
        .get(key)
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{context}{key} must be a string"))
        })
        .transpose()
}
fn boolean(table: &dyn TableLike, key: &str, context: &str) -> Result<Option<bool>, String> {
    table
        .get(key)
        .map(|item| {
            item.as_bool()
                .ok_or_else(|| format!("{context}{key} must be a boolean"))
        })
        .transpose()
}
fn mode(table: &dyn TableLike, key: &str) -> Result<Option<MidiMode>, String> {
    string(table, key, "midi.")?
        .map(|value| match value.as_str() {
            "gm" => Ok(MidiMode::Gm),
            "mt32" => Ok(MidiMode::Mt32),
            _ => Err(format!("midi.{key} must be gm or mt32")),
        })
        .transpose()
}
fn path(base: &Path, value: String, field: &str) -> Result<PathBuf, String> {
    if value.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    Ok(base.join(value)) // Path::join preserves absolute paths.
}

impl Profile {
    fn parse(text: &str, base: &Path) -> Result<Self, String> {
        let document = text
            .parse::<DocumentMut>()
            .map_err(|error| error.to_string())?;
        let table = document.as_table();
        fields(
            table,
            &["version", "name", "game", "drives", "midi", "audio"],
            "",
        )?;
        if table.get("version").and_then(Item::as_integer) != Some(1) {
            return Err("version must be the integer 1".into());
        }
        let mut profile = Self {
            name: string(table, "name", "")?,
            writable: true,
            ..Self::default()
        };
        if let Some(game) = section(table, "game")? {
            fields(
                game,
                &["executable", "arguments", "working_directory"],
                "game.",
            )?;
            profile.executable = string(game, "executable", "game.")?
                .map(|value| path(base, value, "game.executable"))
                .transpose()?;
            profile.working_directory = string(game, "working_directory", "game.")?;
            if let Some(arguments) = game.get("arguments") {
                let array = arguments
                    .as_array()
                    .ok_or("game.arguments must be an array of strings")?;
                for argument in array.iter() {
                    profile.arguments.push(
                        argument
                            .as_str()
                            .ok_or("game.arguments must contain only strings")?
                            .to_owned(),
                    );
                }
            }
        }
        if let Some(midi) = section(table, "midi")? {
            fields(
                midi,
                &["game_mode", "host_mode", "output", "rom_directory"],
                "midi.",
            )?;
            profile.game_mode = mode(midi, "game_mode")?;
            profile.host_mode = mode(midi, "host_mode")?;
            profile.output = string(midi, "output", "midi.")?;
            if profile.output.as_deref().is_some_and(str::is_empty) {
                return Err("midi.output must not be empty".into());
            }
            profile.rom_directory = string(midi, "rom_directory", "midi.")?
                .map(|value| path(base, value, "midi.rom_directory"))
                .transpose()?;
        }
        if let Some(audio) = section(table, "audio")? {
            fields(audio, &["enabled"], "audio.")?;
            profile.audio = boolean(audio, "enabled", "audio.")?;
        }
        if let Some(drives) = table.get("drives") {
            let drives = drives
                .as_array_of_tables()
                .ok_or("drives must use [[drives]] tables")?;
            if drives.is_empty() || drives.len() > 2 {
                return Err("drives supports one C: directory and at most one CD-ROM image".into());
            }
            for drive in drives.iter() {
                fields(
                    drive,
                    &["letter", "kind", "source", "writable", "mode"],
                    "drives.",
                )?;
                let letter =
                    string(drive, "letter", "drives.")?.ok_or("drives.letter is required")?;
                let kind = string(drive, "kind", "drives.")?.ok_or("drives.kind is required")?;
                let source = path(
                    base,
                    string(drive, "source", "drives.")?.ok_or("drives.source is required")?,
                    "drives.source",
                )?;
                match kind.as_str() {
                    "directory" if letter.eq_ignore_ascii_case("C") && profile.root.is_none() => {
                        profile.root = Some(source);
                        profile.writable = boolean(drive, "writable", "drives.")?.unwrap_or(true);
                        if let Some(mode) = string(drive, "mode", "drives.")? {
                            if drive.contains_key("writable") {
                                return Err(
                                    "drives.mode and drives.writable are mutually exclusive".into(),
                                );
                            }
                            profile.drive_mode = Some(match mode.as_str() {
                                "ro" => DriveMode::Ro,
                                "overlay" => DriveMode::Overlay,
                                "rw" => DriveMode::Rw,
                                _ => return Err("drives.mode must be ro, overlay or rw".into()),
                            });
                        }
                    }
                    "cdrom"
                        if letter.len() == 1
                            && (b'D'..=b'Z')
                                .contains(&letter.as_bytes()[0].to_ascii_uppercase())
                            && profile.cdrom.is_none() =>
                    {
                        if boolean(drive, "writable", "drives.")? == Some(true) {
                            return Err(
                                "CD-ROM drives are read-only; drives.writable cannot be true"
                                    .into(),
                            );
                        }
                        if string(drive, "mode", "drives.")?.is_some_and(|mode| mode != "ro") {
                            return Err("CD-ROM mode must be ro".into());
                        }
                        profile.cdrom = Some((letter.as_bytes()[0].to_ascii_uppercase(), source));
                    }
                    "directory" | "cdrom" => {
                        return Err("drives must contain one C: directory and at most one CD-ROM with a distinct letter from D: to Z:".into());
                    }
                    _ => return Err("drives.kind must be directory or cdrom".into()),
                }
            }
            if profile.root.is_none() {
                return Err("drives must include a C: directory".into());
            }
        }
        Ok(profile)
    }
}

fn command_tail(arguments: &[String]) -> Result<Vec<u8>, String> {
    let mut tail = Vec::new();
    for argument in arguments {
        if !argument.is_ascii()
            || argument
                .bytes()
                .any(|byte| byte < 0x20 || byte == 0x7F || byte == b'"')
        {
            return Err(
                "game.arguments must be printable ASCII without embedded double quotes".into(),
            );
        }
        tail.push(b' ');
        let quoted = argument.is_empty() || argument.contains(' ');
        if quoted {
            tail.push(b'"');
        }
        tail.extend_from_slice(argument.as_bytes());
        if quoted {
            tail.push(b'"');
        }
        if tail.len() > 126 {
            return Err("game.arguments exceeds the 126-byte DOS command tail limit".into());
        }
    }
    Ok(tail)
}

impl Cli {
    pub fn resolve(mut self) -> Result<Self, Box<dyn Error>> {
        if let Some(config) = &self.config {
            let text = std::fs::read_to_string(config)
                .map_err(|error| format!("Cannot read {}: {error}", config.display()))?;
            let base = config.parent().unwrap_or(Path::new("."));
            let profile = Profile::parse(&text, base)
                .map_err(|error| format!("{}: {error}", config.display()))?;
            self.game = self.game.or(profile.executable);
            self.midi_mode = self.midi_mode.or(profile.game_mode);
            self.midi_output_mode = self.midi_output_mode.or(profile.host_mode);
            let explicit_audio = self.midi_output.is_some() || self.mt32_rom_dir.is_some();
            self.mt32_rom_dir = self.mt32_rom_dir.or(profile.rom_directory);
            // An explicit output selection also enables audio disabled in a profile.
            self.no_audio |= profile.audio == Some(false) && !explicit_audio;
            self.midi_output = self.midi_output.or(profile.output);
            self.profile_name = profile.name;
            self.launch.command_tail = command_tail(&profile.arguments)?;
            if let Some(directory) = profile.working_directory {
                if !directory.is_ascii()
                    || directory.bytes().any(|byte| byte < 0x20 || byte == 0x7F)
                    || !(directory.starts_with('\\')
                        || directory.to_ascii_uppercase().starts_with("C:\\"))
                {
                    return Err(
                        "game.working_directory must be an absolute ASCII DOS path on C:".into(),
                    );
                }
                self.launch.working_directory = directory.into_bytes();
            }
            self.launch.root = profile.root;
            self.launch.writable = profile.writable;
            self.drive_mode = self.drive_mode.or(profile.drive_mode);
            self.launch.cdrom = profile.cdrom;
            if self.game.is_none() {
                return Err("profile requires game.executable or --game".into());
            }
        }
        self.midi_mode.get_or_insert(MidiMode::Gm);
        self.launch.heroes_timer_waits = self.experimental_heroes_timer_waits;
        if let Some(mode) = self.drive_mode {
            self.launch.writable = mode != DriveMode::Ro;
            self.launch.persist = mode == DriveMode::Rw;
        }
        self.midi_output_mode.get_or_insert(MidiMode::Gm);
        if self.no_audio {
            self.midi_output = None;
            self.mt32_rom_dir = None;
        }
        if let Some(game) = &self.game {
            if !game.is_file() {
                return Err(format!("game.executable is not a file: {}", game.display()).into());
            }
            if let Some(root) = &self.launch.root {
                if !root.is_dir() {
                    return Err(
                        format!("drives.source is not a directory: {}", root.display()).into(),
                    );
                }
                if !game.canonicalize()?.starts_with(root.canonicalize()?) {
                    return Err("game.executable must belong to the mounted C: directory".into());
                }
            }
        }
        if let Some((_, image)) = &self.launch.cdrom
            && !image.is_file()
        {
            return Err(format!(
                "drives.source is not a CD-ROM image file: {}",
                image.display()
            )
            .into());
        }
        if let Some(directory) = &self.mt32_rom_dir {
            for name in ["CM32L_CONTROL.ROM", "CM32L_PCM.ROM"] {
                if !directory.join(name).is_file() {
                    return Err(format!(
                        "midi.rom_directory is missing {name}: {}",
                        directory.display()
                    )
                    .into());
                }
            }
        }
        if self.midi_output.as_deref() == Some("moont") && self.mt32_rom_dir.is_none() {
            return Err("midi.output=moont requires midi.rom_directory or --mt32-rom-dir".into());
        }
        Ok(self)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use clap::Parser;
    use std::sync::atomic::{AtomicU64, Ordering};
    include!("../../../tests/fixtures/relocated_mz.rs");
    include!("../../../tests/fixtures/iso9660.rs");
    #[test]
    fn drive_modes_select_persistence_and_cli_overrides_profile() {
        let fixture = Fixture::new(
            "version=1\n[game]\nexecutable='../TINY.EXE'\n[[drives]]\nletter='C'\nkind='directory'\nsource='..'\nmode='rw'\n",
        );
        let cli = Cli::parse_from([
            std::ffi::OsString::from("doslet"),
            "--config".into(),
            fixture.config.clone().into_os_string(),
        ])
        .resolve()
        .unwrap();
        assert!(cli.launch.persist && cli.launch.writable);
        let cli = Cli::parse_from([
            std::ffi::OsString::from("doslet"),
            "--config".into(),
            fixture.config.clone().into_os_string(),
            "--drive-mode".into(),
            "overlay".into(),
        ])
        .resolve()
        .unwrap();
        assert!(cli.launch.writable && !cli.launch.persist);
        let cli = Cli::parse_from(["doslet", "--drive-mode", "ro"])
            .resolve()
            .unwrap();
        assert!(!cli.launch.writable && !cli.launch.persist);
        assert!(Cli::try_parse_from(["doslet", "--drive-mode", "bad"]).is_err());
        assert!(Profile::parse("version=1\n[[drives]]\nletter='C'\nkind='directory'\nsource='.'\nmode='rw'\nwritable=true", Path::new(".")).is_err());
    }

    pub(crate) struct Fixture {
        pub root: PathBuf,
        pub config: PathBuf,
    }
    impl Fixture {
        pub fn new(profile: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir().join(format!(
                "doslet-profile-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(root.join("profiles")).unwrap();
            std::fs::write(root.join("TINY.EXE"), relocated_mz()).unwrap();
            let config = root.join("profiles/game.toml");
            std::fs::write(&config, profile).unwrap();
            Self { root, config }
        }
        pub fn cli(&self, extra: &[&str]) -> Cli {
            let mut arguments = vec![
                std::ffi::OsString::from("doslet"),
                "--config".into(),
                self.config.as_os_str().into(),
            ];
            arguments.extend(extra.iter().map(std::ffi::OsString::from));
            Cli::parse_from(arguments)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn profile_paths_modes_and_arguments_resolve_without_changing_host_cwd() {
        let fixture = Fixture::new(
            r#"
version = 1
name = "Tiny"
[game]
executable = "../TINY.EXE"
arguments = ["/test", "two words", ""]
working_directory = 'C:\'
[midi]
game_mode = "mt32"
host_mode = "mt32"
output = "rust"
[audio]
enabled = false
"#,
        );
        let cwd = std::env::current_dir().unwrap();
        let cli = fixture.cli(&[]).resolve().unwrap();
        assert_eq!(
            cli.game.unwrap().canonicalize().unwrap(),
            fixture.root.join("TINY.EXE")
        );
        assert_eq!(cli.profile_name.as_deref(), Some("Tiny"));
        assert_eq!(cli.midi_mode, Some(MidiMode::Mt32));
        assert_eq!(cli.midi_output_mode, Some(MidiMode::Mt32));
        assert_eq!(cli.launch.command_tail, b" /test \"two words\" \"\"");
        assert!(cli.no_audio);
        assert_eq!(std::env::current_dir().unwrap(), cwd);
    }

    #[test]
    fn explicit_cli_modes_output_game_and_silence_override_profile() {
        let fixture = Fixture::new(
            "version=1\n[game]\nexecutable='../missing.exe'\n[midi]\ngame_mode='mt32'\nhost_mode='mt32'\noutput='moont'\nrom_directory='../missing-roms'\n",
        );
        let game = fixture.root.join("TINY.EXE");
        let cli = fixture
            .cli(&[
                "--game",
                game.to_str().unwrap(),
                "--midi-mode",
                "gm",
                "--midi-output-mode",
                "gm",
                "--no-audio",
            ])
            .resolve()
            .unwrap();
        assert_eq!(cli.game.as_deref(), Some(game.as_path()));
        assert_eq!(cli.midi_mode, Some(MidiMode::Gm));
        assert_eq!(cli.midi_output_mode, Some(MidiMode::Gm));
        assert!(cli.midi_output.is_none());
        assert!(cli.mt32_rom_dir.is_none());
        std::fs::write(&fixture.config, "version=1\n[game]\nexecutable='../TINY.EXE'\n[midi]\noutput='1'\n[audio]\nenabled=false\n").unwrap();
        let cli = fixture.cli(&["--midi-output", "rust"]).resolve().unwrap();
        assert!(!cli.no_audio);
        assert_eq!(cli.midi_output.as_deref(), Some("rust"));
        assert_eq!(cli.midi_mode, Some(MidiMode::Gm));
    }

    #[test]
    fn cdrom_profile_resolves_the_iso_path_and_mounts_it_readonly() {
        let fixture = Fixture::new(
            "version=1\n[game]\nexecutable='../TINY.EXE'\n[[drives]]\nletter='C'\nkind='directory'\nsource='..'\n[[drives]]\nletter='D'\nkind='cdrom'\nsource='../disc.gog'\nwritable=false\n",
        );
        std::fs::write(fixture.root.join("disc.gog"), test_iso()).unwrap();
        let cli = fixture.cli(&["--no-audio"]).resolve().unwrap();
        let (letter, image) = cli.launch.cdrom.as_ref().unwrap();
        assert_eq!(*letter, b'D');
        assert_eq!(image.canonicalize().unwrap(), fixture.root.join("disc.gog"));
        let session =
            crate::native_input::load_game_with_settings(cli.game.as_ref().unwrap(), &cli.launch)
                .unwrap();
        assert_eq!(session.status(), doslet_runtime::SessionStatus::Ready);
    }

    #[test]
    fn malformed_fields_versions_modes_and_mounts_are_rejected() {
        for (text, expected) in [
            ("version=2", "version"),
            ("version=1\nunknown=true", "unknown field"),
            ("version=1\n[game]\nargumnts=[]", "game.argumnts"),
            ("version=1\n[game]\narguments=[2]", "only strings"),
            ("version=1\n[midi]\ngame_mode='auto'", "gm or mt32"),
            ("version=1\n[audio]\nenabled='yes'", "boolean"),
            (
                "version=1\n[[drives]]\nletter='D'\nkind='cdrom'\nsource='image.iso'",
                "include a C:",
            ),
            (
                "version=1\n[[drives]]\nletter='D'\nkind='directory'\nsource='data'",
                "one C:",
            ),
            ("version=1\n[midi]\noutput=''", "empty"),
        ] {
            let error = Profile::parse(text, Path::new(".")).err().expect(text);
            assert!(error.contains(expected), "{text}: {error}");
        }
        assert!(command_tail(&["x".repeat(126)]).is_err());
        assert_eq!(command_tail(&["x".repeat(125)]).unwrap().len(), 126);
        assert!(command_tail(&["a\nb".into()]).is_err());
        assert!(command_tail(&["zażółć".into()]).is_err());
        assert!(command_tail(&["embedded\"quote".into()]).is_err());
    }

    #[test]
    fn explicit_c_mount_bounds_executable_and_keeps_readonly_setting() {
        let fixture = Fixture::new(
            "version=1\n[game]\nexecutable='../TINY.EXE'\n[[drives]]\nletter='C'\nkind='directory'\nsource='..'\nwritable=false\n",
        );
        let cli = fixture.cli(&["--no-audio"]).resolve().unwrap();
        assert_eq!(
            cli.launch.root.unwrap().canonicalize().unwrap(),
            fixture.root
        );
        assert!(!cli.launch.writable);
        std::fs::write(&fixture.config, "version=1\n[game]\nexecutable='../TINY.EXE'\n[[drives]]\nletter='C'\nkind='directory'\nsource='.'\n").unwrap();
        assert!(
            fixture
                .cli(&[])
                .resolve()
                .unwrap_err()
                .to_string()
                .contains("must belong")
        );
    }

    #[test]
    fn relative_rom_path_and_cli_override_use_distinct_bases() {
        let fixture = Fixture::new(
            "version=1\n[game]\nexecutable='../TINY.EXE'\n[midi]\noutput='moont'\nrom_directory='../roms'\n",
        );
        let roms = fixture.root.join("roms");
        std::fs::create_dir(&roms).unwrap();
        for name in ["CM32L_CONTROL.ROM", "CM32L_PCM.ROM"] {
            std::fs::write(roms.join(name), []).unwrap();
        }
        let cli = fixture.cli(&[]).resolve().unwrap();
        assert_eq!(cli.mt32_rom_dir.unwrap().canonicalize().unwrap(), roms);
        let cli = fixture
            .cli(&["--mt32-rom-dir", roms.to_str().unwrap()])
            .resolve()
            .unwrap();
        assert_eq!(cli.mt32_rom_dir, Some(roms));
    }
}
