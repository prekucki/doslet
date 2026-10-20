//! Procedure AOT for the running game. Once `le_layout` finds where the
//! loader placed the program's LE objects, the module for that layout is
//! installed from the user cache, or compiled from the live bindings on a
//! background thread and installed when ready. The first calls are measured:
//! procedures too short to beat the interpreter are suspended, and the reason
//! is kept beside the module so later runs skip it.
use crate::le_layout::Layout;
use crate::native_input::dpmi_layout;
use crate::procedure_cache::{Recipe, build};
use doslet_aot::binding::DosFlatObjectBinding;
use doslet_runtime::Session;
use std::{
    error::Error,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::Instant,
};

/// Calls measured before judging their length.
const TRIAL_CALLS: u64 = 10_000;
/// Shorter calls cost more than they save: HEROES.EXE averaged 7–9
/// instructions per call and ran 20% slower than the interpreter; ROTT
/// averaged about 665.
const MIN_INSTRUCTIONS_PER_CALL: u64 = 32;

/// A declined or failed module leaves its reason here; delete it to retry.
pub fn failure_marker(module: &Path) -> PathBuf {
    module.with_file_name("unavailable.txt")
}

pub struct Aot {
    name: String,
    exe: Arc<[u8]>,
    /// `aot/<executable-sha256>` in the user cache.
    directory: PathBuf,
    compiled_cache: Option<PathBuf>,
    /// The session the current layout was found in.
    epoch: u64,
    state: State,
}

enum State {
    Idle,
    Building {
        module: PathBuf,
        layout: String,
        done: mpsc::Receiver<Result<usize, String>>,
        started: Instant,
    },
    /// Waiting for a batch boundary at which RAM is not being recorded.
    Ready {
        module: PathBuf,
        layout: String,
    },
    Measuring {
        module: PathBuf,
    },
}

impl Aot {
    pub fn new(
        name: String,
        exe: Arc<[u8]>,
        directory: PathBuf,
        compiled_cache: Option<PathBuf>,
    ) -> Self {
        Self {
            name,
            exe,
            directory,
            compiled_cache,
            epoch: 0,
            state: State::Idle,
        }
    }

    /// Install the module of `layout`, or start compiling it.
    pub fn layout_found(&mut self, session: &mut Session, layout: &Layout) {
        self.epoch = session.epoch();
        let key = layout.key();
        let module = self.directory.join(&key).join("procedures.wasm");
        if matches!(&self.state, State::Building { module: building, .. } if *building == module) {
            return;
        }
        if let Ok(reason) = std::fs::read_to_string(failure_marker(&module)) {
            eprintln!(
                "[INFO doslet] AOT: no procedures for {}: {}; using the interpreter (delete {} to retry)",
                self.name,
                reason.trim(),
                failure_marker(&module).display()
            );
            self.state = State::Idle;
            return;
        }
        if module.is_file() {
            self.state = State::Ready {
                module,
                layout: key,
            };
            self.poll(session);
            return;
        }
        let (sender, done) = mpsc::channel();
        let exe = self.exe.clone();
        let bindings = layout.bindings.clone();
        let dpmi = dpmi_layout(session.dpmi_mode());
        let (target, layout_key) = (module.clone(), key.clone());
        let spawned = std::thread::Builder::new()
            .name("doslet-aot".into())
            .spawn(move || {
                let result = compile(&exe, &bindings, dpmi, &layout_key, &target);
                if let Err(error) = &result {
                    let _ = std::fs::write(failure_marker(&target), format!("{error}\n"));
                }
                let _ = sender.send(result.map_err(|error| error.to_string()));
            });
        if let Err(error) = spawned {
            eprintln!("[WARN doslet] AOT: cannot start compiling procedures: {error}");
            return;
        }
        eprintln!(
            "[INFO doslet] AOT: compiling procedures for {} (layout {key})",
            self.name
        );
        self.state = State::Building {
            module,
            layout: key,
            done,
            started: Instant::now(),
        };
    }

