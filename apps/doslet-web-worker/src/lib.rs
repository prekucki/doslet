//! Single owner of the browser machine, imported storage and scanout conversion.
#[path = "../../doslet/src/archive.rs"]
mod archive;
#[path = "../../common/machine_profile.rs"]
#[allow(
    dead_code,
    reason = "Worker parses profiles; export lives in the window frontend"
)]
mod machine_profile;
use doslet_config::{MachineSpec, SourceKind, WritePolicy};
use doslet_midi_routing::{Mode, Routing, SNAPSHOT_CAPACITY};
use doslet_runtime::{Session, SessionStatus};
use doslet_storage::{
    CachedSource, HostCompletion, HostRequest, RequestTag, SourceId, SourceIdentity, StorageError,
};
use doslet_x86::Gpr16;
use std::sync::Arc;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct WorkerEngine {
    session: Session,
    archive: Option<archive::GameArchive>,
    active: Option<(archive::GameArchive, String)>,
    active_spec: MachineSpec,
    candidate: Option<Session>,
    candidate_spec: Option<MachineSpec>,
    candidate_archive: Option<(archive::GameArchive, String)>,
    request: Option<HostRequest>,
    machine_epoch: u64,
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
            active_spec: MachineSpec::default(),
            candidate: None,
            candidate_spec: None,
            candidate_archive: None,
            request: None,
            machine_epoch: 1,
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
        self.guard_direct_replacement()?;
        let archive = self.archive.as_ref().ok_or("Open a game ZIP first")?;
        let mut session = archive.load(program)?;
        session.start();
        self.active = Some((archive.clone(), program.into()));
        self.active_spec = MachineSpec::default();
        self.machine_epoch += 1;
        self.session = session;
        self.routing = Routing::new(self.routing.mode());
        self.revision = None;
        Ok(())
    }
    /// Build a candidate without touching the active session. Host preparation
    /// (media probe / writer lease) must finish before install_machine.
    pub fn prepare_machine(&mut self, profile: &str, program: &str) -> Result<String, String> {
        let spec = machine_profile::parse_machine_profile(profile)?;
        self.prepare(spec, program, false)
    }
    pub fn prepare_reset(&mut self) -> Result<String, String> {
        let program = self
            .active
            .as_ref()
            .map_or(String::new(), |(_, p)| p.clone());
        self.prepare(self.active_spec.clone(), &program, true)
    }
    pub fn prepare_start_game(&mut self, program: &str) -> Result<String, String> {
        self.prepare(self.active_spec.clone(), program, false)
    }
    pub fn candidate_source(&mut self, length: u64, validator: &[u8]) -> Result<(), String> {
        let spec = self
            .candidate_spec
            .as_ref()
            .ok_or("No prepared configuration")?;
        let letter = spec
            .mounts
            .iter()
            .find(|m| matches!(m.kind, SourceKind::LocalIso | SourceKind::HttpsIso))
            .ok_or("No ISO mount in configuration")?
            .letter;
        let validator: [u8; 32] = validator
            .try_into()
            .map_err(|_| "Invalid source fingerprint")?;
        let session = self.candidate.take().ok_or("No prepared machine")?;
        self.candidate = Some(
            session
                .with_cdrom_source(
                    letter,
                    CachedSource::new(
                        SourceIdentity {
                            id: SourceId(1),
                            generation: self.machine_epoch + 1,
                            length,
                            validator,
                        },
                        self.machine_epoch + 1,
                    ),
                )
                .map_err(|e| e.to_string())?,
        );
        Ok(())
    }
    pub fn candidate_store(
        &mut self,
        namespace: &[u8],
        fence: u64,
        revision: u64,
        payload: &[u8],
    ) -> Result<(), String> {
        let namespace = namespace.try_into().map_err(|_| "Invalid save namespace")?;
        let session = self.candidate.as_mut().ok_or("No prepared machine")?;
        session
            .attach_persistent_store(
                namespace,
                session.base_identity(),
                fence,
                revision,
                if payload.is_empty() {
                    None
                } else {
                    Some(payload)
                },
            )
            .map_err(|e| format!("Save attachment: {e:?}"))
    }
    pub fn install_machine(&mut self) -> Result<(), String> {
        let mut candidate = self.candidate.take().ok_or("No prepared machine")?;
        let spec = self
            .candidate_spec
            .take()
            .ok_or("No prepared configuration")?;
        candidate.start();
        self.session = candidate;
        self.active = self.candidate_archive.take();
        self.active_spec = spec;
        self.machine_epoch = self.machine_epoch.wrapping_add(1);
        self.request = None;
        self.routing = Routing::new(parse_mode(&self.active_spec.output.game_midi)?);
        self.revision = None;
        Ok(())
    }
    pub fn discard_machine(&mut self) {
        self.candidate = None;
        self.candidate_spec = None;
        self.candidate_archive = None;
    }
    pub fn waiting_for_io(&self) -> bool {
        self.session.storage_waiting()
    }
    pub fn pending_commit(&self) -> bool {
        self.session.pending_commit()
    }
    pub fn dirty_overlay(&self) -> bool {
        self.session.dirty_overlay()
    }
    pub fn request_save_commit(&mut self) -> Result<(), String> {
        self.session
            .request_overlay_commit()
            .map_err(|e| e.to_string())
    }
    pub fn candidate_import(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.candidate
            .as_mut()
            .ok_or("No prepared machine")?
            .import_overlay(bytes)
            .map_err(|e| format!("Import: {e:?}"))
    }
    pub fn export_saves(&mut self) -> Result<Vec<u8>, String> {
        self.session
            .export_overlay()
            .map_err(|e| format!("Save export: {e:?}"))
    }
    pub fn import_saves(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.session
            .import_overlay(bytes)
            .map_err(|e| format!("Save import: {e:?}"))
    }
    pub fn physical_key(&mut self, usage: u16, pressed: bool) {
        let _ = self.session.physical_key(usage, pressed);
    }
    pub fn joystick(&mut self, x: i16, y: i16, buttons: u8) {
        self.session
            .set_joystick([x, y], [buttons & 1 != 0, buttons & 2 != 0]);
    }
    pub fn release_input(&mut self) {
        let _ = self.session.release_keys();
        self.session.release_pointer_buttons();
    }
    pub fn release_joystick(&mut self) {
        let _ = self
            .session
            .release_input_source(doslet_input::InputSourceId(1));
    }
    /// Metadata is bounded JSON; byte payloads use their own explicit transfer.
    pub fn next_io_request(&mut self) -> Option<String> {
        if self.request.is_some() {
            return None;
        }
        let request = self.session.next_host_request()?;
        let tag = request.tag();
        let mut metadata = serde_json::json!({"machine":self.machine_epoch.to_string(),"epoch":tag.epoch.to_string(),"id":tag.id.to_string(),"generation":tag.generation.to_string(),"slot":tag.slot});
        match &request {
            HostRequest::Read {
                source,
                offset,
                length,
                ..
            } => {
                metadata["kind"] = "read".into();
                metadata["source"] = source.0.to_string().into();
                metadata["offset"] = offset.to_string().into();
                metadata["length"] = (*length).into();
            }
            HostRequest::Commit { batch, .. } => {
                metadata["kind"] = "commit".into();
                metadata["namespace"] = hex(&batch.namespace).into();
                metadata["base"] = hex(&batch.base).into();
                metadata["fence"] = batch.fencing_token.to_string().into();
                metadata["expected"] = batch.expected_revision.to_string().into();
                metadata["mutation"] = batch.mutation_id.to_string().into();
                metadata["digest"] = batch.digest.to_string().into();
            }
        }
        self.request = Some(request);
        Some(metadata.to_string())
    }
    pub fn take_io_payload(&mut self) -> Vec<u8> {
        let request = self.request.take();
        match request {
            Some(HostRequest::Commit { batch, .. }) => batch.payload,
            _ => Vec::new(),
        }
    }
    pub fn complete_io(
        &mut self,
        metadata: &str,
        bytes: Vec<u8>,
        error: &str,
        revision: u64,
    ) -> Result<(), String> {
        let value: serde_json::Value = serde_json::from_str(metadata).map_err(|e| e.to_string())?;
        let number = |key: &str| -> Result<u64, String> {
            value[key]
                .as_str()
                .ok_or_else(|| format!("Missing {key}"))?
                .parse()
                .map_err(|_| format!("Invalid {key}"))
        };
        if number("machine")? != self.machine_epoch {
            return Ok(());
        }
        let tag = RequestTag {
            epoch: number("epoch")?,
            id: number("id")?,
            generation: number("generation")?,
            slot: value["slot"]
                .as_u64()
                .ok_or("Missing slot")?
                .try_into()
                .map_err(|_| "Invalid slot")?,
        };
        let result = if error.is_empty() {
            Ok(())
        } else {
            Err(storage_error(error))
        };
        let completion = if value["kind"] == "commit" {
            HostCompletion::Commit {
                tag,
                result: result.map(|()| revision),
            }
        } else {
            HostCompletion::Read { tag, bytes, result }
        };
        match self.session.complete_host_request(completion) {
            Ok(()) | Err(StorageError::StaleCompletion) => Ok(()),
            Err(e) => Err(format!("Storage completion: {e:?}")),
        }
    }
    pub fn reset(&mut self) -> Result<(), String> {
        self.guard_direct_replacement()?;
        self.session = if let Some((archive, program)) = &self.active {
            let mut session = archive.load(program)?;
            session.start();
            session
        } else {
            Session::demo()
        };
        self.routing = Routing::new(self.routing.mode());
        self.machine_epoch += 1;
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
    pub fn timeslice_yielded(&self) -> bool {
        self.session.timeslice_yielded()
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
    pub fn pointer_in_use(&self) -> bool {
        self.session.pointer_in_use()
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
    pub fn guest_pcm_rate(&self) -> u32 {
        self.session.audio_status().rate
    }
    pub fn guest_pcm_bits(&self) -> u8 {
        self.session.audio_status().configured_format.map_or(
            0,
            |format| {
                if format.word { 16 } else { 8 }
            },
        )
    }
    pub fn guest_pcm_channels(&self) -> u8 {
        self.session.audio_status().configured_format.map_or(
            0,
            |format| {
                if format.stereo { 2 } else { 1 }
            },
        )
    }
    pub fn midi_upgraded(&self) -> bool {
        self.routing.upgraded()
    }
    pub fn midi_sysex(&self) -> f64 {
        self.routing.translator().stats().sysex as f64
    }
    pub fn midi_bytes(&self) -> f64 {
        self.session.midi_stats().1 as f64
    }
    pub fn midi_notes(&self) -> f64 {
        self.routing.input_note_events() as f64
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
            .zip(self.pcm_wire.as_chunks_mut::<4>().0.iter_mut())
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

impl WorkerEngine {
    fn guard_direct_replacement(&self) -> Result<(), String> {
        if self.session.pending_commit() || self.session.dirty_overlay() {
            return Err("Settle or export changes before replacing this machine".into());
        }
        if self.active_spec.mounts.iter().any(|m| {
            m.policy == WritePolicy::Persistent
                || matches!(m.kind, SourceKind::LocalIso | SourceKind::HttpsIso)
        }) {
            return Err("This machine requires host preparation before replacement".into());
        }
        Ok(())
    }
    fn prepare(&mut self, spec: MachineSpec, program: &str, reset: bool) -> Result<String, String> {
        if self.session.pending_commit() {
            return Err("Wait for the current save commit before replacing this session".into());
        }
        spec.resolve().map_err(|e| e.to_string())?;
        let archive = if reset {
            self.active.as_ref().map(|(a, _)| a)
        } else {
            self.archive.as_ref()
        };
        let mut session = if program.is_empty() {
            Session::demo_configured(spec.hardware).map_err(|e| e.to_string())?
        } else {
            archive
                .ok_or("Open a game ZIP first")?
                .load_configured(program, &spec)?
        };
        session
            .set_epoch(self.machine_epoch + 1)
            .map_err(|e| e.to_string())?;
        session
            .configure_launch(
                &spec.resolve().map_err(|e| e.to_string())?.command_tail,
                spec.game.working_directory.as_bytes(),
            )
            .map_err(|e| e.to_string())?;
        let cd = spec
            .mounts
            .iter()
            .find(|m| matches!(m.kind, SourceKind::LocalIso | SourceKind::HttpsIso));
        let persistent = spec
            .mounts
            .iter()
            .find(|m| m.policy == WritePolicy::Persistent);
        let metadata = serde_json::json!({
            "base":hex(&session.base_identity()),
            "iso":cd.map(|m|serde_json::json!({"kind":if m.kind==SourceKind::HttpsIso {"https"}else{"blob"},"location":spec.binding(&m.source),"letter":m.letter})),
            "store":persistent.map(|m|spec.binding(m.store.as_deref().unwrap_or(""))),
            "input":{"pad_id":spec.input.pad_id,"pad_index":spec.input.pad_index,"axis_x":spec.input.axis_x,"axis_y":spec.input.axis_y,"button_1":spec.input.button_1,"button_2":spec.input.button_2,"deadzone_percent":spec.input.deadzone_percent},
            "output":{"audio":spec.output.audio,"midi_output":spec.output.midi_output,"game_midi":spec.output.game_midi,"host_midi":spec.output.host_midi},
        });
        self.candidate_archive = if program.is_empty() {
            None
        } else {
            Some((archive.ok_or("Missing archive")?.clone(), program.into()))
        };
        self.candidate_spec = Some(spec);
        self.candidate = Some(session);
        Ok(metadata.to_string())
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|v| format!("{v:02x}")).collect()
}
fn storage_error(code: &str) -> StorageError {
    match code {
        "Quota" => StorageError::Quota,
        "BusyWriter" => StorageError::BusyWriter,
        "StaleRevision" => StorageError::StaleRevision,
        "MissingBase" => StorageError::MissingBase,
        "Corrupt" => StorageError::Corrupt,
        "IdentityChanged" => StorageError::IdentityChanged,
        "UnsupportedRange" => StorageError::UnsupportedRange,
        "OutOfBounds" => StorageError::OutOfBounds,
        "MutationConflict" => StorageError::MutationConflict,
        "UnsupportedSchema" => StorageError::UnsupportedSchema,
        _ => StorageError::Unavailable,
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
    include!("../../../tests/fixtures/sb16_pcm.rs");
    #[test]
    fn guest_pcm_metadata_comes_from_guest_dsp_commands() {
        let mut engine = WorkerEngine::new();
        assert_eq!(engine.guest_pcm_bits(), 0);
        engine.session = Session::new(&sb16_pcm_program()).unwrap();
        engine.start();
        engine.advance(10000);
        assert_eq!(engine.guest_pcm_rate(), 16000);
        assert_eq!(engine.guest_pcm_bits(), 8);
        assert_eq!(engine.guest_pcm_channels(), 1);
    }
    include!("../../../tests/fixtures/opl3.rs");
    #[test]
    fn worker_transports_guest_opl3_audio_in_pcm_packets() {
        let mut engine = WorkerEngine::new();
        engine.session = Session::new(&opl3_tone_program()).unwrap();
        engine.start();
        let mut nonzero = 0;
        for _ in 0..50 {
            engine.advance(10000);
            let count = engine.drain_pcm();
            nonzero += engine.pcm[..count]
                .iter()
                .filter(|f| f.samples != [0; 2])
                .count();
        }
        assert_eq!(engine.status(), 1);
        assert!(nonzero > 1000);
    }
    #[test]
    fn worker_exposes_guest_yield_before_the_next_batch() {
        let mut engine = WorkerEngine::new();
        engine.session =
            Session::new(&[0xB8, 0x80, 0x16, 0xCD, 0x2F, 0xB8, 0, 0x4C, 0xCD, 0x21]).unwrap();
        engine.start();
        assert!(engine.advance(1000) < 1000);
        assert!(engine.timeslice_yielded());
        assert_eq!(engine.session.status(), SessionStatus::Running);
        engine.advance(1000);
        assert!(!engine.timeslice_yielded());
        assert_eq!(engine.session.status(), SessionStatus::Exited(0));
    }
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
        assert_eq!(engine.midi_bytes(), message.len() as f64);
        assert_eq!(engine.midi_notes(), 1.0);
        assert!(!engine.midi[..count].contains(&0xF0));
        assert!(
            engine.midi[..count]
                .windows(3)
                .any(|m| m == [0x90, 60, 100])
        );
        engine.set_output_mode("mt32").unwrap();
        let count = engine.snapshot_midi();
        assert_eq!(engine.midi_notes(), 1.0);
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
