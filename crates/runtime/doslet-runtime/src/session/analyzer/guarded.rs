//! Returning guards are source evidence, never a complete work/wait episode.
use super::*;

const GUARDS: usize = 2;
const IRQ_STEPS: u32 = 4096;

#[derive(Clone, Copy, Debug, Default)]
pub struct GuardStats {
    pub head: u32,
    pub operand: u32,
    pub caller: u32,
    pub caller_backedges: u64,
    pub reads: u64,
    pub last_value: u32,
    pub writes: u64,
    pub partial_writes: u64,
    pub writer_pc: Option<u32>,
    pub writer_irq: Option<u8>,
    pub writer_value: Option<u32>,
    pub writer_nanos: Option<u64>,
    pub observed_decrements: u64,
    /// Interval between observed decrements; intervening IRQs may be unobserved.
    pub observed_interval_nanos: Option<u64>,
    pub reason: &'static str,
}

#[derive(Clone, Copy)]
struct Guard {
    stats: GuardStats,
    width: u8,
    context: Context,
    code: [CodeCheck; 3],
    last_read: Option<(u32, u8)>,
    last_writer: Option<(u32, u8, u64)>,
}

#[derive(Default)]
pub(super) struct GuardProbe {
    guards: [Option<Guard>; GUARDS],
    irq: Option<(u8, u16, u32, u32)>,
    remaining: u32,
    pub irq_steps: u64,
    pub interrupted_traces: u64,
}
impl GuardProbe {
    pub(super) fn recurring_source(
        &self,
        head: u32,
        caller: u32,
        context: Context,
        bus: &doslet_pc::PcBus,
        now: u64,
    ) -> Option<(Predicate, GuardStats)> {
        let g = self.guards.iter().flatten().find(|g| {
            g.stats.head == head
                && g.stats.caller == caller
                && g.context == context
                && g.code.iter().all(|code| bus.code_valid(code))
                && g.stats.observed_decrements >= 4
                && g.last_writer
                    .is_some_and(|(_, _, time)| now.saturating_sub(time) <= FRESH)
        })?;
        let mut stats = g.stats;
        let (pc, irq, time) = g.last_writer?;
        if stats.writer_nanos != Some(time) {
            stats.writer_value = None;
        }
        stats.writer_pc = Some(pc);
        stats.writer_irq = Some(irq);
        stats.writer_nanos = Some(time);
        Some((
            Predicate {
                head,
                branch: head,
                end: head,
                operand: g.stats.operand,
                width: g.width,
                port: false,
                version: g.code[0],
            },
            stats,
        ))
    }
    pub fn stats(&self) -> [GuardStats; GUARDS] {
        self.guards
            .map(|g| g.map_or(GuardStats::default(), |g| g.stats))
    }
    pub(super) fn stop(&mut self) {
        self.irq = None;
        self.remaining = 0;
        for g in self.guards.iter_mut().flatten() {
            g.last_read = None;
        }
    }
    fn observe(&mut self, observation: &doslet_pc::StepObservation, pc: u32, now: u64) {
        let irq = self.irq.map(|i| i.0);
        for event in observation.trace.events[..observation.trace.len]
            .iter()
            .flatten()
        {
            for g in self.guards.iter_mut().flatten() {
                if event.address == g.stats.operand
                    && event.width == g.width
                    && event.kind == Kind::Read
                {
                    g.stats.reads += 1;
                    g.stats.last_value = event.value;
                    g.last_read = irq.map(|irq| (event.value, irq));
                }
                if event.kind != Kind::Write
                    || u64::from(event.address) >= u64::from(g.stats.operand) + u64::from(g.width)
                    || u64::from(g.stats.operand)
                        >= u64::from(event.address) + u64::from(event.width)
                {
                    continue;
                }
                g.stats.writes += 1;
                g.stats.writer_pc = observation.irq.is_none().then_some(pc);
                g.stats.writer_irq = irq;
                g.stats.writer_value = Some(event.value);
                g.stats.writer_nanos = Some(now);
                if event.address != g.stats.operand || event.width != g.width {
                    g.stats.partial_writes += 1;
                    g.last_read = None;
                    g.last_writer = None;
                    g.stats.reason = "partial watched write; caller budget unavailable";
                    continue;
                }
                if let (Some(irq), Some((value, read_irq))) = (irq, g.last_read.take())
                    && observation.irq.is_none()
                    && irq == read_irq
                    && value > 0
                    && event.value == value - 1
                {
                    g.stats.observed_decrements += 1;
                    if let Some((writer, previous_irq, time)) = g.last_writer
                        && writer == pc
                        && previous_irq == irq
                    {
                        g.stats.observed_interval_nanos = Some(now.saturating_sub(time));
                    }
                    g.last_writer = Some((pc, irq, now));
                    g.stats.reason = "observed IRQ decrement; caller work/wait coverage incomplete";
                }
            }
        }
    }
}

