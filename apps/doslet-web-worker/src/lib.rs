//! Single owner of the browser machine, imported storage and scanout conversion.
#[path = "../../doslet/src/archive.rs"]
mod archive;
use doslet_midi_synth::wire::{Decoder, REPLAY_CAPACITY, State};
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
    revision: Option<u64>,
    midi: [u8; 1024],
    pcm: [[i16; 2]; 128],
    wire: Decoder,
    wire_state: State,
    replay: [u8; REPLAY_CAPACITY],
}
impl Default for WorkerEngine {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl WorkerEngine {
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
            midi: [0; 1024],
            pcm: [[0; 2]; 128],
            wire: Decoder::new(),
            wire_state: State::new(),
            replay: [0; REPLAY_CAPACITY],
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
        self.wire = Decoder::new();
        self.wire_state = State::new();
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
        self.wire = Decoder::new();
        self.wire_state = State::new();
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
        let count = self.session.drain_midi(&mut self.midi);
        for &byte in &self.midi[..count] {
            if let Some(message) = self
                .wire
                .push(byte)
                .map_err(|e| format!("MIDI wire error: {e:?}"))?
            {
                self.wire_state.receive(message);
            }
        }
        Ok(count)
    }
    pub fn replay_ptr(&self) -> usize {
        self.replay.as_ptr() as usize
    }
    pub fn snapshot_midi(&mut self) -> usize {
        self.wire_state
            .write_with_decoder(&mut self.replay, &self.wire)
    }
    pub fn drain_pcm(&mut self) {
        while self.session.drain_pcm(&mut self.pcm) != 0 {}
    }
    pub fn width(&self) -> usize {
        self.session.frame().map_or(0, |f| f.width)
    }
    pub fn height(&self) -> usize {
        self.session.frame().map_or(0, |f| f.height)
    }
    pub fn frame_ptr(&self) -> usize {
        self.pixels.as_ptr() as usize
    }
    pub fn convert_frame(&mut self) -> usize {
        let Some(frame) = self.session.frame() else {
            return 0;
        };
        if self.revision == Some(frame.revision) {
            return 0;
        }
        self.pixels.resize(frame.width * frame.height * 4, 0);
        frame
            .write_rgba(&mut self.pixels)
            .expect("matching presentation buffer");
        self.revision = Some(frame.revision);
        self.pixels.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
