use crate::app::{Doslet, Message};
use doslet_runtime::SessionStatus;
use doslet_x86::Gpr16;
use iced::widget::{button, column, container, image, responsive, row, scrollable, space, text};
use iced::{Border, Color, Element, Fill, Font, Theme, theme};

const INK: Color = Color::from_rgb8(12, 17, 26);
const PANEL: Color = Color::from_rgb8(20, 28, 40);
const EDGE: Color = Color::from_rgb8(43, 57, 74);
const MUTED: Color = Color::from_rgb8(151, 166, 185);
const ACCENT: Color = Color::from_rgb8(88, 223, 190);

pub fn theme() -> Theme {
    Theme::custom(
        "Doslet midnight",
        theme::Palette {
            background: INK,
            text: Color::from_rgb8(235, 241, 247),
            primary: ACCENT,
            success: ACCENT,
            warning: Color::from_rgb8(245, 197, 112),
            danger: Color::from_rgb8(248, 131, 131),
        },
    )
}

pub fn view(app: &Doslet) -> Element<'_, Message> {
    let status = app.session.status();
    let can_run = matches!(
        status,
        SessionStatus::Ready | SessionStatus::Paused | SessionStatus::Running
    );
    let can_step = matches!(status, SessionStatus::Ready | SessionStatus::Paused);
    let mut run = button(if status == SessionStatus::Running {
        "Pause"
    } else {
        "Run"
    })
    .padding([12, 18]);
    if can_run {
        run = run.on_press(Message::RunPause);
    }
    let mut step = button("Step").padding([12, 18]).style(button::secondary);
    if can_step {
        step = step.on_press(Message::Step);
    }
    let controls = row![
        run,
        step,
        button("Reset")
            .on_press(Message::Reset)
            .padding([12, 18])
            .style(button::secondary)
    ]
    .spacing(10);
    let heading = row![
        column![
            text("doslet").size(38).color(ACCENT),
            text("A little DOS. Room to explore.").size(14).color(MUTED)
        ]
        .spacing(4),
        space::horizontal(),
        container(text("EARLY DEVELOPMENT").size(11).color(ACCENT))
            .padding([9, 13])
            .style(panel),
    ]
    .align_y(iced::Center);

    let registers = app.session.registers();
    let inspector = column![
        text("WORKSPACE").size(11).color(MUTED),
        text(&app.name).size(24),
        text("Running on the guest CPU.").size(14).color(MUTED),
        space::vertical().height(12),
        text("SESSION").size(11).color(MUTED),
        text(status_label(status)).size(16).color(ACCENT),
        text(format!("{} execution steps", app.session.steps()))
            .size(13)
            .color(MUTED),
        text(format!(
            "CS:EIP {:04X}:{:08X}\nAX     {:04X}\nCX     {:04X}\nDI     {:04X}",
            registers.cs,
            registers.ip,
            registers.get(Gpr16::Ax),
            registers.get(Gpr16::Cx),
            registers.get(Gpr16::Di)
        ))
        .font(Font::MONOSPACE)
        .size(14),
        space::vertical().height(16),
        text("COMPATIBILITY TARGET").size(11).color(MUTED),
        text("Machiavelli the Prince").size(17),
        text(app.audio_label()).size(13).color(MUTED),
    ]
    .spacing(12);
    let inspector = inspector
        .push(text("GAME MIDI").size(11).color(MUTED))
        .push(
            iced::widget::pick_list(
                &doslet_midi_routing::Mode::ALL[..],
                Some(app.midi_mode),
                Message::MidiModeSelected,
            )
            .width(Fill),
        )
        .push(text(app.midi_mode_label()).size(12).color(MUTED))
        .push(text("HOST PORT DIALECT").size(11).color(MUTED))
        .push(
            iced::widget::pick_list(
                &doslet_midi_routing::Mode::ALL[..],
                Some(app.midi_output_mode),
                Message::MidiOutputModeSelected,
            )
            .width(Fill),
        )
        .push(text("MIDI OUTPUT").size(11).color(MUTED))
        .push(
            iced::widget::pick_list(
                &app.midi_choices[..],
                Some(&app.midi_choice),
                Message::MidiSelected,
            )
            .width(Fill),
        )
        .push(
            button("Find MIDI outputs")
                .on_press(Message::RefreshMidi)
                .padding([10, 14]),
        );
    #[cfg(target_arch = "wasm32")]
    let inspector = {
        use iced::widget::pick_list;
        let mut open = button(if app.importing {
            "Reading ZIP…"
        } else {
            "Open game ZIP"
        })
        .padding([10, 14]);
        if !app.importing {
            open = open.on_press(Message::OpenArchive);
        }
        let programs = app
            .archive
            .as_ref()
            .map_or(&[][..], |archive| archive.programs.as_slice());
        let mut start = button("Start game").padding([10, 14]);
        if app.program.is_some() && !app.importing {
            start = start.on_press(Message::StartArchive);
        }
        inspector
            .push(text("GAME FILES").size(11).color(MUTED))
            .push(open)
            .push(
                pick_list(programs, app.program.as_ref(), Message::ProgramSelected)
                    .placeholder("Choose EXE / COM")
                    .width(Fill),
            )
            .push(start)
            .push(
                button(app.audio_button_label())
                    .on_press(Message::ToggleAudio)
                    .padding([10, 14]),
            )
            .push(
                text("ZIP stays local. Writes are kept in RAM.")
                    .size(12)
                    .color(MUTED),
            )
    };
    let viewport: Element<'_, Message> = if let Some(handle) = &app.display.image {
        let handle = handle.clone();
        let (pointer_width, pointer_height) = app.pointer_dimensions();
        responsive(move |size| {
            let width = size.width.min(size.height * 4.0 / 3.0);
            container(
                iced::widget::mouse_area(
                    image(handle.clone())
                        .filter_method(image::FilterMethod::Nearest)
                        .content_fit(iced::ContentFit::Fill)
                        .width(width)
                        .height(width * 3.0 / 4.0),
                )
                .on_move(move |position| {
                    Message::PointerMoved(
                        (position.x / width * f32::from(pointer_width))
                            .clamp(0.0, f32::from(pointer_width - 1))
                            as u16,
                        (position.y / (width * 3.0 / 4.0) * f32::from(pointer_height))
                            .clamp(0.0, f32::from(pointer_height - 1))
                            as u16,
                    )
                })
                .on_press(Message::PointerButton(0, true))
                .on_release(Message::PointerButton(0, false))
                .on_right_press(Message::PointerButton(1, true))
                .on_right_release(Message::PointerButton(1, false))
                .on_exit(Message::PointerLeft),
            )
            .center_x(Fill)
            .center_y(Fill)
            .into()
        })
        .into()
    } else {
        container(
            column![
                text(">_").size(72).font(Font::MONOSPACE).color(ACCENT),
                text("Your little DOS awaits.").size(22),
                text("Run to draw the first guest frame.")
                    .size(14)
                    .color(MUTED)
            ]
            .spacing(12)
            .align_x(iced::Center),
        )
        .center_x(Fill)
        .center_y(Fill)
        .into()
    };
    let viewport = container(viewport)
        .padding(18)
        .height(Fill)
        .width(Fill)
        .style(panel);
    let display = column![
        row![
            text("GUEST DISPLAY").size(11).color(MUTED),
            space::horizontal(),
            text(
                app.session
                    .frame()
                    .map_or("80 × 25 · text".into(), |frame| format!(
                        "{} × {} · 256 colors",
                        frame.width, frame.height
                    ))
            )
            .size(12)
            .color(MUTED)
        ],
        viewport,
        text("Keyboard: type into the game; arrows and Enter select menu items.")
            .size(12)
            .color(MUTED),
        controls,
    ]
    .spacing(16)
    .width(Fill)
    .height(Fill);
    let body = row![
        container(scrollable(inspector))
            .width(270)
            .height(Fill)
            .padding(22)
            .style(panel),
        display
    ]
    .spacing(22)
    .height(Fill);
    let mut footer = column![].spacing(4);
    if let Some(error) = app.session.fault() {
        footer = footer.push(
            text(error.to_string())
                .color(Color::from_rgb8(248, 131, 131))
                .size(13),
        );
    }
    if let Some(notice) = &app.notice {
        footer = footer.push(text(notice).size(13).color(MUTED));
    }
    if app.session.fault().is_none() && app.notice.is_none() {
        footer = footer.push(
            text("Rust inside. Desktop and browser outside.")
                .size(12)
                .color(MUTED),
        );
    }
    container(column![heading, body, footer].spacing(24))
        .padding(28)
        .width(Fill)
        .height(Fill)
        .into()
}

fn panel(_: &Theme) -> container::Style {
    container::Style {
        background: Some(PANEL.into()),
        border: Border {
            color: EDGE,
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    }
}
fn status_label(status: SessionStatus) -> &'static str {
    match status {
        SessionStatus::Ready => "Ready",
        SessionStatus::Running => "Running",
        SessionStatus::Paused => "Paused",
        SessionStatus::Exited(_) => "Program finished",
        SessionStatus::Halted => "CPU halted",
        SessionStatus::Faulted => "Execution stopped",
    }
}
