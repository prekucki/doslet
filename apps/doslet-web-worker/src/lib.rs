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
mod aot;
#[path = "../../common/procedure_manifest.rs"]
mod procedure_manifest;
use aot::BrowserSession;
use doslet_storage::{
    CachedSource, HostCompletion, HostRequest, RequestTag, SourceId, SourceIdentity, StorageError,
};
use doslet_x86::Gpr16;
use std::sync::Arc;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

/// Bundles run with the profile's DPMI host, direct unless it names another.
fn bundle_dpmi(spec: Option<&MachineSpec>) -> Option<doslet_runtime::session::dpmi::Mode> {
    use doslet_runtime::session::dpmi::Mode;
    match spec
        .and_then(|spec| spec.dpmi)
        .unwrap_or(doslet_config::DpmiHost::Direct)
    {
        doslet_config::DpmiHost::Off => None,
        doslet_config::DpmiHost::Reflect => Some(Mode::Reflect),
        doslet_config::DpmiHost::Direct => Some(Mode::Direct),
    }
}

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct WorkerEngine {
    session: BrowserSession,
    archive: Option<archive::GameArchive>,
    active: Option<(archive::GameArchive, String)>,
    active_spec: MachineSpec,
    candidate: Option<Session>,
    candidate_spec: Option<MachineSpec>,
    candidate_archive: Option<(archive::GameArchive, String)>,
    bundle_runtime: bool,
    candidate_bundle_runtime: bool,
    adaptive_cpu: bool,
    candidate_adaptive_cpu: bool,
    tier_profile: Option<TierProfile>,
    candidate_tier_profile: Option<TierProfile>,
    tiers_active: bool,
    candidate_tiers_active: bool,
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
    /// Generated AOT modules import this fixed scratch as their base.
    aot_scratch: Box<[u8]>,
}

