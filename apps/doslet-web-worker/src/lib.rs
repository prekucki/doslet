//! Single owner of the browser machine, imported storage and scanout conversion.
#[path = "../../doslet/src/archive.rs"]
mod archive;
use doslet_midi_routing::{Mode, Routing, SNAPSHOT_CAPACITY};
use doslet_runtime::{Session, SessionStatus};
use doslet_x86::Gpr16;
use std::sync::Arc;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct WorkerEngine {
    session: Session,
    archive: Option<archive::GameArchive>,
    active: Option<(archive::GameArchive, String)>,
    pixels: Vec<u8>,
    revision: Option<(usize, usize, u64)>,
    midi: [u8; 16384],
    source_midi: [u8; 64],
    output_mode: Mode,
    pcm: [doslet_pcm::Frame; 128],
    pcm_wire: [f64; 128 * 4],
    routing: Routing,
    replay: Box<[u8]>,
}
impl Default for WorkerEngine {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl WorkerEngine {
    pub fn pointer_relative(&mut self, dx: f64, dy: f64) {
        if self.session.status() == SessionStatus::Running {
            self.session.pointer_relative(dx, dy);
        }
    }
    pub fn pointer_event(&mut self, x: u16, y: u16, button: i8, pressed: bool) {
        if button == -2 {
            self.session.release_pointer_buttons();
        } else if button >= 0 {
            self.session.pointer_button(button as u8, pressed);
        } else {
            self.session.pointer_move(x, y);
        }
    }
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new() -> Self {
        #[cfg(target_arch = "wasm32")]
        console_error_panic_hook::set_once();
        Self {
            session: Session::demo(),
            archive: None,
            active: None,
            pixels: Vec::new(),
            revision: None,
            midi: [0; 16384],
            source_midi: [0; 64],
            output_mode: Mode::Gm,
            pcm: [doslet_pcm::EMPTY; 128],
            pcm_wire: [0.0; 128 * 4],
            routing: Routing::new(Mode::Gm),
            replay: vec![0; SNAPSHOT_CAPACITY].into_boxed_slice(),
        }
    }
    pub fn import_zip(&mut self, bytes: Vec<u8>) -> Result<String, String> {
        let archive = archive::GameArchive::new(Arc::new(bytes))?;
        let programs = archive.programs.join("\n");
        self.archive = Some(archive);
        Ok(programs)
    }
    pub fn start_game(&mut self, program: &str) -> Result<(), String> {
        let archive = self.archive.as_ref().ok_or("Open a game ZIP first")?;
        let mut session = archive.load(program)?;
        session.start();
        self.active = Some((archive.clone(), program.into()));
        self.session = session;
        self.routing = Routing::new(self.routing.mode());
        self.revision = None;
        Ok(())
    }
    pub fn reset(&mut self) -> Result<(), String> {
        self.session = if let Some((archive, program)) = &self.active {
            let mut session = archive.load(program)?;
            session.start();
            session
        } else {
            Session::demo()
        };
        self.routing = Routing::new(self.routing.mode());
        self.revision = None;
        Ok(())
    }
    pub fn start(&mut self) {
        self.session.start();
    }
    pub fn pause(&mut self) {
        self.session.pause();
    }
    pub fn single_step(&mut self) {
        self.session.single_step();
    }
    pub fn advance(&mut self, budget: u32) -> u32 {
        self.session.advance(budget.min(10000))
    }
    pub fn key_event(
        &mut self,
        scan: u8,
        ascii: u8,
        modifiers: u8,
        pressed: bool,
    ) -> Result<bool, String> {
        self.session
            .set_key_modifiers(modifiers)
            .map_err(|e| e.to_string())?;
        if pressed {
            self.session
                .push_key(scan, ascii)
                .map_err(|e| e.to_string())
        } else {
            Ok(true)
        }
    }
    pub fn status(&self) -> u32 {
        match self.session.status() {
            SessionStatus::Ready => 0,
            SessionStatus::Running => 1,
            SessionStatus::Paused => 2,
            SessionStatus::Exited(code) => 256 + u32::from(code),
            SessionStatus::Halted => 3,
            SessionStatus::Faulted => 4,
        }
    }
    pub fn fault(&self) -> Option<String> {
        self.session.fault().map(|v| v.to_string())
    }
    pub fn steps(&self) -> f64 {
        self.session.steps() as f64
    }
    pub fn nanos(&self) -> f64 {
        self.session.emulated_nanos() as f64
    }
    pub fn cs(&self) -> u16 {
        self.session.registers().cs
    }
    pub fn ip(&self) -> u32 {
        self.session.registers().ip
    }
    pub fn ax(&self) -> u16 {
        self.session.registers().get(Gpr16::Ax)
    }
    pub fn cx(&self) -> u16 {
        self.session.registers().get(Gpr16::Cx)
    }
    pub fn di(&self) -> u16 {
        self.session.registers().get(Gpr16::Di)
    }
    pub fn midi_ptr(&self) -> usize {
        self.midi.as_ptr() as usize
    }
    pub fn drain_midi(&mut self) -> Result<usize, String> {
        let count = self.session.drain_midi(&mut self.source_midi);
        let mut used = 0;
        let mut overflow = false;
        for &byte in &self.source_midi[..count] {
            self.routing
                .push(byte, self.output_mode, &mut |message| {
                    if used + message.len() > self.midi.len() {
                        overflow = true;
                        return;
                    }
                    self.midi[used..used + message.len()].copy_from_slice(message);
                    used += message.len();
                })
                .map_err(|e| format!("MIDI wire error: {e:?}"))?;
        }
        if overflow {
            return Err("Translated MIDI batch capacity exceeded".into());
        }
        Ok(used)
    }
    pub fn replay_ptr(&self) -> usize {
        self.replay.as_ptr() as usize
    }
    pub fn snapshot_midi(&mut self) -> usize {
        let mut count = 0;
        self.routing.replay(self.output_mode, &mut |message| {
            self.replay[count..count + message.len()].copy_from_slice(message);
            count += message.len();
        });
        count
    }
    pub fn set_midi_mode(&mut self, mode: &str) -> Result<(), String> {
        let mode = parse_mode(mode)?;
        if mode == Mode::Gm && self.output_mode == Mode::Mt32 {
            return Err("Select a GM output before GM game mode".into());
        }
        self.routing.set_mode(mode);
        Ok(())
    }
    pub fn set_output_mode(&mut self, mode: &str) -> Result<(), String> {
        let mode = parse_mode(mode)?;
        if mode == Mode::Mt32 && self.routing.mode() == Mode::Gm {
            return Err("Select MT-32 game mode before an MT-32 host port".into());
        }
        self.output_mode = mode;
        Ok(())
    }
    pub fn midi_mode(&self) -> String {
        self.routing.mode().as_str().into()
    }
    pub fn midi_upgraded(&self) -> bool {
        self.routing.upgraded()
    }
    pub fn midi_sysex(&self) -> f64 {
        self.routing.translator().stats().sysex as f64
    }
    pub fn midi_unmapped(&self) -> f64 {
        self.routing.translator().stats().unmapped_notes as f64
    }
    pub fn midi_display(&self) -> String {
        String::from_utf8_lossy(self.routing.translator().display()).into_owned()
    }
    pub fn pcm_ptr(&self) -> usize {
        self.pcm_wire.as_ptr() as usize
    }
    pub fn drain_pcm(&mut self) -> usize {
        let count = self.session.drain_pcm_timed(&mut self.pcm);
        for (frame, wire) in self.pcm[..count]
            .iter()
            .zip(self.pcm_wire.chunks_exact_mut(4))
        {
            wire.copy_from_slice(&[
                frame.nanos as f64,
                f64::from(frame.duration),
                f64::from(frame.samples[0]),
                f64::from(frame.samples[1]),
            ]);
        }
        count
    }
    pub fn width(&self) -> usize {
        self.session.presentation_frame().width()
    }
    pub fn height(&self) -> usize {
        self.session.presentation_frame().height()
    }
    pub fn frame_ptr(&self) -> usize {
        self.pixels.as_ptr() as usize
    }
    pub fn convert_frame(&mut self) -> usize {
        let frame = self.session.presentation_frame();
        let revision = (frame.width(), frame.height(), frame.revision());
        if self.revision == Some(revision) {
            return 0;
        }
        self.pixels.resize(frame.width() * frame.height() * 4, 0);
        frame
            .write_rgba(&mut self.pixels)
            .expect("matching presentation buffer");
        self.revision = Some(revision);
        self.pixels.len()
    }
}

fn parse_mode(mode: &str) -> Result<Mode, String> {
    match mode {
        "gm" => Ok(Mode::Gm),
        "mt32" => Ok(Mode::Mt32),
        _ => Err("MIDI mode must be gm or mt32".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gm_to_raw_mt32_port_is_rejected_without_changing_dialects() {
        let mut engine = WorkerEngine::new();
        assert!(engine.set_output_mode("mt32").is_err());
        assert_eq!(engine.output_mode, Mode::Gm);
        engine.set_midi_mode("mt32").unwrap();
        engine.set_output_mode("mt32").unwrap();
        assert!(engine.set_midi_mode("gm").is_err());
        assert_eq!(engine.midi_mode(), "mt32");
        engine.set_output_mode("gm").unwrap();
        engine.set_midi_mode("gm").unwrap();
    }
    #[test]
    fn worker_upgrades_roland_before_worklet_and_replays_raw_sysex_to_mt32_port() {
        let mut engine = WorkerEngine::new();
        let message = [
            0xF0, 0x41, 0x10, 0x16, 0x12, 0x20, 0, 0, b'A', 0x1F, 0xF7, 0x91, 60, 100,
        ];
        let mut program = vec![0xBA, 0x31, 3, 0xB0, 0x3F, 0xEE, 0xBA, 0x30, 3];
        for byte in message {
            program.extend_from_slice(&[0xB0, byte, 0xEE]);
        }
        program.extend_from_slice(&[0xEB, 0xFE]);
        engine.session = Session::new(&program).unwrap();
        engine.start();
        engine.advance(1000);
        let count = engine.drain_midi().unwrap();
        assert_eq!(engine.midi_mode(), "mt32");
        assert!(engine.midi_upgraded());
        assert_eq!(engine.midi_sysex(), 1.0);
        assert!(!engine.midi[..count].contains(&0xF0));
        assert!(
            engine.midi[..count]
                .windows(3)
                .any(|m| m == [0x90, 60, 100])
        );
        engine.set_output_mode("mt32").unwrap();
        let count = engine.snapshot_midi();
        assert!(
            engine.replay[..count]
                .windows(11)
                .any(|m| m == &message[..11])
        );
        assert!(
            engine.replay[..count]
                .windows(3)
                .any(|m| m == [0x91, 60, 100])
        );
    }
    #[test]
    fn worker_owns_demo_and_bounds_each_execution_chunk() {
        let mut engine = WorkerEngine::new();
        engine.start();
        assert!(engine.advance(u32::MAX) <= 10000);
        engine.pause();
        let steps = engine.steps();
        assert_eq!(engine.advance(10000), 0);
        engine.single_step();
        assert!(engine.steps() > steps);
        engine.reset().unwrap();
        assert_eq!(engine.steps(), 0.0);
        assert_eq!(engine.status(), 0);
    }
    #[test]
    fn reset_uses_the_running_zip_after_importing_another_game_or_bad_zip() {
        use zip::CompressionMethod;
        let first = archive::tests::pack(
            &[("FIRST.COM", &[0xB8, 0, 0x4C, 0xCD, 0x21])],
            CompressionMethod::Stored,
        );
        let second = archive::tests::pack(
            &[("SECOND.COM", &[0xB8, 7, 0x4C, 0xCD, 0x21])],
            CompressionMethod::Deflated,
        );
        let mut engine = WorkerEngine::new();
        engine.import_zip(first.as_ref().clone()).unwrap();
        engine.start_game("FIRST.COM").unwrap();
        engine.advance(100);
        assert_eq!(engine.status(), 256);
        engine.import_zip(second.as_ref().clone()).unwrap();
        assert!(engine.import_zip(b"bad zip".to_vec()).is_err());
        engine.reset().unwrap();
        assert_eq!(engine.steps(), 0.0);
        engine.advance(100);
        assert_eq!(engine.status(), 256);
        engine.start_game("SECOND.COM").unwrap();
        engine.advance(100);
        assert_eq!(engine.status(), 263);
    }
}
