#[cfg(target_arch = "wasm32")]
use crate::app::Doslet;
use crate::app::Message;
#[cfg(not(target_arch = "wasm32"))]
use crate::desktop::Doslet;
use doslet_runtime::SessionStatus;
use doslet_x86::Gpr16;
use iced::widget::{
    button, column, container, image, opaque, pick_list, responsive, row, rule, scrollable, space,
    stack, svg, text, tooltip,
};
use iced::{Border, Color, Element, Fill, Theme, theme};

const INK: Color = Color::from_rgb8(17, 18, 21);
pub(crate) const PANEL: Color = Color::from_rgb8(25, 27, 31);
pub(crate) const EDGE: Color = Color::from_rgb8(48, 51, 57);
pub(crate) const MUTED: Color = Color::from_rgb8(151, 156, 165);
pub(crate) const ACCENT: Color = Color::from_rgb8(177, 224, 190);
pub(crate) const FOREGROUND: Color = Color::from_rgb8(235, 237, 240);
const HOVER: Color = Color::from_rgb8(41, 44, 50);
const DANGER: Color = Color::from_rgb8(244, 150, 146);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Audio,
    Details,
    #[cfg(target_arch = "wasm32")]
    Games,
}

#[derive(Clone, Copy, Debug)]
pub enum Action {
    Open(Panel),
    Close,
    DismissNotice,
}

#[derive(Clone, Default)]
pub struct Workspace {
    pub panel: Option<Panel>,
}

pub fn theme() -> Theme {
    Theme::custom(
        "Doslet graphite",
        theme::Palette {
            background: INK,
            text: FOREGROUND,
            primary: ACCENT,
            success: ACCENT,
            warning: Color::from_rgb8(228, 193, 133),
            danger: DANGER,
        },
    )
}

pub fn view(app: &Doslet) -> Element<'_, Message> {
    responsive(move |size| workspace(app, size)).into()
}

