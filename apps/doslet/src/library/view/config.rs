//! Game configuration window: a modal over the game page, or the whole window
//! when it is small. Every setting can stay at Default, which follows the
//! global settings, the hardware preset or the built-in value.
use super::{
    ACTIVE, Alignment, Border, Color, EDGE, Element, FOREGROUND, Fill, Game, GameEdit, Library,
    MUTED, Message, SIDEBAR, SURFACE, Shadow, Vector, WARNING, bold, chip_style, fade, nav_style,
    play_style, secondary_button_style, semibold, tr,
};
use crate::library::{ConfigPage, ConfigWindow, Setting};
use doslet_config::{
    CpuMode, DpmiHost, HardwarePreset, KeyboardMode, MouseMode, SoundCard, SourceKind, VideoFilter,
    WritePolicy,
};
use iced::widget::{
    button, column, container, opaque, pick_list, row, rule, scrollable, slider, space, text,
    text_input,
};
use toml_edit::Value;

/// Below this width the section list becomes a row of chips.
const NARROW: f32 = 720.0;
/// Below this size the window covers the whole library.
const FULL: iced::Size = iced::Size::new(1000.0, 700.0);

impl core::fmt::Display for ConfigPage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::General => "Programs",
            Self::Machine => "Machine",
            Self::Sound => "Sound & MIDI",
            Self::Input => "Mouse & input",
            Self::Display => "Display",
            Self::Drives => "Drives",
        })
    }
}

pub(super) fn window<'a>(
    library: &'a Library,
    game: &'a Game,
    window: &'a ConfigWindow,
    size: iced::Size,
) -> Element<'a, Message> {
    let s = tr(library);
    let narrow = size.width < NARROW;
    let full = size.width < FULL.width || size.height < FULL.height;
    let header = row![
        column![
            text(s.game_configuration).size(20).font(bold()),
            text(&game.name).size(13).color(MUTED),
        ]
        .spacing(2),
        space::horizontal(),
        button(text("×").size(20))
            .padding([2, 12])
            .style(nav_style(false))
            .on_press(edit(GameEdit::Cancel)),
    ]
    .align_y(Alignment::Center);
    let content = scrollable(
        container(page(library, window))
            .padding(if narrow { [18, 18] } else { [24, 28] })
            .width(Fill),
    )
    .height(Fill);
    let body: Element<'_, Message> = if narrow {
        let chips = ConfigPage::ALL.into_iter().map(|page| {
            button(text(page.to_string()).size(12))
                .padding([6, 12])
                .style(chip_style(window.page == page))
                .on_press(edit(GameEdit::Page(page)))
                .into()
        });
        column![
            container(row(chips).spacing(6).wrap().vertical_spacing(6)).padding([12, 18]),
            rule::horizontal(1),
            content,
        ]
        .into()
    } else {
        let sections = ConfigPage::ALL.into_iter().map(|page| {
            let selected = window.page == page;
            button(text(page.to_string()).size(14).font(if selected {
                semibold()
            } else {
                iced::Font::DEFAULT
            }))
            .width(Fill)
            .padding([9, 12])
            .style(nav_style(selected))
            .on_press(edit(GameEdit::Page(page)))
            .into()
        });
        row![
            container(column(sections).spacing(2))
                .width(190)
                .padding([16, 12]),
            rule::vertical(1),
            content,
        ]
        .into()
    };
    let status: Element<'_, Message> = if let Some(problem) = &window.problem {
        text(problem).size(12).color(WARNING).into()
    } else if window.converted {
        hint("This version-1 profile will be saved as version 2.")
    } else {
        hint("Default follows Settings, the hardware preset or Doslet's built-in value.")
    };
    let footer = row![
        container(status).width(Fill),
        button(text("Cancel").size(13))
            .padding([9, 18])
            .style(secondary_button_style)
            .on_press(edit(GameEdit::Cancel)),
        button(text(s.save_to_profile).size(13).font(bold()))
            .padding([9, 20])
            .style(play_style)
            .on_press_maybe(
                (window.problem.is_none() && window.changed()).then_some(edit(GameEdit::Save)),
            ),
    ]
    .spacing(10)
    .align_y(Alignment::Center);
    let frame = container(
        column![
            container(header).padding([16, 22]),
            rule::horizontal(1),
            body,
            rule::horizontal(1),
            container(footer).padding([12, 22]),
        ]
        .height(Fill),
    );
    let frame = if full {
        frame.width(Fill).height(Fill).style(|_| container::Style {
            background: Some(SIDEBAR.into()),
            ..Default::default()
        })
    } else {
        frame
            .width(940)
            .height((size.height - 64.0).min(720.0))
            .style(|_| container::Style {
                background: Some(SIDEBAR.into()),
                border: Border {
                    color: EDGE,
                    width: 1.0,
                    radius: 16.0.into(),
                },
                shadow: Shadow {
                    color: fade(Color::BLACK, 0.6),
                    offset: Vector::new(0.0, 18.0),
                    blur_radius: 48.0,
                },
                ..Default::default()
            })
    };
    // The backdrop takes every click so the page below stays inert.
    opaque(container(frame).center(Fill).style(|_| container::Style {
        background: Some(fade(Color::BLACK, 0.55).into()),
        ..Default::default()
    }))
}

