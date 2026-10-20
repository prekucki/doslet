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

    /// Prepare the machine off the window thread: loading media, audio and
    /// AOT code can take a moment, during which the library keeps drawing.
    fn start(&mut self, index: usize) -> Task<Message> {
        if self.game.is_some() || self.library.starting.is_some() {
            return Task::none();
        }
        let Some(game) = self.library.games.get(index) else {
            return Task::none();
        };
        let arguments = std::iter::once(OsString::from("doslet"))
            .chain(self.forwarded.iter().cloned())
            .chain([OsString::from("--config"), game.profile.clone().into()]);
        let cli = match <crate::cli::Cli as clap::Parser>::try_parse_from(arguments) {
            Ok(cli) => cli,
            Err(error) => {
                let name = game.name.clone();
                let profile = game.profile.clone();
                self.report_start_error(&name, &profile, &error.to_string());
                return Task::none();
            }
        };
        self.library.starting = Some(index);
        self.library.notice = None;
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
            Message::Library(library::Message::Launch(index)) => self.start(index),
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
            // Dropping the frontend stops and joins its emulation thread.
            Message::Game(app::Message::LibraryReady) => {
                self.game = None;
                Task::none()
            }
            Message::Game(message) => {
                let Some(game) = &mut self.game else {
                    return Task::none();
                };
                let task = game.update(message).map(Message::Game);
                // A program that ends returns to the library like the toolbar
                // button, once its last save is committed. A refusal there
                // (unsaved temporary changes) leaves it on screen with the reason.
                let session = &game.session;
                let exited = matches!(session.status(), SessionStatus::Exited(_));
                let settled = !session.pending_commit() && !session.storage_waiting();
                if !exited {
                    self.exited = false;
                } else if settled && !self.exited {
                    self.exited = true;
                    let leave = game.update(app::Message::ReturnToLibrary);
                    return Task::batch([task, leave.map(Message::Game)]);
                }
                task
            }
            Message::CloseRequested(id) => iced::window::close(id),
        }
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
        let mut game = desktop::Doslet::with_engine(move || {
            let mut engine = Box::new(app::Doslet::new());
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
        while !done(shell) {
            assert!(Instant::now() < deadline, "the window did not settle");
            let mut queue = vec![first.take().unwrap_or(Message::Game(app::Message::Tick))];
            while let Some(message) = queue.pop() {
                let task = shell.update(message);
                let Some(mut stream) = iced_runtime::task::into_stream(task) else {
                    continue;
                };
                // Every iced task first yields once to its executor.
                let mut pending = 0;
                while pending < 4 {
                    match stream.next().now_or_never() {
                        Some(Some(iced_runtime::Action::Output(message))) => queue.push(message),
                        Some(Some(_)) => {}
                        Some(None) => break,
                        None => pending += 1,
                    }
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