fn workspace(app: &Doslet, size: iced::Size) -> Element<'_, Message> {
    let narrow = size.width < 700.0;
    let compact_footer = size.width < 1040.0;
    let status = app.session.status();
    let can_run = matches!(
        status,
        SessionStatus::Ready | SessionStatus::Paused | SessionStatus::Running
    );
    let (icon, label) = match status {
        SessionStatus::Running => (Icon::Pause, "Pause"),
        SessionStatus::Paused => (Icon::Play, "Resume"),
        SessionStatus::Ready => (Icon::Play, "Run"),
        _ => (Icon::Restart, "Restart"),
    };
    let run = icon_button(
        icon,
        label,
        if can_run {
            Message::RunPause
        } else {
            Message::Reset
        },
        true,
    );
    let identity = row![
        text("doslet").size(20),
        container(rule::vertical(1)).height(18),
        text(shorten(&app.name, if narrow { 16 } else { 38 }))
            .size(13)
            .color(MUTED),
    ]
    .spacing(16)
    .align_y(iced::Center);
    let mut actions = row![run].spacing(4).align_y(iced::Center);
    if can_run {
        actions = actions.push(icon_button(Icon::Restart, "Restart", Message::Reset, false));
    }
    #[cfg(target_arch = "wasm32")]
    let actions = actions.push(small_button(
        "Open…",
        Message::Ui(Action::Open(Panel::Games)),
    ));
    let toolbar: Element<'_, Message> = if size.width < 480.0 {
        column![identity, actions].spacing(10).into()
    } else {
        row![identity, space::horizontal(), actions]
            .align_y(iced::Center)
            .into()
    };
    let toolbar = container(toolbar).padding([10, 16]).width(Fill);
    let state_color = match status {
        SessionStatus::Running => ACCENT,
        SessionStatus::Faulted => DANGER,
        _ => MUTED,
    };
    let progress = if app.configurator.pending {
        Some("Preparing…")
    } else if app.session.pending_commit() {
        Some("Saving…")
    } else if app.session.storage_waiting() {
        Some("Loading…")
    } else if app.session.dirty_overlay() {
        Some("Unsaved changes")
    } else {
        None
    };
    let mut bottom = row![
        container(space::horizontal().width(6))
            .height(6)
            .style(move |_| container::Style {
                background: Some(state_color.into()),
                border: Border {
                    radius: 3.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            }),
        text(if compact_footer {
            progress.unwrap_or(status_label(status))
        } else {
            status_label(status)
        })
        .size(12)
        .color(MUTED),
    ]
    .spacing(7)
    .align_y(iced::Center);
    if !compact_footer {
        bottom = bottom.push(space::horizontal().width(12)).push(
            text(if let Some(progress) = progress {
                progress
            } else if app.has_mouse_capture() {
                "Ctrl+Esc to release mouse"
            } else {
                ""
            })
            .size(12)
            .color(MUTED),
        );
    }
    if !compact_footer {
        bottom = bottom.push(space::horizontal());
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        bottom = bottom.push(adaptive_indicator(app));
    }
    bottom = bottom.push(audio_spectrum(app));
    bottom = bottom.push(mpu_indicator(app));
    if let Some(iced::widget::image::Handle::Rgba { width, height, .. }) = &app.display.image {
        bottom = bottom.push(text(format!("{width} × {height}")).size(12).color(MUTED));
    }
    {
        use crate::presentation::{DisplayFilter, SizingMessage};
        let perfect = app.display_sizing.pixel_perfect;
        bottom = bottom
            .push(
                button(text("Pixel perfect").size(12))
                    .padding([3, 9])
                    .style(move |theme, status| segment(theme, status, perfect))
                    .on_press_maybe(
                        app.display
                            .dimensions()
                            .map(|_| Message::DisplaySizing(SizingMessage::PixelPerfect)),
                    ),
            )
            .push(
                button(text("Fit").size(12))
                    .padding([3, 9])
                    .style(move |theme, status| segment(theme, status, !perfect))
                    .on_press(Message::DisplaySizing(SizingMessage::Fit)),
            );
        bottom = bottom.push(tooltip(
            pick_list(
                &DisplayFilter::ALL[..],
                Some(app.display_sizing.filter),
                |filter| Message::DisplaySizing(SizingMessage::Filter(filter)),
            )
            .text_size(12)
            .padding([3, 7]),
            container(text("Display filter").size(12))
                .padding([6, 10])
                .style(panel),
            tooltip::Position::Top,
        ));
    }
    bottom = bottom.push(small_button(
        "Info",
        Message::Ui(Action::Open(Panel::Details)),
    ));
    let mut shell = column![toolbar, viewport(app)].height(Fill).width(Fill);
    let notice = app
        .session
        .fault()
        .map(|fault| fault.to_string())
        .or_else(|| app.display_sizing.error().map(str::to_owned))
        .or_else(|| app.notice.clone());
    if let Some(notice) = notice {
        let label = shorten(&notice, if narrow { 48 } else { 112 });
        let mut alert = row![
            tooltip(
                text(label).size(12).color(DANGER),
                container(text(notice).size(13))
                    .padding(12)
                    .max_width(540)
                    .style(panel),
                tooltip::Position::Top
            ),
            space::horizontal(),
            small_button("Details", Message::Ui(Action::Open(Panel::Details))),
        ]
        .spacing(8)
        .align_y(iced::Center);
        if app.session.fault().is_none() && app.display_sizing.error().is_none() {
            alert = alert.push(small_button("×", Message::Ui(Action::DismissNotice)));
        }
        shell = shell.push(container(alert).padding([2, 16]).width(Fill));
    }
    let bottom: Element<'_, Message> = if compact_footer {
        bottom.wrap().into()
    } else {
        bottom.into()
    };
    shell = shell.push(container(bottom).padding([5, 16]).width(Fill));
    let shell: Element<'_, Message> = shell.into();
    if app.configurator.open {
        // Keep the tabs in one place as sections change; only the sheet's
        // lower edge moves to accommodate its content.
        let top = ((size.height - 620.0) / 2.0).max(if narrow { 96.0 } else { 64.0 });
        let settings = container(
            app.configurator
                .view(app.session.dirty_overlay())
                .map(Message::Config),
        )
        .max_width(800)
        .width(Fill)
        .height((size.height - top - 16.0).clamp(
            1.0,
            app.configurator.preferred_height() + if size.width < 620.0 { 70.0 } else { 0.0 },
        ))
        .style(panel);
        return stack![
            shell,
            opaque(
                container(settings)
                    .center_x(Fill)
                    .height(Fill)
                    .padding(iced::Padding {
                        top,
                        right: 8.0,
                        bottom: 16.0,
                        left: 8.0
                    })
                    .style(backdrop)
            )
        ]
        .into();
    }
    if let Some(kind) = app.workspace.panel {
        let (title, content) = match kind {
            Panel::Audio => ("Audio", audio_panel(app)),
            Panel::Details => ("Session", details_panel(app)),
            #[cfg(target_arch = "wasm32")]
            Panel::Games => ("Open a game", games_panel(app)),
        };
        let content = if kind != Panel::Details
            && let Some(notice) = &app.notice
        {
            column![content, text(notice).size(12).color(DANGER)]
                .spacing(14)
                .into()
        } else {
            content
        };
        let header = row![
            text(title).size(18),
            space::horizontal(),
            small_button("Close", Message::Ui(Action::Close))
        ]
        .align_y(iced::Center);
        let dialog = container(
            column![
                header,
                rule::horizontal(1),
                scrollable(content).height(Fill)
            ]
            .spacing(18),
        )
        .padding(22)
        .max_width(520)
        .width(Fill)
        .height(
            (size.height - 40.0).clamp(160.0, if kind == Panel::Details { 430.0 } else { 320.0 }),
        )
        .style(panel);
        return modal(shell, dialog.into(), narrow);
    }
    shell
}

fn modal<'a>(
    base: Element<'a, Message>,
    dialog: Element<'a, Message>,
    narrow: bool,
) -> Element<'a, Message> {
    stack![
        base,
        opaque(
            container(dialog)
                .center_x(Fill)
                .center_y(Fill)
                .padding(if narrow { 8 } else { 16 })
                .style(backdrop)
        )
    ]
    .into()
}