fn page<'a>(library: &'a Library, window: &'a ConfigWindow) -> Element<'a, Message> {
    match window.page {
        ConfigPage::General => programs(window),
        ConfigPage::Machine => machine(library, window),
        ConfigPage::Sound => sound(library, window),
        ConfigPage::Input => input(window),
        ConfigPage::Display => display(window),
        ConfigPage::Drives => drives(window),
    }
}

fn edit(edit: GameEdit) -> Message {
    Message::GameConfig(edit)
}

pub(super) fn field<'a>(
    label: &'a str,
    input: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![text(label).size(12).color(MUTED), input.into()]
        .spacing(6)
        .into()
}

pub(super) fn hint<'a>(label: impl text::IntoFragment<'a>) -> Element<'a, Message> {
    text(label).size(12).color(fade(MUTED, 0.85)).into()
}

pub(super) fn group<'a>(
    title: &'a str,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![
        text(title).size(15).font(semibold()).color(FOREGROUND),
        content.into()
    ]
    .spacing(12)
    .into()
}

pub(super) fn card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    container(content)
        .width(Fill)
        .padding([12, 14])
        .style(|_| container::Style {
            background: Some(fade(SURFACE, 0.7).into()),
            border: Border {
                color: EDGE,
                width: 1.0,
                radius: 10.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn input_box<'a>(
    placeholder: &str,
    value: &str,
    on_input: impl Fn(String) -> GameEdit + 'a,
) -> Element<'a, Message> {
    text_input(placeholder, value)
        .on_input(move |v| edit(on_input(v)))
        .padding([8, 12])
        .size(13)
        .into()
}

pub(super) fn on_off(value: bool) -> &'static str {
    if value { "On" } else { "Off" }
}

fn same(a: &Value, b: &Value) -> bool {
    a.as_str() == b.as_str() && a.as_integer() == b.as_integer() && a.as_bool() == b.as_bool()
}

/// A row of chips: Default, then each value. The selected chip shows what
/// the profile sets.
fn choice<'a, V: Into<Value>, L: Into<String>>(
    window: &ConfigWindow,
    setting: Setting,
    default: String,
    options: impl IntoIterator<Item = (V, L)>,
) -> Element<'a, Message> {
    let current = window.get(setting).cloned();
    let chip = |label: String, selected: bool, message: GameEdit| -> Element<'a, Message> {
        button(text(label).size(12))
            .padding([6, 12])
            .style(chip_style(selected))
            .on_press(edit(message))
            .into()
    };
    let mut chips = vec![chip(
        format!("Default · {default}"),
        !window.is_set(setting),
        GameEdit::Unset(setting),
    )];
    for (value, label) in options {
        let value = value.into();
        let selected = current.as_ref().is_some_and(|c| same(c, &value));
        chips.push(chip(label.into(), selected, GameEdit::Set(setting, value)));
    }
    row(chips).spacing(8).wrap().vertical_spacing(8).into()
}

fn switch<'a>(window: &ConfigWindow, setting: Setting, default: bool) -> Element<'a, Message> {
    choice(
        window,
        setting,
        on_off(default).into(),
        [(true, "On"), (false, "Off")],
    )
}

