//! Minimal iced window for isolating compositor crashes on leaving full screen.
//!
//! `cargo run -p doslet --example fullscreen_probe -- [flags]`
//!
//! Without flags it only toggles full screen with a button, as the Display
//! panel did before the full screen strip. Each flag adds one part of what
//! Doslet now does, so a crash can be narrowed to a single change:
//!
//! - `--swap`        hide the toolbar/footer and show a thin strip in full screen
//! - `--hide-cursor` hide the host cursor over the picture (as during play)
//! - `--query`       read the window mode before each request (`set_fullscreen`)
//! - `--image`       draw a scaled 320×200 image, like the game picture
//! - `--animate`     present a new picture every 16 ms, as a running game does
//! - `--tooltip`     wrap the toggle in a tooltip, like the toolbar icons
//! - `--capture`     clicking the picture captures the mouse (Esc releases)
//! - `--auto`        enter full screen after 2 s and leave it 3 s later, no clicks
//!
//! Every window event and request is printed with a timestamp to stderr.
//! `WGPU_POWER_PREF=low` selects the integrated GPU instead of the discrete one.
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(not(target_arch = "wasm32"))]
fn main() -> iced::Result {
    probe::run()
}

#[cfg(not(target_arch = "wasm32"))]
mod probe {
    use iced::widget::{button, column, container, image, mouse_area, row, space, text, tooltip};
    use iced::window::{self, Mode};
    use iced::{Element, Fill, Subscription, Task, mouse};
    use std::time::{Duration, Instant};

    #[derive(Clone, Copy, Default)]
    struct Flags {
        swap: bool,
        hide_cursor: bool,
        query: bool,
        image: bool,
        animate: bool,
        tooltip: bool,
        capture: bool,
        auto: bool,
    }

    struct Probe {
        flags: Flags,
        fullscreen: bool,
        started: Instant,
        picture: image::Handle,
        frame: u32,
    }

    #[derive(Clone, Debug)]
    enum Message {
        Fullscreen(bool),
        Window(window::Event),
        AutoEnter,
        AutoLeave,
        Frame,
        Capture,
        Captured(String),
    }

    pub fn run() -> iced::Result {
        let mut flags = Flags::default();
        for arg in std::env::args().skip(1) {
            match arg.as_str() {
                "--swap" => flags.swap = true,
                "--hide-cursor" => flags.hide_cursor = true,
                "--query" => flags.query = true,
                "--image" => flags.image = true,
                "--animate" => flags.animate = true,
                "--tooltip" => flags.tooltip = true,
                "--capture" => flags.capture = true,
                "--auto" => flags.auto = true,
                other => {
                    eprintln!("unknown flag {other}");
                    std::process::exit(2);
                }
            }
        }
        iced::application(
            move || {
                let probe = Probe {
                    flags,
                    fullscreen: false,
                    started: Instant::now(),
                    picture: picture(0),
                    frame: 0,
                };
                probe.log(format_args!(
                    "start swap={} hide_cursor={} query={} image={} animate={} tooltip={} \
                     capture={} auto={}",
                    flags.swap,
                    flags.hide_cursor,
                    flags.query,
                    flags.image,
                    flags.animate,
                    flags.tooltip,
                    flags.capture,
                    flags.auto
                ));
                let task = if flags.auto {
                    delayed(Duration::from_secs(2), Message::AutoEnter)
                } else {
                    Task::none()
                };
                (probe, task)
            },
            Probe::update,
            Probe::view,
        )
        .title("Doslet full screen probe")
        .subscription(Probe::subscription)
        .window(window::Settings {
            size: iced::Size::new(960.0, 640.0),
            ..Default::default()
        })
        .run()
    }

    /// A fresh handle per frame, like Doslet's presentation snapshots.
    fn picture(frame: u32) -> image::Handle {
        let mut pixels = Vec::with_capacity(320 * 200 * 4);
        for y in 0..200u32 {
            for x in 0..320u32 {
                pixels.extend_from_slice(&[
                    ((x + frame) * 255 / 319) as u8,
                    (y * 255 / 199) as u8,
                    96,
                    255,
                ]);
            }
        }
        image::Handle::from_rgba(320, 200, pixels)
    }

