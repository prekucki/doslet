//! Standalone game runtime compiled to WASM, without JS or WASI imports.
//! Setup, state serialization and audio hashing are outside timed execution.
#[allow(dead_code)]
#[path = "../../doslet/src/archive.rs"]
mod archive;
#[allow(dead_code)]
#[path = "../../common/machine_profile.rs"]
mod machine_profile;
#[path = "../../common/wasm_game.rs"]
mod wasm_game;

use doslet_runtime::{Session, session::WorkingState};
use doslet_x86_wasm::{Translation, host::Host};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use wasm_game::{Game, setup};

pub struct Probe {
    host: Host<Game>,
    initial: WorkingState,
    scratch: Box<[u8]>,
    output: Vec<u8>,
    pcm: Sha256,
    midi: Sha256,
}
impl Probe {
    /// Four little-endian lengths followed by ZIP, TOML, saved state and profile.
    pub fn new(bundle: &[u8]) -> Result<Self, String> {
        let mut rest = bundle.get(16..).ok_or("short fixture header")?;
        let mut parts = Vec::new();
        for len in bundle[..16].as_chunks::<4>().0 {
            let len = u32::from_le_bytes(*len) as usize;
            if len > rest.len() {
                return Err("short fixture body".into());
            }
            let (part, tail) = rest.split_at(len);
            parts.push(part);
            rest = tail;
        }
        if !rest.is_empty() {
            return Err("trailing fixture bytes".into());
        }
        let mut spec = machine_profile::parse_machine_profile(
            std::str::from_utf8(parts[1]).map_err(|e| e.to_string())?,
        )?;
        for mount in &mut spec.mounts {
            mount.policy = doslet_config::WritePolicy::Temporary;
        }
        let archive = archive::GameArchive::new(Arc::new(parts[0].to_vec()))?;
        let wanted = spec.game.executable.replace('\\', "/").to_ascii_uppercase();
        let selected = archive
            .programs
            .iter()
            .find(|p| **p == wanted || p.ends_with(&format!("/{wanted}")))
            .ok_or("missing EXE")?;
        let media = archive.load_configured(selected, &spec)?;
        let mut session =
            Session::load_state_with_media(parts[2], &media).map_err(|e| e.to_string())?;
        setup(&mut session);
        let initial = session.capture_working_state().map_err(|e| e.to_string())?;
        let translation = Translation::from_bytes(parts[3]).map_err(str::to_owned)?;
        Ok(Self {
            host: Host::new(translation, Game { session }),
            initial,
            scratch: vec![0; doslet_x86_wasm::STATE_BYTES].into_boxed_slice(),
            output: Vec::new(),
            pcm: Sha256::new(),
            midi: Sha256::new(),
        })
    }
    pub fn reset(&mut self, cache: bool) -> Result<(), String> {
        let mut session = self.initial.restore().map_err(|e| e.to_string())?;
        setup(&mut session);
        session.set_decode_cache(cache);
        self.host
            .replace_runtime(Game { session })
            .map_err(str::to_owned)?;
        self.pcm = Sha256::new();
        self.midi = Sha256::new();
        Ok(())
    }
    pub fn run_reference(&mut self, budget: u32) -> u32 {
        self.host.runtime_mut().session.advance(budget)
    }
    pub fn poll(&mut self, completed: u32, remaining: u32) -> i32 {
        self.host
            .poll(completed, remaining, &mut self.scratch)
            .expect("game runtime poll")
    }
    pub fn drain(&mut self) {
        let session = &mut self.host.runtime_mut().session;
        let mut pcm = [[0i16; 2]; 2048];
        loop {
            let n = session.drain_pcm(&mut pcm);
            if n == 0 {
                break;
            }
            for frame in &pcm[..n] {
                for sample in frame {
                    self.pcm.update(sample.to_le_bytes());
                }
            }
        }
        let mut midi = [0; 2048];
        loop {
            let n = session.drain_midi(&mut midi);
            if n == 0 {
                break;
            }
            self.midi.update(&midi[..n]);
        }
    }
    pub fn prepare_output(&mut self, kind: u32) {
        self.output = match kind {
            0 => self
                .host
                .runtime()
                .session
                .save_state()
                .expect("save guest state"),
            1 => self
                .pcm
                .clone()
                .finalize()
                .iter()
                .chain(self.midi.clone().finalize().iter())
                .copied()
                .collect(),
            2 => self.host.translation().wasm(),
            3 => {
                let s = self.host.stats();
                serde_json::to_vec(&serde_json::json!({"translated_steps":s.translated_steps,"interpreted_steps":s.interpreted_steps,"polls":s.polls,"grants":s.grants})).unwrap()
            }
            _ => panic!("unknown output kind"),
        };
    }
    pub fn output(&self) -> &[u8] {
        &self.output
    }
}

// This host-only ABI is not exposed by the emulation libraries. Every pointer
// is allocated here and used only by the synchronous diagnostic Wasmtime driver.
#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
mod abi {
    use super::Probe;
    use std::ptr;

    #[unsafe(no_mangle)]
    pub extern "C" fn probe_input(len: u32) -> u32 {
        assert!(len <= 256 * 1024 * 1024);
        Box::into_raw(vec![0u8; len as usize].into_boxed_slice()) as *mut u8 as u32
    }
    // SAFETY contract for all following exports: the trusted driver supplies
    // live, module-owned handles/buffers of the original size, synchronously,
    // without aliasing calls. new consumes input; free consumes a probe handle.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn probe_new(input: u32, len: u32) -> u32 {
        let bytes = unsafe {
            Box::from_raw(ptr::slice_from_raw_parts_mut(
                input as *mut u8,
                len as usize,
            ))
        };
        Box::into_raw(Box::new(Probe::new(&bytes).expect("load probe fixture"))) as u32
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn probe_free(handle: u32) {
        drop(unsafe { Box::from_raw(handle as *mut Probe) });
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn probe_reset(handle: u32, cache: u32) {
        unsafe { &mut *(handle as *mut Probe) }
            .reset(cache != 0)
            .expect("reset probe");
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn probe_run(handle: u32, budget: u32) -> u32 {
        unsafe { &mut *(handle as *mut Probe) }.run_reference(budget)
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn probe_poll(handle: u32, completed: u32, remaining: u32) -> i32 {
        unsafe { &mut *(handle as *mut Probe) }.poll(completed, remaining)
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn probe_scratch(handle: u32) -> u32 {
        unsafe { &mut *(handle as *mut Probe) }.scratch.as_mut_ptr() as u32
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn probe_drain(handle: u32) {
        unsafe { &mut *(handle as *mut Probe) }.drain();
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn probe_output(handle: u32, kind: u32) -> u64 {
        let probe = unsafe { &mut *(handle as *mut Probe) };
        probe.prepare_output(kind);
        ((probe.output.len() as u64) << 32) | (probe.output.as_ptr() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_bundle_is_rejected() {
        assert!(Probe::new(&[]).is_err());
        assert!(Probe::new(&[0xff; 16]).is_err());
        assert!(Probe::new(&[0; 17]).is_err());
    }
}