impl Session {
    /// Admit only an absolute CMP-zero / forward JNE-to-RET guard, called
    /// directly with its return address still at the top of a flat stack.
    pub(super) fn analyzer_guard_discover(
        &mut self,
        observation: &doslet_pc::StepObservation,
        pc: u32,
        context: Option<Context>,
        sp: u32,
    ) {
        let Some(context) = context else {
            return;
        };
        if observation.irq.is_some() || observation.trace.incomplete {
            return;
        }
        let Some(bytes) = self.machine.bus.code_bytes(pc, 16) else {
            return;
        };
        let Some(cmp) = doslet_x86::analysis::decode(bytes, pc) else {
            return;
        };
        let Effect::Compare {
            left: Operand::Absolute { segment, address },
            right: Operand::Immediate(0),
            test: false,
        } = cmp.effect
        else {
            return;
        };
        if self.registers().segment_cache(segment).base != 0
            || !self.machine.bus.is_plain_ram(address, cmp.width)
        {
            return;
        }
        let Some(read) = observation.trace.events[..observation.trace.len]
            .iter()
            .flatten()
            .find(|e| e.kind == Kind::Read && e.address == address && e.width == cmp.width)
        else {
            return;
        };
        let branch = pc + u32::from(cmp.length);
        let Some(jump) = doslet_x86::analysis::decode(&bytes[usize::from(cmp.length)..], branch)
        else {
            return;
        };
        let Flow::Branch {
            condition: Some(5),
            target,
        } = jump.flow
        else {
            return;
        };
        if target <= branch
            || target - branch > 256
            || self.machine.bus.code_bytes(target, 1) != Some(&[0xc3][..])
        {
            return;
        }
        let Some(stack) = self.machine.bus.debug_ram(sp, 4) else {
            return;
        };
        let return_pc = u32::from_le_bytes(stack.try_into().unwrap());
        let Some(caller) = return_pc.checked_sub(5) else {
            return;
        };
        let Some(call) = self
            .machine
            .bus
            .code_bytes(caller, 5)
            .and_then(|b| doslet_x86::analysis::decode(b, caller))
        else {
            return;
        };
        if call.length != 5 || call.flow != Flow::Call(pc) {
            return;
        }
        let Some(code) = self.machine.bus.code_check(pc, cmp.length + jump.length) else {
            return;
        };
        let Some(ret_code) = self.machine.bus.code_check(target, 1) else {
            return;
        };
        let Some(call_code) = self.machine.bus.code_check(caller, 5) else {
            return;
        };
        if read.value != 0 {
            self.analyzer_caller_begin(pc, caller, context, sp);
        }
        if self
            .analyzer
            .guards
            .guards
            .iter()
            .flatten()
            .any(|g| g.stats.head == pc && g.stats.caller == caller && g.context == context)
        {
            return;
        }
        if let Some(slot) = self
            .analyzer
            .guards
            .guards
            .iter_mut()
            .find(|g| g.is_none_or(|g| g.stats.head == pc && g.stats.caller == caller))
        {
            *slot = Some(Guard {
                stats: GuardStats {
                    head: pc,
                    operand: address,
                    caller,
                    reads: 1,
                    last_value: read.value,
                    reason: "returning guard; IRQ source and caller work/wait coverage required",
                    ..Default::default()
                },
                width: cmp.width,
                context,
                code: [code, ret_code, call_code],
                last_read: None,
                last_writer: None,
            });
        } else {
            self.analyzer.stats.dropped_candidates += 1;
        }
    }
    pub(super) fn analyzer_guard_trace(
        &mut self,
        observation: &doslet_pc::StepObservation,
        pc: u32,
        now: u64,
    ) {
        if observation.trace.incomplete
            || observation.service_writes_unobserved
            || observation.service_device_accesses_unobserved
            || observation.device_writes_unobserved
        {
            self.analyzer.guards.interrupted_traces +=
                u64::from(self.analyzer.guards.irq.is_some());
            self.analyzer.guards.stop();
            return;
        }
        self.analyzer.guards.observe(observation, pc, now);
        if self.analyzer.guards.irq.is_none()
            && observation.irq.is_none()
            && let Some(d) = self
                .machine
                .bus
                .code_bytes(pc, 15)
                .and_then(|b| doslet_x86::analysis::decode(b, pc))
        {
            let next_pc = self.registers().ip;
            for g in self.analyzer.guards.guards.iter_mut().flatten() {
                if matches!(d.flow, Flow::Branch { target, .. } if target == g.stats.caller && target < pc)
                    && next_pc == g.stats.caller
                {
                    g.stats.caller_backedges += 1;
                }
            }
        }
    }
    /// Only handlers are stepped continuously, with a hard bound. Foreground
    /// JIT batches remain unchanged; no writer attribution crosses a lost IRQ.
    pub(super) fn analyzer_guard_irq(&mut self) -> Result<Option<u32>, RuntimeError> {
        for g in &mut self.analyzer.guards.guards {
            if g.as_ref()
                .is_some_and(|g| g.code.iter().any(|code| !self.machine.bus.code_valid(code)))
            {
                *g = None;
            }
        }
        if self.analyzer.guards.guards.iter().all(Option::is_none) {
            self.analyzer.guards.stop();
            return Ok(None);
        }
        if self.analyzer.guards.irq.is_none()
            && !(self.machine.interrupt_pending() && self.machine.cpu.accepts_maskable_interrupt())
        {
            return Ok(None);
        }
        let r = self.registers();
        let (ip, pc, cs, sp) = (r.ip, self.debug_pc(), r.cs, r.get32(Gpr32::Esp));
        // IRQs also arrive while the caller is inside a 16-bit BIOS wrapper.
        // The source is attributed from actual bus addresses and the delivered
        // vector, independently of the interrupted foreground's code context.
        // The guard's mapping/code checks above and exact IRET target below
        // remain mandatory; nested interrupts still terminate attribution.
        let return_instruction = self.machine.bus.code_bytes(pc, 1) == Some(&[0xcf][..])
            || self.machine.bus.code_bytes(pc, 2) == Some(&[0x66, 0xcf][..]);
        if self.analyzer.guards.irq.is_none() {
            // Do not let a VGA iteration span the independently traced handler:
            // IRQ service time is work, never confirmed port waiting.
            self.analyzer_suspend_vga_boundary(cs, ip, sp);
            self.idle_abandon();
            self.analyzer
                .gap("IRQ source observation outside foreground trace");
        }
        let before = self.emulated_nanos();
        let mut observation = doslet_pc::StepObservation::default();
        let used = self.analyzer_observe_step(&mut observation, false)?;
        self.analyzer.guards.irq_steps += u64::from(used);
        if let Some(vector) = observation.irq {
            if self.analyzer.guards.irq.is_some() {
                self.analyzer.guards.interrupted_traces += 1;
                self.analyzer.guards.stop();
                return Ok(Some(used));
            }
            self.analyzer.guards.irq = Some((vector, cs, ip, sp));
            self.analyzer.guards.remaining = IRQ_STEPS;
        }
        self.analyzer_guard_trace(&observation, pc, before);
        if let Some((_, return_cs, return_pc, return_sp)) = self.analyzer.guards.irq {
            self.analyzer.guards.remaining = self.analyzer.guards.remaining.saturating_sub(1);
            let returned = return_instruction
                && observation.irq.is_none()
                && (
                    self.registers().cs,
                    self.registers().ip,
                    self.registers().get32(Gpr32::Esp),
                ) == (return_cs, return_pc, return_sp);
            // A redirected IRET ends this attribution scope too. Retaining
            // the old IRQ until its instruction budget expires would label
            // foreground writes as interrupt effects and hide its VGA polls.
            if return_instruction && observation.irq.is_none()
                || self.analyzer.guards.remaining == 0
            {
                self.analyzer.guards.interrupted_traces += u64::from(!returned);
                self.analyzer.guards.stop();
                // A protected-mode bridge can use an intermediate IRET before
                // the exact foreground return. Keep its separately bounded
                // suspended VGA condition; only that exact return resumes it.
            }
        }
        Ok(Some(used))
    }
}