#[path = "../../common/web_speed_tiers.rs"]
mod web_speed_tiers;
use web_speed_tiers::{TierProfile, parse_tier_profile};
impl Default for WorkerEngine {
    fn default() -> Self {
        Self::new()
    }
}
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl WorkerEngine {
    pub fn keyboard_mode(&self) -> String {
        match self.session.hardware_config().keyboard {
            doslet_config::KeyboardMode::At => "at",
            doslet_config::KeyboardMode::Bios => "bios",
        }
        .into()
    }
    pub fn set_keyboard_mode(&mut self, mode: &str) -> Result<(), String> {
        let mode = match mode {
            "at" => doslet_config::KeyboardMode::At,
            "bios" => doslet_config::KeyboardMode::Bios,
            _ => return Err("Keyboard mode must be bios or at".into()),
        };
        self.session
            .set_keyboard_mode(mode)
            .map_err(|e| e.to_string())?;
        self.session.release_pointer_buttons();
        self.active_spec.hardware.keyboard = mode;
        Ok(())
    }
    pub fn audio_use(&self) -> u8 {
        let used = self.session.audio_use();
        u8::from(used.sb16) | (u8::from(used.opl3) << 1) | (u8::from(used.mpu401) << 2)
    }
    pub fn configure_unused_audio(&mut self, profile: &str) -> Result<(), String> {
        let spec = machine_profile::parse_machine_profile(profile)?;
        let audio = spec.hardware.audio();
        self.session
            .configure_unused_audio(audio)
            .map_err(|e| e.to_string())?;
        self.active_spec.hardware = self.active_spec.hardware.with_audio(audio);
        Ok(())
    }
    pub fn cpu_mhz(&self) -> u16 {
        self.active_spec.hardware.cpu_mhz
    }
    pub fn effective_cpu_mhz(&self) -> u16 {
        self.session.hardware_config().cpu_mhz
    }
    /// Browser pacing caps adaptive throughput at host capacity, preserving the
    /// user's ceiling. Zero, or the ceiling itself, removes the cap.
    pub fn set_host_cpu_mhz(&mut self, mhz: u16) -> Result<(), String> {
        if !self.adaptive_cpu || mhz > self.active_spec.hardware.cpu_mhz {
            return Err("Host CPU rate must be within the adaptive ceiling".into());
        }
        self.session
            .set_host_cpu_limit(mhz)
            .map_err(|e| e.to_string())
    }
    /// Current host capacity cap, or zero.
    pub fn host_cpu_limit(&self) -> u16 {
        self.session.host_cpu_limit()
    }
    pub fn cpu_mode(&self) -> String {
        if self.tiers_active {
            "tiers"
        } else if self.adaptive_cpu {
            "adaptive"
        } else {
            "fixed"
        }
        .into()
    }
    pub fn set_cpu_settings(&mut self, mode: &str, mhz: u16) -> Result<(), String> {
        let adaptive = match mode {
            "fixed" => false,
            "adaptive" => true,
            _ => return Err("CPU mode must be fixed or adaptive".into()),
        };
        if !(1..=1000).contains(&mhz) {
            return Err("CPU rate must be 1..1000 MHz".into());
        }
        if self.tiers_active {
            self.session
                .set_execution_tiers(None, [Some(500), Some(8), Some(133)])
                .map_err(str::to_owned)?;
            self.tiers_active = false;
        }
        self.set_cpu_mhz(mhz)?;
        self.session.set_adaptive_cpu(adaptive);
        if !adaptive && self.bundle_runtime {
            self.session.set_adaptive_idle(true);
        }
        self.adaptive_cpu = adaptive;
        Ok(())
    }
    pub fn set_tier_settings(&mut self, hot: u16, cold: u16, grey: u16) -> Result<(), String> {
        if hot > 1000 || !(1..=1000).contains(&cold) || !(1..=1000).contains(&grey) {
            return Err("Speed-tier rates must be 1..1000 MHz".into());
        }
        let profile = self
            .tier_profile
            .as_mut()
            .ok_or("No native speed-tier profile loaded")?;
        let rates = [
            if hot == 0 { None } else { Some(hot) },
            Some(cold),
            Some(grey),
        ];
        self.session.set_adaptive_cpu(false);
        self.session.set_adaptive_idle(false);
        self.session.set_synchronization_analysis(false);
        self.session
            .set_execution_tiers(Some(profile.labels.clone()), rates)
            .map_err(str::to_owned)?;
        profile.rates = rates;
        self.adaptive_cpu = false;
        self.tiers_active = true;
        Ok(())
    }
    pub fn tier_rates(&self) -> Vec<u16> {
        self.tier_profile
            .as_ref()
            .map_or_else(Vec::new, |p| p.rates.map(|r| r.unwrap_or(0)).to_vec())
    }
    pub fn tier_counts(&self) -> Vec<u64> {
        self.session.executed_tier_counts().to_vec()
    }
    pub fn prepare_candidate_tiers(&mut self, profile: &str) -> Result<(), String> {
        let profile = parse_tier_profile(profile)?;
        let candidate = self.candidate.as_mut().ok_or("No prepared machine")?;
        candidate.set_adaptive_cpu(false);
        candidate.set_adaptive_idle(false);
        candidate.set_synchronization_analysis(false);
        candidate
            .set_execution_tiers(Some(profile.labels.clone()), profile.rates)
            .map_err(str::to_owned)?;
        self.candidate_tier_profile = Some(profile);
        self.candidate_tiers_active = true;
        self.candidate_adaptive_cpu = false;
        Ok(())
    }
    pub fn set_cpu_mhz(&mut self, mhz: u16) -> Result<(), String> {
        self.session.set_cpu_mhz(mhz).map_err(|e| e.to_string())?;
        self.active_spec.hardware.cpu_mhz = mhz;
        Ok(())
    }
    pub fn pointer_relative(&mut self, dx: f64, dy: f64) {
        if self.session.status() == SessionStatus::Running && self.pointer_relative_in_use() {
            self.session.pointer_relative(dx, dy);
        }
    }
    pub fn pointer_relative_in_use(&self) -> bool {
        self.active_spec
            .input
            .mouse_mode
            .relative(self.session.pointer_relative_in_use())
    }
    pub fn set_mouse_mode(&mut self, mode: &str) -> Result<(), String> {
        self.active_spec.input.mouse_mode = mode.parse::<doslet_config::MouseMode>()?;
        self.session.release_pointer_buttons();
        Ok(())
    }
    pub fn pointer_event(&mut self, x: u16, y: u16, button: i8, pressed: bool) {
        if button == -2 {
            self.session.release_pointer_buttons();
        } else if button >= 0 {
            self.session.pointer_button(button as u8, pressed);
        } else if !self.pointer_relative_in_use() {
            self.session.pointer_move(x, y);
        }
    }
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new() -> Self {
        #[cfg(target_arch = "wasm32")]
        console_error_panic_hook::set_once();
        Self {
            session: Session::demo().into(),
            archive: None,
            active: None,
            active_spec: MachineSpec::default(),
            candidate: None,
            candidate_spec: None,
            candidate_archive: None,
            bundle_runtime: false,
            candidate_bundle_runtime: false,
            adaptive_cpu: false,
            candidate_adaptive_cpu: false,
            tier_profile: None,
            candidate_tier_profile: None,
            tiers_active: false,
            candidate_tiers_active: false,
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
            aot_scratch: vec![0; doslet_x86_wasm::STATE_BYTES].into_boxed_slice(),
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
        self.adaptive_cpu = false;
        self.tier_profile = None;
        self.tiers_active = false;
        self.bundle_runtime = false;
        self.machine_epoch += 1;
        self.session = session.into();
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
    /// Enable the profile's DPMI host and adaptive CPU throughput for the bundled game.
    pub fn prepare_bundle_runtime(&mut self) -> Result<(), String> {
        let host = bundle_dpmi(self.candidate_spec.as_ref());
        let candidate = self.candidate.as_mut().ok_or("No prepared machine")?;
        if let Some(mode) = host {
            candidate
                .enable_dpmi(mode)
                .map_err(|error| error.to_string())?;
        }
        candidate.set_adaptive_cpu(true);
        self.candidate_bundle_runtime = true;
        self.candidate_adaptive_cpu = true;
        Ok(())
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
        self.session = candidate.into();
        self.active = self.candidate_archive.take();
        self.active_spec = spec;
        self.bundle_runtime = self.candidate_bundle_runtime;
        self.candidate_bundle_runtime = false;
        self.adaptive_cpu = self.candidate_adaptive_cpu;
        self.candidate_adaptive_cpu = false;
        self.tier_profile = self.candidate_tier_profile.take();
        self.tiers_active = self.candidate_tiers_active;
        self.candidate_tiers_active = false;
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
        self.candidate_bundle_runtime = false;
        self.candidate_adaptive_cpu = false;
        self.candidate_tier_profile = None;
        self.candidate_tiers_active = false;
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
        }
        .into();
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
    /// Install only compiler-produced ABI-v1 metadata between execution batches.
    pub fn validate_aot(&self, profile: &[u8]) -> Result<(), String> {
        doslet_x86_wasm::Translation::from_bytes(profile)
            .map(|_| ())
            .map_err(str::to_owned)
    }
    /// Configured pacing and idle policies stay active; grants yield to them
    /// whenever they observe execution.
    pub fn install_aot(&mut self, profile: &[u8]) -> Result<(), String> {
        self.session.install(profile)
    }
    pub fn aot_begin(&mut self) {
        self.session.begin();
    }
    pub fn aot_abi_ptr(&self) -> *const u8 {
        self.aot_scratch.as_ptr()
    }
    pub fn aot_abi_len(&self) -> usize {
        doslet_x86_wasm::STATE_BYTES
    }
    pub fn aot_poll(&mut self, completed: u32, remaining: u32) -> Result<i32, String> {
        self.session
            .poll(completed, remaining, &mut self.aot_scratch)
    }
    pub fn aot_translated_steps(&self) -> u64 {
        self.session.translated()
            + self
                .session
                .wasm_procedure_stats()
                .map_or(0, |stats| stats.translated_instructions)
    }
    pub fn aot_diagnostics(&self) -> String {
        self.session.diagnostics()
    }
    /// Validate the same bounded declarations used by the native loader.
    pub fn validate_procedure_aot(&self, metadata: &[u8]) -> Result<(), String> {
        procedure_manifest::parse(metadata)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
    /// The generated exports import this worker's memory. This callback only
    /// selects an already compiled export; it must never reenter WorkerEngine.
    #[cfg(target_arch = "wasm32")]
    pub fn install_procedure_aot(
        &mut self,
        metadata: &[u8],
        callback: js_sys::Function,
    ) -> Result<(), String> {
        let bindings = procedure_manifest::parse(metadata).map_err(|error| error.to_string())?;
        let call = Box::new(move |index: u32, pointer: u32| {
            let result = callback
                .call2(
                    &JsValue::UNDEFINED,
                    &JsValue::from(index),
                    &JsValue::from(pointer),
                )
                .map_err(|error| format!("Browser procedure execution: {error:?}"))?;
            result
                .as_f64()
                .filter(|n| {
                    n.is_finite() && *n >= 0.0 && *n <= f64::from(u32::MAX) && n.fract() == 0.0
                })
                .map(|n| n as u32)
                .ok_or_else(|| "Browser procedure returned a non-u32 status".to_owned())
        });
        self.session.install_browser_wasm_procedures(call, bindings)
    }
    pub fn procedure_aot_diagnostics(&self) -> String {
        let stats = self.session.wasm_procedure_stats();
        serde_json::json!({
            "installed": stats.is_some(),
            "entries": self.session.wasm_procedure_entry_count(),
            "copyTierEntries": self.session.wasm_procedure_copy_tier_entries().collect::<Vec<_>>(),
            "calls": stats.map_or(0, |s| s.calls),
            "translatedSteps": stats.map_or(0, |s| s.translated_instructions),
            "kernelCalls": stats.map_or(0, |s| s.kernel_calls),
            "kernelInstructions": stats.map_or(0, |s| s.kernel_instructions),
            "sideExits": stats.map_or(0, |s| s.side_exits),
            "sourceRefusals": stats.map_or(0, |s| s.source_refusals),
            "budgetRefusals": stats.map_or(0, |s| s.budget_refusals),
            "kernelIrqRefusals": stats.map_or(0, |s| s.kernel_irq_refusals),
            "tierRefusals": stats.map_or(0, |s| s.tier_refusals),
            "translatedTiers": stats.map_or([0;4], |s| s.translated_tiers),
            "lastError": self.session.wasm_procedure_last_error(),
        })
        .to_string()
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
    /// Queue one configured ASCII character as a BIOS scan/ASCII pair.
    pub fn startup_key(&mut self, ascii: u8) -> Result<bool, String> {
        let (scan, ascii) = doslet_config::bios_key_for_ascii(ascii)
            .ok_or("Unsupported startup keyboard character")?;
        self.session
            .push_key(scan, ascii)
            .map_err(|e| e.to_string())
    }
    pub fn waiting_for_key(&self) -> bool {
        self.session.waiting_for_key()
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
    pub fn disk_read_bytes(&self) -> f64 {
        self.session.disk_read_bytes() as f64
    }
    pub fn video_mode_label(&self) -> String {
        self.session.video_mode_label().to_string()
    }
    pub fn video_refresh_millihertz(&self) -> u32 {
        self.session.video_refresh_millihertz()
    }
    pub fn idle_nanos(&self) -> f64 {
        self.session
            .idle_loop_stats()
            .skipped_nanos
            .saturating_add(self.session.menu_idle_stats().nanos) as f64
    }
    pub fn memory_usage(&self) -> String {
        let m = self.session.memory_usage();
        serde_json::json!({
            "installed_bytes": m.installed_bytes,
            "protected_mode": m.protected_mode,
            "dos_allocated": m.dos_allocated,
            "dos_free": m.dos_free,
            "dos_largest_free": m.dos_largest_free,
            "dos_blocks": m.dos_blocks,
            "dpmi_available": m.dpmi_available,
            "dpmi_entered": m.dpmi_entered,
            "dpmi_capacity": m.dpmi_capacity,
            "dpmi_requested": m.dpmi_requested,
            "dpmi_committed": m.dpmi_committed,
            "dpmi_peak": m.dpmi_peak,
            "dpmi_blocks": m.dpmi_blocks,
        })
        .to_string()
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
    pub fn midi_channels(&self) -> Vec<u8> {
        let mut bytes = vec![0; 16 * doslet_midi_routing::Channel::BYTES];
        for (channel, out) in self.routing.channels(self.output_mode).into_iter().zip(
            bytes
                .as_chunks_mut::<{ doslet_midi_routing::Channel::BYTES }>()
                .0,
        ) {
            channel.write(out);
        }
        bytes
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
        if !spec.game.patches.is_empty() {
            return Err(
                "game.patches currently requires the native loader and local patch files".into(),
            );
        }
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
        self.candidate_bundle_runtime = false;
        self.candidate_adaptive_cpu = false;
        self.candidate_tier_profile = if reset {
            self.tier_profile.clone()
        } else {
            None
        };
        self.candidate_tiers_active = reset && self.tiers_active;
        if reset && self.bundle_runtime {
            if let Some(mode) = bundle_dpmi(Some(&spec)) {
                session
                    .enable_dpmi(mode)
                    .map_err(|error| error.to_string())?;
            }
            if self.adaptive_cpu {
                session.set_adaptive_cpu(true);
            } else if !self.tiers_active {
                session.set_adaptive_idle(true);
            }
            self.candidate_bundle_runtime = true;
        }
        if reset && self.adaptive_cpu && !self.bundle_runtime {
            session.set_adaptive_cpu(true);
        }
        self.candidate_adaptive_cpu = reset && self.adaptive_cpu;
        if self.candidate_tiers_active {
            let profile = self
                .candidate_tier_profile
                .as_ref()
                .ok_or("Missing tier profile")?;
            session
                .set_execution_tiers(Some(profile.labels.clone()), profile.rates)
                .map_err(str::to_owned)?;
        }
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
    include!("../../../tests/fixtures/midi.rs");
    use super::*;
    #[test]
    #[ignore = "requires the local prepared ROTT browser module and learned instruction labels"]
    fn local_browser_rott_copy_has_a_canonical_cfg_and_exact_mixed_label_counts() {
        use doslet_runtime::wasm_procedures::bundle;
        let module_path = std::env::var_os("DOSLET_PROCEDURE_BUNDLE")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../target/rott-browser-procedures/procedures.wasm")
            });
        let profile_path = std::env::var_os("DOSLET_PROCEDURE_TIERS")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("XDG_CACHE_HOME")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| {
                        std::path::PathBuf::from(
                            std::env::var_os("HOME").expect("HOME or DOSLET_PROCEDURE_TIERS"),
                        )
                        .join(".cache")
                    })
                    .join(
                        "doslet/speed-tiers/profiles/RiseoftheTriad-speed-tiers.json.learned.json",
                    )
            });
        let module = std::fs::read(module_path).unwrap();
        let metadata = bundle::read(&module).unwrap().unwrap().metadata;
        let mut bindings = procedure_manifest::parse(metadata).unwrap();
        let profile: serde_json::Value =
            serde_json::from_slice(&std::fs::read(profile_path).unwrap()).unwrap();
        let nodes: std::collections::BTreeMap<_, _> = profile["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|node| node["bits"] == 32)
            .map(|node| (node["pc"].as_u64().unwrap() as u32, node))
            .collect();
        let binding = bindings
            .iter_mut()
            .find(|binding| binding.kernel.is_some())
            .unwrap();
        let entry = binding.entry_ip;
        let kernel = binding.kernel.unwrap();
        binding.code.sort_unstable_by_key(|part| part.linear);
        let mut sites = Vec::new();
        for part in &binding.code {
            let mut cursor = 0;
            while cursor < part.expected.len() {
                let length = usize::from(
                    doslet_x86::translation::instruction_length(&part.expected[cursor..], true)
                        .unwrap(),
                );
                let address = part.linear + cursor as u32;
                let expected = &part.expected[cursor..cursor + length];
                let tier = nodes
                    .get(&address)
                    .filter(|node| {
                        node["bytes"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .take(length)
                            .map(|n| n.as_u64().unwrap() as u8)
                            .eq(expected.iter().copied())
                    })
                    .map_or(2, |node| match node["tier"].as_str().unwrap() {
                        "fast" => 0,
                        "slow" => 1,
                        "irq" => 3,
                        _ => 2,
                    });
                sites.push(tier);
                cursor += length;
            }
        }
        assert_eq!(kernel.canonical_sites(), Some(sites.len()));
        let count = 201;
        let mut counts = [0u64; 4];
        for (site, tier) in sites.into_iter().enumerate() {
            counts[tier] += u64::from(kernel.canonical_repetitions(count, site).unwrap());
        }
        assert_eq!(
            counts.iter().sum::<u64>(),
            u64::from(kernel.actual_work(count).unwrap())
        );
        assert!(
            counts[0] > 0 && counts[2] > 0,
            "learned renderer should retain both FAST and GREY work"
        );
        let mut engine = WorkerEngine::new();
        engine
            .session
            .install_wasm_procedures(&module, bindings)
            .unwrap();
        let diagnostics: serde_json::Value =
            serde_json::from_str(&engine.procedure_aot_diagnostics()).unwrap();
        assert!(
            diagnostics["copyTierEntries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|value| value.as_u64() == Some(u64::from(entry)))
        );
        println!(
            "local browser copy entry={entry:#x}, canonical CFG proved, ECX={count}, original instruction tier counts={counts:?}"
        );
    }
    #[test]
    fn blaster_audio_change_requires_restart_and_preserves_worker_state() {
        let mut engine = WorkerEngine::new();
        engine.session = Session::new(&[0xeb, 0xfe]).unwrap().into();
        engine.start();
        engine.advance(100);
        let before = (
            engine.session.epoch(),
            engine.session.steps(),
            engine.session.emulated_nanos(),
        );
        let mut spec = engine.active_spec.clone();
        spec.hardware.sb16 = false;
        spec.hardware.mpu401 = false;
        let error = engine
            .configure_unused_audio(&machine_profile::export_machine_profile(&spec))
            .unwrap_err();
        assert!(error.contains("BLASTER"));
        assert_eq!(
            (
                engine.session.epoch(),
                engine.session.steps(),
                engine.session.emulated_nanos()
            ),
            before
        );
        assert_eq!(engine.session.status(), SessionStatus::Running);
        assert_eq!(engine.audio_use(), 0);
        engine.prepare_reset().unwrap();
        assert!(engine.candidate.as_ref().unwrap().hardware_config().sb16);
        assert!(engine.candidate.as_ref().unwrap().hardware_config().mpu401);
    }
    #[test]
    fn keyboard_mode_changes_live_and_is_retained_by_reset() {
        let mut engine = WorkerEngine::new();
        engine.session = Session::new(&[0x40, 0xeb, 0xfd]).unwrap().into();
        engine.start();
        engine.advance(100);
        let before = (
            engine.session.debug_pc(),
            engine.session.steps(),
            engine.session.emulated_nanos(),
        );
        engine.set_keyboard_mode("at").unwrap();
        assert_eq!(engine.keyboard_mode(), "at");
        assert_eq!(engine.session.status(), SessionStatus::Running);
        assert_eq!(
            (
                engine.session.debug_pc(),
                engine.session.steps(),
                engine.session.emulated_nanos()
            ),
            before
        );
        assert!(engine.set_keyboard_mode("invalid").is_err());
        assert_eq!(engine.keyboard_mode(), "at");
        engine.prepare_reset().unwrap();
        assert_eq!(
            engine
                .candidate
                .as_ref()
                .unwrap()
                .hardware_config()
                .keyboard,
            doslet_config::KeyboardMode::At
        );
        engine.discard_machine();
        engine.set_keyboard_mode("bios").unwrap();
        assert_eq!(engine.keyboard_mode(), "bios");
    }
    include!("../../../tests/fixtures/sb16_pcm.rs");
    #[test]
    fn mouse_capture_mode_selects_host_motion_without_restarting_the_worker() {
        let mut engine = WorkerEngine::new();
        engine.session = Session::new(&[0xb8, 0x0b, 0, 0xcd, 0x33, 0xEB, 0xFE])
            .unwrap()
            .into();
        engine.start();
        engine.advance(16);
        assert!(engine.pointer_relative_in_use());
        let before = engine.session.pointer_position();
        engine.pointer_event(10, 20, -1, false);
        assert_eq!(engine.session.pointer_position(), before);
        let steps = engine.session.steps();
        engine.set_mouse_mode("no-capture").unwrap();
        engine.pointer_event(10, 20, -1, false);
        engine.pointer_relative(30.0, 30.0);
        assert_eq!(engine.session.pointer_position(), (10, 20));
        engine.set_mouse_mode("capture").unwrap();
        let until = engine.session.emulated_nanos() + 1_500_000_000;
        while engine.session.emulated_nanos() < until {
            let remaining = until - engine.session.emulated_nanos();
            engine.session.debug_advance_device_time(remaining).unwrap();
        }
        assert!(!engine.session.pointer_relative_in_use());
        assert!(engine.pointer_relative_in_use());
        engine.pointer_relative(2.0, 3.0);
        assert_eq!(engine.session.pointer_position(), (12, 23));
        engine.set_mouse_mode("auto").unwrap();
        assert!(!engine.pointer_relative_in_use());
        assert_eq!(engine.session.steps(), steps);
        assert!(engine.set_mouse_mode("unknown").is_err());
        assert_eq!(
            engine.active_spec.input.mouse_mode,
            doslet_config::MouseMode::Auto
        );
    }

    #[test]
    fn live_cpu_speed_preserves_execution_and_the_reset_recipe() {
        let mut engine = WorkerEngine::new();
        engine.session = Session::new(&[0x40, 0xeb, 0xfd]).unwrap().into();
        engine.start();
        engine.advance(100);
        engine.pause();
        let pc = engine.session.debug_pc();
        let nanos = engine.session.emulated_nanos();
        let steps = engine.session.steps();
        engine.set_cpu_mhz(100).unwrap();
        assert_eq!(engine.cpu_mhz(), 100);
        assert_eq!(engine.session.hardware_config().cpu_mhz, 100);
        assert_eq!(engine.session.debug_pc(), pc);
        assert_eq!(engine.session.emulated_nanos(), nanos);
        assert_eq!(engine.session.steps(), steps);
        assert_eq!(engine.session.status(), SessionStatus::Paused);
        assert!(engine.set_cpu_mhz(0).is_err());
        assert!(engine.set_cpu_mhz(1001).is_err());
        assert_eq!(engine.cpu_mhz(), 100);
        engine.prepare_reset().unwrap();
        assert_eq!(
            engine.candidate.as_ref().unwrap().hardware_config().cpu_mhz,
            100
        );
    }
    #[test]
    fn adaptive_cpu_setting_survives_reset_and_can_return_to_fixed() {
        let mut engine = WorkerEngine::new();
        engine.set_cpu_settings("adaptive", 133).unwrap();
        assert_eq!(engine.cpu_mode(), "adaptive");
        assert_eq!(engine.cpu_mhz(), 133);
        assert!(engine.session.synchronization_analysis().enabled);
        engine.set_host_cpu_mhz(33).unwrap();
        assert_eq!(engine.cpu_mhz(), 133);
        assert_eq!(engine.effective_cpu_mhz(), 33);
        // The host cap never replaces the adaptive ceiling, and it can recover.
        assert_eq!(engine.host_cpu_limit(), 33);
        assert_eq!(
            engine.session.synchronization_analysis().configured_mhz,
            133
        );
        assert!(engine.set_host_cpu_mhz(134).is_err());
        engine.set_host_cpu_mhz(0).unwrap();
        assert_eq!(engine.host_cpu_limit(), 0);
        engine.set_host_cpu_mhz(33).unwrap();
        engine.prepare_reset().unwrap();
        assert!(engine.candidate_adaptive_cpu);
        assert!(
            engine
                .candidate
                .as_ref()
                .unwrap()
                .synchronization_analysis()
                .enabled
        );
        engine.discard_machine();
        engine.set_cpu_settings("fixed", 66).unwrap();
        assert_eq!(engine.cpu_mode(), "fixed");
        assert_eq!(engine.cpu_mhz(), 66);
        assert!(!engine.session.synchronization_analysis().controller_enabled);
        assert!(engine.set_cpu_settings("invalid", 100).is_err());
        assert_eq!(engine.cpu_mhz(), 66);
    }
    #[test]
    fn native_labels_drive_browser_tiers_and_survive_reset() {
        let guard = vec![0x90u8; 15];
        let input = serde_json::json!({
            "unknown":"grey", "irq_policy":"static-grey", "rates":[133,8,33],
            "nodes":[{"pc":0x1000,"bits":16,"bytes":guard,"tier":"fast"}]
        });
        let profile = parse_tier_profile(&input.to_string()).unwrap();
        assert_eq!(profile.rates, [Some(133), Some(8), Some(33)]);
        let mut engine = WorkerEngine::new();
        engine.tier_profile = Some(profile);
        engine.set_tier_settings(133, 8, 33).unwrap();
        assert_eq!(engine.cpu_mode(), "tiers");
        assert_eq!(engine.tier_rates(), vec![133, 8, 33]);
        engine.prepare_reset().unwrap();
        assert!(engine.candidate_tiers_active);
        assert!(engine.candidate_tier_profile.is_some());
        engine.set_cpu_settings("adaptive", 133).unwrap();
        assert_eq!(engine.cpu_mode(), "adaptive");
        assert!(engine.set_tier_settings(133, 8, 0).is_err());
        assert!(
            parse_tier_profile(&input.to_string().replace("static-grey", "static-fast")).is_err()
        );
    }
    #[test]
    fn guest_pcm_metadata_comes_from_guest_dsp_commands() {
        let mut engine = WorkerEngine::new();
        assert_eq!(engine.guest_pcm_bits(), 0);
        engine.session = Session::new(&sb16_pcm_program()).unwrap().into();
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
        engine.session = Session::new(&opl3_tone_program()).unwrap().into();
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
        engine.session = Session::new(&[0xB8, 0x80, 0x16, 0xCD, 0x2F, 0xB8, 0, 0x4C, 0xCD, 0x21])
            .unwrap()
            .into();
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
            append_midi_byte(&mut program, byte);
        }
        program.extend_from_slice(&[0xEB, 0xFE]);
        engine.session = Session::new(&program).unwrap().into();
        engine.start();
        for _ in 0..4 {
            engine.advance(10000);
        }
        let count = engine.drain_midi().unwrap();
        assert_eq!(engine.midi_mode(), "mt32");
        assert!(engine.midi_upgraded());
        assert_eq!(engine.midi_sysex(), 1.0);
        assert_eq!(engine.midi_bytes(), message.len() as f64);
        assert_eq!(engine.midi_notes(), 1.0);
        let channels = engine.midi_channels();
        let stride = doslet_midi_routing::Channel::BYTES;
        assert_eq!(channels.len(), 16 * stride);
        assert_eq!(
            doslet_midi_routing::Channel::read(&channels[..stride])
                .unwrap()
                .state
                .notes,
            1
        );
        assert!(!engine.midi[..count].contains(&0xF0));
        assert!(
            engine.midi[..count]
                .windows(3)
                .any(|m| m == [0x90, 60, 100])
        );
        engine.set_output_mode("mt32").unwrap();
        let channels = engine.midi_channels();
        assert_eq!(
            doslet_midi_routing::Channel::read(&channels[..stride])
                .unwrap()
                .state
                .notes,
            0
        );
        assert_eq!(
            doslet_midi_routing::Channel::read(&channels[stride..2 * stride])
                .unwrap()
                .state
                .notes,
            1
        );
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
