//! Shared machine editor. The draft never mutates an executing machine.
use doslet_config::{KeyboardMode, MachineSpec, MountSpec, SourceKind, WritePolicy};
use iced::{
    Element, Fill,
    widget::{
        button, checkbox, column, container, pick_list, responsive, row, scrollable, space, text,
        text_input,
    },
};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Machine,
    Sound,
    Input,
    Drives,
    Output,
    Profile,
    Resources,
}
impl Page {
    pub const ALL: [Self; 7] = [
        Self::Machine,
        Self::Sound,
        Self::Input,
        Self::Drives,
        Self::Output,
        Self::Profile,
        Self::Resources,
    ];
}
impl core::fmt::Display for Page {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Machine => "Machine",
            Self::Sound => "Sound",
            Self::Input => "Input",
            Self::Drives => "Drives",
            Self::Output => "Output",
            Self::Profile => "Profile",
            Self::Resources => "Resources",
        })
    }
}
#[derive(Clone, Debug)]
pub enum Edit {
    Toggle,
    Page(Page),
    Memory(u8),
    CpuMhz(u16),
    X87(bool),
    Sb16(bool),
    Mpu(bool),
    Irq(u8),
    Dma8(u8),
    Dma16(u8),
    Keyboard(bool),
    Gameport(bool),
    Capture(bool),
    HideCursor(bool),
    PadId(String),
    Number(&'static str, String),
    Name(String),
    Executable(String),
    Root(String),
    Iso(String),
    Https(String),
    Policy(String),
    Store(String),
    #[allow(dead_code, reason = "Native save exchange file action")]
    SavePath(String),
    Audio(bool),
    Output(String),
    GameMidi(String),
    HostMidi(String),
    #[allow(
        dead_code,
        reason = "Browser profile picker and shared editor tests construct this action"
    )]
    ProfileText(String),
    ProfileAction(iced::widget::text_editor::Action),
    ImportProfile,
    ExportProfile,
    #[allow(dead_code, reason = "Native profile path; browsers use a file picker")]
    ProfilePath(String),
    LoadProfileFile,
    SaveProfileFile,
    Validate,
    Apply,
    Discard,
    ExportSaves,
    ImportSaves,
    #[allow(dead_code, reason = "Browser-only file picker action")]
    OpenIso,
    DiscardChanges(bool),
}
pub struct ConfiguratorState {
    pub open: bool,
    pub page: Page,
    pub draft: MachineSpec,
    pub active: MachineSpec,
    pub profile_text: String,
    pub profile_editor: iced::widget::text_editor::Content,
    pub profile_path: String,
    pub save_path: String,
    pub message: Option<String>,
    pub pending: bool,
    pub discard_changes: bool,
    pub controllers: Vec<String>,
}