/// Choices of a pick list that can also stay at Default.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pick {
    Default(String),
    Value(String),
}
impl core::fmt::Display for Pick {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Default(value) => write!(f, "Default · {value}"),
            Self::Value(value) => f.write_str(value),
        }
    }
}

fn pick<'a>(
    window: &ConfigWindow,
    setting: Setting,
    default: String,
    values: impl IntoIterator<Item = String>,
    to_value: fn(&str) -> Value,
) -> Element<'a, Message> {
    let current = match window.get(setting) {
        Some(value) => Pick::Value(
            value
                .as_str()
                .map(str::to_owned)
                .or_else(|| value.as_integer().map(|v| v.to_string()))
                .unwrap_or_default(),
        ),
        None => Pick::Default(default.clone()),
    };
    let mut options = vec![Pick::Default(default)];
    options.extend(values.into_iter().map(Pick::Value));
    if !options.contains(&current) {
        options.push(current.clone());
    }
    pick_list(options, Some(current), move |choice| {
        edit(match choice {
            Pick::Default(_) => GameEdit::Unset(setting),
            Pick::Value(value) => GameEdit::Set(setting, to_value(&value)),
        })
    })
    .text_size(13)
    .into()
}

fn number(text: &str) -> Value {
    text.parse::<i64>().unwrap_or_default().into()
}

fn string(text: &str) -> Value {
    text.into()
}

/// Picks the executable from the programs found on C:.
fn program_pick<'a>(
    window: &'a ConfigWindow,
    on_select: impl Fn(String) -> GameEdit + 'a,
) -> Element<'a, Message> {
    pick_list(window.programs.as_slice(), None::<&String>, move |name| {
        edit(on_select(name))
    })
    .placeholder(if window.programs.is_empty() {
        "No programs found on C:"
    } else {
        "Choose from C:…"
    })
    .text_size(13)
    .width(210)
    .into()
}

