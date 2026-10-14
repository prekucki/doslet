//! Native setup adapter shared by the CLI and desktop frontend.
use doslet_dos::files::ReadOnlyDrive;
use doslet_runtime::Session;
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct LaunchSettings {
    pub command_tail: Vec<u8>,
    pub working_directory: Vec<u8>,
    pub root: Option<PathBuf>,
    pub writable: bool,
}
impl Default for LaunchSettings {
    fn default() -> Self {
        Self {
            command_tail: Vec::new(),
            working_directory: b"C:\\".to_vec(),
            root: None,
            writable: true,
        }
    }
}

#[allow(
    dead_code,
    reason = "legacy entry point used by the headless frontend sharing this module"
)]
pub fn load_game(path: &Path) -> Result<Session, Box<dyn Error>> {
    load_game_with_settings(path, &LaunchSettings::default())
}
pub fn load_game_with_settings(
    path: &Path,
    settings: &LaunchSettings,
) -> Result<Session, Box<dyn Error>> {
    if !fs::metadata(path)?.is_file() {
        return Err("executable path is not a file".into());
    }
    let default_root = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let root = fs::canonicalize(settings.root.as_deref().unwrap_or(default_root))?;
    let executable = fs::canonicalize(path)?;
    let filename = executable
        .strip_prefix(&root)
        .map_err(|_| "game.executable must belong to the mounted C: directory")?
        .to_str()
        .ok_or("non-ASCII DOS path")?
        .replace('/', "\\")
        .to_ascii_uppercase();
    if !filename.is_ascii() {
        return Err("non-ASCII DOS path".into());
    }
    let dos_name = format!("C:\\{filename}");
    let mut drive = ReadOnlyDrive::new();
    mount_directory(&root, &root, &mut drive)?;
    let drive = if settings.writable {
        drive.with_write_overlay(1024 * 1024)
    } else {
        drive
    };
    let mut session = Session::from_mz_stub_named(
        drive
            .file(filename.as_bytes())
            .map_err(|code| format!("DOS mount error {code}"))?,
        dos_name.as_bytes(),
    )?
    .with_drive(drive);
    session.configure_launch(&settings.command_tail, &settings.working_directory)?;
    Ok(session)
}
fn mount_directory(
    root: &Path,
    directory: &Path,
    drive: &mut ReadOnlyDrive,
) -> Result<(), Box<dyn Error>> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        let path = entry.path();
        if kind.is_dir() {
            mount_directory(root, &path, drive)?;
        } else if kind.is_file() {
            let name = path
                .strip_prefix(root)?
                .to_str()
                .ok_or("non-ASCII DOS path")?;
            drive
                .mount(name.as_bytes(), fs::read(&path)?)
                .map_err(|code| format!("cannot mount {name}: DOS error {code}"))?;
        }
    }
    Ok(())
}
