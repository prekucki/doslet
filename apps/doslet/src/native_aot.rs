//! Prepare the AOT procedure module of a launched game in the background.
//! A private copy of the game runs until its LE code is loaded; the bound
//! procedures are then compiled into the user cache for later launches.
use crate::native_input::{LaunchSettings, dpmi_layout};
use crate::procedure_cache::{Recipe, build, capture};
use doslet_config::WritePolicy;
use std::{
    collections::BTreeSet,
    error::Error,
    path::{Path, PathBuf},
    sync::{Mutex, mpsc},
    time::{Duration, Instant},
};

/// ROTT's DOS4GW code is live after about 11 million steps.
const MAX_STEPS: u64 = 60_000_000;
const MAX_TIME: Duration = Duration::from_secs(180);

/// Modules being prepared in this process, so a relaunch does not repeat one.
static ACTIVE: Mutex<BTreeSet<PathBuf>> = Mutex::new(BTreeSet::new());

/// A failed preparation leaves its reason beside the module; delete it to retry.
pub fn failure_marker(module: &Path) -> PathBuf {
    module.with_file_name("unavailable.txt")
}

pub struct Job {
    receiver: mpsc::Receiver<PathBuf>,
}
impl Job {
    /// Start preparing `launch.prepare_procedures` for the program at `game`.
    pub fn start(game: &Path, launch: &LaunchSettings) -> Option<Self> {
        let target = launch.prepare_procedures.clone()?;
        if !ACTIVE.lock().ok()?.insert(target.clone()) {
            return None;
        }
        let settings = private_copy(launch);
        let game = game.to_owned();
        let (sender, receiver) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("doslet-aot".into())
            .spawn(move || {
                let started = Instant::now();
                match prepare(&game, &settings, &target) {
                    Ok(entries) => {
                        eprintln!(
                            "[INFO doslet] AOT: prepared {entries} procedures in {:.1} s ({}); they apply after Reset or the next start",
                            started.elapsed().as_secs_f32(),
                            target.display()
                        );
                        let _ = sender.send(target.clone());
                    }
                    Err(error) => {
                        let marker = failure_marker(&target);
                        let _ = std::fs::write(&marker, format!("{error}\n"));
                        eprintln!(
                            "[WARN doslet] AOT: cannot prepare procedures: {error}; delete {} to retry",
                            marker.display()
                        );
                    }
                }
                if let Ok(mut active) = ACTIVE.lock() {
                    active.remove(&target);
                }
            });
        if spawned.is_err()
            && let Ok(mut active) = ACTIVE.lock()
        {
            active.remove(launch.prepare_procedures.as_ref()?);
        }
        spawned.ok().map(|_| Self { receiver })
    }
    /// The module path once it is written.
    pub fn poll(&self) -> Option<PathBuf> {
        self.receiver.try_recv().ok()
    }
}

/// The same media and machine, but nothing that outlives the copy: no save
/// store, persisted writes, AOT module, profiles or analysis.
fn private_copy(launch: &LaunchSettings) -> LaunchSettings {
    let mut settings = LaunchSettings {
        hardware: launch.hardware,
        machine_spec: launch.machine_spec.clone(),
        command_tail: launch.command_tail.clone(),
        patches: launch.patches.clone(),
        working_directory: launch.working_directory.clone(),
        root: launch.root.clone(),
        writable: true,
        persist: false,
        preload_local_media: true,
        cdrom: launch.cdrom.clone(),
        no_dynamic_core: true,
        dpmi: launch.dpmi,
        ..LaunchSettings::default()
    };
    if let Some(spec) = &mut settings.machine_spec {
        for mount in &mut spec.mounts {
            mount.policy = WritePolicy::Temporary;
            mount.store = None;
        }
    }
    settings
}

