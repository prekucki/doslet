//! Native setup adapter shared by the CLI and desktop frontend.
use doslet_dos::files::ReadOnlyDrive;
use doslet_runtime::Session;
use std::{error::Error, fs, path::Path};

pub fn load_game(path: &Path) -> Result<Session, Box<dyn Error>> {
    if !fs::metadata(path)?.is_file() {
        return Err("executable path is not a file".into());
    }
    let filename = path
        .file_name()
        .ok_or("missing executable filename")?
        .to_str()
        .ok_or("non-ASCII DOS filename")?
        .to_ascii_uppercase();
    if !filename.is_ascii() {
        return Err("non-ASCII DOS filename".into());
    }
    let dos_name = format!("C:\\{filename}");
    let root = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut drive = ReadOnlyDrive::new();
    mount_directory(root, root, &mut drive)?;
    let drive = drive.with_write_overlay(1024 * 1024);
    Ok(Session::from_mz_stub_named(
        drive
            .file(filename.as_bytes())
            .map_err(|code| format!("DOS mount error {code}"))?,
        dos_name.as_bytes(),
    )?
    .with_drive(drive))
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
