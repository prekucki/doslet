//! Native setup adapter shared by the CLI and desktop frontend.
use doslet_dos::files::ReadOnlyDrive;
use doslet_runtime::Session;
#[path = "native_patches.rs"]
pub(crate) mod patches;
use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct LaunchSettings {
    pub hardware: doslet_config::HardwareConfig,
    pub machine_spec: Option<doslet_config::MachineSpec>,
    pub command_tail: Vec<u8>,
    pub patches: Vec<PathBuf>,
    pub working_directory: Vec<u8>,
    pub root: Option<PathBuf>,
    pub writable: bool,
    pub persist: bool,
    pub preload_local_media: bool,
    pub cdrom: Option<(u8, PathBuf)>,
    pub heroes_timer_waits: bool,
    pub heroes_menu_idle: bool,
    pub observe_idle: bool,
    pub analyze_synchronization: bool,
    pub adaptive_cpu: bool,
    pub adaptive_idle: bool,
    pub no_dynamic_core: bool,
    pub dpmi: Option<doslet_runtime::session::dpmi::Mode>,
    pub core_profile: Option<PathBuf>,
    /// Without an explicit `core_profile`, one profile per executable lives here.
    #[allow(dead_code, reason = "headless runners select profiles explicitly")]
    pub core_profile_cache: Option<PathBuf>,
    /// An explicit module, installed at load. The desktop otherwise installs
    /// the cached module of the live layout while the game runs.
    pub wasm_procedures: Option<PathBuf>,
    pub interpret_procedures: bool,
    /// Native code compiled from procedure modules; `None` compiles every launch.
    #[allow(dead_code, reason = "read only with the wasm-procedures feature")]
    pub compiled_cache: Option<PathBuf>,
}
impl Default for LaunchSettings {
    fn default() -> Self {
        Self {
            hardware: doslet_config::HardwareConfig::default(),
            machine_spec: None,
            command_tail: Vec::new(),
            patches: Vec::new(),
            working_directory: b"C:\\".to_vec(),
            root: None,
            writable: true,
            persist: false,
            preload_local_media: false,
            cdrom: None,
            heroes_timer_waits: false,
            heroes_menu_idle: false,
            observe_idle: false,
            analyze_synchronization: false,
            adaptive_cpu: false,
            adaptive_idle: false,
            no_dynamic_core: false,
            dpmi: None,
            core_profile: None,
            core_profile_cache: None,
            wasm_procedures: None,
            interpret_procedures: false,
            compiled_cache: None,
        }
    }
}

/// The DPMI mode a procedure module or patch is bound to, as named in its metadata.
pub fn dpmi_layout(mode: Option<doslet_runtime::session::dpmi::Mode>) -> &'static str {
    use doslet_runtime::session::dpmi::Mode;
    match mode {
        None => "off",
        Some(Mode::Reflect) => "reflect",
        Some(Mode::Direct) => "direct",
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
    let program = drive
        .file(filename.as_bytes())
        .map_err(|code| format!("DOS mount error {code}"))?;
    let patch_plan = patches::load(&settings.patches, program)?;
    let mut session =
        Session::from_mz_stub_named_configured(program, dos_name.as_bytes(), settings.hardware)?
            .with_drive(drive);
    if (settings.machine_spec.is_none() || settings.preload_local_media)
        && let Some((letter, image)) = &settings.cdrom
    {
        session = session.with_cdrom_image(*letter, fs::read(image)?)?;
    }
    session.configure_launch(&settings.command_tail, &settings.working_directory)?;
    session.configure_patches(patch_plan)?;
    configure_runtime(&mut session, settings)?;
    Ok(session)
}

pub fn configure_runtime(
    session: &mut Session,
    settings: &LaunchSettings,
) -> Result<(), Box<dyn Error>> {
    if let Some(mode) = settings.dpmi {
        session.enable_dpmi(mode)?;
    }
    session.set_dynamic_core(!settings.no_dynamic_core);
    // These sites are linear addresses of the layout without a DPMI host.
    let heroes_sites = settings.dpmi.is_none();
    if !heroes_sites && (settings.heroes_menu_idle || settings.heroes_timer_waits) {
        eprintln!(
            "[WARN doslet] Experimental Heroes sites use the DPMI off layout; ignored with DPMI {}",
            dpmi_layout(settings.dpmi)
        );
    }
    set_heroes_menu_idle(session, settings.heroes_menu_idle && heroes_sites);
    session.set_idle_observation(settings.observe_idle);
    if settings.adaptive_idle {
        session.set_adaptive_idle(true);
    }
    session.set_synchronization_analysis(settings.analyze_synchronization);
    if settings.adaptive_cpu {
        session.set_adaptive_cpu(true);
    }
    if let Some(path) = &settings.core_profile {
        load_core_profile(session, path);
    }
    if settings.heroes_timer_waits && heroes_sites {
        session.set_memory_wait_sites(0x281098, [0x1E281C, 0x1E2881, 0x1E331A]);
        eprintln!(
            "[INFO doslet] Experimental Heroes timer waits enabled (three validated polling sites)"
        );
    }
    install_wasm_procedures(session, settings)?;
    Ok(())
}

