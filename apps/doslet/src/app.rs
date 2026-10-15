use crate::presentation::{DisplayFrame, DisplaySizing, SizingMessage};
use doslet_midi_routing::{Mode, Routing, SNAPSHOT_CAPACITY};
use doslet_midi_synth::Timeline;
use doslet_runtime::{RuntimeError, Session, SessionStatus};
use iced::{Element, Subscription, Task, Theme};
use std::time::{Duration, Instant};

const MAX_STEPS_PER_TICK: u32 = 400_000;
const CHUNK: u32 = 10_000;
pub struct Doslet {
    pub session: Session,
    pub display: DisplayFrame,
    pub display_sizing: DisplaySizing,
    pub name: String,
    pub notice: Option<String>,
    logged_fault: Option<RuntimeError>,
    logged_notice: Option<String>,
    pub midi_choices: Vec<String>,
    pub midi_choice: String,
    host_midi: Option<crate::native_midi::Output>,
    routing: Routing,
    pub midi_mode: Mode,
    pub midi_output_mode: Mode,
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
    DisplaySizing(SizingMessage),
    PointerMoved(u16, u16),
    PointerButton(u8, bool),
    PointerLeft,
    CaptureWindow(iced::window::Id, u64),
    MouseCapture(iced::window::Id, u64, iced::window::MouseCaptureEvent),
    Keyboard(iced::keyboard::Event),
    RunPause,
    Step,
    Reset,
    Tick,
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
            display: DisplayFrame::default(),
            display_sizing: DisplaySizing::default(),
            name: "Color bars".into(),
            notice: None,
            logged_fault: None,
            logged_notice: None,
            midi_choices: vec!["Rust synth".into()],
            midi_choice: "Rust synth".into(),
            host_midi: None,
            routing: Routing::new(Mode::Gm),
            midi_mode: Mode::Gm,
            midi_output_mode: Mode::Gm,
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
    pub fn from_cli(cli: crate::cli::Cli) -> Result<Self, Box<dyn std::error::Error>> {
        let cli = cli.resolve()?;
        let mut app = Self::new();
        app.midi_mode = cli.midi_mode.unwrap_or_default().into();
        app.midi_output_mode = cli.midi_output_mode.unwrap_or_default().into();
        app.launch = cli.launch;
        app.routing.set_mode(app.midi_mode);
        if let Some(path) = cli.game {
            let mut session = crate::native_input::load_game_with_settings(&path, &app.launch)
                .map_err(|error| format!("Cannot load {}: {error}", path.display()))?;
            session.start();
            app.session = session;
            app.name = cli
                .profile_name
                .unwrap_or_else(|| path.file_name().unwrap().to_string_lossy().into_owned());
            app.source = Some(path);
            if !cli.no_audio && cli.midi_output.is_none() {
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
                {
                    self.session.pointer_move(x, y);
                }
            }
            Message::PointerButton(button, pressed) => {
                if pressed
                    && self.session.status() == SessionStatus::Running
                    && self.mouse_capture.is_none()
                    && !self.mouse_capture_pending
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
                if self.session.status() == SessionStatus::Running || !pressed {
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
                        }
                    }
                    MouseCaptureEvent::Failed(error) => {
                        self.notice = Some(format!("Mouse capture unavailable: {error}"));
                    }
                }
            }
            Message::Keyboard(event) => {
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
                if self.session.status() == SessionStatus::Running {
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
            Message::RefreshMidi => match crate::native_midi::ports() {
                Ok(ports) => {
                    self.midi_choices = vec!["Rust synth".into()];
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
                self.session.single_step();
                if let Err(error) = self.process_audio() {
                    self.notice = Some(error);
                }
            }
            Message::Reset => self.reset(),
            Message::Tick => self.tick(),
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
    fn release_mouse(&mut self) -> Task<Message> {
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
        self.notice = None;
        #[cfg(test)]
        if let Some((archive, program)) = &self.active_archive {
            match archive.load(program) {
                Ok(mut session) => {
                    session.start();
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
                        session.start();
                        self.session = session;
                    }
                    Err(error) => {
                        self.notice = Some(format!("Cannot reload game: {error}"));
                        return;
                    }
                }
            } else {
                self.session = Session::demo();
            }
        }
        self.clear_presentation();
    }
    fn clear_presentation(&mut self) {
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
        self.midi = Timeline::new(rate).unwrap();
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
        if let Some(output) = &mut self.host_midi
            && let Err(error) = output.clear()
        {
            self.notice = Some(error);
        }
        self.display = DisplayFrame::default();
        self.anchor = None;
    }
    fn tick(&mut self) {
        self.tick_at(Instant::now());
    }
    fn tick_at(&mut self, now: Instant) {
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
            let completed = self.session.advance(budget.min(CHUNK));
            budget -= completed;
            if let Err(error) = self.process_audio() {
                self.notice = Some(error);
                self.session.pause();
                break;
            }
            if completed == 0 {
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
                mt32.push_byte(byte).map_err(|e| e.to_string())?;
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
                "Audio · {} Hz · {} MIDI notes · peak {:.2} · {} played frames · SB {} nonzero frames",
                audio.rate,
                self.midi.note_events(),
                peak,
                frames,
                self.pcm_mixer.nonzero()
            );
        }
        "Audio output inactive".into()
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
    fn reselect_midi(&mut self, selection: &str) -> Result<(), String> {
        if selection == "CM-32L / MT-32" || selection == "Rust synth" {
            let audio = crate::audio::Output::new()?;
            let mut replay_error = None;
            if selection == "CM-32L / MT-32" {
                let directory = self
                    .mt32_rom_dir
                    .as_ref()
                    .ok_or("Use --mt32-rom-dir to load CM-32L ROMs")?;
                let mut engine = Self::load_mt32(directory, audio.rate, self.midi_mode)
                    .map_err(|e| e.to_string())?;
                engine.rebase(self.session.emulated_nanos());
                self.routing.replay(self.midi_mode, &mut |message| {
                    for &byte in message {
                        if let Err(error) = engine.push_byte(byte) {
                            replay_error = Some(error.to_string());
                        }
                    }
                });
                if let Some(error) = replay_error {
                    return Err(error);
                }
                self.mt32 = Some(engine);
            } else {
                let mut engine = Timeline::new(audio.rate).map_err(|e| e.to_string())?;
                engine
                    .skip_until(self.session.emulated_nanos())
                    .map_err(|e| e.to_string())?;
                self.routing.replay(Mode::Gm, &mut |message| {
                    for &byte in message {
                        if let Err(error) = engine.push_byte(byte) {
                            replay_error = Some(error.to_string());
                        }
                    }
                });
                if let Some(error) = replay_error {
                    return Err(error);
                }
                self.midi = engine;
                self.mt32 = None;
            }
            audio.set_active(self.session.status() == SessionStatus::Running);
            if let Some(output) = &mut self.host_midi {
                output.clear()?;
            }
            self.host_midi = None;
            self.pcm_mixer
                .reset(audio.rate, self.session.emulated_nanos())
                .map_err(|e| e.to_string())?;
            self.audio = Some(audio);
        } else {
            if self.midi_mode == Mode::Gm && self.midi_output_mode == Mode::Mt32 {
                return Err("GM to an MT-32 host port is unsupported; select MT-32 game mode. GM on moont uses GmDevice.".into());
            }
            let ports = crate::native_midi::ports()?;
            self.midi_choices = vec!["Rust synth".into()];
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
            "Rust synth" => self.audio.is_some() && self.mt32.is_none(),
            "CM-32L / MT-32" => self.audio.is_some() && self.mt32.is_some(),
            _ => self.host_midi.as_ref().is_some_and(|o| o.check().is_ok()),
        };
        if selection == self.midi_choice && ready {
            return Ok(());
        }
        self.reselect_midi(selection)
    }
    pub fn midi_mode_label(&self) -> String {
        let route = if self.midi_mode == Mode::Mt32 && self.output_dialect() == Mode::Gm {
            "MT-32 → GM (przybliżenie)"
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
    pub fn hide_host_cursor(&self) -> bool {
        self.mouse_capture.is_some() && !self.mouse_capture_pending
    }
    pub fn subscription(&self) -> Subscription<Message> {
        let tick = if self.session.status() == SessionStatus::Running {
            iced::time::every(Duration::from_millis(16)).map(|_| Message::Tick)
        } else {
            Subscription::none()
        };
        Subscription::batch([
            tick,
            DisplaySizing::subscription().map(Message::DisplaySizing),
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    include!("../../../tests/fixtures/sb16_pcm.rs");
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
                .chunks_exact(2)
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
        assert!(app.midi_mode_label().contains("przybliżenie"));
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
    fn local_prince_reaches_intro_through_ui_batches() {
        let path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../PRINCE/PRINCE.EXE");
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
        assert!(pixels.chunks_exact(4).any(|pixel| pixel[..3] != [0, 0, 0]));
        app.display.refresh(&app.session);
        assert!(app.display.image.is_some());
        let _ = app.update(Message::Reset);
        assert_eq!(app.session.status(), SessionStatus::Running);
        assert_eq!(app.session.steps(), 0);
        assert_eq!(app.midi.note_events(), 0);
        assert_eq!(app.name, "PRINCE.EXE");
    }
}
