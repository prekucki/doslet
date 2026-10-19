//! Window state contains snapshots and pixels, never an emulation Session.
use crate::presentation::{DisplayFrame, DisplaySizing, SizingMessage};
use doslet_midi_routing::Mode;
use doslet_runtime::SessionStatus;
use doslet_x86::{Gpr16, Registers};
use iced::{Element, Subscription, Task, Theme};
use js_sys::{Array, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;

#[wasm_bindgen(module = "/src/browser/bridge.js")]
extern "C" {
    type Browser;
    #[wasm_bindgen(constructor, catch)]
    fn new() -> Result<Browser, JsValue>;
    #[wasm_bindgen(method, catch)]
    fn command(this: &Browser, kind: &str, program: &str) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch, js_name = applyMachine)]
    fn apply_machine(
        this: &Browser,
        profile: &str,
        program: &str,
        discard_changes: bool,
    ) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch, js_name = physicalKey)]
    fn physical_key(
        this: &Browser,
        usage: u16,
        pressed: bool,
        enabled: bool,
    ) -> Result<(), JsValue>;
    #[wasm_bindgen(method, js_name = releaseInput)]
    fn release_input(this: &Browser);
    #[wasm_bindgen(method, js_name = inputCapture)]
    fn input_capture(this: &Browser, enabled: bool);
    #[wasm_bindgen(method, js_name = mouseMode)]
    fn mouse_mode(this: &Browser, mode: &str);
    #[wasm_bindgen(method, catch, js_name = chooseProfile)]
    fn choose_profile(this: &Browser) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = downloadProfile)]
    fn download_profile(this: &Browser, text: &str) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch, js_name = chooseIso)]
    fn choose_iso(this: &Browser) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = exportSaves)]
    fn export_saves(this: &Browser) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = importSaves)]
    fn import_saves(this: &Browser) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = keyEvent)]
    fn key_event(
        this: &Browser,
        scan: u8,
        ascii: u8,
        modifiers: u8,
        pressed: bool,
    ) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch, js_name = pointerEvent)]
    fn pointer_event(
        this: &Browser,
        x: u16,
        y: u16,
        button: i8,
        pressed: bool,
    ) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch, js_name = captureMouse)]
    fn capture_mouse(this: &Browser) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch, js_name = importZip)]
    fn import_zip(this: &Browser, bytes: &[u8]) -> Result<(), JsValue>;
    #[wasm_bindgen(method)]
    fn poll(this: &Browser) -> JsValue;
    #[wasm_bindgen(method, js_name = releaseFrame)]
    fn release_frame(this: &Browser);
    #[wasm_bindgen(method, catch, js_name = resumeAudio)]
    fn resume_audio(this: &Browser) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(method, js_name = toggleMute)]
    fn toggle_mute(this: &Browser);
    #[wasm_bindgen(method, js_name = setVolume)]
    fn set_volume(this: &Browser, volume: u8);
    #[wasm_bindgen(method, catch, js_name = listMidi)]
    fn list_midi(this: &Browser) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = selectMidi)]
    fn select_midi(
        this: &Browser,
        selection: &str,
        dialect: &str,
        game: &str,
    ) -> Result<js_sys::Promise, JsValue>;
}
pub struct Graphics {
    pub height: usize,
}
pub struct Snapshot {
    status: SessionStatus,
    steps: u64,
    registers: Registers,
    graphics: Option<Graphics>,
    fault: Option<String>,
    waiting_for_io: bool,
    pending_commit: bool,
    dirty: bool,
    pointer_in_use: bool,
}
impl Snapshot {
    pub fn status(&self) -> SessionStatus {
        self.status
    }
    pub fn steps(&self) -> u64 {
        self.steps
    }
    pub fn dirty_overlay(&self) -> bool {
        self.dirty
    }
    pub fn storage_waiting(&self) -> bool {
        self.waiting_for_io
    }
    pub fn pending_commit(&self) -> bool {
        self.pending_commit
    }
    pub fn registers(&self) -> &Registers {
        &self.registers
    }
    pub fn frame(&self) -> Option<&Graphics> {
        self.graphics.as_ref()
    }
    pub fn fault(&self) -> Option<&str> {
        self.fault.as_deref()
    }
}
pub struct Archive {
    pub programs: Vec<String>,
}
pub struct Doslet {
    pub cpu_settings: crate::cpu_config::Settings,
    pub cpu_tiers_available: bool,
    pub(crate) disk_activity: crate::disk_activity::Activity,
    pub video_mode_label: String,
    pub audio_volume: u8,
    pub audio_use: doslet_runtime::AudioUse,
    pub memory_usage: doslet_runtime::session::MemoryUsage,
    pub(crate) cpu_activity: Option<crate::cpu_meter::Activity>,
    cpu_meter: crate::cpu_meter::Meter,
    pub session: Snapshot,
    pub workspace: crate::ui::Workspace,
    pub configurator: crate::configurator::ConfiguratorState,
    pub display: DisplayFrame,
    pub display_sizing: DisplaySizing,
    pub name: String,
    pub notice: Option<String>,
    pub archive: Option<Archive>,
    pub program: Option<String>,
    pub importing: bool,
    pub midi_choices: Vec<String>,
    pub midi_choice: String,
    pub midi_mode: Mode,
    pub midi_output_mode: Mode,
    pub midi_activity: crate::midi_activity::MidiActivity,
    pub midi_channels: [doslet_midi_routing::Channel; 16],
    midi_summary: String,
    bridge: Option<Browser>,
    audio_state: String,
    audio_ready: bool,
    muted: bool,
    notes: u64,
    frames: u64,
    underruns: u64,
    peak: f64,
    spectrum: [f32; doslet_pcm::spectrum::BANDS],
    guest_pcm: (u32, u8, u8),
    host_messages: u64,
    pcm_frames: u64,
    mouse_captured: bool,
    mouse_relative: bool,
    storage_policy: Option<String>,
}
#[derive(Clone, Debug)]
pub enum Message {
    Cpu(crate::cpu_config::Edit),
    DisplaySizing(SizingMessage),
    Ui(crate::ui::Action),
    Config(crate::configurator::Edit),
    ProfilePicked(Result<Option<String>, String>),
    ConfigIoFinished(&'static str, Result<(), String>),
    PointerMoved(u16, u16),
    PointerButton(u8, bool),
    PointerLeft,
    Keyboard(iced::keyboard::Event),
    KeyboardCaptured(iced::keyboard::Event),
    RunPause,
    Step,
    Reset,
    Tick,
    OpenArchive,
    ArchiveLoaded(Result<Option<std::sync::Arc<Vec<u8>>>, String>),
    ProgramSelected(String),
    StartArchive,
    ToggleAudio,
    AudioResumed(Result<(), String>),
    RefreshMidi,
    MidiPorts(Result<Vec<String>, String>),
    AudioVolume(u8),
    MidiSelected(String),
    MidiModeSelected(Mode),
    MidiOutputModeSelected(Mode),
    MidiSwitched(String, Result<(), String>),
}
impl Doslet {
    pub fn new() -> Self {
        let (bridge, notice) = match Browser::new() {
            Ok(bridge) => (Some(bridge), None),
            Err(error) => (None, Some(format!("Worker startup failed: {error:?}"))),
        };
        Self {
            cpu_settings: crate::cpu_config::Settings::default(),
            cpu_tiers_available: false,
            cpu_activity: None,
            disk_activity: crate::disk_activity::Activity::new(0, 0),
            video_mode_label: "—".into(),
            audio_volume: 100,
            audio_use: doslet_runtime::AudioUse::default(),
            memory_usage: doslet_runtime::session::MemoryUsage::default(),
            cpu_meter: crate::cpu_meter::Meter::default(),
            workspace: crate::ui::Workspace::default(),
            configurator: crate::configurator::ConfiguratorState::default(),
            session: Snapshot {
                status: SessionStatus::Ready,
                steps: 0,
                registers: Registers::default(),
                graphics: None,
                fault: None,
                waiting_for_io: false,
                pending_commit: false,
                dirty: false,
                pointer_in_use: false,
            },
            display: DisplayFrame::default(),
            display_sizing: DisplaySizing::default(),
            name: "Color bars".into(),
            notice,
            archive: None,
            program: None,
            importing: false,
            midi_choices: vec!["Rust synth".into()],
            midi_choice: "Rust synth".into(),
            midi_mode: Mode::Gm,
            midi_output_mode: Mode::Gm,
            midi_activity: crate::midi_activity::MidiActivity::default(),
            midi_channels: [doslet_midi_routing::Channel::default(); 16],
            midi_summary: String::new(),
            bridge,
            audio_state: "inactive".into(),
            audio_ready: false,
            muted: false,
            notes: 0,
            frames: 0,
            underruns: 0,
            peak: 0.0,
            spectrum: [0.0; doslet_pcm::spectrum::BANDS],
            guest_pcm: (0, 0, 0),
            host_messages: 0,
            pcm_frames: 0,
            mouse_captured: false,
            mouse_relative: false,
            storage_policy: None,
        }
    }
    fn command(&mut self, kind: &str, program: &str) {
        if let Some(bridge) = &self.bridge
            && let Err(error) = bridge.command(kind, program)
        {
            self.notice = Some(format!("Worker command failed: {error:?}"));
        }
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        if matches!(
            &message,
            Message::Keyboard(iced::keyboard::Event::KeyPressed {
                key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                ..
            })
        ) && (self.configurator.open || self.workspace.panel.is_some())
        {
            self.configurator.open = false;
            self.workspace.panel = None;
            if let Some(bridge) = &self.bridge {
                bridge.release_input();
            }
            return Task::none();
        }
        if let Message::Keyboard(event) | Message::KeyboardCaptured(event) = &message {
            let enabled = self.session.status() == SessionStatus::Running
                && !self.configurator.open
                && self.workspace.panel.is_none()
                && !self.configurator.pending
                && !matches!(message, Message::KeyboardCaptured(_));
            if let Some(bridge) = &self.bridge {
                if self.configurator.active.hardware.keyboard == doslet_config::KeyboardMode::At {
                    if let Some((usage, pressed)) = crate::keyboard::physical(event)
                        && let Err(error) = bridge.physical_key(usage, pressed, enabled)
                    {
                        self.notice = Some(format!("Keyboard: {error:?}"));
                    }
                } else if enabled {
                    let (modifiers, key) = crate::keyboard::translate(event);
                    let (scan, ascii) = key.unwrap_or((0, 0));
                    if let Err(error) = bridge.key_event(scan, ascii, modifiers, key.is_some()) {
                        self.notice = Some(format!("Keyboard: {error:?}"));
                    }
                }
            }
            return Task::none();
        }
        match message {
            Message::Ui(action) => match action {
                crate::ui::Action::Open(panel) => {
                    if panel == crate::ui::Panel::Cpu {
                        self.workspace.cpu = Some(self.cpu_settings);
                    }
                    self.workspace.panel = Some(panel);
                    self.configurator.open = false;
                    if let Some(bridge) = &self.bridge {
                        bridge.input_capture(false);
                        bridge.release_input();
                    }
                }
                crate::ui::Action::Close => {
                    self.workspace.panel = None;
                    self.configurator.open = false;
                }
                crate::ui::Action::DismissNotice => self.notice = None,
                crate::ui::Action::AudioPage(page) => self.workspace.audio_page = page,
            },
            Message::Cpu(edit) => {
                use crate::cpu_config::{Core, Edit, Mode};
                match edit {
                    Edit::Revert => self.workspace.cpu = Some(self.cpu_settings),
                    Edit::Apply => {
                        if let Some(settings) = self.workspace.cpu
                            && !self.configurator.pending
                            && settings.valid()
                            && (settings.mode != Mode::Tiers || self.cpu_tiers_available)
                            && settings.core == Core::Interpreter
                            && !settings.learning
                        {
                            let program = if settings.mode == Mode::Tiers {
                                format!(
                                    "tiers:{}:{}:{}",
                                    settings.hot.unwrap_or(0),
                                    settings.cold,
                                    settings.grey
                                )
                            } else {
                                format!(
                                    "{}:{}",
                                    if settings.mode == Mode::Adaptive {
                                        "adaptive"
                                    } else {
                                        "fixed"
                                    },
                                    settings.mhz
                                )
                            };
                            self.command("cpu-settings", &program);
                        }
                    }
                    _ => self
                        .workspace
                        .cpu
                        .get_or_insert(self.cpu_settings)
                        .edit(edit),
                }
            }
            Message::Config(edit) => {
                use crate::configurator::Edit;
                let keep_panel = match self.workspace.panel {
                    Some(crate::ui::Panel::Audio) => edit.audio_panel_edit(),
                    Some(crate::ui::Panel::Drives) => edit.drives_panel_edit(),
                    Some(crate::ui::Panel::Input) => edit.input_panel_edit(),
                    Some(crate::ui::Panel::Memory) => matches!(edit, Edit::Memory(_)),
                    _ => false,
                };
                if !keep_panel {
                    self.workspace.panel = None;
                }
                if self.configurator.pending && !matches!(edit, Edit::Toggle) {
                    return Task::none();
                }
                self.configurator.edit(&edit);
                if let Edit::Keyboard(at) = edit {
                    self.command("keyboard-mode", if at { "at" } else { "bios" });
                    return Task::none();
                }
                if let Edit::HideCursor(hidden) = edit {
                    self.configurator.active.input.hide_cursor = hidden;
                    return Task::none();
                }
                if let Edit::MouseMode(mode) = edit {
                    self.configurator.active.input.mouse_mode = mode;
                    self.mouse_relative = mode == doslet_config::MouseMode::Capture;
                    if let Some(bridge) = &self.bridge {
                        bridge.mouse_mode(mode.as_str());
                    }
                    return Task::none();
                }
                if matches!(edit, Edit::Toggle)
                    && self.configurator.open
                    && let Some(bridge) = &self.bridge
                {
                    bridge.input_capture(false);
                    bridge.release_input();
                }
                if let Some(bridge) = &self.bridge {
                    match edit {
                        Edit::LoadProfileFile => match bridge.choose_profile() {
                            Ok(p) => {
                                return Task::perform(
                                    async move {
                                        wasm_bindgen_futures::JsFuture::from(p)
                                            .await
                                            .map(|v| v.as_string())
                                            .map_err(|e| format!("Profile import failed: {e:?}"))
                                    },
                                    Message::ProfilePicked,
                                );
                            }
                            Err(e) => {
                                self.configurator.message =
                                    Some(format!("Profile import failed: {e:?}"))
                            }
                        },
                        Edit::SaveProfileFile => {
                            let text = crate::machine_profile::export_machine_profile(
                                &self.configurator.draft,
                            );
                            if let Err(e) = bridge.download_profile(&text) {
                                self.configurator.message =
                                    Some(format!("Profile download failed: {e:?}"));
                            }
                        }
                        Edit::OpenIso | Edit::ExportSaves | Edit::ImportSaves => {
                            let (label, promise) = match edit {
                                Edit::OpenIso => ("ISO", bridge.choose_iso()),
                                Edit::ExportSaves => ("Save export", bridge.export_saves()),
                                _ => ("Save import", bridge.import_saves()),
                            };
                            match promise {
                                Ok(p) => {
                                    return Task::perform(
                                        async move {
                                            wasm_bindgen_futures::JsFuture::from(p)
                                                .await
                                                .map(|_| ())
                                                .map_err(|e| format!("{label} failed: {e:?}"))
                                        },
                                        move |r| Message::ConfigIoFinished(label, r),
                                    );
                                }
                                Err(e) => {
                                    self.configurator.message =
                                        Some(format!("{label} failed: {e:?}"))
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if matches!(edit, Edit::ApplyAudio) && self.configurator.audio_only_changes() {
                    let profile =
                        crate::machine_profile::export_machine_profile(&self.configurator.draft);
                    if let Some(bridge) = &self.bridge {
                        match bridge.command("audio-hardware", &profile) {
                            Ok(()) => self.configurator.pending = true,
                            Err(error) => {
                                self.configurator.message =
                                    Some(format!("Audio hardware: {error:?}"))
                            }
                        }
                    }
                    return Task::none();
                }
                if matches!(edit, Edit::Apply) {
                    let audio = if self.configurator.draft.output.audio {
                        self.resume_audio()
                    } else {
                        Task::none()
                    };
                    match self.configurator.draft.resolve() {
                        Ok(_) => {
                            if let Some(bridge) = &self.bridge {
                                let profile = crate::machine_profile::export_machine_profile(
                                    &self.configurator.draft,
                                );
                                let program = self
                                    .program
                                    .as_deref()
                                    .unwrap_or(&self.configurator.draft.game.executable);
                                match bridge.apply_machine(
                                    &profile,
                                    program,
                                    self.configurator.discard_changes,
                                ) {
                                    Ok(()) => {
                                        self.configurator.pending = true;
                                        self.configurator.message =
                                            Some("Preparing machine and sources…".into());
                                    }
                                    Err(e) => {
                                        self.configurator.message =
                                            Some(format!("Machine preparation failed: {e:?}"))
                                    }
                                }
                            }
                        }
                        Err(e) => self.configurator.message = Some(e.to_string()),
                    }
                    return audio;
                }
            }
            Message::ProfilePicked(result) => match result {
                Ok(Some(text)) => {
                    self.configurator
                        .edit(&crate::configurator::Edit::ProfileText(text));
                    self.configurator
                        .edit(&crate::configurator::Edit::ImportProfile);
                }
                Ok(None) => {}
                Err(e) => self.configurator.message = Some(e),
            },
            Message::ConfigIoFinished(label, result) => match result {
                Ok(()) => {
                    if label == "ISO" {
                        self.configurator
                            .edit(&crate::configurator::Edit::Iso("selected-iso".into()));
                    }
                    self.configurator.message = Some(format!("{label} completed."));
                }
                Err(e) => self.configurator.message = Some(e),
            },
            Message::PointerMoved(x, y) => {
                if self.configurator.open || self.workspace.panel.is_some() {
                    return Task::none();
                }
                if let Some(bridge) = &self.bridge
                    && let Err(error) = bridge.pointer_event(x, y, -1, false)
                {
                    self.notice = Some(format!("Mouse: {error:?}"));
                }
            }
            Message::PointerButton(button, pressed) => {
                if pressed && (self.configurator.open || self.workspace.panel.is_some()) {
                    return Task::none();
                }
                if pressed
                    && !self.mouse_captured
                    && self.session.status() == SessionStatus::Running
                    && self.mouse_relative
                {
                    if let Some(bridge) = &self.bridge
                        && let Err(error) = bridge.capture_mouse()
                    {
                        self.notice = Some(format!("Mouse capture: {error:?}"));
                    }
                    return Task::none();
                }
                if let Some(bridge) = &self.bridge
                    && let Err(error) = bridge.pointer_event(0, 0, button as i8, pressed)
                {
                    self.notice = Some(format!("Mouse: {error:?}"));
                }
            }
            Message::PointerLeft => {
                if let Some(bridge) = &self.bridge {
                    let _ = bridge.pointer_event(0, 0, -2, false);
                }
            }
            Message::Keyboard(_) | Message::KeyboardCaptured(_) => unreachable!("handled above"),
            Message::RefreshMidi => {
                if let Some(bridge) = &self.bridge {
                    match bridge.list_midi() {
                        Ok(promise) => {
                            return Task::perform(
                                async move {
                                    wasm_bindgen_futures::JsFuture::from(promise)
                                        .await
                                        .map(|value| {
                                            Array::from(&value)
                                                .iter()
                                                .filter_map(|v| v.as_string())
                                                .collect()
                                        })
                                        .map_err(|error| format!("Web MIDI failed: {error:?}"))
                                },
                                Message::MidiPorts,
                            );
                        }
                        Err(error) => self.notice = Some(format!("Web MIDI failed: {error:?}")),
                    }
                }
            }
            Message::MidiPorts(result) => match result {
                Ok(ports) => {
                    self.midi_choices = vec!["Rust synth".into()];
                    if self.configurator.active.output.rom_binding.as_deref() == Some("bundle")
                        || self.midi_choice == "CM-32L / MT-32"
                    {
                        self.midi_choices.push("CM-32L / MT-32".into());
                    }
                    self.midi_choices.extend(ports);
                    self.notice = None;
                }
                Err(error) => self.notice = Some(error),
            },
            Message::MidiModeSelected(mode) => {
                self.midi_mode = mode;
                self.command("midi-mode", mode.as_str());
                self.configurator.remember_midi(
                    self.midi_mode.as_str(),
                    self.midi_output_mode.as_str(),
                    &self.midi_choice,
                );
                if self.midi_choice == "CM-32L / MT-32" {
                    return self.update(Message::MidiSelected(self.midi_choice.clone()));
                }
            }
            Message::MidiOutputModeSelected(mode) => {
                self.midi_output_mode = mode;
                // Reopen through the bridge so raw MT-32 requests SysEx access.
                self.command("output-mode", mode.as_str());
                self.configurator.remember_midi(
                    self.midi_mode.as_str(),
                    self.midi_output_mode.as_str(),
                    &self.midi_choice,
                );
                if !matches!(self.midi_choice.as_str(), "Rust synth" | "CM-32L / MT-32") {
                    return self.update(Message::MidiSelected(self.midi_choice.clone()));
                }
            }
            Message::MidiSelected(selection) => {
                if let Some(bridge) = &self.bridge {
                    match bridge.select_midi(
                        &selection,
                        self.midi_output_mode.as_str(),
                        self.midi_mode.as_str(),
                    ) {
                        Ok(promise) => {
                            return Task::perform(
                                async move {
                                    let result = wasm_bindgen_futures::JsFuture::from(promise)
                                        .await
                                        .map(|_| ())
                                        .map_err(|error| format!("MIDI output failed: {error:?}"));
                                    (selection, result)
                                },
                                |(selection, result)| Message::MidiSwitched(selection, result),
                            );
                        }
                        Err(error) => self.notice = Some(format!("MIDI output failed: {error:?}")),
                    }
                }
            }
            Message::MidiSwitched(selection, result) => match result {
                Ok(()) => {
                    self.midi_choice = selection;
                    self.configurator.remember_midi(
                        self.midi_mode.as_str(),
                        self.midi_output_mode.as_str(),
                        &self.midi_choice,
                    );
                    self.notice = None;
                }
                Err(error) => self.notice = Some(error),
            },
            Message::AudioVolume(volume) => {
                self.audio_volume = volume.min(100);
                if let Some(bridge) = &self.bridge {
                    bridge.set_volume(self.audio_volume);
                }
            }
            Message::DisplaySizing(message) => {
                if matches!(
                    message,
                    SizingMessage::Filter(crate::presentation::DisplayFilter::Xbrz)
                ) && !crate::browser_diagnostics::gpu_canvas_active()
                {
                    log::warn!("xBRZ unavailable: the active renderer is Canvas2D");
                    self.notice = Some(
                        "xBRZ requires GPU rendering. Enable browser hardware acceleration and reload the page."
                            .into(),
                    );
                    return Task::none();
                }
                return self
                    .display_sizing
                    .update(message, &self.display)
                    .map(Message::DisplaySizing);
            }
            Message::Tick => {
                if let Some(bridge) = &self.bridge {
                    bridge.input_capture(!self.configurator.open && self.workspace.panel.is_none());
                }
                self.poll();
                return self
                    .display_sizing
                    .refresh(&self.display)
                    .map(Message::DisplaySizing);
            }
            Message::RunPause => {
                self.command(
                    if self.session.status() == SessionStatus::Running {
                        "pause"
                    } else {
                        "run"
                    },
                    "",
                );
            }
            Message::Step => self.command("step", ""),
            Message::Reset => {
                let discard = self.configurator.discard_changes;
                self.command("reset", if discard { "discard" } else { "" });
            }
            Message::OpenArchive => {
                self.command("pause", "");
                match crate::browser_input::pick_zip() {
                    Ok(selection) => {
                        self.importing = true;
                        return Task::perform(selection, Message::ArchiveLoaded);
                    }
                    Err(error) => self.notice = Some(error),
                }
            }
            Message::ArchiveLoaded(result) => match result {
                Ok(Some(bytes)) => {
                    if let Some(bridge) = &self.bridge {
                        if let Err(error) = bridge.import_zip(&bytes) {
                            self.importing = false;
                            self.notice = Some(format!("ZIP transfer failed: {error:?}"));
                        }
                    } else {
                        self.importing = false;
                    }
                }
                Ok(None) => self.importing = false,
                Err(error) => {
                    self.importing = false;
                    self.notice = Some(error);
                }
            },
            Message::ProgramSelected(program) => {
                self.configurator
                    .edit(&crate::configurator::Edit::Executable(program.clone()));
                self.program = Some(program);
            }
            Message::StartArchive => {
                if let Some(program) = self.program.clone() {
                    self.configurator
                        .edit(&crate::configurator::Edit::Executable(program));
                }
                return self.update(Message::Config(crate::configurator::Edit::Apply));
            }
            Message::ToggleAudio => {
                if !self.uses_local_audio() || (self.audio_ready && self.audio_state == "running") {
                    if let Some(bridge) = &self.bridge {
                        bridge.toggle_mute();
                    }
                } else {
                    return self.resume_audio();
                }
            }
            Message::AudioResumed(result) => match result {
                Ok(()) => {
                    log::info!("Web Audio worklet resumed");
                    if self
                        .notice
                        .as_deref()
                        .is_some_and(|text| text.starts_with("Web Audio"))
                    {
                        self.notice = None;
                    }
                }
                Err(error) => self.notice = Some(error),
            },
        }
        Task::none()
    }
    fn resume_audio(&mut self) -> Task<Message> {
        let Some(bridge) = &self.bridge else {
            return Task::none();
        };
        match bridge.resume_audio() {
            Ok(promise) => Task::perform(
                async move {
                    wasm_bindgen_futures::JsFuture::from(promise)
                        .await
                        .map(|_| ())
                        .map_err(|error| format!("Web Audio failed: {error:?}"))
                },
                Message::AudioResumed,
            ),
            Err(error) => Task::done(Message::AudioResumed(Err(format!(
                "Web Audio failed: {error:?}"
            )))),
        }
    }
    fn poll(&mut self) {
        let Some(bridge) = &self.bridge else {
            return;
        };
        let packet = bridge.poll();
        self.configurator.controllers = Array::from(&field(&packet, "gamepads"))
            .iter()
            .map(|pad| {
                format!(
                    "{}: {}",
                    number(&pad, "index") as usize,
                    field(&pad, "id").as_string().unwrap_or_default()
                )
            })
            .collect();
        self.mouse_captured = field(&packet, "mouseCaptured").as_bool().unwrap_or(false);
        let state = field(&packet, "state");
        self.mouse_relative = field(&state, "pointerRelative").as_bool().unwrap_or(false);
        self.guest_pcm = (
            number(&state, "guestPcmRate") as u32,
            number(&state, "guestPcmBits") as u8,
            number(&state, "guestPcmChannels") as u8,
        );
        if !state.is_null() && !state.is_undefined() {
            let used = number(&state, "audioUse") as u8;
            self.audio_use = doslet_runtime::AudioUse {
                sb16: used & 1 != 0,
                opl3: used & 2 != 0,
                mpu401: used & 4 != 0,
            };
            let keyboard =
                field(&state, "keyboardMode")
                    .as_string()
                    .and_then(|mode| match mode.as_str() {
                        "at" => Some(doslet_config::KeyboardMode::At),
                        "bios" => Some(doslet_config::KeyboardMode::Bios),
                        _ => None,
                    });
            if let Some(mode) = keyboard
                && self.configurator.active.hardware.keyboard != mode
            {
                self.configurator.active.hardware.keyboard = mode;
                self.configurator.draft.hardware.keyboard = mode;
            }
            let mhz = number(&state, "cpuMhz") as u16;
            if (1..=1000).contains(&mhz) && self.cpu_settings.mhz != mhz {
                self.cpu_settings.mhz = mhz;
                self.configurator.active.hardware.cpu_mhz = mhz;
                self.configurator.draft.hardware.cpu_mhz = mhz;
            }
            if let Some(mode) = field(&state, "cpuMode").as_string() {
                self.cpu_settings.mode = if mode == "adaptive" {
                    crate::cpu_config::Mode::Adaptive
                } else if mode == "tiers" {
                    crate::cpu_config::Mode::Tiers
                } else {
                    crate::cpu_config::Mode::Fixed
                };
            }
            let rates = field(&state, "tierRates");
            self.cpu_tiers_available =
                js_sys::Array::is_array(&rates) && js_sys::Array::from(&rates).length() == 3;
            if self.cpu_tiers_available {
                let rates = js_sys::Array::from(&rates);
                self.cpu_settings.hot = match rates.get(0).as_f64().unwrap_or(0.0) as u16 {
                    0 => None,
                    value => Some(value),
                };
                self.cpu_settings.cold = rates.get(1).as_f64().unwrap_or(8.0) as u16;
                self.cpu_settings.grey = rates.get(2).as_f64().unwrap_or(133.0) as u16;
            }
            self.session.status = match number(&state, "status") as u32 {
                0 => SessionStatus::Ready,
                1 => SessionStatus::Running,
                2 => SessionStatus::Paused,
                3 => SessionStatus::Halted,
                4 => SessionStatus::Faulted,
                code => SessionStatus::Exited((code - 256) as u8),
            };
            self.session.steps = number(&state, "steps") as u64;
            self.disk_activity.observe(
                number(&state, "epoch") as u64,
                number(&state, "diskReadBytes") as u64,
                web_time::Instant::now(),
            );
            self.video_mode_label = field(&state, "videoMode")
                .as_string()
                .unwrap_or_else(|| "—".into());
            self.midi_activity.observe(
                number(&state, "nanos") as u64,
                number(&state, "midiBytes") as u64,
                number(&state, "midiNotes") as u64,
            );
            let channels = js_sys::Uint8Array::new(&field(&state, "midiChannels")).to_vec();
            if channels.len() == 16 * doslet_midi_routing::Channel::BYTES {
                for (out, bytes) in self
                    .midi_channels
                    .iter_mut()
                    .zip(channels.chunks_exact(doslet_midi_routing::Channel::BYTES))
                {
                    if let Some(channel) = doslet_midi_routing::Channel::read(bytes) {
                        *out = channel;
                    }
                }
            }
            self.session.registers.cs = number(&state, "cs") as u16;
            self.session.registers.ip = number(&state, "ip") as u32;
            self.session
                .registers
                .set(Gpr16::Ax, number(&state, "ax") as u16);
            self.session
                .registers
                .set(Gpr16::Cx, number(&state, "cx") as u16);
            self.session
                .registers
                .set(Gpr16::Di, number(&state, "di") as u16);
            self.session.fault = field(&state, "fault").as_string();
            self.session.waiting_for_io = field(&state, "waitingForIo").as_bool().unwrap_or(false);
            self.session.pending_commit = field(&state, "pendingCommit").as_bool().unwrap_or(false);
            self.session.dirty = field(&state, "dirty").as_bool().unwrap_or(false);
            self.session.pointer_in_use = field(&state, "pointerInUse").as_bool().unwrap_or(false);
            let memory = field(&state, "memory");
            self.memory_usage = doslet_runtime::session::MemoryUsage {
                installed_bytes: number(&memory, "installed_bytes") as u64,
                protected_mode: field(&memory, "protected_mode").as_bool().unwrap_or(false),
                dos_allocated: number(&memory, "dos_allocated") as u64,
                dos_free: number(&memory, "dos_free") as u64,
                dos_largest_free: number(&memory, "dos_largest_free") as u64,
                dos_blocks: number(&memory, "dos_blocks") as u32,
                dpmi_available: field(&memory, "dpmi_available").as_bool().unwrap_or(false),
                dpmi_entered: field(&memory, "dpmi_entered").as_bool().unwrap_or(false),
                dpmi_capacity: number(&memory, "dpmi_capacity") as u64,
                dpmi_requested: number(&memory, "dpmi_requested") as u64,
                dpmi_committed: number(&memory, "dpmi_committed") as u64,
                dpmi_peak: number(&memory, "dpmi_peak") as u64,
                dpmi_blocks: number(&memory, "dpmi_blocks") as u32,
            };
            let tier_counts = field(&state, "tierCounts");
            let tier_counts = js_sys::Array::is_array(&tier_counts)
                .then(|| js_sys::Array::from(&tier_counts))
                .filter(|counts| counts.length() == 4);
            let tiered = self.cpu_settings.mode == crate::cpu_config::Mode::Tiers
                && self.cpu_tiers_available
                && tier_counts.is_some();
            let executed = tier_counts.map_or([0; 4], |counts| {
                std::array::from_fn(|i| counts.get(i as u32).as_f64().unwrap_or(0.0) as u64)
            });
            self.cpu_activity = self.cpu_meter.sample(
                self.session.status == SessionStatus::Running && !self.session.waiting_for_io,
                crate::cpu_meter::Counters {
                    epoch: number(&state, "epoch") as u64,
                    guest: number(&state, "nanos") as u64,
                    idle: number(&state, "idleNanos") as u64,
                    executed,
                    grey_mhz: tiered.then_some(self.cpu_settings.grey),
                },
            );
        }
        let event = field(&packet, "event");
        match field(&event, "kind").as_string().as_deref() {
            Some("bundle-display") => {
                if let Some(filter) = field(&event, "filter").as_string()
                    && let Ok(filter) = filter.parse::<doslet_config::VideoFilter>()
                {
                    self.display_sizing.filter = filter.into();
                }
            }
            Some("storage-policy") => {
                self.storage_policy = field(&event, "message").as_string();
            }
            Some("audio-hardware-applied") => {
                let audio = self.configurator.draft.hardware.audio();
                self.configurator.active.hardware =
                    self.configurator.active.hardware.with_audio(audio);
                self.configurator.pending = false;
                self.configurator.message =
                    Some("Audio hardware updated without restarting.".into());
            }
            Some("audio-hardware-failed") => {
                self.configurator.pending = false;
                self.configurator.message = field(&event, "message").as_string();
            }
            Some("machine-applied") => {
                if let Some(profile) = field(&event, "profile").as_string()
                    && let Ok(spec) = crate::machine_profile::parse_machine_profile(&profile)
                {
                    self.name = spec.name.clone();
                    self.configurator.draft = spec;
                }
                self.configurator.open = false;
                self.workspace.panel = None;
                self.midi_mode = if self.configurator.draft.output.game_midi == "mt32" {
                    Mode::Mt32
                } else {
                    Mode::Gm
                };
                self.midi_output_mode = if self.configurator.draft.output.host_midi == "mt32" {
                    Mode::Mt32
                } else {
                    Mode::Gm
                };
                self.midi_choice = match self.configurator.draft.output.midi_output.as_str() {
                    "rust" => "Rust synth".into(),
                    "moont" => "CM-32L / MT-32".into(),
                    v => v.into(),
                };
                if self.configurator.draft.output.rom_binding.as_deref() == Some("bundle")
                    && !self
                        .midi_choices
                        .iter()
                        .any(|choice| choice == "CM-32L / MT-32")
                {
                    self.midi_choices.push("CM-32L / MT-32".into());
                }
                if let Some(name) = field(&event, "name").as_string() {
                    self.name = name;
                }
                self.display = DisplayFrame::default();
                self.session.graphics = None;
                self.configurator
                    .set_active(self.configurator.draft.clone());
                self.display_sizing.filter = self.configurator.active.output.video_filter.into();
                self.configurator.message = Some("Machine applied and restarted.".into());
            }
            Some("machine-failed") => {
                if !matches!(
                    self.workspace.panel,
                    Some(crate::ui::Panel::Audio | crate::ui::Panel::Drives)
                ) {
                    self.configurator.open = true;
                    self.workspace.panel = None;
                }
                self.configurator.pending = false;
                self.configurator.message = Some(
                    field(&event, "message")
                        .as_string()
                        .unwrap_or_else(|| "Machine preparation failed".into()),
                );
            }
            Some("imported") => {
                self.importing = false;
                let programs = Array::from(&field(&event, "programs"))
                    .iter()
                    .filter_map(|v| v.as_string())
                    .collect::<Vec<_>>();
                self.program = (programs.len() == 1).then(|| programs[0].clone());
                if let Some(program) = &self.program {
                    self.configurator
                        .edit(&crate::configurator::Edit::Executable(program.clone()));
                }
                self.configurator
                    .edit(&crate::configurator::Edit::Root("selected-zip".into()));
                self.archive = Some(Archive { programs });
                self.notice = None;
            }
            Some("started" | "reset-done") => {
                if let Some(name) = field(&event, "name").as_string() {
                    self.name = name;
                }
                self.display = DisplayFrame::default();
                self.session.graphics = None;
                self.notice = None;
            }
            Some("import-error" | "error" | "audio-error" | "storage-error") => {
                self.importing = false;
                self.notice = field(&event, "error").as_string();
            }
            _ => {}
        }
        let frame = field(&packet, "frame");
        if !frame.is_undefined() {
            let width = number(&frame, "width") as u32;
            let height = number(&frame, "height") as u32;
            let pixels = Uint8Array::new(&field(&frame, "pixels")).to_vec();
            self.display.image = Some(iced::widget::image::Handle::from_rgba(
                width, height, pixels,
            ));
            self.session.graphics = Some(Graphics {
                height: height as usize,
            });
            bridge.release_frame();
        }
        let state = field(&packet, "state");
        if let Some(mode) = field(&state, "midiMode").as_string() {
            self.midi_mode = if mode == "mt32" { Mode::Mt32 } else { Mode::Gm };
        }
        self.midi_summary = format!(
            "{} · {} SysEx · {} unmapped notes · {}{}",
            if self.midi_mode == Mode::Mt32
                && (self.midi_choice == "Rust synth"
                    || (self.midi_choice != "CM-32L / MT-32" && self.midi_output_mode == Mode::Gm))
            {
                "MT-32 → GM (approximation)"
            } else {
                ""
            },
            number(&state, "midiSysex") as u64,
            number(&state, "midiUnmapped") as u64,
            field(&state, "midiDisplay").as_string().unwrap_or_default(),
            if field(&state, "midiUpgraded").as_bool().unwrap_or(false) {
                " · Roland detected: upgraded to MT-32"
            } else {
                ""
            }
        );
        let audio = field(&packet, "audio");
        self.audio_state = field(&audio, "state").as_string().unwrap_or_default();
        self.audio_ready = field(&audio, "ready").as_bool().unwrap_or(false);
        self.muted = field(&audio, "muted").as_bool().unwrap_or(false);
        self.notes = number(&audio, "notes") as u64;
        self.frames = number(&audio, "frames") as u64;
        self.peak = number(&audio, "peak");
        let spectrum = field(&audio, "spectrum");
        self.spectrum.fill(0.0);
        if Array::is_array(&spectrum) {
            let spectrum = Array::from(&spectrum);
            for (band, value) in self.spectrum.iter_mut().enumerate() {
                *value = spectrum
                    .get(band as u32)
                    .as_f64()
                    .filter(|v| v.is_finite())
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0) as f32;
            }
        }
        self.pcm_frames = number(&audio, "pcmNonzero") as u64;
        self.underruns = number(&audio, "underruns") as u64;
        self.host_messages = number(&audio, "messages") as u64;
    }
    pub fn midi_mode_label(&self) -> String {
        self.midi_summary.clone()
    }
    pub fn storage_label(&self) -> String {
        if self.configurator.pending {
            return "Preparing machine and sources…".into();
        }
        let status: String = if self.session.pending_commit {
            "Saving changes…".into()
        } else if self.session.waiting_for_io {
            "Waiting for storage…".into()
        } else if self.session.dirty {
            "Unsaved changes".into()
        } else {
            "No unsaved changes".into()
        };
        match &self.storage_policy {
            Some(policy) => format!("{status}\n{policy}"),
            None => status,
        }
    }
    pub fn audio_label(&self) -> String {
        if !self.uses_local_audio() {
            return format!(
                "Host MIDI · {} · {} sent messages · SB {} frames · peak {:.2}",
                self.midi_choice, self.host_messages, self.pcm_frames, self.peak
            );
        }
        format!(
            "Web Audio · {}{} · {} notes · peak {:.2} · {} rendered frames · {} underruns · SB {} frames",
            self.audio_state,
            if self.muted { " · muted" } else { "" },
            self.notes,
            self.peak,
            self.frames,
            self.underruns,
            self.pcm_frames
        )
    }
    pub fn audio_button_label(&self) -> &'static str {
        if self.uses_local_audio() && (!self.audio_ready || self.audio_state != "running") {
            "Enable audio"
        } else if self.muted {
            "Unmute"
        } else {
            "Mute"
        }
    }
    fn uses_local_audio(&self) -> bool {
        matches!(
            self.midi_choice.as_str(),
            "Rust synth" | "moont" | "CM-32L / MT-32"
        )
    }
    pub fn view(&self) -> Element<'_, Message> {
        crate::ui::view(self)
    }
    pub fn theme(&self) -> Theme {
        crate::ui::theme()
    }
    pub fn audio_spectrum(&self) -> [f32; doslet_pcm::spectrum::BANDS] {
        if self.session.status() == SessionStatus::Running
            && self.audio_ready
            && !self.muted
            && (self.audio_state == "running" || self.audio_state == "MIDI port")
        {
            self.spectrum
        } else {
            [0.0; doslet_pcm::spectrum::BANDS]
        }
    }
    pub fn guest_audio_description(&self) -> String {
        crate::ui::guest_pcm_description(self.guest_pcm.0, self.guest_pcm.1, self.guest_pcm.2)
    }
    pub fn pointer_dimensions(&self) -> (u16, u16) {
        (
            640,
            if self
                .session
                .frame()
                .is_some_and(|frame| frame.height == 480)
            {
                480
            } else {
                200
            },
        )
    }
    pub fn has_mouse_capture(&self) -> bool {
        self.mouse_captured
    }
    pub fn hide_host_cursor(&self) -> bool {
        self.has_mouse_capture()
            || self.configurator.active.input.hide_cursor
                && !self.mouse_relative
                && self.session.pointer_in_use
                && self.session.status() == SessionStatus::Running
                && !self.configurator.open
                && self.workspace.panel.is_none()
                && !self.configurator.pending
    }
    pub fn subscription(&self) -> Subscription<Message> {
        // Poll bounded presentation snapshots even while paused/importing. The
        // timer neither runs the CPU nor determines audio/device timing.
        Subscription::batch([
            iced::time::every(web_time::Duration::from_millis(16)).map(|_| Message::Tick),
            iced::event::listen_with(|event, status, _| {
                if let iced::Event::Keyboard(event) = event {
                    Some(if status == iced::event::Status::Ignored {
                        Message::Keyboard(event)
                    } else {
                        Message::KeyboardCaptured(event)
                    })
                } else {
                    None
                }
            }),
        ])
    }
}
fn field(value: &JsValue, key: &str) -> JsValue {
    Reflect::get(value, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}
fn number(value: &JsValue, key: &str) -> f64 {
    field(value, key).as_f64().unwrap_or(0.0)
}