fn backdrop(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgba8(0, 0, 0, 0.72).into()),
        ..Default::default()
    }
}

fn audio_panel(app: &Doslet) -> Element<'_, Message> {
    let body = column![
        field(
            "Output",
            pick_list(
                &app.midi_choices[..],
                Some(&app.midi_choice),
                Message::MidiSelected
            )
            .width(Fill)
            .text_size(14)
            .padding(8)
            .into()
        ),
        row![
            field(
                "Game MIDI",
                pick_list(
                    &doslet_midi_routing::Mode::ALL[..],
                    Some(app.midi_mode),
                    Message::MidiModeSelected
                )
                .width(Fill)
                .text_size(14)
                .padding(8)
                .into()
            ),
            field(
                "Output MIDI",
                pick_list(
                    &doslet_midi_routing::Mode::ALL[..],
                    Some(app.midi_output_mode),
                    Message::MidiOutputModeSelected
                )
                .width(Fill)
                .text_size(14)
                .padding(8)
                .into()
            ),
        ]
        .spacing(12),
        small_button("Refresh outputs", Message::RefreshMidi),
    ]
    .spacing(18);
    #[cfg(target_arch = "wasm32")]
    let body = body.push(
        button(text(app.audio_button_label()).size(13))
            .padding([8, 14])
            .style(primary)
            .on_press(Message::ToggleAudio),
    );
    body.into()
}

