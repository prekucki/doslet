//! Transport-independent debugger access. No host sockets, clocks or file policy.
use super::{Session, SessionStatus};
use doslet_x86::{Access, Gpr32, SegmentRegister};

/// Execution settings saved on debugger attach and restored after detach.
pub struct ExecutionSettings {
    dynamic: bool,
    idle: bool,
    vga: bool,
    dpmi: Option<super::dpmi::Mode>,
    analysis: bool,
    adaptive_cpu: bool,
}

/// Owner/mapping/page identity for an immutable diagnostic code span.
#[derive(Clone, Copy)]
pub struct CodeVersion(doslet_bus::CodeCheck);

pub const GENERAL: [Gpr32; 8] = [
    Gpr32::Eax,
    Gpr32::Ecx,
    Gpr32::Edx,
    Gpr32::Ebx,
    Gpr32::Esp,
    Gpr32::Ebp,
    Gpr32::Esi,
    Gpr32::Edi,
];

impl Session {
    pub fn debug_code_version(&self, address: u32, length: u8) -> Option<CodeVersion> {
        use doslet_bus::MemoryBus;
        self.machine
            .bus
            .code_check(address, length)
            .map(CodeVersion)
    }

    pub fn debug_code_valid(&self, version: &CodeVersion) -> bool {
        use doslet_bus::MemoryBus;
        self.machine.bus.code_valid(&version.0)
    }

    /// Experimental bounded CPU execution with device time charged separately.
    /// The host adapter supplies elapsed time through `debug_advance_device_time`.
    /// Other execution policies must be disabled before using this interface.
    pub fn debug_external_advance(&mut self, budget: u32) -> u32 {
        if self.analyzer.enabled || self.menu_idle_enabled() || self.idle.enabled {
            return 0;
        }
        self.machine.debug_external_cpu_clock(true);
        let completed = self.advance(budget.min(64));
        self.machine.debug_external_cpu_clock(false);
        completed
    }

    pub fn debug_irq_pending(&self) -> bool {
        self.machine.interrupt_pending()
    }

    pub fn debug_irq_entries(&self) -> u64 {
        self.machine.interrupt_entries()
    }

    /// Diagnostic elapsed execution time, not a grant to skip guest work.
    pub fn debug_advance_device_time(&mut self, nanos: u64) -> Result<u64, super::RuntimeError> {
        Ok(self.machine.debug_advance_device_time(nanos)?)
    }

    /// Diagnostic only: this horizon is not permission to omit guest work.
    pub fn debug_service_horizon(&self) -> Option<u64> {
        self.machine.diagnostic_service_horizon().0
    }

    /// Explicit cooperative experiment, with at most 250 us and 256 device slices.
    /// Leaves the CPU at the same PC, stops for an eligible IRQ, and advances
    /// audio/timers normally. The caller must qualify a wait boundary; this is
    /// not an instruction-equivalent optimization or a host sleep operation.
    pub fn debug_cooperative_idle(&mut self, nanos: u64) -> Result<u64, super::RuntimeError> {
        if self.status != SessionStatus::Running
            || self.analyzer.enabled
            || self.menu_idle_enabled()
            || self.idle.enabled
        {
            return Ok(0);
        }
        let start = self.emulated_nanos();
        let end = start.saturating_add(nanos.min(250_000));
        for _ in 0..256 {
            let now = self.emulated_nanos();
            if now >= end {
                break;
            }
            let steps = self.machine.advance_cooperative_idle(100_000, end - now)?;
            if steps == 0 {
                break;
            }
            self.steps += u64::from(steps);
        }
        Ok(self.emulated_nanos() - start)
    }

    pub fn debug_execution_settings(&self) -> ExecutionSettings {
        ExecutionSettings {
            dynamic: self.dynamic_core_enabled(),
            idle: self.idle.enabled,
            vga: self.machine.vga_wait_acceleration_enabled(),
            dpmi: self.dpmi.as_ref().map(|h| h.mode),
            analysis: self.analyzer.enabled,
            adaptive_cpu: self.analyzer.controller,
        }
    }

    pub fn debug_disable_accelerations(&mut self) -> ExecutionSettings {
        let settings = self.debug_execution_settings();
        self.set_synchronization_analysis(false);
        self.set_dynamic_core(false);
        self.set_idle_loops(false);
        self.set_vga_wait_acceleration(false);
        if let Some(host) = &mut self.dpmi {
            host.mode = super::dpmi::Mode::Reflect;
        }
        settings
    }