fn programs(window: &ConfigWindow) -> Element<'_, Message> {
    let mut others = column![].spacing(10);
    for (index, action) in window.actions.iter().enumerate() {
        others = others.push(card(
            column![
                row![
                    container(input_box("Label", &action.name, move |v| {
                        GameEdit::ActionName(index, v)
                    }))
                    .width(Fill),
                    button(text("Remove").size(12))
                        .padding([6, 10])
                        .style(nav_style(false))
                        .on_press(edit(GameEdit::RemoveAction(index))),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                row![
                    container(input_box("SETUP.EXE", &action.executable, move |v| {
                        GameEdit::ActionExecutable(index, v)
                    }))
                    .width(Fill),
                    program_pick(window, move |v| GameEdit::ActionExecutable(index, v)),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
                input_box("Arguments", &action.arguments, move |v| {
                    GameEdit::ActionArguments(index, v)
                }),
            ]
            .spacing(8),
        ));
    }
    if window.actions.is_empty() {
        others = others.push(hint("None."));
    }
    column![
        field("Name", input_box("Game name", &window.name, GameEdit::Name)),
        group(
            "Game",
            column![
                field(
                    "Executable",
                    row![
                        container(input_box(
                            "GAME.EXE",
                            &window.executable,
                            GameEdit::Executable
                        ))
                        .width(Fill),
                        program_pick(window, GameEdit::Executable),
                    ]
                    .spacing(8)
                    .align_y(Alignment::Center),
                ),
                hint(
                    "For a folder, the path is relative to the profile; for a ZIP archive, it is \
                     the path inside the archive.",
                ),
                field(
                    "Arguments",
                    input_box("None", &window.arguments, GameEdit::Arguments),
                ),
                field(
                    "Working directory",
                    input_box(
                        "Default · C:\\",
                        &window.working_directory,
                        GameEdit::WorkingDirectory,
                    ),
                ),
                field(
                    "Startup keys",
                    input_box("None", &window.startup_keys, GameEdit::StartupKeys),
                ),
                hint(
                    "Typed into the keyboard buffer when the game first waits for a key, at most \
                     15 characters.",
                ),
            ]
            .spacing(14),
        ),
        rule::horizontal(1),
        group(
            "Other programs",
            column![
                hint(
                    "Setup and editor programs that share this game's drives and saves. Each \
                     gets its own button next to Play."
                ),
                others,
                button(text("Add program").size(12).font(semibold()))
                    .padding([8, 14])
                    .style(secondary_button_style)
                    .on_press(edit(GameEdit::AddAction)),
            ]
            .spacing(10),
        ),
    ]
    .spacing(20)
    .into()
}

fn machine<'a>(library: &'a Library, window: &'a ConfigWindow) -> Element<'a, Message> {
    let effective = &window.effective;
    let defaults = &window.defaults;
    let global_preset = library
        .global
        .find_preset(&library.global.default_preset)
        .map_or(library.global.default_preset.clone(), |p| p.name);
    let mut presets: Vec<(String, String)> = Vec::new();
    for preset in HardwarePreset::builtins()
        .into_iter()
        .chain(library.global.presets.iter().cloned())
    {
        if !presets.iter().any(|(id, _)| *id == preset.id) {
            presets.push((preset.id, preset.name));
        }
    }
    let mode = effective.cpu_mode.unwrap_or_default();
    let dpmi = effective.dpmi.unwrap_or(DpmiHost::Direct);
    let mhz = effective.hardware.cpu_mhz;
    column![
        field(
            "Hardware preset",
            choice(window, Setting::Preset, global_preset, presets,),
        ),
        hint("A preset sets the clock, speed mode, memory, x87 and sound cards left at Default."),
        group(
            "Processor",
            column![
                field(
                    "Clock",
                    column![
                        choice(
                            window,
                            Setting::CpuMhz,
                            format!("{} MHz", defaults.hardware.cpu_mhz),
                            [
                                (16i64, "16"),
                                (25, "25"),
                                (33, "33"),
                                (66, "66"),
                                (100, "100"),
                                (133, "133"),
                                (266, "266"),
                                (500, "500"),
                            ],
                        ),
                        row![
                            slider(1..=1000, mhz, |v| edit(GameEdit::Set(
                                Setting::CpuMhz,
                                i64::from(v).into()
                            )))
                            .width(Fill),
                            text(format!("{mhz} MHz")).size(13).width(72),
                        ]
                        .spacing(12)
                        .align_y(Alignment::Center),
                    ]
                    .spacing(8),
                ),
                field(
                    "Speed mode",
                    choice(
                        window,
                        Setting::CpuMode,
                        defaults.cpu_mode.unwrap_or_default().to_string(),
                        [
                            (CpuMode::Fixed.as_str(), "Fixed clock"),
                            (CpuMode::Adaptive.as_str(), "Adaptive"),
                            (CpuMode::SpeedTiers.as_str(), "Speed tiers"),
                        ],
                    ),
                ),
                hint(match mode {
                    CpuMode::Fixed => "Runs at the set clock all the time.",
                    CpuMode::Adaptive =>
                        "Experimental: sets the speed from how the game waits, with the clock \
                         as the ceiling.",
                    CpuMode::SpeedTiers =>
                        "Runs code at its learned Hot, Cold and Grey rates from a recorded tier \
                         profile.",
                }),
                field(
                    "x87 coprocessor (FPU)",
                    switch(window, Setting::X87, defaults.hardware.x87),
                ),
            ]
            .spacing(14),
        ),
        field(
            "Memory",
            choice(
                window,
                Setting::Memory,
                format!("{} MiB", defaults.hardware.memory_mib),
                [
                    (4i64, "4 MiB"),
                    (8, "8 MiB"),
                    (16, "16 MiB"),
                    (32, "32 MiB")
                ],
            ),
        ),
        group(
            "Protected mode",
            column![
                field(
                    "DPMI host",
                    choice(
                        window,
                        Setting::Dpmi,
                        dpmi_label(defaults.dpmi.unwrap_or(DpmiHost::Direct)).into(),
                        DpmiHost::ALL.map(|host| (host.as_str(), dpmi_label(host))),
                    ),
                ),
                hint(match dpmi {
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
    .spacing(22)
    .into()
}

pub(super) fn dpmi_label(host: DpmiHost) -> &'static str {
    match host {
        DpmiHost::Direct => "Direct",
        DpmiHost::Reflect => "Reflect",
        DpmiHost::Off => "Off",
    }
}

pub(super) fn midi_label(id: &str) -> &'static str {
    if id.eq_ignore_ascii_case("mt32") {
        "Roland MT-32"
    } else {
        "General MIDI"
    }
}

fn sound<'a>(library: &'a Library, window: &'a ConfigWindow) -> Element<'a, Message> {
    let effective = &window.effective;
    let defaults = &window.defaults;
    let mut blaster = column![field(
        "Sound card",
        sound_card_choice(window, defaults.hardware.sound())
    )]
    .spacing(14);
    let sound = effective.hardware.sound();
    if sound.has_dsp() {
        let mut io = row![
            field(
                "IRQ",
                choice(
                    window,
                    Setting::SbIrq,
                    defaults.hardware.sb_irq.to_string(),
                    [(5i64, "5"), (7, "7")]
                )
            ),
            field(
                "8-bit DMA",
                choice(
                    window,
                    Setting::SbDma8,
                    defaults.hardware.sb_dma8.to_string(),
                    [(0i64, "0"), (1, "1"), (3, "3")]
                )
            ),
        ]
        .spacing(28);
        if sound.has_dma16() {
            io = io.push(field(
                "16-bit DMA",
                choice(
                    window,
                    Setting::SbDma16,
                    defaults.hardware.sb_dma16.to_string(),
                    [(5i64, "5"), (6, "6"), (7, "7")],
                ),
            ));
        }
        blaster = blaster.push(io.wrap().vertical_spacing(12));
    }
    let mut midi = column![field(
        "MPU-401 · port 330h",
        switch(window, Setting::Mpu401, defaults.hardware.mpu401)
    )]
    .spacing(14);
    if effective.hardware.mpu401 {
        let dialects = [("gm", "General MIDI"), ("mt32", "Roland MT-32")];
        midi = midi
            .push(field(
                "Music the game sends",
                choice(
                    window,
                    Setting::GameMidi,
                    midi_label(&defaults.output.game_midi).into(),
                    dialects,
                ),
            ))
            .push(field(
                "Output device",
                container(pick(
                    window,
                    Setting::MidiOutput,
                    defaults.output.midi_output.clone(),
                    library.midi_choices.iter().cloned(),
                    string,
                ))
                .width(300),
            ))
            .push(field(
                "Device expects",
                choice(
                    window,
                    Setting::HostMidi,
                    midi_label(&defaults.output.host_midi).into(),
                    dialects,
                ),
            ))
            .push(hint(
                "A General MIDI device gets MT-32 music translated to General MIDI.",
            ));
    }
    let mut page = column![field(
        "Play sound on this computer",
        switch(window, Setting::Audio, defaults.output.audio)
    ),]
    .spacing(20);
    if !library.global.audio_enabled {
        page = page.push(
            text("Audio is off in Settings, which silences every game.")
                .size(12)
                .color(WARNING),
        );
    }
    page.push(rule::horizontal(1))
        .push(group("Sound card", blaster))
        .push(rule::horizontal(1))
        .push(group("MIDI", midi))
        .into()
}

fn sound_card_choice(window: &ConfigWindow, default: SoundCard) -> Element<'_, Message> {
    let explicit = window
        .get(Setting::SoundCard)
        .and_then(Value::as_str)
        .and_then(SoundCard::parse)
        .or_else(|| {
            window
                .get(Setting::Sb16)
                .and_then(Value::as_bool)
                .map(|on| if on { SoundCard::Sb16 } else { SoundCard::None })
        });
    let mut choices = row![
        button(text(format!("Default · {default}")).size(12))
            .padding([6, 12])
            .style(chip_style(explicit.is_none()))
            .on_press(edit(GameEdit::Unset(Setting::SoundCard)))
    ]
    .spacing(8);
    for card in SoundCard::ALL {
        choices = choices.push(
            button(text(card.to_string()).size(12))
                .padding([6, 12])
                .style(chip_style(explicit == Some(card)))
                .on_press(edit(GameEdit::Set(
                    Setting::SoundCard,
                    card.as_str().into(),
                ))),
        );
    }
    choices.wrap().vertical_spacing(8).into()
}

pub(super) fn mouse_label(mode: MouseMode) -> &'static str {
    match mode {
        MouseMode::Auto => "Automatic",
        MouseMode::Capture => "Always capture",
        MouseMode::NoCapture => "Never capture",
    }
}

fn input(window: &ConfigWindow) -> Element<'_, Message> {
    let effective = &window.effective;
    let defaults = &window.defaults;
    let mode = effective.input.mouse_mode;
    let mut mouse = column![
        choice(
            window,
            Setting::MouseMode,
            mouse_label(defaults.input.mouse_mode).into(),
            MouseMode::ALL.map(|mode| (mode.as_str(), mouse_label(mode))),
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
            switch(window, Setting::HideCursor, defaults.input.hide_cursor),
        ));
    }
    let keyboard_label = |mode| match mode {
        KeyboardMode::Bios => "BIOS compatible",
        KeyboardMode::At => "AT controller",
    };
    let keyboard = column![
        choice(
            window,
            Setting::Keyboard,
            keyboard_label(defaults.hardware.keyboard).into(),
            [("bios", "BIOS compatible"), ("at", "AT controller")],
        ),
        hint(match effective.hardware.keyboard {
            KeyboardMode::Bios => "BIOS scan codes and text input; works with most games.",
            KeyboardMode::At =>
                "Physical key positions with press and release events, for games that read \
                 port 60h.",
        }),
    ]
    .spacing(10);
    let mut joystick = column![field(
        "Emulate a PC gameport (port 201h)",
        switch(window, Setting::Gameport, defaults.hardware.gameport)
    )]
    .spacing(14);
    if effective.hardware.gameport {
        let numbers = |count: u8| (0..count).map(|n| n.to_string()).collect::<Vec<_>>();
        let deadzone = effective.input.deadzone_percent;
        joystick = joystick
            .push(field(
                "Controller",
                choice(
                    window,
                    Setting::PadIndex,
                    format!("Pad {}", defaults.input.pad_index + 1),
                    [(0i64, "Pad 1"), (1, "Pad 2"), (2, "Pad 3"), (3, "Pad 4")],
                ),
            ))
            .push(field(
                "Controller ID · overrides the pad number",
                input_box(
                    &format!(
                        "Default · {}",
                        defaults.input.pad_id.as_deref().unwrap_or("none")
                    ),
                    window
                        .get(Setting::PadId)
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                    |v| {
                        if v.is_empty() {
                            GameEdit::Unset(Setting::PadId)
                        } else {
                            GameEdit::Set(Setting::PadId, v.into())
                        }
                    },
                ),
            ))
            .push(field(
                "Stick dead zone",
                column![
                    choice(
                        window,
                        Setting::Deadzone,
                        format!("{}%", defaults.input.deadzone_percent),
                        [(5i64, "5%"), (12, "12%"), (20, "20%"), (30, "30%")],
                    ),
                    row![
                        slider(0..=95, deadzone, |v| edit(GameEdit::Set(
                            Setting::Deadzone,
                            i64::from(v).into()
                        )))
                        .width(260),
                        text(format!("{deadzone}%")).size(13),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                ]
                .spacing(8),
            ))
            .push(
                row![
                    field(
                        "Horizontal axis",
                        pick(
                            window,
                            Setting::AxisX,
                            defaults.input.axis_x.to_string(),
                            numbers(8),
                            number
                        )
                    ),
                    field(
                        "Vertical axis",
                        pick(
                            window,
                            Setting::AxisY,
                            defaults.input.axis_y.to_string(),
                            numbers(8),
                            number
                        )
                    ),
                    field(
                        "Button 1",
                        pick(
                            window,
                            Setting::Button1,
                            defaults.input.button_1.to_string(),
                            numbers(32),
                            number
                        )
                    ),
                    field(
                        "Button 2",
                        pick(
                            window,
                            Setting::Button2,
                            defaults.input.button_2.to_string(),
                            numbers(32),
                            number
                        )
                    ),
                ]
                .spacing(16)
                .wrap()
                .vertical_spacing(12),
            );
    }
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

pub(super) fn filter_label(filter: VideoFilter) -> &'static str {
    match filter {
        VideoFilter::Nearest => "Nearest · sharp pixels",
        VideoFilter::SharpBilinear => "Sharp bilinear",
        VideoFilter::Hq2x => "HQ2x",
        VideoFilter::Xbrz => "xBRZ · GPU",
        VideoFilter::Ravu => "RAVU-Zoom · GPU",
    }
}

pub(super) fn filter_hint(filter: VideoFilter) -> &'static str {
    match filter {
        VideoFilter::Nearest => "Square pixel edges at any size.",
        VideoFilter::SharpBilinear => {
            "Whole-pixel enlargement, then smooths only the fraction left."
        }
        VideoFilter::Hq2x => "Edge-aware 2× enlargement on the CPU; smooths diagonals.",
        VideoFilter::Xbrz => {
            "GPU shader that smooths pixel-art edges at the display size, including \
             fractional scales."
        }
        VideoFilter::Ravu => {
            "GPU prescaler (RAVU-Zoom) for a soft, natural enlargement rather than sharp \
             pixel edges."
        }
    }
}

