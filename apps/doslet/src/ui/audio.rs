#[cfg(not(target_arch = "wasm32"))]
use super::sheet::semibold;
use super::sheet::{card, card_content, control, fixed_value, section_title, setting};
use super::{
    ACCENT, Action, AudioPage, Doslet, Icon, MUTED, Message, ghost, panel, primary, secondary,
    segment,
};
use crate::configurator::Edit;
use doslet_midi_routing::Mode;
#[cfg(not(target_arch = "wasm32"))]
use iced::Font;
use iced::widget::{
    button, checkbox, column, container, pick_list, progress_bar, row, rule, scrollable, slider,
    space, svg, text, toggler,
};
use iced::{Border, Color, Element, Fill};

/// A dedicated sheet keeps navigation and actions outside the scrolling form.
pub(super) fn dialog(app: &Doslet, size: iced::Size) -> Element<'_, Message> {
    let page = app.workspace.audio_page;
    let narrow = size.width < 540.0;
    let pending = app.configurator.pending;
    let title = text("Audio").size(20);
    #[cfg(not(target_arch = "wasm32"))]
    let title = title.font(semibold());
    let icon = container(
        svg(svg::Handle::from_memory(Icon::Audio.bytes()))
            .width(24)
            .height(24)
            .style(|_, _| svg::Style {
                color: Some(ACCENT),
            }),
    )
    .padding(8)
    .style(|_| container::Style {
        background: Some(Color::from_rgba8(177, 224, 190, 0.08).into()),
        border: Border {
            radius: 10.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });
    let header = row![
        icon,
        title,
        space::horizontal(),
        button(text("Close").size(13))
            .padding([8, 12])
            .style(ghost)
            .on_press(Message::Ui(Action::Close)),
    ]
    .spacing(12)
    .align_y(iced::Center);
    let tabs = container(
        row![
            button(text("Playback").size(13))
                .width(Fill)
                .padding([9, 12])
                .style(move |theme, status| segment(theme, status, page == AudioPage::Output))
                .on_press_maybe(
                    (!pending).then_some(Message::Ui(Action::AudioPage(AudioPage::Output)))
                ),
            button(
                row![
                    text("Hardware").size(13),
                    text(if app.configurator.draft != app.configurator.active {
                        "•"
                    } else {
                        ""
                    })
                    .size(13)
                    .color(ACCENT)
                ]
                .spacing(6)
                .align_y(iced::Center)
            )
            .width(Fill)
            .padding([9, 12])
            .style(move |theme, status| segment(theme, status, page == AudioPage::Hardware))
            .on_press_maybe(
                (!pending).then_some(Message::Ui(Action::AudioPage(AudioPage::Hardware)))
            ),
        ]
        .spacing(4),
    )
    .padding(4)
    .style(|_| container::Style {
        background: Some(super::INK.into()),
        border: Border {
            radius: 8.0.into(),
            ..Default::default()
        },
        ..Default::default()
    });
    let content = match page {
        AudioPage::Output => playback(app, narrow),
        AudioPage::Hardware => hardware(app, narrow),
    };
    let mut footer = column![].spacing(10);
    if let Some(notice) = &app.notice {
        footer = footer.push(
            container(text(notice).size(12).color(super::DANGER))
                .padding(10)
                .width(Fill)
                .style(card),
        );
    }
    if page == AudioPage::Hardware {
        footer = footer.push(hardware_actions(app, narrow));
    } else {
        let mut actions = row![].align_y(iced::Center).spacing(10);
        #[cfg(target_arch = "wasm32")]
        {
            actions = actions.push(
                button(text(app.audio_button_label()).size(13))
                    .padding([8, 12])
                    .style(secondary)
                    .on_press(Message::ToggleAudio),
            );
        }
        actions = actions.push(space::horizontal()).push(
            button(text("Done").size(13))
                .padding([8, 22])
                .style(primary)
                .on_press(Message::Ui(Action::Close)),
        );
        footer = footer.push(actions);
    }
    let preferred: f32 = match page {
        AudioPage::Hardware => {
            if narrow || app.session.dirty_overlay() {
                620.0
            } else {
                550.0
            }
        }
        AudioPage::Output => {
            if app.configurator.active.hardware.mpu401 {
                if narrow { 620.0 } else { 570.0 }
            } else if narrow {
                390.0
            } else {
                350.0
            }
        }
    };
    container(
        column![
            header,
            tabs,
            scrollable(container(content).padding(iced::Padding {
                right: 8.0,
                ..Default::default()
            }))
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::default().width(4).scroller_width(4),
            ))
            .height(Fill),
            rule::horizontal(1),
            footer,
        ]
        .spacing(14),
    )
    .padding(if narrow { 16 } else { 22 })
    .width(Fill)
    .max_width(600)
    .height(preferred.min((size.height - 32.0).max(160.0)))
    .style(panel)
    .into()
}