    /// Install a finished module; judge the call length after enough calls.
    pub fn poll(&mut self, session: &mut Session) {
        match &self.state {
            State::Idle => {}
            State::Building {
                module,
                layout,
                done,
                started,
            } => match done.try_recv() {
                Ok(Ok(entries)) => {
                    eprintln!(
                        "[INFO doslet] AOT: compiled {entries} procedures in {:.1} s ({})",
                        started.elapsed().as_secs_f32(),
                        module.display()
                    );
                    // A reset meanwhile installs it when its layout is found.
                    self.state = if session.epoch() == self.epoch {
                        State::Ready {
                            module: module.clone(),
                            layout: layout.clone(),
                        }
                    } else {
                        State::Idle
                    };
                    self.poll(session);
                }
                Ok(Err(error)) => {
                    eprintln!(
                        "[INFO doslet] AOT: cannot compile procedures for {}: {error}; using the interpreter",
                        self.name
                    );
                    self.state = State::Idle;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.state = State::Idle,
            },
            State::Ready { .. } if session.epoch() != self.epoch => self.state = State::Idle,
            State::Ready { module, layout } => {
                if session.wasm_procedures_installable() {
                    let (module, layout) = (module.clone(), layout.clone());
                    self.install(session, &module, &layout);
                }
            }
            State::Measuring { module } => {
                let Some(stats) = session
                    .wasm_procedure_stats()
                    .filter(|_| session.epoch() == self.epoch)
                else {
                    self.state = State::Idle;
                    return;
                };
                if stats.calls < TRIAL_CALLS {
                    return;
                }
                let per_call = stats.translated_instructions / stats.calls;
                if per_call >= MIN_INSTRUCTIONS_PER_CALL {
                    eprintln!(
                        "[INFO doslet] AOT: {per_call} instructions per call; procedures stay on"
                    );
                } else {
                    let reason = format!(
                        "procedures average {per_call} instructions per call, too few to beat the interpreter"
                    );
                    let _ = session.set_wasm_procedures_suspended(true);
                    let _ = std::fs::write(failure_marker(module), format!("{reason}\n"));
                    eprintln!(
                        "[INFO doslet] AOT: {reason}; suspended, using the interpreter (delete {} to retry)",
                        failure_marker(module).display()
                    );
                }
                self.state = State::Idle;
            }
        }
    }

    fn install(&mut self, session: &mut Session, module: &Path, layout: &str) {
        match crate::native_input::install_layout_procedures(
            session,
            module,
            layout,
            self.compiled_cache.as_deref(),
        ) {
            Ok(()) => {
                // Learned speed tiers charge proved calls to their tier.
                let _ = session.set_wasm_procedure_tier_accounting(true);
                self.state = State::Measuring {
                    module: module.to_owned(),
                };
            }
            Err(error) => {
                eprintln!(
                    "[WARN doslet] AOT: ignoring {}: {error}; using the interpreter",
                    module.display()
                );
                self.state = State::Idle;
            }
        }
    }
}

/// Compile and publish atomically: another process may read the cache.
fn compile(
    exe: &[u8],
    bindings: &[DosFlatObjectBinding],
    dpmi: &str,
    layout: &str,
    target: &Path,
) -> Result<usize, Box<dyn Error>> {
    let module = build(
        exe,
        bindings,
        dpmi,
        layout,
        &Recipe::DESKTOP,
        &mut |_, _, _| Ok(true),
        &mut |_| {},
    )?;
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

    fn local_game(profile: &str, extra: &[&str]) -> (PathBuf, crate::native_input::LaunchSettings) {
        use clap::Parser;
        let profile = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../games")
            .join(profile);
        let mut arguments = vec!["doslet", "--config", profile.to_str().unwrap(), "--no-aot"];
        arguments.extend(extra);
        let cli = crate::cli::Cli::try_parse_from(arguments)
            .unwrap()
            .resolve()
            .unwrap();
        let mut launch = cli.launch;
        launch.persist = false;
        launch.preload_local_media = true;
        (cli.game.unwrap(), launch)
    }

    /// Run `game` from boot with automatic AOT until its module is judged.
    fn run_with_aot(
        game: &Path,
        launch: &crate::native_input::LaunchSettings,
        directory: &Path,
    ) -> (Session, Layout) {
        let exe: Arc<[u8]> = crate::native_zip::program_bytes(game, launch)
            .unwrap()
            .into();
        let mut session = crate::native_zip::load_game_with_settings(game, launch).unwrap();
        let mut watcher = crate::le_layout::Watcher::new(exe.clone()).unwrap();
        let mut aot = Aot::new("test".into(), exe, directory.to_owned(), None);
        session.start();
        let mut pcm = [doslet_pcm::EMPTY; 4096];
        let mut midi = [0; 4096];
        let mut found = None;
        while session.steps() < 400_000_000 {
            session.advance(100_000);
            while session.drain_pcm_timed(&mut pcm) != 0 {}
            while session.drain_midi(&mut midi) != 0 {}
            if let Some(layout) = watcher.poll(&session) {
                aot.layout_found(&mut session, &layout);
                found = Some(layout);
            }
            aot.poll(&mut session);
            if found.is_some() && matches!(aot.state, State::Idle) {
                break;
            }
            if matches!(aot.state, State::Building { .. }) {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        (session, found.expect("live layout"))
    }

    /// `cargo test -p doslet --release --bin doslet rott_module -- --ignored`
    #[test]
    #[ignore = "requires the local ROTT profile; builds its module like a desktop run"]
    fn rott_module_matches_the_offline_generator_and_stays_on() {
        let (game, launch) = local_game("ROTT.toml", &[]);
        let directory =
            std::env::temp_dir().join(format!("doslet-rott-aot-{}", std::process::id()));
        let started = Instant::now();
        let (session, layout) = run_with_aot(&game, &launch, &directory);
        let module = directory.join(layout.key()).join("procedures.wasm");
        eprintln!("{} in {:?}", module.display(), started.elapsed());
        assert!(session.wasm_procedures_active());
        assert!(!failure_marker(&module).exists());
        // The offline module of the same layout predates the layout key in
        // the metadata; compare the code sections.
        let expected = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/rott-aot-direct/procedures.wasm"),
        )
        .unwrap();
        let actual = std::fs::read(&module).unwrap();
        let code = |bytes: &[u8]| {
            let data = doslet_runtime::wasm_procedures::bundle::read(bytes)
                .unwrap()
                .unwrap();
            [data.before, data.after].concat()
        };
        assert!(code(&actual) == code(&expected));
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// `cargo test -p doslet --release --bin doslet heroes_module -- --ignored`
    #[test]
    #[ignore = "requires the local Heroes profile"]
    fn heroes_module_is_suspended_for_short_calls() {
        let (game, launch) = local_game("HoMnM.toml", &[]);
        let directory =
            std::env::temp_dir().join(format!("doslet-heroes-aot-{}", std::process::id()));
        let (session, layout) = run_with_aot(&game, &launch, &directory);
        let module = directory.join(layout.key()).join("procedures.wasm");
        let reason = std::fs::read_to_string(failure_marker(&module)).unwrap();
        eprintln!("{reason}");
        assert!(reason.contains("instructions per call"));
        assert!(!session.wasm_procedures_active());
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
