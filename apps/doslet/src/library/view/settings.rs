//! Global settings: the defaults every game starts from, laid out like the
//! game configuration window. Each change is saved at once.
use super::config::{
    SIMPLE_FILTERS, card, dpmi_label, field, filter_hint, filter_label, frame_limit_label, group,
    hint, midi_label, mouse_label,
};
use super::{
    Alignment, Element, Fill, Filter, GlobalEdit, Library, MUTED, Message, PAGE_PADDING, bold,
    chip_style, filters, nav_style, notice_bar, secondary_button_style, semibold, tr,
};
use crate::library::{PresetEdit, SettingsPage};
use doslet_config::{CpuMode, DpmiHost, HardwarePreset, KeyboardMode, MouseMode, VideoFilter};
use iced::widget::{
    button, column, container, pick_list, row, rule, scrollable, slider, space, text, text_input,
};

impl core::fmt::Display for SettingsPage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Machine => "Machine",
            Self::Presets => "Hardware presets",
            Self::Sound => "Sound & MIDI",
            Self::Input => "Mouse & input",
            Self::Display => "Display",
            Self::Interface => "Interface",
        })
    }
}

fn global(edit: GlobalEdit) -> Message {
    Message::Global(edit)
}

/// A row of chips for one value.
fn chips<'a, T: Copy + PartialEq + 'a, L: Into<String>>(
    options: impl IntoIterator<Item = (T, L)>,
    current: T,
    on_press: impl Fn(T) -> GlobalEdit,
) -> Element<'a, Message> {
    row(options.into_iter().map(|(value, label)| {
        button(text(label.into()).size(12))
            .padding([6, 12])
            .style(chip_style(value == current))
            .on_press(global(on_press(value)))
            .into()
    }))
    .spacing(8)
    .wrap()
    .vertical_spacing(8)
    .into()
}

fn switch<'a>(value: bool, on_press: fn(bool) -> GlobalEdit) -> Element<'a, Message> {
    chips([(true, "On"), (false, "Off")], value, on_press)
}

fn input<'a>(
    placeholder: &'a str,
    value: &'a str,
    on_input: impl Fn(String) -> GlobalEdit + 'a,
) -> Element<'a, Message> {
    text_input(placeholder, value)
        .on_input(move |v| global(on_input(v)))
        .padding([8, 12])
        .size(13)
        .into()
}

pub(super) fn page(library: &Library, compact: bool) -> Element<'_, Message> {
    let s = tr(library);
    let mut page = column![
        column![
            text(s.global_settings_title).size(30).font(bold()),
            text(s.global_settings_subtitle).size(13).color(MUTED),
        ]
        .spacing(2)
    ]
    .spacing(18)
    .padding([30.0, PAGE_PADDING])
    .height(Fill);
    if compact {
        let mut items = filters(library);
        items.push((s.global_settings, Filter::Settings));
        page = page.push(
            row(items.into_iter().map(|(label, filter)| {
                button(text(label).size(13))
                    .padding([6, 14])
                    .style(chip_style(library.filter == filter))
                    .on_press(Message::Filter(filter))
                    .into()
            }))
            .spacing(8)
            .wrap()
            .vertical_spacing(8),
        );
    }
    if let Some(notice) = &library.notice {
        page = page.push(notice_bar(notice));
    }
    let content = scrollable(
        container(section(library))
            .padding([4, 24])
            .max_width(760)
            .width(Fill),
    )
    .height(Fill);
    let body: Element<'_, Message> = if compact {
        column![
            row(SettingsPage::ALL.into_iter().map(|page| {
                button(text(page.to_string()).size(12))
                    .padding([6, 12])
                    .style(chip_style(library.settings_page == page))
                    .on_press(global(GlobalEdit::Page(page)))
                    .into()
            }))
            .spacing(6)
            .wrap()
            .vertical_spacing(6),
            content,
        ]
        .spacing(16)
        .into()
    } else {
        row![
            column(SettingsPage::ALL.into_iter().map(|page| {
                let selected = library.settings_page == page;
                button(text(page.to_string()).size(14).font(if selected {
                    semibold()
                } else {
                    iced::Font::DEFAULT
                }))
                .width(Fill)
                .padding([9, 12])
                .style(nav_style(selected))
                .on_press(global(GlobalEdit::Page(page)))
                .into()
            }))
            .spacing(2)
            .width(200),
            rule::vertical(1),
            content,
        ]
        .spacing(4)
        .into()
    };
    page.push(body).into()
}