    pub fn debug_restore_accelerations(&mut self, settings: ExecutionSettings) {
        self.set_synchronization_analysis(settings.analysis);
        if settings.adaptive_cpu {
            self.set_adaptive_cpu(true);
        }
        self.set_dynamic_core(settings.dynamic);
        self.set_idle_loops(settings.idle);
        self.set_vga_wait_acceleration(settings.vga);
        if let (Some(host), Some(mode)) = (&mut self.dpmi, settings.dpmi) {
            host.mode = mode;
        }
    }
    /// Bus-linear PC before the PC's A20 mapping. Architectural EIP remains
    /// available through `registers`; a debugger can display both explicitly.
    pub fn debug_pc(&self) -> u32 {
        let r = self.registers();
        r.segment_cache(SegmentRegister::Cs).base.wrapping_add(r.ip)
    }

    pub fn debug_read_memory(&self, address: u32, output: &mut [u8]) -> bool {
        if output.is_empty() {
            return true;
        }
        let Ok(length) = u32::try_from(output.len()) else {
            return false;
        };
        let Some(bytes) = self.machine.bus.debug_ram(address, length) else {
            return false;
        };
        output.copy_from_slice(bytes);
        true
    }

    pub fn debug_write_memory(&mut self, address: u32, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return true;
        }
        let Ok(length) = u32::try_from(bytes.len()) else {
            return false;
        };
        let Some(output) = self.machine.bus.debug_ram_mut(address, length) else {
            return false;
        };
        output.copy_from_slice(bytes);
        // A patch can invalidate an in-progress runtime proof independently of
        // the ordinary bus write log. Never reuse a proof across a debug edit.
        self.set_idle_loops(false);
        true
    }

    /// Update ordinary registers and a bus-linear PC atomically. Selectors and
    /// hidden descriptors are intentionally changed only by guest instructions.
    pub fn debug_write_core(&mut self, general: [u32; 8], pc: u32, eflags: u32) -> bool {
        let base = self.registers().segment_cache(SegmentRegister::Cs).base;
        let offset = pc.wrapping_sub(base);
        if self
            .registers()
            .address(SegmentRegister::Cs, offset, 1, Access::Execute)
            .is_err()
        {
            return false;
        }
        self.set_idle_loops(false);
        for (register, value) in GENERAL.into_iter().zip(general) {
            self.machine.cpu.registers.set32(register, value);
        }
        self.machine.cpu.registers.ip = offset;
        self.machine.cpu.flags = eflags | 2;
        true
    }

    /// Raw x87 state in wide protected-layout FSAVE format, without FNSAVE's
    /// reset or other guest-visible effects. Selectors/offsets stay explicit.
    pub fn debug_x87(&self) -> Option<[u8; 108]> {
        self.machine
            .cpu
            .fpu
            .as_ref()
            .map(|fpu| fpu.save_environment(true, true, true))
    }

    /// Suspend without releasing host input or changing guest keyboard RAM.
    pub fn debug_suspend(&mut self) {
        if self.status == SessionStatus::Running {
            self.status = SessionStatus::Paused;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_execution_is_bounded_and_restores_ordinary_clock() {
        let mut s = Session::new(&[0x40, 0xEB, 0xFD]).unwrap();
        s.debug_disable_accelerations();
        assert_eq!(s.debug_external_advance(1), 0);
        s.start();
        let before = s.emulated_nanos();
        let eax = s.registers().get32(Gpr32::Eax);
        assert_eq!(s.debug_external_advance(u32::MAX), 64);
        assert_eq!(s.emulated_nanos(), before);
        assert_eq!(s.registers().get32(Gpr32::Eax), eax + 32);
        assert_eq!(s.debug_advance_device_time(1000).unwrap(), 1000);
        assert_eq!(s.advance(1), 1);
        assert!(s.emulated_nanos() > before + 1000);
        s.set_adaptive_cpu(true);
        let cpu = s.machine.cpu.snapshot();
        assert_eq!(s.debug_external_advance(1), 0);
        assert!(s.machine.cpu.matches(&cpu));
    }

    #[test]
    fn explicit_cooperative_idle_preserves_cpu_and_stops_for_pending_irq() {
        let mut s = Session::new(&[0x90, 0xEB, 0xFD]).unwrap();
        s.start();
        s.debug_disable_accelerations();
        let before = s.machine.cpu.snapshot();
        let mhz = s.hardware_config().cpu_mhz;
        let elapsed = s.debug_cooperative_idle(u64::MAX).unwrap();
        assert!(elapsed > 0 && elapsed <= 250_000 + 4000 / u64::from(mhz));
        assert!(s.machine.cpu.matches(&before));
        assert_eq!(s.hardware_config().cpu_mhz, mhz);
        s.machine.bus.pic.raise_edge(0);
        let now = s.emulated_nanos();
        assert_eq!(s.debug_cooperative_idle(100_000).unwrap(), 0);
        assert_eq!(s.emulated_nanos(), now);
    }

    #[test]
    fn cooperative_debug_wait_refuses_paused_disabled_irq_and_competing_policy() {
        let mut s = Session::new(&[0x90]).unwrap();
        s.debug_disable_accelerations();
        assert_eq!(s.debug_cooperative_idle(100_000).unwrap(), 0);
        s.start();
        s.machine.cpu.flags &= !doslet_x86::flags::INTERRUPT;
        assert_eq!(s.debug_cooperative_idle(100_000).unwrap(), 0);
        s.machine.cpu.flags |= doslet_x86::flags::INTERRUPT;
        s.set_adaptive_cpu(true);
        assert_eq!(s.debug_cooperative_idle(100_000).unwrap(), 0);
    }

    #[test]
    fn reads_and_stops_do_not_advance_time_or_modify_input_or_guest_state() {
        let mut session = Session::new(&[0x40, 0xEB, 0xFD]).unwrap();
        session.start();
        session.advance(20);
        let before = session.machine.cpu.snapshot();
        let time = session.emulated_nanos();
        let keyboard = session.ram_view(0x400..0x500).unwrap().to_vec();
        let mut bytes = [0; 3];
        assert!(session.debug_read_memory(0x10100, &mut bytes));
        assert_eq!(bytes, [0x40, 0xEB, 0xFD]);
        session.debug_suspend();
        assert!(session.machine.cpu.snapshot() == before);
        assert_eq!(session.emulated_nanos(), time);
        assert_eq!(session.ram_view(0x400..0x500).unwrap(), keyboard);
    }

    #[test]
    fn rejected_spans_and_invalid_pc_have_no_partial_effects() {
        let mut session = Session::new(&[0x90, 0xEB, 0xFD]).unwrap();
        for address in [0x9FFFF, 0xA0000, 0xFFFF_FFFF, 0x1000000] {
            let mut bytes = [0xCC; 4];
            assert!(!session.debug_read_memory(address, &mut bytes));
            assert_eq!(bytes, [0xCC; 4]);
            assert!(!session.debug_write_memory(address, &[1, 2, 3, 4]));
        }
        let before = session.machine.cpu.snapshot();
        assert!(session.machine.bus.debug_ram(0, u32::MAX).is_none());
        assert!(session.machine.bus.debug_ram_mut(0, u32::MAX).is_none());
        assert!(!session.debug_write_core([42; 8], 0xFFFF_FFFF, 0xFFF));
        assert!(session.machine.cpu.snapshot() == before);
    }

    #[test]
    fn patch_rejects_vga_and_a20_discontinuity_but_maps_contiguous_aliases() {
        let mut session = Session::new(&[0x90]).unwrap();
        assert!(!session.a20_enabled());
        assert!(session.debug_write_memory(0x110100, &[0x40]));
        assert_eq!(session.ram_view(0x10100..0x10101).unwrap(), &[0x40]);
        assert!(!session.debug_write_memory(0xFFFFF, &[1, 2]));
        assert!(!session.debug_write_memory(0xAFFFF, &[1, 2]));
        assert!(!session.debug_write_memory(0x1A0000, &[1]));
    }

    #[test]
    fn code_patches_invalidate_warmed_blocks_and_record_writes() {
        let mut session = Session::new(&[0x40, 0xEB, 0xFD]).unwrap();
        session.set_dynamic_core(false);
        session.start();
        session.advance(100);
        session.debug_suspend();
        let proof = session.machine.begin_idle_proof();
        assert!(session.debug_write_memory(0x10100, &[0x48]));
        // Borrowed writes are also visible to the idle-proof write log.
        assert!(!session.machine.end_idle_proof(proof));
        let before = session.registers().get32(Gpr32::Eax);
        assert!(session.debug_write_core(
            GENERAL.map(|g| session.registers().get32(g)),
            0x10100,
            session.eflags()
        ));
        session.start();
        session.advance(1);
        assert_eq!(
            session.registers().get32(Gpr32::Eax),
            before.wrapping_sub(1) & 0xFFFF
        );
    }
}
