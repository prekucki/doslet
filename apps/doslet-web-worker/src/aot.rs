//! ABI v1 browser adapter. RAM stays in Session; only the compiler's bounded
//! register/dispatch scratch crosses the synchronous WASM call boundary.
use doslet_runtime::Session;
use doslet_runtime::SessionStatus;
use doslet_x86::Cpu;
use doslet_x86_wasm::host::{CodeCheck, Runtime};
use doslet_x86_wasm::{Translation, host::Host};
use std::ops::{Deref, DerefMut};
#[cfg(test)]
#[path = "../../../tests/fixtures/protected_graphics.rs"]
#[rustfmt::skip]
mod fixture;
struct Game {
    session: Session,
}
impl Runtime for Game {
    type Error = String;
    fn cpu(&self) -> &Cpu {
        self.session.debug_cpu()
    }
    fn cpu_mut(&mut self) -> &mut Cpu {
        self.session.debug_cpu_mut()
    }
    fn running(&self) -> bool {
        self.session.status() == SessionStatus::Running
            && !self.session.storage_waiting()
            && !self.session.timeslice_yielded()
    }
    fn prepare(&mut self) -> Result<(), String> {
        self.session.debug_prepare_register_engine();
        Ok(())
    }
    fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
        self.session.debug_code_bytes(address, length)
    }
    fn translated_budget(&self, remaining: u32) -> u32 {
        if self.session.debug_cpu().execution_labels_enabled() {
            self.session.debug_tiered_register_engine_budget(remaining)
        } else {
            self.session.debug_register_engine_budget(remaining)
        }
    }
    fn translated_tier(&self, address: u32, length: u8) -> Option<u8> {
        self.session
            .debug_cpu()
            .execution_labels_enabled()
            .then(|| self.session.debug_execution_tier_at(address, length))
    }
    fn account_translated_tier(&mut self, steps: u32, tier: u8) -> Result<(), String> {
        self.session
            .debug_account_tiered_register_engine(steps, tier)
            .map_err(|e| e.to_string())
    }
    fn account_translated(&mut self, steps: u32) -> Result<(), String> {
        self.session
            .debug_account_register_engine(steps)
            .map_err(|e| e.to_string())
    }
    fn code_check(&self, address: u32, length: u8) -> Option<CodeCheck> {
        self.session.debug_code_check(address, length)
    }
    fn code_valid(&self, check: &CodeCheck) -> bool {
        self.session.debug_code_check_valid(check)
    }
    fn set_entries(&mut self, sorted: Vec<u32>) -> Result<(), String> {
        self.session
            .debug_set_prepared_entries(sorted)
            .map_err(str::to_owned)
    }
    fn interpret(&mut self, budget: u32) -> Result<u32, String> {
        // Faults and stops end the run through `running`; zero ends it too.
        Ok(self.session.debug_advance_to_prepared_entry(budget))
    }
}