fn details_panel(app: &Doslet) -> Element<'_, Message> {
    let registers = app.session.registers();
    let register = |label: &'static str, value: String| {
        row![
            text(label).size(12).color(MUTED).width(76),
            text(value).size(13)
        ]
        .align_y(iced::Center)
    };
    let mut step = button(text("Step instruction").size(13))
        .padding([7, 12])
        .style(secondary);
    if matches!(
        app.session.status(),
        SessionStatus::Ready | SessionStatus::Paused
    ) {
        step = step.on_press(Message::Step);
    }
    let mut body = column![
        text(&app.name).size(15),
        row![
            small_button(
                "Settings…",
                Message::Config(crate::configurator::Edit::Toggle),
            ),
            small_button("Audio…", Message::Ui(Action::Open(Panel::Audio))),
        ]
        .spacing(8),
        text(app.storage_label()).size(13).color(MUTED),
        text(format!("{} instructions", app.session.steps()))
            .size(13)
            .color(MUTED),
        column![
            register(
                "CS:EIP",
                format!("{:04X}:{:08X}", registers.cs, registers.ip)
            ),
            register("AX", format!("{:04X}", registers.get(Gpr16::Ax))),
            register("CX", format!("{:04X}", registers.get(Gpr16::Cx))),
            register("DI", format!("{:04X}", registers.get(Gpr16::Di))),
        ]
        .spacing(4),
        row![step, small_button("Restart", Message::Reset)].spacing(8),
        rule::horizontal(1),
        text(app.audio_label()).size(12).color(MUTED),
        text(app.midi_mode_label()).size(12).color(MUTED),
    ]
    .spacing(14);
    #[cfg(not(target_arch = "wasm32"))]
    {
        if let Some(tiers) = app.speed_tiers {
            body = body.push(
                column![
                    text(format!(
                        "Speed tiers · hot {} / cold {} / grey {} MHz",
                        tiers
                            .hot_mhz
                            .map_or_else(|| "MAX".into(), |m| m.to_string()),
                        tiers.cold_mhz,
                        tiers.grey_mhz
                    ))
                    .size(12),
                    row![
                        text(format!("Hot {:.1}%", tiers.percent(0)))
                            .color(Color::from_rgb8(240, 170, 100)),
                        text(format!("Cold {:.1}%", tiers.percent(1)))
                            .color(Color::from_rgb8(120, 180, 240)),
                        text(format!("Grey {:.1}%", tiers.percent(2))).color(MUTED),
                        text(format!("IRQ {:.1}%", tiers.percent(3))).color(MUTED),
                    ]
                    .spacing(12),
                    text("Share of guest time since activation · frozen experimental profile")
                        .size(11)
                        .color(MUTED),
                    small_button(
                        if tiers.enabled {
                            "Use normal CPU"
                        } else {
                            "Enable speed tiers"
                        },
                        Message::SpeedTiersToggle
                    ),
                ]
                .spacing(8),
            );
        }
        let (mcp_label, mcp_action) = match app.mcp {
            Some(address) => (format!("MCP http://{address}/mcp"), "Stop MCP"),
            None => ("MCP disabled".into(), "Enable MCP · screen and input"),
        };
        body = body.push(
            column![
                text(mcp_label).size(12).color(MUTED),
                small_button(mcp_action, Message::McpToggle)
            ]
            .spacing(8),
        );
        let (label, action) = match app.gdb {
            Some((address, connected)) => (
                format!(
                    "GDB {address} · {}",
                    if connected { "connected" } else { "listening" }
                ),
                "Stop GDB",
            ),
            None => ("GDB disabled".into(), "Enable GDB · 127.0.0.1:9001"),
        };
        body = body.push(
            column![
                text(label).size(12).color(MUTED),
                small_button(action, Message::GdbToggle),
            ]
            .spacing(8),
        );
        body = body.push(
            row![
                text("CPU MHz · live").size(12),
                pick_list(
                    &[8u16, 16, 25, 33, 50, 66, 100, 132, 133, 200, 266, 500, 1000][..],
                    Some(app.configurator.active.hardware.cpu_mhz),
                    Message::CpuMhz,
                ),
            ]
            .spacing(8),
        );
        body = body.push(
            text(format!(
                "Core: {} MHz · emulation lag {:.1} ms",
                app.configurator.active.hardware.cpu_mhz,
                app.lag_nanos as f64 / 1e6,
            ))
            .size(12)
            .color(MUTED),
        );
        body = body.push(
            column![
                small_button(
                    if app.menu_idle.0 && !app.learned_idle {
                        "Disable Heroes adaptive idle"
                    } else {
                        "Try Heroes adaptive idle"
                    },
                    Message::HeroesMenuIdleToggle
                ),
                text(format!(
                    "Experimental · {:.1} ms CPU idle · wait {:.0} µs",
                    app.menu_idle.1 as f64 / 1e6,
                    app.menu_idle.2 as f64 / 1e3
                ))
                .size(12)
                .color(MUTED),
            ]
            .spacing(8),
        );
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let o = app.idle_observation;
        body = body.push(
            column![
                small_button(
                    if app.learned_idle {
                        "Disable learned idle"
                    } else {
                        "Enable learned idle"
                    },
                    Message::AdaptiveIdleToggle
                ),
                text("Experimental · budgets empty input polls and countdown guards · waits up to 250 µs")
                    .size(12)
                    .color(MUTED),
            ]
            .spacing(8),
        );
        body = body.push(column![
            small_button(if o.enabled { "Stop idle observation" } else { "Observe idle loops" },
                Message::IdleObservationToggle),
            text(if !o.enabled { "Observer off".to_owned() } else { format!(
                "Observer · {}\n{} samples · {} at {:04X}:{:08X}\n{} input queries · {} RAM writes · {} device accesses\nDiagnostic only — no automatic sleep.",
                o.reason, o.samples, o.repeated, o.cs, o.ip, o.queries, o.writes, o.device_accesses,
            ) }).size(12).color(MUTED),
        ].spacing(8));
    }
    #[cfg(not(target_arch = "wasm32"))]
    if app.idle_observation.enabled {
        let s = app.synchronization;
        if app.learned_idle {
            body = body.push(
                text(format!(
                    "Empty-poll waits {} · observed CPU work {:.1} µs\nLearned guards · {} arrivals · last check {:08X}: {}",
                    s.poll_waits, s.poll_cpu_peak_nanos as f64 / 1000.0,
                    s.learned_hits,
                    s.learned_checked_ip,
                    if s.learned_reason.is_empty() {
                        "searching for guard"
                    } else {
                        s.learned_reason
                    }
                ))
                .size(12)
                .color(MUTED),
            );
        }
        let slack = s.provisional_slack_nanos.map_or_else(
            || "collecting fresh recurrence samples".to_owned(),
            |ns| {
                format!(
                    "{:.3} ms (~{} nominal steps)",
                    ns as f64 / 1e6,
                    s.provisional_slack_steps.unwrap_or(0)
                )
            },
        );
        let horizon = s.horizon_nanos.map_or_else(
            || "unknown".to_owned(),
            |ns| format!("{:.3} ms · {}", ns as f64 / 1e6, s.horizon_source),
        );
        body = body.push(text(format!(
            "Provisional budget · {slack}\nNext known service: {horizon}\nObserved recurrence peak {:.3} ms · {} samples · reserve {:.3} ms\nRecurrence is not measured useful work. Only VGA mask 08h has a linked transition; other wake sources are unproven. No automatic sleep.",
            s.recurrence_peak_nanos as f64 / 1e6, s.recurrence_samples,
            s.reserve_nanos as f64 / 1e6,
        )).size(12).color(MUTED));
        body = body.push(text(format!(
            "Synchronization · {} traced steps · {} conditions\n{:04X}:{:08X} · {} {:08X} · {} byte(s)\nValue {:X}, compare/mask {:X} · taken {} / fallthrough {}\nValue changes between tests: {} · across traced steps: {}\nLast change: RAM {:08X}, step {:04X}:{:08X} · IRQ pending {} · device access {}\nChange source unproven; tracing does not authorize sleep.",
            s.traced_steps, s.conditions, s.cs, s.ip, if s.port { "port" } else { "RAM" },
            s.operand, s.width, s.value, s.comparison, s.taken, s.fallthrough,
            s.value_changes, s.observed_changes, s.change_operand, s.change_cs, s.change_ip,
            s.change_with_irq, s.change_with_device_access,
        )).size(12).color(MUTED));
    }
    #[cfg(not(target_arch = "wasm32"))]
    if app.analysis.enabled {
        let a = app.analysis;
        let guest_audio = if a.audio_irq_deadlines == 0 {
            "Guest DMA IRQ service: no complete observation".to_owned()
        } else {
            format!(
                "Guest DMA IRQ service: {} missed / {} observed block deadlines",
                a.audio_irq_deadline_misses, a.audio_irq_deadlines
            )
        };
        body = body.push(
            text(format!(
                "Host PCM missing frames: {:?}\n{guest_audio}",
                app.host_audio_underruns,
            ))
            .size(12)
            .color(MUTED),
        );
        body=body.push(text(format!("Synchronization analyzer · {}\n{} nodes · {} backedges · {} traced steps · {} gaps\nCPU {} MHz effective / {} MHz ceiling\n{}",a.graph_reason,a.graph_nodes,a.backedges,a.observed_steps,a.gaps,a.effective_mhz,a.configured_mhz,a.reason)).size(12).color(MUTED));
        for c in a.candidates.into_iter().filter(|c| c.width != 0) {
            let work_label = if c.boundary_accounting {
                "work upper bound"
            } else {
                "traced work"
            };
            let source = if c.port {
                format!("VGA retrace · port {:04X}", c.operand)
            } else {
                format!(
                    "RAM {:08X} · writer {:?} · IRQ {:?} · {} partial writes",
                    c.operand, c.writer_pc, c.writer_irq, c.partial_writes
                )
            };
            body=body.push(text(format!("{:08X} · {} · {}\n{} complete episodes · {} {:.3} ms / confirmed wait {:.3} ms · max {:.3} ms\n{} · predicted {:?} MHz",c.head,c.state,c.reason,c.episodes,work_label,c.work_nanos as f64/1e6,c.wait_nanos as f64/1e6,c.work_max_nanos as f64/1e6,source,c.predicted_mhz)).size(12).color(MUTED));
            if c.reference_period_nanos != 0 {
                let mean = c.cadence_mean_nanos.map_or_else(
                    || "collecting".to_owned(),
                    |nanos| format!("{:.3} ms", nanos as f64 / 1e6),
                );
                body = body.push(
                    text(format!(
                        "Reference period {:.3} ms · current mean {mean} · cadence floor {} MHz",
                        c.reference_period_nanos as f64 / 1e6,
                        c.cadence_floor_mhz,
                    ))
                    .size(12)
                    .color(MUTED),
                );
            }
        }
        for g in a.guards.into_iter().filter(|g| g.head != 0) {
            body = body.push(text(format!(
                "Returning guard {:08X} · RAM {:08X}\nCaller {:08X} · {} observed backedges\n{} IRQ decrements · writer {:?} · IRQ {:?}\n{}",
                g.head, g.operand, g.caller, g.caller_backedges, g.observed_decrements, g.writer_pc, g.writer_irq, g.reason,
            )).size(12).color(MUTED));
        }
        if a.caller.head != 0 {
            let c = a.caller;
            body = body.push(text(format!(
                "Caller trace {:08X} · {}/{} complete iterations · {} exhausted\nLast {} steps / {:.3} ms · {} service gaps · {} dropped effects\n{}",
                c.caller, c.complete, c.attempts, c.exhausted, c.last_steps,
                c.last_nanos as f64 / 1e6, c.service_gaps, c.dropped_effects, c.reason,
            )).size(12).color(MUTED));
        }
        if a.recurrence.head != 0 {
            let r = a.recurrence;
            let kind = if r.backedge { "backedge" } else { "caller" };
            body = body.push(text(format!(
                "Recurring {kind} {:08X} · {}/{} unchanged intervals\nMeasured wait {:.3} ms · {} rejected · {} cancelled\n{} abandoned probes · {}",
                r.caller, r.unchanged, r.intervals, r.measured_wait_nanos as f64 / 1e6, r.rejected, r.cancelled,
                r.probe_rejections, r.last_probe_rejection_reason,
            )).size(12).color(MUTED));
        }
    }
    if let Some(fault) = app.session.fault() {
        body = body.push(text(fault.to_string()).size(13).color(DANGER));
    }
    if let Some(notice) = &app.notice {
        body = body.push(text(notice).size(13).color(MUTED));
    }
    body.into()
}

