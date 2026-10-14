use crate::presentation::DisplayFrame;
use doslet_midi_synth::Timeline;
use doslet_midi_synth::wire::{Decoder, REPLAY_CAPACITY, State};
use doslet_runtime::{Session, SessionStatus};
use iced::{Element, Subscription, Task, Theme};
use std::time::{Duration, Instant};

const MAX_STEPS_PER_TICK: u32 = 400_000;
const CHUNK: u32 = 10_000;
pub struct Doslet {
    pub session: Session,
    pub display: DisplayFrame,
    pub name: String,
    pub notice: Option<String>,
    pub midi_choices: Vec<String>,
    pub midi_choice: String,
    host_midi: Option<crate::native_midi::Output>,
    wire: Decoder,
    wire_state: State,
    #[cfg(test)]
    pub archive: Option<crate::archive::GameArchive>,
    #[cfg(test)]
    pub program: Option<String>,
    #[cfg(test)]
    active_archive: Option<(crate::archive::GameArchive, String)>,
    theme: Theme,
    anchor: Option<(Instant, u64)>,
    midi: Timeline,
    samples: [[f32; 2]; 512],
    midi_bytes: [u8; 1024],
    pcm: [[i16; 2]; 128],
    #[cfg(not(target_arch = "wasm32"))]
    source: Option<std::path::PathBuf>,
    #[cfg(not(target_arch = "wasm32"))]
    audio: Option<crate::audio::Output>,
}
#[derive(Clone, Debug)]
pub enum Message {
    RunPause,
    Step,
    Reset,
    Tick,
    RefreshMidi,
    MidiSelected(String),
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
            name: "Color bars".into(),
            notice: None,
            midi_choices: vec!["Rust synth".into()],
            midi_choice: "Rust synth".into(),
            host_midi: None,
            wire: Decoder::new(),
            wire_state: State::new(),
            #[cfg(test)]
            archive: None,
            #[cfg(test)]
            program: None,
            #[cfg(test)]
            active_archive: None,
            theme: crate::ui::theme(),
            anchor: None,
            midi: Timeline::new(48000).unwrap(),
            samples: [[0.0; 2]; 512],
            midi_bytes: [0; 1024],
            pcm: [[0; 2]; 128],
            #[cfg(not(target_arch = "wasm32"))]
            source: None,
            #[cfg(not(target_arch = "wasm32"))]
            audio: None,
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn from_cli(cli: crate::cli::Cli) -> Result<Self, Box<dyn std::error::Error>> {
        let mut app = Self::new();
        if let Some(path) = cli.game {
            let mut session = crate::native_input::load_game(&path)
                .map_err(|error| format!("Cannot load {}: {error}", path.display()))?;
            session.start();
            app.session = session;
            app.name = path.file_name().unwrap().to_string_lossy().into_owned();
            app.source = Some(path);
            if !cli.no_audio && cli.midi_output.is_none() {
                match crate::audio::Output::new() {
                    Ok(audio) => {
                        app.midi = Timeline::new(audio.rate).unwrap();
                        app.audio = Some(audio);
                    }
                    Err(error) => app.notice = Some(format!("Audio unavailable: {error}")),
                }
            }
        }
        if let Some(selection) = cli.midi_output {
            let ports = crate::native_midi::ports()?;
            app.midi_choices.extend(ports.names.iter().cloned());
            let mut output = ports.connect(&selection)?;
            output.set_active(app.session.status() == SessionStatus::Running)?;
            app.midi_choice = output.name.clone();
            app.host_midi = Some(output);
        }
        Ok(app)
    }
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::RefreshMidi => match crate::native_midi::ports() {
                Ok(ports) => {
                    self.midi_choices = vec!["Rust synth".into()];
                    self.midi_choices.extend(ports.names);
                }
                Err(error) => self.notice = Some(error),
            },
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
                    let mut replay = [0; REPLAY_CAPACITY];
                    let count = self.wire_state.write_with_decoder(&mut replay, &self.wire);
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
        self.display.refresh(&self.session);
        #[cfg(not(target_arch = "wasm32"))]
        {
            Task::none()
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
                match crate::native_input::load_game(path) {
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
        self.midi = Timeline::new(self.midi.sample_rate()).unwrap();
        self.wire = Decoder::new();
        self.wire_state = State::new();
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
    }
    fn process_audio(&mut self) -> Result<(), String> {
        if self.host_midi.is_some() {
            self.midi
                .skip_until(self.session.emulated_nanos())
                .map_err(|e| e.to_string())?;
        } else {
            loop {
                let count = self
                    .midi
                    .render_until(self.session.emulated_nanos(), &mut self.samples)
                    .map_err(|e| e.to_string())?;
                if count == 0 {
                    break;
                }
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(audio) = &self.audio {
                    audio.push(&self.samples[..count])?;
                }
            }
        }
        let count = self.session.drain_midi(&mut self.midi_bytes);
        for byte in &self.midi_bytes[..count] {
            if let Some(message) = self
                .wire
                .push(*byte)
                .map_err(|e| format!("MIDI wire error: {e:?}"))?
            {
                self.wire_state.receive(message);
            }
            if self.host_midi.is_none() {
                self.midi.push_byte(*byte).map_err(|e| e.to_string())?;
            }
        }
        if let Some(output) = &mut self.host_midi {
            output.push(self.session.emulated_nanos(), &self.midi_bytes[..count])?;
        }
        while self.session.drain_pcm(&mut self.pcm) != 0 {}
        Ok(())
    }
    pub fn audio_label(&self) -> String {
        if let Some(output) = &self.host_midi {
            return output.label();
        }
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(audio) = &self.audio {
            let (frames, _, peak) = audio.stats();
            return format!(
                "MIDI · {} Hz · {} notes · peak {:.2} · {} played frames",
                audio.rate,
                self.midi.note_events(),
                peak,
                frames
            );
        }
        "Audio output inactive".into()
    }
    fn select_midi(&mut self, selection: &str) -> Result<(), String> {
        if selection == self.midi_choice
            && self
                .host_midi
                .as_ref()
                .is_none_or(|output| output.check().is_ok())
        {
            return Ok(());
        }
        let mut replay = [0; REPLAY_CAPACITY];
        let count = self.wire_state.write_with_decoder(&mut replay, &self.wire);
        if selection == "Rust synth" {
            let audio = crate::audio::Output::new()?;
            let mut midi = Timeline::new(audio.rate).map_err(|e| e.to_string())?;
            midi.skip_until(self.session.emulated_nanos())
                .map_err(|e| e.to_string())?;
            for &byte in &replay[..count] {
                midi.push_byte(byte).map_err(|e| e.to_string())?;
            }
            audio.set_active(self.session.status() == SessionStatus::Running);
            self.host_midi = None;
            self.audio = Some(audio);
            self.midi = midi;
        } else {
            let mut output = crate::native_midi::ports()?.connect(selection)?;
            output.clear()?;
            output.set_active(self.session.status() == SessionStatus::Running)?;
            output.push(self.session.emulated_nanos(), &replay[..count])?;
            if let Some(audio) = &self.audio {
                audio.set_active(false);
            }
            self.audio = None;
            self.host_midi = Some(output);
        }
        self.midi_choice = selection.into();
        self.anchor = None;
        self.notice = None;
        Ok(())
    }
    pub fn view(&self) -> Element<'_, Message> {
        crate::ui::view(self)
    }
    pub fn theme(&self) -> Theme {
        self.theme.clone()
    }
    pub fn subscription(&self) -> Subscription<Message> {
        if self.session.status() == SessionStatus::Running {
            iced::time::every(Duration::from_millis(16)).map(|_| Message::Tick)
        } else {
            Subscription::none()
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

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
