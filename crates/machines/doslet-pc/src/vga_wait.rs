//! Conservative structural polling-loop batching at the PC composition layer.
use crate::{MachineError, PcMachine};
use doslet_x86::{Access, Gpr16, SegmentCache, SegmentRegister, flags};

#[derive(Clone, Copy)]
pub(crate) struct Candidate {
    start: u32,
    cs: SegmentCache,
    cr0: u32,
    port: u16,
    jump: u8,
    phase: u8,
    confirmed: bool,
}
pub(crate) struct WaitState {
    pub enabled: bool,
    pub candidate: Option<Candidate>,
}
impl Default for WaitState {
    fn default() -> Self {
        Self {
            enabled: true,
            candidate: None,
        }
    }
}
impl WaitState {
    #[inline]
    pub(crate) fn ready(&self, budget: u32) -> bool {
        self.enabled && budget >= 3 && self.candidate.is_some_and(|c| c.confirmed && c.phase == 0)
    }
}
impl PcMachine {
    fn wait_code(&self, start: u32) -> Option<&[u8]> {
        // Require a contiguous ordinary-RAM instruction range. Wrapping code,
        // MMIO fetches and unusual A20 aliases fall back to the interpreter.
        if !self.cpu.registers.code32() && start > 0xFFFA {
            return None;
        }
        let linear = self
            .cpu
            .registers
            .address(SegmentRegister::Cs, start, 5, Access::Execute)
            .ok()? as usize;
        self.bus.ram_view(linear..linear.checked_add(5)?)
    }
    fn wait_allowed(&self) -> bool {
        self.vga_wait.enabled
            && !self.cpu.is_halted()
            && !self.cpu.has_interrupt_shadow()
            && self.cpu.flags & (flags::TRAP | flags::RESUME | flags::VIRTUAL) == 0
            && self.cpu.control.cr0 & 0x8000_0000 == 0
    }
    pub(crate) fn validate_vga_wait(&mut self) -> bool {
        let Some(candidate) = self.vga_wait.candidate else {
            return false;
        };
        let offset = [0, 1, 3][usize::from(candidate.phase)];
        let valid = self.wait_allowed()
            && self.cpu.registers.ip == candidate.start + offset
            && self.cpu.registers.segment_cache(SegmentRegister::Cs) == candidate.cs
            && self.cpu.control.cr0 == candidate.cr0
            && self.cpu.registers.get(Gpr16::Dx) == candidate.port
            && self.bus.vga.input_status_port() == candidate.port
            && self.wait_code(candidate.start) == Some(&[0xEC, 0x24, 8, candidate.jump, 0xFB]);
        if !valid {
            self.vga_wait.candidate = None;
        }
        valid
    }
    pub(crate) fn advance_vga_wait(&mut self, budget: u32) -> Result<Option<u32>, MachineError> {
        if budget < 3
            || !self
                .vga_wait
                .candidate
                .is_some_and(|c| c.confirmed && c.phase == 0)
        {
            return Ok(None);
        }
        if !self.validate_vga_wait()
            || (self.cpu.accepts_maskable_interrupt() && self.bus.pic.has_interrupt())
        {
            return Ok(None);
        }
        let candidate = self.vga_wait.candidate.expect("validated candidate");
        let masked = self
            .bus
            .vga
            .read_port(candidate.port)
            .expect("active status port")
            & 8;
        let taken = if candidate.jump == 0x74 {
            masked == 0
        } else {
            masked != 0
        };
        let arithmetic = flags::CARRY
            | flags::PARITY
            | flags::AUXILIARY
            | flags::ZERO
            | flags::SIGN
            | flags::OVERFLOW;
        let expected = if masked == 0 {
            flags::ZERO | flags::PARITY
        } else {
            0
        };
        if !taken
            || self.cpu.registers.get_byte(0) != masked
            || self.cpu.flags & arithmetic != expected
        {
            return Ok(None);
        }
        let deadline = self
            .bus
            .vga
            .retrace_deadline(candidate.port)
            .expect("active status port");
        // Never cross an event-producing step, even if it occurs within the
        // three-instruction iteration. IRQ entry must see the original IN/AND/
        // Jcc phase. At the boundary the next few instructions run normally.
        let event_step = self.steps_to_event_nanos(u32::MAX, deadline);
        let steps = budget.min(event_step.saturating_sub(1)) / 3 * 3;
        if steps == 0 {
            return Ok(None);
        }
        // A whole taken iteration restores exactly the same AL, flags and IP.
        // VGA status reads currently have no other architectural side effects.
        self.advance_hardware_steps(steps)?;
        #[cfg(feature = "profiling")]
        {
            self.bus.stats.cpu_steps += u64::from(steps);
            self.bus.stats.vga_wait_steps += u64::from(steps);
            self.bus.stats.vga_wait_batches += 1;
        }
        Ok(Some(steps))
    }
    #[inline(always)]
    pub(crate) fn observe_vga_wait(&mut self, valid: bool, interrupted: bool, status_read: bool) {
        if !self.vga_wait.enabled {
            return;
        }
        if !valid && !status_read {
            return;
        }
        self.observe_vga_candidate(valid, interrupted, status_read);
    }
    fn observe_vga_candidate(&mut self, valid: bool, interrupted: bool, status_read: bool) {
        if interrupted || !self.wait_allowed() {
            self.vga_wait.candidate = None;
            return;
        }
        if valid {
            let candidate = self
                .vga_wait
                .candidate
                .as_mut()
                .expect("validated candidate");
            let next = match candidate.phase {
                0 => candidate.start + 1,
                1 => candidate.start + 3,
                _ => candidate.start,
            };
            if self.cpu.registers.cs == candidate.cs.selector && self.cpu.registers.ip == next {
                candidate.phase += 1;
                if candidate.phase == 3 {
                    candidate.phase = 0;
                    candidate.confirmed = true;
                }
                return;
            }
            self.vga_wait.candidate = None;
        }
        // Discovery happens only after actual status I/O during this completed
        // instruction, never after a generic backward jump or stale host read.
        if !status_read {
            return;
        }
        let Some(start) = self.cpu.registers.ip.checked_sub(1) else {
            return;
        };
        let port = self.cpu.registers.get(Gpr16::Dx);
        if self.bus.vga.input_status_port() != port {
            return;
        }
        let Some(code) = self.wait_code(start) else {
            return;
        };
        if code[..3] != [0xEC, 0x24, 8] || !matches!(code[3], 0x74 | 0x75) || code[4] != 0xFB {
            return;
        }
        self.vga_wait.candidate = Some(Candidate {
            start,
            cs: self.cpu.registers.segment_cache(SegmentRegister::Cs),
            cr0: self.cpu.control.cr0,
            port,
            jump: code[3],
            phase: 1,
            confirmed: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doslet_bus::{MemoryBus, PortBus};
    use doslet_x86::Gpr32;

    fn machine(wide: bool, jump: u8, dma: bool) -> PcMachine {
        let mut machine = crate::timing::tests::idle_machine(dma);
        for (i, byte) in [
            0x50, 0x52, 0xBA, 0x2F, 2, 0xEC, 0xB0, 0x20, 0xE6, 0x20, 0x5A, 0x58, 0xCF,
        ]
        .into_iter()
        .enumerate()
        {
            machine.bus.write_u8(0x200 + i as u32, byte).unwrap();
        }
        let start = if wide {
            // Enter genuine 32-bit protected code with EIP above 64 KiB.
            let base = 0x20000u32;
            let descriptor = [
                0xFF,
                0xFF,
                base as u8,
                (base >> 8) as u8,
                (base >> 16) as u8,
                0x9A,
                0x41,
                0,
            ];
            for (i, value) in descriptor.into_iter().enumerate() {
                machine.bus.write_u8(0x818 + i as u32, value).unwrap();
            }
            machine.cpu.control.gdtr.base = 0x800;
            machine.cpu.control.gdtr.limit = 0x7F;
            let bootstrap = [
                0xB8, 0x15, 0, 0x0F, 0x01, 0xF0, 0x90, 0x66, 0xEA, 0x20, 0, 1, 0, 0x18, 0,
            ];
            for (i, value) in bootstrap.into_iter().enumerate() {
                machine.bus.write_u8(i as u32, value).unwrap();
            }
            for _ in 0..4 {
                machine.cpu.step(&mut machine.bus).unwrap();
            }
            // IRQs in this fixture use real mode only; wide-code tests retain
            // PIC requests with IF clear rather than invent a protected IDT.
            machine.cpu.flags = 2;
            0x30020
        } else {
            machine.cpu.registers.ip = 0x1000;
            0x1000
        };
        for (i, value) in [
            0xEC,
            0x24,
            8,
            jump,
            0xFB,
            0xEC,
            0x24,
            8,
            if jump == 0x74 { 0x75 } else { 0x74 },
            0xFB,
            0xEB,
            0xF4,
        ]
        .into_iter()
        .enumerate()
        {
            machine.bus.write_u8(start + i as u32, value).unwrap();
        }
        machine.cpu.registers.set(Gpr16::Dx, 0x3DA);
        machine.cpu.registers.set32(Gpr32::Eax, 0xAABB_CC55);
        machine.bus.vga.set_vbe_101h(false);
        if jump == 0x75 {
            machine.bus.vga.advance_pixel_ticks(800 * 490 + 32);
        }
        machine
    }
    fn compare(fast: &mut PcMachine, reference: &mut PcMachine) {
        assert_eq!(fast.time(), reference.time());
        assert_eq!(fast.cpu.registers, reference.cpu.registers);
        assert_eq!(fast.cpu.flags, reference.cpu.flags);
        assert_eq!(fast.cpu.control.cr0, reference.cpu.control.cr0);
        assert_eq!(fast.cpu.control.cr2, reference.cpu.control.cr2);
        assert_eq!(fast.cpu.control.cr3, reference.cpu.control.cr3);
        assert_eq!(fast.cpu.control.gdtr, reference.cpu.control.gdtr);
        assert_eq!(fast.cpu.control.idtr, reference.cpu.control.idtr);
        assert_eq!(fast.cpu.control.ldtr, reference.cpu.control.ldtr);
        assert_eq!(fast.cpu.control.task, reference.cpu.control.task);
        assert_eq!(
            fast.cpu.has_interrupt_shadow(),
            reference.cpu.has_interrupt_shadow()
        );
        assert_eq!(
            fast.bus.pic.read_command(),
            reference.bus.pic.read_command()
        );
        assert_eq!(
            fast.bus.vga.read_port(0x3DA),
            reference.bus.vga.read_port(0x3DA)
        );
        assert_eq!(
            fast.bus.vga.retrace_deadline(0x3DA),
            reference.bus.vga.retrace_deadline(0x3DA)
        );
        assert_eq!(fast.bus.dsp.irq_status(), reference.bus.dsp.irq_status());
        for _ in 0..2 {
            assert_eq!(
                fast.bus.pit.read_counter(0),
                reference.bus.pit.read_counter(0)
            );
        }
        let mut a = [[0; 2]; 4096];
        let mut b = [[0; 2]; 4096];
        assert_eq!(fast.drain_pcm(&mut a), reference.drain_pcm(&mut b));
        assert_eq!(a, b);
        assert_eq!(
            fast.bus.read_port_u8(0xD0).unwrap(),
            reference.bus.read_port_u8(0xD0).unwrap()
        );
    }
    fn advance(machine: &mut PcMachine, steps: u32) -> u32 {
        let mut remaining = steps;
        let mut dispatches = 0;
        while remaining > 0 {
            let (_, consumed) = machine.step_with_idle_budget(remaining).unwrap();
            assert!(consumed > 0 && consumed <= remaining);
            remaining -= consumed;
            dispatches += 1;
        }
        dispatches
    }
    #[test]
    fn whole_iterations_match_reference_across_partial_budgets_edges_irqs_and_pcm() {
        for wide in [false, true] {
            for jump in [0x74, 0x75] {
                for dma in [false, true] {
                    let mut fast = machine(wide, jump, dma);
                    let mut reference = machine(wide, jump, dma);
                    reference.set_vga_wait_acceleration(false);
                    let mut dispatched = 0;
                    let mut total = 0;
                    for i in 0..100 {
                        let steps = [1, 2, 3, 7, 100, 10000][i % 6];
                        dispatched += advance(&mut fast, steps);
                        advance(&mut reference, steps);
                        total += steps;
                        compare(&mut fast, &mut reference);
                        if i == 30 {
                            fast.bus.vga.set_mode_13h();
                            reference.bus.vga.set_mode_13h();
                        }
                        if i == 60 {
                            fast.bus.vga.set_vbe_101h(true);
                            reference.bus.vga.set_vbe_101h(true);
                        }
                    }
                    assert!(
                        dispatched < total * 3 / 4,
                        "wide={wide} jump={jump:X} dma={dma}: {dispatched}/{total}"
                    );
                }
            }
        }
    }
    #[test]
    fn pending_irq_at_each_instruction_phase_is_delivered_without_batching() {
        for phase in 0..3 {
            for jump in [0x74, 0x75] {
                let mut fast = machine(false, jump, false);
                let mut reference = machine(false, jump, false);
                reference.set_vga_wait_acceleration(false);
                advance(&mut fast, 6 + phase);
                advance(&mut reference, 6 + phase);
                fast.bus.pic.raise_edge(0);
                reference.bus.pic.raise_edge(0);
                assert_eq!(fast.step_with_idle_budget(10000).unwrap().1, 1);
                reference.step().unwrap();
                compare(&mut fast, &mut reference);
                advance(&mut fast, 2000);
                advance(&mut reference, 2000);
                compare(&mut fast, &mut reference);
            }
        }
    }
    #[test]
    fn changed_state_code_port_segment_and_debugging_force_normal_execution() {
        for alteration in 0..8 {
            let mut fast = machine(false, 0x74, false);
            let mut reference = machine(false, 0x74, false);
            reference.set_vga_wait_acceleration(false);
            advance(&mut fast, 6);
            advance(&mut reference, 6);
            assert!(fast.vga_wait.candidate.is_some_and(|c| c.confirmed));
            for machine in [&mut fast, &mut reference] {
                match alteration {
                    0 => machine.cpu.registers.set_byte(0, 0xFF),
                    1 => machine.cpu.flags |= flags::CARRY,
                    2 => machine.cpu.flags |= flags::TRAP,
                    3 => {
                        machine.bus.write_u8(0x1002, 1).unwrap();
                    }
                    4 => {
                        machine.cpu.registers.set(Gpr16::Dx, 0x3BA);
                    }
                    5 => {
                        machine.bus.write_port_u8(0x3C2, 0).unwrap();
                    }
                    6 => {
                        machine.cpu.registers.cs = 0x10;
                    }
                    _ => {
                        machine.bus.set_a20(true);
                        machine.cpu.registers.set32(Gpr32::Eax, 0x1234_5600);
                    }
                }
            }
            // Inactive status reads fault equally; do not turn an unsupported
            // port into a wait. The last case changes only preserved high AL.
            let result = fast.step_with_idle_budget(10000);
            if alteration == 7 {
                let consumed = result.unwrap().1;
                advance(&mut reference, consumed);
            } else {
                assert_eq!(
                    result.map(|(outcome, units)| {
                        assert_eq!(units, 1);
                        outcome
                    }),
                    reference.step()
                );
            }
            compare(&mut fast, &mut reference);
        }
    }
    #[test]
    fn segment_end_and_interrupt_shadow_do_not_get_batched() {
        let mut machine = machine(false, 0x74, false);
        advance(&mut machine, 6);
        // STI creates its own next-boundary inhibition even with IF set.
        machine.bus.write_u8(0x3000, 0xFB).unwrap();
        machine.cpu.registers.ip = 0x3000;
        machine.cpu.step(&mut machine.bus).unwrap();
        machine.cpu.registers.ip = 0x1000;
        assert!(machine.cpu.has_interrupt_shadow());
        assert_eq!(machine.step_with_idle_budget(10000).unwrap().1, 1);
        assert!(!machine.cpu.has_interrupt_shadow());
        machine.cpu.registers.ip = 0xFFFE;
        assert!(machine.wait_code(0xFFFE).is_none());
        assert!(machine.wait_code(0xFFFC).is_none());
    }
    #[test]
    fn discovery_comes_from_current_instruction_status_io_not_stale_host_reads() {
        let mut machine = machine(false, 0x74, false);
        machine.cpu.flags = 2;
        // Jump directly to AND after an unrelated host-side status inspection.
        for (i, byte) in [0xE9, 0xFE, 0xDF].into_iter().enumerate() {
            machine.bus.write_u8(0x3000 + i as u32, byte).unwrap();
        }
        machine.cpu.registers.ip = 0x3000;
        machine.bus.read_port_u8(0x3DA).unwrap();
        machine.step().unwrap();
        assert_eq!(machine.cpu.registers.ip, 0x1001);
        assert!(machine.vga_wait.candidate.is_none());
        // Arrange a taken JZ. A backward branch alone must not discover it.
        machine.cpu.registers.set_byte(0, 0);
        machine.step().unwrap();
        machine.step().unwrap();
        assert_eq!(machine.cpu.registers.ip, 0x1000);
        assert!(machine.vga_wait.candidate.is_none());
        machine.step().unwrap(); // actual IN triggers discovery
        assert!(
            machine
                .vga_wait
                .candidate
                .is_some_and(|c| c.phase == 1 && !c.confirmed)
        );
        machine.step().unwrap();
        machine.step().unwrap();
        assert!(
            machine
                .vga_wait
                .candidate
                .is_some_and(|c| c.phase == 0 && c.confirmed)
        );
        assert!(machine.step_with_idle_budget(10000).unwrap().1 > 1);
        machine.set_vga_wait_acceleration(false);
        let generation = machine.bus.vga_status_read_generation;
        machine.bus.read_port_u8(0x3DA).unwrap();
        assert_eq!(machine.bus.vga_status_read_generation, generation);
    }
    #[test]
    fn status_notification_is_active_port_only_and_wraps_without_losing_discovery() {
        let mut machine = machine(false, 0x74, false);
        let generation = machine.bus.vga_status_read_generation;
        machine.bus.read_port_u8(0x3CC).unwrap();
        assert_eq!(machine.bus.vga_status_read_generation, generation);
        assert!(machine.bus.read_port_u8(0x3BA).is_err());
        assert_eq!(machine.bus.vga_status_read_generation, generation);
        machine.bus.vga_status_read_generation = u32::MAX;
        machine.step().unwrap();
        assert_eq!(machine.bus.vga_status_read_generation, 0);
        assert!(
            machine
                .vga_wait
                .candidate
                .is_some_and(|c| c.phase == 1 && !c.confirmed)
        );
        machine.bus.write_port_u8(0x3C2, 0).unwrap();
        machine.bus.read_port_u8(0x3BA).unwrap();
        assert_eq!(machine.bus.vga_status_read_generation, 1);
        assert!(machine.bus.read_port_u8(0x3DA).is_err());
        assert_eq!(machine.bus.vga_status_read_generation, 1);
        machine.set_vga_wait_acceleration(false);
        machine.bus.read_port_u8(0x3BA).unwrap();
        assert_eq!(machine.bus.vga_status_read_generation, 1);
    }
}