fn section(library: &Library) -> Element<'_, Message> {
    match library.settings_page {
        SettingsPage::Machine => machine(library),
        SettingsPage::Presets => presets(library),
        SettingsPage::Sound => sound(library),
        SettingsPage::Input => input_page(library),
        SettingsPage::Display => display(library),
        SettingsPage::Interface => interface(library),
    }
}

fn preset_summary(preset: &HardwarePreset) -> String {
    let mut cards = Vec::new();
    if preset.sb16 {
        cards.push("SB16");
    }
    if preset.mpu401 {
        cards.push("MPU-401");
    }
    format!(
        "{} MHz · {} · {} MiB{}{}",
        preset.cpu_mhz,
        preset.cpu_mode,
        preset.memory_mib,
        if preset.x87 { " · x87" } else { "" },
        if cards.is_empty() {
            String::new()
        } else {
            format!(" · {}", cards.join(", "))
        }
    )
}

fn machine(library: &Library) -> Element<'_, Message> {
    let g = &library.global;
    let presets = g.all_presets().into_iter().map(|preset| {
        let selected = g.default_preset == preset.id;
        button(
            column![
                text(preset.name.clone()).size(13).font(semibold()),
                text(preset_summary(&preset)).size(11).color(MUTED),
            ]
            .spacing(2),
        )
        .padding([10, 14])
        .style(chip_style(selected))
        .on_press(global(GlobalEdit::DefaultPreset(preset.id)))
        .into()
    });
    column![
        group(
            "Default hardware",
            column![
                hint(
                    "Games that name no preset start from this one: its clock, speed mode, \
                     memory, x87 and sound cards."
                ),
                row(presets).spacing(10).wrap().vertical_spacing(10),
                button(text("Edit presets…").size(12))
                    .padding([6, 12])
                    .style(secondary_button_style)
                    .on_press(global(GlobalEdit::Page(SettingsPage::Presets))),
            ]
            .spacing(12),
        ),
        rule::horizontal(1),
        group(
            "Protected mode",
            column![
                field(
                    "DPMI host",
                    chips(
                        DpmiHost::ALL.map(|host| (host, dpmi_label(host))),
                        g.dpmi,
                        GlobalEdit::Dpmi
                    ),
                ),
                hint(match g.dpmi {
                    DpmiHost::Direct =>
                        "Doslet's host serves DOS extenders and handles eligible BIOS and DOS \
                         calls in protected mode.",
                    DpmiHost::Reflect =>
                        "Doslet's host serves DOS extenders; BIOS and DOS calls run through \
                         real-mode handlers.",
                    DpmiHost::Off => "No host: DOS extenders use their own.",
                }),
            ]
            .spacing(12),
        ),
    ]
    .spacing(20)
    .into()
}

fn presets(library: &Library) -> Element<'_, Message> {
    let g = &library.global;
    let mut list = column![
        hint(
            "Built-in presets are fixed; copy one to change it. A game that names a preset \
             takes the values it does not set itself."
        ),
        button(text("New preset").size(12).font(semibold()))
            .padding([8, 14])
            .style(secondary_button_style)
            .on_press(global(GlobalEdit::NewPreset(None))),
    ]
    .spacing(12);
    for preset in g.all_presets() {
        let custom = g.presets.iter().any(|p| p.id == preset.id);
        let default = g.default_preset == preset.id;
        let id = preset.id.clone();
        let mut actions = row![].spacing(6).align_y(Alignment::Center);
        if default {
            actions = actions.push(text("Default").size(12).color(super::ACTIVE));
        } else {
            actions = actions.push(
                button(text("Use as default").size(12))
                    .padding([5, 10])
                    .style(nav_style(false))
                    .on_press(global(GlobalEdit::DefaultPreset(id.clone()))),
            );
        }
        actions = actions.push(
            button(text("Copy").size(12))
                .padding([5, 10])
                .style(nav_style(false))
                .on_press(global(GlobalEdit::NewPreset(Some(id.clone())))),
        );
        if custom {
            actions = actions.push(
                button(text("Remove").size(12))
                    .padding([5, 10])
                    .style(nav_style(false))
                    .on_press(global(GlobalEdit::RemovePreset(id.clone()))),
            );
        }
        let title: Element<'_, Message> = if custom {
            let edit_id = id.clone();
            container(input("Preset name", leak_name(g, &id), move |v| {
                GlobalEdit::Preset(edit_id.clone(), PresetEdit::Name(v))
            }))
            .width(Fill)
            .into()
        } else {
            column![
                text(preset.name.clone()).size(14).font(semibold()),
                text(format!("Built-in · {}", preset_summary(&preset)))
                    .size(12)
                    .color(MUTED),
            ]
            .spacing(2)
            .width(Fill)
            .into()
        };
        let mut body =
            column![row![title, actions].spacing(10).align_y(Alignment::Center)].spacing(12);
        if custom {
            let mhz = preset.cpu_mhz;
            let slide = preset_edit(&id, PresetEdit::CpuMhz);
            body = body
                .push(field(
                    "Clock",
                    row![
                        slider(1..=1000, mhz, move |v| global(slide(v))).width(Fill),
                        text(format!("{mhz} MHz")).size(13).width(72),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                ))
                .push(field(
                    "Speed mode",
                    chips(
                        CpuMode::ALL.map(|mode| (mode, mode.to_string())),
                        preset.cpu_mode,
                        preset_edit(&id, PresetEdit::CpuMode),
                    ),
                ))
                .push(field(
                    "Memory",
                    chips(
                        [4u8, 8, 16, 32].map(|mib| (mib, format!("{mib} MiB"))),
                        preset.memory_mib,
                        preset_edit(&id, PresetEdit::Memory),
                    ),
                ))
                .push(
                    row![
                        field(
                            "x87",
                            chips(
                                [(true, "On"), (false, "Off")],
                                preset.x87,
                                preset_edit(&id, PresetEdit::X87)
                            )
                        ),
                        field(
                            "Sound Blaster 16",
                            chips(
                                [(true, "On"), (false, "Off")],
                                preset.sb16,
                                preset_edit(&id, PresetEdit::Sb16)
                            )
                        ),
                        field(
                            "MPU-401",
                            chips(
                                [(true, "On"), (false, "Off")],
                                preset.mpu401,
                                preset_edit(&id, PresetEdit::Mpu401)
                            )
                        ),
                    ]
                    .spacing(24)
                    .wrap()
                    .vertical_spacing(12),
                );
        }
        list = list.push(card(body));
    }
    list.into()
}

