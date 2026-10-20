use super::sheet::{card_content, fixed_value, section_title, semibold, setting};
use super::{ACCENT, Action, Doslet, Icon, MUTED, Message, Panel, ghost, panel};
use crate::configurator::Edit;
use doslet_config::{KeyboardMode, MouseMode};
use iced::widget::{
    button, column, container, pick_list, row, rule, scrollable, space, svg, text, toggler, tooltip,
};
use iced::{Element, Fill};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keyboard {
    Bios,
    At,
}
impl std::fmt::Display for Keyboard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Bios => "BIOS · compatibility",
            Self::At => "AT · physical keys",
        })
    }
}

fn icon() -> iced::widget::Svg<'static> {
    svg(svg::Handle::from_memory(Icon::Input.bytes()))
        .width(24)
        .height(24)
        .style(|_, _| svg::Style {
            color: Some(ACCENT),
        })
}

pub(super) fn indicator() -> Element<'static, Message> {
    tooltip(
        button(icon())
            .padding([6, 8])
            .style(ghost)
            .on_press(Message::Ui(Action::Open(Panel::Input))),
        container(text("Mouse, keyboard & joystick").size(12))
            .padding([6, 10])
            .style(panel),
        tooltip::Position::Top,
    )
    .into()
}

pub(super) fn dialog(app: &Doslet, size: iced::Size) -> Element<'_, Message> {
    let narrow = size.width < 540.0;
    let config = &app.configurator.active;
    #[cfg(not(target_arch = "wasm32"))]
    let debugging = app.gdb.is_some_and(|(_, connected)| connected);
    #[cfg(target_arch = "wasm32")]
    let debugging = false;
    let ready = !app.configurator.pending && !debugging;
    let mode = config.input.mouse_mode;
    let capture = if ready {
        pick_list(&MouseMode::ALL[..], Some(mode), |v| {
            Message::Config(Edit::MouseMode(v))
        })
        .width(Fill)
        .into()
    } else {
        fixed_value(match mode {
            MouseMode::Auto => "Auto",
            MouseMode::Capture => "Capture",
            MouseMode::NoCapture => "No capture",
        })
    };
    let mut mouse = column![
        section_title("Mouse"),
        setting("Capture", capture, narrow),
        text(match mode {
            MouseMode::Auto => "Follows the game's mouse input.",
            MouseMode::Capture => "Click the game to capture relative movement.",
            MouseMode::NoCapture => "Use the pointer without capturing it.",
        })
        .size(12)
        .color(MUTED),
    ]
    .spacing(14);
    if mode != MouseMode::Capture {
        mouse = mouse.push(rule::horizontal(1)).push(
            toggler(config.input.hide_cursor)
                .label("Hide system cursor over the game")
                .text_size(13)
                .on_toggle_maybe(ready.then_some(|v| Message::Config(Edit::HideCursor(v)))),
        );
    }
    let keyboard = if config.hardware.keyboard == KeyboardMode::At {
        Keyboard::At
    } else {
        Keyboard::Bios
    };
    let delivery = if ready {
        pick_list(&[Keyboard::Bios, Keyboard::At][..], Some(keyboard), |v| {
            Message::Config(Edit::Keyboard(v == Keyboard::At))
        })
        .width(Fill)
        .into()
    } else {
        fixed_value(match keyboard {
            Keyboard::At => "AT · physical keys",
            Keyboard::Bios => "BIOS · compatibility",
        })
    };
    let keyboard = column![
        section_title("Keyboard"),
        setting("Delivery", delivery, narrow),
        text(match keyboard {
            Keyboard::Bios => "BIOS scan codes and text input.",
            Keyboard::At => "Physical key positions, press and release events.",
        })
        .size(12)
        .color(MUTED),
    ]
    .spacing(14);
    let joystick = column![
        section_title("Joystick"),
        toggler(config.hardware.gameport)
            .label("Emulate PC gameport (port 201h)")
            .text_size(13)
            .on_toggle_maybe(ready.then_some(|v| Message::Config(Edit::Gameport(v)))),
    ]
    .spacing(10);
    let header = row![
        icon(),
        text("Input").size(20).font(semibold()),
        space::horizontal(),
        button(text("Close").size(13))
            .padding([8, 12])
            .style(ghost)
            .on_press(Message::Ui(Action::Close))
    ]
    .spacing(12)
    .align_y(iced::Center);
    let mut footer = column![
        text(if debugging {
            "Detach GDB to change input settings"
        } else if !ready {
            "Preparing machine…"
        } else {
            "Changes apply immediately"
        })
        .size(12)
        .color(MUTED)
    ]
    .spacing(8);
    if let Some(notice) = &app.notice {
        footer = footer.push(text(notice).size(12).color(super::DANGER));
    }
    let mut cards = column![
        card_content(mouse.into()),
        card_content(keyboard.into()),
        card_content(joystick.into()),
    ]
    .spacing(14);
    if mode != MouseMode::NoCapture {
        cards = cards.push(
            text(if cfg!(target_arch = "wasm32") {
                "Esc releases the mouse"
            } else {
                "Ctrl + Esc releases the mouse"
            })
            .size(12)
            .color(MUTED),
        );
    }
    container(
        column![
            header,
            scrollable(container(cards).padding(iced::Padding {
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
    .max_width(580)
    .width(Fill)
    .height((size.height - 32.0).clamp(160.0, if narrow { 580.0 } else { 540.0 }))
    .style(panel)
    .into()
}
