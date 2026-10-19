#[cfg(not(target_arch = "wasm32"))]
use super::sheet::semibold;
use super::sheet::{card_content, section_title, setting};
use super::{
    ACCENT, Action, COLD, Doslet, HOT, Icon, MUTED, Message, ghost, panel, primary, secondary,
};
use crate::cpu_config::{Core, Edit, JIT_AVAILABLE, Mode, Settings};
#[cfg(not(target_arch = "wasm32"))]
use iced::Font;
use iced::widget::{
    button, checkbox, column, container, pick_list, row, rule, scrollable, slider, space, svg,
    text, toggler, tooltip,
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
    let profile_form_open = app.workspace.cpu_profile_form.is_some();
    #[cfg(target_arch = "wasm32")]
    let profile_form_open = false;
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
    let mut controls = column![
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
    #[cfg(not(target_arch = "wasm32"))]
    {
        controls = controls
            .push(rule::horizontal(1))
            .push(profile_controls(app, ready, narrow));
    }
    if draft.mode == Mode::Tiers {
        #[cfg(not(target_arch = "wasm32"))]
        let learning_available = app.cpu_learning_available;
        #[cfg(target_arch = "wasm32")]
        let learning_available = false;
        let learning = tooltip(
            toggler(draft.learning)
                .label("Background learning")
                .text_size(13)
                .on_toggle_maybe(
                    (learning_available || draft.learning)
                        .then_some(|v| message(Edit::Learning(v))),
                )
                .width(Fill),
            container(
                text(if learning_available {
                    "Classify unknown code using background replays. Off by default."
                } else if cfg!(target_arch = "wasm32") {
                    "Browser learning is unavailable; this site uses native labels."
                } else {
                    "Requires local media without a persistent storage lease."
                })
                .size(12),
            )
            .padding(10)
            .max_width(320)
            .style(panel),
            tooltip::Position::Bottom,
        );
        controls = controls.push(rule::horizontal(1)).push(learning);
        #[cfg(target_arch = "wasm32")]
        {
            controls = controls.push(
                text("Native ROTT labels bundled with this site; no browser learning.")
                    .size(12)
                    .color(MUTED),
            );
        }
    }
    let mut body = column![card_content(controls.into())].spacing(14);
    if draft.mode == Mode::Tiers {
        body = body.push(tiers(draft));
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
        for mhz in [33, 66, 100, 133, 266] {
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
                text("Adjusts throughput from synchronization episodes. Experimental.")
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
    let status = text(if profile_form_open {
        "Complete profile setup above"
    } else if ready {
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
            .on_press_maybe(
                (changed && ready && !profile_form_open && draft.valid())
                    .then_some(message(Edit::Apply))
            ),
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
    .max_width(620)
    .height((size.height - 32.0).clamp(
        160.0,
        if draft.mode == Mode::Tiers {
            660.0
        } else if narrow {
            if cfg!(target_arch = "wasm32") {
                530.0
            } else {
                610.0
            }
        } else {
            if profile_form_open {
                610.0
            } else if cfg!(target_arch = "wasm32") {
                420.0
            } else {
                500.0
            }
        },
    ))
    .style(panel)
    .into()
}

#[cfg(not(target_arch = "wasm32"))]
fn profile_controls(app: &Doslet, ready: bool, narrow: bool) -> Element<'_, Message> {
    use crate::cpu_config::ProfileEdit;
    use iced::widget::text_input;
    let action = Message::CpuProfile;
    let saved = app.cpu_profile.as_ref().is_some_and(|p| p.path.is_some());
    if let Some(form) = &app.workspace.cpu_profile_form {
        let mut content = column![
            text(if form.open {
                "Open speed-tier profile"
            } else if saved {
                "Save profile as"
            } else {
                "Create speed-tier profile"
            })
            .size(13)
            .font(semibold()),
            text_input("Path to a JSON file", &form.path)
                .on_input(move |v| action(ProfileEdit::Path(v)))
                .on_submit(action(ProfileEdit::Submit))
                .padding(9)
                .size(12),
            text(if form.open {
                "Loads labels and saved speeds. Learning stays off."
            } else {
                "Saves labels and current tier speeds. Learning stays off."
            })
            .size(12)
            .color(MUTED),
        ]
        .spacing(10);
        if let Some(error) = &form.error {
            content = content.push(text(error).size(12).color(super::DANGER));
        }
        return content
            .push(
                row![
                    space::horizontal(),
                    button(text("Cancel").size(12))
                        .padding([6, 10])
                        .style(secondary)
                        .on_press(action(ProfileEdit::Cancel)),
                    button(
                        text(if form.open {
                            "Open & use"
                        } else if saved {
                            "Save & use"
                        } else {
                            "Create & use"
                        })
                        .size(12)
                    )
                    .padding([6, 10])
                    .style(primary)
                    .on_press_maybe(
                        (ready && !form.path.trim().is_empty())
                            .then_some(action(ProfileEdit::Submit))
                    ),
                ]
                .spacing(8)
                .wrap(),
            )
            .into();
    }
    let status = app.cpu_profile.as_ref().map_or_else(
        || "No profile".into(),
        |p| {
            format!(
                "{} · {} labels",
                if p.path.is_some() {
                    "Profile"
                } else {
                    "Session profile"
                },
                p.labels
            )
        },
    );
    let status: Element<'_, Message> =
        if let Some(path) = app.cpu_profile.as_ref().and_then(|p| p.path.as_ref()) {
            tooltip(
                text(status).size(12).color(MUTED),
                container(text(path).size(12))
                    .padding(10)
                    .max_width(480)
                    .style(panel),
                tooltip::Position::Bottom,
            )
            .into()
        } else {
            text(status).size(12).color(MUTED).into()
        };
    let actions = row![
        button(
            text(if saved {
                "Save profile as…"
            } else {
                "Create profile"
            })
            .size(12)
        )
        .padding([6, 10])
        .style(secondary)
        .on_press_maybe(ready.then_some(action(ProfileEdit::New))),
        button(text("Open profile…").size(12))
            .padding([6, 10])
            .style(secondary)
            .on_press_maybe(ready.then_some(action(ProfileEdit::Open))),
    ]
    .spacing(8)
    .align_y(iced::Center);
    if narrow {
        column![status, actions.wrap()].spacing(8).into()
    } else {
        row![status, space::horizontal(), actions]
            .spacing(8)
            .align_y(iced::Center)
            .into()
    }
}

fn tiers(draft: Settings) -> Element<'static, Message> {
    let mut content = column![
        row![
            section_title("Speed ranges"),
            space::horizontal(),
            text("1–1000 MHz").size(11).color(MUTED)
        ]
        .align_y(iced::Center),
        rate("Grey · reference", draft.grey, MUTED, Edit::Grey),
        text("Reference for CPU activity and IRQ code.")
            .size(12)
            .color(MUTED),
        rule::horizontal(1),
    ]
    .spacing(12);
    if let Some(hot) = draft.hot {
        content = content.push(rate("Hot · turbo", hot, HOT, Edit::Hot));
    } else {
        let max = text("MAX").size(14).color(HOT);
        #[cfg(not(target_arch = "wasm32"))]
        let max = max.font(Font::MONOSPACE);
        content = content.push(row![
            text("Hot · turbo").size(13).color(HOT),
            space::horizontal(),
            max
        ]);
    }
    content = content
        .push(
            checkbox(draft.hot.is_none())
                .label("Uncapped Hot speed")
                .size(14)
                .text_size(12)
                .on_toggle(|v| message(Edit::Uncapped(v))),
        )
        .push(rate("Cold", draft.cold, COLD, Edit::Cold));
    card_content(content.into())
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
