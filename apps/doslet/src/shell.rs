//! One desktop window that shows the game library or a game started from it.
//! A started game owns its emulation thread exactly as a direct launch does;
//! returning to the library drops it after its saves are settled.
use crate::{app, desktop, library};
use doslet_runtime::SessionStatus;
use iced::{Element, Subscription, Task, Theme};
use std::{
    ffi::OsString,
    path::Path,
    sync::{Arc, Mutex},
};

/// A prepared game, moved once from its preparation thread into the window.
#[derive(Clone)]
pub struct Started(Arc<Mutex<Option<Box<desktop::Doslet>>>>);
impl std::fmt::Debug for Started {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Started")
    }
}

#[derive(Clone, Debug)]
pub enum Message {
    Library(library::Message),
    Game(app::Message),
    Started(Result<Started, String>),
    CloseRequested(iced::window::Id),
}

pub struct Shell {
    library: library::Library,
    game: Option<Box<desktop::Doslet>>,
    /// Options given without a game apply to every game started here.
    forwarded: Vec<OsString>,
    /// The program has ended and the shell asked to return once.
    exited: bool,
}
impl Shell {
    fn report_start_error(&mut self, name: &str, profile: &Path, error: &str) {
        eprintln!(
            "[ERROR doslet] Cannot start {name} (profile {}): {error}",
            profile.display()
        );
        self.library.notice = Some(format!("Cannot start {name}: {error}"));
    }

    pub fn new(library: library::Library, forwarded: Vec<OsString>) -> Self {
        Self {
            library,
            game: None,
            forwarded,
            exited: false,
        }
    }

    fn apply_global_to_cli(&self, cli: &mut crate::cli::Cli) {
        let g = &self.library.global;
        let has_flag = |flag: &str| {
            self.forwarded
                .iter()
                .any(|arg| arg.to_string_lossy().starts_with(flag))
        };
        if !g.audio_enabled && !has_flag("--no-audio") {
            cli.no_audio = true;
        }
        if !has_flag("--audio-mixer") {
            cli.audio_mixer = if g.audio_mixer.eq_ignore_ascii_case("system") {
                crate::audio::Mixer::System
            } else {
                crate::audio::Mixer::Application
            };
        }
        // MIDI output and ROMs come from the profile first; `profile.rs`
        // falls back to these global settings.
        cli.global_settings = Some(g.clone());
    }

    /// Prepare the machine off the window thread: loading media, audio and
    /// AOT code can take a moment, during which the library keeps drawing.
    fn start(
        &mut self,
        index: usize,
        action: Option<String>,
        resume_autosave: bool,
    ) -> Task<Message> {
        if self.game.is_some() || self.library.starting.is_some() {
            return Task::none();
        }
        let Some(game) = self.library.games.get(index) else {
            return Task::none();
        };
        let mut arguments: Vec<OsString> = std::iter::once(OsString::from("doslet"))
            .chain(self.forwarded.iter().cloned())
            .chain([OsString::from("--config"), game.profile.clone().into()])
            .collect();
        if let Some(action_id) = &action {
            // Running a secondary entrypoint (e.g. SETUP.EXE) may modify game
            // configuration files in the persistent overlay, so clear any stale
            // memory snapshot before launching the utility.
            let _ = std::fs::remove_file(&game.autosave);
            arguments.push(OsString::from("--action"));
            arguments.push(OsString::from(action_id));
        } else {
            arguments.push(OsString::from("--autosave-path"));
            arguments.push(game.autosave.clone().into());
            if resume_autosave && game.autosave.is_file() {
                arguments.push(OsString::from("--load-state"));
                arguments.push(game.autosave.clone().into());
            }
        }
        let mut cli = match <crate::cli::Cli as clap::Parser>::try_parse_from(arguments) {
            Ok(cli) => cli,
            Err(error) => {
                let name = game.name.clone();
                let profile = game.profile.clone();
                self.report_start_error(&name, &profile, &error.to_string());
                return Task::none();
            }
        };
        self.apply_global_to_cli(&mut cli);
        self.library.starting = Some(index);
        self.library.notice = None;
        self.spawn_cli(cli)
    }

    fn start_demo(&mut self) -> Task<Message> {
        if self.game.is_some() || self.library.starting.is_some() {
            return Task::none();
        }
        let arguments = std::iter::once(OsString::from("doslet"))
            .chain(self.forwarded.iter().cloned())
            .chain([OsString::from("--demo")]);
        let mut cli = match <crate::cli::Cli as clap::Parser>::try_parse_from(arguments) {
            Ok(cli) => cli,
            Err(error) => {
                self.library.notice = Some(format!("Cannot start demo: {error}"));
                return Task::none();
            }
        };
        self.apply_global_to_cli(&mut cli);
        self.library.notice = None;
        self.spawn_cli(cli)
    }

