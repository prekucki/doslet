//! Procedure AOT for the running game. Once `le_layout` finds where the
//! loader placed the program's LE objects or COFF sections, the module for
//! that layout is installed from the user cache, or compiled from the live
//! bindings on a background thread and installed when ready. The first calls
//! are measured per entry: only entries averaging enough work remain enabled.
//! If none qualify, the module is removed, returning memory to the interpreter's
//! own owners, and the reason is kept beside the module so later runs skip it.
//!
//! A DJGPP program's later layouts follow it: when its block moves, the
//! installed procedures move their source checks with it; when it has patched
//! its code, the module of the patched text replaces the installed one.
use crate::le_layout::{Layout, program_image};
use crate::native_input::dpmi_layout;
use crate::procedure_cache::{CodeSegment, Recipe, Target, build_target};
use doslet_runtime::Session;
use std::{
    error::Error,
    path::{Path, PathBuf},
    sync::{Arc, mpsc},
    time::Instant,
};

#[cfg(test)]
#[path = "nonflat_slot_tests.rs"]
mod nonflat_slot_tests;

/// Check interval across the module; each entry has its own sample minimum.
const TRIAL_CALLS: u64 = 10_000;
const ENTRY_TRIAL_CALLS: u64 = 128;
/// Shorter calls cost more than they save: HEROES.EXE averaged 7–9
/// instructions per call and ran 20% slower than the interpreter; ROTT
/// averaged about 665.
const MIN_INSTRUCTIONS_PER_CALL: u64 = 32;

// Bump when admission or code generation changes: stale failure markers must
// not suppress a new compiler, even when the executable and layout are equal.
const COMPILER_CACHE_REVISION: &str = "coff-procedure-colors-v7";

pub fn module_path(directory: &Path, layout: &Layout) -> PathBuf {
    directory
        .join(COMPILER_CACHE_REVISION)
        .join(layout.key())
        .join("procedures.wasm")
}

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
    /// The layout whose module is installed in that session.
    installed: Option<Layout>,
}

