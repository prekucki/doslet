use crate::native_synth::{APPLE_LABEL, Timeline};
use crate::presentation::{DisplayFrame, DisplaySizing, SizingMessage};
use doslet_midi_routing::{Mode, Routing, SNAPSHOT_CAPACITY};
use doslet_runtime::{RuntimeError, Session, SessionStatus};
use iced::{Element, Subscription, Task, Theme};
use std::time::{Duration, Instant};

const MAX_STEPS_PER_TICK: u32 = 400_000;
const CHUNK: u32 = 10_000;
struct PreparedOutput {
    audio: Option<crate::audio::Output>,
    host_midi: Option<crate::native_midi::Output>,
    mt32: Option<doslet_mt32::Timeline>,
    rom_dir: Option<std::path::PathBuf>,
    midi: Timeline,
    midi_mode: Mode,
    output_mode: Mode,
    choice: String,
    choices: Vec<String>,
    routing: Routing,
    mixer: doslet_pcm::Mixer,
}
struct PreparedMachine {
    session: Session,
    source: Option<std::path::PathBuf>,
    launch: crate::native_input::LaunchSettings,
    spec: doslet_config::MachineSpec,
}
pub(crate) struct MachinePreparation {
    old_storage: Option<crate::native_storage::NativeStorage>,
    result: Result<(PreparedMachine, crate::native_storage::NativeStorage), String>,
}
impl core::fmt::Debug for MachinePreparation {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("MachinePreparation")
    }
}
type PreparationSlot = std::sync::Arc<std::sync::Mutex<Option<MachinePreparation>>>;
pub struct Doslet {
    pub session: Session,
    pub workspace: crate::ui::Workspace,
    pub configurator: crate::configurator::ConfiguratorState,
    storage: Option<crate::native_storage::NativeStorage>,
    gamepads: Option<crate::native_gamepad::NativeGamepads>,
    pad_axes: [i16; 2],
    pad_buttons: [bool; 2],
    pad_generation: Option<u64>,
    pad_sequence: u64,
    pending_output: Option<Box<PreparedOutput>>,
    preparation_was_running: bool,
    pub display: DisplayFrame,
    pub display_sizing: DisplaySizing,
    pub name: String,
    pub notice: Option<String>,
    logged_fault: Option<RuntimeError>,
    logged_notice: Option<String>,
    dynamic_error_reported: bool,
    pub midi_choices: Vec<String>,
    pub midi_choice: String,
    host_midi: Option<crate::native_midi::Output>,
    routing: Routing,
    pub midi_mode: Mode,
    pub midi_output_mode: Mode,
    pub midi_activity: crate::midi_activity::MidiActivity,
    #[cfg(test)]
    pub archive: Option<crate::archive::GameArchive>,
    #[cfg(test)]
    pub program: Option<String>,
    #[cfg(test)]
    active_archive: Option<(crate::archive::GameArchive, String)>,
    theme: Theme,
    anchor: Option<(Instant, u64)>,
    midi: Timeline,
    mt32: Option<doslet_mt32::Timeline>,
    mt32_rom_dir: Option<std::path::PathBuf>,
    samples: [[f32; 2]; 512],
    midi_bytes: [u8; 1024],
    pcm: [doslet_pcm::Frame; 128],
    pcm_mixer: doslet_pcm::Mixer,
    mouse_capture: Option<iced::window::Id>,
    mouse_capture_pending: bool,
    mouse_capture_generation: u64,
    #[cfg(not(target_arch = "wasm32"))]
    source: Option<std::path::PathBuf>,
    #[cfg(not(target_arch = "wasm32"))]
    launch: crate::native_input::LaunchSettings,
    #[cfg(not(target_arch = "wasm32"))]
    audio: Option<crate::audio::Output>,
}
#[derive(Clone, Debug)]
pub enum Message {
    Ui(crate::ui::Action),
    Config(crate::configurator::Edit),
    MachinePrepared(Result<PreparationSlot, String>),
    DisplaySizing(SizingMessage),
    PointerMoved(u16, u16),
    PointerButton(u8, bool),
    PointerLeft,
    CaptureWindow(iced::window::Id, u64),
    MouseCapture(iced::window::Id, u64, iced::window::MouseCaptureEvent),
    Keyboard(iced::keyboard::Event),
    KeyboardCaptured(iced::keyboard::Event),
    RunPause,
    Step,
    Reset,
    Tick,
    GamepadsReady,
    FocusLost,
    CloseRequested(iced::window::Id),
    RefreshMidi,
    MidiSelected(String),
    MidiModeSelected(Mode),
    MidiOutputModeSelected(Mode),
    #[cfg(test)]
    ArchiveLoaded(Result<Option<std::sync::Arc<Vec<u8>>>, String>),
    #[cfg(test)]
    ProgramSelected(String),
    #[cfg(test)]
    StartArchive,
}
impl Doslet {
    pub fn new() -> Self {
        Self {
            session: Session::demo(),
            workspace: crate::ui::Workspace::default(),
            configurator: crate::configurator::ConfiguratorState::default(),
            storage: None,
            gamepads: None,
            pad_axes: [0; 2],
            pad_buttons: [false; 2],
            pad_generation: None,
            pad_sequence: 0,
            pending_output: None,
            preparation_was_running: false,
            display: DisplayFrame::default(),
            display_sizing: DisplaySizing::default(),
            name: "Color bars".into(),
            notice: None,
            logged_fault: None,
            logged_notice: None,
            dynamic_error_reported: false,
            midi_choices: crate::native_synth::choices(),
            midi_choice: "Rust synth".into(),
            host_midi: None,
            routing: Routing::new(Mode::Gm),
            midi_mode: Mode::Gm,
            midi_output_mode: Mode::Gm,
            midi_activity: crate::midi_activity::MidiActivity::default(),
            #[cfg(test)]
            archive: None,
            #[cfg(test)]
            program: None,
            #[cfg(test)]
            active_archive: None,
            theme: crate::ui::theme(),
            anchor: None,
            midi: Timeline::new(48000).unwrap(),
            mt32: None,
            mt32_rom_dir: None,
            samples: [[0.0; 2]; 512],
            midi_bytes: [0; 1024],
            pcm: [doslet_pcm::EMPTY; 128],
            pcm_mixer: doslet_pcm::Mixer::new(48000).unwrap(),
            mouse_capture: None,
            mouse_capture_pending: false,
            mouse_capture_generation: 0,
            #[cfg(not(target_arch = "wasm32"))]
            source: None,
            #[cfg(not(target_arch = "wasm32"))]
            launch: crate::native_input::LaunchSettings::default(),
            #[cfg(not(target_arch = "wasm32"))]
            audio: None,
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_cli(cli: crate::cli::Cli) -> Result<Box<Self>, Box<dyn std::error::Error>> {
        let cli = cli.resolve()?;
        let mut app = Box::new(Self::new());
        app.gamepads = Some(crate::native_gamepad::NativeGamepads::new());
        app.midi_mode = cli.midi_mode.unwrap_or_default().into();
        app.midi_output_mode = cli.midi_output_mode.unwrap_or_default().into();
        app.launch = cli.launch;
        let mut machine_spec = app.launch.machine_spec.clone().unwrap_or_default();
        machine_spec.hardware = app.launch.hardware;
        machine_spec.name = cli.profile_name.clone().unwrap_or_default();
        machine_spec.game.executable = cli
            .game
            .as_ref()
            .map_or(String::new(), |p| p.to_string_lossy().into_owned());
        machine_spec.output.audio = !cli.no_audio;
        machine_spec.output.game_midi = app.midi_mode.as_str().into();
        machine_spec.output.host_midi = app.midi_output_mode.as_str().into();
        machine_spec.output.midi_output = cli.midi_output.clone().unwrap_or_else(|| "rust".into());
        app.configurator.set_active(machine_spec);
        if cli.game.is_none() {
            app.session = Session::demo_configured(app.launch.hardware)?;
            app.session.set_dynamic_core(!app.launch.no_dynamic_core);
            if let Some(path) = &app.launch.core_profile {
                crate::native_input::load_core_profile(&mut app.session, path);
            }
        }
        app.routing.set_mode(app.midi_mode);
        if let Some(path) = cli.game {
            let mut session = crate::native_input::load_game_with_settings(&path, &app.launch)
                .map_err(|error| format!("Cannot load {}: {error}", path.display()))?;
            let storage =
                crate::native_storage::NativeStorage::prepare_mut(&mut session, &app.launch)?;
            app.storage = Some(storage);
            session.start();
            app.session = session;
            app.name = cli
                .profile_name
                .unwrap_or_else(|| path.file_name().unwrap().to_string_lossy().into_owned());
            app.source = Some(path);
            if !cli.no_audio && cli.midi_output.is_none() && cli.mt32_rom_dir.is_none() {
                match crate::audio::Output::new() {
                    Ok(audio) => {
                        app.midi = Timeline::new(audio.rate).unwrap();
                        app.pcm_mixer.reset(audio.rate, 0).unwrap();
                        app.audio = Some(audio);
                    }
                    Err(error) => app.notice = Some(format!("Audio unavailable: {error}")),
                }
            }
        }
        if let Some(directory) = cli.mt32_rom_dir {
            app.mt32_rom_dir = Some(directory);
            app.midi_choices.push("CM-32L / MT-32".into());
        }
        if let Some(selection) = cli.midi_output {
            let selection = match selection.as_str() {
                "rust" => "Rust synth",
                "apple" => APPLE_LABEL,
                "moont" => "CM-32L / MT-32",
                other => other,
            };
            app.select_midi(selection)?;
        } else if app.mt32_rom_dir.is_some() {
            app.select_midi("CM-32L / MT-32")?;
        }
        app.log_errors();
        Ok(app)
    }
    fn load_mt32(
        directory: &std::path::Path,
        rate: u32,
        mode: Mode,
    ) -> Result<doslet_mt32::Timeline, Box<dyn std::error::Error>> {
        let control = std::fs::read(directory.join("CM32L_CONTROL.ROM"))?;
        let pcm = std::fs::read(directory.join("CM32L_PCM.ROM"))?;
        Ok(doslet_mt32::Timeline::with_gm(
            &control,
            &pcm,
            rate,
            mode == Mode::Gm,
        )?)
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        let mouse_task = if matches!(message, Message::Reset) {
            self.release_mouse()
        } else {
            Task::none()
        };
        match message {
            Message::Ui(action) => match action {
                crate::ui::Action::Open(panel) => {
                    self.workspace.panel = Some(panel);
                    self.configurator.open = false;
                    return self.release_mouse();
                }
                crate::ui::Action::Close => {
                    self.workspace.panel = None;
                    self.configurator.open = false;
                }
                crate::ui::Action::DismissNotice => self.notice = None,
            },
            Message::Config(edit) => {
                use crate::configurator::Edit;
                self.workspace.panel = None;
                if self.configurator.pending && !matches!(edit, Edit::Toggle) {
                    return Task::none();
                }
                self.configurator.edit(&edit);
                match edit {
                    Edit::Apply => return self.prepare_machine(self.configurator.draft.clone()),
                    Edit::LoadProfileFile => {
                        use clap::Parser;
                        let path = self.configurator.profile_path.clone();
                        match crate::cli::Cli::try_parse_from(["doslet", "--config", &path])
                            .map_err(|e| e.to_string())
                            .and_then(|cli| cli.resolve().map_err(|e| e.to_string()))
                        {
                            Ok(cli) => {
                                self.configurator.draft =
                                    cli.launch.machine_spec.unwrap_or_default();
                                self.configurator.edit(&Edit::ExportProfile);
                            }
                            Err(e) => self.configurator.message = Some(e),
                        }
                    }
                    Edit::SaveProfileFile => {
                        let text = crate::machine_profile::export_machine_profile(
                            &self.configurator.draft,
                        );
                        self.configurator.message = Some(
                            match std::fs::write(&self.configurator.profile_path, text) {
                                Ok(()) => "Profile saved.".into(),
                                Err(e) => e.to_string(),
                            },
                        );
                    }
                    Edit::ImportSaves => {
                        let path = std::path::PathBuf::from(&self.configurator.save_path);
                        return self.prepare_machine_with_import(
                            self.configurator.active.clone(),
                            Some(path),
                        );
                    }
                    Edit::ExportSaves => {
                        let path = std::path::PathBuf::from(&self.configurator.save_path);
                        self.configurator.message =
                            Some(if let Some(storage) = &mut self.storage {
                                storage.export(&mut self.session, &path).map_or_else(
                                    |e| e,
                                    |_| format!("Save data exported: {}", path.display()),
                                )
                            } else {
                                "Start a configured machine first.".into()
                            });
                    }
                    _ => {}
                }
                if self.configurator.open {
                    return self.release_mouse();
                }
            }
            Message::MachinePrepared(result) => self.finish_preparation(result),
            Message::DisplaySizing(message) => {
                return self
                    .display_sizing
                    .update(message, &self.display)
                    .map(Message::DisplaySizing);
            }
            Message::PointerMoved(x, y) => {
                if self.mouse_capture.is_none()
                    && !self.mouse_capture_pending
                    && self.session.status() == SessionStatus::Running
                    && !self.configurator.open
                    && self.workspace.panel.is_none()
                {
                    self.session.pointer_move(x, y);
                }
            }
            Message::PointerButton(button, pressed) => {
                if pressed
                    && self.session.status() == SessionStatus::Running
                    && self.mouse_capture.is_none()
                    && !self.mouse_capture_pending
                    && self.configurator.active.input.capture
                    && !self.configurator.open
                    && self.workspace.panel.is_none()
                {
                    self.mouse_capture_pending = true;
                    self.mouse_capture_generation = self.mouse_capture_generation.wrapping_add(1);
                    let generation = self.mouse_capture_generation;
                    return iced::window::latest()
                        .and_then(move |id| Task::done(Message::CaptureWindow(id, generation)));
                }
                if self.mouse_capture_pending {
                    return Task::none();
                }
                if (self.session.status() == SessionStatus::Running
                    && !self.configurator.open
                    && self.workspace.panel.is_none())
                    || !pressed
                {
                    self.session.pointer_button(button, pressed);
                }
            }
            Message::PointerLeft => {
                if self.mouse_capture.is_none() && !self.mouse_capture_pending {
                    self.session.release_pointer_buttons();
                }
            }
            Message::CaptureWindow(id, generation) => {
                if generation == self.mouse_capture_generation
                    && self.mouse_capture_pending
                    && self.session.status() == SessionStatus::Running
                {
                    self.mouse_capture = Some(id);
                    return iced::window::capture_mouse(id)
                        .map(move |event| Message::MouseCapture(id, generation, event));
                }
            }
            Message::MouseCapture(id, generation, event) => {
                if generation != self.mouse_capture_generation {
                    return Task::none();
                }
                use iced::window::MouseCaptureEvent;
                match event {
                    MouseCaptureEvent::Acquired => {
                        if !self.mouse_capture_pending
                            || self.session.status() != SessionStatus::Running
                        {
                            return iced::window::release_mouse(id).map(|()| Message::PointerLeft);
                        }
                        self.mouse_capture_pending = false;
                        self.mouse_capture = Some(id);
                    }
                    MouseCaptureEvent::Motion { dx, dy } => {
                        if self.mouse_capture == Some(id)
                            && self.session.status() == SessionStatus::Running
                        {
                            self.session.pointer_relative(dx, dy);
                        }
                    }
                    MouseCaptureEvent::Button { button, pressed } => {
                        if self.mouse_capture == Some(id) {
                            self.session.pointer_button(button, pressed);
                        }
                    }
                    MouseCaptureEvent::Released => {
                        if self.mouse_capture == Some(id) || self.mouse_capture_pending {
                            self.mouse_capture = None;
                            self.mouse_capture_pending = false;
                            self.session.release_pointer_buttons();
                            let _ = self.session.release_keys();
                        }
                    }
                    MouseCaptureEvent::Failed(error) => {
                        self.notice = Some(format!("Mouse capture unavailable: {error}"));
                    }
                }
            }
            Message::Keyboard(event) => {
                if (self.configurator.open || self.workspace.panel.is_some())
                    && matches!(
                        &event,
                        iced::keyboard::Event::KeyPressed {
                            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                            ..
                        }
                    )
                {
                    self.configurator.open = false;
                    self.workspace.panel = None;
                    return self.release_mouse();
                }
                if (self.mouse_capture.is_some() || self.mouse_capture_pending)
                    && matches!(
                        &event,
                        iced::keyboard::Event::KeyPressed {
                            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                            ..
                        }
                    )
                {
                    return self.release_mouse();
                }
                if self.configurator.active.hardware.keyboard == doslet_config::KeyboardMode::At {
                    self.track_physical_key(
                        &event,
                        self.session.status() == SessionStatus::Running
                            && !self.configurator.open
                            && self.workspace.panel.is_none()
                            && !self.configurator.pending,
                    );
                    return Task::none();
                }
                if self.session.status() == SessionStatus::Running
                    && !self.configurator.open
                    && self.workspace.panel.is_none()
                {
                    let (modifiers, key) = crate::keyboard::translate(&event);
                    if let Err(error) = self.session.set_key_modifiers(modifiers).and_then(|()| {
                        if let Some((scan, ascii)) = key {
                            self.session.push_key(scan, ascii).map(|_| ())
                        } else {
                            Ok(())
                        }
                    }) {
                        self.notice = Some(error.to_string());
                    }
                }
            }
            Message::KeyboardCaptured(event) => {
                if self.configurator.active.hardware.keyboard == doslet_config::KeyboardMode::At {
                    self.track_physical_key(&event, false);
                }
            }
            Message::RefreshMidi => match crate::native_midi::ports() {
                Ok(ports) => {
                    self.midi_choices = crate::native_synth::choices();
                    if self.mt32_rom_dir.is_some() {
                        self.midi_choices.push("CM-32L / MT-32".into());
                    }
                    self.midi_choices.extend(ports.names);
                }
                Err(error) => self.notice = Some(error),
            },
            Message::MidiModeSelected(mode) => {
                let previous = self.midi_mode;
                self.midi_mode = mode;
                self.routing.set_mode(mode);
                let selection = self.midi_choice.clone();
                if let Err(error) = self.reselect_midi(&selection) {
                    self.midi_mode = previous;
                    self.routing.set_mode(previous);
                    self.notice = Some(error);
                }
            }
            Message::MidiOutputModeSelected(mode) => {
                let previous = self.midi_output_mode;
                self.midi_output_mode = mode;
                if self.host_midi.is_some() {
                    let selection = self.midi_choice.clone();
                    if let Err(error) = self.reselect_midi(&selection) {
                        self.midi_output_mode = previous;
                        self.notice = Some(error);
                    }
                }
            }
            Message::MidiSelected(selection) => {
                if let Err(error) = self.select_midi(&selection) {
                    self.notice = Some(error);
                }
            }
            #[cfg(test)]
            Message::ArchiveLoaded(result) => match result {
                Ok(Some(bytes)) => match crate::archive::GameArchive::new(bytes) {
                    Ok(archive) => {
                        self.program =
                            (archive.programs.len() == 1).then(|| archive.programs[0].clone());
                        self.archive = Some(archive);
                        self.notice = None;
                    }
                    Err(error) => self.notice = Some(error),
                },
                Ok(None) => {}
                Err(error) => self.notice = Some(error),
            },
            #[cfg(test)]
            Message::ProgramSelected(program) => self.program = Some(program),
            #[cfg(test)]
            Message::StartArchive => {
                if let (Some(archive), Some(program)) = (&self.archive, &self.program) {
                    match archive.load(program) {
                        Ok(mut session) => {
                            session.start();
                            self.session = session;
                            self.name = program.rsplit('/').next().unwrap().into();
                            self.active_archive = Some((archive.clone(), program.clone()));
                            self.clear_presentation();
                        }
                        Err(error) => self.notice = Some(error),
                    }
                }
            }
            Message::RunPause => {
                if self.configurator.pending {
                    return Task::none();
                }
                self.save_core_profile();
                if self.session.status() == SessionStatus::Running {
                    self.session.pause();
                } else {
                    self.session.start();
                }
                self.anchor = None;
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(audio) = &self.audio {
                    audio.set_active(self.session.status() == SessionStatus::Running);
                }
            }
            Message::Step => {
                if self.configurator.pending {
                    return Task::none();
                }
                self.session.single_step();
                if let Err(error) = self.process_audio() {
                    self.notice = Some(error);
                }
            }
            Message::Reset => {
                if self.configurator.pending {
                    return Task::none();
                }
                if self
                    .configurator
                    .active
                    .mounts
                    .iter()
                    .any(|m| m.kind == doslet_config::SourceKind::HttpsIso)
                {
                    return self.prepare_machine(self.configurator.active.clone());
                }
                self.reset();
            }
            Message::FocusLost => {
                let _ = self.session.release_keys();
                return self.release_mouse();
            }
            Message::CloseRequested(id) => {
                self.save_core_profile();
                return iced::window::close(id);
            }
            Message::Tick => self.tick(),
            Message::GamepadsReady => self.poll_gamepads(),
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(audio) = &self.audio {
            audio.set_active(self.session.status() == SessionStatus::Running);
        }
        if let Some(output) = &mut self.host_midi {
            match output.set_active(self.session.status() == SessionStatus::Running) {
                Ok(true) => {
                    let mut replay = Vec::with_capacity(SNAPSHOT_CAPACITY);
                    self.routing.replay(self.midi_output_mode, &mut |message| {
                        replay.extend_from_slice(message)
                    });
                    let count = replay.len();
                    if let Err(error) = output.push(self.session.emulated_nanos(), &replay[..count])
                    {
                        self.notice = Some(error);
                        self.session.pause();
                        let _ = output.set_active(false);
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    self.notice = Some(error);
                    self.session.pause();
                }
            }
        }
        self.log_errors();
        self.midi_activity.observe(
            self.session.emulated_nanos(),
            self.session.midi_stats().1,
            self.routing.input_note_events(),
        );
        self.display.refresh(&self.session);
        #[cfg(not(target_arch = "wasm32"))]
        {
            if self.session.status() != SessionStatus::Running {
                Task::batch([mouse_task, self.release_mouse()])
            } else {
                mouse_task
            }
        }
    }
    fn prepare_output(spec: &doslet_config::MachineSpec) -> Result<PreparedOutput, String> {
        let mut output = Self::new();
        output.midi_mode = if spec.output.game_midi == "mt32" {
            Mode::Mt32
        } else {
            Mode::Gm
        };
        output.midi_output_mode = if spec.output.host_midi == "mt32" {
            Mode::Mt32
        } else {
            Mode::Gm
        };
        output.routing.set_mode(output.midi_mode);
        output.mt32_rom_dir = spec
            .output
            .rom_binding
            .as_ref()
            .map(|id| std::path::PathBuf::from(spec.binding(id)));
        if spec.output.audio {
            let selection = match spec.output.midi_output.as_str() {
                "rust" => "Rust synth",
                "apple" => APPLE_LABEL,
                "moont" => "CM-32L / MT-32",
                other => other,
            };
            output.select_midi(selection)?;
        }
        Ok(PreparedOutput {
            audio: output.audio,
            host_midi: output.host_midi,
            mt32: output.mt32,
            rom_dir: output.mt32_rom_dir,
            midi: output.midi,
            midi_mode: output.midi_mode,
            output_mode: output.midi_output_mode,
            choice: output.midi_choice,
            choices: output.midi_choices,
            routing: output.routing,
            mixer: output.pcm_mixer,
        })
    }
    fn prepare_machine(&mut self, spec: doslet_config::MachineSpec) -> Task<Message> {
        self.prepare_machine_with_import(spec, None)
    }
    fn prepare_machine_with_import(
        &mut self,
        spec: doslet_config::MachineSpec,
        import_path: Option<std::path::PathBuf>,
    ) -> Task<Message> {
        if self.configurator.pending {
            return Task::none();
        }
        let plan = match spec.resolve() {
            Ok(plan) => plan,
            Err(e) => {
                self.configurator.message = Some(e.to_string());
                return Task::none();
            }
        };
        if let Some(storage) = &mut self.storage
            && let Err(e) = storage.service(&mut self.session)
        {
            self.configurator.message = Some(e);
            return Task::none();
        }
        if self.session.storage_waiting() || self.session.pending_commit() {
            self.configurator.message =
                Some("Wait for pending storage operations before restarting.".into());
            return Task::none();
        }
        if self.session.dirty_overlay()
            && !self.launch.persist
            && !self.configurator.discard_changes
        {
            self.configurator.message = Some(
                "Export saves or select Discard unsaved temporary changes before restarting."
                    .into(),
            );
            return Task::none();
        }
        if let Some(path) = &self.source
            && let Err(e) = crate::native_input::flush_writes(&mut self.session, path, &self.launch)
        {
            self.configurator.message = Some(e.to_string());
            return Task::none();
        }
        let output = match Self::prepare_output(&spec) {
            Ok(output) => output,
            Err(e) => {
                self.configurator.message = Some(e);
                return Task::none();
            }
        };
        self.save_core_profile();
        let mut settings = self.launch.clone();
        settings.hardware = plan.hardware;
        settings.machine_spec = Some(spec.clone());
        settings.command_tail = plan.command_tail;
        settings.working_directory = spec.game.working_directory.as_bytes().to_vec();
        settings.root = None;
        settings.cdrom = None;
        for mount in &spec.mounts {
            match mount.kind {
                doslet_config::SourceKind::NativeDirectory => {
                    settings.root = Some(std::path::PathBuf::from(spec.binding(&mount.source)));
                    settings.writable = mount.policy != doslet_config::WritePolicy::ReadOnly;
                    settings.persist = mount.policy == doslet_config::WritePolicy::LegacyWriteback;
                }
                doslet_config::SourceKind::LocalIso => {
                    settings.cdrom = Some((
                        mount.letter,
                        std::path::PathBuf::from(spec.binding(&mount.source)),
                    ))
                }
                doslet_config::SourceKind::HttpsIso => {}
                _ => {
                    self.configurator.message = Some(
                        "Native machines require a folder and local or HTTPS ISO sources.".into(),
                    );
                    return Task::none();
                }
            }
        }
        self.pending_output = Some(Box::new(output));
        self.preparation_was_running = self.session.status() == SessionStatus::Running;
        self.session.pause();
        self.anchor = None;
        if let Some(audio) = &self.audio {
            audio.set_active(false);
        }
        if let Some(output) = &mut self.host_midi {
            let _ = output.set_active(false);
        }
        self.configurator.pending = true;
        self.configurator.message = Some("Preparing machine and sources…".into());
        let old_storage = self.storage.take();
        let (sender, receiver) = iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let _ = sender.send(Self::prepare_candidate(
                spec,
                settings,
                old_storage,
                import_path,
            ));
        });
        let task = Task::perform(
            async move {
                receiver
                    .await
                    .map(|r| std::sync::Arc::new(std::sync::Mutex::new(Some(r))))
                    .map_err(|_| "Machine preparation worker stopped.".into())
            },
            Message::MachinePrepared,
        );
        Task::batch([self.release_mouse(), task])
    }
    fn prepare_candidate(
        spec: doslet_config::MachineSpec,
        settings: crate::native_input::LaunchSettings,
        mut old_storage: Option<crate::native_storage::NativeStorage>,
        import_path: Option<std::path::PathBuf>,
    ) -> MachinePreparation {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<_, String> {
                let path = (!spec.game.executable.is_empty())
                    .then(|| std::path::PathBuf::from(&spec.game.executable));
                let mut session = match &path {
                    Some(path) => crate::native_input::load_game_with_settings(path, &settings)
                        .map_err(|e| e.to_string())?,
                    None => {
                        let mut session = Session::demo_configured(settings.hardware)
                            .map_err(|e| e.to_string())?;
                        session.set_dynamic_core(!settings.no_dynamic_core);
                        if let Some(path) = &settings.core_profile {
                            crate::native_input::load_core_profile(&mut session, path);
                        }
                        session
                    }
                };
                let mut storage = if let Some(old) = &mut old_storage {
                    old.prepare_replacement_mut(&mut session, &settings)
                } else {
                    crate::native_storage::NativeStorage::prepare_mut(&mut session, &settings)
                }
                .map_err(|e| e.to_string())?;
                if let Some(path) = import_path {
                    storage.import(&mut session, &path)?;
                }
                Ok((
                    PreparedMachine {
                        session,
                        source: path,
                        launch: settings,
                        spec,
                    },
                    storage,
                ))
            }))
            .unwrap_or_else(|_| Err("Machine preparation worker failed.".into()));
        MachinePreparation {
            old_storage,
            result,
        }
    }
    fn finish_preparation(&mut self, result: Result<PreparationSlot, String>) {
        self.configurator.pending = false;
        let prepared = result.and_then(|slot| {
            slot.lock()
                .map_err(|_| "Machine preparation result unavailable.".to_string())
                .and_then(|mut slot| {
                    slot.take()
                        .ok_or_else(|| "Machine preparation result already consumed.".into())
                })
        });
        match prepared {
            Ok(prepared) => {
                self.storage = prepared.old_storage;
                match prepared.result {
                    Ok((mut candidate, storage)) => {
                        let Some(output) = self.pending_output.take() else {
                            self.configurator.message =
                                Some("Prepared host output unavailable.".into());
                            return;
                        };
                        let output = *output;
                        if candidate.spec.input.pad_id == self.configurator.active.input.pad_id
                            && candidate.spec.input.pad_index
                                == self.configurator.active.input.pad_index
                        {
                            self.seed_pad(&mut candidate.session);
                        } else {
                            self.pad_generation = None;
                            self.pad_axes = [0; 2];
                            self.pad_buttons = [false; 2];
                        }
                        let _ = candidate.session.release_keys();
                        candidate.session.start();
                        self.session = candidate.session;
                        self.dynamic_error_reported = false;
                        self.source = candidate.source;
                        self.launch = candidate.launch;
                        self.storage = Some(storage);
                        self.audio = output.audio;
                        self.host_midi = output.host_midi;
                        self.mt32 = output.mt32;
                        self.mt32_rom_dir = output.rom_dir;
                        self.midi = output.midi;
                        self.midi_mode = output.midi_mode;
                        self.midi_output_mode = output.output_mode;
                        self.midi_choice = output.choice;
                        self.midi_choices = output.choices;
                        self.routing = output.routing;
                        self.midi_activity = crate::midi_activity::MidiActivity::default();
                        self.pcm_mixer = output.mixer;
                        self.display = DisplayFrame::default();
                        self.anchor = None;
                        self.notice = None;
                        self.name = if candidate.spec.name.is_empty() {
                            "Configured machine".into()
                        } else {
                            candidate.spec.name.clone()
                        };
                        self.configurator.set_active(candidate.spec);
                        self.configurator.open = false;
                        self.workspace.panel = None;
                        self.configurator.message = Some("Machine applied and restarted.".into());
                    }
                    Err(error) => {
                        self.pending_output = None;
                        if self.preparation_was_running {
                            self.session.start();
                        }
                        self.anchor = None;
                        self.notice = Some(error.clone());
                        self.configurator.message = Some(error);
                    }
                }
            }
            Err(error) => {
                self.pending_output = None;
                if self.preparation_was_running {
                    self.session.start();
                }
                self.anchor = None;
                self.notice = Some(error.clone());
                self.configurator.message = Some(error);
            }
        }
    }
    fn release_mouse(&mut self) -> Task<Message> {
        let _ = self.session.release_keys();
        if self.mouse_capture.is_some() || self.mouse_capture_pending {
            self.mouse_capture_generation = self.mouse_capture_generation.wrapping_add(1);
        }
        self.mouse_capture_pending = false;
        self.session.release_pointer_buttons();
        self.mouse_capture.take().map_or_else(Task::none, |id| {
            iced::window::release_mouse(id).map(|()| Message::PointerLeft)
        })
    }
    fn log_errors(&mut self) {
        let fault = self.session.fault();
        if fault != self.logged_fault {
            if let Some(error) = fault {
                eprintln!("[ERROR doslet] {}: {error}", self.name);
            }
            self.logged_fault = fault;
        }
        if self.notice != self.logged_notice {
            if let Some(error) = &self.notice {
                eprintln!("[ERROR doslet] {}: {error}", self.name);
            }
            self.logged_notice.clone_from(&self.notice);
        }
    }
    fn reset(&mut self) {
        self.save_core_profile();
        self.notice = None;
        if self.session.dirty_overlay()
            && !self.launch.persist
            && !self.configurator.discard_changes
        {
            self.configurator.open = true;
            self.configurator.message = Some(
                "Export saves or select Discard unsaved temporary changes before Reset.".into(),
            );
            return;
        }
        if let Some(storage) = &mut self.storage
            && let Err(error) = storage.service(&mut self.session)
        {
            self.notice = Some(error);
            return;
        }
        if self.session.storage_waiting() {
            self.notice = Some(
                "Save commit is pending; reset will be available after acknowledgement.".into(),
            );
            return;
        }
        if let Some(path) = &self.source
            && let Err(error) =
                crate::native_input::flush_writes(&mut self.session, path, &self.launch)
        {
            self.notice = Some(error.to_string());
            return;
        }
        #[cfg(test)]
        if let Some((archive, program)) = &self.active_archive {
            match archive.load(program) {
                Ok(mut session) => {
                    session.start();
                    self.seed_pad(&mut session);
                    let _ = session.release_keys();
                    self.session = session;
                    self.name = program.rsplit('/').next().unwrap().into();
                    self.clear_presentation();
                }
                Err(error) => self.notice = Some(error),
            }
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(path) = &self.source {
                match crate::native_input::load_game_with_settings(path, &self.launch) {
                    Ok(mut session) => {
                        match if let Some(storage) = &mut self.storage {
                            storage.prepare_replacement_mut(&mut session, &self.launch)
                        } else {
                            crate::native_storage::NativeStorage::prepare_mut(
                                &mut session,
                                &self.launch,
                            )
                        } {
                            Ok(storage) => {
                                session.start();
                                self.session = session;
                                self.storage = Some(storage);
                            }
                            Err(error) => {
                                self.notice = Some(format!("Cannot reload saves: {error}"));
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        self.notice = Some(format!("Cannot reload game: {error}"));
                        return;
                    }
                }
            } else {
                self.session = Session::demo_configured(self.launch.hardware)
                    .expect("validated demo hardware");
                self.session.set_dynamic_core(!self.launch.no_dynamic_core);
                if let Some(path) = &self.launch.core_profile {
                    crate::native_input::load_core_profile(&mut self.session, path);
                }
            }
        }
        if let Some(generation) = self.pad_generation {
            let _ = Self::route_pad(
                &mut self.session,
                generation,
                self.pad_sequence,
                self.pad_axes,
                self.pad_buttons,
            );
        }
        let _ = self.session.release_keys();
        self.clear_presentation();
    }
    fn save_core_profile(&self) {
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(path) = &self.launch.core_profile {
            crate::native_input::save_core_profile(&self.session, path);
        }
    }
    fn clear_presentation(&mut self) {
        self.dynamic_error_reported = false;
        self.notice = None;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(audio) = &self.audio {
            audio.clear();
            audio.set_active(self.session.status() == SessionStatus::Running);
        }
        let rate = self
            .audio
            .as_ref()
            .map_or(self.midi.sample_rate(), |audio| audio.rate);
        let synth = if self.midi_choice == APPLE_LABEL {
            Timeline::apple(rate)
        } else {
            Timeline::new(rate)
        };
        match synth {
            Ok(engine) => self.midi = engine,
            Err(error) => {
                self.notice = Some(format!("MIDI reset: {error}"));
                self.session.pause();
            }
        }
        self.pcm_mixer.reset(self.midi.sample_rate(), 0).unwrap();
        if self.midi_choice == "CM-32L / MT-32"
            && let Some(directory) = &self.mt32_rom_dir
        {
            let rate = self
                .audio
                .as_ref()
                .map_or(self.midi.sample_rate(), |audio| audio.rate);
            match Self::load_mt32(directory, rate, self.midi_mode) {
                Ok(engine) => self.mt32 = Some(engine),
                Err(error) => {
                    self.notice = Some(format!("CM-32L reset: {error}"));
                    self.session.pause();
                }
            }
        }
        self.routing = Routing::new(self.midi_mode);
        self.midi_activity = crate::midi_activity::MidiActivity::default();
        if let Some(output) = &mut self.host_midi
            && let Err(error) = output.clear()
        {
            self.notice = Some(error);
        }
        self.display = DisplayFrame::default();
        self.anchor = None;
    }
    fn track_physical_key(&mut self, event: &iced::keyboard::Event, enabled: bool) {
        if let Some((key, pressed)) = crate::keyboard::physical(event)
            && let Err(e) = self
                .session
                .key_event(doslet_input::PhysicalKey(key), pressed)
        {
            self.notice = Some(e.to_string());
        }
        if !enabled {
            let _ = self.session.release_keys();
        }
    }
    fn route_pad(
        session: &mut Session,
        generation: u64,
        sequence: u64,
        axes: [i16; 2],
        buttons: [bool; 2],
    ) -> Result<(), RuntimeError> {
        use doslet_input::{InputAction, InputEvent, InputSourceId};
        session.connect_input_joystick(InputSourceId(1), generation)?;
        let epoch = session.input_router_mut().epoch();
        for (index, action) in [
            InputAction::Axis {
                axis: 0,
                value: axes[0],
            },
            InputAction::Axis {
                axis: 1,
                value: axes[1],
            },
            InputAction::Button {
                button: 0,
                pressed: buttons[0],
            },
            InputAction::Button {
                button: 1,
                pressed: buttons[1],
            },
        ]
        .into_iter()
        .enumerate()
        {
            session.route_input(InputEvent {
                session_epoch: epoch,
                source: InputSourceId(1),
                connection_generation: generation,
                sequence: sequence + index as u64,
                action,
            })?;
        }
        Ok(())
    }
    fn seed_pad(&self, session: &mut Session) {
        if let Some(generation) = self.pad_generation {
            let _ = Self::route_pad(
                session,
                generation,
                self.pad_sequence,
                self.pad_axes,
                self.pad_buttons,
            );
            let _ = session.release_input_source(doslet_input::InputSourceId(1));
        }
    }
    fn poll_gamepads(&mut self) {
        use crate::native_gamepad::{GamepadUpdate, uuid_string};
        let Some(gamepads) = &mut self.gamepads else {
            return;
        };
        gamepads.acknowledge();
        for _ in 0..256 {
            let Some(update) = gamepads.poll() else {
                break;
            };
            match update {
                GamepadUpdate::State(state) => {
                    let settings = &self.configurator.active.input;
                    if let Some(id) = &settings.pad_id {
                        if *id != uuid_string(state.uuid) {
                            continue;
                        }
                        if state.ambiguous {
                            let _ = self
                                .session
                                .release_input_source(doslet_input::InputSourceId(1));
                            self.notice=Some("Controller identity is ambiguous; choose a controller index explicitly.".into());
                            continue;
                        }
                    } else if usize::from(settings.pad_index) != state.id {
                        continue;
                    }
                    let axis = |index: u8| {
                        let value = state.axes.get(usize::from(index)).copied().unwrap_or(0);
                        if i32::from(value).abs() * 100
                            < i32::from(settings.deadzone_percent) * 32767
                        {
                            0
                        } else {
                            value
                        }
                    };
                    let button = |index: u8| index < 32 && state.buttons & (1u32 << index) != 0;
                    let axes = [axis(settings.axis_x), axis(settings.axis_y)];
                    let buttons = [button(settings.button_1), button(settings.button_2)];
                    match Self::route_pad(
                        &mut self.session,
                        state.generation,
                        state.sequence,
                        axes,
                        buttons,
                    ) {
                        Ok(()) => {}
                        Err(RuntimeError::Input(doslet_input::InputError::Stale)) => continue,
                        Err(error) => {
                            self.notice = Some(error.to_string());
                            continue;
                        }
                    }
                    self.pad_axes = axes;
                    self.pad_buttons = buttons;
                    self.pad_generation = Some(state.generation);
                    self.pad_sequence = state.sequence;
                    if self.session.status() != SessionStatus::Running
                        || self.configurator.open
                        || self.workspace.panel.is_some()
                        || state.resync
                    {
                        let _ = self
                            .session
                            .release_input_source(doslet_input::InputSourceId(1));
                    }
                }
                GamepadUpdate::Disconnected {
                    id,
                    uuid,
                    generation,
                    sequence,
                } => {
                    if self.configurator.active.input.pad_id.as_ref().map_or(
                        id == usize::from(self.configurator.active.input.pad_index),
                        |selected| *selected == uuid,
                    ) && self.pad_generation == Some(generation)
                    {
                        let epoch = self.session.input_router_mut().epoch();
                        let _ = self.session.route_input(doslet_input::InputEvent {
                            session_epoch: epoch,
                            source: doslet_input::InputSourceId(1),
                            connection_generation: generation,
                            sequence,
                            action: doslet_input::InputAction::ReleaseSource,
                        });
                        self.pad_axes = [0; 2];
                        self.pad_buttons = [false; 2];
                    }
                }
                GamepadUpdate::ReleaseAll => {
                    let _ = self
                        .session
                        .release_input_source(doslet_input::InputSourceId(1));
                }
                GamepadUpdate::Error(error) => {
                    self.notice = Some(format!("Controller unavailable: {error}"))
                }
                GamepadUpdate::Connected {
                    id,
                    name,
                    uuid,
                    generation,
                } => {
                    if self.configurator.active.input.pad_id.as_ref().map_or(
                        id == usize::from(self.configurator.active.input.pad_index),
                        |selected| *selected == uuid,
                    ) {
                        let _ = self
                            .session
                            .connect_input_joystick(doslet_input::InputSourceId(1), generation);
                        self.pad_generation = Some(generation);
                        self.pad_sequence = 0;
                    }
                    self.configurator
                        .controllers
                        .retain(|label| !label.starts_with(&format!("{id}:")));
                    self.configurator
                        .controllers
                        .push(format!("{id}: {name} · {uuid}"));
                }
            }
        }
    }
    fn tick(&mut self) {
        self.tick_at(Instant::now());
    }
    fn tick_at(&mut self, now: Instant) {
        self.poll_gamepads();
        if self.configurator.pending {
            return;
        }
        if let Some(storage) = &mut self.storage
            && let Err(error) = storage.service(&mut self.session)
        {
            self.notice = Some(error);
            self.session.pause();
        }
        if self.session.storage_waiting() {
            self.anchor = None;
            return;
        }

        if self.session.status() != SessionStatus::Running {
            return;
        }
        if self.anchor.is_none()
            && let Some(output) = &self.host_midi
        {
            output.start_clock(self.session.emulated_nanos(), now);
        }
        let (start, guest) = *self
            .anchor
            .get_or_insert((now, self.session.emulated_nanos()));
        let mut target = guest.saturating_add(
            now.duration_since(start)
                .as_nanos()
                .min(u128::from(u64::MAX)) as u64,
        );
        if self.host_midi.is_some() {
            target = target.saturating_add(crate::native_midi::LOOKAHEAD.as_nanos() as u64);
        }
        let mut budget = MAX_STEPS_PER_TICK;
        let previous_memory_wait_steps = self.session.memory_wait_steps();
        while budget != 0
            && self.session.status() == SessionStatus::Running
            && self.session.emulated_nanos() < target
        {
            #[cfg(not(target_arch = "wasm32"))]
            if self
                .audio
                .as_ref()
                .is_some_and(|audio| audio.available() < 512)
            {
                break;
            }
            self.session.prepare_dynamic(2);
            if !self.dynamic_error_reported
                && let Some(error) = self.session.dynamic_error()
            {
                self.dynamic_error_reported = true;
                self.notice = Some(format!(
                    "JIT preparation stopped: {error}. Interpreter fallback remains active."
                ));
            }
            let completed = self.session.advance(budget.min(CHUNK));
            budget -= completed;
            if let Err(error) = self.process_audio() {
                self.notice = Some(error);
                self.session.pause();
                break;
            }
            if completed == 0 || self.session.timeslice_yielded() {
                break;
            }
        }
        if previous_memory_wait_steps == 0 && self.session.memory_wait_steps() != 0 {
            eprintln!(
                "[INFO doslet] {}: experimental timer waits active; batched {} logical steps",
                self.name,
                self.session.memory_wait_steps()
            );
        }
    }
    fn process_audio(&mut self) -> Result<(), String> {
        loop {
            let count = self.session.drain_pcm_timed(&mut self.pcm);
            if count == 0 {
                break;
            }
            self.pcm_mixer
                .receive(&self.pcm[..count])
                .map_err(|e| e.to_string())?;
        }
        if let Some(mt32) = &mut self.mt32 {
            loop {
                let count = mt32
                    .render_until(self.session.emulated_nanos(), &mut self.samples)
                    .map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                self.pcm_mixer.mix(&mut self.samples[..count]);
                if let Some(audio) = &self.audio {
                    audio.push(&self.samples[..count])?;
                }
            }
        } else if self.host_midi.is_some() {
            self.midi
                .skip_until(self.session.emulated_nanos())
                .map_err(|e| e.to_string())?;
            loop {
                let count = self
                    .pcm_mixer
                    .render_until(self.session.emulated_nanos(), &mut self.samples)
                    .map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                if let Some(audio) = &self.audio {
                    audio.push(&self.samples[..count])?;
                }
            }
        } else {
            loop {
                let count = self
                    .midi
                    .render_until(self.session.emulated_nanos(), &mut self.samples)
                    .map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                self.pcm_mixer.mix(&mut self.samples[..count]);
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(audio) = &self.audio {
                    audio.push(&self.samples[..count])?;
                }
            }
        }
        let count = self.session.drain_midi(&mut self.midi_bytes);
        let dialect = self.output_dialect();
        let mut bytes = [0u8; 16384];
        let mut used = 0;
        let mut overflow = false;
        for &byte in &self.midi_bytes[..count] {
            self.routing
                .push(byte, dialect, &mut |message| {
                    if used + message.len() > bytes.len() {
                        overflow = true;
                        return;
                    }
                    bytes[used..used + message.len()].copy_from_slice(message);
                    used += message.len();
                })
                .map_err(|error| format!("MIDI wire error: {error:?}"))?;
        }
        if overflow {
            return Err("Translated MIDI batch capacity exceeded".into());
        }
        if self.routing.mode() != self.midi_mode {
            self.midi_mode = self.routing.mode();
            if self.mt32.is_some() {
                let selection = self.midi_choice.clone();
                self.reselect_midi(&selection)?;
                used = 0; // Replay already includes this batch's device state and notes.
            }
        }
        if let Some(output) = &mut self.host_midi {
            output.push(self.session.emulated_nanos(), &bytes[..used])?;
        } else if let Some(mt32) = &mut self.mt32 {
            for &byte in &bytes[..used] {
                mt32.push_byte(byte).map_err(|error| {
                    format!(
                        "{error} at guest {} ms (notes={}, SysEx={})",
                        self.session.emulated_nanos() / 1_000_000,
                        mt32.note_events(),
                        mt32.sysex_messages()
                    )
                })?;
            }
        } else {
            for &byte in &bytes[..used] {
                self.midi.push_byte(byte).map_err(|e| e.to_string())?;
            }
        }
        if let Some(source) = &self.source {
            crate::native_input::flush_writes(&mut self.session, source, &self.launch)
                .map_err(|error| format!("C: RW writeback failed: {error}"))?;
        }
        Ok(())
    }
    pub fn audio_label(&self) -> String {
        if let Some(output) = &self.host_midi {
            return format!(
                "{} · SB {} nonzero frames",
                output.label(),
                self.pcm_mixer.nonzero()
            );
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(audio) = &self.audio {
            let (frames, _, peak) = audio.stats();
            if let Some(mt32) = &self.mt32 {
                return format!(
                    "CM-32L · {} notes · {} SysEx · peak {:.2} · {} frames · SB {} nonzero frames",
                    mt32.note_events(),
                    mt32.sysex_messages(),
                    peak,
                    frames,
                    self.pcm_mixer.nonzero()
                );
            }
            return format!(
                "{} · {} Hz · {} MIDI notes · peak {:.2} · {} played frames · SB {} nonzero frames",
                self.midi_choice,
                audio.rate,
                self.midi.note_events(),
                peak,
                frames,
                self.pcm_mixer.nonzero()
            );
        }
        "Audio output inactive".into()
    }
    pub fn storage_label(&self) -> String {
        if self.configurator.pending {
            "Preparing machine and sources…".into()
        } else if self.session.pending_commit() {
            "Saving changes…".into()
        } else if self.session.storage_waiting() {
            "Waiting for storage…".into()
        } else if self.session.dirty_overlay() {
            "Unsaved changes".into()
        } else if let Some(revision) = self.session.committed_revision() {
            format!("Saved · revision {revision}")
        } else {
            "No unsaved changes".into()
        }
    }
    fn output_dialect(&self) -> Mode {
        if self.host_midi.is_some() {
            self.midi_output_mode
        } else if self.mt32.is_some() {
            self.midi_mode
        } else {
            Mode::Gm
        }
    }
    fn restored_mt32(&self, rate: u32) -> Result<doslet_mt32::Timeline, String> {
        let directory = self
            .mt32_rom_dir
            .as_ref()
            .ok_or("Use --mt32-rom-dir to load CM-32L ROMs")?;
        let mut engine =
            Self::load_mt32(directory, rate, self.midi_mode).map_err(|error| error.to_string())?;
        engine.rebase(self.session.emulated_nanos());
        let mut replay_error = None;
        self.routing.replay(self.midi_mode, &mut |message| {
            if replay_error.is_some() {
                return;
            }
            for &byte in message {
                if let Err(error) = engine.restore_byte(byte) {
                    replay_error = Some(error.to_string());
                    break;
                }
            }
        });
        if let Some(error) = replay_error {
            return Err(error);
        }
        Ok(engine)
    }
    fn reselect_midi(&mut self, selection: &str) -> Result<(), String> {
        if selection == APPLE_LABEL && !cfg!(target_os = "macos") {
            return Err("Apple MIDI output is available only on native macOS".into());
        }
        if matches!(selection, "CM-32L / MT-32" | "Rust synth" | APPLE_LABEL) {
            // A MIDI dialect/synth change does not require reopening CPAL.
            // Keep the current stream until the replacement synth is ready.
            let new_audio = if self.audio.is_none() {
                Some(crate::audio::Output::new()?)
            } else {
                None
            };
            let rate = self.audio.as_ref().or(new_audio.as_ref()).unwrap().rate;
            let (mt32, midi) = if selection == "CM-32L / MT-32" {
                (Some(self.restored_mt32(rate)?), None)
            } else {
                (None, Some(self.restored_synth(rate, selection)?))
            };
            if let Some(output) = &mut self.host_midi {
                output.clear()?;
            }
            self.mt32 = mt32;
            if let Some(engine) = midi {
                self.midi = engine;
            }
            self.host_midi = None;
            self.pcm_mixer
                .reset(rate, self.session.emulated_nanos())
                .map_err(|e| e.to_string())?;
            if let Some(audio) = new_audio {
                self.audio = Some(audio);
            }
            let audio = self.audio.as_ref().unwrap();
            audio.clear();
            audio.set_active(self.session.status() == SessionStatus::Running);
        } else {
            if self.midi_mode == Mode::Gm && self.midi_output_mode == Mode::Mt32 {
                return Err("GM to an MT-32 host port is unsupported; select MT-32 game mode. GM on moont uses GmDevice.".into());
            }
            let ports = crate::native_midi::ports()?;
            self.midi_choices = crate::native_synth::choices();
            if self.mt32_rom_dir.is_some() {
                self.midi_choices.push("CM-32L / MT-32".into());
            }
            self.midi_choices.extend(ports.names.iter().cloned());
            let mut output = ports.connect(selection)?;
            // Finish the old sender's final silence before replay on the new port.
            drop(self.host_midi.take());
            output.clear()?;
            output.set_active(self.session.status() == SessionStatus::Running)?;
            let mut replay = Vec::with_capacity(SNAPSHOT_CAPACITY);
            self.routing.replay(self.midi_output_mode, &mut |message| {
                replay.extend_from_slice(message)
            });
            output.push(self.session.emulated_nanos(), &replay)?;
            if self.audio.is_none() {
                match crate::audio::Output::new() {
                    Ok(audio) => self.audio = Some(audio),
                    Err(error) => {
                        eprintln!("[WARN doslet] Sound Blaster output unavailable: {error}")
                    }
                }
            }
            let rate = self
                .audio
                .as_ref()
                .map_or(self.midi.sample_rate(), |audio| audio.rate);
            self.midi = Timeline::new(rate).map_err(|e| e.to_string())?;
            self.midi
                .skip_until(self.session.emulated_nanos())
                .map_err(|e| e.to_string())?;
            self.pcm_mixer
                .reset(rate, self.session.emulated_nanos())
                .map_err(|e| e.to_string())?;
            if let Some(audio) = &self.audio {
                audio.clear();
                audio.set_active(self.session.status() == SessionStatus::Running);
            }
            self.mt32 = None;
            self.host_midi = Some(output);
        }
        self.midi_choice = self
            .host_midi
            .as_ref()
            .map_or_else(|| selection.into(), |output| output.name.clone());
        self.anchor = None;
        self.notice = None;
        Ok(())
    }
    fn select_midi(&mut self, selection: &str) -> Result<(), String> {
        let ready = match selection {
            "Rust synth" | APPLE_LABEL => self.audio.is_some() && self.mt32.is_none(),
            "CM-32L / MT-32" => self.audio.is_some() && self.mt32.is_some(),
            _ => self.host_midi.as_ref().is_some_and(|o| o.check().is_ok()),
        };
        if selection == self.midi_choice && ready && self.notice.is_none() {
            return Ok(());
        }
        self.reselect_midi(selection)
    }
    fn restored_synth(&self, rate: u32, selection: &str) -> Result<Timeline, String> {
        let mut engine = if selection == APPLE_LABEL {
            Timeline::apple(rate)?
        } else {
            Timeline::new(rate)?
        };
        engine.skip_until(self.session.emulated_nanos())?;
        let mut replay_error = None;
        self.routing.replay(Mode::Gm, &mut |message| {
            if replay_error.is_some() {
                return;
            }
            for &byte in message {
                if let Err(error) = engine.restore_byte(byte) {
                    replay_error = Some(error);
                    break;
                }
            }
        });
        if let Some(error) = replay_error {
            return Err(error);
        }
        Ok(engine)
    }
    pub fn midi_mode_label(&self) -> String {
        let route = if self.midi_mode == Mode::Mt32 && self.output_dialect() == Mode::Gm {
            "MT-32 → GM (approximation)"
        } else if self.mt32.is_some() && self.midi_mode == Mode::Gm {
            "GM → CM-32L (approximation, 8 melodic parts)"
        } else {
            ""
        };
        let stats = self.routing.translator().stats();
        let lcd = String::from_utf8_lossy(self.routing.translator().display());
        format!(
            "{route}{} · {} SysEx · {} unmapped notes · {lcd}",
            if self.routing.upgraded() {
                " · Roland detected: upgraded to MT-32"
            } else {
                ""
            },
            stats.sysex,
            stats.unmapped_notes
        )
    }
    pub fn view(&self) -> Element<'_, Message> {
        crate::ui::view(self)
    }
    pub fn theme(&self) -> Theme {
        self.theme.clone()
    }
    pub fn pointer_dimensions(&self) -> (u16, u16) {
        self.session.pointer_dimensions()
    }
    pub fn has_mouse_capture(&self) -> bool {
        self.mouse_capture.is_some() && !self.mouse_capture_pending
    }
    pub fn hide_host_cursor(&self) -> bool {
        self.has_mouse_capture()
            || self.configurator.active.input.hide_cursor
                && self.session.pointer_in_use()
                && self.session.status() == SessionStatus::Running
                && !self.configurator.open
                && self.workspace.panel.is_none()
                && !self.configurator.pending
    }
    pub fn subscription(&self) -> Subscription<Message> {
        let ticking =
            self.session.status() == SessionStatus::Running || self.session.storage_waiting();
        let tick = if ticking {
            iced::time::every(Duration::from_millis(16)).map(|_| Message::Tick)
        } else {
            Subscription::none()
        };
        Subscription::batch([
            tick,
            if ticking {
                Subscription::none()
            } else {
                self.gamepads
                    .as_ref()
                    .map_or_else(Subscription::none, |pads| {
                        pads.subscription().map(|()| Message::GamepadsReady)
                    })
            },
            iced::window::close_requests().map(Message::CloseRequested),
            DisplaySizing::subscription().map(Message::DisplaySizing),
            iced::event::listen_with(|event, status, _| {
                if matches!(event, iced::Event::Window(iced::window::Event::Unfocused)) {
                    return Some(Message::FocusLost);
                }
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    #[cfg(all(
        feature = "dynamic-core",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    mod native_profile_fixture {
        include!("../../../tests/fixtures/protected_graphics.rs");
    }
    #[cfg(all(
        feature = "dynamic-core",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    #[test]
    fn native_window_close_saves_observations_before_returning_the_close_task() {
        let fixture = crate::profile::tests::Fixture::new("");
        let path = fixture.root.join("native.core-profile");
        let mut program = native_profile_fixture::protected_graphics_com();
        let code = [0x40, 0x41, 0x42, 0xEB, 0xFB];
        program[0x300..0x300 + code.len()].copy_from_slice(&code);
        let mut app = Doslet::new();
        app.session = Session::new(&program).unwrap();
        app.launch.core_profile = Some(path.clone());
        app.session.start();
        assert_eq!(app.session.advance(1000), 1000);
        let mut expected = vec![0; doslet_x86::DYNAMIC_PROFILE_MAX_BYTES];
        let length = app.session.write_dynamic_profile(&mut expected);
        assert!(length > 6, "synthetic loop must record hot code");
        assert!(!path.exists());
        let _ = app.update(Message::CloseRequested(iced::window::Id::unique()));
        assert_eq!(std::fs::read(path).unwrap(), expected[..length]);
    }
    #[test]
    fn captured_at_typing_is_discarded_and_paused_release_allows_a_fresh_press() {
        use iced::keyboard::{
            Event, Key, Location, Modifiers,
            key::{Code, Physical},
        };
        let press = Event::KeyPressed {
            key: Key::Character("a".into()),
            modified_key: Key::Character("a".into()),
            physical_key: Physical::Code(Code::KeyA),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
            text: Some("a".into()),
            repeat: false,
        };
        let release = Event::KeyReleased {
            key: Key::Character("a".into()),
            modified_key: Key::Character("a".into()),
            physical_key: Physical::Code(Code::KeyA),
            location: Location::Standard,
            modifiers: Modifiers::empty(),
        };
        let mut app = Doslet::new();
        let hardware = doslet_config::HardwareConfig {
            keyboard: doslet_config::KeyboardMode::At,
            ..Default::default()
        };
        app.session = Session::demo_configured(hardware).unwrap();
        app.configurator.active.hardware = hardware;
        app.session.start();
        let _ = app.update(Message::KeyboardCaptured(press.clone()));
        assert_eq!(
            app.session.input_router_mut().next_event(),
            Some(doslet_input::GuestEvent::ReleaseAll)
        );
        assert_eq!(app.session.input_router_mut().next_event(), None);
        app.session.pause();
        let _ = app.update(Message::Keyboard(release));
        while app.session.input_router_mut().next_event().is_some() {}
        app.session.start();
        let _ = app.update(Message::Keyboard(press));
        assert_eq!(
            app.session.input_router_mut().next_event(),
            Some(doslet_input::GuestEvent::Key {
                key: doslet_input::PhysicalKey(4),
                pressed: true
            })
        );
        assert_eq!(app.session.steps(), 0);
    }
    #[test]
    fn failed_candidate_restores_running_session_and_reset_recipe() {
        let mut app = Doslet::new();
        app.session.start();
        app.session.advance(25);
        let steps = app.session.steps();
        app.source = Some(std::path::PathBuf::from("old-game.exe"));
        let old = app.configurator.active.clone();
        let mut draft = old.clone();
        draft.hardware.memory_mib = 8;
        draft.game.executable = "/missing/doslet-game.exe".into();
        draft.output.audio = false;
        app.configurator.draft = draft.clone();
        app.configurator.pending = true;
        app.preparation_was_running = true;
        app.session.pause();
        let mut settings = app.launch.clone();
        settings.hardware = draft.hardware;
        let result = Doslet::prepare_candidate(draft, settings, None, None);
        assert!(result.result.is_err());
        app.finish_preparation(Ok(std::sync::Arc::new(std::sync::Mutex::new(Some(result)))));
        assert_eq!(app.session.steps(), steps);
        assert_eq!(app.session.status(), SessionStatus::Running);
        assert_eq!(app.configurator.active, old);
        assert_eq!(app.launch.hardware.memory_mib, 16);
        assert_eq!(
            app.source.as_deref(),
            Some(std::path::Path::new("old-game.exe"))
        );
        assert!(!app.configurator.pending);
    }
    #[test]
    fn prepared_hardware_becomes_the_active_session_and_reset_recipe() {
        let mut app = Doslet::new();
        let mut spec = doslet_config::MachineSpec::default();
        spec.hardware.memory_mib = 4;
        spec.hardware.sb16 = false;
        spec.output.audio = false;
        let settings = crate::native_input::LaunchSettings {
            hardware: spec.hardware,
            machine_spec: Some(spec.clone()),
            no_dynamic_core: true,
            core_profile: Some(std::path::PathBuf::from("unused-disabled-core-profile")),
            ..Default::default()
        };
        app.dynamic_error_reported = true;
        let candidate = Doslet::prepare_candidate(spec.clone(), settings, None, None);
        assert!(candidate.result.is_ok());
        app.pending_output = Some(Box::new(Doslet::prepare_output(&spec).unwrap()));
        app.configurator.pending = true;
        app.finish_preparation(Ok(std::sync::Arc::new(std::sync::Mutex::new(Some(
            candidate,
        )))));
        assert_eq!(app.session.hardware_config(), spec.hardware);
        assert_eq!(app.configurator.active, spec);
        assert_eq!(app.launch.hardware.memory_mib, 4);
        assert!(app.launch.no_dynamic_core);
        assert!(!app.session.dynamic_core_enabled());
        assert!(!app.dynamic_error_reported);
        assert_eq!(
            app.launch.core_profile.as_deref(),
            Some(std::path::Path::new("unused-disabled-core-profile"))
        );
        app.reset();
        assert_eq!(app.session.hardware_config(), spec.hardware);
        assert!(!app.session.dynamic_core_enabled());
    }
    #[test]
    fn configured_demo_respects_disabled_native_core_on_launch_and_reset() {
        let mut app = Doslet::from_cli(crate::cli::Cli {
            no_audio: true,
            no_dynamic_core: true,
            memory_mib: Some(4),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(app.session.hardware_config().memory_mib, 4);
        assert!(!app.session.dynamic_core_enabled());
        app.reset();
        assert_eq!(app.session.hardware_config().memory_mib, 4);
        assert!(!app.session.dynamic_core_enabled());
    }
    include!("../../../tests/fixtures/sb16_pcm.rs");
    #[test]
    fn uncaptured_hidden_cursor_keeps_absolute_motion_and_passes_escape_to_dos() {
        let mut app = Doslet::new();
        // Read the mouse, then one BIOS key and store AX at DS:0200h.
        app.session = Session::new(&[
            0xB8, 3, 0, 0xCD, 0x33, 0xB4, 0, 0xCD, 0x16, 0xA3, 0, 2, 0xEB, 0xFE,
        ])
        .unwrap();
        app.configurator.active.input.capture = false;
        app.configurator.active.input.hide_cursor = true;
        app.session.start();
        assert!(!app.hide_host_cursor());
        app.session.advance(100);
        assert!(app.session.waiting_for_key());
        assert!(app.hide_host_cursor());
        let _ = app.update(Message::PointerMoved(123, 45));
        let _ = app.update(Message::PointerButton(0, true));
        assert_eq!(app.session.pointer_position(), (123, 45));
        assert!(app.mouse_capture.is_none());
        assert!(!app.mouse_capture_pending);
        let escape = iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape);
        let _ = app.update(Message::Keyboard(iced::keyboard::Event::KeyPressed {
            key: escape.clone(),
            modified_key: escape,
            physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::Escape),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        }));
        // The BIOS keyboard wait resumes at the next guest timer interrupt.
        app.session.advance(1_000_000);
        assert_eq!(app.session.ram_view(0x10200..0x10202).unwrap(), &[0x1B, 1]);
        assert!(app.hide_host_cursor());
        app.workspace.panel = Some(crate::ui::Panel::Audio);
        assert!(!app.hide_host_cursor());
        app.workspace.panel = None;
        app.session.pause();
        assert!(!app.hide_host_cursor());
    }

    #[test]
    fn mouse_capture_uses_relative_motion_releases_on_escape_and_ignores_stale_events() {
        use iced::window::MouseCaptureEvent;
        let mut app = Doslet::new();
        app.session = Session::new(&[0xEB, 0xFE]).unwrap();
        app.session.start();
        let id = iced::window::Id::unique();
        let _ = app.update(Message::PointerButton(0, true));
        let generation = app.mouse_capture_generation;
        let _ = app.update(Message::CaptureWindow(id, generation));
        assert!(!app.hide_host_cursor());
        let _ = app.update(Message::MouseCapture(
            id,
            generation,
            MouseCaptureEvent::Acquired,
        ));
        assert!(app.hide_host_cursor());
        let before = app.session.pointer_position();
        let _ = app.update(Message::PointerMoved(0, 0));
        assert_eq!(app.session.pointer_position(), before);
        let _ = app.update(Message::MouseCapture(
            id,
            generation,
            MouseCaptureEvent::Motion {
                dx: 20.0,
                dy: -15.0,
            },
        ));
        assert_eq!(
            app.session.pointer_position(),
            (before.0 + 20, before.1 - 15)
        );
        let escape = iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape);
        let _ = app.update(Message::Keyboard(iced::keyboard::Event::KeyPressed {
            key: escape.clone(),
            modified_key: escape,
            physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::Escape),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        }));
        assert!(!app.hide_host_cursor());
        let _ = app.update(Message::PointerButton(0, true));
        let next = app.mouse_capture_generation;
        let _ = app.update(Message::MouseCapture(
            id,
            generation,
            MouseCaptureEvent::Released,
        ));
        assert!(app.mouse_capture_pending);
        let _ = app.update(Message::CaptureWindow(id, next));
        let _ = app.update(Message::MouseCapture(id, next, MouseCaptureEvent::Acquired));
        let _ = app.update(Message::RunPause);
        assert!(!app.hide_host_cursor());
        assert_eq!(app.session.status(), SessionStatus::Paused);
        let _ = app.update(Message::MouseCapture(
            id,
            next,
            MouseCaptureEvent::Motion { dx: 10.0, dy: 10.0 },
        ));
        assert_eq!(
            app.session.pointer_position(),
            (before.0 + 20, before.1 - 15)
        );
    }
    #[test]
    fn rw_writeback_persists_closed_files_and_overlay_keeps_host_unchanged() {
        use doslet_dos::files::ReadOnlyDrive;
        let root = std::env::temp_dir().join(format!("doslet-rw-test-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("Heroes.cfg");
        std::fs::write(&target, b"original").unwrap();
        let mut drive = ReadOnlyDrive::new();
        drive.mount(b"HEROES.CFG", b"original".to_vec()).unwrap();
        let mut drive = drive.with_write_overlay(1024);
        let handle = drive.create(b"HEROES.CFG", 0).unwrap();
        drive
            .write_buffer(handle, 3)
            .unwrap()
            .copy_from_slice(b"new");
        let mut session = Session::new(&[0x90]).unwrap().with_drive(drive);
        let settings = crate::native_input::LaunchSettings {
            persist: true,
            ..Default::default()
        };
        let executable = root.join("GAME.EXE");
        assert_eq!(
            crate::native_input::flush_writes(&mut session, &executable, &settings).unwrap(),
            0
        );
        let mut drive = ReadOnlyDrive::new();
        drive.mount(b"HEROES.CFG", b"original".to_vec()).unwrap();
        let mut drive = drive.with_write_overlay(1024);
        let handle = drive.create(b"HEROES.CFG", 0).unwrap();
        drive
            .write_buffer(handle, 3)
            .unwrap()
            .copy_from_slice(b"new");
        drive.close(handle).unwrap();
        let mut session = Session::new(&[0x90]).unwrap().with_drive(drive);
        let overlay = crate::native_input::LaunchSettings::default();
        assert_eq!(
            crate::native_input::flush_writes(&mut session, &executable, &overlay).unwrap(),
            0
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        assert_eq!(
            crate::native_input::flush_writes(&mut session, &executable, &settings).unwrap(),
            1
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(
            crate::native_input::flush_writes(&mut session, &executable, &settings).unwrap(),
            0
        );
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn sb16_pcm_flows_through_native_mixer_without_host_device() {
        let mut app = Doslet::new();
        app.session = Session::new(&sb16_pcm_program()).unwrap();
        app.session.start();
        for _ in 0..100 {
            app.session.advance(10000);
            app.process_audio().unwrap();
        }
        assert_eq!(
            app.session.status(),
            SessionStatus::Running,
            "{:?}",
            app.session.fault()
        );
        assert!(app.pcm_mixer.nonzero() > 1000);
        assert!(
            app.samples
                .iter()
                .flatten()
                .any(|sample| sample.abs() > 0.1)
        );
    }
    #[test]
    #[ignore = "opens the user's audio device and plays a short synthetic SB16 tone"]
    fn local_sb16_pcm_reaches_host_output() {
        use clap::Parser;
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cli = crate::cli::Cli::parse_from([
            std::ffi::OsString::from("doslet"),
            "--config".into(),
            repo.join("games/HoMnM.toml").into_os_string(),
            "--midi-output".into(),
            "rust".into(),
        ]);
        let mut app = Doslet::from_cli(cli).unwrap();
        assert!(app.audio.is_some());
        app.session = Session::new(&sb16_pcm_program()).unwrap();
        app.session.start();
        for _ in 0..100 {
            app.session.advance(10000);
            app.process_audio().unwrap();
        }
        std::thread::sleep(Duration::from_millis(300));
        let (played, underruns, peak) = app.audio.as_ref().unwrap().stats();
        println!(
            "SB16 host output: source={} nonzero={} played={played} underruns={underruns} peak={peak}",
            app.pcm_mixer.received(),
            app.pcm_mixer.nonzero()
        );
        assert!(played > 1000);
        assert!(peak > 0.1);
    }

    #[test]
    #[ignore = "requires the user's HoMnM installation and ISO; runs the real setup without window/audio"]
    fn local_heroes_setup_profile_reaches_menus_and_exits_without_saving() {
        use clap::Parser;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cli = crate::cli::Cli::parse_from([
            std::ffi::OsString::from("doslet"),
            "--config".into(),
            root.join("games/HoMnM.toml").into_os_string(),
            "--game".into(),
            root.join("games/HoMnM/SETUP.EXE").into_os_string(),
            "--no-audio".into(),
        ]);
        let mut app = Doslet::from_cli(cli).unwrap();
        let text = |session: &Session| -> String {
            session
                .ram_view(0xB8000..0xB8000 + 4000)
                .unwrap()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|cell| char::from(cell[0]))
                .collect()
        };
        app.session.advance(90_000_000);
        assert_eq!(
            app.session.status(),
            SessionStatus::Running,
            "{:?}",
            app.session.fault()
        );
        assert!(text(&app.session).contains("Main Menu"));
        assert!(text(&app.session).contains("VESA Compatible"));
        app.session.push_key(0x02, b'1').unwrap();
        app.session.advance(20_000_000);
        assert!(text(&app.session).contains("Select a New Sound Card"));
        app.session.push_key(0x01, 0x1B).unwrap();
        app.session.advance(20_000_000);
        assert!(text(&app.session).contains("Main Menu"));
        app.session.push_key(0x01, 0x1B).unwrap();
        app.session.advance(20_000_000);
        assert_eq!(
            app.session.status(),
            SessionStatus::Exited(0),
            "{:?}",
            app.session.fault()
        );
        println!(
            "SETUP profile: menus and exit verified at {} steps",
            app.session.steps()
        );
    }

    #[test]
    #[ignore = "requires the user's HEROES installation and homm1.gog ISO"]
    fn local_heroes_profile_passes_the_cd_check() {
        use clap::Parser;
        let config =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../games/HoMnM.toml");
        let cli = crate::cli::Cli::parse_from([
            std::ffi::OsString::from("doslet"),
            "--config".into(),
            config.into_os_string(),
            "--no-audio".into(),
        ]);
        let mut app = Doslet::from_cli(cli).unwrap();
        app.session.advance(30_000_000);
        assert_eq!(
            app.session.status(),
            SessionStatus::Running,
            "{:?}",
            app.session.fault()
        );
        let console = String::from_utf8_lossy(app.session.console());
        assert!(!console.contains("Unable to access CD Drive"));
        assert!(!console.contains("must have the Heroes CD"));
        println!(
            "HEROES profile: {:?}, steps={}, mode={:?}",
            app.session.status(),
            app.session.steps(),
            app.session.video_mode()
        );
    }

    #[test]
    #[ignore = "requires the user's local games and TOML profiles; runs without a window or audio device"]
    fn local_toml_profiles_load_real_games_without_audio() {
        use clap::Parser;
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for name in ["CIV", "PRINCE", "HoMnM"] {
            let profile = root.join(format!("games/{name}.toml"));
            let cli = crate::cli::Cli::parse_from([
                std::ffi::OsString::from("doslet"),
                "--config".into(),
                profile.into_os_string(),
                "--no-audio".into(),
            ]);
            let mut app = Doslet::from_cli(cli).unwrap();
            assert!(app.audio.is_none());
            assert!(app.mt32.is_none());
            app.session.advance(1_000_000);
            assert_ne!(
                app.session.status(),
                SessionStatus::Faulted,
                "{name}: {:?}",
                app.session.fault()
            );
            println!(
                "{name}: {:?}, steps={}",
                app.session.status(),
                app.session.steps()
            );
        }
    }

    #[test]
    fn profile_arguments_working_directory_and_mode_survive_game_reset() {
        let fixture = crate::profile::tests::Fixture::new(
            r#"
version = 1
name = "Configured game"
[game]
executable = "../TINY.EXE"
arguments = ["/test", "two words"]
working_directory = 'C:\SUB'
[midi]
game_mode = "mt32"
[audio]
enabled = false
"#,
        );
        std::fs::create_dir(fixture.root.join("SUB")).unwrap();
        std::fs::write(fixture.root.join("SUB/DATA.DAT"), b"data").unwrap();
        let mut app = Doslet::from_cli(fixture.cli(&[])).unwrap();
        assert_eq!(app.name, "Configured game");
        assert_eq!(app.midi_mode, Mode::Mt32);
        assert!(app.audio.is_none());
        let tail = b" /test \"two words\"";
        for _ in 0..2 {
            assert_eq!(
                app.session.ram_view(0x10080..0x10081).unwrap(),
                &[tail.len() as u8]
            );
            assert_eq!(
                app.session.ram_view(0x10081..0x10081 + tail.len()).unwrap(),
                tail
            );
            app.session.advance(100);
            assert_eq!(app.session.status(), SessionStatus::Exited(7));
            assert_eq!(app.session.console(), b"Relocated MZ");
            let _ = app.update(Message::Reset);
            assert_eq!(app.session.status(), SessionStatus::Running);
            assert_eq!(app.midi_mode, Mode::Mt32);
        }
    }

    #[test]
    fn incompatible_host_dialect_selection_keeps_the_existing_output_and_mode() {
        let mut app = Doslet::new();
        let (output, _receiver) = crate::native_midi::Output::mock();
        app.midi_choice = output.name.clone();
        app.host_midi = Some(output);
        let _ = app.update(Message::MidiOutputModeSelected(Mode::Mt32));
        assert_eq!(app.midi_output_mode, Mode::Gm);
        assert!(app.host_midi.is_some());
        assert!(app.notice.is_some());
        app.midi_mode = Mode::Mt32;
        app.routing.set_mode(Mode::Mt32);
        app.midi_output_mode = Mode::Mt32;
        let _ = app.update(Message::MidiModeSelected(Mode::Gm));
        assert_eq!(app.midi_mode, Mode::Mt32);
        assert_eq!(app.routing.mode(), Mode::Mt32);
        assert!(app.host_midi.is_some());
    }
    #[test]
    fn native_rust_output_upgrades_roland_and_accepts_translated_notes() {
        let mut app = Doslet::new();
        let mut program = vec![0xBA, 0x31, 3, 0xB0, 0x3F, 0xEE, 0xBA, 0x30, 3];
        for byte in [
            0xF0, 0x41, 0x10, 0x16, 0x12, 0x20, 0, 0, b'A', 0x1F, 0xF7, 0x91, 60, 100,
        ] {
            program.extend_from_slice(&[0xB0, byte, 0xEE]);
        }
        program.extend_from_slice(&[0xEB, 0xFE]);
        app.session = Session::new(&program).unwrap();
        app.session.start();
        app.session.advance(1000);
        app.process_audio().unwrap();
        assert_eq!(app.midi_mode, Mode::Mt32);
        assert_eq!(app.routing.translator().stats().sysex, 1);
        assert_eq!(app.midi.note_events(), 1);
        assert!(app.midi_mode_label().contains("approximation"));
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn apple_output_routes_mt32_replays_notes_and_survives_pause_and_reset() {
        let mut app = Doslet::new();
        app.midi_choice = APPLE_LABEL.into();
        app.midi = Timeline::apple(48000).unwrap();
        app.midi_output_mode = Mode::Mt32; // Applies only to external ports.
        let mut program = vec![0xBA, 0x31, 3, 0xB0, 0x3F, 0xEE, 0xBA, 0x30, 3];
        for byte in [
            0xF0, 0x41, 0x10, 0x16, 0x12, 0x20, 0, 0, b'A', 0x1F, 0xF7, 0x91, 60, 100,
        ] {
            program.extend_from_slice(&[0xB0, byte, 0xEE]);
        }
        program.extend_from_slice(&[0xEB, 0xFE]);
        app.session = Session::new(&program).unwrap();
        app.session.start();
        app.session.advance(1000);
        app.process_audio().unwrap();
        assert_eq!(app.midi_mode, Mode::Mt32);
        assert_eq!(app.output_dialect(), Mode::Gm);
        assert_eq!(app.midi.note_events(), 1);
        let now = app.session.emulated_nanos();
        let mut restored = app.restored_synth(48000, APPLE_LABEL).unwrap();
        assert_eq!(restored.note_events(), 1);
        assert_eq!(restored.render_until(now, &mut app.samples).unwrap(), 0);
        let mut peak = 0.0_f32;
        loop {
            let count = restored
                .render_until(now + 100_000_000, &mut app.samples)
                .unwrap();
            if count == 0 {
                break;
            }
            peak = app.samples[..count]
                .iter()
                .flatten()
                .fold(peak, |p, s| p.max(s.abs()));
        }
        assert!(peak > 0.0001, "restored translated note must produce PCM");
        let _ = app.update(Message::RunPause);
        let _ = app.update(Message::Tick);
        assert_eq!(app.session.emulated_nanos(), now);
        assert_eq!(app.midi.note_events(), 1);
        let _ = app.update(Message::RunPause);
        assert_eq!(app.midi.note_events(), 1);
        let _ = app.update(Message::Reset);
        assert_eq!(app.midi_choice, APPLE_LABEL);
        assert!(matches!(app.midi, Timeline::Apple(_)));
        assert_eq!(app.midi.note_events(), 0);
        assert_eq!(app.session.emulated_nanos(), 0);
        assert!(app.notice.is_none(), "{:?}", app.notice);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn apple_output_is_rejected_without_changing_the_current_output() {
        let mut app = Doslet::new();
        let error = app.select_midi(APPLE_LABEL).unwrap_err();
        assert!(error.contains("only on native macOS"));
        assert_eq!(app.midi_choice, "Rust synth");
        assert!(!app.midi_choices.iter().any(|v| v == APPLE_LABEL));
    }

    #[test]
    fn host_midi_lookahead_is_bounded_and_rebases_after_pause_and_reset() {
        let mut app = Doslet::new();
        app.session = Session::new(&[0xEB, 0xFE]).unwrap();
        app.session.start();
        let (mut output, _receiver) = crate::native_midi::Output::mock();
        output.set_active(true).unwrap();
        app.host_midi = Some(output);
        let start = Instant::now();
        app.tick_at(start);
        assert_eq!(app.session.steps(), u64::from(MAX_STEPS_PER_TICK));
        app.tick_at(start + Duration::from_millis(16));
        assert!((96_000_000..98_000_000).contains(&app.session.emulated_nanos()));
        let steps = app.session.steps();
        let _ = app.update(Message::RunPause);
        app.tick_at(start + Duration::from_secs(60));
        assert_eq!(app.session.steps(), steps);
        let _ = app.update(Message::RunPause);
        let guest = app.session.emulated_nanos();
        app.tick_at(start + Duration::from_secs(120));
        assert_eq!(app.session.steps() - steps, u64::from(MAX_STEPS_PER_TICK));
        assert!(app.session.emulated_nanos() - guest < 80_000_000);
        let _ = app.update(Message::Reset);
        assert_eq!(app.session.emulated_nanos(), 0);
        assert!(app.anchor.is_none());
        assert!(app.notice.is_none());
        let _ = app.update(Message::RunPause);
        app.tick_at(start + Duration::from_secs(180));
        assert!(app.session.emulated_nanos() < 80_000_000);
    }

    #[test]
    fn guest_timeslice_yield_ends_the_native_tick() {
        let mut app = Doslet::new();
        app.session = Session::new(&[
            0xB8, 0x80, 0x16, 0xCD, 0x2F, // Yield.
            0xB8, 0, 0x4C, 0xCD, 0x21, // Exit only on the next tick.
        ])
        .unwrap();
        app.session.start();
        let start = Instant::now();
        app.tick_at(start);
        app.tick_at(start + Duration::from_millis(16));
        assert!(app.session.timeslice_yielded());
        assert_eq!(app.session.status(), SessionStatus::Running);
        app.tick_at(start + Duration::from_millis(32));
        assert_eq!(app.session.status(), SessionStatus::Exited(0));
    }

    #[test]
    fn archive_selection_failure_and_reset_preserve_the_active_game() {
        use std::{
            io::{Cursor, Write},
            sync::Arc,
        };
        use zip::{ZipWriter, write::SimpleFileOptions};
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        for name in ["GAME/START.COM", "GAME/OTHER.COM"] {
            zip.start_file(name, SimpleFileOptions::default()).unwrap();
            zip.write_all(&[0xb8, 0, 0x4c, 0xcd, 0x21]).unwrap();
        }
        let mut app = Doslet::new();
        let _ = app.update(Message::ArchiveLoaded(Ok(Some(Arc::new(
            zip.finish().unwrap().into_inner(),
        )))));
        assert!(app.program.is_none());
        let _ = app.update(Message::ProgramSelected("GAME/START.COM".into()));
        let _ = app.update(Message::StartArchive);
        assert_eq!(app.name, "START.COM");
        app.session.advance(100);
        assert_eq!(app.session.status(), SessionStatus::Exited(0));
        let _ = app.update(Message::ProgramSelected("GAME/OTHER.COM".into()));
        let _ = app.update(Message::ArchiveLoaded(Ok(Some(Arc::new(
            b"invalid".to_vec(),
        )))));
        assert!(app.notice.is_some());
        let _ = app.update(Message::Reset);
        assert_eq!(app.name, "START.COM");
        assert_eq!(app.session.status(), SessionStatus::Running);
        assert_eq!(app.session.steps(), 0);
        assert!(app.notice.is_none());
        let _ = app.update(Message::ArchiveLoaded(Ok(None)));
        assert!(app.archive.is_some());
    }

    #[test]
    fn controller_notifications_update_a_paused_session_without_advancing_it() {
        use crate::native_gamepad::{GamepadUpdate, NativeGamepads, PadState, uuid_string};
        let mut app = Doslet::new();
        app.session.start();
        app.session.advance(25);
        app.session.pause();
        let steps = app.session.steps();
        let nanos = app.session.emulated_nanos();
        let (pads, send) = NativeGamepads::test_pair();
        app.gamepads = Some(pads);
        let uuid = [0xAB; 16];
        send(GamepadUpdate::Connected {
            id: 0,
            name: "Test controller".into(),
            uuid: uuid_string(uuid),
            generation: 1,
        });
        send(GamepadUpdate::State(PadState {
            id: 0,
            uuid,
            generation: 1,
            sequence: 1,
            resync: true,
            ambiguous: false,
            axes: [32767, -32767, 0, 0, 0, 0, 0, 0],
            buttons: 1,
        }));
        let _ = app.update(Message::GamepadsReady);
        assert!(
            app.configurator
                .controllers
                .iter()
                .any(|v| v.contains("Test controller"))
        );
        assert_eq!(app.pad_generation, Some(1));
        assert_eq!(app.pad_axes, [32767, -32767]);
        assert_eq!(app.pad_buttons, [true, false]);
        send(GamepadUpdate::Disconnected {
            id: 0,
            uuid: uuid_string(uuid),
            generation: 1,
            sequence: 5,
        });
        let _ = app.update(Message::GamepadsReady);
        assert_eq!(app.pad_axes, [0; 2]);
        assert_eq!(app.pad_buttons, [false; 2]);
        assert_eq!(app.session.status(), SessionStatus::Paused);
        assert_eq!(app.session.steps(), steps);
        assert_eq!(app.session.emulated_nanos(), nanos);
        assert!(app.notice.is_none(), "{:?}", app.notice);
    }

    #[test]
    fn pause_does_not_accumulate_host_time_and_reset_restores_demo() {
        let mut app = Doslet::new();
        let start = Instant::now();
        let _ = app.update(Message::RunPause);
        app.tick_at(start);
        app.tick_at(start + Duration::from_micros(100));
        assert!(app.session.steps() > 0);
        let _ = app.update(Message::RunPause);
        let steps = app.session.steps();
        app.tick_at(start + Duration::from_secs(60));
        assert_eq!(app.session.steps(), steps);
        let _ = app.update(Message::RunPause);
        app.tick_at(start + Duration::from_secs(120));
        assert_eq!(app.session.steps(), steps);
        let _ = app.update(Message::Reset);
        assert_eq!(app.session.status(), SessionStatus::Ready);
        assert_eq!(app.session.steps(), 0);
        assert_eq!(app.midi.note_events(), 0);
    }

    #[test]
    #[ignore = "requires the user's local PRINCE installation; run from repository root"]
    fn local_prince_moont_queue_survives_repeated_restoration() {
        check_local_prince_moont_queue(121_000_000_000, true);
    }

    #[test]
    #[ignore = "requires local PRINCE and DOSLET_MT32_ROM_DIR; run from repository root"]
    fn local_prince_moont_queue_survives_ten_guest_minutes() {
        check_local_prince_moont_queue(600_000_000_000, false);
    }

    fn check_local_prince_moont_queue(until: u64, restore: bool) {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let legacy = root.join("PRINCE/PRINCE.EXE");
        let path = if legacy.is_file() {
            legacy
        } else {
            root.join("games/PRINCE/PRINCE.EXE")
        };
        let directory = std::path::PathBuf::from(
            std::env::var_os("DOSLET_MT32_ROM_DIR").expect("set DOSLET_MT32_ROM_DIR"),
        );
        let mut app = Doslet::new();
        let cli = crate::cli::Cli {
            config: Some(root.join("games/PRINCE.toml")),
            no_audio: true,
            ..Default::default()
        }
        .resolve()
        .unwrap();
        app.launch = cli.launch;
        app.session = crate::native_input::load_game_with_settings(&path, &app.launch).unwrap();
        app.mt32_rom_dir = Some(directory);
        app.mt32 = Some(app.restored_mt32(44100).unwrap());
        app.pcm_mixer.reset(44100, 0).unwrap();
        app.session.start();
        let mut batch = 0;
        while app.session.emulated_nanos() < until {
            if restore && batch != 0 && batch % 20000 == 0 {
                app.mt32 = Some(app.restored_mt32(44100).unwrap());
            }
            app.session.prepare_dynamic(2);
            assert_ne!(app.session.advance(CHUNK), 0);
            assert_eq!(
                app.session.status(),
                SessionStatus::Running,
                "{:?}",
                app.session.fault()
            );
            app.process_audio().unwrap_or_else(|error| {
                panic!(
                    "step={} nanos={} notes={} sysex={}: {error}",
                    app.session.steps(),
                    app.session.emulated_nanos(),
                    app.mt32.as_ref().unwrap().note_events(),
                    app.mt32.as_ref().unwrap().sysex_messages()
                )
            });
            batch += 1;
        }
        eprintln!(
            "PRINCE moont: guest={} ms, notes={}, SysEx={}",
            app.session.emulated_nanos() / 1_000_000,
            app.mt32.as_ref().unwrap().note_events(),
            app.mt32.as_ref().unwrap().sysex_messages()
        );
    }

    #[test]
    #[ignore = "requires the user's local PRINCE installation; run from repository root"]
    fn local_prince_reaches_intro_through_ui_batches() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let legacy = root.join("PRINCE/PRINCE.EXE");
        let path = if legacy.is_file() {
            legacy
        } else {
            root.join("games/PRINCE/PRINCE.EXE")
        };
        let mut app = Doslet::from_cli(crate::cli::Cli {
            game: Some(path),
            no_audio: true,
            midi_output: None,
            list_midi_outputs: false,
            mt32_rom_dir: None,
            midi_mode: Some(crate::cli::MidiMode::Gm),
            midi_output_mode: Some(crate::cli::MidiMode::Gm),
            ..Default::default()
        })
        .unwrap();
        let start = Instant::now();
        app.tick_at(start);
        for _ in 0..750 {
            let before = app.session.steps();
            app.tick_at(start + Duration::from_secs(60));
            assert!(app.session.steps() - before <= u64::from(MAX_STEPS_PER_TICK));
            assert_eq!(
                app.session.status(),
                SessionStatus::Running,
                "{:?}",
                app.session.fault()
            );
        }
        assert_eq!(app.session.steps(), 300_000_000);
        assert!(app.midi.note_events() > 0);
        let frame = app.session.frame().expect("game selected a graphics mode");
        assert_eq!((frame.width, frame.height), (640, 480));
        let mut pixels = vec![0; frame.width * frame.height * 4];
        frame.write_rgba(&mut pixels).unwrap();
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[..3] != [0, 0, 0])
        );
        app.display.refresh(&app.session);
        assert!(app.display.image.is_some());
        let _ = app.update(Message::Reset);
        assert_eq!(app.session.status(), SessionStatus::Running);
        assert_eq!(app.session.steps(), 0);
        assert_eq!(app.midi.note_events(), 0);
        assert_eq!(app.name, "PRINCE.EXE");
    }
}