impl Default for ConfiguratorState {
    fn default() -> Self {
        Self {
            open: false,
            page: Page::Machine,
            draft: MachineSpec::default(),
            active: MachineSpec::default(),
            profile_text: String::new(),
            profile_editor: iced::widget::text_editor::Content::new(),
            profile_path: "machine.toml".into(),
            save_path: "game.doslet-save".into(),
            message: None,
            pending: false,
            discard_changes: false,
            controllers: Vec::new(),
        }
    }
}
impl ConfiguratorState {
    #[cfg(not(target_arch = "wasm32"))]
    pub fn presentation_copy(&self) -> Self {
        Self {
            open: self.open,
            page: self.page,
            draft: self.draft.clone(),
            active: self.active.clone(),
            profile_text: self.profile_text.clone(),
            profile_editor: iced::widget::text_editor::Content::with_text(&self.profile_text),
            profile_path: self.profile_path.clone(),
            save_path: self.save_path.clone(),
            message: self.message.clone(),
            pending: self.pending,
            discard_changes: self.discard_changes,
            controllers: self.controllers.clone(),
        }
    }
    pub fn set_active(&mut self, spec: MachineSpec) {
        self.active = spec.clone();
        self.draft = spec;
        self.pending = false;
        self.discard_changes = false;
    }
    pub fn edit(&mut self, edit: &Edit) {
        if self.pending && !matches!(edit, Edit::Toggle) {
            return;
        }
        self.message = None;
        match edit {
            Edit::ProfileAction(action) => {
                self.profile_editor.perform(action.clone());
                self.profile_text = self.profile_editor.text();
            }
            Edit::DiscardChanges(value) => self.discard_changes = *value,
            Edit::Toggle => self.open = !self.open,
            Edit::Page(p) => self.page = *p,
            Edit::Memory(v) => self.draft.hardware.memory_mib = *v,
            Edit::CpuMhz(v) => self.draft.hardware.cpu_mhz = *v,
            Edit::X87(v) => self.draft.hardware.x87 = *v,
            Edit::Sb16(v) => self.draft.hardware.sb16 = *v,
            Edit::Mpu(v) => self.draft.hardware.mpu401 = *v,
            Edit::Irq(v) => self.draft.hardware.sb_irq = *v,
            Edit::Dma8(v) => self.draft.hardware.sb_dma8 = *v,
            Edit::Dma16(v) => self.draft.hardware.sb_dma16 = *v,
            Edit::Keyboard(v) => {
                self.draft.hardware.keyboard = if *v {
                    KeyboardMode::At
                } else {
                    KeyboardMode::Bios
                }
            }
            Edit::Gameport(v) => self.draft.hardware.gameport = *v,
            Edit::Capture(v) => self.draft.input.capture = *v,
            Edit::HideCursor(v) => self.draft.input.hide_cursor = *v,
            Edit::PadId(v) => self.draft.input.pad_id = (!v.is_empty()).then(|| v.clone()),
            Edit::Number(k, v) => match v.parse::<u8>() {
                Ok(v) => match *k {
                    "pad" => self.draft.input.pad_index = v,
                    "deadzone" => self.draft.input.deadzone_percent = v,
                    "x" => self.draft.input.axis_x = v,
                    "y" => self.draft.input.axis_y = v,
                    "b1" => self.draft.input.button_1 = v,
                    "b2" => self.draft.input.button_2 = v,
                    _ => {}
                },
                Err(_) => self.message = Some(format!("{k}: enter an integer from 0 to 255")),
            },
            Edit::Name(v) => self.draft.name = v.clone(),
            Edit::Executable(v) => self.draft.game.executable = v.clone(),
            Edit::Root(v) => {
                let kind = if cfg!(target_arch = "wasm32") {
                    SourceKind::BrowserZip
                } else {
                    SourceKind::NativeDirectory
                };
                let mount = self.c_drive();
                mount.kind = kind;
                mount.source = v.clone();
            }
            Edit::Iso(v) | Edit::Https(v) => {
                self.draft.mounts.retain(|m| m.letter == b'C');
                if !v.is_empty() {
                    self.draft.mounts.push(MountSpec {
                        letter: b'D',
                        source: v.clone(),
                        kind: if matches!(edit, Edit::Https(_)) {
                            SourceKind::HttpsIso
                        } else {
                            SourceKind::LocalIso
                        },
                        policy: WritePolicy::ReadOnly,
                        store: None,
                    });
                }
            }
            Edit::Policy(v) => {
                self.c_drive().policy = match v.as_str() {
                    "Read only" => WritePolicy::ReadOnly,
                    "Persistent changes" => WritePolicy::Persistent,
                    "Legacy direct writeback" => WritePolicy::LegacyWriteback,
                    _ => WritePolicy::Temporary,
                }
            }
            Edit::Store(v) => self.c_drive().store = (!v.is_empty()).then(|| v.clone()),
            Edit::SavePath(v) => self.save_path = v.clone(),
            Edit::Audio(v) => self.draft.output.audio = *v,
            Edit::Output(v) => self.draft.output.midi_output = v.clone(),
            Edit::GameMidi(v) => self.draft.output.game_midi = v.clone(),
            Edit::HostMidi(v) => self.draft.output.host_midi = v.clone(),
            Edit::ProfileText(v) => {
                self.profile_text = v.clone();
                self.profile_editor = iced::widget::text_editor::Content::with_text(v);
            }
            Edit::ProfilePath(v) => self.profile_path = v.clone(),
            Edit::ImportProfile => {
                match crate::machine_profile::parse_machine_profile(&self.profile_text) {
                    Ok(s) => {
                        self.draft = s;
                        self.message =
                            Some("Profile imported into draft. Apply to restart.".into());
                    }
                    Err(e) => self.message = Some(e),
                }
            }
            Edit::ExportProfile => {
                self.profile_text = crate::machine_profile::export_machine_profile(&self.draft);
                self.profile_editor =
                    iced::widget::text_editor::Content::with_text(&self.profile_text);
                self.message = Some("Version 2 profile ready below.".into());
            }
            Edit::Validate => {
                self.message = Some(self.draft.resolve().map_or_else(
                    |e| e.to_string(),
                    |_| "Configuration is valid. Apply restarts the guest.".into(),
                ))
            }
            Edit::Discard => self.draft = self.active.clone(),
            _ => {}
        }
    }
    fn c_drive(&mut self) -> &mut MountSpec {
        if !self.draft.mounts.iter().any(|m| m.letter == b'C') {
            self.draft.mounts.insert(
                0,
                MountSpec {
                    letter: b'C',
                    source: String::new(),
                    kind: if cfg!(target_arch = "wasm32") {
                        SourceKind::BrowserZip
                    } else {
                        SourceKind::NativeDirectory
                    },
                    policy: WritePolicy::Temporary,
                    store: None,
                },
            );
        }
        self.draft
            .mounts
            .iter_mut()
            .find(|m| m.letter == b'C')
            .unwrap()
    }
    pub fn preferred_height(&self) -> f32 {
        match self.page {
            Page::Machine => 350.0,
            Page::Sound => 390.0,
            Page::Input => {
                if self.draft.hardware.gameport {
                    550.0
                } else {
                    320.0
                }
            }
            Page::Drives | Page::Profile => 620.0,
            Page::Output => 410.0,
            Page::Resources => 540.0,
        }
    }
    pub fn view(&self, dirty: bool) -> Element<'_, Edit> {
        responsive(move |available| self.view_layout(available.width < 590.0, dirty)).into()
    }
    fn view_layout(&self, narrow: bool, dirty: bool) -> Element<'_, Edit> {
        use crate::ui::{ACCENT, MUTED, field, ghost, primary, secondary, segment};
        use iced::widget::rule;
        let s = &self.draft;
        let h = s.hardware;
        let mut body = column![].spacing(18).width(Fill);
        match self.page {
            Page::Machine => {
                body = body
                    .push(pair(
                        field("Processor", text("i386").size(15).into()),
                        field("Display", text("VGA / VBE · 512 KiB").size(15).into()),
                        narrow,
                    ))
                    .push(rule::horizontal(1))
                    .push(field(
                        "CPU clock (MHz)",
                        pick_list(
                            &[8u16, 16, 25, 33, 50, 66, 100, 132, 133, 200, 266, 500, 1000][..],
                            Some(h.cpu_mhz),
                            Edit::CpuMhz,
                        )
                        .width(Fill)
                        .text_size(14)
                        .padding(8)
                        .into(),
                    ))
                    .push(pair(
                        field(
                            "Memory (MiB)",
                            pick_list(&[4u8, 8, 16, 32][..], Some(h.memory_mib), Edit::Memory)
                                .width(Fill)
                                .text_size(14)
                                .padding(8)
                                .into(),
                        ),
                        field(
                            "Coprocessor",
                            checkbox(h.x87)
                                .label("x87")
                                .size(16)
                                .text_size(14)
                                .on_toggle(Edit::X87)
                                .into(),
                        ),
                        narrow,
                    ));
            }
            Page::Sound => {
                body = body.push(
                    row![
                        checkbox(h.sb16)
                            .label("Sound Blaster 16")
                            .size(16)
                            .text_size(14)
                            .on_toggle(Edit::Sb16),
                        space::horizontal(),
                        text("220h").size(12).color(MUTED),
                    ]
                    .align_y(iced::Center),
                );
                if h.sb16 {
                    body = body.push(
                        row![
                            field(
                                "IRQ",
                                pick_list(&[5u8, 7][..], Some(h.sb_irq), Edit::Irq)
                                    .width(Fill)
                                    .text_size(14)
                                    .padding(8)
                                    .into()
                            ),
                            field(
                                "8-bit DMA",
                                pick_list(&[0u8, 1, 3][..], Some(h.sb_dma8), Edit::Dma8)
                                    .width(Fill)
                                    .text_size(14)
                                    .padding(8)
                                    .into()
                            ),
                            field(
                                "16-bit DMA",
                                pick_list(&[5u8, 6, 7][..], Some(h.sb_dma16), Edit::Dma16)
                                    .width(Fill)
                                    .text_size(14)
                                    .padding(8)
                                    .into()
                            ),
                        ]
                        .spacing(12),
                    );
                }
                body = body.push(rule::horizontal(1)).push(
                    row![
                        checkbox(h.mpu401)
                            .label("MPU-401 MIDI")
                            .size(16)
                            .text_size(14)
                            .on_toggle(Edit::Mpu),
                        space::horizontal(),
                        text("330h").size(12).color(MUTED),
                    ]
                    .align_y(iced::Center),
                );
            }
            Page::Input => {
                body = body
                    .push(pair(
                        checkbox(h.keyboard == KeyboardMode::At)
                            .label("AT keyboard")
                            .size(16)
                            .text_size(14)
                            .on_toggle(Edit::Keyboard)
                            .into(),
                        checkbox(s.input.capture)
                            .label("Capture mouse on click")
                            .size(16)
                            .text_size(14)
                            .on_toggle(Edit::Capture)
                            .into(),
                        narrow,
                    ))
                    .push(
                        checkbox(s.input.hide_cursor)
                            .label("Hide cursor over the game")
                            .size(16)
                            .text_size(14)
                            .on_toggle(Edit::HideCursor),
                    )
                    .push(
                        text("Turn off AT keyboard to use BIOS compatibility mode.")
                            .size(12)
                            .color(MUTED),
                    )
                    .push(rule::horizontal(1))
                    .push(
                        checkbox(h.gameport)
                            .label("Joystick / gameport")
                            .size(16)
                            .text_size(14)
                            .on_toggle(Edit::Gameport),
                    );
                if h.gameport {
                    for controller in &self.controllers {
                        body = body.push(text(controller).size(12).color(MUTED));
                    }
                    body = body.push(field(
                        "Controller ID · optional",
                        text_input(
                            "Use controller index",
                            s.input.pad_id.as_deref().unwrap_or(""),
                        )
                        .on_input(Edit::PadId)
                        .size(14)
                        .padding(8)
                        .into(),
                    ));
                    for ((left, left_key, left_value), (right, right_key, right_value)) in [
                        (
                            ("Controller index", "pad", s.input.pad_index),
                            ("Dead zone %", "deadzone", s.input.deadzone_percent),
                        ),
                        (
                            ("Horizontal axis", "x", s.input.axis_x),
                            ("Vertical axis", "y", s.input.axis_y),
                        ),
                        (
                            ("First button", "b1", s.input.button_1),
                            ("Second button", "b2", s.input.button_2),
                        ),
                    ] {
                        body = body.push(pair(
                            field(
                                left,
                                text_input("0", &left_value.to_string())
                                    .on_input(move |v| Edit::Number(left_key, v))
                                    .size(14)
                                    .padding(8)
                                    .into(),
                            ),
                            field(
                                right,
                                text_input("0", &right_value.to_string())
                                    .on_input(move |v| Edit::Number(right_key, v))
                                    .size(14)
                                    .padding(8)
                                    .into(),
                            ),
                            false,
                        ));
                    }
                }
            }
            Page::Drives => {
                let c = s.mounts.iter().find(|m| m.letter == b'C');
                let cd = s.mounts.iter().find(|m| m.letter != b'C');
                body = body
                    .push(text("C:  Game files").size(14))
                    .push(field(
                        if cfg!(target_arch = "wasm32") {
                            "ZIP binding"
                        } else {
                            "Installation folder"
                        },
                        text_input("Game files", c.map_or("", |m| m.source.as_str()))
                            .on_input(Edit::Root)
                            .size(14)
                            .padding(8)
                            .into(),
                    ))
                    .push(field(
                        "Executable",
                        text_input("GAME.EXE", &s.game.executable)
                            .on_input(Edit::Executable)
                            .size(14)
                            .padding(8)
                            .into(),
                    ));
                let policy = c.map_or("Temporary changes", |m| match m.policy {
                    WritePolicy::ReadOnly => "Read only",
                    WritePolicy::Temporary => "Temporary changes",
                    WritePolicy::Persistent => "Persistent changes",
                    WritePolicy::LegacyWriteback => "Legacy direct writeback",
                });
                let policies: Vec<String> = if cfg!(target_arch = "wasm32") {
                    vec![
                        "Read only".into(),
                        "Temporary changes".into(),
                        "Persistent changes".into(),
                    ]
                } else {
                    vec![
                        "Read only".into(),
                        "Temporary changes".into(),
                        "Persistent changes".into(),
                        "Legacy direct writeback".into(),
                    ]
                };
                body = body.push(field(
                    "Save policy",
                    pick_list(policies, Some(policy.to_string()), Edit::Policy)
                        .width(Fill)
                        .text_size(14)
                        .padding(8)
                        .into(),
                ));
                if c.is_some_and(|m| m.policy == WritePolicy::Persistent) {
                    body = body.push(field(
                        if cfg!(target_arch = "wasm32") {
                            "Save store name"
                        } else {
                            "Save directory"
                        },
                        text_input(
                            "Save store",
                            c.and_then(|m| m.store.as_deref()).unwrap_or(""),
                        )
                        .on_input(Edit::Store)
                        .size(14)
                        .padding(8)
                        .into(),
                    ));
                }
                body = body
                    .push(rule::horizontal(1))
                    .push(text("D:  CD-ROM").size(14))
                    .push(field(
                        "Local ISO",
                        text_input(
                            "No disc",
                            cd.filter(|m| m.kind == SourceKind::LocalIso)
                                .map_or("", |m| m.source.as_str()),
                        )
                        .on_input(Edit::Iso)
                        .size(14)
                        .padding(8)
                        .into(),
                    ));
                #[cfg(target_arch = "wasm32")]
                {
                    body = body.push(
                        button(text("Choose ISO…").size(13))
                            .style(secondary)
                            .padding([7, 12])
                            .on_press(Edit::OpenIso),
                    );
                }
                body = body
                    .push(field(
                        "Or stream an ISO",
                        text_input(
                            "https://…",
                            cd.filter(|m| m.kind == SourceKind::HttpsIso)
                                .map_or("", |m| m.source.as_str()),
                        )
                        .on_input(Edit::Https)
                        .size(14)
                        .padding(8)
                        .into(),
                    ))
                    .push(rule::horizontal(1))
                    .push(text("Save transfer").size(14));
                #[cfg(not(target_arch = "wasm32"))]
                {
                    body = body.push(field(
                        "Snapshot file",
                        text_input("game.doslet-save", &self.save_path)
                            .on_input(Edit::SavePath)
                            .size(14)
                            .padding(8)
                            .into(),
                    ));
                }
                body = body.push(
                    row![
                        button(text("Export saves").size(13))
                            .style(secondary)
                            .padding([7, 12])
                            .on_press(Edit::ExportSaves),
                        button(text("Import saves").size(13))
                            .style(secondary)
                            .padding([7, 12])
                            .on_press(Edit::ImportSaves),
                    ]
                    .spacing(8),
                );
            }
            Page::Output => {
                body = body
                    .push(
                        checkbox(s.output.audio)
                            .label("Enable audio")
                            .size(16)
                            .text_size(14)
                            .on_toggle(Edit::Audio),
                    )
                    .push(field(
                        "MIDI output",
                        text_input(
                            if cfg!(target_os = "macos") {
                                "rust, apple, moont, or port name"
                            } else {
                                "rust, moont, or port name"
                            },
                            &s.output.midi_output,
                        )
                        .on_input(Edit::Output)
                        .size(14)
                        .padding(8)
                        .into(),
                    ))
                    .push(pair(
                        field(
                            "Game MIDI",
                            pick_list(
                                vec!["gm".to_string(), "mt32".into()],
                                Some(s.output.game_midi.clone()),
                                Edit::GameMidi,
                            )
                            .width(Fill)
                            .text_size(14)
                            .padding(8)
                            .into(),
                        ),
                        field(
                            "Output MIDI",
                            pick_list(
                                vec!["gm".to_string(), "mt32".into()],
                                Some(s.output.host_midi.clone()),
                                Edit::HostMidi,
                            )
                            .width(Fill)
                            .text_size(14)
                            .padding(8)
                            .into(),
                        ),
                        narrow,
                    ));
            }
            Page::Profile => {
                body = body.push(field(
                    "Profile name",
                    text_input("Untitled", &s.name)
                        .on_input(Edit::Name)
                        .size(14)
                        .padding(8)
                        .into(),
                ));
                #[cfg(not(target_arch = "wasm32"))]
                {
                    body = body.push(field(
                        "Profile file",
                        text_input("machine.toml", &self.profile_path)
                            .on_input(Edit::ProfilePath)
                            .size(14)
                            .padding(8)
                            .into(),
                    ));
                }
                body = body
                    .push(
                        row![
                            button(text("Import file…").size(13))
                                .style(secondary)
                                .padding([7, 12])
                                .on_press(Edit::LoadProfileFile),
                            button(text("Export file…").size(13))
                                .style(secondary)
                                .padding([7, 12])
                                .on_press(Edit::SaveProfileFile),
                        ]
                        .spacing(8),
                    )
                    .push(rule::horizontal(1))
                    .push(field(
                        "TOML",
                        iced::widget::text_editor(&self.profile_editor)
                            .on_action(Edit::ProfileAction)
                            .height(210)
                            .size(13)
                            .into(),
                    ))
                    .push(
                        row![
                            button(text("Use this text").size(13))
                                .style(secondary)
                                .padding([7, 12])
                                .on_press(Edit::ImportProfile),
                            button(text("Update from settings").size(13))
                                .style(ghost)
                                .padding([7, 12])
                                .on_press(Edit::ExportProfile),
                        ]
                        .spacing(8),
                    );
            }
            Page::Resources => match s.resolve() {
                Ok(plan) => {
                    body = body.push(row![
                        text("Device").size(12).color(MUTED).width(Fill),
                        text("I/O ports").size(12).color(MUTED)
                    ]);
                    for c in plan.io {
                        body = body.push(row![
                            text(c.owner).size(13).width(Fill),
                            text(format!("{:04X}–{:04X}", c.first, c.last)).size(13)
                        ]);
                    }
                    if h.sb16 {
                        body = body.push(rule::horizontal(1)).push(
                            text(format!(
                                "Sound Blaster · IRQ {} · DMA {} / {}",
                                h.sb_irq, h.sb_dma8, h.sb_dma16
                            ))
                            .size(12)
                            .color(MUTED),
                        );
                    }
                }
                Err(e) => body = body.push(text(e.to_string()).size(13)),
            },
        }
        let changed = self.draft != self.active;
        let header = row![
            text("Machine settings").size(18),
            space::horizontal(),
            text(if self.pending {
                "Preparing…"
            } else if changed {
                "Edited"
            } else {
                ""
            })
            .size(12)
            .color(ACCENT),
            button(text("Close").size(13))
                .padding([6, 10])
                .style(ghost)
                .on_press(Edit::Toggle),
        ]
        .spacing(12)
        .align_y(iced::Center);
        let mut tabs = row![].spacing(3);
        for page in Page::ALL {
            let selected = page == self.page;
            tabs = tabs.push(
                button(text(page.to_string()).size(13))
                    .padding([8, 12])
                    .style(move |theme, status| segment(theme, status, selected))
                    .on_press_maybe((!self.pending).then_some(Edit::Page(page))),
            );
        }
        let tabs = scrollable(tabs)
            .direction(scrollable::Direction::Horizontal(
                scrollable::Scrollbar::default().width(2).scroller_width(2),
            ))
            .width(Fill)
            .height(42);
        let mut footer = column![].spacing(10);
        if let Some(message) = &self.message {
            footer = footer.push(text(message).size(12).color(MUTED));
        }
        if dirty {
            footer = footer.push(
                checkbox(self.discard_changes)
                    .label("Discard unsaved game changes on restart")
                    .size(14)
                    .text_size(12)
                    .on_toggle(Edit::DiscardChanges),
            );
        }
        let apply = button(
            text(if self.pending {
                "Preparing…"
            } else {
                "Apply & restart"
            })
            .size(13),
        )
        .padding([8, 14])
        .style(primary)
        .on_press_maybe((!self.pending).then_some(Edit::Apply));
        footer = footer.push(
            row![
                button(text("Validate").size(13))
                    .style(ghost)
                    .padding([7, 10])
                    .on_press_maybe((!self.pending).then_some(Edit::Validate)),
                button(text("Revert").size(13))
                    .style(ghost)
                    .padding([7, 10])
                    .on_press_maybe((!self.pending && changed).then_some(Edit::Discard)),
                space::horizontal(),
                apply,
            ]
            .spacing(4)
            .align_y(iced::Center),
        );
        column![
            container(header).padding([14, 18]).width(Fill),
            container(tabs).padding([0, 12]).width(Fill),
            rule::horizontal(1),
            scrollable(
                container(body)
                    .padding(if narrow { 18 } else { 24 })
                    .width(Fill)
            )
            .height(Fill),
            rule::horizontal(1),
            container(footer).padding([12, 16]).width(Fill),
        ]
        .width(Fill)
        .height(Fill)
        .into()
    }
}