#[cfg(target_arch = "wasm32")]
fn games_panel(app: &Doslet) -> Element<'_, Message> {
    let programs = app
        .archive
        .as_ref()
        .map_or(&[][..], |archive| archive.programs.as_slice());
    let mut open = button(
        text(if app.importing {
            "Opening…"
        } else {
            "Choose ZIP…"
        })
        .size(14),
    )
    .padding([10, 16])
    .style(secondary);
    if !app.importing {
        open = open.on_press(Message::OpenArchive);
    }
    let mut start = button(text("Start game").size(14))
        .padding([10, 16])
        .style(primary);
    if app.program.is_some() && !app.importing {
        start = start.on_press(Message::StartArchive);
    }
    column![
        open,
        field(
            "Program",
            pick_list(programs, app.program.as_ref(), Message::ProgramSelected)
                .placeholder("Choose EXE / COM")
                .width(Fill)
                .text_size(14)
                .padding(9)
                .into()
        ),
        row![space::horizontal(), start],
    ]
    .spacing(20)
    .into()
}

pub(crate) fn field<'a, M: 'a>(label: &'a str, input: Element<'a, M>) -> Element<'a, M> {
    column![text(label).size(12).color(MUTED), input]
        .spacing(6)
        .width(Fill)
        .into()
}

