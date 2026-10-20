#[cfg(not(target_arch = "wasm32"))]
use super::sheet::semibold;
use super::sheet::{card_content, section_title, setting};
use super::{ACCENT, Action, Doslet, Icon, MUTED, Message, ghost, panel, primary, secondary};
use crate::cpu_config::{Core, Edit, JIT_AVAILABLE, Mode};
#[cfg(not(target_arch = "wasm32"))]
use iced::Font;
use iced::widget::{
    button, column, container, pick_list, row, rule, scrollable, slider, space, svg, text,
};
use iced::{Element, Fill};

fn message(edit: Edit) -> Message {
    Message::Cpu(edit)
}

pub(super) fn dialog(app: &Doslet, size: iced::Size) -> Element<'_, Message> {
    let narrow = size.width < 540.0;
    let draft = app.workspace.cpu.unwrap_or(app.cpu_settings);
    let changed = draft != app.cpu_settings;
    #[cfg(not(target_arch = "wasm32"))]
    let ready = !app.configurator.pending && !app.gdb.is_some_and(|(_, connected)| connected);
    #[cfg(target_arch = "wasm32")]
    let ready = !app.configurator.pending;
    let title = text("CPU").size(20);
    #[cfg(not(target_arch = "wasm32"))]
    let title = title.font(semibold());
    let header = row![
        svg(svg::Handle::from_memory(Icon::Cpu.bytes()))
            .width(24)
            .height(24)
            .style(|_, _| svg::Style {
                color: Some(ACCENT)
            }),
        title,
        space::horizontal(),
        button(text("Close").size(13))
            .padding([8, 12])
            .style(ghost)
            .on_press(Message::Ui(Action::Close)),
    ]
    .spacing(12)
    .align_y(iced::Center);
    #[cfg(not(target_arch = "wasm32"))]
    let modes = &[Mode::Fixed, Mode::Tiers, Mode::Adaptive][..];
    #[cfg(target_arch = "wasm32")]
    let modes = if app.cpu_tiers_available {
        &[Mode::Fixed, Mode::Tiers, Mode::Adaptive][..]
    } else {
        &[Mode::Fixed, Mode::Adaptive][..]
    };
    // Installed procedures stay for the session and keep the JIT idle.
    let cores = if app.cpu_settings.core == Core::Aot {
        &[Core::Aot][..]
    } else if JIT_AVAILABLE {
        &[Core::Interpreter, Core::Jit][..]
    } else {
        &[Core::Interpreter][..]
    };
    let controls = column![
        setting(
            "Operating mode",
            pick_list(modes, Some(draft.mode), |v| message(Edit::Mode(v)))
                .width(Fill)
                .into(),
            narrow
        ),
        setting(
            "Execution core",
            pick_list(cores, Some(draft.core), |v| message(Edit::Core(v)))
                .width(Fill)
                .into(),
            narrow
        ),
    ]
    .spacing(14);
    let mut body = column![card_content(controls.into())].spacing(14);
    if draft.mode == Mode::Tiers {
        body = body.push(card_content(
            column![
                section_title("Automatic speed tiers"),
                text("Applies fast execution to game work and throttled clocks to busy-wait loops.")
                    .size(12)
                    .color(MUTED),
            ]
            .spacing(8)
            .into(),
        ));
    } else {
        let mut speed = column![
            section_title(if draft.mode == Mode::Adaptive {
                "Speed ceiling"
            } else {
                "Target speed"
            }),
            rate("CPU", draft.mhz, ACCENT, Edit::Mhz),
        ]
        .spacing(12);
        let mut presets = row![].spacing(6);
        for mhz in [16, 33, 66, 100, 133, 266] {
            presets = presets.push(
                button(text(mhz.to_string()).size(12))
                    .padding([5, 10])
                    .style(if draft.mhz == mhz { primary } else { secondary })
                    .on_press(message(Edit::Mhz(mhz))),
            );
        }
        speed = speed.push(presets.wrap());
        if draft.mode == Mode::Adaptive {
            speed = speed.push(
                text("Adjusts throughput automatically from guest synchronization episodes.")
                    .size(12)
                    .color(MUTED),
            );
        }
        body = body.push(card_content(speed.into()));
    }
    let mut footer = column![].spacing(10);
    if let Some(notice) = &app.notice {
        footer = footer.push(text(notice).size(12).color(super::DANGER));
    }
    let status = text(if ready {
        "Applies without restarting"
    } else {
        "Controls temporarily unavailable"
    })
    .size(12)
    .color(MUTED);
    let actions = row![
        button(text("Revert").size(13))
            .padding([8, 12])
            .style(secondary)
            .on_press_maybe(changed.then_some(message(Edit::Revert))),
        button(text("Apply").size(13))
            .padding([8, 18])
            .style(primary)
            .on_press_maybe((changed && ready && draft.valid()).then_some(message(Edit::Apply))),
    ]
    .spacing(8);
    footer = if size.width < 390.0 {
        footer.push(status).push(actions)
    } else {
        footer.push(row![status, space::horizontal(), actions].align_y(iced::Center))
    };
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
    .max_width(580)
    .height((size.height - 32.0).clamp(160.0, if narrow { 460.0 } else { 420.0 }))
    .style(panel)
    .into()
}

fn rate(
    label: &'static str,
    mhz: u16,
    color: iced::Color,
    edit: fn(u16) -> Edit,
) -> Element<'static, Message> {
    // A logarithmic track leaves useful room for DOS-era clocks and still reaches 1 GHz.
    let position = (mhz as f32).log10() / 3.0;
    let value = text(format!("{mhz} MHz")).size(14).color(color);
    #[cfg(not(target_arch = "wasm32"))]
    let value = value.font(Font::MONOSPACE);
    column![
        row![
            text(label).size(13).color(color),
            space::horizontal(),
            value
        ],
        slider(0.0..=1.0, position, move |v| message(edit(
            10.0_f32.powf(v * 3.0).round() as u16
        )))
        .step(0.001_f32),
    ]
    .spacing(6)
    .into()
}