fn preset_edit<T>(id: &str, edit: fn(T) -> PresetEdit) -> impl Fn(T) -> GlobalEdit + use<T> {
    let id = id.to_owned();
    move |value| GlobalEdit::Preset(id.clone(), edit(value))
}

/// The stored name of a custom preset, borrowed from the settings.
fn leak_name<'a>(g: &'a crate::global_settings::GlobalSettings, id: &str) -> &'a str {
    g.presets
        .iter()
        .find(|p| p.id == id)
        .map_or("", |p| p.name.as_str())
}

fn sound(library: &Library) -> Element<'_, Message> {
    let g = &library.global;
    let mut midi_choices = library.midi_choices.clone();
    if !midi_choices.contains(&g.midi_output) {
        midi_choices.push(g.midi_output.clone());
    }
    let dialects = [("gm", "General MIDI"), ("mt32", "Roland MT-32")];
    let dialect = |current: &str, on_press: fn(String) -> GlobalEdit| -> Element<'_, Message> {
        row(dialects.map(|(id, label)| {
            button(text(label).size(12))
                .padding([6, 12])
                .style(chip_style(current.eq_ignore_ascii_case(id)))
                .on_press(global(on_press(id.into())))
                .into()
        }))
        .spacing(8)
        .into()
    };
    column![
        group(
            "Output",
            column![
                field(
                    "Play sound",
                    switch(g.audio_enabled, GlobalEdit::AudioEnabled)
                ),
                hint("Off silences every game, whatever its own setting."),
                field(
                    "Volume",
                    row![
                        slider(0..=100, g.audio_volume, |v| global(
                            GlobalEdit::AudioVolume(v)
                        ))
                        .width(260),
                        text(format!("{}%", g.audio_volume)).size(13),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                ),
                field(
                    "Mixer",
                    chips(
                        [("system", "System mixer"), ("application", "Doslet mixer")],
                        if g.audio_mixer.eq_ignore_ascii_case("system") {
                            "system"
                        } else {
                            "application"
                        },
                        |v| GlobalEdit::AudioMixer(v.into()),
                    ),
                ),
            ]
            .spacing(14),
        ),
        rule::horizontal(1),
        group(
            "Sound Blaster 16 · default resources",
            row![
                field(
                    "IRQ",
                    chips([(5u8, "5"), (7, "7")], g.sb_irq, GlobalEdit::SbIrq)
                ),
                field(
                    "8-bit DMA",
                    chips(
                        [(0u8, "0"), (1, "1"), (3, "3")],
                        g.sb_dma8,
                        GlobalEdit::SbDma8
                    )
                ),
                field(
                    "16-bit DMA",
                    chips(
                        [(5u8, "5"), (6, "6"), (7, "7")],
                        g.sb_dma16,
                        GlobalEdit::SbDma16
                    )
                ),
            ]
            .spacing(28)
            .wrap()
            .vertical_spacing(12),
        ),
        rule::horizontal(1),
        group(
            "MIDI",
            column![
                field(
                    "Output device",
                    row![
                        pick_list(midi_choices, Some(g.midi_output.clone()), |choice| {
                            global(GlobalEdit::MidiOutput(choice))
                        })
                        .text_size(13)
                        .width(280),
                        button(text("Refresh ports").size(12))
                            .padding([6, 12])
                            .style(secondary_button_style)
                            .on_press(global(GlobalEdit::RefreshMidiPorts)),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                ),
                field(
                    "Music the game sends",
                    dialect(&g.game_midi, GlobalEdit::GameMidi)
                ),
                field(
                    "Device expects",
                    dialect(&g.host_midi, GlobalEdit::HostMidi)
                ),
                hint(format!(
                    "Now: {} music to a {} device.",
                    midi_label(&g.game_midi),
                    midi_label(&g.host_midi)
                )),
                field(
                    "MT-32 / CM-32L ROM folder",
                    input(
                        "Folder with MT32_CONTROL.ROM and MT32_PCM.ROM",
                        g.mt32_rom_dir.as_deref().unwrap_or(""),
                        GlobalEdit::Mt32RomDir,
                    ),
                ),
            ]
            .spacing(14),
        ),
    ]
    .spacing(20)
    .into()
}

fn input_page(library: &Library) -> Element<'_, Message> {
    let g = &library.global;
    let mode = g.input.mouse_mode;
    let mut mouse = column![
        chips(
            MouseMode::ALL.map(|mode| (mode, mouse_label(mode))),
            mode,
            GlobalEdit::MouseMode
        ),
        hint(match mode {
            MouseMode::Auto =>
                "Captures the pointer when the game reads relative mouse motion; otherwise the \
                 guest cursor follows the system pointer.",
            MouseMode::Capture =>
                "Click the game screen to capture the pointer for relative motion. Ctrl+Esc \
                 releases it.",
            MouseMode::NoCapture =>
                "The pointer moves freely and the guest cursor follows its position over the \
                 game screen.",
        }),
    ]
    .spacing(10);
    if mode != MouseMode::Capture {
        mouse = mouse.push(field(
            "Hide the system cursor over the game screen",
            switch(g.input.hide_cursor, GlobalEdit::HideCursor),
        ));
    }
    let keyboard = column![
        chips(
            [
                (KeyboardMode::Bios, "BIOS compatible"),
                (KeyboardMode::At, "AT controller")
            ],
            g.keyboard,
            GlobalEdit::Keyboard
        ),
        hint(match g.keyboard {
            KeyboardMode::Bios => "BIOS scan codes and text input; works with most games.",
            KeyboardMode::At =>
                "Physical key positions with press and release events, for games that read \
                 port 60h.",
        }),
    ]
    .spacing(10);
    let numbers = |count: u8| (0..count).collect::<Vec<u8>>();
    let joystick = column![
        field(
            "Emulate a PC gameport (port 201h)",
            switch(g.gameport, GlobalEdit::Gameport)
        ),
        field(
            "Controller",
            chips(
                (0u8..4).map(|n| (n, format!("Pad {}", n + 1))),
                g.input.pad_index,
                GlobalEdit::PadIndex
            ),
        ),
        field(
            "Controller ID · overrides the pad number",
            input(
                "Any controller",
                g.input.pad_id.as_deref().unwrap_or(""),
                GlobalEdit::PadId
            ),
        ),
        field(
            "Stick dead zone",
            row![
                slider(0..=95, g.input.deadzone_percent, |v| global(
                    GlobalEdit::Deadzone(v)
                ))
                .width(260),
                text(format!("{}%", g.input.deadzone_percent)).size(13),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        ),
        row![
            field(
                "Horizontal axis",
                pick_list(numbers(8), Some(g.input.axis_x), |v| global(
                    GlobalEdit::AxisX(v)
                ))
                .text_size(13)
                .width(90),
            ),
            field(
                "Vertical axis",
                pick_list(numbers(8), Some(g.input.axis_y), |v| global(
                    GlobalEdit::AxisY(v)
                ))
                .text_size(13)
                .width(90),
            ),
            field(
                "Button 1",
                pick_list(numbers(32), Some(g.input.button_1), |v| global(
                    GlobalEdit::Button1(v)
                ))
                .text_size(13)
                .width(90),
            ),
            field(
                "Button 2",
                pick_list(numbers(32), Some(g.input.button_2), |v| global(
                    GlobalEdit::Button2(v)
                ))
                .text_size(13)
                .width(90),
            ),
        ]
        .spacing(16)
        .wrap()
        .vertical_spacing(12),
    ]
    .spacing(14);
    column![
        group("Mouse", mouse),
        rule::horizontal(1),
        group("Keyboard", keyboard),
        rule::horizontal(1),
        group("Joystick", joystick),
    ]
    .spacing(20)
    .into()
}

fn display(library: &Library) -> Element<'_, Message> {
    let g = &library.global;
    let perfect = g.scaling == "pixel-perfect";
    let shape = match g.shape.as_str() {
        "16:9" | "16:9-smart" => g.shape.as_str(),
        _ => "4:3",
    };
    let native = g.fullscreen == "native";
    column![
        group(
            "Picture",
            column![
                field(
                    "Scaling filter",
                    chips(
                        VideoFilter::ALL.map(|filter| (filter, filter_label(filter))),
                        g.video_filter,
                        GlobalEdit::VideoFilter,
                    ),
                ),
                hint(filter_hint(g.video_filter)),
                field(
                    "Without a GPU",
                    chips(
                        SIMPLE_FILTERS.map(|filter| (filter, filter_label(filter))),
                        g.video_filter_fallback.unwrap_or(g.video_filter.fallback()),
                        |filter| GlobalEdit::VideoFilterFallback(Some(filter)),
                    ),
                ),
                hint(
                    "HQ2x, xBRZ and RAVU-Zoom need a hardware GPU. In a browser without WebGPU \
                     or WebGL2, or with only a software renderer such as llvmpipe, this simple \
                     filter is shown instead."
                ),
                field(
                    "Size",
                    chips(
                        [(false, "Fit window"), (true, "Pixel perfect")],
                        perfect,
                        |v| GlobalEdit::Scaling(if v { "pixel-perfect" } else { "fit" }.into()),
                    ),
                ),
                field(
                    "Screen shape",
                    chips(
                        [
                            ("4:3", "4:3"),
                            ("16:9", "16:9"),
                            ("16:9-smart", "16:9 smart")
                        ],
                        shape,
                        |v| GlobalEdit::Shape(v.into()),
                    ),
                ),
                hint(if perfect {
                    "Pixel perfect only enlarges whole pixels at the original proportions: no \
                     filter and no screen shape."
                } else {
                    match shape {
                        "16:9" => "16:9 stretches the whole picture evenly.",
                        "16:9-smart" => {
                            "16:9 smart keeps the centre near 4:3 and stretches only the edges. \
                             The mouse follows the picture."
                        }
                        _ => "4:3 is the proportion of a CRT monitor.",
                    }
                }),
            ]
            .spacing(14),
        ),
        rule::horizontal(1),
        group(
            "Frame limit",
            column![
                chips(
                    [0u16, 30, 35, 50, 60, 70].map(|limit| (limit, frame_limit_label(limit))),
                    g.frame_limit,
                    GlobalEdit::FrameLimit,
                ),
                hint(
                    "Most pictures per second the window shows, to save host CPU and GPU time. \
                     Games run at full speed either way; a game can set its own limit."
                ),
            ]
            .spacing(10),
        ),
        rule::horizontal(1),
        group(
            "Full screen",
            column![
                chips(
                    [(false, "Borderless window"), (true, "Native")],
                    native,
                    |v| GlobalEdit::Fullscreen(if v { "native" } else { "borderless" }.into()),
                ),
                hint(if native {
                    "Native uses the compositor's full screen. On exit the picture pauses for a \
                     second before the window mode changes."
                } else {
                    "Borderless window maximizes the window without its frame. Desktop panels \
                     may stay visible, but the compositor never switches modes."
                }),
            ]
            .spacing(10),
        ),
        hint("These apply when a game starts; the Display panel changes them while playing."),
    ]
    .spacing(20)
    .into()
}

fn interface(library: &Library) -> Element<'_, Message> {
    let g = &library.global;
    let language = match g.language.as_str() {
        "en" | "pl" => g.language.as_str(),
        _ => "auto",
    };
    column![
        field(
            "Language",
            chips(
                [("auto", "System"), ("en", "English"), ("pl", "Polski")],
                language,
                |v| GlobalEdit::Language(v.into()),
            ),
        ),
        hint("The library uses it; the game window is in English."),
        space().height(8),
        text(format!(
            "Profiles: {}",
            library
                .directories()
                .iter()
                .map(|d| d.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .size(12)
        .color(MUTED),
    ]
    .spacing(12)
    .into()
}
