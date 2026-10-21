use super::sheet::{card_content, section_title, semibold, setting};
use super::{
    ACCENT, Action, Doslet, EDGE, Icon, MUTED, Message, Panel, ghost, panel, primary, secondary,
};
use crate::configurator::Edit;
use doslet_config::SourceKind;
#[cfg(target_arch = "wasm32")]
use iced::widget::pick_list;
use iced::widget::{
    button, checkbox, column, container, row, rule, scrollable, space, svg, text, text_input,
    tooltip,
};
use iced::{Border, Element, Fill};

pub(super) fn indicator(app: &Doslet) -> Element<'_, Message> {
    let reading = app.disk_activity.reading(
        app.session.status() == doslet_runtime::SessionStatus::Running
            || app.session.storage_waiting(),
        web_time::Instant::now(),
    );
    let color = if reading { ACCENT } else { EDGE };
    let lamp = container(space::horizontal())
        .width(5)
        .height(5)
        .style(move |_| container::Style {
            background: Some(color.into()),
            border: Border {
                radius: 3.0.into(),
                ..Default::default()
            },
            shadow: iced::Shadow {
                color: if reading {
                    iced::Color::from_rgba8(177, 224, 190, 0.3)
                } else {
                    iced::Color::TRANSPARENT
                },
                blur_radius: 6.0,
                ..Default::default()
            },
            ..Default::default()
        });
    let icon = svg(svg::Handle::from_memory(Icon::Disk.bytes()))
        .width(24)
        .height(24)
        .style(|_, _| svg::Style {
            color: Some(ACCENT),
        });
    tooltip(
        button(row![icon, lamp].spacing(5).align_y(iced::Center))
            .padding([6, 8])
            .style(ghost)
            .on_press(Message::Ui(Action::Open(Panel::Drives))),
        container(
            text(if reading {
                "Drives · Reading"
            } else {
                "Drives"
            })
            .size(12),
        )
        .padding([6, 10])
        .style(panel),
        tooltip::Position::Top,
    )
    .into()
}

pub(super) fn dialog(app: &Doslet, size: iced::Size) -> Element<'_, Message> {
    let narrow = size.width < 540.0;
    let config = &app.configurator;
    let changed = config.draft != config.active;
    let ready = !config.pending;
    let header = row![
        svg(svg::Handle::from_memory(Icon::Disk.bytes()))
            .width(24)
            .height(24)
            .style(|_, _| svg::Style {
                color: Some(ACCENT)
            }),
        text("Drives").size(20).font(semibold()),
        space::horizontal(),
        button(text("Close").size(13))
            .padding([8, 12])
            .style(ghost)
            .on_press(Message::Ui(Action::Close)),
    ]
    .spacing(12)
    .align_y(iced::Center);
    let mut body = column![system_drive(app, narrow)].spacing(14);
    let cd = config.draft.mounts.iter().find(|m| m.letter != b'C');
    if cd.is_some() {
        body = body.push(cdrom(app, narrow));
    } else {
        body = body.push(
            button(
                row![text("+").size(18), text("Add CD-ROM").size(13)]
                    .spacing(10)
                    .align_y(iced::Center),
            )
            .padding([12, 16])
            .width(Fill)
            .style(secondary)
            .on_press_maybe(ready.then_some(Message::Config(Edit::AddCdrom))),
        );
    }
    let mut footer = column![].spacing(10);
    if let Some(message) = app
        .notice
        .as_ref()
        .or(config.message.as_ref().filter(|_| changed && ready))
    {
        footer = footer.push(text(message).size(12).color(super::DANGER));
    }
    if changed && app.session.dirty_overlay() {
        footer = footer.push(
            checkbox(config.discard_changes)
                .label("Discard unsaved game changes")
                .size(14)
                .text_size(12)
                .on_toggle_maybe(ready.then_some(|v| Message::Config(Edit::DiscardChanges(v)))),
        );
    }
    let missing_folder = config
        .draft
        .mounts
        .iter()
        .find(|m| m.letter == b'C')
        .is_none_or(|m| config.draft.binding(&m.source).is_empty());
    let missing_program = config.draft.game.executable.is_empty();
    let missing_image = cd.is_some_and(|m| config.draft.binding(&m.source).is_empty());
    let incomplete = missing_folder || missing_program || missing_image;
    let actions = row![
        button(text("Revert").size(13))
            .padding([8, 12])
            .style(secondary)
            .on_press_maybe((changed && ready).then_some(Message::Config(Edit::Discard))),
        button(
            text(if ready {
                "Apply & restart"
            } else {
                "Preparing…"
            })
            .size(13)
        )
        .padding([8, 14])
        .style(primary)
        .on_press_maybe((changed && ready && !incomplete).then_some(Message::Config(Edit::Apply))),
    ]
    .spacing(8);
    let status = text(if config.pending {
        "Preparing…"
    } else if changed && missing_folder {
        "Choose game files"
    } else if changed && missing_program {
        "Choose a program"
    } else if changed && missing_image {
        "Choose a disc image"
    } else if changed {
        "Restart required"
    } else {
        "No changes"
    })
    .size(12)
    .color(MUTED);
    if narrow {
        footer = footer.push(status).push(actions);
    } else {
        footer = footer.push(row![status, space::horizontal(), actions].align_y(iced::Center));
    }
    container(
        column![
            header,
            scrollable(container(body).padding(iced::Padding {
                right: 8.0,
                ..Default::default()
            }))
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::default().width(4).scroller_width(4)
            ))
            .height(Fill),
            rule::horizontal(1),
            footer,
        ]
        .spacing(16),
    )
    .padding(if narrow { 16 } else { 22 })
    .width(Fill)
    .max_width(620)
    .height((size.height - 32.0).clamp(
        160.0,
        if narrow {
            540.0
        } else if cd.is_some() {
            480.0
        } else {
            370.0
        },
    ))
    .style(panel)
    .into()
}