fn small_button(label: &str, message: Message) -> iced::widget::Button<'_, Message> {
    button(text(label).size(13))
        .padding([6, 10])
        .style(ghost)
        .on_press(message)
}

#[derive(Clone, Copy)]
enum Icon {
    Play,
    Pause,
    Restart,
}

impl Icon {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Play => include_bytes!("icons/play.svg"),
            Self::Pause => include_bytes!("icons/pause.svg"),
            Self::Restart => include_bytes!("icons/restart.svg"),
        }
    }
}

fn icon_button(
    icon: Icon,
    label: &'static str,
    message: Message,
    prominent: bool,
) -> Element<'static, Message> {
    let icon = svg(svg::Handle::from_memory(icon.bytes()))
        .width(18)
        .height(18)
        .style(move |_, _| svg::Style {
            color: Some(if prominent { INK } else { FOREGROUND }),
        });
    tooltip(
        button(container(icon).center_x(Fill).center_y(Fill))
            .width(36)
            .height(36)
            .padding(0)
            .style(if prominent { primary } else { ghost })
            .on_press(message),
        container(text(label).size(12))
            .padding([6, 10])
            .style(panel),
        tooltip::Position::Bottom,
    )
    .gap(6)
    .into()
}

fn viewport(app: &Doslet) -> Element<'_, Message> {
    let viewport: Element<'_, Message> = if let Some(frame) = app.display_sizing.image() {
        let handle = frame.image.clone();
        let pixels = frame.source_size;
        let sampling = frame.filter.sampling();
        let (pointer_width, pointer_height) = app.pointer_dimensions();
        let cursor = if app.hide_host_cursor() {
            iced::mouse::Interaction::Hidden
        } else {
            iced::mouse::Interaction::None
        };
        responsive(move |available| {
            let size = app.display_sizing.image_size(available, pixels);
            let (width, height) = (size.width, size.height);
            let pixels: Element<'_, Message> = image(handle.clone())
                .filter_method(sampling)
                .content_fit(iced::ContentFit::Fill)
                .width(width)
                .height(height)
                .into();
            #[cfg(not(target_arch = "wasm32"))]
            let pixels: Element<'_, Message> = if app.speed_tiers.is_some_and(|s| s.enabled) {
                stack![
                    pixels,
                    container(speed_tier_overlay(app))
                        .width(width)
                        .height(height)
                        .align_x(iced::Right)
                        .padding(10),
                ]
                .into()
            } else {
                pixels
            };
            let content = container(
                iced::widget::mouse_area(pixels)
                    .interaction(cursor)
                    .on_move(move |position| {
                        Message::PointerMoved(
                            (position.x / width * f32::from(pointer_width))
                                .clamp(0.0, f32::from(pointer_width - 1))
                                as u16,
                            (position.y / height * f32::from(pointer_height))
                                .clamp(0.0, f32::from(pointer_height - 1))
                                as u16,
                        )
                    })
                    .on_press(Message::PointerButton(0, true))
                    .on_release(Message::PointerButton(0, false))
                    .on_right_press(Message::PointerButton(1, true))
                    .on_right_release(Message::PointerButton(1, false))
                    .on_exit(Message::PointerLeft),
            );
            if app.display_sizing.pixel_perfect
                && (width > available.width || height > available.height)
            {
                scrollable(
                    content
                        .center_x(available.width.max(width))
                        .center_y(available.height.max(height)),
                )
                .direction(scrollable::Direction::Both {
                    vertical: scrollable::Scrollbar::default(),
                    horizontal: scrollable::Scrollbar::default(),
                })
                .width(Fill)
                .height(Fill)
                .into()
            } else {
                content.center_x(Fill).center_y(Fill).into()
            }
        })
        .into()
    } else {
        container(
            column![
                text("Ready to play").size(22),
                text("Press the play button to start.")
                    .size(14)
                    .color(MUTED)
            ]
            .spacing(8)
            .align_x(iced::Center),
        )
        .center_x(Fill)
        .center_y(Fill)
        .into()
    };
    let viewport = iced::widget::sensor(viewport)
        .on_show(|size| Message::DisplaySizing(crate::presentation::SizingMessage::Viewport(size)))
        .on_resize(|size| {
            Message::DisplaySizing(crate::presentation::SizingMessage::Viewport(size))
        });
    container(viewport)
        .width(Fill)
        .height(Fill)
        .style(|_| container::Style {
            background: Some(Color::BLACK.into()),
            ..Default::default()
        })
        .into()
}

