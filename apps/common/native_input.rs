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
    pub persist: bool,
    pub cdrom: Option<(u8, PathBuf)>,
    pub heroes_timer_waits: bool,
}
impl Default for LaunchSettings {
    fn default() -> Self {
        Self {
            command_tail: Vec::new(),
            working_directory: b"C:\\".to_vec(),
            root: None,
            writable: true,
            persist: false,
            cdrom: None,
            heroes_timer_waits: false,
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
    let image = settings
        .cdrom
        .as_ref()
        .map(|(_, path)| fs::canonicalize(path))
        .transpose()?;
    mount_directory(&root, &root, &mut drive, image.as_deref())?;
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
    if let Some((letter, image)) = &settings.cdrom {
        session = session.with_cdrom_image(*letter, fs::read(image)?)?;
    }
    session.configure_launch(&settings.command_tail, &settings.working_directory)?;
    if settings.heroes_timer_waits {
        session.set_memory_wait_sites(0x281098, [0x1E281C, 0x1E2881, 0x1E331A]);
        eprintln!(
            "[INFO doslet] Experimental Heroes timer waits enabled (three validated polling sites)"
        );
    }
    Ok(session)
}
fn mount_directory(
    root: &Path,
    directory: &Path,
    drive: &mut ReadOnlyDrive,
    excluded: Option<&Path>,
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
            mount_directory(root, &path, drive, excluded)?;
        } else if kind.is_file() {
            if excluded == Some(path.as_path()) {
                continue;
            }
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

/// Persist closed DOS files only when the native launch explicitly selected RW.
/// Streams borrowed overlay bytes into a sibling temporary file, then renames it.
#[allow(
    dead_code,
    reason = "shared native adapter; read-only headless diagnostics do not persist"
)]
pub fn flush_writes(
    session: &mut Session,
    executable: &Path,
    settings: &LaunchSettings,
) -> Result<usize, Box<dyn Error>> {
    use std::io::Write;
    if !settings.persist || !session.has_closed_writes() {
        return Ok(0);
    }
    let default_root = executable
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let root = fs::canonicalize(settings.root.as_deref().unwrap_or(default_root))?;
    let cdrom = settings
        .cdrom
        .as_ref()
        .map(|(_, path)| fs::canonicalize(path))
        .transpose()?;
    session.flush_writes(|name, view| -> Result<(), Box<dyn Error>> {
        let name = std::str::from_utf8(name)?;
        let parts: Vec<_> = name.split('\\').collect();
        let mut target = root.clone();
        for (index, part) in parts.iter().enumerate() {
            if part.is_empty()
                || *part == "."
                || *part == ".."
                || part.contains('/')
                || part.contains(':')
            {
                return Err("invalid DOS writeback path".into());
            }
            let matched = fs::read_dir(&target)?.filter_map(Result::ok).find(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|entry| entry.eq_ignore_ascii_case(part))
            });
            if let Some(entry) = matched {
                if entry.file_type()?.is_symlink() {
                    return Err("DOS writeback refuses symbolic links".into());
                }
                target = entry.path();
            } else if index + 1 == parts.len() {
                target.push(part);
            } else {
                return Err("DOS writeback parent does not exist".into());
            }
        }
        let parent = fs::canonicalize(target.parent().ok_or("missing writeback parent")?)?;
        if !parent.starts_with(&root) || cdrom.as_ref().is_some_and(|image| *image == target) {
            return Err("DOS writeback is outside C: or targets the CD image".into());
        }
        let permissions = match fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_file() => Some(metadata.permissions()),
            Ok(_) => return Err("DOS writeback target is not a regular file".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos();
        let temporary = parent.join(format!(".doslet-rw-{}-{stamp}", std::process::id()));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let result = (|| -> Result<(), Box<dyn Error>> {
            let mut buffer = [0; 4096];
            for offset in (0..view.len()).step_by(buffer.len()) {
                let count = buffer.len().min(view.len() - offset);
                for (index, byte) in buffer[..count].iter_mut().enumerate() {
                    *byte = view.byte(offset + index).expect("bounded file view");
                }
                file.write_all(&buffer[..count])?;
            }
            file.sync_all()?;
            if let Some(permissions) = permissions {
                fs::set_permissions(&temporary, permissions)?;
            }
            fs::rename(&temporary, &target)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result?;
        eprintln!("[INFO doslet] C: RW saved {name} ({} bytes)", view.len());
        Ok(())
    })
}
