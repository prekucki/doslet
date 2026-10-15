//! Window state contains snapshots and pixels, never an emulation Session.
use crate::presentation::DisplayFrame;
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
    #[wasm_bindgen(method, catch, js_name = listMidi)]
    fn list_midi(this: &Browser) -> Result<js_sys::Promise, JsValue>;
    #[wasm_bindgen(method, catch, js_name = selectMidi)]
    fn select_midi(
        this: &Browser,
        selection: &str,
        dialect: &str,
    ) -> Result<js_sys::Promise, JsValue>;
}
pub struct Graphics {
    pub width: usize,
    pub height: usize,
}
pub struct Snapshot {
    status: SessionStatus,
    steps: u64,
    registers: Registers,
    graphics: Option<Graphics>,
    fault: Option<String>,
}
impl Snapshot {
    pub fn status(&self) -> SessionStatus {
        self.status
    }
    pub fn steps(&self) -> u64 {
        self.steps
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
    pub session: Snapshot,
    pub display: DisplayFrame,
    pub name: String,
    pub notice: Option<String>,
    pub archive: Option<Archive>,
    pub program: Option<String>,
    pub importing: bool,
    pub midi_choices: Vec<String>,
    pub midi_choice: String,
    pub midi_mode: Mode,
    pub midi_output_mode: Mode,
    midi_summary: String,
    bridge: Option<Browser>,
    audio_state: String,
    audio_ready: bool,
    muted: bool,
    notes: u64,
    frames: u64,
    underruns: u64,
    peak: f64,
    host_messages: u64,
    pcm_frames: u64,
    mouse_captured: bool,
}
#[derive(Clone, Debug)]
pub enum Message {
    PointerMoved(u16, u16),
    PointerButton(u8, bool),
    PointerLeft,
    Keyboard(iced::keyboard::Event),
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
            session: Snapshot {
                status: SessionStatus::Ready,
                steps: 0,
                registers: Registers::default(),
                graphics: None,
                fault: None,
            },
            display: DisplayFrame::default(),
            name: "Color bars".into(),
            notice,
            archive: None,
            program: None,
            importing: false,
            midi_choices: vec!["Rust synth".into()],
            midi_choice: "Rust synth".into(),
            midi_mode: Mode::Gm,
            midi_output_mode: Mode::Gm,
            midi_summary: String::new(),
            bridge,
            audio_state: "inactive".into(),
            audio_ready: false,
            muted: false,
            notes: 0,
            frames: 0,
            underruns: 0,
            peak: 0.0,
            host_messages: 0,
            pcm_frames: 0,
            mouse_captured: false,
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
        if let Message::Keyboard(event) = &message {
            if self.session.status() == SessionStatus::Running
                && let Some(bridge) = &self.bridge
            {
                let (modifiers, key) = crate::keyboard::translate(event);
                let (scan, ascii) = key.unwrap_or((0, 0));
                if let Err(error) = bridge.key_event(scan, ascii, modifiers, key.is_some()) {
                    self.notice = Some(format!("Keyboard: {error:?}"));
                }
            }
            return Task::none();
        }
        match message {
            Message::PointerMoved(x, y) => {
                if let Some(bridge) = &self.bridge
                    && let Err(error) = bridge.pointer_event(x, y, -1, false)
                {
                    self.notice = Some(format!("Mouse: {error:?}"));
                }
            }
            Message::PointerButton(button, pressed) => {
                if pressed
                    && !self.mouse_captured
                    && self.session.status() == SessionStatus::Running
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
            Message::Keyboard(_) => unreachable!("handled above"),
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
                    self.midi_choices.extend(ports);
                    self.notice = None;
                }
                Err(error) => self.notice = Some(error),
            },
            Message::MidiModeSelected(mode) => {
                self.midi_mode = mode;
                self.command("midi-mode", mode.as_str());
            }
            Message::MidiOutputModeSelected(mode) => {
                self.midi_output_mode = mode;
                // Reopen through the bridge so raw MT-32 requests SysEx access.
                self.command("output-mode", mode.as_str());
                if self.midi_choice != "Rust synth" {
                    return self.update(Message::MidiSelected(self.midi_choice.clone()));
                }
            }
            Message::MidiSelected(selection) => {
                if let Some(bridge) = &self.bridge {
                    match bridge.select_midi(&selection, self.midi_output_mode.as_str()) {
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
                    self.notice = None;
                }
                Err(error) => self.notice = Some(error),
            },
            Message::Tick => self.poll(),
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
            Message::Reset => self.command("reset", ""),
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
            Message::ProgramSelected(program) => self.program = Some(program),
            Message::StartArchive => {
                let audio = self.resume_audio();
                if let Some(program) = self.program.clone() {
                    self.command("start-game", &program);
                }
                return audio;
            }
            Message::ToggleAudio => {
                if self.midi_choice != "Rust synth"
                    || (self.audio_ready && self.audio_state == "running")
                {
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
        self.mouse_captured = field(&packet, "mouseCaptured").as_bool().unwrap_or(false);
        let state = field(&packet, "state");
        if !state.is_null() && !state.is_undefined() {
            self.session.status = match number(&state, "status") as u32 {
                0 => SessionStatus::Ready,
                1 => SessionStatus::Running,
                2 => SessionStatus::Paused,
                3 => SessionStatus::Halted,
                4 => SessionStatus::Faulted,
                code => SessionStatus::Exited((code - 256) as u8),
            };
            self.session.steps = number(&state, "steps") as u64;
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
        }
        let event = field(&packet, "event");
        match field(&event, "kind").as_string().as_deref() {
            Some("imported") => {
                self.importing = false;
                let programs = Array::from(&field(&event, "programs"))
                    .iter()
                    .filter_map(|v| v.as_string())
                    .collect::<Vec<_>>();
                self.program = (programs.len() == 1).then(|| programs[0].clone());
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
            Some("import-error" | "error" | "audio-error") => {
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
                width: width as usize,
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
                && (self.midi_choice == "Rust synth" || self.midi_output_mode == Mode::Gm)
            {
                "MT-32 → GM (przybliżenie)"
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
        self.pcm_frames = number(&audio, "pcmNonzero") as u64;
        self.underruns = number(&audio, "underruns") as u64;
        self.host_messages = number(&audio, "messages") as u64;
    }
    pub fn midi_mode_label(&self) -> String {
        self.midi_summary.clone()
    }
    pub fn audio_label(&self) -> String {
        if self.midi_choice != "Rust synth" {
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
        if self.midi_choice == "Rust synth" && (!self.audio_ready || self.audio_state != "running")
        {
            "Enable audio"
        } else if self.muted {
            "Unmute"
        } else {
            "Mute"
        }
    }
    pub fn view(&self) -> Element<'_, Message> {
        crate::ui::view(self)
    }
    pub fn theme(&self) -> Theme {
        crate::ui::theme()
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
    pub fn hide_host_cursor(&self) -> bool {
        self.mouse_captured
    }
    pub fn subscription(&self) -> Subscription<Message> {
        // Poll bounded presentation snapshots even while paused/importing. The
        // timer neither runs the CPU nor determines audio/device timing.
        Subscription::batch([
            iced::time::every(web_time::Duration::from_millis(16)).map(|_| Message::Tick),
            iced::event::listen_with(|event, status, _| {
                if status == iced::event::Status::Ignored
                    && let iced::Event::Keyboard(event) = event
                {
                    Some(Message::Keyboard(event))
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
