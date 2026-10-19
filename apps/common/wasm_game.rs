//! Shared diagnostic Session adapter for native and WASM comparisons.
use doslet_runtime::{Session, SessionStatus};
use doslet_x86::Cpu;
use doslet_x86_wasm::host::{CodeCheck, Runtime};

pub struct Game {
    pub session: Session,
}
pub fn setup(s: &mut Session) {
    s.set_dynamic_core(false);
    s.set_decode_cache(false);
    s.set_idle_loops(false);
    s.set_vga_wait_acceleration(false);
    s.set_synchronization_analysis(false);
    s.set_adaptive_idle(false);
    s.set_idle_observation(false);
    s.start();
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
        self.session.status() == SessionStatus::Running && !self.session.storage_waiting()
    }
    fn prepare(&mut self) -> Result<(), String> {
        self.session.debug_prepare_register_engine();
        Ok(())
    }
    fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
        self.session.debug_code_bytes(address, length)
    }
    fn translated_budget(&self, remaining: u32) -> u32 {
        self.session.debug_register_engine_budget(remaining)
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