/// Simple filters, the only ones drawn without a hardware GPU.
pub(super) const SIMPLE_FILTERS: [VideoFilter; 2] =
    [VideoFilter::Nearest, VideoFilter::SharpBilinear];

pub(super) fn frame_limit_label(limit: u16) -> String {
    if limit == 0 {
        "No limit".into()
    } else {
        format!("{limit} FPS")
    }
}

fn display(window: &ConfigWindow) -> Element<'_, Message> {
    let filter = window.effective.output.video_filter;
    let limit = window.effective.output.frame_limit;
    let custom = window
        .get(Setting::FrameLimit)
        .and_then(Value::as_integer)
        .filter(|v| ![0, 30, 35, 50, 60, 70].contains(v))
        .map_or(String::new(), |v| v.to_string());
    column![
        field(
            "Frame limit",
            column![
                choice(
                    window,
                    Setting::FrameLimit,
                    frame_limit_label(window.defaults.output.frame_limit),
                    [
                        (0i64, "No limit"),
                        (30, "30"),
                        (35, "35"),
                        (50, "50"),
                        (60, "60"),
                        (70, "70"),
                    ],
                ),
                row![
                    text("Other").size(12).color(MUTED),
                    container(input_box("10–240", &custom, |v| {
                        match v.trim().parse::<i64>() {
                            Ok(v) => GameEdit::Set(Setting::FrameLimit, v.into()),
                            Err(_) => GameEdit::Unset(Setting::FrameLimit),
                        }
                    }))
                    .width(110),
                    text("FPS").size(12).color(MUTED),
                ]
                .spacing(8)
                .align_y(Alignment::Center),
            ]
            .spacing(8),
        ),
        hint(if limit == 0 {
            "The window shows each new picture as it comes, up to about 60 per second.".to_owned()
        } else {
            format!(
                "The window shows at most {limit} pictures per second, evenly spaced, to save \
                 host CPU and GPU time. The game itself runs at full speed; set it to the \
                 game's own frame rate, such as 35 for Rise of the Triad."
            )
        }),
        rule::horizontal(1),
        field(
            "Scaling filter",
            choice(
                window,
                Setting::VideoFilter,
                filter_label(window.defaults.output.video_filter).into(),
                VideoFilter::ALL.map(|filter| (filter.as_str(), filter_label(filter))),
            ),
        ),
        hint(filter_hint(filter)),
        if filter.needs_gpu() {
            column![
                field(
                    "Without a GPU",
                    choice(
                        window,
                        Setting::VideoFilterFallback,
                        filter_label(window.defaults.output.filter(false)).into(),
                        SIMPLE_FILTERS.map(|filter| (filter.as_str(), filter_label(filter))),
                    ),
                ),
                hint(
                    "HQ2x, xBRZ and RAVU-Zoom need a hardware GPU. In a browser without WebGPU \
                     or WebGL2, or with only a software renderer such as llvmpipe, this simple \
                     filter is shown instead.",
                ),
            ]
            .spacing(10)
            .into()
        } else {
            Element::from(space())
        },
        hint(
            "Filters apply in Fit window; Pixel perfect only enlarges whole pixels. Size, \
             screen shape and full screen come from Settings and the Display panel.",
        ),
    ]
    .spacing(14)
    .into()
}

