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
const POINTER_SETTLE_MS: u16 = 250;
static GENERATION: AtomicU64 = AtomicU64::new(1);
enum Phase {
    Position(u8),
    PointerDown,
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
    closing: bool,
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
            closing: false,
        })
    }
    pub fn address(&self) -> SocketAddr {
        self.server.address()
    }
    pub fn is_closing(&self) -> bool {
        self.closing
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
            Phase::Position(button) => {
                self.held_epoch = Some(app.session.epoch());
                app.agent_pointer_button(button, true);
                p.phase = Phase::PointerDown;
                p.until = app
                    .session
                    .emulated_nanos()
                    .saturating_add(u64::from(p.hold_ms) * 1_000_000);
            }
            Phase::PointerDown => {
                self.release(app);
                p.phase = Phase::Wait;
                p.until = app
                    .session
                    .emulated_nanos()
                    .saturating_add(u64::from(POINTER_SETTLE_MS) * 1_000_000);
            }
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
        if self.closing {
            let _ = reply.send(Err("Doslet is closing".into()));
            return Task::none();
        }
        let mut effects = Task::none();
        let result = match action {
            Action::CodeTags {
                start,
                end,
                bits,
                tier,
            } => app
                .code_tags(start, end, bits, tier)
                .map(Response::CodeTags),
            Action::Exit => {
                self.cancel(app, "Doslet is closing");
                self.closing = true;
                effects = app.update(Message::ExitRequested);
                Ok(Response::Exiting)
            }
            Action::Screenshot => Ok(screen(app)),
            Action::Status => Ok(Response::Status(status(app))),
            Action::Synchronization { enabled, adaptive } => {
                if app.gdb_connected() || app.configurator.pending || (adaptive && !enabled) {
                    Err("Synchronization mode requires no debugger/preparation; adaptive requires enabled".into())
                } else {
                    effects = app.update(Message::SynchronizationMode { enabled, adaptive });
                    app.presentation_revision = app.presentation_revision.wrapping_add(1);
                    Ok(Response::Status(status(app)))
                }
            }
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
                        Action::MoveRelative { dx, dy } => {
                            app.session.pointer_relative(f64::from(dx), f64::from(dy));
                            Ok(())
                        }
                        Action::Click {
                            position: Some((x, y)),
                            ..
                        } => move_mouse(app, x, y),
                        _ => Ok(()),
                    };
                    if let Err(error) = result {
                        Err(error)
                    } else {
                        let (strokes, button, hold_ms, phase, delay_ms) = match action {
                            Action::Move { .. } | Action::MoveRelative { .. } => (
                                Vec::new(),
                                None,
                                POINTER_SETTLE_MS,
                                Phase::Wait,
                                POINTER_SETTLE_MS,
                            ),
                            Action::Click {
                                position,
                                button,
                                hold_ms,
                            } => {
                                if position.is_some() {
                                    (
                                        Vec::new(),
                                        None,
                                        hold_ms,
                                        Phase::Position(button),
                                        POINTER_SETTLE_MS,
                                    )
                                } else {
                                    (
                                        Vec::new(),
                                        Some(button),
                                        hold_ms,
                                        Phase::PointerDown,
                                        hold_ms,
                                    )
                                }
                            }
                            Action::Keys { strokes, hold_ms } => {
                                (strokes, None, hold_ms, Phase::Down, hold_ms)
                            }
                            Action::Wait { milliseconds } => {
                                (Vec::new(), None, milliseconds, Phase::Wait, milliseconds)
                            }
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
                                    .saturating_add(u64::from(delay_ms) * 1_000_000),
                                phase,
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
    let guest_audio = app.session.audio_status();
    let audio = guest_audio.dsp;
    let audio_deadline_observed = audio.block_irq_deadlines != 0;
    Status {
        speed_tiers: app.speed_tier_stats().map(|s| {
            serde_json::json!({
                "enabled":s.enabled,"hot_mhz":s.hot_mhz,"cold_mhz":s.cold_mhz,"grey_mhz":s.grey_mhz,
                "guest_nanos":s.nanos,"execution_nanos":s.execution_nanos,
                "steps":s.steps,
                "persistence":{"restored_labels":s.restored_labels,"saved_updates":s.saved_updates,"pending":s.save_pending},
                "learning": {"enabled":s.learning, "running":s.learning_running, "pending_snapshots":s.learning_pending,
                    "jobs":s.learning_jobs, "changes":s.learning_changes,
                    "grey_executed_percent":s.learning_grey_percent, "capture_micros":s.learning_capture_micros},
                "order":["hot","cold","grey","irq"], "irq_mhz":s.grey_mhz
            })
        }),
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
        mouse_driver_x: app.session.pointer_position().0,
        mouse_driver_y: app.session.pointer_position().1,
        mouse_buttons: app.session.pointer_buttons(),
        mouse_relative: app.session.pointer_relative_in_use(),
        mouse_captured: app.has_mouse_capture(),
        menu_idle_nanos: app.session.menu_idle_stats().nanos,
        adaptive_idle_interval_nanos: app.session.menu_idle_stats().interval_nanos,
        adaptive_idle_head: app.session.menu_idle_stats().last_head,
        idle_observer_enabled: app.session.idle_observation().enabled,
        synchronization: format!("{:?}", app.session.synchronization_stats()),
        synchronization_analysis: analysis_status(
            app.session.synchronization_analysis(),
            app.session.synchronization_caller_effects(),
        ),
        host_audio_underruns: app.audio_underruns(),
        guest_fm_started: guest_audio.opl_started,
        guest_fm_key_on_writes: guest_audio.opl_key_on_writes,
        guest_fm_peak: guest_audio.opl_peak,
        guest_fm_levels: guest_audio.fm_levels,
        host_process_id: std::process::id(),
        host_process_cpu_ticks: process_cpu_ticks(),
        guest_audio_service_deadline_misses: audio_deadline_observed
            .then_some(audio.block_irq_deadline_misses),
        guest_audio_service_deadline_checks: audio.block_irq_deadlines,
        guest_audio_service_deadline_source: audio_deadline_observed
            .then_some("SB16 DMA block IRQ acknowledgement before next block"),
        learned_idle_enabled: app.session.adaptive_idle_enabled(),
        idle_observer_candidate: app.session.idle_observation().candidate,
        idle_observer_reason: app.session.idle_observation().reason.to_owned(),
        idle_observer_samples: app.session.idle_observation().samples,
        idle_observer_repeated: app.session.idle_observation().repeated,
        idle_observer_cs: app.session.idle_observation().cs,
        idle_observer_ip: app.session.idle_observation().ip,
        idle_observer_queries: app.session.idle_observation().queries,
        idle_observer_writes: app.session.idle_observation().writes,
        idle_observer_device_accesses: app.session.idle_observation().device_accesses,
        adaptive_idle_input_wakes: app.session.menu_idle_stats().input_wakes,
        adaptive_idle_work_wakes: app.session.menu_idle_stats().work_wakes,
        adaptive_idle_interrupt_wakes: app.session.menu_idle_stats().interrupt_wakes,
    }
}
/// Host-only diagnostic read on MCP requests, never in guest execution.
pub(crate) fn process_cpu_ticks() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        parse_process_cpu_ticks(&std::fs::read_to_string("/proc/self/stat").ok()?)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
#[cfg(any(target_os = "linux", test))]
fn parse_process_cpu_ticks(stat: &str) -> Option<u64> {
    // comm is parenthesized and can itself contain spaces and parentheses.
    // The remaining fields begin at state (3); utime/stime are fields 14/15.
    let (_, fields) = stat.rsplit_once(") ")?;
    let mut fields = fields.split_whitespace();
    let user = fields.nth(11)?.parse::<u64>().ok()?;
    user.checked_add(fields.next()?.parse::<u64>().ok()?)
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

fn analysis_status(
    s: doslet_runtime::session::AnalyzerStats,
    effects: &[Option<doslet_runtime::session::CallerEffect>],
) -> serde_json::Value {
    let c = s.caller;
    let r = s.recurrence;
    let recurrence = serde_json::json!({
        "backedge":r.backedge,"probe_rejections":r.probe_rejections,
        "last_probe_rejection_reason":r.last_probe_rejection_reason,
        "head":r.head,"caller":r.caller,"intervals":r.intervals,"unchanged":r.unchanged,
        "rejected":r.rejected,"cancelled":r.cancelled,
        "measured_wait_nanos":r.measured_wait_nanos,"last_wait_nanos":r.last_wait_nanos,
        "services":r.services,
    });
    let effects: Vec<_> = effects
        .iter()
        .flatten()
        .map(|e| {
            serde_json::json!({
                "pc":e.pc,"kind":format!("{:?}",e.kind),"irq":e.irq,"device":e.device,
                "first_address":e.first_address,"last_address":e.last_address,"width":e.width,
                "last_value":e.last_value,"count":e.count,
            })
        })
        .collect();
    let caller = serde_json::json!({
        "head":c.head,"caller":c.caller,"attempts":c.attempts,"returns":c.returns,
        "complete":c.complete,"exhausted":c.exhausted,"steps":c.steps,"irq_steps":c.irq_steps,
        "service_gaps":c.service_gaps,"device_gaps":c.device_gaps,"dropped_effects":c.dropped_effects,
        "last_steps":c.last_steps,"last_nanos":c.last_nanos,"last_complete":c.last_complete,
        "last_cpu_unchanged":c.last_cpu_unchanged,"last_input_unchanged":c.last_input_unchanged,
        "last_cmos_unchanged":c.last_cmos_unchanged,
        "last_ram_changed_bytes":c.last_ram_changed_bytes,
        "last_ram_changes":c.last_ram_changes,
        "reason":c.reason,"effects":effects,
    });
    let guards = s.guards.map(|g| serde_json::json!({
        "head":g.head,"operand":g.operand,"caller":g.caller,"caller_backedges":g.caller_backedges,
        "reads":g.reads,"last_value":g.last_value,"writes":g.writes,"partial_writes":g.partial_writes,
        "writer_pc":g.writer_pc,"writer_irq":g.writer_irq,"writer_value":g.writer_value,"writer_nanos":g.writer_nanos,
        "observed_decrements":g.observed_decrements,"observed_interval_nanos":g.observed_interval_nanos,"reason":g.reason,
    }));
    let candidates = s.candidates.map(|c| {
        serde_json::json!({
            "head":c.head,
        "boundary_accounting":c.boundary_accounting,
            "branch":c.branch,
            "operand":c.operand,
            "width":c.width,
            "port":c.port,
            "state":c.state,
            "reason":c.reason,
            "writer_pc":c.writer_pc,
            "writer_kind":c.writer_kind,
            "writer_irq":c.writer_irq,
            "writer_address":c.writer_address,
            "writer_width":c.writer_width,
            "writer_value":c.writer_value,
            "writer_nanos":c.writer_nanos,
            "writes":c.writes,
            "partial_writes":c.partial_writes,
            "source_transitions":c.source_transitions,
            "source_period_nanos":c.source_period_nanos,
            "episodes":c.episodes,
            "work_nanos":c.work_nanos,
            "wait_nanos":c.wait_nanos,
            "work_max_nanos":c.work_max_nanos,
            "period_nanos":c.period_nanos,
            "missed_deadlines":c.missed_deadlines,
            "work_histogram":c.work_histogram,
            "last_episode_nanos":c.last_episode_nanos,
            "measured_mhz":c.measured_mhz,
            "predicted_mhz":c.predicted_mhz,
            "reference_period_nanos":c.reference_period_nanos,"cadence_floor_mhz":c.cadence_floor_mhz,
            "reference_period_max_nanos":c.reference_period_max_nanos,
            "cadence_mean_nanos":c.cadence_mean_nanos,
            "variable_cadence":c.variable_cadence,
        })
    });
    serde_json::json!({
        "enabled":s.enabled,
        "controller_enabled":s.controller_enabled,
        "configured_mhz":s.configured_mhz,
        "effective_mhz":s.effective_mhz,
        "reductions":s.reductions,"recoveries":s.recoveries,
        "last_recovery_reason":s.last_recovery_reason,
        "audio_irq_deadlines":s.audio_irq_deadlines,
        "audio_irq_deadline_misses":s.audio_irq_deadline_misses,
        "reason":s.reason,
        "graph_reason":s.graph_reason,
        "graph_nodes":s.graph_nodes,
        "backedges":s.backedges,
        "unknown_effects":s.unknown_effects,
        "observed_steps":s.observed_steps,
        "proven_idle_nanos":s.proven_idle_nanos,
        "guards":guards,
        "caller":caller,
        "recurrence":recurrence,
        "guard_irq_steps":s.guard_irq_steps,
        "guard_interrupted_traces":s.guard_interrupted_traces,
        "gaps":s.gaps,
        "last_gap_reason":s.last_gap_reason,
        "discovery_windows":s.discovery_windows,
        "extended_windows":s.extended_windows,
        "invalidations":s.invalidations,
        "dropped_candidates":s.dropped_candidates,
        "unobserved_service_writes":s.unobserved_service_writes,
        "unobserved_service_device_accesses":s.unobserved_service_device_accesses,
        "unobserved_device_writes":s.unobserved_device_writes,
        "candidates":candidates
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use doslet_input::GuestEvent;
    #[test]
    fn process_cpu_ticks_handles_command_names_and_rejects_incomplete_counters() {
        let line = "123 (a ) tricky name) R 4 5 6 7 8 9 10 11 12 13 140 150 999 999";
        assert_eq!(parse_process_cpu_ticks(line), Some(290));
        assert_eq!(parse_process_cpu_ticks("123 (x) R 4 5"), None);
        assert_eq!(
            parse_process_cpu_ticks(&line.replace("140", "18446744073709551615")),
            None
        );
        assert_eq!(
            parse_process_cpu_ticks(&line.replace("150", "unknown")),
            None
        );
        #[cfg(target_os = "linux")]
        assert!(process_cpu_ticks().is_some());
    }
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
    #[test]
    fn exit_cancels_a_held_button_and_rejects_further_control() {
        let mut app = hardware_app();
        app.session.start();
        let mut computer = Computer::listen("127.0.0.1:0".parse().unwrap()).unwrap();
        let (reply, mut gesture) = tokio_reply();
        let _ = computer.handle(
            &mut app,
            Request {
                action: Action::Click {
                    position: None,
                    button: 0,
                    hold_ms: 1000,
                },
                reply,
            },
        );
        assert_eq!(app.session.pointer_buttons(), 1);
        let (reply, mut exit) = tokio_reply();
        let effect = computer.handle(
            &mut app,
            Request {
                action: Action::Exit,
                reply,
            },
        );
        assert!(matches!(exit.try_recv().unwrap(), Ok(Response::Exiting)));
        assert!(gesture.try_recv().unwrap().is_err());
        assert!(computer.pending.is_none());
        assert_eq!(app.session.pointer_buttons(), 0);
        assert_eq!(app.session.status(), SessionStatus::Paused);
        assert!(effect.units() > 0);
        let (reply, mut resume) = tokio_reply();
        let _ = computer.handle(
            &mut app,
            Request {
                action: Action::Resume,
                reply,
            },
        );
        assert!(resume.try_recv().unwrap().is_err());
        assert_eq!(app.session.status(), SessionStatus::Paused);
    }

    fn finish_phase(computer: &mut Computer, app: &mut Doslet) {
        let until = computer.pending.as_ref().unwrap().until;
        while app.session.emulated_nanos() < until {
            assert_ne!(app.session.advance(1000), 0);
        }
        computer.progress(app);
    }
    #[test]
    fn relative_motion_reaches_guest_at_driver_edge_and_click_does_not_reposition() {
        let mut app = hardware_app();
        // A guest-owned cursor accumulates raw INT 33h/000Bh counts in RAM.
        app.session = doslet_runtime::Session::new(&[
            0xb8, 0x0b, 0, 0xcd, 0x33, 0x01, 0x0e, 0, 2, 0x01, 0x16, 2, 2, 0xeb, 0xf1,
        ])
        .unwrap()
        .into();
        app.session.set_cpu_mhz(1).unwrap();
        app.session.pointer_move(639, 199);
        app.session.start();
        app.session.advance(100);
        let address = u32::from(app.session.registers().ds) * 16 + 0x200;
        assert!(app.session.debug_write_memory(address, &[0; 4]));
        let mut computer = Computer::listen("127.0.0.1:0".parse().unwrap()).unwrap();
        let (reply, mut response) = tokio_reply();
        let _ = computer.handle(
            &mut app,
            Request {
                action: Action::MoveRelative { dx: 20, dy: -10 },
                reply,
            },
        );
        assert!(
            response.try_recv().is_err(),
            "must not return a pre-movement frame"
        );
        finish_phase(&mut computer, &mut app);
        assert!(response.try_recv().unwrap().is_ok());
        let mut motion = [0; 4];
        assert!(app.session.debug_read_memory(address, &mut motion));
        assert_eq!(motion, [20, 0, 246, 255]);
        assert_eq!(app.session.pointer_position(), (639, 189));
        let (reply, mut response) = tokio_reply();
        let _ = computer.handle(
            &mut app,
            Request {
                action: Action::Click {
                    position: None,
                    button: 1,
                    hold_ms: 80,
                },
                reply,
            },
        );
        assert_eq!(app.session.pointer_buttons(), 2);
        finish_phase(&mut computer, &mut app);
        assert_eq!(app.session.pointer_buttons(), 0);
        assert!(
            response.try_recv().is_err(),
            "release must settle before screenshot"
        );
        finish_phase(&mut computer, &mut app);
        assert!(response.try_recv().unwrap().is_ok());
        let mut after = [0; 4];
        assert!(app.session.debug_read_memory(address, &mut after));
        assert_eq!(motion, after, "in-place click must not inject motion");
    }
    #[test]
    fn positioned_click_waits_before_press_and_cancellation_during_move_never_presses() {
        let mut app = hardware_app();
        app.session.set_cpu_mhz(1).unwrap();
        app.session.start();
        let mut computer = Computer::listen("127.0.0.1:0".parse().unwrap()).unwrap();
        for cancel in [true, false] {
            let (reply, mut response) = tokio_reply();
            let _ = computer.handle(
                &mut app,
                Request {
                    action: Action::Click {
                        position: Some((10, 20)),
                        button: 0,
                        hold_ms: 80,
                    },
                    reply,
                },
            );
            assert_eq!(app.session.pointer_buttons(), 0);
            assert!(matches!(
                computer.pending.as_ref().unwrap().phase,
                Phase::Position(0)
            ));
            if cancel {
                drop(response);
                computer.progress(&mut app);
                assert!(computer.pending.is_none());
                assert_eq!(app.session.pointer_buttons(), 0);
            } else {
                finish_phase(&mut computer, &mut app);
                assert_eq!(app.session.pointer_buttons(), 1);
                finish_phase(&mut computer, &mut app);
                assert_eq!(app.session.pointer_buttons(), 0);
                finish_phase(&mut computer, &mut app);
                assert!(response.try_recv().unwrap().is_ok());
            }
        }
    }
    fn tokio_reply() -> (
        Reply,
        tokio::sync::oneshot::Receiver<Result<Response, String>>,
    ) {
        tokio::sync::oneshot::channel()
    }
}