fn system_drive(app: &Doslet, narrow: bool) -> Element<'_, Message> {
    let s = &app.configurator.draft;
    let c = s.mounts.iter().find(|m| m.letter == b'C');
    let source = c.map_or("", |m| s.binding(&m.source));
    let mut body = column![
        row![
            section_title("C:  Game files"),
            space::horizontal(),
            text("Startup drive").size(11).color(MUTED)
        ]
        .align_y(iced::Center)
    ]
    .spacing(14);
    #[cfg(not(target_arch = "wasm32"))]
    {
        body = body
            .push(setting(
                "Folder",
                text_input("Installation folder", source)
                    .on_input(|v| Message::Config(Edit::Root(v)))
                    .padding([7, 10])
                    .size(13)
                    .into(),
                narrow,
            ))
            .push(setting(
                "Program",
                text_input("Path to GAME.EXE", &s.game.executable)
                    .on_input(|v| Message::Config(Edit::Executable(v)))
                    .padding([7, 10])
                    .size(13)
                    .into(),
                narrow,
            ));
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = source;
        body = body.push(setting(
            "Game files",
            button(
                text(if app.archive.is_some() {
                    "Replace ZIP…"
                } else {
                    "Choose ZIP…"
                })
                .size(13),
            )
            .padding([8, 12])
            .style(secondary)
            .on_press_maybe((!app.importing).then_some(Message::OpenArchive))
            .into(),
            narrow,
        ));
        if let Some(archive) = &app.archive {
            body = body.push(setting(
                "Program",
                pick_list(
                    &archive.programs[..],
                    app.program.as_ref(),
                    Message::ProgramSelected,
                )
                .placeholder("Choose EXE / COM")
                .padding([7, 10])
                .text_size(13)
                .width(Fill)
                .into(),
                narrow,
            ));
        }
    }
    body = body.push(text(app.storage_label()).size(12).color(MUTED));
    card_content(body.into())
}

fn cdrom(app: &Doslet, narrow: bool) -> Element<'_, Message> {
    let config = &app.configurator;
    let s = &config.draft;
    let cd = s.mounts.iter().find(|m| m.letter != b'C').unwrap();
    let remote = cd.kind == SourceKind::HttpsIso;
    let title = format!("{}:  CD-ROM", char::from(cd.letter));
    let header = row![
        text(title).size(14).font(semibold()),
        space::horizontal(),
        button(text("Remove").size(12))
            .padding([4, 8])
            .style(ghost)
            .on_press_maybe((!config.pending).then_some(Message::Config(Edit::RemoveCdrom))),
    ]
    .align_y(iced::Center);
    let kinds = row![
        button(text("ISO file").size(13))
            .padding([7, 12])
            .width(Fill)
            .style(move |theme, status| super::segment(theme, status, !remote))
            .on_press(Message::Config(Edit::CdromKind(SourceKind::LocalIso))),
        button(text("HTTPS").size(13))
            .padding([7, 12])
            .width(Fill)
            .style(move |theme, status| super::segment(theme, status, remote))
            .on_press(Message::Config(Edit::CdromKind(SourceKind::HttpsIso))),
    ]
    .spacing(4);
    let mut body = column![header, setting("Source", kinds.into(), narrow)].spacing(14);
    #[cfg(target_arch = "wasm32")]
    if !remote {
        body = body.push(setting(
            "Disc image",
            button(
                text(if cd.source.is_empty() {
                    "Choose ISO…"
                } else {
                    "Replace ISO…"
                })
                .size(13),
            )
            .padding([8, 12])
            .style(secondary)
            .on_press(Message::Config(Edit::OpenIso))
            .into(),
            narrow,
        ));
    }
    if remote || !cfg!(target_arch = "wasm32") {
        body = body.push(setting(
            if remote { "URL" } else { "Disc image" },
            text_input(
                if remote {
                    "https://…/disc.iso"
                } else {
                    "Path to disc.iso"
                },
                s.binding(&cd.source),
            )
            .on_input(|v| Message::Config(Edit::CdromSource(v)))
            .padding([7, 10])
            .size(13)
            .into(),
            narrow,
        ));
    }
    body = body.push(
        text("Read only · Remove disconnects the image; files are kept.")
            .size(11)
            .color(MUTED),
    );
    card_content(body.into())
}