#[cfg(test)]
pub(in crate::session::analyzer) mod tests {
    use super::*;
    use doslet_x86::flags;

    pub(in crate::session::analyzer) fn session() -> Session {
        let mut s = super::super::super::cooperative::tests::session();
        s.set_menu_idle_site(None);
        s.set_idle_loops(false);
        s.set_vga_wait_acceleration(false);
        s.machine.cpu.flags = 2;
        // CALL guard; JMP caller. The guard returns while its counter is nonzero.
        let code = [0xe8, 11, 0, 0, 0, 0xeb, 0xf9];
        for (i, byte) in code.into_iter().enumerate() {
            s.machine.bus.write_u8(0x10000 + i as u32, byte).unwrap();
        }
        for (i, byte) in [0x83, 0x3d, 0, 0x30, 0, 0, 0, 0x75, 1, 0x90, 0xc3]
            .into_iter()
            .enumerate()
        {
            s.machine.bus.write_u8(0x10010 + i as u32, byte).unwrap();
        }
        s.machine.bus.write_u32(0x3000, 100).unwrap();
        s.machine.cpu.registers.ip = 0x10000;
        s.machine.cpu.control.idtr.base = 0x8000;
        s.machine.cpu.control.idtr.limit = 0x7ff;
        for (i, b) in [0, 1, 0x18, 0, 0, 0x8e, 1, 0].into_iter().enumerate() {
            s.machine.bus.write_u8(0x8040 + i as u32, b).unwrap();
        }
        // PUSH, separate load/decrement/store, POP and IRETD. PUSH/POP are
        // outside the static effect inventory but all bus effects are observed.
        for (i, b) in [
            0x50, 0xa1, 0, 0x30, 0, 0, 0x48, 0xa3, 0, 0x30, 0, 0, 0x58, 0xcf,
        ]
        .into_iter()
        .enumerate()
        {
            s.machine.bus.write_u8(0x10100 + i as u32, b).unwrap();
        }
        s.set_synchronization_analysis(true);
        s.start();
        s.advance(20);
        s
    }
    #[test]
    fn returning_guard_links_caller_and_actual_irq_store_without_budget() {
        let mut s = session();
        let mut reference = session();
        reference.set_synchronization_analysis(false);
        let g = s.synchronization_analysis().guards[0];
        assert_eq!((g.head, g.operand, g.caller), (0x10010, 0x3000, 0x10000));
        assert!(g.caller_backedges > 0);
        // Force foreground scout to end. IRQ source observation is independent.
        s.analyzer.remaining = 0;
        s.analyzer.next = u64::MAX;
        let allocated = allocation_counter::measure(|| {
            for _ in 0..4 {
                s.machine.cpu.flags |= flags::INTERRUPT;
                reference.machine.cpu.flags |= flags::INTERRUPT;
                s.machine.bus.pic.raise_edge(0);
                reference.machine.bus.pic.raise_edge(0);
                s.advance(8);
                reference.advance(8);
                assert!(s.machine.cpu.matches(&reference.machine.cpu.snapshot()));
                assert_eq!(s.emulated_nanos(), reference.emulated_nanos());
                assert_eq!(
                    s.machine.bus.ram_view(0..0xa0000),
                    reference.machine.bus.ram_view(0..0xa0000)
                );
                s.machine.bus.pic.write_command(0x20);
                reference.machine.bus.pic.write_command(0x20);
            }
        });
        assert_eq!(allocated.count_total, 0);
        assert_eq!(s.fault(), None);
        let stats = s.synchronization_analysis();
        let g = stats.guards[0];
        assert_eq!(g.writer_pc, Some(0x10107));
        assert_eq!(g.writer_irq, Some(8));
        assert_eq!(g.observed_decrements, 4);
        assert!(g.observed_interval_nanos.is_some());
        assert!(stats.candidates.iter().all(|c| c.predicted_mhz.is_none()));
        s.set_synchronization_analysis(false);
        assert!(
            s.synchronization_analysis()
                .guards
                .iter()
                .all(|g| g.head == 0)
        );
    }
    #[test]
    fn mutation_invalidates_guard_and_irq_trace_has_a_hard_bound() {
        let mut s = session();
        // A handler that never returns must not keep instruction tracing on.
        s.machine.bus.write_u8(0x10100, 0xeb).unwrap();
        s.machine.bus.write_u8(0x10101, 0xfe).unwrap();
        // The handler shares a code-generation page with the guard. Rediscover
        // after this deliberate code change before exercising the IRQ bound.
        s.set_synchronization_analysis(true);
        s.advance(20);
        s.analyzer.remaining = 0;
        s.analyzer.next = u64::MAX;
        s.machine.cpu.flags |= flags::INTERRUPT;
        s.machine.bus.pic.raise_edge(0);
        s.advance(IRQ_STEPS + 100);
        assert_eq!(
            s.synchronization_analysis().guard_irq_steps,
            u64::from(IRQ_STEPS)
        );
        assert_eq!(s.synchronization_analysis().guard_interrupted_traces, 1);
        s.machine.bus.write_u8(0x10010, 0x90).unwrap();
        s.advance(1);
        assert_eq!(s.synchronization_analysis().guards[0].head, 0);
    }