    fn spawn_cli(&mut self, cli: crate::cli::Cli) -> Task<Message> {
        Task::perform(
            async move {
                let (sender, receiver) = iced::futures::channel::oneshot::channel();
                let spawned = std::thread::Builder::new()
                    .name("doslet-start".into())
                    .spawn(move || {
                        let game = desktop::Doslet::from_cli(cli).map_err(|e| e.to_string());
                        let _ = sender.send(game);
                    });
                if let Err(error) = spawned {
                    return Err(error.to_string());
                }
                receiver
                    .await
                    .unwrap_or_else(|_| Err("game preparation stopped".into()))
            },
            |game| Message::Started(game.map(|game| Started(Arc::new(Mutex::new(Some(game)))))),
        )
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Library(library::Message::Launch(index)) => self.start(index, None, true),
            Message::Library(library::Message::Restart(index)) => {
                if let Some(game) = self.library.games.get(index) {
                    let _ = std::fs::remove_file(&game.autosave);
                }
                self.library.refresh_autosaves();
                self.start(index, None, false)
            }
            Message::Library(library::Message::LaunchAction(index, action_id)) => {
                self.start(index, Some(action_id), false)
            }
            Message::Library(library::Message::LaunchDemo) => self.start_demo(),
            Message::Library(message) => self.library.update(message).map(Message::Library),
            Message::Started(result) => {
                let index = self.library.starting.take();
                let game = index
                    .and_then(|index| self.library.games.get(index))
                    .map(|game| (game.name.clone(), game.profile.clone()))
                    .unwrap_or_else(|| ("the game".into(), Path::new("<unknown>").into()));
                match result.map(|slot| slot.0.lock().ok().and_then(|mut slot| slot.take())) {
                    Ok(Some(mut game)) => {
                        game.from_library = true;
                        self.game = Some(game);
                        self.exited = false;
                    }
                    Ok(None) => self.report_start_error(
                        &game.0,
                        &game.1,
                        "game preparation did not return a game",
                    ),
                    Err(error) => self.report_start_error(&game.0, &game.1, &error),
                }
                Task::none()
            }
            // Dropping the frontend stops and joins its emulation thread
            // (which writes autosave.state.gz if the guest is still active).
            Message::Game(app::Message::LibraryReady) => {
                self.game = None;
                self.library.refresh_autosaves();
                // The library has no full screen controls; never leave the
                // window stuck in the mode the game used. Leave full screen
                // only after the library was presented, not in the same frame.
                crate::presentation::after(crate::presentation::LEAVE_FULLSCREEN_DELAY, ())
                    .then(|()| crate::presentation::set_fullscreen(false))
            }
            Message::Game(message @ (app::Message::ReturnToLibrary | app::Message::ShutDown)) => {
                self.leave_game(message)
            }
            Message::Game(message) => {
                let Some(game) = &mut self.game else {
                    return Task::none();
                };
                let task = game.update(message).map(Message::Game);
                // A program that ends returns to the library like the toolbar
                // button, once its last save is committed.
                let session = &game.session;
                let exited = matches!(session.status(), SessionStatus::Exited(_));
                let settled = !session.pending_commit() && !session.storage_waiting();
                if !exited {
                    self.exited = false;
                } else if settled && !self.exited {
                    self.exited = true;
                    return Task::batch([task, self.leave_game(app::Message::ReturnToLibrary)]);
                }
                task
            }
            Message::CloseRequested(id) => iced::window::close(id),
        }
    }

    /// Leaves the game for the library. Full screen only makes sense in a
    /// game, so the window leaves it first, in the same order as the exit
    /// button (bars, then windowed mode), and the library follows after.
    fn leave_game(&mut self, message: app::Message) -> Task<Message> {
        use crate::presentation::{LEAVE_FULLSCREEN_DELAY, SizingMessage, after};
        let Some(game) = &mut self.game else {
            return Task::none();
        };
        let sizing = &game.display_sizing;
        if sizing.fullscreen || sizing.leaving_fullscreen() {
            let exit = if sizing.fullscreen {
                game.update(app::Message::DisplaySizing(SizingMessage::Fullscreen(
                    false,
                )))
                .map(Message::Game)
            } else {
                Task::none()
            };
            // Retried until the exit has settled; then the library follows.
            return Task::batch([
                exit,
                after(LEAVE_FULLSCREEN_DELAY * 3, Message::Game(message)),
            ]);
        }
        game.update(message).map(Message::Game)
    }

    pub fn view(&self) -> Element<'_, Message> {
        match &self.game {
            Some(game) => game.view().map(Message::Game),
            None => self.library.view().map(Message::Library),
        }
    }

    pub fn title(&self) -> String {
        match &self.game {
            Some(game) => format!("{} — Doslet", game.name),
            None => "Library — Doslet".into(),
        }
    }

    pub fn theme(&self) -> Theme {
        crate::ui::theme()
    }

    pub fn subscription(&self) -> Subscription<Message> {
        match &self.game {
            // The game handles close requests itself, including its saves.
            Some(game) => game.subscription().map(Message::Game),
            None => Subscription::batch([
                self.library.subscription().map(Message::Library),
                iced::window::close_requests().map(Message::CloseRequested),
            ]),
        }
    }
}

