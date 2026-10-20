use super::{
    ACCENT, Action, Doslet, Icon, MUTED, Message, Panel, ghost, panel, primary, secondary,
};
use crate::configurator::Edit;
use doslet_runtime::session::MemoryUsage;
use iced::widget::{button, column, container, pick_list, row, rule, svg, text, tooltip};
use iced::{Element, Fill};

fn bytes(value: u64) -> String {
    if value >= 1024 * 1024 {
        format!("{:.1} MiB", value as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.0} KiB", value as f64 / 1024.0)
    }
}

pub(super) fn indicator(app: &Doslet) -> Element<'_, Message> {
    let m = app.memory_usage;
    let icon = svg(svg::Handle::from_memory(Icon::Ram.bytes()))
        .width(24)
        .height(24)
        .style(|_, _| svg::Style {
            color: Some(ACCENT),
        });
    let label = if m.installed_bytes == 0 {
        "— MiB".into()
    } else {
        format!("{} MiB", m.installed_bytes / (1024 * 1024))
    };
    let control = button(
        row![icon, text(label).size(12),]
            .spacing(9)
            .align_y(iced::Center),
    )
    .padding([6, 8])
    .style(ghost)
    .on_press(Message::Ui(Action::Open(Panel::Memory)));
    tooltip(
        control,
        container(
            text("Configured RAM capacity · click for DOS and DPMI allocation details").size(12),
        )
        .padding([6, 10])
        .style(panel),
        tooltip::Position::Top,
    )
    .into()
}

pub(super) fn details(app: &Doslet) -> Element<'_, Message> {
    let active = app.configurator.active.hardware.memory_mib;
    let draft = app.configurator.draft.hardware.memory_mib;
    let ready = !app.configurator.pending;
    let size: Element<'_, Message> = if ready {
        pick_list(&[4u8, 8, 16, 32][..], Some(draft), |value| {
            Message::Config(Edit::Memory(value))
        })
        .width(Fill)
        .into()
    } else {
        text(format!("{draft} MiB")).size(13).into()
    };
    column![
        text("Installed RAM (MiB)").size(13),
        size,
        text("Changing RAM restarts the machine. Export unsaved game changes first.")
            .size(12)
            .color(MUTED),
        row![
            button(text("Revert").size(13))
                .padding([8, 12])
                .style(secondary)
                .on_press_maybe(
                    (ready && draft != active).then_some(Message::Config(Edit::Memory(active)))
                ),
            button(text("Apply & restart").size(13))
                .padding([8, 12])
                .style(primary)
                .on_press_maybe((ready && draft != active).then_some(Message::Config(Edit::Apply))),
        ]
        .spacing(8),
        rule::horizontal(1),
        content(app.memory_usage),
    ]
    .spacing(10)
    .into()
}

fn content(m: MemoryUsage) -> Element<'static, Message> {
    if m.installed_bytes == 0 {
        return text("Waiting for memory information…").size(13).into();
    }
    let mut body = column![
        text(format!("{} installed RAM", bytes(m.installed_bytes))).size(15),
        text(if m.protected_mode {
            "CPU · Protected mode"
        } else {
            "CPU · Real mode"
        })
        .size(12)
        .color(MUTED),
        rule::horizontal(1),
        text("DOS conventional memory").size(13),
        text(format!(
            "Allocated {} · Available {}",
            bytes(m.dos_allocated),
            bytes(m.dos_free)
        ))
        .size(12),
        text(format!(
            "Largest free block {} · {} allocated blocks",
            bytes(m.dos_largest_free),
            m.dos_blocks
        ))
        .size(12)
        .color(MUTED),
    ]
    .spacing(10);
    body = body
        .push(rule::horizontal(1))
        .push(text("DPMI extended memory").size(13));
    if m.dpmi_available {
        body = body
            .push(
                text(if m.dpmi_entered {
                    "Built-in host · Client entered"
                } else {
                    "Built-in host · Available, not entered"
                })
                .size(12),
            )
            .push(
                text(format!(
                    "Pool {} · Allocated {} · Available {}",
                    bytes(m.dpmi_capacity),
                    bytes(m.dpmi_committed),
                    bytes(m.dpmi_capacity.saturating_sub(m.dpmi_committed))
                ))
                .size(12),
            )
            .push(
                text(format!(
                    "Requested {} · Peak allocated {} · {} blocks",
                    bytes(m.dpmi_requested),
                    bytes(m.dpmi_peak),
                    m.dpmi_blocks
                ))
                .size(12)
                .color(MUTED),
            );
    } else {
        body = body.push(text("Built-in host · Not exposed").size(12));
    }
    body.push(text("Allocation counters cover DOS and the built-in DPMI host. Memory managed by a guest extender is not measured; protected mode alone does not prove DPMI use. Counts exclude allocator headers and system reservations, and do not measure host RAM usage.").size(12).color(MUTED)).into()
}