    #[test]
    fn redirected_iret_ends_irq_attribution_before_foreground_work() {
        let mut s = session();
        for (i, b) in [0xff, 0x0d, 0, 0x30, 0, 0, 0xeb, 0xfe]
            .into_iter()
            .enumerate()
        {
            s.machine.bus.write_u8(0x12000 + i as u32, b).unwrap();
        }
        s.analyzer.remaining = 0;
        s.analyzer.next = u64::MAX;
        s.machine.cpu.flags |= flags::INTERRUPT;
        s.machine.bus.pic.raise_edge(0);
        s.advance(1);
        assert!(s.analyzer.guards.irq.is_some());
        // An interrupt handler may redirect its return frame. After IRET the
        // old IRQ identity must not leak into that continuation's RAM writes.
        let sp = s.registers().get32(Gpr32::Esp);
        s.machine.bus.write_u32(sp, 0x12000).unwrap();
        s.advance(6);
        assert_eq!(s.registers().ip, 0x12000);
        assert_eq!(s.analyzer.guards.irq, None);
        assert_eq!(s.analyzer.guards.interrupted_traces, 1);
        s.advance(1);
        assert_eq!(s.machine.bus.read_u32(0x3000).unwrap(), 98);
        assert_eq!(
            s.synchronization_analysis().guards[0].observed_decrements,
            1
        );
    }