pub fn install_wasm_procedures(
    session: &mut Session,
    settings: &LaunchSettings,
) -> Result<(), Box<dyn Error>> {
    if settings.interpret_procedures && settings.wasm_procedures.is_none() {
        return Err("interpreted procedure boundaries require a procedure bundle".into());
    }
    #[cfg(not(feature = "wasm-procedures"))]
    let _ = session;
    if let Some(path) = &settings.wasm_procedures {
        // Validation precedes any session change; a rejected module stops the launch.
        #[cfg(feature = "wasm-procedures")]
        wasm_procedures::install(
            session,
            path,
            wasm_procedures::Expect::Dpmi(dpmi_layout(session.dpmi_mode())),
            settings.interpret_procedures,
            settings.compiled_cache.as_deref(),
        )?;
        #[cfg(not(feature = "wasm-procedures"))]
        return Err(format!(
            "{} requires the wasm-procedures Cargo feature",
            path.display()
        )
        .into());
    }
    Ok(())
}

/// Install a cached module into a running session whose LE objects were
/// found at `layout` (`le_layout::Layout::key`).
#[cfg(feature = "wasm-procedures")]
#[allow(
    dead_code,
    reason = "only the desktop installs modules while a game runs"
)]
pub fn install_layout_procedures(
    session: &mut Session,
    path: &Path,
    layout: &str,
    compiled_cache: Option<&Path>,
) -> Result<(), Box<dyn Error>> {
    wasm_procedures::install(
        session,
        path,
        wasm_procedures::Expect::Layout(layout),
        false,
        compiled_cache,
    )
}

#[cfg(feature = "wasm-procedures")]
#[path = "native_wasm_procedures.rs"]
mod wasm_procedures;
pub fn set_heroes_menu_idle(session: &mut Session, enabled: bool) {
    session.set_menu_idle_site(enabled.then_some(doslet_runtime::session::MenuIdleSite {
        head: 0x217da2,
        call: 0x206fc0,
        backedge: 0x217f09,
        event_offset: 0x10,
        callback_offset: 0x28,
        callback: 0x2092dc,
        timer: 0x2810a8,
        guard: 0x2832c0,
    }));
    session.set_event_idle_site(enabled.then_some(doslet_runtime::session::EventIdleSite {
        head: 0x219d1a,
        call: 0x213f4c,
        backedge: 0x219d8f,
        queue: 0x2832f0,
        context: 0x2832c4,
    }));
}
/// Optional preparation hints; malformed input cannot prevent interpretation.
pub fn load_core_profile(session: &mut Session, path: &Path) {
    use std::io::Read;
    if !session.dynamic_core_enabled() {
        return;
    }
    let result = (|| -> Result<(), Box<dyn Error>> {
        let file = match fs::File::open(path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let mut bytes = Vec::new();
        file.take(doslet_x86::DYNAMIC_PROFILE_MAX_BYTES as u64 + 1)
            .read_to_end(&mut bytes)?;
        session.load_dynamic_profile(&bytes)?;
        session.prepare_dynamic(8);
        Ok(())
    })();
    if let Err(error) = result {
        eprintln!(
            "[WARN doslet] Ignoring core profile {}: {error}",
            path.display()
        );
    }
}

#[allow(
    dead_code,
    reason = "shared native adapter is also included by diagnostic examples"
)]
pub fn save_core_profile(session: &Session, path: &Path) {
    if !session.dynamic_core_enabled() {
        return;
    }
    let mut bytes = vec![0; doslet_x86::DYNAMIC_PROFILE_MAX_BYTES];
    let length = session.write_dynamic_profile(&mut bytes);
    // Keep useful previous observations when startup has not seen hot code yet.
    if length <= 6 {
        return;
    }
    if let Err(error) = fs::write(path, &bytes[..length]) {
        eprintln!(
            "[WARN doslet] Cannot save core profile {}: {error}",
            path.display()
        );
    }
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