#[cfg(not(target_arch = "wasm32"))]
fn speed_tier_overlay(app: &Doslet) -> Element<'_, Message> {
    let colors = [
        Color::from_rgb8(255, 179, 105),
        Color::from_rgb8(125, 194, 255),
        MUTED,
        Color::from_rgb8(194, 155, 255),
    ];
    let shares = app.tier_meter.shares;
    let heading = if app.gdb.is_some_and(|(_, connected)| connected) {
        "Speed tiers · GDB controls execution".into()
    } else if app.session.status() != SessionStatus::Running {
        "Speed tiers · paused".into()
    } else if let Some(s) = shares {
        format!("Speed tiers · last {:.1} s", s.seconds)
    } else {
        "Speed tiers · sampling…".into()
    };
    let mut table = row![
        column![
            text(" ").size(12),
            text("Steps").size(12),
            text("Guest time").size(12)
        ]
        .spacing(3)
    ]
    .spacing(12);
    for (i, label) in ["Hot", "Cold", "Grey", "IRQ"].into_iter().enumerate() {
        let value = |cpu: bool| {
            shares.map_or_else(
                || "—".into(),
                |s| format!("{:.1}%", if cpu { s.cpu[i] } else { s.guest[i] }),
            )
        };
        table = table.push(
            column![
                text(label).size(12).color(colors[i]),
                text(value(true)).size(12).color(colors[i]),
                text(value(false)).size(12).color(colors[i]),
            ]
            .width(52)
            .spacing(3),
        );
    }
    // Non-interactive: the surrounding mouse area still receives game input.
    container(column![text(heading).size(11).color(FOREGROUND), table].spacing(5))
        .padding([8, 10])
        .style(|_| container::Style {
            background: Some(Color::from_rgba8(17, 18, 21, 0.84).into()),
            border: Border {
                radius: 7.0.into(),
                color: EDGE,
                width: 1.0,
            },
            ..Default::default()
        })
        .into()
}

pub(crate) fn panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(PANEL.into()),
        border: Border {
            color: EDGE,
            width: 1.0,
            radius: 10.0.into(),
        },
        ..Default::default()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn adaptive_indicator(app: &Doslet) -> Element<'_, Message> {
    if let Some(tiers) = app.speed_tiers {
        let label = if tiers.enabled {
            format!(
                "Tiers · {} / {} / {}",
                tiers
                    .hot_mhz
                    .map_or_else(|| "MAX".into(), |m| m.to_string()),
                tiers.cold_mhz,
                tiers.grey_mhz
            )
        } else {
            "Tiers off".into()
        };
        return button(
            text(label)
                .size(12)
                .color(if tiers.enabled { ACCENT } else { MUTED }),
        )
        .style(button::text)
        .on_press(Message::Ui(Action::Open(Panel::Details)))
        .into();
    }
    let enabled = app.menu_idle.0;
    let running = app.session.status() == SessionStatus::Running;
    let percent = if enabled && running {
        app.adaptive_idle_percent
    } else {
        0
    };
    let label = if app.idle_observation.enabled && app.idle_observation.candidate && percent == 0 {
        "Idle candidate".to_owned()
    } else if running
        && app.idle_observation.enabled
        && percent == 0
        && app.synchronization.conditions != 0
    {
        if app.synchronization.port {
            "Trace: port"
        } else {
            "Trace: RAM"
        }
        .to_owned()
    } else if app.idle_observation.enabled && percent == 0 {
        if running {
            "Observing"
        } else {
            "Observer paused"
        }
        .to_owned()
    } else if !enabled {
        "Auto off".to_owned()
    } else if !running {
        "Auto paused".to_owned()
    } else if percent == 0 {
        "Full speed".to_owned()
    } else {
        format!("Idle {percent}%")
    };
    let mut bars = iced::widget::Row::new().spacing(2).align_y(iced::Bottom);
    for i in 0..10 {
        let lit = u16::from(percent) > i * 10;
        let color = if lit { ACCENT } else { EDGE };
        bars = bars.push(
            container(space::horizontal().width(3))
                .height(4 + i as u32)
                .style(move |_| container::Style {
                    background: Some(color.into()),
                    ..Default::default()
                }),
        );
    }
    tooltip(
        button(row![bars, text(label).size(12).color(if percent > 0 { ACCENT } else { MUTED })]
            .spacing(6).align_y(iced::Center))
            .padding([4, 6]).style(button::text)
            .on_press(Message::Ui(Action::Open(Panel::Details))),
        container(text(format!(
            "Adaptive idle · {percent}% guest time waiting\nRecent 250 ms of emulated time · more bars = more waiting\nActive CPU: {} MHz · wait limit: {:.0} µs\nNot host CPU usage or battery savings. Click for Details.\nObserver: {} (never skips execution)",
            app.configurator.active.hardware.cpu_mhz, app.menu_idle.2 as f64 / 1e3,
            if app.idle_observation.enabled { app.idle_observation.reason } else { "off" },
        )).size(12)).padding(10).style(panel),
        tooltip::Position::Top,
    ).into()
}