    #[test]
    fn irq_source_is_observed_when_flat_caller_has_entered_real_mode() {
        let mut s = session();
        let mut reference = session();
        reference.set_synchronization_analysis(false);
        for s in [&mut s, &mut reference] {
            s.machine.cpu = doslet_x86::Cpu::default();
            s.machine
                .cpu
                .registers
                .set(super::super::super::Gpr16::Sp, 0x8000);
            s.machine.cpu.flags = 0x202;
            s.machine.bus.write_u32(0x20, 0x1000).unwrap();
            // Real-mode DEC dword [3000h]; IRET. Source attribution comes from
            // observed read/modify/write effects, not a flat instruction graph.
            for (i, b) in [0x66, 0xff, 0x0e, 0, 0x30, 0xcf].into_iter().enumerate() {
                s.machine.bus.write_u8(0x1000 + i as u32, b).unwrap();
            }
            s.machine.bus.pic.raise_edge(0);
            s.advance(3);
        }
        assert_eq!(s.fault(), None);
        assert!(s.machine.cpu.matches(&reference.machine.cpu.snapshot()));
        assert_eq!(s.emulated_nanos(), reference.emulated_nanos());
        let g = s.synchronization_analysis().guards[0];
        assert_eq!(g.observed_decrements, 1);
        assert_eq!(g.writer_pc, Some(0x1000));
        assert_eq!(g.writer_irq, Some(8));
        assert_eq!(s.analyzer.guards.irq, None);
    }
}
