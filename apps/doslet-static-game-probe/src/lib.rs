//! Acceptance host for one static game module and the actual Rust WASM runtime.
//! Executable conversion and runtime code admission remain distinct operations.

#[path = "../../common/static_session.rs"]
pub mod static_session;

#[allow(dead_code)]
#[path = "../../doslet/src/archive.rs"]
mod archive;
#[allow(dead_code)]
#[path = "../../common/machine_profile.rs"]
mod machine_profile;
#[path = "../../common/wasm_game.rs"]
#[allow(dead_code)]
mod wasm_game;

pub mod metadata;
#[cfg(not(target_arch = "wasm32"))]
pub mod prepare;

use doslet_runtime::{Session, session::WorkingState};
use doslet_x86_wasm::static_program::{Address, Frame, abi, memory};
use sha2::{Digest, Sha256};
use static_session::{Control, Metadata, StaticSession};
use std::sync::Arc;

pub const OUTPUT_STATE: u32 = 0;
/// SHA-256 of drained little-endian stereo PCM, then SHA-256 of drained MIDI.
pub const OUTPUT_AUDIO_HASHES: u32 = 1;
pub const OUTPUT_STATS: u32 = 3;
pub const MAX_CONFIG_BYTES: usize = 1024 * 1024;
pub const MAX_STATE_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_METADATA_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_BUNDLE_BYTES: usize =
    16 + archive::MAX_ZIP_BYTES + MAX_CONFIG_BYTES + MAX_STATE_BYTES + MAX_METADATA_BYTES;

/// The frame is first, so its wasm32 address is also the opaque owner handle.
/// The matching generated module executes synchronously between begin/finish;
/// no Rust reference may survive entry to that module. A generated-code trap
/// requires abandoning this owner, since partially executed work cannot commit.
#[repr(C)]
pub struct Probe {
    frame: [u8; abi::FRAME_BYTES + memory::BYTES],
    code_pages: Vec<u8>,
    direct_guards: Vec<u8>,
    owner: StaticSession,
    initial: WorkingState,
    output: Vec<u8>,
    pcm: Sha256,
    midi: Sha256,
    reference_steps: u64,
    reference_skipped_steps: u64,
}

/// Validate all framing and limits before parsing or copying fixture data.
fn bundle_parts(bundle: &[u8]) -> Result<[&[u8]; 4], String> {
    if bundle.len() > MAX_BUNDLE_BYTES {
        return Err("fixture exceeds the total size limit".into());
    }
    let header = bundle.get(..16).ok_or("short fixture header")?;
    let limits = [
        archive::MAX_ZIP_BYTES,
        MAX_CONFIG_BYTES,
        MAX_STATE_BYTES,
        MAX_METADATA_BYTES,
    ];
    let mut parts = [&[][..]; 4];
    let mut offset = 16usize;
    for (index, length) in header.as_chunks::<4>().0.iter().enumerate() {
        let length = u32::from_le_bytes(*length) as usize;
        if length > limits[index] {
            return Err(format!("fixture part {index} exceeds its size limit"));
        }
        let end = offset.checked_add(length).ok_or("fixture size overflow")?;
        parts[index] = bundle.get(offset..end).ok_or("short fixture body")?;
        offset = end;
    }
    if offset != bundle.len() {
        return Err("trailing fixture bytes".into());
    }
    Ok(parts)
}

impl Probe {
    /// Four little-endian u32 lengths, then ZIP, TOML, saved state and DSM1/DSM2
    /// metadata. The ZIP supplies immutable media; mounted writes are temporary.
    pub fn new(bundle: &[u8]) -> Result<Self, String> {
        let [zip, config, state, metadata] = bundle_parts(bundle)?;
        let metadata = metadata::decode(metadata).map_err(|error| error.to_string())?;
        let mut spec = machine_profile::parse_machine_profile(
            std::str::from_utf8(config).map_err(|error| error.to_string())?,
        )?;
        for mount in &mut spec.mounts {
            mount.policy = doslet_config::WritePolicy::Temporary;
        }
        let archive = archive::GameArchive::new(Arc::new(zip.to_vec()))?;
        let wanted = spec.game.executable.replace('\\', "/").to_ascii_uppercase();
        let selected = archive
            .programs
            .iter()
            .find(|program| **program == wanted || program.ends_with(&format!("/{wanted}")))
            .ok_or("missing EXE/COM")?;
        let media = archive.load_configured(selected, &spec)?;
        let mut session =
            Session::load_state_with_media(state, &media).map_err(|error| error.to_string())?;
        wasm_game::setup(&mut session);
        let initial = session
            .capture_working_state()
            .map_err(|error| error.to_string())?;
        let mut owner = StaticSession::new(session, metadata).map_err(|error| error.to_string())?;
        let code_pages = owner
            .direct_code_pages()
            .map_err(|error| error.to_string())?;
        let direct_guards = owner.direct_guards().map_err(|error| error.to_string())?;
        let mut probe = Self {
            frame: [0; abi::FRAME_BYTES + memory::BYTES],
            code_pages,
            direct_guards,
            owner,
            initial,
            output: Vec::new(),
            pcm: Sha256::new(),
            midi: Sha256::new(),
            reference_steps: 0,
            reference_skipped_steps: 0,
        };
        probe.load_idle_frame(0, 0);
        Ok(probe)
    }