enum State {
    Idle,
    Building {
        module: PathBuf,
        layout: Layout,
        done: mpsc::Receiver<Result<usize, String>>,
        started: Instant,
    },
    /// Waiting for a batch boundary at which RAM is not being recorded.
    Ready {
        module: PathBuf,
        layout: Layout,
    },
    Measuring {
        module: PathBuf,
        next_check: u64,
    },
    /// Suspended for short calls, to be removed at an unrecorded batch
    /// boundary.
    Removing,
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
            installed: None,
        }
    }

    /// Install the module of `layout`, or start compiling it.
    pub fn layout_found(&mut self, session: &mut Session, layout: &Layout) {
        if session.epoch() != self.epoch {
            self.installed = None;
        }
        self.epoch = session.epoch();
        if self.follow_moved_code(session, layout) {
            return;
        }
        let key = layout.key();
        let module = module_path(&self.directory, layout);
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
                layout: layout.clone(),
            };
            self.poll(session);
            return;
        }
        let (sender, done) = mpsc::channel();
        let exe = self.exe.clone();
        let job = layout.clone();
        let dpmi = dpmi_layout(session.dpmi_mode());
        let target = module.clone();
        let spawned = std::thread::Builder::new()
            .name("doslet-aot".into())
            .spawn(move || {
                let result = compile(&exe, &job, dpmi, &target);
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
            layout: layout.clone(),
            done,
            started: Instant::now(),
        };
    }

    /// The installed module also serves `layout` when only the code's base
    /// changed: its compiled procedures name EIPs.
    fn follow_moved_code(&mut self, session: &mut Session, layout: &Layout) -> bool {
        let Some(installed) = &self.installed else {
            return false;
        };
        if session.wasm_procedure_code_segment().is_none()
            || installed.text.is_none()
            || installed.text != layout.text
            || installed.code.limit != layout.code.limit
        {
            return false;
        }
        if installed.code.base != layout.code.base {
            match session.rebase_wasm_procedures(layout.code.base) {
                Ok(()) => eprintln!(
                    "[INFO doslet] AOT: procedures follow {} to its code at {:#x}",
                    self.name, layout.code.base
                ),
                Err(error) => {
                    eprintln!("[WARN doslet] AOT: cannot move procedures: {error}");
                    return false;
                }
            }
        }
        self.installed = Some(layout.clone());
        true
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
                if session.wasm_procedures_installable() || session.wasm_procedures_replaceable() {
                    let (module, layout) = (module.clone(), layout.clone());
                    self.install(session, &module, layout);
                }
            }
            State::Measuring { module, next_check } => {
                let Some(stats) = session
                    .wasm_procedure_stats()
                    .filter(|_| session.epoch() == self.epoch)
                else {
                    self.state = State::Idle;
                    return;
                };
                if stats.calls < *next_check {
                    return;
                }
                let (retained, pending, changed) = match select_measured_entries(session) {
                    Ok(retained) => retained,
                    Err(error) => {
                        eprintln!("[WARN doslet] AOT: cannot select measured entries: {error}");
                        self.state = State::Idle;
                        return;
                    }
                };
                if retained != 0 {
                    if changed != 0 || *next_check == TRIAL_CALLS || pending == 0 {
                        eprintln!(
                            "[INFO doslet] AOT: {} entries average at least {MIN_INSTRUCTIONS_PER_CALL} instructions per call; {pending} still sampling, {changed} newly disabled",
                            retained - pending
                        );
                    }
                } else {
                    let reason = format!(
                        "no measured entry averages {MIN_INSTRUCTIONS_PER_CALL} instructions per call after {} trial calls",
                        stats.calls
                    );
                    let _ = std::fs::write(failure_marker(module), format!("{reason}\n"));
                    eprintln!(
                        "[INFO doslet] AOT: {reason}; removed, using the interpreter (delete {} to retry)",
                        failure_marker(module).display()
                    );
                    // A suspended plan keeps RAM in its shared memory, which
                    // slows the interpreter; removal waits for unrecorded RAM.
                    let _ = session.set_wasm_procedures_suspended(true);
                    self.state = State::Removing;
                    self.poll(session);
                    return;
                }
                self.state = if pending != 0 {
                    State::Measuring {
                        module: module.clone(),
                        next_check: stats.calls.saturating_add(TRIAL_CALLS),
                    }
                } else {
                    State::Idle
                };
            }
            State::Removing if session.epoch() != self.epoch => self.state = State::Idle,
            State::Removing => {
                if session.wasm_procedures_replaceable() {
                    if let Err(error) = session.uninstall_wasm_procedures() {
                        eprintln!("[WARN doslet] AOT: procedures stay suspended: {error}");
                    }
                    self.installed = None;
                    self.state = State::Idle;
                }
            }
        }
    }

    fn install(&mut self, session: &mut Session, module: &Path, layout: Layout) {
        match crate::native_input::install_layout_procedures(
            session,
            module,
            &layout.key(),
            self.compiled_cache.as_deref(),
        ) {
            Ok(()) => {
                // Learned speed tiers charge proved calls to their tier.
                let _ = session.set_wasm_procedure_tier_accounting(true);
                self.installed = Some(layout);
                self.state = State::Measuring {
                    module: module.to_owned(),
                    next_check: TRIAL_CALLS,
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

/// Each entry gets its own trial, including regions first reached in a later
/// frame. Return enabled, still sampling and newly disabled entry counts.
fn select_measured_entries(session: &mut Session) -> Result<(usize, usize, usize), &'static str> {
    let mut pending = 0;
    let mut changed = 0;
    let enabled: Vec<_> = (0..session.wasm_procedure_entry_count())
        .map(|index| {
            let stats = session
                .wasm_procedure_entry_stats(index)
                .expect("indexed entry");
            let sampling = stats.calls < ENTRY_TRIAL_CALLS;
            pending += usize::from(sampling);
            let enabled = sampling
                || stats.translated_instructions / stats.calls >= MIN_INSTRUCTIONS_PER_CALL;
            changed +=
                usize::from(!enabled && session.wasm_procedure_entry_enabled(index) == Some(true));
            enabled
        })
        .collect();
    let retained = enabled.iter().filter(|&&enabled| enabled).count();
    session.set_wasm_procedure_entries_enabled(&enabled)?;
    Ok((retained, pending, changed))
}

/// Compile and publish atomically: another process may read the cache.
fn compile(
    exe: &[u8],
    layout: &Layout,
    dpmi: &str,
    target: &Path,
) -> Result<usize, Box<dyn Error>> {
    compile_with_recipe(exe, layout, dpmi, target, &Recipe::DESKTOP)
}

fn compile_with_recipe(
    exe: &[u8],
    layout: &Layout,
    dpmi: &str,
    target: &Path,
    recipe: &Recipe,
) -> Result<usize, Box<dyn Error>> {
    let image = program_image(exe, layout.text.as_deref())?;
    let module = build_target(
        &Target {
            exe,
            image: &image,
            bindings: &layout.bindings,
            code: CodeSegment {
                base: layout.code.base,
                limit: layout.code.limit,
            },
            dpmi,
            layout: &layout.key(),
        },
        recipe,
        &mut |_, _, _| Ok(true),
        &mut |message| {
            if message.starts_with("Pre-AOT analysis:") || message.starts_with("  execution=") {
                eprintln!("[INFO doslet] AOT: {message}");
            }
        },
    )?;
    let directory = target.parent().ok_or("invalid AOT cache path")?;
    std::fs::create_dir_all(directory)?;
    let partial = directory.join(format!("procedures.{}.partial", std::process::id()));
    std::fs::write(&partial, &module.bytes)?;
    std::fs::rename(&partial, target)?;
    Ok(module.entries)
}

/// Quake v1.06 waits for a key after its "MSCDEX not loaded" notice and then
/// offers joystick calibration: space at 3 s, ESC at 12 s (seconds, HID usage,
/// pressed) reach its demo behind the main menu by about 15 s.
#[cfg(test)]
pub(crate) const QUAKE_KEYS: [(u64, u16, bool); 4] = [
    (3, 0x2c, true),
    (4, 0x2c, false),
    (12, 0x29, true),
    (13, 0x29, false),
];

#[cfg(test)]
pub(crate) fn test_cache_directory(label: &str) -> PathBuf {
    // Sandboxed test processes can reuse the same PID. Never reuse an old
    // module or failure marker left behind by an interrupted test.
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("test clock after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("doslet-{label}-{}-{nonce}", std::process::id()))
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
        let directory = test_cache_directory("rott-aot");
        let started = Instant::now();
        let (session, layout) = run_with_aot(&game, &launch, &directory);
        let module = module_path(&directory, &layout);
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
        let directory = test_cache_directory("heroes-aot");
        let (session, layout) = run_with_aot(&game, &launch, &directory);
        let module = module_path(&directory, &layout);
        let reason = std::fs::read_to_string(failure_marker(&module)).unwrap();
        eprintln!("{reason}");
        assert!(reason.contains("instructions per call"));
        assert!(!session.wasm_procedures_active());
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// Run Quake to guest time `until`, pressing `QUAKE_KEYS` from `pressed`
    /// on, with `each` after every batch.
    fn quake_advance(
        session: &mut Session,
        pressed: &mut usize,
        until: u64,
        each: &mut dyn FnMut(&mut Session),
    ) {
        let mut pcm = [doslet_pcm::EMPTY; 1024];
        let mut midi = [0; 1024];
        while session.emulated_nanos() < until
            && session.status() == doslet_runtime::SessionStatus::Running
        {
            session.advance(10_000);
            while session.drain_pcm_timed(&mut pcm) != 0 {}
            while session.drain_midi(&mut midi) != 0 {}
            if let Some(&(second, usage, down)) = QUAKE_KEYS.get(*pressed)
                && session.emulated_nanos() >= second * 1_000_000_000
            {
                session.physical_key(usage, down).unwrap();
                *pressed += 1;
            }
            each(session);
        }
    }

    /// `cargo test -p doslet --release --locked --bin doslet quake_coff -- --ignored --nocapture`
    #[test]
    #[ignore = "requires the local Quake profile; builds its modules like a desktop run"]
    fn quake_coff_layouts_are_followed_and_their_modules_judged() {
        let (game, launch) = local_game("Quake.toml", &["--dpmi", "direct"]);
        let directory = test_cache_directory("quake-aot");
        let exe: Arc<[u8]> = crate::native_zip::program_bytes(&game, &launch)
            .unwrap()
            .into();
        let coff = doslet_executable::CoffImage::discover(&exe, Default::default()).unwrap();
        let text = coff.text();
        let mut session = crate::native_zip::load_game_with_settings(&game, &launch).unwrap();
        let mut watcher = crate::le_layout::Watcher::new(exe.clone()).unwrap();
        let mut aot = Aot::new("QUAKE.EXE".into(), exe.clone(), directory.clone(), None);
        let mut layouts = Vec::new();
        let mut pressed = 0;
        session.start();
        let started = Instant::now();
        let mut follow = |session: &mut Session| {
            if let Some(layout) = watcher.poll(session) {
                eprintln!(
                    "{:.2} s: layout {} (code base {:#x}, limit {:#x})",
                    session.emulated_nanos() as f64 / 1e9,
                    layout.key(),
                    layout.code.base,
                    layout.code.limit
                );
                aot.layout_found(session, &layout);
                layouts.push(layout);
            }
            aot.poll(session);
        };
        quake_advance(&mut session, &mut pressed, 20_000_000_000, &mut follow);
        drop(follow);
        // The last module is compiled in the background and then judged.
        while !matches!(aot.state, State::Idle) && session.emulated_nanos() < 40_000_000_000 {
            let until = session.emulated_nanos() + 10_000_000;
            quake_advance(&mut session, &mut pressed, until, &mut |session| {
                aot.poll(session)
            });
        }
        eprintln!(
            "{:.1} guest s in {:?}: {:?}, AOT active {}, stats {:?}",
            session.emulated_nanos() as f64 / 1e9,
            started.elapsed(),
            session.status(),
            session.wasm_procedures_active(),
            session.wasm_procedure_stats()
        );
        assert_eq!(session.status(), doslet_runtime::SessionStatus::Running);
        assert!(matches!(aot.state, State::Idle | State::Measuring { .. }));
        // Found once program text runs, then again after Quake patched its
        // drawing code; the block did not move in this run.
        assert!(layouts.len() >= 2, "{} layouts", layouts.len());
        for layout in &layouts {
            let live = layout.text.as_deref().unwrap();
            let changed: Vec<_> = (0..live.len())
                .filter(|&i| live[i] != text.data[i])
                .collect();
            eprintln!(
                "{}: {} text bytes differ from the file, first at {:x?}",
                layout.key(),
                changed.len(),
                changed
                    .iter()
                    .take(4)
                    .map(|&i| i as u32 + text.virtual_address)
                    .collect::<Vec<_>>()
            );
        }
        assert!(layouts.windows(2).all(|pair| pair[0].text != pair[1].text));
        let base = layouts[0].code.base;
        for (index, layout) in layouts.iter().enumerate() {
            assert_eq!(
                layout.code,
                crate::le_layout::CodeSpace {
                    base,
                    limit: text.virtual_address + text.size - 1
                }
            );
            assert!(layouts[..index].iter().all(|l| l.key() != layout.key()));
            let module = module_path(&directory, layout);
            assert!(module.is_file(), "{}", module.display());
            if let Ok(reason) = std::fs::read_to_string(failure_marker(&module)) {
                eprintln!("{}: {}", layout.key(), reason.trim());
                assert!(reason.contains("instructions per call"), "{reason}");
            } else {
                eprintln!("{}: procedures stay on", layout.key());
            }
        }
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// `cargo test -p doslet --release --locked --bin doslet quake_aot_matches -- --ignored --nocapture`
    /// `QUAKE_COMPARE_STEPS` (default 200,000,000) bounds the replay.
    #[test]
    #[ignore = "requires the local Quake profile; replays its demo with and without AOT"]
    fn quake_aot_matches_the_interpreter_from_a_demo_state() {
        let (game, launch) = local_game("Quake.toml", &["--dpmi", "direct"]);
        let exe: Arc<[u8]> = crate::native_zip::program_bytes(&game, &launch)
            .unwrap()
            .into();
        let mut session = crate::native_zip::load_game_with_settings(&game, &launch).unwrap();
        let mut watcher = crate::le_layout::Watcher::new(exe.clone()).unwrap();
        let mut layout = None;
        let mut pressed = 0;
        session.start();
        quake_advance(&mut session, &mut pressed, 16_000_000_000, &mut |session| {
            if let Some(found) = watcher.poll(session) {
                layout = Some(found);
            }
        });
        let layout = layout.expect("live COFF layout");
        assert!(layout.text.is_some());
        if std::env::var("QUAKE_COMPARE_DUMP_TEXT").as_deref() == Ok("1") {
            // Local compatibility diagnostics only; target/ remains ignored.
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/quake-aot-replay-text.bin");
            std::fs::write(path, layout.text.as_deref().unwrap()).unwrap();
        }
        let state = session.save_state().unwrap();
        drop(session);
        let directory = test_cache_directory("quake-compare");
        let module = module_path(&directory, &layout);
        let mut recipe = Recipe::DESKTOP;
        recipe.pc24_locals = std::env::var("QUAKE_COMPARE_PC24").as_deref() == Ok("1");
        if let Ok(value) = std::env::var("QUAKE_COMPARE_WORK_LIMIT") {
            recipe.max_work_per_call =
                Some(value.parse().expect("invalid QUAKE_COMPARE_WORK_LIMIT"));
        }
        if std::env::var("QUAKE_COMPARE_LEGACY").as_deref() == Ok("1") {
            recipe.bounded_fragments = false;
            recipe.long_fragments = false;
        }
        if std::env::var("QUAKE_COMPARE_LONG_FRAGMENTS").as_deref() == Ok("0") {
            recipe.long_fragments = false;
        }
        eprintln!(
            "Quake AOT recipe: bounded_fragments={}, long_fragments={}, maximum_work={:?}, copy_work={:?}, pc24_locals={}",
            recipe.bounded_fragments,
            recipe.long_fragments,
            recipe.max_work_per_call,
            recipe.copy_work_per_call,
            recipe.pc24_locals
        );
        let entries = compile_with_recipe(&exe, &layout, "direct", &module, &recipe).unwrap();
        let module_bytes = std::fs::read(&module).unwrap();
        let bundle = doslet_x86_wasm::static_program::procedure_memory::bundle::read(&module_bytes)
            .unwrap()
            .unwrap();
        let manifest: serde_json::Value = serde_json::from_slice(bundle.metadata).unwrap();
        let mut x87_entries: Vec<u32> = manifest["functions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|entry| entry["x87"].is_object())
            .map(|entry| entry["entry"].as_u64().unwrap() as u32)
            .collect();
        x87_entries.sort_unstable();
        eprintln!(
            "compiled entries containing x87: {}/{}",
            x87_entries.len(),
            entries
        );
        let media = crate::native_zip::load_game_with_settings(&game, &launch).unwrap();
        let mut reference = Session::load_state_with_media(&state, &media).unwrap();
        let mut translated = Session::load_state_with_media(&state, &media).unwrap();
        // Empty labels classify every instruction as GREY while exercising
        // the same tier-proof path used by learned native/browser profiles.
        // Equal rates preserve the configured 266 MHz guest clock.
        let tier_accounting = std::env::var("QUAKE_COMPARE_TIERS").as_deref() == Ok("1");
        if tier_accounting {
            for session in [&mut reference, &mut translated] {
                session
                    .set_execution_tiers(Some(Vec::new()), [Some(266); 3])
                    .unwrap();
            }
        }
        reference.start();
        translated.start();
        crate::native_input::install_layout_procedures(
            &mut translated,
            &module,
            &layout.key(),
            None,
        )
        .unwrap();
        translated.set_wasm_procedure_tier_accounting(true).unwrap();
        eprintln!("Quake AOT replay: active GREY labels={tier_accounting}");
        let steps: u64 = std::env::var("QUAKE_COMPARE_STEPS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(200_000_000);
        let checkpoints = 4;
        let start_nanos = reference.emulated_nanos();
        let advance = |session: &mut Session, budget: u64| {
            u64::from(session.advance(budget.min(u64::from(u32::MAX)) as u32))
        };
        use sha2::{Digest, Sha256};
        let (mut reference_audio, mut translated_audio) = (Sha256::new(), Sha256::new());
        let mut completed = 0u64;
        let filter_entries = std::env::var("QUAKE_COMPARE_ENTRY_FILTER").as_deref() == Ok("1");
        let mut filtered = false;
        let mut next_filter = TRIAL_CALLS;
        for checkpoint in 1..=checkpoints {
            let target = steps * checkpoint / checkpoints;
            while completed < target {
                let budget = (target - completed).min(4096);
                let mut ordinary = advance(&mut reference, budget);
                let mut actual = advance(&mut translated, budget);
                if filter_entries && translated.wasm_procedure_stats().unwrap().calls >= next_filter
                {
                    let (retained, pending, changed) =
                        select_measured_entries(&mut translated).unwrap();
                    if !filtered || changed != 0 {
                        eprintln!(
                            "entry trial: enabled {retained}/{entries}, sampling {pending}, newly disabled {changed}"
                        );
                    }
                    next_filter = translated
                        .wasm_procedure_stats()
                        .unwrap()
                        .calls
                        .saturating_add(TRIAL_CALLS);
                    filtered = true;
                }
                // Whole computations may finish past a soft budget; bring the
                // other replay to the same logical boundary.
                for _ in 0..8 {
                    if ordinary < actual {
                        ordinary += advance(&mut reference, actual - ordinary);
                    } else if actual < ordinary {
                        actual += advance(&mut translated, ordinary - actual);
                    }
                }
                assert_eq!(ordinary, actual, "replay progress at {completed}");
                assert_ne!(ordinary, 0, "{:?}", translated.fault());
                completed += ordinary;
                for (session, audio) in [
                    (&mut reference, &mut reference_audio),
                    (&mut translated, &mut translated_audio),
                ] {
                    let mut pcm = [doslet_pcm::EMPTY; 1024];
                    let mut midi = [0; 1024];
                    loop {
                        let count = session.drain_pcm_timed(&mut pcm);
                        if count == 0 {
                            break;
                        }
                        for frame in &pcm[..count] {
                            audio.update(frame.nanos.to_le_bytes());
                            audio.update(frame.duration.to_le_bytes());
                            for sample in frame.samples {
                                audio.update(sample.to_le_bytes());
                            }
                        }
                    }
                    loop {
                        let count = session.drain_midi(&mut midi);
                        if count == 0 {
                            break;
                        }
                        audio.update([0xff]);
                        audio.update(&midi[..count]);
                    }
                }
            }
            assert_eq!(translated.status(), doslet_runtime::SessionStatus::Running);
            let equal = reference.save_state().unwrap() == translated.save_state().unwrap();
            let audio = reference_audio.clone().finalize() == translated_audio.clone().finalize();
            eprintln!(
                "step {completed}: states equal {equal}, PCM and MIDI equal {audio}, stats {:?}",
                translated.wasm_procedure_stats()
            );
            assert!(equal, "full portable state differs at step {completed}");
            assert!(audio, "drained audio differs at step {completed}");
            assert_eq!(
                reference.execution_tier_stats(),
                translated.execution_tier_stats()
            );
            assert_eq!(
                reference.executed_tier_counts(),
                translated.executed_tier_counts()
            );
        }
        let stats = translated.wasm_procedure_stats().unwrap();
        assert!(stats.calls > 0 && stats.translated_instructions > 0);
        let guest = (reference.emulated_nanos() - start_nanos) as f64 / 1e9;
        eprintln!(
            "{entries} procedures; {completed} scheduling steps ({guest:.2} guest s): AOT work / scheduling steps = {:.2}% in {} calls ({} instructions per call)",
            100.0 * stats.translated_instructions as f64 / completed as f64,
            stats.calls,
            stats.translated_instructions / stats.calls
        );
        let mut per_entry: Vec<_> = (0..translated.wasm_procedure_entry_count())
            .map(|index| {
                (
                    translated.wasm_procedure_entry_ip(index).unwrap(),
                    translated.wasm_procedure_entry_stats(index).unwrap(),
                )
            })
            .collect();
        assert_eq!(
            per_entry.iter().map(|(_, s)| s.calls).sum::<u64>(),
            stats.calls
        );
        assert_eq!(
            per_entry
                .iter()
                .map(|(_, s)| s.translated_instructions)
                .sum::<u64>(),
            stats.translated_instructions
        );
        assert_eq!(
            per_entry.iter().map(|(_, s)| s.side_exits).sum::<u64>(),
            stats.side_exits
        );
        per_entry.sort_unstable_by_key(|(_, s)| std::cmp::Reverse(s.translated_instructions));
        let x87_work: u64 = per_entry
            .iter()
            .filter(|(ip, _)| x87_entries.binary_search(ip).is_ok())
            .map(|(_, stats)| stats.translated_instructions)
            .sum();
        let x87_calls: u64 = per_entry
            .iter()
            .filter(|(ip, _)| x87_entries.binary_search(ip).is_ok())
            .map(|(_, stats)| stats.calls)
            .sum();
        eprintln!(
            "entries containing x87: {x87_calls} calls, {x87_work} mixed scalar/x87 instructions (not an x87-only count)"
        );
        let manifest_entries = manifest["functions"].as_array().unwrap();
        let mut color_work = std::collections::BTreeMap::new();
        for (ip, stats) in per_entry.iter().filter(|(_, stats)| stats.calls != 0) {
            let entry = manifest_entries
                .iter()
                .find(|entry| entry["entry"].as_u64() == Some(u64::from(*ip)))
                .expect("entry has analyzed metadata");
            let analysis = &entry["analysis"];
            let key = (
                analysis["execution"].as_str().unwrap(),
                analysis["x87_class"].as_str().unwrap(),
            );
            let totals = color_work.entry(key).or_insert((0u64, 0u64, 0u64));
            totals.0 += stats.calls;
            totals.1 += stats.translated_instructions;
            totals.2 += stats.side_exits;
        }
        eprintln!("Executed procedure colors (work includes all instructions in each region):");
        for ((strategy, x87), (calls, work, exits)) in color_work {
            eprintln!(
                "  execution={strategy}, x87={x87}: calls={calls} instructions={work} side_exits={exits} AOT_work_share={:.2}%",
                100.0 * work as f64 / stats.translated_instructions as f64
            );
        }
        if std::env::var("QUAKE_COMPARE_DUMP_COLORS").as_deref() == Ok("1") {
            let entries: Vec<_> = per_entry
                .iter()
                .map(|(ip, stats)| {
                    let entry = manifest_entries
                        .iter()
                        .find(|entry| entry["entry"].as_u64() == Some(u64::from(*ip)))
                        .unwrap();
                    serde_json::json!({"entry":ip,"analysis":entry["analysis"],
                    "calls":stats.calls,"translated_instructions":stats.translated_instructions,
                    "side_exits":stats.side_exits})
                })
                .collect();
            let report = serde_json::json!({"version":1,
                "replay_start_sha256":format!("{:x}", Sha256::digest(&state)),
                "scheduling_steps":completed,"aot_instructions":stats.translated_instructions,
                "functions":entries});
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../target/quake-procedure-colors.json");
            std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
            eprintln!("Saved target/quake-procedure-colors.json");
        }
        eprintln!(
            "replay start SHA256 {:x}; top entries by executed AOT work:",
            Sha256::digest(&state)
        );
        for (ip, entry) in per_entry.iter().filter(|(_, s)| s.calls != 0).take(20) {
            eprintln!(
                "  EIP {ip:#010x}: calls={} instructions={} mean={:.2} side_exits={} contains_x87={}",
                entry.calls,
                entry.translated_instructions,
                entry.translated_instructions as f64 / entry.calls as f64,
                entry.side_exits,
                x87_entries.binary_search(ip).is_ok()
            );
        }
        let long_entries: Vec<_> = per_entry
            .iter()
            .filter(|(_, s)| {
                s.calls != 0 && s.translated_instructions / s.calls >= MIN_INSTRUCTIONS_PER_CALL
            })
            .collect();
        eprintln!(
            "entries averaging >= {MIN_INSTRUCTIONS_PER_CALL}: {} of {} called; calls={} instructions={}; trial filter applied={filtered}",
            long_entries.len(),
            per_entry.iter().filter(|(_, s)| s.calls != 0).count(),
            long_entries.iter().map(|(_, s)| s.calls).sum::<u64>(),
            long_entries
                .iter()
                .map(|(_, s)| s.translated_instructions)
                .sum::<u64>()
        );
        // Coverage and equivalence are the default experiment. Timing this
        // low-coverage module does not guide which compiler barrier to remove.
        let color_timing = std::env::var("QUAKE_COMPARE_COLOR_TIMING").as_deref() == Ok("1");
        if std::env::var("QUAKE_COMPARE_TIMING").as_deref() != Ok("1") && !color_timing {
            std::fs::remove_dir_all(&directory).unwrap();
            return;
        }
        let expected_final_state = reference.save_state().unwrap();
        drop((reference, translated));
        // Timing: each variant replays the same steps alone, in rotating
        // order, five times. Report median and range; keep admission sampling
        // and batch sizes identical to the correctness replay above. Module
        // installation is outside the timer. Audio is drained, not hashed.
        let ordinary_variants = [
            "interpreter",
            "AOT",
            "AOT installed but suspended",
            "AOT installed, then removed",
        ];
        let color_variants = [
            "interpreter",
            "AOT all colors",
            "AOT x87 none",
            "AOT x87 none + metadata-only + independent-balanced",
            "AOT x87 regions only",
        ];
        let variants = if color_timing {
            &color_variants[..]
        } else {
            &ordinary_variants[..]
        };
        let count =
            if !color_timing && std::env::var("QUAKE_COMPARE_TIMING_PAIR").as_deref() == Ok("1") {
                2
            } else {
                variants.len()
            };
        let mut samples: Vec<Vec<f64>> = (0..count).map(|_| Vec::with_capacity(5)).collect();
        for pass in 0usize..(5 + usize::from(color_timing)) {
            let validation = color_timing && pass == 0;
            let phase = if validation { "validation" } else { "timing" };
            let round = pass.saturating_sub(usize::from(color_timing));
            for slot in 0..count {
                let variant = (slot + round) % count;
                let mut session = Session::load_state_with_media(&state, &media).unwrap();
                if tier_accounting {
                    session
                        .set_execution_tiers(Some(Vec::new()), [Some(266); 3])
                        .unwrap();
                }
                session.start();
                let mut color_mask = None;
                if variant > 0 {
                    crate::native_input::install_layout_procedures(
                        &mut session,
                        &module,
                        &layout.key(),
                        None,
                    )
                    .unwrap();
                    session.set_wasm_procedure_tier_accounting(true).unwrap();
                    session
                        .set_wasm_procedures_suspended(!color_timing && variant == 2)
                        .unwrap();
                    if !color_timing && variant == 3 {
                        session.uninstall_wasm_procedures().unwrap();
                    }
                    if color_timing {
                        let mask: Vec<_> = (0..session.wasm_procedure_entry_count())
                            .map(|index| {
                                let ip = session.wasm_procedure_entry_ip(index).unwrap();
                                let entry = manifest_entries
                                    .iter()
                                    .find(|entry| entry["entry"].as_u64() == Some(u64::from(ip)))
                                    .expect("timed entry has analyzed metadata");
                                let class = entry["analysis"]["x87_class"].as_str().unwrap();
                                match variant {
                                    1 => true,
                                    2 => class == "none",
                                    3 => matches!(
                                        class,
                                        "none" | "metadata-only" | "independent-balanced"
                                    ),
                                    4 => class != "none",
                                    _ => unreachable!(),
                                }
                            })
                            .collect();
                        eprintln!(
                            "{phase} round {} {}: selected {} of {} entries",
                            round + 1,
                            variants[variant],
                            mask.iter().filter(|&&enabled| enabled).count(),
                            mask.len()
                        );
                        session.set_wasm_procedure_entries_enabled(&mask).unwrap();
                        color_mask = Some(mask);
                    }
                }
                let mut pcm = [doslet_pcm::EMPTY; 1024];
                let mut midi = [0; 1024];
                let mut next_filter = TRIAL_CALLS;
                let mut validation_audio = validation.then(Sha256::new);
                let started = Instant::now();
                let mut done = 0u64;
                while done < steps {
                    done += advance(&mut session, (steps - done).min(4096));
                    if (variant == 1 || (color_timing && variant > 0))
                        && filter_entries
                        && session.wasm_procedure_stats().unwrap().calls >= next_filter
                    {
                        if let Some(mask) = &color_mask {
                            let enabled: Vec<_> = mask
                                .iter()
                                .enumerate()
                                .map(|(index, &allowed)| {
                                    let stats = session
                                        .wasm_procedure_entry_stats(index)
                                        .expect("indexed entry");
                                    allowed
                                        && (stats.calls < ENTRY_TRIAL_CALLS
                                            || stats.translated_instructions / stats.calls
                                                >= MIN_INSTRUCTIONS_PER_CALL)
                                })
                                .collect();
                            session
                                .set_wasm_procedure_entries_enabled(&enabled)
                                .unwrap();
                        } else {
                            select_measured_entries(&mut session).unwrap();
                        }
                        next_filter = session
                            .wasm_procedure_stats()
                            .unwrap()
                            .calls
                            .saturating_add(TRIAL_CALLS);
                    }
                    loop {
                        let count = session.drain_pcm_timed(&mut pcm);
                        if count == 0 {
                            break;
                        }
                        if let Some(audio) = &mut validation_audio {
                            for frame in &pcm[..count] {
                                audio.update(frame.nanos.to_le_bytes());
                                audio.update(frame.duration.to_le_bytes());
                                for sample in frame.samples {
                                    audio.update(sample.to_le_bytes());
                                }
                            }
                        }
                    }
                    loop {
                        let count = session.drain_midi(&mut midi);
                        if count == 0 {
                            break;
                        }
                        if let Some(audio) = &mut validation_audio {
                            audio.update([0xff]);
                            audio.update(&midi[..count]);
                        }
                    }
                }
                let seconds = started.elapsed().as_secs_f64();
                if !validation {
                    samples[variant].push(seconds);
                }
                assert_eq!(session.status(), doslet_runtime::SessionStatus::Running);
                assert_eq!(done, completed, "timed replay work differs");
                assert!(
                    session.save_state().unwrap() == expected_final_state,
                    "timed replay state differs"
                );
                if let Some(audio) = validation_audio {
                    assert_eq!(
                        audio.finalize(),
                        reference_audio.clone().finalize(),
                        "color validation PCM/MIDI differs for {}",
                        variants[variant]
                    );
                    eprintln!(
                        "validation {}: full state and drained PCM/MIDI equal",
                        variants[variant]
                    );
                }
                if let Some(mask) = &color_mask {
                    for (index, &allowed) in mask.iter().enumerate() {
                        if !allowed {
                            assert_eq!(
                                session.wasm_procedure_entry_stats(index).unwrap().calls,
                                0,
                                "excluded color entry executed in {}",
                                variants[variant]
                            );
                        }
                    }
                }
                let timing_stats = session.wasm_procedure_stats();
                eprintln!(
                    "{phase} round {} {}: {seconds:.6} s, scheduling steps={done}, AOT work={}, calls={}, side exits={}",
                    round + 1,
                    variants[variant],
                    timing_stats.map_or(0, |s| s.translated_instructions),
                    timing_stats.map_or(0, |s| s.calls),
                    timing_stats.map_or(0, |s| s.side_exits)
                );
            }
        }
        for (variant, samples) in variants[..count].iter().zip(&mut samples[..count]) {
            samples.sort_by(f64::total_cmp);
            let seconds = samples[2];
            eprintln!(
                "{variant}: median {seconds:.6} s, range {:.6}..{:.6}, for {guest:.2} guest s ({:.3} guest s per host s)",
                samples[0],
                samples[4],
                guest / seconds
            );
        }
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
