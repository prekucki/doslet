//! Computer tools are serviced only by the emulation owner, between batches.
use crate::app::{Doslet, Message};
use doslet_input::{InputAction, InputEvent, InputSourceId, PhysicalKey};
use doslet_mcp::{Action, Reply, Request, Response, Screen, Server, Status, Stroke};
use doslet_runtime::SessionStatus;
use iced::Task;
use std::{
    io,
    net::SocketAddr,
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const SOURCE: InputSourceId = InputSourceId(2);
static GENERATION: AtomicU64 = AtomicU64::new(1);
enum Phase {
    Down,
    Gap,
    Wait,
}
struct Pending {
    reply: Reply,
    epoch: u64,
    began: Instant,
    until: u64,
    phase: Phase,
    strokes: Vec<Stroke>,
    index: usize,
    hold_ms: u16,
}
pub struct Computer {
    server: Server,
    pending: Option<Pending>,
    generation: u64,
    sequence: u64,
    held: [bool; 256],
    held_epoch: Option<u64>,
    bios_active: bool,
}
impl Computer {
    pub fn listen(address: SocketAddr) -> io::Result<Self> {
        let generation = GENERATION
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |g| g.checked_add(1))
            .map_err(|_| io::Error::other("MCP input generation exhausted"))?;
        Ok(Self {
            server: Server::listen(address)?,
            pending: None,
            generation,
            sequence: 0,
            held: [false; 256],
            held_epoch: None,
            bios_active: false,
        })
    }
    pub fn address(&self) -> SocketAddr {
        self.server.address()
    }
    pub fn cancel(&mut self, app: &mut Doslet, reason: &str) {
        self.release(app);
        if let Some(pending) = self.pending.take() {
            let _ = pending.reply.send(Err(reason.into()));
        }
    }
    fn release(&mut self, app: &mut Doslet) {
        if self
            .held_epoch
            .is_some_and(|epoch| epoch != app.session.epoch())
        {
            self.held.fill(false);
            self.bios_active = false;
            app.forget_agent_input();
            return;
        }
        if self.held_epoch == Some(app.session.epoch()) {
            for usage in 0..256 {
                if self.held[usage] {
                    let _ = self.edge(app, usage as u16, false);
                }
            }
        }
        self.held.fill(false);
        for button in 0..3 {
            app.agent_pointer_button(button, false);
        }
        if self.bios_active {
            self.bios_active = false;
            let _ = app.agent_modifiers(0);
        }
    }
    fn edge(&mut self, app: &mut Doslet, usage: u16, pressed: bool) -> Result<(), String> {
        let epoch = app.session.epoch();
        self.sequence = self
            .sequence
            .checked_add(1)
            .ok_or("MCP input sequence exhausted")?;
        app.session
            .route_input(InputEvent {
                session_epoch: epoch,
                source: SOURCE,
                connection_generation: self.generation,
                sequence: self.sequence,
                action: InputAction::Key {
                    key: PhysicalKey(usage),
                    pressed,
                },
            })
            .map_err(|e| e.to_string())?;
        self.held[usage as usize] = pressed;
        Ok(())
    }
    fn press(&mut self, app: &mut Doslet, stroke: Stroke) -> Result<(), String> {
        let epoch = app.session.epoch();
        if self.held_epoch != Some(epoch) {
            self.sequence = 0;
            self.held_epoch = Some(epoch);
        }
        if app.session.hardware_config().keyboard == doslet_config::KeyboardMode::Bios {
            self.bios_active = true;
            app.agent_modifiers(stroke.modifiers)
                .map_err(|e| e.to_string())?;
            if !app
                .session
                .push_key(stroke.scan, stroke.ascii)
                .map_err(|e| e.to_string())?
            {
                return Err("Guest BIOS keyboard buffer is full".into());
            }
            return Ok(());
        }
        app.session
            .connect_input_keyboard(SOURCE, self.generation)
            .map_err(|e| e.to_string())?;
        for (flag, usage) in [(4, 224), (3, 225), (8, 226)] {
            if stroke.modifiers & flag != 0 {
                self.edge(app, usage, true)?;
            }
        }
        self.edge(app, stroke.usage, true)
    }
    pub fn poll(&mut self, app: &mut Doslet) -> Task<Message> {
        self.progress(app);
        let mut effects = Task::none();
        for _ in 0..4 {
            let Some(request) = self.server.try_request() else {
                break;
            };
            if request.reply.is_closed() {
                continue;
            }
            effects = Task::batch([effects, self.handle(app, request)]);
        }
        effects
    }
    fn progress(&mut self, app: &mut Doslet) {
        let Some(mut p) = self.pending.take() else {
            return;
        };
        if p.reply.is_closed()
            || p.epoch != app.session.epoch()
            || p.began.elapsed() > Duration::from_secs(20)
            || app.session.status() != SessionStatus::Running
            || app.configurator.pending
        {
            self.release(app);
            let _ = p.reply.send(Err("Input cancelled: caller disconnected, guest stopped/replaced, or 20-second timeout".into()));
            return;
        }
        if app.session.emulated_nanos() < p.until {
            self.pending = Some(p);
            return;
        }
        match p.phase {
            Phase::Down => {
                self.release(app);
                p.phase = Phase::Gap;
                p.until = app.session.emulated_nanos().saturating_add(30_000_000);
            }
            Phase::Gap if p.index + 1 < p.strokes.len() => {
                p.index += 1;
                if let Err(error) = self.press(app, p.strokes[p.index]) {
                    self.release(app);
                    let _ = p.reply.send(Err(error));
                    return;
                }
                p.phase = Phase::Down;
                p.until = app
                    .session
                    .emulated_nanos()
                    .saturating_add(u64::from(p.hold_ms) * 1_000_000);
            }
            Phase::Gap | Phase::Wait => {
                let _ = p.reply.send(Ok(screen(app)));
                return;
            }
        }
        self.pending = Some(p);
    }
    fn handle(&mut self, app: &mut Doslet, request: Request) -> Task<Message> {
        let Request { action, reply } = request;
        let mut effects = Task::none();
        let result = match action {
            Action::Screenshot => Ok(screen(app)),
            Action::Status => Ok(Response::Status(status(app))),
            Action::CpuMhz { mhz } => {
                if app.gdb_connected() || app.configurator.pending || !(1..=1000).contains(&mhz) {
                    Err(
                        "CPU clock requires 1..1000 MHz and no debugger or pending preparation"
                            .into(),
                    )
                } else {
                    effects = app.update(Message::CpuMhz(mhz));
                    app.presentation_revision = app.presentation_revision.wrapping_add(1);
                    Ok(Response::Status(status(app)))
                }
            }
            Action::Pause | Action::Resume => {
                if app.gdb_connected() {
                    Err("GDB controls execution; use GDB to pause/resume".into())
                } else if app.configurator.pending {
                    Err("Machine preparation is pending".into())
                } else {
                    let pause = matches!(action, Action::Pause);
                    if pause {
                        self.cancel(app, "Paused through MCP");
                    }
                    let state = app.session.status();
                    if (pause && state == SessionStatus::Running)
                        || (!pause && matches!(state, SessionStatus::Ready | SessionStatus::Paused))
                    {
                        effects = app.update(Message::RunPause);
                        Ok(Response::Status(status(app)))
                    } else if pause || state == SessionStatus::Running {
                        Ok(Response::Status(status(app)))
                    } else {
                        Err("This session cannot resume; use desktop Reset".into())
                    }
                }
            }
            action => {
                if app.session.status() != SessionStatus::Running || app.configurator.pending {
                    Err("Guest must be running; use GDB to resume if connected".into())
                } else if self.pending.is_some() {
                    Err("Another input/wait gesture is still active".into())
                } else {
                    let result = match action {
                        Action::Move { x, y } => move_mouse(app, x, y),
                        Action::Click { x, y, .. } => move_mouse(app, x, y),
                        _ => Ok(()),
                    };
                    if let Err(error) = result {
                        Err(error)
                    } else {
                        let (strokes, button, hold_ms, waiting) = match action {
                            Action::Move { .. } => {
                                let _ = reply.send(Ok(screen(app)));
                                return effects;
                            }
                            Action::Click {
                                button, hold_ms, ..
                            } => (Vec::new(), Some(button), hold_ms, false),
                            Action::Keys { strokes, hold_ms } => (strokes, None, hold_ms, false),
                            Action::Wait { milliseconds } => (Vec::new(), None, milliseconds, true),
                            _ => unreachable!(),
                        };
                        let result = if let Some(button) = button {
                            self.held_epoch = Some(app.session.epoch());
                            app.agent_pointer_button(button, true);
                            Ok(())
                        } else if let Some(stroke) = strokes.first() {
                            self.press(app, *stroke)
                        } else {
                            Ok(())
                        };
                        if let Err(error) = result {
                            self.release(app);
                            Err(error)
                        } else {
                            self.pending = Some(Pending {
                                reply,
                                epoch: app.session.epoch(),
                                began: Instant::now(),
                                until: app
                                    .session
                                    .emulated_nanos()
                                    .saturating_add(u64::from(hold_ms) * 1_000_000),
                                phase: if waiting { Phase::Wait } else { Phase::Down },
                                strokes,
                                index: 0,
                                hold_ms,
                            });
                            return effects;
                        }
                    }
                }
            }
        };
        let _ = reply.send(result);
        effects
    }
}
fn status(app: &Doslet) -> Status {
    let frame = app.session.presentation_frame();
    Status {
        name: app.name.clone(),
        status: format!("{:?}", app.session.status()),
        steps: app.session.steps(),
        emulated_nanos: app.session.emulated_nanos(),
        cs: app.session.registers().cs,
        eip: app.session.registers().ip,
        linear_pc: app.session.debug_pc(),
        width: frame.width() as u32,
        height: frame.height() as u32,
        gdb_connected: app.gdb_connected(),
        cpu_mhz: app.session.hardware_config().cpu_mhz,
    }
}
fn screen(app: &Doslet) -> Response {
    let status = status(app);
    let mut rgba = vec![0; status.width as usize * status.height as usize * 4];
    app.session
        .presentation_frame()
        .write_rgba(&mut rgba)
        .expect("native frame dimensions");
    Response::Screen(Screen { status, rgba })
}
fn move_mouse(app: &mut Doslet, x: u32, y: u32) -> Result<(), String> {
    let frame = app.session.presentation_frame();
    let (width, height) = (frame.width() as u32, frame.height() as u32);
    if x >= width || y >= height {
        return Err(format!("Coordinates outside {width}x{height} guest image"));
    }
    let (pw, ph) = app.session.pointer_dimensions();
    let px = x * u32::from(pw.saturating_sub(1)) / width.saturating_sub(1).max(1);
    let py = y * u32::from(ph.saturating_sub(1)) / height.saturating_sub(1).max(1);
    app.session.pointer_move(px as u16, py as u16);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use doslet_input::GuestEvent;
    fn hardware_app() -> Doslet {
        let mut app = Doslet::new();
        app.session = doslet_runtime::Session::new_configured(
            &[0xEB, 0xFE],
            doslet_config::HardwareConfig {
                keyboard: doslet_config::KeyboardMode::At,
                ..Default::default()
            },
        )
        .unwrap()
        .into();
        while app.session.input_router_mut().next_event().is_some() {}
        app
    }
    #[test]
    fn repeated_hardware_gestures_keep_sequence_and_preserve_human_modifiers() {
        let mut app = hardware_app();
        let mut computer = Computer::listen("127.0.0.1:0".parse().unwrap()).unwrap();
        app.session.physical_key(225, true).unwrap();
        assert_eq!(
            app.session.input_router_mut().next_event(),
            Some(GuestEvent::Key {
                key: PhysicalKey(225),
                pressed: true
            })
        );
        for _ in 0..2 {
            computer
                .press(
                    &mut app,
                    doslet_mcp::key_stroke("a", &["shift".into()]).unwrap(),
                )
                .unwrap();
            assert_eq!(
                app.session.input_router_mut().next_event(),
                Some(GuestEvent::Key {
                    key: PhysicalKey(4),
                    pressed: true
                })
            );
            computer.release(&mut app);
            assert_eq!(
                app.session.input_router_mut().next_event(),
                Some(GuestEvent::Key {
                    key: PhysicalKey(4),
                    pressed: false
                })
            );
            assert_eq!(app.session.input_router_mut().next_event(), None);
        }
        app.session.release_input_source(InputSourceId(0)).unwrap();
        assert_eq!(
            app.session.input_router_mut().next_event(),
            Some(GuestEvent::Key {
                key: PhysicalKey(225),
                pressed: false
            })
        );
    }
    #[test]
    fn cancelled_gesture_releases_only_agent_and_stale_epoch_never_reaches_replacement() {
        let mut app = hardware_app();
        app.session.start();
        let mut computer = Computer::listen("127.0.0.1:0".parse().unwrap()).unwrap();
        let (reply, received) = tokio_reply();
        let _ = computer.handle(
            &mut app,
            Request {
                action: Action::Keys {
                    strokes: doslet_mcp::text_strokes("a").unwrap(),
                    hold_ms: 80,
                },
                reply,
            },
        );
        assert!(computer.pending.is_some());
        while app.session.input_router_mut().next_event().is_some() {}
        drop(received);
        computer.progress(&mut app);
        assert!(computer.pending.is_none());
        assert_eq!(
            app.session.input_router_mut().next_event(),
            Some(GuestEvent::Key {
                key: PhysicalKey(4),
                pressed: false
            })
        );
        let (reply, mut received) = tokio_reply();
        let _ = computer.handle(
            &mut app,
            Request {
                action: Action::Keys {
                    strokes: doslet_mcp::text_strokes("b").unwrap(),
                    hold_ms: 80,
                },
                reply,
            },
        );
        app.session = doslet_runtime::Session::new(&[0xEB, 0xFE]).unwrap().into();
        while app.session.input_router_mut().next_event().is_some() {}
        computer.progress(&mut app);
        assert!(received.try_recv().unwrap().is_err());
        assert_eq!(app.session.input_router_mut().next_event(), None);
        assert_eq!(app.session.steps(), 0);
    }
    // Keep the transport channel type behind the public request API.
    fn tokio_reply() -> (
        Reply,
        tokio::sync::oneshot::Receiver<Result<Response, String>>,
    ) {
        tokio::sync::oneshot::channel()
    }
}