    /// Architectural state is current after finish; generated work can be live
    /// in frame/register temporaries while a run is active.
    pub fn session(&self) -> &Session {
        self.owner.session()
    }

    pub fn metadata(&self) -> &Metadata {
        self.owner.metadata()
    }

    pub fn frame(&self) -> &[u8; abi::FRAME_BYTES] {
        self.frame[..abi::FRAME_BYTES]
            .try_into()
            .expect("fixed CPU frame")
    }

    /// Trusted driver view of the small ABI frame, never the guest RAM owner.
    pub fn frame_mut(&mut self) -> &mut [u8; abi::FRAME_BYTES] {
        (&mut self.frame[..abi::FRAME_BYTES])
            .try_into()
            .expect("fixed CPU frame")
    }

    /// Restore outside timing. WorkingState restoration resets acceleration
    /// policy, so every restore explicitly reapplies the controlled baseline.
    pub fn reset(&mut self, cache: bool) -> Result<(), String> {
        self.owner
            .session_mut()
            .map_err(|error| error.to_string())?;
        let mut session = self.initial.restore().map_err(|error| error.to_string())?;
        wasm_game::setup(&mut session);
        session.set_decode_cache(cache);
        let mut owner = StaticSession::new(session, self.owner.metadata().clone())
            .map_err(|error| error.to_string())?;
        owner
            .set_fallback_diagnostics(self.owner.fallback_diagnostics_enabled())
            .map_err(|error| error.to_string())?;
        self.direct_guards = owner.direct_guards().map_err(|error| error.to_string())?;
        self.owner = owner;
        self.pcm = Sha256::new();
        self.midi = Sha256::new();
        self.reference_steps = 0;
        self.reference_skipped_steps = 0;
        self.output.clear();
        self.frame.fill(0);
        self.load_idle_frame(0, 0);
        Ok(())
    }

    pub fn run_reference(&mut self, budget: u32) -> Result<u32, String> {
        let skipped_before = self.session().idle_loop_stats().skipped_steps;
        let completed = self
            .owner
            .session_mut()
            .map_err(|error| error.to_string())?
            .advance(budget);
        self.reference_steps = self.reference_steps.saturating_add(u64::from(completed));
        self.reference_skipped_steps = self.reference_skipped_steps.saturating_add(
            self.session()
                .idle_loop_stats()
                .skipped_steps
                .saturating_sub(skipped_before),
        );
        self.load_idle_frame(
            u64::from(budget.saturating_sub(completed)),
            u64::from(completed),
        );
        Ok(completed)
    }

    /// Explicit reference-policy selection for comparisons. Disable again
    /// before comparing portable state, whose header records the idle setting.
    pub fn set_reference_idle(&mut self, enabled: bool) -> Result<(), String> {
        self.owner
            .session_mut()
            .map_err(|error| error.to_string())?
            .set_idle_loops(enabled);
        Ok(())
    }

    /// Optional read-only fallback classification; retained across reset while
    /// its counts restart at zero. Select before begin, outside measured work.
    pub fn set_fallback_diagnostics(&mut self, enabled: bool) -> Result<(), String> {
        self.owner
            .set_fallback_diagnostics(enabled)
            .map_err(|error| error.to_string())
    }

    pub fn begin(&mut self, budget: u64) -> Result<(), String> {
        self.owner
            .begin_run(budget)
            .map_err(|error| error.to_string())?
            .write(&mut self.frame)
            .expect("fixed static frame size");
        self.refresh_direct_memory()?;
        Ok(())
    }