fn pair<'a>(left: Element<'a, Edit>, right: Element<'a, Edit>, narrow: bool) -> Element<'a, Edit> {
    if narrow {
        column![left, right].spacing(14).width(Fill).into()
    } else {
        row![container(left).width(Fill), container(right).width(Fill)]
            .spacing(20)
            .align_y(iced::Center)
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_and_failed_profile_import_preserve_active_recipe() {
        let mut editor = ConfiguratorState::default();
        editor.edit(&Edit::Memory(8));
        editor.edit(&Edit::Sb16(false));
        assert_eq!(editor.active.hardware.memory_mib, 16);
        assert!(editor.active.hardware.sb16);
        let draft = editor.draft.clone();
        editor.edit(&Edit::ProfileText(
            "version=2\n[machine]\ncpu='pentium'".into(),
        ));
        editor.edit(&Edit::ImportProfile);
        assert_eq!(editor.draft, draft);
        assert!(editor.message.as_deref().unwrap().contains("cpu"));
        editor.edit(&Edit::Discard);
        assert_eq!(editor.draft, editor.active);
    }
    #[test]
    fn pending_preparation_freezes_the_candidate() {
        let mut editor = ConfiguratorState {
            pending: true,
            ..Default::default()
        };
        editor.edit(&Edit::Memory(4));
        assert_eq!(editor.draft.hardware.memory_mib, 16);
        editor.edit(&Edit::Toggle);
        assert!(editor.open);
    }
}