fn playback(app: &Doslet, narrow: bool) -> Element<'_, Message> {
    let h = app.configurator.active.hardware;
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let internal = app.audio_mixer == crate::audio::Mixer::Application;
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let internal = true;
    let mut output = column![section_title("Output")].spacing(14);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let switches = row![
            button(text("Internal").size(13))
                .padding([7, 12])
                .width(Fill)
                .style(move |theme, status| segment(theme, status, internal))
                .on_press(Message::AudioMixerSelected(
                    crate::audio::Mixer::Application
                )),
            button(text("System").size(13))
                .padding([7, 12])
                .width(Fill)
                .style(move |theme, status| segment(theme, status, !internal))
                .on_press(Message::AudioMixerSelected(crate::audio::Mixer::System)),
        ]
        .spacing(4);
        output = output.push(setting("Mixer", switches.into(), narrow));
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        output = output.push(setting("Mixer", text("Internal").size(13).into(), narrow));
    }
    if internal {
        let volume = text(format!("{}%", app.audio_volume)).size(12).width(40);
        #[cfg(not(target_arch = "wasm32"))]
        let volume = volume.font(Font::MONOSPACE);
        output = output.push(setting(
            "Volume",
            row![
                slider(0..=100, app.audio_volume, Message::AudioVolume).step(1),
                volume,
            ]
            .spacing(14)
            .align_y(iced::Center)
            .into(),
            narrow,
        ));
    } else {
        output = output.push(
            text("Adjust levels in your system mixer.")
                .size(12)
                .color(MUTED),
        );
    }
    let mut body = column![card_content(output.into())].spacing(14);
    if !h.sound().has_fm() && !h.mpu401 {
        body = body.push(
            text("No sound hardware enabled. Add a device in Hardware.")
                .size(12)
                .color(MUTED),
        );
    }
    if h.mpu401 {
        body = body.push(midi(app, narrow));
        body = body.push(midi_channels(app));
    }
    body.into()
}

fn midi_channels(app: &Doslet) -> Element<'_, Message> {
    let headings = text("Ch / instrument            Notes  Vel  Vol  Expr  Sus")
        .size(11)
        .color(MUTED);
    #[cfg(not(target_arch = "wasm32"))]
    let headings = headings.font(Font::MONOSPACE);
    let mut body = column![
        section_title("MIDI channels"),
        text("Routed MIDI state · bars estimate note level from velocity, volume and expression; receiver audio is not measured.")
            .size(11).color(MUTED),
        headings,
    ].spacing(8);
    for (index, channel) in app.midi_channels.iter().enumerate() {
        let state = channel.state;
        let name = String::from_utf8_lossy(&channel.instrument);
        let name = name.trim_matches(char::from(0)).trim();
        let instrument = if !channel.enabled {
            "Unassigned".to_owned()
        } else if !name.is_empty() {
            name.to_owned()
        } else if index == 9 {
            format!("Drum kit {}", u16::from(state.program) + 1)
        } else {
            format!("Program {}", u16::from(state.program) + 1)
        };
        let level =
            if channel.enabled && app.session.status() == doslet_runtime::SessionStatus::Running {
                f32::from(state.velocity) * f32::from(state.volume) * f32::from(state.expression)
                    / (127.0 * 127.0 * 127.0)
            } else {
                0.0
            };
        let stats = text(format!(
            "{:3}  {:3}  {:3}  {:3}   {}",
            state.notes,
            state.velocity,
            state.volume,
            state.expression,
            if state.sustain { "●" } else { "·" }
        ))
        .size(11);
        #[cfg(not(target_arch = "wasm32"))]
        let stats = stats.font(Font::MONOSPACE);
        body = body.push(
            column![
                row![
                    text(format!("{:02}  {instrument}", index + 1))
                        .size(11)
                        .width(Fill),
                    stats,
                ]
                .spacing(8),
                progress_bar(0.0..=1.0, level).girth(3),
            ]
            .spacing(3),
        );
    }
    card_content(body.into())
}