    /// Required after every generated return, including a callback Exit.
    pub fn finish(&mut self) -> Result<(), String> {
        self.commit_direct_memory()?;
        let mut frame = Frame::read(&self.frame).expect("fixed static frame size");
        let result = self.owner.finish_run(&mut frame);
        frame
            .write(&mut self.frame)
            .expect("fixed static frame size");
        result.map_err(|error| error.to_string())
    }

    /// Admission must not synchronize stale frame GPRs while generated register
    /// temporaries are live. The owner checks only code/context and hard grants.
    pub fn admit(&mut self, id: u32, required: u32) -> Result<bool, String> {
        self.owner
            .admit(id, required)
            .map_err(|error| error.to_string())
    }

    pub fn callback(&mut self, reason: u32) -> Result<Control, String> {
        self.commit_direct_memory()?;
        let mut frame = Frame::read(&self.frame).expect("fixed static frame size");
        let result = self.owner.callback(&mut frame, reason);
        frame
            .write(&mut self.frame)
            .expect("fixed static frame size");
        let control = result.map_err(|error| error.to_string())?;
        if control == Control::Continue {
            self.refresh_direct_memory()?;
        }
        Ok(control)
    }

    /// Memory-only slow path. Registers remain live in the generated module;
    /// neither instruction execution nor guest time is advanced here.
    /// A refusal leaves reservations and all architectural/device state intact.
    pub fn memory_write(&mut self, address: u32, bytes: u32, value: u32) -> Result<bool, String> {
        if !self
            .owner
            .memory_write_allowed(address, bytes)
            .map_err(|error| error.to_string())?
        {
            return Ok(false);
        }
        let effects = memory::Effects::read(&self.frame[abi::FRAME_BYTES..])
            .expect("fixed memory descriptor");
        if effects.inline_reserved == 0 {
            return Err("memory helper requires a current inline reservation".into());
        }
        self.commit_direct_memory()?;
        self.owner
            .memory_write(address, bytes, value)
            .map_err(|error| error.to_string())?;
        // The frame's address and GPRs may be stale. Refresh only the borrowed
        // memory contract, retaining the accumulated pending instruction count.
        #[cfg(target_arch = "wasm32")]
        let descriptor = self
            .owner
            .prepare_direct_memory(&self.code_pages, &self.direct_guards)
            .map_err(|error| error.to_string())?;
        #[cfg(not(target_arch = "wasm32"))]
        let descriptor = memory::Descriptor::default();
        descriptor
            .write(&mut self.frame[abi::FRAME_BYTES..])
            .expect("fixed memory descriptor");
        Ok(true)
    }

    fn commit_direct_memory(&mut self) -> Result<(), String> {
        let bytes = &mut self.frame[abi::FRAME_BYTES..];
        let effects = memory::Effects::read(bytes).expect("fixed memory descriptor");
        if effects.device_reads != 0 || effects.ram_writes != 0 || effects.inline_reserved != 0 {
            self.owner
                .complete_direct_memory(effects)
                .map_err(|error| error.to_string())?;
            memory::Effects::clear(bytes).expect("fixed memory descriptor");
        }
        Ok(())
    }

    fn refresh_direct_memory(&mut self) -> Result<(), String> {
        let address = Frame::read(&self.frame).expect("fixed CPU frame").address;
        let generated_entry = self
            .owner
            .refresh_direct_guard(address, &mut self.direct_guards)
            .map_err(|error| error.to_string())?;
        #[cfg(target_arch = "wasm32")]
        let descriptor = if generated_entry {
            self.owner
                .prepare_direct_memory(&self.code_pages, &self.direct_guards)
                .map_err(|error| error.to_string())?
        } else {
            memory::Descriptor::default()
        };
        #[cfg(not(target_arch = "wasm32"))]
        let descriptor = {
            let _ = (&self.code_pages, generated_entry);
            memory::Descriptor::default()
        };
        descriptor
            .write(&mut self.frame[abi::FRAME_BYTES..])
            .expect("fixed memory descriptor");
        Ok(())
    }

    /// Drain at identical boundaries for each engine; hashing is outside timing.
    pub fn drain(&mut self) -> Result<(), String> {
        let session = self
            .owner
            .session_mut()
            .map_err(|error| error.to_string())?;
        let mut pcm = [[0i16; 2]; 2048];
        loop {
            let count = session.drain_pcm(&mut pcm);
            if count == 0 {
                break;
            }
            for frame in &pcm[..count] {
                for sample in frame {
                    self.pcm.update(sample.to_le_bytes());
                }
            }
        }
        let mut midi = [0; 2048];
        loop {
            let count = session.drain_midi(&mut midi);
            if count == 0 {
                break;
            }
            self.midi.update(&midi[..count]);
        }
        Ok(())
    }