fn audio_spectrum(app: &Doslet) -> Element<'_, Message> {
    let mut bars = iced::widget::Row::new().spacing(2);
    for level in app.audio_spectrum() {
        let lit = (level * 8.0).round() as usize;
        let mut bar = iced::widget::Column::new().spacing(1);
        for segment in (0..8).rev() {
            let color = if segment >= lit {
                Color::from_rgb8(34, 44, 38)
            } else if segment >= 7 {
                Color::from_rgb8(226, 125, 113)
            } else if segment >= 5 {
                Color::from_rgb8(221, 192, 106)
            } else {
                Color::from_rgb8(117, 202, 145)
            };
            bar = bar.push(
                container(space::horizontal().width(4))
                    .height(2)
                    .style(move |_| container::Style {
                        background: Some(color.into()),
                        ..Default::default()
                    }),
            );
        }
        bars = bars.push(bar);
    }
    tooltip(
        container(bars).padding([4, 6]),
        container(
            text(format!(
                "Audio spectrum · bass → treble\n{}\nFM / MIDI · synthesized audio\nExternal MIDI audio is not measured.",
                app.guest_audio_description(),
            ))
            .size(12),
        )
        .padding(10)
        .style(panel),
        tooltip::Position::Top,
    )
    .into()
}

pub(crate) fn guest_pcm_description(rate: u32, bits: u8, channels: u8) -> String {
    if bits == 0 {
        "Guest PCM · not configured".into()
    } else {
        format!(
            "Guest PCM · {rate} Hz · {bits}-bit · {}",
            if channels == 2 { "Stereo" } else { "Mono" },
        )
    }
}

fn mpu_indicator(app: &Doslet) -> Element<'_, Message> {
    use crate::midi_activity::Lamp;
    let lamp = app
        .midi_activity
        .lamp(app.session.status() == SessionStatus::Running);
    let color = match lamp {
        Lamp::Note => ACCENT,
        Lamp::Data => Color::from_rgb8(228, 193, 133),
        Lamp::Idle => Color::from_rgb8(79, 85, 94),
    };
    let light = container(space::horizontal().width(6))
        .height(6)
        .style(move |_| container::Style {
            background: Some(color.into()),
            border: Border {
                radius: 3.0.into(),
                ..Default::default()
            },
            shadow: iced::Shadow {
                color: Color {
                    a: if lamp == Lamp::Idle { 0.0 } else { 0.3 },
                    ..color
                },
                blur_radius: 5.0,
                ..Default::default()
            },
            ..Default::default()
        });
    let badge = container(
        row![light, text("MPU").size(10).color(MUTED)]
            .spacing(6)
            .align_y(iced::Center),
    )
    .padding([4, 8])
    .style(|_| container::Style {
        background: Some(PANEL.into()),
        border: Border {
            color: EDGE,
            width: 1.0,
            radius: 5.0.into(),
        },
        ..Default::default()
    });
    tooltip(
        badge,
        container(text(app.midi_activity.description()).size(12))
            .padding(10)
            .style(panel),
        tooltip::Position::Top,
    )
    .into()
}

pub(crate) fn primary(_: &Theme, status: button::Status) -> button::Style {
    let disabled = status == button::Status::Disabled;
    button::Style {
        background: Some(
            if disabled {
                HOVER
            } else if status == button::Status::Hovered {
                Color::from_rgb8(199, 238, 209)
            } else {
                ACCENT
            }
            .into(),
        ),
        text_color: if disabled { MUTED } else { INK },
        border: Border {
            radius: 6.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub(crate) fn secondary(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: Some(
            if status == button::Status::Hovered {
                HOVER
            } else {
                PANEL
            }
            .into(),
        ),
        text_color: if status == button::Status::Disabled {
            MUTED
        } else {
            FOREGROUND
        },
        border: Border {
            color: EDGE,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    }
}

pub(crate) fn ghost(_: &Theme, status: button::Status) -> button::Style {
    button::Style {
        background: (status == button::Status::Hovered).then_some(HOVER.into()),
        text_color: if status == button::Status::Hovered {
            FOREGROUND
        } else {
            MUTED
        },
        border: Border {
            radius: 6.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub(crate) fn segment(theme: &Theme, status: button::Status, selected: bool) -> button::Style {
    if selected {
        button::Style {
            background: Some(Color::from_rgba8(177, 224, 190, 0.10).into()),
            text_color: ACCENT,
            border: Border {
                radius: 6.0.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    } else {
        ghost(theme, status)
    }
}

fn shorten(value: &str, length: usize) -> String {
    let mut chars = value.chars();
    let mut short: String = chars.by_ref().take(length).collect();
    if chars.next().is_some() {
        short.push('…');
    }
    short
}

fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Ready => "Ready",
        SessionStatus::Running => "Running",
        SessionStatus::Paused => "Paused",
        SessionStatus::Exited(_) => "Finished",
        SessionStatus::Halted => "Halted",
        SessionStatus::Faulted => "Stopped",
    }
}