fn midi(app: &Doslet, narrow: bool) -> Element<'_, Message> {
    let header = row![
        section_title("MIDI"),
        space::horizontal(),
        button(text("Refresh outputs").size(12))
            .padding([4, 8])
            .style(ghost)
            .on_press(Message::RefreshMidi),
    ]
    .align_y(iced::Center);
    let output = pick_list(
        &app.midi_choices[..],
        Some(&app.midi_choice),
        Message::MidiSelected,
    )
    .width(Fill)
    .text_size(13)
    .padding([7, 10]);
    let game = pick_list(
        &Mode::ALL[..],
        Some(app.midi_mode),
        Message::MidiModeSelected,
    )
    .width(Fill)
    .text_size(13)
    .padding([7, 10]);
    let built_in = matches!(
        app.midi_choice.as_str(),
        "Rust synth" | "Apple DLS synth" | "CM-32L / MT-32" | "moont"
    );
    let destination: Element<'_, Message> = if built_in {
        let label = if matches!(app.midi_choice.as_str(), "CM-32L / MT-32" | "moont") {
            if app.midi_mode == Mode::Mt32 {
                "MT-32"
            } else {
                "GM on CM-32L"
            }
        } else {
            "General MIDI"
        };
        fixed_value(label)
    } else {
        pick_list(
            &Mode::ALL[..],
            Some(app.midi_output_mode),
            Message::MidiOutputModeSelected,
        )
        .width(Fill)
        .text_size(13)
        .padding([7, 10])
        .into()
    };
    let mut body = column![
        header,
        setting("Output", output.into(), narrow),
        setting("Game format", game.into(), narrow),
        setting("Receiver format", destination, narrow),
    ]
    .spacing(12);
    if built_in
        && !matches!(app.midi_choice.as_str(), "CM-32L / MT-32" | "moont")
        && app.midi_mode == Mode::Mt32
    {
        body = body.push(
            text("MT-32 messages are translated to General MIDI.")
                .size(12)
                .color(MUTED),
        );
    } else if !built_in {
        body = body.push(
            text("Volume is controlled by the MIDI receiver.")
                .size(12)
                .color(MUTED),
        );
    }
    card_content(body.into())
}

fn hardware(app: &Doslet, narrow: bool) -> Element<'_, Message> {
    let config = &app.configurator;
    let h = config.draft.hardware;
    let sound = h.sound();
    let ready = !config.pending;
    let subtitle = match sound {
        doslet_config::SoundCard::None => "Disabled",
        doslet_config::SoundCard::Adlib => "OPL2 FM synthesis · 388h",
        doslet_config::SoundCard::Sb => "8-bit digital audio · OPL2 FM",
        doslet_config::SoundCard::Sb16 => "16-bit digital audio · OPL3 FM",
    };
    let card_picker: Element<'_, Message> = if ready {
        pick_list(&doslet_config::SoundCard::ALL[..], Some(sound), |v| {
            Message::Config(Edit::SoundCard(v))
        })
        .text_size(13)
        .padding([7, 10])
        .width(if narrow { Fill } else { 180.into() })
        .into()
    } else {
        fixed_value(match sound {
            doslet_config::SoundCard::None => "Off",
            doslet_config::SoundCard::Adlib => "AdLib",
            doslet_config::SoundCard::Sb => "Sound Blaster",
            doslet_config::SoundCard::Sb16 => "Sound Blaster 16",
        })
    };
    let header: Element<'_, Message> = if narrow {
        column![
            column![section_title("Sound card"), text(subtitle).size(12).color(MUTED)].spacing(4),
            card_picker,
        ]
        .spacing(10)
        .into()
    } else {
        row![
            column![section_title("Sound card"), text(subtitle).size(12).color(MUTED)].spacing(4),
            space::horizontal(),
            card_picker,
        ]
        .spacing(12)
        .align_y(iced::Center)
        .into()
    };
    let mut sb = column![header].spacing(16);
    match sound {
        doslet_config::SoundCard::None => {}
        doslet_config::SoundCard::Adlib => {
            sb = sb.push(setting("I/O port", fixed_value("388h · fixed"), narrow));
        }
        doslet_config::SoundCard::Sb | doslet_config::SoundCard::Sb16 => {
            let port = control("I/O port", fixed_value("220h"));
            let irq = control(
                "IRQ",
                pick_list(&[5u8, 7][..], Some(h.sb_irq), |v| {
                    Message::Config(Edit::Irq(v))
                })
                .text_size(13)
                .padding([7, 10])
                .width(Fill)
                .into(),
            );
            let dma8 = control(
                "DMA 8-bit",
                pick_list(&[0u8, 1, 3][..], Some(h.sb_dma8), |v| {
                    Message::Config(Edit::Dma8(v))
                })
                .text_size(13)
                .padding([7, 10])
                .width(Fill)
                .into(),
            );
            if sound.has_dma16() {
                let dma16 = control(
                    "DMA 16-bit",
                    pick_list(&[5u8, 6, 7][..], Some(h.sb_dma16), |v| {
                        Message::Config(Edit::Dma16(v))
                    })
                    .text_size(13)
                    .padding([7, 10])
                    .width(Fill)
                    .into(),
                );
                if narrow {
                    sb = sb.push(
                        column![row![port, irq].spacing(12), row![dma8, dma16].spacing(12)]
                            .spacing(12),
                    );
                } else {
                    sb = sb.push(row![port, irq, dma8, dma16].spacing(12));
                }
                sb = sb.push(
                    text("Fixed ports: SB16 220h · OPL3 220h / 388h")
                        .size(11)
                        .color(MUTED),
                );
            } else {
                if narrow {
                    sb = sb.push(column![row![port, irq].spacing(12), dma8].spacing(12));
                } else {
                    sb = sb.push(row![port, irq, dma8].spacing(12));
                }
                sb = sb.push(
                    text("Fixed ports: SB 220h · OPL2 220h / 388h")
                        .size(11)
                        .color(MUTED),
                );
            }
        }
    }
    let mut mpu = column![device_header(
        "MPU-401",
        "UART MIDI interface",
        h.mpu401,
        ready.then_some(|v| Message::Config(Edit::Mpu(v))),
    )]
    .spacing(14);
    if h.mpu401 {
        mpu = mpu.push(setting("I/O port", fixed_value("330h · fixed"), narrow));
    }
    sb = sb.push(
        text(if app.audio_use.sb16 || app.audio_use.opl3 {
            "Accessed by guest · hardware changes require restart"
        } else {
            "Unused · hardware can be changed live"
        })
        .size(11)
        .color(MUTED),
    );
    mpu = mpu.push(
        text(if app.audio_use.mpu401 {
            "Accessed by guest · hardware changes require restart"
        } else {
            "Unused · hardware can be changed live"
        })
        .size(11)
        .color(MUTED),
    );
    column![card_content(sb.into()), card_content(mpu.into())]
        .spacing(14)
        .into()
}