    pub fn prepare_output(&mut self, kind: u32) -> Result<(), String> {
        // Outputting a stale Session during generated execution would make a
        // seemingly valid comparison omit live translated work.
        self.owner
            .session_mut()
            .map_err(|error| error.to_string())?;
        self.output = match kind {
            OUTPUT_STATE => self
                .session()
                .save_state()
                .map_err(|error| error.to_string())?,
            OUTPUT_AUDIO_HASHES => self
                .pcm
                .clone()
                .finalize()
                .iter()
                .chain(self.midi.clone().finalize().iter())
                .copied()
                .collect(),
            OUTPUT_STATS => {
                let counters = self.owner.counters();
                let diagnostics = counters.fallback_diagnostics;
                serde_json::to_vec(&serde_json::json!({
                    "translated_steps": counters.translated_steps,
                    "interpreted_steps": counters.interpreted_steps.saturating_add(self.reference_steps.saturating_sub(self.reference_skipped_steps)),
                    "callbacks": counters.callbacks,
                    "memory_write_helpers": counters.memory_write_helpers,
                    "guard_refusals": counters.guard_refusals,
                    "wait_calls": counters.wait_calls,
                    "skipped_wait_steps": counters.skipped_wait_steps.saturating_add(self.reference_skipped_steps),
                    "fallback_diagnostics_enabled": self.owner.fallback_diagnostics_enabled(),
                    "fallback_diagnostics": {
                        "fallback_callbacks": diagnostics.fallback_callbacks,
                        "service_callbacks": diagnostics.service_callbacks,
                        "unknown_entry": diagnostics.unknown_entry,
                        "unsupported_stub": diagnostics.unsupported_stub,
                        "context_refused": diagnostics.context_refused,
                        "hard_grant": diagnostics.hard_grant,
                        "hard_grant_required_steps_sum": diagnostics.hard_grant_required_steps_sum,
                        "hard_grant_available_steps_sum": diagnostics.hard_grant_available_steps_sum,
                        "hard_grant_required_steps_max": diagnostics.hard_grant_required_steps_max,
                        "hard_grant_available_steps_max": diagnostics.hard_grant_available_steps_max,
                        "source_changed": diagnostics.source_changed,
                        "stale_proof": diagnostics.stale_proof,
                        "memory_or_guard": diagnostics.memory_or_guard,
                    },
                    "steps": self.session().steps(),
                    "emulated_nanos": self.session().emulated_nanos(),
                    "status": format!("{:?}", self.session().status()),
                    "fault": self.session().fault().map(|fault| fault.to_string()),
                }))
                .map_err(|error| error.to_string())?
            }
            _ => return Err("unknown output kind".into()),
        };
        Ok(())
    }

    /// Borrowed output remains valid until prepare_output, reset or free.
    pub fn output(&self) -> &[u8] {
        &self.output
    }

    fn load_idle_frame(&mut self, remaining_budget: u64, charged_work: u64) {
        let session = self.session();
        let linear = session.debug_pc();
        let address = self
            .metadata()
            .objects
            .iter()
            .find_map(|object| {
                let offset = linear.checked_sub(object.linear_base)?;
                (offset < object.size).then_some(Address {
                    space: object.space,
                    offset,
                })
            })
            .unwrap_or(Address {
                space: 0,
                offset: linear,
            });
        Frame {
            gpr: doslet_runtime::session::debug::GENERAL
                .map(|register| session.registers().get32(register)),
            eflags: session.eflags(),
            address,
            remaining_budget,
            charged_work,
            ..Frame::default()
        }
        .write(&mut self.frame)
        .expect("fixed static frame size");
    }
}

// Trusted synchronous diagnostic ABI, confined to this app. Handles and input
// buffers must originate in this module, remain live, and retain their original
// size. new consumes input and free consumes a handle. The host must not alias
// calls, reenter generated execution from callbacks, or access any memory except
// the frame while generated execution is live. No Rust borrow spans that entry.
// Callback protocol failures trap; the host must then abandon the instance.
#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
mod ffi {
    use super::{MAX_BUNDLE_BYTES, Probe};
    use std::ptr;

