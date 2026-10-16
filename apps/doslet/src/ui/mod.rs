#[cfg(target_arch = "wasm32")]
use crate::app::Doslet;
use crate::app::Message;
#[cfg(not(target_arch = "wasm32"))]
use crate::desktop::Doslet;
use doslet_runtime::SessionStatus;
use doslet_x86::Gpr16;
use iced::widget::{
    button, column, container, image, opaque, pick_list, responsive, row, rule, scrollable, space,
    stack, text, tooltip,
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
    #[cfg(target_arch = "wasm32")]
    ActualSize(bool),
}

#[derive(Clone, Default)]
pub struct Workspace {
    pub panel: Option<Panel>,
    #[cfg(target_arch = "wasm32")]
    pub actual_size: bool,
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
    let status = app.session.status();
    let can_run = matches!(
        status,
        SessionStatus::Ready | SessionStatus::Paused | SessionStatus::Running
    );
    let run = button(
        text(match status {
            SessionStatus::Running => "Pause",
            SessionStatus::Paused => "Resume",
            SessionStatus::Ready => "Run",
            _ => "Restart",
        })
        .size(13),
    )
    .padding([7, 16])
    .style(primary)
    .on_press(if can_run {
        Message::RunPause
    } else {
        Message::Reset
    });
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
    if !narrow && can_run {
        actions = actions.push(small_button("Restart", Message::Reset));
    }
    let actions = actions.extend(
        [
            small_button("Audio", Message::Ui(Action::Open(Panel::Audio))),
            small_button(
                "Settings",
                Message::Config(crate::configurator::Edit::Toggle),
            ),
        ]
        .map(Element::from),
    );
    #[cfg(target_arch = "wasm32")]
    let actions = actions.push(small_button(
        "Open…",
        Message::Ui(Action::Open(Panel::Games)),
    ));
    let toolbar: Element<'_, Message> = if narrow {
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
        text(if narrow {
            progress.unwrap_or(status_label(status))
        } else {
            status_label(status)
        })
        .size(12)
        .color(MUTED),
    ]
    .spacing(7)
    .align_y(iced::Center);
    if !narrow {
        bottom = bottom.push(space::horizontal().width(12)).push(
            text(if let Some(progress) = progress {
                progress
            } else if app.has_mouse_capture() {
                "Esc to release mouse"
            } else {
                ""
            })
            .size(12)
            .color(MUTED),
        );
    }
    if !narrow {
        bottom = bottom.push(space::horizontal());
    }
    bottom = bottom.push(audio_spectrum(app));
    bottom = bottom.push(mpu_indicator(app));
    if let Some(iced::widget::image::Handle::Rgba { width, height, .. }) = &app.display.image {
        bottom = bottom.push(text(format!("{width} × {height}")).size(12).color(MUTED));
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use crate::presentation::SizingMessage;
        let actual = app.display_sizing.actual_size;
        bottom = bottom
            .push(
                button(text("1:1").size(12))
                    .padding([3, 9])
                    .style(move |theme, status| segment(theme, status, actual))
                    .on_press_maybe(
                        app.display
                            .dimensions()
                            .map(|_| Message::DisplaySizing(SizingMessage::ActualSize)),
                    ),
            )
            .push(
                button(text("Fit").size(12))
                    .padding([3, 9])
                    .style(move |theme, status| segment(theme, status, !actual))
                    .on_press(Message::DisplaySizing(SizingMessage::Fit)),
            );
    }
    #[cfg(target_arch = "wasm32")]
    {
        let actual = app.workspace.actual_size;
        bottom = bottom
            .push(
                button(text("1:1").size(12))
                    .padding([3, 9])
                    .style(move |theme, status| segment(theme, status, actual))
                    .on_press_maybe(
                        app.display
                            .image
                            .as_ref()
                            .map(|_| Message::Ui(Action::ActualSize(true))),
                    ),
            )
            .push(
                button(text("Fit").size(12))
                    .padding([3, 9])
                    .style(move |theme, status| segment(theme, status, !actual))
                    .on_press(Message::Ui(Action::ActualSize(false))),
            );
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
        if app.session.fault().is_none() {
            alert = alert.push(small_button("×", Message::Ui(Action::DismissNotice)));
        }
        shell = shell.push(container(alert).padding([2, 16]).width(Fill));
    }
    let bottom: Element<'_, Message> = if narrow {
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

fn viewport(app: &Doslet) -> Element<'_, Message> {
    let viewport: Element<'_, Message> = if let Some(handle) = &app.display.image {
        let handle = handle.clone();
        let (pointer_width, pointer_height) = app.pointer_dimensions();
        let cursor = if app.hide_host_cursor() {
            iced::mouse::Interaction::Hidden
        } else {
            iced::mouse::Interaction::None
        };
        #[cfg(not(target_arch = "wasm32"))]
        let actual_size = app.display_sizing.image_size(&app.display);
        #[cfg(target_arch = "wasm32")]
        let actual_size = if app.workspace.actual_size {
            let scale = web_sys::window().map_or(1.0, |window| window.device_pixel_ratio()) as f32;
            app.session.frame().map(|frame| {
                iced::Size::new(frame.width as f32 / scale, frame.height as f32 / scale)
            })
        } else {
            None
        };
        responsive(move |available| {
            let size =
                actual_size.unwrap_or_else(|| crate::presentation::fit_size(available, 4.0 / 3.0));
            let (width, height) = (size.width, size.height);
            let content = container(
                iced::widget::mouse_area(
                    image(handle.clone())
                        .filter_method(image::FilterMethod::Nearest)
                        .content_fit(iced::ContentFit::Fill)
                        .width(width)
                        .height(height),
                )
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
            if actual_size.is_some() {
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
                text("Start this session with Run.").size(14).color(MUTED)
            ]
            .spacing(8)
            .align_x(iced::Center),
        )
        .center_x(Fill)
        .center_y(Fill)
        .into()
    };
    #[cfg(not(target_arch = "wasm32"))]
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