    fn delayed(after: Duration, message: Message) -> Task<Message> {
        Task::future(async move {
            // Runs on iced's executor thread pool; a blocking sleep is fine here.
            std::thread::sleep(after);
            message
        })
    }

    impl Probe {
        fn log(&self, what: std::fmt::Arguments<'_>) {
            eprintln!("[{:>8.3}s] {what}", self.started.elapsed().as_secs_f64());
        }

        fn request(&mut self, on: bool) -> Task<Message> {
            self.log(format_args!(
                "request {}",
                if on { "Fullscreen" } else { "Windowed" }
            ));
            self.fullscreen = on;
            let target = if on { Mode::Fullscreen } else { Mode::Windowed };
            if self.flags.query {
                window::latest().and_then(move |id| {
                    window::mode(id).then(move |current| {
                        eprintln!("          current mode {current:?}");
                        if (current == Mode::Fullscreen) == on {
                            Task::none()
                        } else {
                            window::set_mode(id, target)
                        }
                    })
                })
            } else {
                window::latest().and_then(move |id| window::set_mode(id, target))
            }
        }

        fn update(&mut self, message: Message) -> Task<Message> {
            match message {
                Message::Fullscreen(on) => self.request(on),
                Message::AutoEnter => {
                    let task = self.request(true);
                    Task::batch([task, delayed(Duration::from_secs(3), Message::AutoLeave)])
                }
                Message::AutoLeave => self.request(false),
                Message::Frame => {
                    self.frame = (self.frame + 1) % 320;
                    self.picture = picture(self.frame);
                    Task::none()
                }
                Message::Capture => {
                    self.log(format_args!("capture mouse"));
                    window::latest().and_then(|id| {
                        iced_runtime::window::capture_mouse(id)
                            .map(|event| Message::Captured(format!("{event:?}")))
                    })
                }
                Message::Captured(event) => {
                    // Motion arrives continuously; log only state changes.
                    if !event.contains("Motion") {
                        self.log(format_args!("capture {event}"));
                    }
                    Task::none()
                }
                Message::Window(event) => {
                    self.log(format_args!("window {event:?}"));
                    Task::none()
                }
            }
        }

        fn subscription(&self) -> Subscription<Message> {
            let events = window::events().map(|(_, event)| Message::Window(event));
            if self.flags.animate {
                Subscription::batch([
                    events,
                    iced::time::every(Duration::from_millis(16)).map(|_| Message::Frame),
                ])
            } else {
                events
            }
        }

        fn view(&self) -> Element<'_, Message> {
            let toggle = button(text(if self.fullscreen {
                "Exit full screen"
            } else {
                "Full screen"
            }))
            .on_press(Message::Fullscreen(!self.fullscreen));
            let toggle: Element<'_, Message> = if self.flags.tooltip {
                tooltip(
                    toggle,
                    container(text("Toggle full screen").size(12)).padding([6, 10]),
                    tooltip::Position::Bottom,
                )
                .into()
            } else {
                toggle.into()
            };
            let picture: Element<'_, Message> = if self.flags.image {
                image(self.picture.clone())
                    .filter_method(image::FilterMethod::Nearest)
                    .content_fit(iced::ContentFit::Contain)
                    .width(Fill)
                    .height(Fill)
                    .into()
            } else {
                container(text("picture")).center(Fill).into()
            };
            let mut picture = mouse_area(picture).interaction(if self.flags.hide_cursor {
                mouse::Interaction::Hidden
            } else {
                mouse::Interaction::None
            });
            if self.flags.capture {
                picture = picture.on_press(Message::Capture);
            }
            if self.flags.swap && self.fullscreen {
                let strip = row![space::horizontal(), toggle].padding([0, 6]);
                column![strip, picture].into()
            } else {
                let toolbar = row![text("toolbar"), space::horizontal(), toggle]
                    .padding([10, 16])
                    .align_y(iced::Center);
                let footer = container(text("footer").size(12)).padding([5, 16]);
                column![toolbar, picture, footer].into()
            }
        }
    }
}