fn prepare(game: &Path, settings: &LaunchSettings, target: &Path) -> Result<usize, Box<dyn Error>> {
    let exe = crate::native_zip::program_bytes(game, settings)?;
    let mut session = crate::native_zip::load_game_with_settings(game, settings)?;
    let bindings = capture(&mut session, &exe, MAX_STEPS, Instant::now() + MAX_TIME)?;
    drop(session);
    let module = build(
        &exe,
        &bindings,
        dpmi_layout(settings.dpmi),
        &Recipe::DESKTOP,
        &mut |_, _, _| Ok(true),
        &mut |_| {},
    )?;
    // Another process may read the cache meanwhile; publish complete files only.
    let directory = target.parent().ok_or("invalid AOT cache path")?;
    std::fs::create_dir_all(directory)?;
    let partial = directory.join(format!("procedures.{}.partial", std::process::id()));
    std::fs::write(&partial, &module.bytes)?;
    std::fs::rename(&partial, target)?;
    Ok(module.entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_private_copy_never_writes_saves_or_reuses_aot() {
        let mut launch = LaunchSettings {
            persist: true,
            writable: false,
            wasm_procedures: Some("old.wasm".into()),
            automatic_procedures: true,
            prepare_procedures: Some("new.wasm".into()),
            core_profile: Some("core.profile".into()),
            adaptive_cpu: true,
            dpmi: Some(doslet_runtime::session::dpmi::Mode::Direct),
            ..LaunchSettings::default()
        };
        let mut spec = doslet_config::MachineSpec::default();
        spec.mounts.push(doslet_config::MountSpec {
            letter: b'C',
            source: "game.zip".into(),
            kind: doslet_config::SourceKind::BrowserZip,
            policy: WritePolicy::Persistent,
            store: Some("saves".into()),
        });
        launch.machine_spec = Some(spec);
        let copy = private_copy(&launch);
        assert!(!copy.persist && copy.writable && copy.preload_local_media);
        assert!(copy.wasm_procedures.is_none() && copy.prepare_procedures.is_none());
        assert!(copy.core_profile.is_none() && !copy.adaptive_cpu);
        assert_eq!(copy.dpmi, launch.dpmi);
        let mount = &copy.machine_spec.unwrap().mounts[0];
        assert_eq!(mount.policy, WritePolicy::Temporary);
        assert!(mount.store.is_none());
    }

    /// `cargo test -p doslet --release --bin doslet rott_module -- --ignored`
    #[test]
    #[ignore = "requires the local ROTT profile; prepares its module like a desktop launch"]
    fn rott_module_matches_the_offline_generator() {
        use clap::Parser;
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let profile = root.join("games/ROTT.toml");
        let cli =
            crate::cli::Cli::try_parse_from(["doslet", "--config", profile.to_str().unwrap()])
                .unwrap()
                .resolve()
                .unwrap();
        let target = std::env::temp_dir()
            .join(format!("doslet-rott-aot-{}", std::process::id()))
            .join("procedures.wasm");
        let started = Instant::now();
        let entries = prepare(
            cli.game.as_deref().unwrap(),
            &private_copy(&cli.launch),
            &target,
        )
        .unwrap();
        eprintln!("prepared {entries} entries in {:?}", started.elapsed());
        let expected = std::fs::read(root.join("target/rott-aot-direct/procedures.wasm")).unwrap();
        assert!(std::fs::read(&target).unwrap() == expected);
        std::fs::remove_dir_all(target.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_real_mode_program_reports_why_no_module_exists() {
        let dir = std::env::temp_dir().join(format!("doslet-aot-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // MOV AX,4C00h; INT 21h: an MZ-less COM program has no LE image.
        let game = dir.join("GAME.COM");
        std::fs::write(&game, [0xB8, 0x00, 0x4C, 0xCD, 0x21]).unwrap();
        let target = dir.join("dpmi-direct").join("procedures.wasm");
        let error = prepare(&game, &LaunchSettings::default(), &target).unwrap_err();
        assert!(!target.exists(), "{error}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