fn status<'a>(found: bool, label: &'a str) -> Element<'a, Message> {
    text(if found { "Found" } else { label })
        .size(12)
        .color(if found { ACTIVE } else { WARNING })
        .into()
}

fn drives(window: &ConfigWindow) -> Element<'_, Message> {
    let mut drive_c = column![].spacing(14);
    match &window.drive_c {
        Some(c) => {
            let kind = match c.kind {
                SourceKind::BrowserZip => "ZIP archive",
                SourceKind::NativeDirectory => "Folder",
                _ => "Disc image",
            };
            let policy = match c.policy {
                WritePolicy::Persistent => "Keeps changes in a save store",
                WritePolicy::Temporary => "Discards changes when the game stops",
                WritePolicy::ReadOnly => "Read only",
                WritePolicy::LegacyWriteback => "Writes changes into the source",
            };
            let mut summary = column![
                row![
                    text(kind).size(13).font(semibold()),
                    space::horizontal(),
                    status(c.exists, "Not found"),
                ]
                .align_y(Alignment::Center),
                text(c.path.display().to_string()).size(12).color(MUTED),
                text(policy).size(12).color(FOREGROUND),
            ]
            .spacing(4);
            if let Some(store) = &c.store {
                summary = summary.push(
                    text(format!("Save store: {}", store.display()))
                        .size(12)
                        .color(MUTED),
                );
            }
            drive_c = drive_c.push(card(summary));
            drive_c = drive_c.push(field(
                "Source",
                row![
                    button(text("Executable's folder").size(12))
                        .padding([6, 12])
                        .style(chip_style(!c.explicit))
                        .on_press(edit(GameEdit::DriveC(false))),
                    button(text("Set in this profile").size(12))
                        .padding([6, 12])
                        .style(chip_style(c.explicit))
                        .on_press(edit(GameEdit::DriveC(true))),
                ]
                .spacing(8),
            ));
            if c.explicit {
                drive_c = drive_c
                    .push(field(
                        "Folder or ZIP archive",
                        input_box(
                            "Path, relative to the profile",
                            &window.drive_text(b'C', "source"),
                            GameEdit::DriveCSource,
                        ),
                    ))
                    .push(field(
                        "When the game writes files",
                        row([
                            (WritePolicy::Persistent, "Keep changes"),
                            (WritePolicy::Temporary, "Discard on exit"),
                            (WritePolicy::ReadOnly, "Read only"),
                            (WritePolicy::LegacyWriteback, "Write to the source"),
                        ]
                        .map(|(policy, label)| {
                            button(text(label).size(12))
                                .padding([6, 12])
                                .style(chip_style(c.policy == policy))
                                .on_press(edit(GameEdit::DriveCPolicy(policy)))
                                .into()
                        }))
                        .spacing(8)
                        .wrap()
                        .vertical_spacing(8),
                    ));
                if c.policy == WritePolicy::Persistent {
                    drive_c = drive_c.push(field(
                        "Save store · a name, or a folder relative to the profile",
                        input_box(
                            "Required",
                            &window.drive_text(b'C', "store"),
                            GameEdit::DriveCStore,
                        ),
                    ));
                }
            } else {
                drive_c = drive_c.push(hint(
                    "C: is the folder that holds the executable, and changes last until the \
                     game stops.",
                ));
            }
        }
        None => {
            drive_c = drive_c.push(hint("Set the executable first, or name a folder or ZIP."));
            drive_c = drive_c.push(field(
                "Folder or ZIP archive",
                input_box("Path, relative to the profile", "", GameEdit::DriveCSource),
            ));
        }
    }
    let mut drive_d = column![].spacing(10);
    match &window.drive_d {
        Some((path, found)) => {
            drive_d = drive_d.push(card(
                column![
                    row![
                        text("Disc image").size(13).font(semibold()),
                        space::horizontal(),
                        status(*found, "Not found"),
                    ]
                    .align_y(Alignment::Center),
                    text(path.display().to_string()).size(12).color(MUTED),
                ]
                .spacing(4),
            ));
        }
        None => drive_d = drive_d.push(hint("No CD-ROM drive.")),
    }
    drive_d = drive_d
        .push(field(
            "Disc image (.iso)",
            input_box(
                "None",
                &window.drive_text(b'D', "source"),
                GameEdit::DriveDSource,
            ),
        ))
        .push(hint("Leave empty to remove the drive."));
    column![
        group("C:  Game files", drive_c),
        rule::horizontal(1),
        group("D:  CD-ROM", drive_d),
    ]
    .spacing(20)
    .into()
}