fn hardware_actions(app: &Doslet, narrow: bool) -> Element<'_, Message> {
    let config = &app.configurator;
    let changed = config.draft != config.active;
    let blocker = app.audio_use.change_blocker(
        config.active.hardware.audio(),
        config.draft.hardware.audio(),
    );
    let live = config.audio_only_changes()
        && blocker.is_none()
        && !doslet_runtime::Session::audio_environment_requires_restart(
            config.active.hardware.audio(),
            config.draft.hardware.audio(),
        );
    let mut footer = column![].spacing(10);
    if !config.pending
        && let Some(message) = &config.message
    {
        footer =
            footer.push(
                text(message)
                    .size(12)
                    .color(if changed { super::DANGER } else { MUTED }),
            );
    }
    if changed && !live && app.session.dirty_overlay() {
        footer = footer.push(
            checkbox(config.discard_changes)
                .label("Discard unsaved game changes")
                .size(14)
                .text_size(12)
                .on_toggle_maybe(
                    (!config.pending).then_some(|v| Message::Config(Edit::DiscardChanges(v))),
                ),
        );
    }
    let status = if config.pending {
        "Preparing…"
    } else if live {
        "Unused hardware · no restart needed"
    } else if changed {
        "Restart required"
    } else {
        "No changes"
    };
    let actions = row![
        button(text("Revert").size(13))
            .padding([8, 12])
            .style(secondary)
            .on_press_maybe((changed && !config.pending).then_some(Message::Config(Edit::Discard))),
        button(
            text(if config.pending {
                "Preparing…"
            } else if live {
                "Apply"
            } else {
                "Apply & restart"
            })
            .size(13)
        )
        .padding([8, 14])
        .style(primary)
        .on_press_maybe(
            (changed && !config.pending).then_some(Message::Config(if live {
                Edit::ApplyAudio
            } else {
                Edit::Apply
            }))
        ),
    ]
    .spacing(8);
    if narrow {
        footer = footer
            .push(text(status).size(12).color(MUTED))
            .push(actions);
    } else {
        footer = footer.push(
            row![
                text(status).size(12).color(MUTED),
                space::horizontal(),
                actions
            ]
            .align_y(iced::Center),
        );
    }
    footer.into()
}

fn device_header<'a>(
    title: &'static str,
    subtitle: &'static str,
    enabled: bool,
    change: Option<impl Fn(bool) -> Message + 'a>,
) -> Element<'a, Message> {
    row![
        column![section_title(title), text(subtitle).size(12).color(MUTED)].spacing(4),
        space::horizontal(),
        toggler(enabled).size(18).on_toggle_maybe(change),
    ]
    .spacing(12)
    .align_y(iced::Center)
    .into()
}