pub struct BrowserSession {
    host: Host<Game>,
    installed: bool,
}
impl From<Session> for BrowserSession {
    fn from(session: Session) -> Self {
        Self {
            host: Host::new(
                Translation::from_profile(b"DSJ1\0\0").expect("empty profile"),
                Game { session },
            ),
            installed: false,
        }
    }
}
impl Deref for BrowserSession {
    type Target = Session;
    fn deref(&self) -> &Session {
        &self.host.runtime().session
    }
}
impl DerefMut for BrowserSession {
    fn deref_mut(&mut self) -> &mut Session {
        &mut self.host.runtime_mut().session
    }
}
impl BrowserSession {
    pub fn install(&mut self, profile: &[u8]) -> Result<(), String> {
        let translation = Translation::from_bytes(profile)?;
        self.host
            .replace_translation(translation)
            .map_err(str::to_owned)?;
        self.installed = true;
        Ok(())
    }
    pub fn begin(&mut self) {
        self.host.runtime_mut().session.advance(0);
    }
    /// `scratch` is the generated module's imported base; its owner keeps one
    /// allocation for the worker's lifetime, across machine replacement.
    pub fn poll(
        &mut self,
        completed: u32,
        remaining: u32,
        scratch: &mut [u8],
    ) -> Result<i32, String> {
        if !self.installed {
            return Err("AOT is not installed".into());
        }
        self.host
            .poll(completed, remaining, scratch)
            .map_err(|e| format!("AOT runtime: {e:?}"))
    }
    pub fn translated(&self) -> u64 {
        self.host.stats().translated_steps
    }
    pub fn diagnostics(&self) -> String {
        let stats = self.host.stats();
        let cpu = self.host.runtime().session.debug_cpu();
        let cs = cpu.registers.segment_cache(doslet_x86::SegmentRegister::Cs);
        let regions: Vec<_> = self
            .host
            .regions()
            .map(|(r, delta)| serde_json::json!({"base": r.base, "size": r.size, "delta": delta}))
            .collect();
        serde_json::json!({
            "installed": self.installed,
            "entries": self.host.translation().blocks().len(),
            "regions": regions,
            "polls": stats.polls,
            "translatedSteps": stats.translated_steps,
            "translatedTiers": stats.translated_tiers,
            "interpretedSteps": stats.interpreted_steps,
            "interpreterBatches": stats.interpreter_batches,
            "grants": stats.grants,
            "zeroBudget": stats.zero_budget,
            "rejectedContext": stats.rejected_context,
            "missingCode": stats.missing_code,
            "pageValidations": stats.page_validations,
            "rebaseAttempts": stats.rebase_attempts,
            "coldEntries": stats.cold_entries,
            "code32": cpu.registers.code32(),
            "protectedMode": cpu.control.cr0 & 1 != 0,
            "paging": cpu.control.cr0 & 0x8000_0000 != 0,
            "executionLabels": cpu.execution_labels_enabled(),
            "csBase": cs.base,
            "csLimit": cs.limit,
        })
        .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn session() -> Session {
        let mut code = fixture::protected_graphics_com();
        // Same real-mode transition, with flat code and a register-only loop.
        code[19..23].copy_from_slice(&0x10400u32.to_le_bytes());
        code[0x208..0x210].copy_from_slice(&[0xff, 0xff, 0, 0, 0, 0x9a, 0xcf, 0]);
        code[0x300..0x303].copy_from_slice(&[0x40, 0xeb, 0xfd]);
        let mut session = Session::new(&code).unwrap();
        session.set_idle_loops(false);
        session.set_vga_wait_acceleration(false);
        session.start();
        for _ in 0..100 {
            if session.debug_cpu().registers.code32() {
                break;
            }
            assert_eq!(session.advance(1), 1);
        }
        assert_eq!(session.debug_pc(), 0x10400);
        session
    }

    #[test]
    fn browser_adapter_generated_work_matches_the_complete_interpreter_state() {
        let mut profile = b"DSJ1\x01\0".to_vec();
        profile.extend_from_slice(&0x10400u32.to_le_bytes());
        profile.extend_from_slice(&[3, 0x40, 0xeb, 0xfd]);
        let initial = session().capture_working_state().unwrap();
        let restore = || {
            let mut session = initial.restore().unwrap();
            session.set_idle_loops(false);
            session.set_vga_wait_acceleration(false);
            session.start();
            session
        };
        let host = Host::new(
            Translation::from_bytes(&profile).unwrap(),
            Game { session: restore() },
        );
        let mut runner = doslet_x86_wasm::native::Runner::new(host).unwrap();
        let mut reference = restore();
        assert_eq!(runner.run(1000).unwrap(), reference.advance(1000));
        assert!(runner.host().stats().translated_steps > 0);
        let actual = &runner.host().runtime().session;
        assert!(
            reference
                .debug_cpu()
                .matches(&actual.debug_cpu().snapshot()),
            "CPU differs"
        );
        assert_eq!(actual.emulated_nanos(), reference.emulated_nanos());
        assert!(
            runner.host().runtime().session.save_state().unwrap()
                == reference.save_state().unwrap(),
            "AOT state differs from interpreter"
        );
        // A changed instruction must use ordinary execution, preserving state.
        runner
            .host_mut()
            .runtime_mut()
            .session
            .debug_write_memory(0x10400, &[0x48]);
        reference.debug_write_memory(0x10400, &[0x48]);
        let translated = runner.host().stats().translated_steps;
        assert_eq!(runner.run(1000).unwrap(), reference.advance(1000));
        assert_eq!(runner.host().stats().translated_steps, translated);
        assert!(
            runner.host().runtime().session.save_state().unwrap()
                == reference.save_state().unwrap(),
            "changed source did not preserve interpreter state"
        );
    }

    #[test]
    fn guest_yield_stops_the_aot_batch_and_resumes_on_the_next_slice() {
        let mut session =
            Session::new(&[0xb8, 0x80, 0x16, 0xcd, 0x2f, 0xb8, 0, 0x4c, 0xcd, 0x21]).unwrap();
        session.start();
        let host = Host::new(
            Translation::from_profile(b"DSJ1\0\0").unwrap(),
            Game { session },
        );
        let mut runner = doslet_x86_wasm::native::Runner::new(host).unwrap();
        assert!(runner.run(100).unwrap() < 100);
        assert!(runner.host().runtime().session.timeslice_yielded());
        runner.host_mut().runtime_mut().session.advance(0);
        assert!(runner.run(100).unwrap() > 0);
        assert!(!runner.host().runtime().session.timeslice_yielded());
        assert!(runner.host().runtime().session.fault().is_none());
    }

    #[test]
    fn tiered_aot_matches_interpreter_time_counts_and_full_state_at_every_boundary() {
        let initial = session().capture_working_state().unwrap();
        for cache_enabled in [false, true] {
            for rates in [[Some(133), Some(8), Some(33)], [None, Some(8), Some(133)]] {
                for tiers in [[0u8, 1u8], [2, 3], [0, 0], [1, 1]] {
                    let labels: Vec<_> = [0x10400u32, 0x10401]
                        .into_iter()
                        .zip(tiers)
                        .map(|(address, tier)| {
                            let original = initial.restore().unwrap();
                            doslet_x86::ExecutionLabel {
                                address,
                                code32: true,
                                bytes: original
                                    .debug_code_bytes(address, 15)
                                    .unwrap()
                                    .try_into()
                                    .unwrap(),
                                tier,
                            }
                        })
                        .collect();
                    let nodes:Vec<_> = labels.iter().map(|l| serde_json::json!({
                    "pc":l.address,"bits":32,"bytes":l.bytes,"tier":(["fast","slow","grey","irq"][usize::from(l.tier)])
                })).collect();
                    let profile = serde_json::to_vec(&serde_json::json!({"nodes":nodes})).unwrap();
                    let restore = || {
                        let mut session = initial.restore().unwrap();
                        session.set_idle_loops(false);
                        session.set_vga_wait_acceleration(false);
                        session.set_decode_cache(cache_enabled);
                        session
                            .set_execution_tiers(Some(labels.clone()), rates)
                            .unwrap();
                        session.start();
                        session
                    };
                    let mut runner = doslet_x86_wasm::native::Runner::new(Host::new(
                        Translation::from_bytes(&profile).unwrap(),
                        Game { session: restore() },
                    ))
                    .unwrap();
                    let mut reference = restore();
                    for budget in [1, 2, 7, 64, 513] {
                        assert_eq!(runner.run(budget).unwrap(), reference.advance(budget));
                        let actual = &runner.host().runtime().session;
                        assert_eq!(
                            actual.executed_tier_counts(),
                            reference.executed_tier_counts()
                        );
                        assert_eq!(actual.emulated_nanos(), reference.emulated_nanos());
                        assert!(
                            actual.save_state().unwrap() == reference.save_state().unwrap(),
                            "tiered AOT changed guest state"
                        );
                    }
                    assert!(runner.host().stats().translated_steps > 0);
                    assert!(runner.host().stats().translated_tiers[usize::from(tiers[0])] > 0);
                    assert!(runner.host().stats().translated_tiers[usize::from(tiers[1])] > 0);
                    // Prepared execution guards the decoded instruction prefix;
                    // unrelated bytes after the loop must not recolour its work.
                    runner
                        .host_mut()
                        .runtime_mut()
                        .session
                        .debug_write_memory(0x10408, &[0x77]);
                    reference.debug_write_memory(0x10408, &[0x77]);
                    let before = runner.host().stats().translated_steps;
                    assert_eq!(runner.run(64).unwrap(), reference.advance(64));
                    assert!(runner.host().stats().translated_steps > before);
                    let actual = &runner.host().runtime().session;
                    assert_eq!(
                        actual.executed_tier_counts(),
                        reference.executed_tier_counts()
                    );
                    assert!(
                        actual.save_state().unwrap() == reference.save_state().unwrap(),
                        "changed label signatures changed guest state"
                    );
                    // Changed instruction bytes must fall back and become GREY.
                    runner
                        .host_mut()
                        .runtime_mut()
                        .session
                        .debug_write_memory(0x10400, &[0x48]);
                    reference.debug_write_memory(0x10400, &[0x48]);
                    let before = runner.host().stats().translated_steps;
                    assert_eq!(runner.run(64).unwrap(), reference.advance(64));
                    // A tier change splits the loop, so only then is the
                    // unchanged JMP its own block; one block falls back whole.
                    let jmp_block = if tiers[0] != tiers[1] { 32 } else { 0 };
                    assert_eq!(runner.host().stats().translated_steps - before, jmp_block);
                    let actual = &runner.host().runtime().session;
                    assert_eq!(
                        actual.executed_tier_counts(),
                        reference.executed_tier_counts()
                    );
                    assert!(
                        actual.save_state().unwrap() == reference.save_state().unwrap(),
                        "changed code must preserve tiered interpreter behavior"
                    );
                }
            }
        }
    }
}