pub fn run(shell: Shell) -> iced::Result {
    let initial = std::cell::Cell::new(Some(shell));
    let window = iced::window::Settings {
        size: iced::Size::new(1280.0, 820.0),
        min_size: Some(crate::presentation::MIN_WINDOW_SIZE),
        exit_on_close_request: false,
        #[cfg(target_os = "linux")]
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: "org.doslet.Doslet".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    iced::application(
        move || {
            let shell = initial.take().expect("iced boots the window once");
            let art = shell.library.load_art().map(Message::Library);
            (shell, art)
        },
        Shell::update,
        Shell::view,
    )
    .title(Shell::title)
    .theme(Shell::theme)
    .subscription(Shell::subscription)
    .window(window)
    .centered()
    .run()
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::{FutureExt, StreamExt};
    use std::time::{Duration, Instant};

    /// A library window already showing a game built from `program`.
    fn playing(program: &'static [u8]) -> Shell {
        playing_with_autosave(program, None)
    }

    fn playing_with_autosave(
        program: &'static [u8],
        autosave: Option<std::path::PathBuf>,
    ) -> Shell {
        let mut game = desktop::Doslet::with_engine(move || {
            let mut engine = Box::new(app::Doslet::new());
            engine.autosave_path = autosave;
            engine.session = doslet_runtime::Session::new(program).unwrap().into();
            engine.session.start();
            Ok(engine)
        });
        game.from_library = true;
        let mut shell = Shell::new(library::Library::discover(), Vec::new());
        shell.game = Some(game);
        shell
    }

    /// Drive the window as iced would: tick, then feed every task's ready
    /// messages back. Window operations wait for a runtime and are dropped.
    fn run_until(shell: &mut Shell, mut first: Option<Message>, done: impl Fn(&Shell) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        // Tasks still waiting, e.g. for a delayed message, are polled again
        // every round instead of being dropped.
        let mut streams = Vec::new();
        while !done(shell) {
            assert!(Instant::now() < deadline, "the window did not settle");
            let mut queue = vec![first.take().unwrap_or(Message::Game(app::Message::Tick))];
            loop {
                while let Some(message) = queue.pop() {
                    if let Some(stream) = iced_runtime::task::into_stream(shell.update(message)) {
                        streams.push(stream);
                    }
                }
                // Every iced task first yields once to its executor.
                for _ in 0..4 {
                    streams.retain_mut(|stream| {
                        loop {
                            match stream.next().now_or_never() {
                                Some(Some(iced_runtime::Action::Output(message))) => {
                                    queue.push(message)
                                }
                                Some(Some(_)) => {}
                                Some(None) => return false,
                                None => return true,
                            }
                        }
                    });
                }
                if queue.is_empty() {
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn a_program_that_exits_returns_the_window_to_the_library() {
        // MOV AX,4C07h; INT 21h
        let mut shell = playing(&[0xB8, 0x07, 0x4C, 0xCD, 0x21]);
        assert_eq!(
            shell.title(),
            format!("{} — Doslet", shell.game.as_ref().unwrap().name)
        );
        run_until(&mut shell, None, |shell| shell.game.is_none());
        assert_eq!(shell.title(), "Library — Doslet");
    }

    #[test]
    fn the_toolbar_returns_a_running_game_to_the_library() {
        // INC AX; JMP back: runs until the player leaves.
        let mut shell = playing(&[0x40, 0xEB, 0xFD]);
        run_until(&mut shell, None, |shell| {
            shell.game.as_ref().unwrap().session.steps() > 0
        });
        let leave = Message::Game(app::Message::ReturnToLibrary);
        run_until(&mut shell, Some(leave), |shell| shell.game.is_none());
    }

    #[test]
    fn a_full_screen_game_leaves_full_screen_before_the_library_appears() {
        use crate::presentation::SizingMessage;
        let mut shell = playing(&[0x40, 0xEB, 0xFD]);
        run_until(&mut shell, None, |shell| {
            shell.game.as_ref().unwrap().session.steps() > 0
        });
        let native = Message::Game(app::Message::DisplaySizing(SizingMessage::FullscreenStyle(
            crate::presentation::FullscreenStyle::Native,
        )));
        let _ = shell.update(native);
        let enter = Message::Game(app::Message::DisplaySizing(SizingMessage::Fullscreen(true)));
        run_until(&mut shell, Some(enter), |shell| {
            shell.game.as_ref().unwrap().display_sizing.fullscreen
        });
        // The game view itself leaves full screen; the library appears only
        // after the window has settled as windowed.
        let left_in_game = std::cell::Cell::new(false);
        let settled_before_library = std::cell::Cell::new(false);
        run_until(
            &mut shell,
            Some(Message::Game(app::Message::ShutDown)),
            |shell| match &shell.game {
                Some(game) => {
                    let sizing = &game.display_sizing;
                    let leaving = sizing.leaving_fullscreen();
                    left_in_game.set(left_in_game.get() || leaving);
                    settled_before_library.set(!sizing.fullscreen && !leaving);
                    false
                }
                None => true,
            },
        );
        assert!(
            left_in_game.get(),
            "the game view must leave full screen itself"
        );
        assert!(
            settled_before_library.get(),
            "the library appeared while leaving"
        );
    }

    #[test]
    fn back_keeps_a_suspended_state_and_shut_down_discards_it() {
        let dir = std::env::temp_dir().join(format!("doslet-shut-down-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let autosave = dir.join("game.autosave.state.gz");
        for (message, resumable) in [
            (app::Message::ReturnToLibrary, true),
            (app::Message::ShutDown, false),
        ] {
            std::fs::write(&autosave, b"stale").unwrap();
            let mut shell = playing_with_autosave(&[0x40, 0xEB, 0xFD], Some(autosave.clone()));
            run_until(&mut shell, None, |shell| {
                shell.game.as_ref().unwrap().session.steps() > 0
            });
            run_until(&mut shell, Some(Message::Game(message.clone())), |shell| {
                shell.game.is_none()
            });
            let saved = std::fs::read(&autosave).ok();
            if resumable {
                assert!(
                    saved.is_some_and(|bytes| bytes != b"stale"),
                    "{message:?} must replace the suspended state"
                );
            } else {
                assert!(
                    saved.is_none(),
                    "{message:?} must not leave a state to resume"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `cargo test -p doslet --bin doslet game_toolbar_screenshot -- --ignored`
    #[test]
    #[ignore = "requires a wgpu adapter; writes target/library-game.png"]
    fn game_toolbar_screenshot() {
        use iced::{Color, Font, Pixels, Size};
        use iced_runtime::core::{clipboard, mouse, renderer};
        use iced_runtime::user_interface::{Cache, UserInterface};
        let mut shell = playing(&[0x40, 0xEB, 0xFD]);
        run_until(&mut shell, None, |shell| {
            shell.game.as_ref().unwrap().session.steps() > 0
        });
        let mut renderer = iced::futures::executor::block_on(
            <iced::Renderer as renderer::Headless>::new(Font::DEFAULT, Pixels(16.0), Some("wgpu")),
        )
        .expect("a wgpu adapter is required for this opt-in test");
        let size = Size::new(960.0, 640.0);
        let mut ui = UserInterface::build(shell.view(), size, Cache::default(), &mut renderer);
        let cursor = mouse::Cursor::Unavailable;
        ui.update(
            &[],
            cursor,
            &mut renderer,
            &mut clipboard::Null,
            &mut Vec::new(),
        );
        let style = renderer::Style {
            text_color: crate::ui::FOREGROUND,
        };
        ui.draw(&mut renderer, &shell.theme(), &style, cursor);
        let pixels =
            renderer::Headless::screenshot(&mut renderer, Size::new(960, 640), 1.0, Color::BLACK);
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/library-game.png");
        image::RgbaImage::from_raw(960, 640, pixels)
            .unwrap()
            .save(path)
            .unwrap();
    }
}