    #[unsafe(no_mangle)]
    pub extern "C" fn static_probe_input(len: u32) -> u32 {
        assert!(len as usize <= MAX_BUNDLE_BYTES, "fixture allocation limit");
        Box::into_raw(vec![0u8; len as usize].into_boxed_slice()) as *mut u8 as u32
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_probe_new(input: u32, len: u32) -> u32 {
        let bytes = unsafe {
            Box::from_raw(ptr::slice_from_raw_parts_mut(
                input as *mut u8,
                len as usize,
            ))
        };
        Box::into_raw(Box::new(
            Probe::new(&bytes).expect("load static game probe fixture"),
        )) as u32
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_probe_free(handle: u32) {
        drop(unsafe { Box::from_raw(handle as *mut Probe) });
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_probe_reset(handle: u32, cache: u32) {
        unsafe { &mut *(handle as *mut Probe) }
            .reset(cache != 0)
            .expect("reset static game probe");
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_probe_diagnose_fallbacks(handle: u32, enabled: u32) {
        unsafe { &mut *(handle as *mut Probe) }
            .set_fallback_diagnostics(enabled != 0)
            .expect("select static game fallback diagnostics");
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_probe_begin(handle: u32, budget: u64) {
        unsafe { &mut *(handle as *mut Probe) }
            .begin(budget)
            .expect("begin static game probe run");
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_probe_finish(handle: u32) {
        unsafe { &mut *(handle as *mut Probe) }
            .finish()
            .expect("finish static game probe run");
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_probe_frame(handle: u32) -> u32 {
        unsafe { &mut *(handle as *mut Probe) }
            .frame_mut()
            .as_mut_ptr() as u32
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_admit(frame_pointer: u32, id: u32, required: u32) -> u32 {
        u32::from(
            unsafe { &mut *(frame_pointer as *mut Probe) }
                .admit(id, required)
                .expect("admit static game probe region"),
        )
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_runtime(frame_pointer: u32, reason: u32) -> u32 {
        unsafe { &mut *(frame_pointer as *mut Probe) }
            .callback(reason)
            .expect("static game probe callback")
            .abi_value()
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_memory_write(
        frame_pointer: u32,
        address: u32,
        bytes: u32,
        value: u32,
    ) -> u32 {
        u32::from(
            unsafe { &mut *(frame_pointer as *mut Probe) }
                .memory_write(address, bytes, value)
                .expect("static game memory write helper"),
        )
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_probe_reference(handle: u32, budget: u32) -> u32 {
        unsafe { &mut *(handle as *mut Probe) }
            .run_reference(budget)
            .expect("run static game probe reference")
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_probe_drain(handle: u32) {
        unsafe { &mut *(handle as *mut Probe) }
            .drain()
            .expect("drain static game probe audio");
    }

    /// Packed u64: borrowed pointer in low 32 bits, byte length in high 32 bits.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn static_probe_output(handle: u32, kind: u32) -> u64 {
        let probe = unsafe { &mut *(handle as *mut Probe) };
        probe
            .prepare_output(kind)
            .expect("serialize static game probe output");
        ((probe.output().len() as u64) << 32) | (probe.output().as_ptr() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doslet_x86::SegmentRegister;
    use static_session::{CodeContext, CodePart, ObjectBinding, RegionMetadata};
    use std::io::{Cursor, Write};

    const CODE: u32 = 0x10130;

    /// Self-contained DOS media and a genuine guest protected-mode transition.
    /// This fixture establishes wrapper behavior, not game compatibility.
    fn fixture_bundle(body: &[u8], required_steps: u32) -> Vec<u8> {
        let mut program = [0x90; 0xB0];
        program[..24].copy_from_slice(&[
            0xFA, // CLI
            0x0F, 0x01, 0x16, 0xA0, 0x01, // LGDT [01A0]
            0x0F, 0x20, 0xC0, // MOV EAX,CR0
            0x66, 0x83, 0xC8, 0x01, // OR EAX,1
            0x0F, 0x22, 0xC0, // MOV CR0,EAX
            0x66, 0xEA, 0x30, 0x01, 0x01, 0, 8, 0, // JMP 0008:00010130
        ]);
        program[0x30..0x30 + body.len()].copy_from_slice(body);
        program[0x80..0x88].fill(0);
        program[0x88..0x90].copy_from_slice(&[0xFF, 0xFF, 0, 0, 0, 0x9B, 0xCF, 0]);
        program[0xA0..0xA6].copy_from_slice(&[0x0F, 0, 0x80, 0x01, 0x01, 0]);
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in [("GAME.COM", &program[..]), ("DATA.DAT", b"fixture media")] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        let zip = writer.finish().unwrap().into_inner();
        let config = b"version=2\n[machine]\nmemory_mib=4\n[game]\nexecutable='GAME.COM'\n[[drives]]\nletter='C'\nkind='zip'\nsource='fixture.zip'\npolicy='persistent'\nstore='fixture'\n";
        let mut spec =
            machine_profile::parse_machine_profile(str::from_utf8(config).unwrap()).unwrap();
        for mount in &mut spec.mounts {
            mount.policy = doslet_config::WritePolicy::Temporary;
        }
        let archive = archive::GameArchive::new(Arc::new(zip.clone())).unwrap();
        let mut session = archive.load_configured("GAME.COM", &spec).unwrap();
        wasm_game::setup(&mut session);
        assert_eq!(session.advance(6), 6, "{:?}", session.fault());
        assert_eq!(session.debug_pc(), CODE);
        let metadata = Metadata {
            objects: vec![ObjectBinding {
                space: 1,
                linear_base: CODE,
                size: 0x50,
                default32: true,
            }],
            regions: vec![RegionMetadata {
                id: 0,
                entry: Address {
                    space: 1,
                    offset: 0,
                },
                required_steps,
                wait_proof_steps: 0,
                parts: if required_steps == 0 {
                    vec![]
                } else {
                    vec![CodePart {
                        linear: CODE,
                        expected: body.to_vec(),
                    }]
                },
            }],
            context: CodeContext {
                cs: session.registers().segment_cache(SegmentRegister::Cs),
            },
        };
        let state = session.save_state().unwrap();
        let metadata = metadata::encode(&metadata).unwrap();
        let mut bundle = Vec::new();
        for part in [&zip[..], &config[..], &state, &metadata] {
            bundle.extend_from_slice(&(part.len() as u32).to_le_bytes());
        }
        for part in [&zip[..], &config[..], &state, &metadata] {
            bundle.extend_from_slice(part);
        }
        bundle
    }

    fn output(probe: &mut Probe, kind: u32) -> Vec<u8> {
        probe.prepare_output(kind).unwrap();
        probe.output().to_vec()
    }

    #[test]
    fn bundle_limits_truncation_and_trailing_data_are_checked_before_parsing() {
        let mut bundle = Vec::new();
        for length in [3u32, 2, 1, 4] {
            bundle.extend_from_slice(&length.to_le_bytes());
        }
        bundle.extend_from_slice(b"0123456789");
        assert_eq!(
            bundle_parts(&bundle).unwrap(),
            [b"012".as_slice(), b"34", b"5", b"6789"]
        );
        for length in 0..bundle.len() {
            assert!(bundle_parts(&bundle[..length]).is_err());
        }
        bundle.push(0);
        assert!(bundle_parts(&bundle).is_err());
        for (index, limit) in [
            archive::MAX_ZIP_BYTES,
            MAX_CONFIG_BYTES,
            MAX_STATE_BYTES,
            MAX_METADATA_BYTES,
        ]
        .into_iter()
        .enumerate()
        {
            let mut header = [0; 16];
            header[index * 4..index * 4 + 4].copy_from_slice(&((limit + 1) as u32).to_le_bytes());
            assert!(bundle_parts(&header).unwrap_err().contains("size limit"));
        }
        assert!(Probe::new(&[0; 16]).is_err());
        assert!(Probe::new(&[0xFF; 16]).is_err());
    }

    #[test]
    fn exclusive_frame_admission_finish_and_outputs_preserve_full_session() {
        let bundle = fixture_bundle(&[0x40, 0xEB, 0xFD], 2);
        let mut probe = Probe::new(&bundle).unwrap();
        let mut reference = Probe::new(&bundle).unwrap();
        assert_eq!(
            std::ptr::from_ref(&probe).cast::<u8>(),
            probe.frame().as_ptr()
        );
        probe.frame_mut()[44..48].copy_from_slice(&[1, 2, 3, 4]);
        probe.frame_mut()[68..72].copy_from_slice(&[5, 6, 7, 8]);
        let original_eax = probe.session().registers().get32(doslet_x86::Gpr32::Eax);
        probe.begin(4).unwrap();
        assert!(probe.reset(false).is_err());
        assert!(probe.run_reference(1).is_err());
        assert!(probe.drain().is_err());
        assert!(probe.prepare_output(OUTPUT_STATE).is_err());
        assert!(probe.admit(0, 2).unwrap());
        // Direct chaining sees the old frame while compiled temporaries are live.
        assert!(probe.admit(0, 2).unwrap());
        assert_eq!(
            probe.session().registers().get32(doslet_x86::Gpr32::Eax),
            original_eax
        );
        assert_eq!(reference.run_reference(4).unwrap(), 4);
        let completed = Frame::read(reference.frame()).unwrap();
        completed.write(probe.frame_mut()).unwrap();
        assert_eq!(probe.callback(abi::REASON_SERVICE).unwrap(), Control::Exit);
        // Callback Exit still requires the explicit ownership-return boundary.
        assert!(probe.reset(false).is_err());
        probe.finish().unwrap();
        probe.finish().unwrap();
        assert_eq!(&probe.frame()[44..48], &[1, 2, 3, 4]);
        assert_eq!(&probe.frame()[68..72], &[5, 6, 7, 8]);
        assert_eq!(
            output(&mut probe, OUTPUT_STATE),
            output(&mut reference, OUTPUT_STATE)
        );
        probe.drain().unwrap();
        reference.drain().unwrap();
        assert_eq!(
            output(&mut probe, OUTPUT_AUDIO_HASHES),
            output(&mut reference, OUTPUT_AUDIO_HASHES)
        );
        let stats: serde_json::Value =
            serde_json::from_slice(&output(&mut probe, OUTPUT_STATS)).unwrap();
        assert_eq!(stats["translated_steps"], 4);
        assert_eq!(stats["interpreted_steps"], 0);
        assert_eq!(stats["callbacks"], 1);
        assert_eq!(stats["status"], "Running");
        assert_eq!(stats["fault"], serde_json::Value::Null);
    }

    #[test]
    fn stale_guard_retry_refreshes_current_entry_and_preserves_complete_outputs() {
        for diagnostics in [false, true] {
            let bundle = fixture_bundle(&[0x40, 0xEB, 0xFD], 2);
            let mut probe = Probe::new(&bundle).unwrap();
            let mut reference = Probe::new(&bundle).unwrap();
            probe.set_fallback_diagnostics(diagnostics).unwrap();
            for candidate in [&mut probe, &mut reference] {
                assert!(
                    candidate
                        .owner
                        .session_mut()
                        .unwrap()
                        .debug_write_memory(CODE + 16, &[0x91])
                );
            }
            // Retain the stale shared table, as when a direct edge reaches a
            // region after ordinary execution changed another byte on its page.
            let entry = probe.owner.begin_run(2).unwrap();
            entry.write(probe.frame_mut()).unwrap();
            let stale_guards = probe.direct_guards.clone();
            assert_eq!(
                probe.callback(abi::REASON_FALLBACK).unwrap(),
                Control::Continue
            );
            assert_eq!(Frame::read(probe.frame()).unwrap(), entry);
            assert_ne!(probe.direct_guards, stale_guards);
            assert_eq!(probe.owner.counters().interpreted_steps, 0);

            assert!(probe.admit(0, 2).unwrap());
            assert_eq!(reference.run_reference(2).unwrap(), 2);
            Frame::read(reference.frame())
                .unwrap()
                .write(probe.frame_mut())
                .unwrap();
            probe.finish().unwrap();
            assert_eq!(probe.owner.counters().translated_steps, 2);
            assert_eq!(
                output(&mut probe, OUTPUT_STATE),
                output(&mut reference, OUTPUT_STATE)
            );
            probe.drain().unwrap();
            reference.drain().unwrap();
            assert_eq!(
                output(&mut probe, OUTPUT_AUDIO_HASHES),
                output(&mut reference, OUTPUT_AUDIO_HASHES)
            );
        }
    }

    #[test]
    fn repeated_restores_reapply_policy_and_reset_observable_outputs() {
        let mut probe = Probe::new(&fixture_bundle(&[0x40, 0xEB, 0xFD], 2)).unwrap();
        let initial = output(&mut probe, OUTPUT_STATE);
        let mut expected = None;
        for cache in [false, true, true, false] {
            probe.reset(cache).unwrap();
            assert!(!probe.session().dynamic_core_enabled());
            assert!(probe.session().debug_register_engine_budget(2) >= 2);
            if !cache {
                assert_eq!(output(&mut probe, OUTPUT_STATE), initial);
            }
            assert_eq!(probe.run_reference(128).unwrap(), 128);
            let cpu = probe.session().debug_cpu().snapshot();
            if let Some(expected) = &expected {
                assert!(probe.session().debug_cpu().matches(expected));
            } else {
                expected = Some(cpu);
            }
            probe.drain().unwrap();
            let stats: serde_json::Value =
                serde_json::from_slice(&output(&mut probe, OUTPUT_STATS)).unwrap();
            assert_eq!(stats["translated_steps"], 0);
            assert_eq!(stats["interpreted_steps"], 128);
        }
        probe.reset(false).unwrap();
        assert!(probe.output().is_empty());
        let hashes = output(&mut probe, OUTPUT_AUDIO_HASHES);
        assert_eq!(&hashes[..32], &Sha256::digest([])[..]);
        assert_eq!(&hashes[32..], &Sha256::digest([])[..]);
        assert!(probe.prepare_output(2).is_err());
    }

    #[test]
    fn fallback_policy_survives_reset_and_changes_only_diagnostic_stats() {
        let bundle = fixture_bundle(&[0x40, 0xEB, 0xFD], 2);
        let mut enabled = Probe::new(&bundle).unwrap();
        let mut disabled = Probe::new(&bundle).unwrap();
        let initial_stats: serde_json::Value =
            serde_json::from_slice(&output(&mut enabled, OUTPUT_STATS)).unwrap();
        assert_eq!(initial_stats["fallback_diagnostics_enabled"], false);
        assert!(
            initial_stats["fallback_diagnostics"]
                .as_object()
                .unwrap()
                .values()
                .all(|count| count == 0)
        );
        enabled.set_fallback_diagnostics(true).unwrap();
        for cache in [false, true] {
            enabled.reset(cache).unwrap();
            disabled.reset(cache).unwrap();
            let reset_stats: serde_json::Value =
                serde_json::from_slice(&output(&mut enabled, OUTPUT_STATS)).unwrap();
            assert_eq!(reset_stats["fallback_diagnostics_enabled"], true);
            assert_eq!(reset_stats["fallback_diagnostics"]["fallback_callbacks"], 0);
            for probe in [&mut enabled, &mut disabled] {
                probe.begin(4).unwrap();
                assert!(probe.set_fallback_diagnostics(false).is_err());
                for _ in 0..4 {
                    probe.callback(abi::REASON_FALLBACK).unwrap();
                }
                probe.finish().unwrap();
                probe.drain().unwrap();
            }
            assert_eq!(
                output(&mut enabled, OUTPUT_STATE),
                output(&mut disabled, OUTPUT_STATE)
            );
            assert_eq!(
                output(&mut enabled, OUTPUT_AUDIO_HASHES),
                output(&mut disabled, OUTPUT_AUDIO_HASHES)
            );
            let counts: serde_json::Value =
                serde_json::from_slice(&output(&mut enabled, OUTPUT_STATS)).unwrap();
            assert_eq!(counts["fallback_diagnostics"]["fallback_callbacks"], 4);
            assert_eq!(counts["fallback_diagnostics"]["unknown_entry"], 2);
            assert_eq!(counts["fallback_diagnostics"]["memory_or_guard"], 2);
            let disabled_stats: serde_json::Value =
                serde_json::from_slice(&output(&mut disabled, OUTPUT_STATS)).unwrap();
            assert!(
                disabled_stats["fallback_diagnostics"]
                    .as_object()
                    .unwrap()
                    .values()
                    .all(|count| count == 0)
            );
        }
    }

    #[test]
    fn ordinary_guest_fault_exits_and_status_remains_serializable() {
        let mut probe = Probe::new(&fixture_bundle(&[0x0F, 0x0B], 0)).unwrap();
        probe.begin(1).unwrap();
        assert_eq!(probe.callback(abi::REASON_FALLBACK).unwrap(), Control::Exit);
        probe.finish().unwrap();
        let frame = Frame::read(probe.frame()).unwrap();
        assert_eq!(frame.charged_work, 0);
        assert_eq!(frame.remaining_budget, 1);
        assert!(probe.prepare_output(OUTPUT_STATE).is_err());
        let stats: serde_json::Value =
            serde_json::from_slice(&output(&mut probe, OUTPUT_STATS)).unwrap();
        assert_eq!(stats["status"], "Faulted");
        assert!(stats["fault"].is_string());
        assert_eq!(stats["interpreted_steps"], 0);
        // A normal guest fault can reset; a host protocol fault cannot.
        probe.reset(false).unwrap();
        probe.begin(1).unwrap();
        assert!(probe.callback(u32::MAX).is_err());
        assert!(probe.finish().is_err());
        assert!(probe.reset(false).is_err());
        assert!(probe.prepare_output(OUTPUT_STATE).is_err());
    }
}
