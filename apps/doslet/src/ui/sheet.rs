use super::{EDGE, FOREGROUND, MUTED, Message};
use iced::widget::{column, container, row, text};
use iced::{Border, Color, Element, Fill, Font, Theme};
pub(super) fn setting<'a>(
    label: &'static str,
    value: Element<'a, Message>,
    narrow: bool,
) -> Element<'a, Message> {
    if narrow {
        column![text(label).size(12).color(MUTED), value]
            .spacing(6)
            .into()
    } else {
        row![
            text(label).size(13).color(MUTED).width(132),
            container(value).width(Fill)
        ]
        .spacing(16)
        .align_y(iced::Center)
        .into()
    }
}

pub(super) fn control<'a>(
    label: &'static str,
    value: Element<'a, Message>,
) -> Element<'a, Message> {
    column![text(label).size(11).color(MUTED), value]
        .spacing(6)
        .width(Fill)
        .into()
}

pub(super) fn fixed_value(value: &'static str) -> Element<'static, Message> {
    let value = text(value).size(13).color(FOREGROUND);
    #[cfg(not(target_arch = "wasm32"))]
    let value = value.font(Font::MONOSPACE);
    container(value)
        .padding([8, 10])
        .width(Fill)
        .style(|_| container::Style {
            background: Some(super::INK.into()),
            border: Border {
                radius: 5.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
}

pub(super) fn section_title(value: &'static str) -> Element<'static, Message> {
    let title = text(value).size(14);
    #[cfg(not(target_arch = "wasm32"))]
    let title = title.font(semibold());
    title.into()
}

pub(super) fn semibold() -> Font {
    #[cfg(target_arch = "wasm32")]
    return Font::DEFAULT;
    #[cfg(not(target_arch = "wasm32"))]
    Font {
        weight: iced::font::Weight::Semibold,
        ..Font::DEFAULT
    }
}

pub(super) fn card_content(value: Element<'_, Message>) -> Element<'_, Message> {
    container(value).padding(16).width(Fill).style(card).into()
}

pub(super) fn card(_: &Theme) -> container::Style {
    container::Style {
        background: Some(Color::from_rgba8(30, 32, 37, 0.88).into()),
        border: Border {
            color: EDGE,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}
