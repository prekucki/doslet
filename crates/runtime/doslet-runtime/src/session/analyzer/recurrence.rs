//! Actual recurring intervals, without instruction tracing or execution skipping.
use super::*;
mod backedge;

const INTERVAL_LIMIT: u64 = 5_000_000;

#[derive(Clone, Copy, Debug, Default)]
pub struct RecurrenceStats {
    /// A dynamically observed backedge, rather than a returning IRQ guard.
    pub backedge: bool,
    pub probe_rejections: u64,
    pub last_probe_rejection_reason: &'static str,
    pub head: u32,
    pub caller: u32,
    pub intervals: u64,
    pub unchanged: u64,
    pub rejected: u64,
    pub cancelled: u64,
    pub measured_wait_nanos: u64,
    pub last_wait_nanos: u64,
    pub services: [Option<(u8, u16, u64)>; 8],
}

#[derive(Clone, Copy)]
struct Anchor {
    head: u32,
    caller: u32,
    context: Context,
    ip: u32,
    sp: u32,
    last_arrival: u64,
    kind: AnchorKind,
}
#[derive(Clone, Copy)]
enum AnchorKind {
    Guard,
    Backedge {
        code: [CodeCheck; 2],
        unchanged: u8,
        rejected: u8,
        qualified: bool,
        last_unchanged: u64,
    },
}
struct Interval {
    cpu: doslet_x86::CpuSnapshot,
    input: super::super::idle::InputState,
    mark: doslet_pc::RecurrenceMark,
    start: u64,
    source: bool,
}
#[derive(Default)]
pub(super) struct Recurrence {
    anchor: Option<Anchor>,
    interval: Option<Interval>,
    pub stats: RecurrenceStats,
    rejected: [Option<(u32, u64)>; 8],
    next_rejected: usize,
}
impl Recurrence {
    pub(super) fn follows(&self, head: u32) -> bool {
        // Multiple callers can visit the same guard in one event iteration.
        // Keep the selected recurrence until it disappears instead of resetting
        // complete episodes whenever a sparse scout sees the other caller.
        self.anchor
            .is_some_and(|a| a.head == head && matches!(a.kind, AnchorKind::Guard))
    }
}
impl Session {
    pub(in crate::session) fn analyzer_recurrence_service(&mut self, vector: u8, ax: u16) {
        if self.analyzer_recurrence_recording() {
            let slots = &mut self.analyzer.recurrence.stats.services;
            if let Some(slot) = slots
                .iter_mut()
                .find(|s| s.is_some_and(|(v, a, _)| v == vector && a == ax))
            {
                slot.as_mut().unwrap().2 += 1;
            } else if let Some(slot) = slots.iter_mut().find(|s| s.is_none()) {
                *slot = Some((vector, ax, 1));
            }
            if !super::super::idle::repeatable_service(
                vector,
                ax,
                self.registers().get_byte(2),
                self.mouse.motion,
            ) {
                self.analyzer_recurrence_cancel();
            }
        }
    }
    pub(in crate::session) fn analyzer_recurrence_recording(&self) -> bool {
        self.analyzer.recurrence.interval.is_some()
    }
    pub(super) fn analyzer_recurrence_learn(&mut self, head: u32, caller: u32, context: Context) {
        if self
            .analyzer
            .recurrence
            .anchor
            .is_some_and(|a| matches!(a.kind, AnchorKind::Backedge { .. }))
        {
            return;
        }
        self.analyzer_recurrence_forget();
        self.idle_abandon();
        let ip = self.registers().ip;
        self.analyzer.recurrence.anchor = Some(Anchor {
            head,
            caller,
            context,
            ip,
            sp: self.registers().get32(Gpr32::Esp),
            last_arrival: self.emulated_nanos(),
            kind: AnchorKind::Guard,
        });
        self.analyzer.recurrence.stats.backedge = false;
        self.analyzer.recurrence.stats.head = head;
        self.analyzer.recurrence.stats.caller = caller;
        self.machine.set_batch_yield_ip(Some(ip));
    }
    pub(in crate::session) fn analyzer_recurrence_cancel(&mut self) {
        let now = self.emulated_nanos();
        if let Some(interval) = self.analyzer.recurrence.interval.take() {
            self.machine.end_recurrence(interval.mark);
            self.analyzer.recurrence.stats.cancelled += 1;
            let mut abandon = false;
            if let Some(Anchor {
                kind:
                    AnchorKind::Backedge {
                        unchanged,
                        rejected,
                        qualified,
                        last_unchanged,
                        ..
                    },
                ..
            }) = &mut self.analyzer.recurrence.anchor
            {
                *unchanged = 0;
                *rejected = rejected.saturating_add(1);
                abandon = if *qualified {
                    now.saturating_sub(*last_unchanged) > FRESH
                } else {
                    *rejected >= 8
                };
            }
            if abandon {
                self.analyzer.recurrence.stats.last_probe_rejection_reason =
                    "incomplete intervals exceeded the probe observation bound";
                self.analyzer_recurrence_forget();
            }
        }
    }
    pub(super) fn analyzer_recurrence_forget(&mut self) {
        self.analyzer_recurrence_cancel();
        if let Some(anchor) = self.analyzer.recurrence.anchor.take() {
            self.machine.set_batch_yield_ip(None);
            if let AnchorKind::Backedge { qualified, .. } = anchor.kind {
                let recurrence = &mut self.analyzer.recurrence;
                recurrence.rejected[recurrence.next_rejected] = Some((
                    anchor.head,
                    self.machine.time().as_nanos().saturating_add(FRESH),
                ));
                recurrence.next_rejected =
                    (recurrence.next_rejected + 1) % recurrence.rejected.len();
                recurrence.stats.probe_rejections += 1;
                if qualified {
                    for c in &mut self.analyzer.candidates {
                        if c.stats.port && c.stats.boundary_accounting {
                            c.relearn("recurring backedge boundary invalidated");
                        }
                    }
                }
            }
            for c in &mut self.analyzer.candidates {
                if matches!(anchor.kind, AnchorKind::Guard)
                    && c.stats.head == anchor.head
                    && c.stats.boundary_accounting
                    && !c.stats.port
                {
                    c.gap("recurring caller boundary invalidated");
                    c.predicate = None;
                }
            }
            self.analyzer.relearn_until = 0;
        }
    }
    pub(super) fn analyzer_recurrence_boundary(&mut self) {
        let Some(anchor) = self.analyzer.recurrence.anchor else {
            return;
        };
        if matches!(anchor.kind, AnchorKind::Backedge { .. }) {
            self.analyzer_backedge_boundary(anchor);
            return;
        }
        let now = self.emulated_nanos();
        if now.saturating_sub(anchor.last_arrival) > FRESH {
            self.analyzer_recurrence_forget();
            return;
        }
        if self
            .analyzer
            .recurrence
            .interval
            .as_ref()
            .is_some_and(|i| now.saturating_sub(i.start) > INTERVAL_LIMIT)
        {
            self.analyzer_recurrence_cancel();
        }
        if self.registers().ip != anchor.ip || self.registers().get32(Gpr32::Esp) != anchor.sp {
            return;
        }
        // The caller inventory can finish at the same boundary where this
        // method already ran. Starting the next interval must happen only once.
        if self
            .analyzer
            .recurrence
            .interval
            .as_ref()
            .is_some_and(|i| i.start == now)
        {
            return;
        }
        let context = Context::new(
            self.registers(),
            self.machine.cpu.control.cr0,
            self.machine.cpu.control.cr3,
        );
        let return_pc = self
            .machine
            .bus
            .debug_ram(anchor.sp, 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()));
        if context != Some(anchor.context) || return_pc != anchor.caller.checked_add(5) {
            self.analyzer_recurrence_forget();
            return;
        }
        let evidence = self.analyzer.guards.recurring_source(
            anchor.head,
            anchor.caller,
            anchor.context,
            &self.machine.bus,
            now,
        );
        let source = evidence.is_some();
        let waiting = self.machine.cpu.flags & doslet_x86::flags::ZERO == 0;
        if let Some(interval) = self.analyzer.recurrence.interval.take() {
            let unchanged = self.machine.end_recurrence(interval.mark)
                && self.machine.cpu.matches(&interval.cpu)
                && self.input_state() == interval.input;
            let stats = &mut self.analyzer.recurrence.stats;
            stats.intervals += 1;
            stats.unchanged += u64::from(unchanged);
            stats.rejected += u64::from(!unchanged);
            if unchanged && interval.source && source && waiting {
                let elapsed = now.saturating_sub(interval.start);
                stats.last_wait_nanos = elapsed;
                stats.measured_wait_nanos = stats.measured_wait_nanos.saturating_add(elapsed);
                self.analyzer.confirmed_wait_nanos =
                    self.analyzer.confirmed_wait_nanos.saturating_add(elapsed);
            }
        }
        self.analyzer
            .recurrence
            .anchor
            .as_mut()
            .unwrap()
            .last_arrival = now;
        if let Some((mut predicate, guard)) = evidence {
            predicate.branch = anchor.ip;
            // Successive zero arrivals bracket a complete timed episode:
            // update work, then recurring waiting until the next expiration.
            // All unverified time, including IRQ service, is charged as work.
            self.analyzer.record_branch(
                predicate,
                waiting,
                now,
                self.hardware_config().cpu_mhz,
                Some(self.analyzer.confirmed_wait_nanos),
                None,
            );
            if let Some(c) = self
                .analyzer
                .candidates
                .iter_mut()
                .find(|c| c.stats.head == anchor.head && c.stats.boundary_accounting)
            {
                c.stats.source_transitions = guard.observed_decrements;
                c.stats.source_period_nanos = guard.observed_interval_nanos;
                c.stats.writer_pc = guard.writer_pc;
                c.stats.writer_irq = guard.writer_irq;
                c.stats.writer_kind = "observed IRQ decrement";
                c.stats.writer_address = Some(guard.operand);
                c.stats.writer_width = predicate.width;
                c.stats.writer_value = guard.writer_value;
                c.stats.writer_nanos = guard.writer_nanos;
                c.stats.writes = guard.writes;
                c.stats.partial_writes = guard.partial_writes;
            }
        } else {
            for c in &mut self.analyzer.candidates {
                if c.stats.head == anchor.head && c.stats.boundary_accounting && !c.stats.port {
                    c.gap("recurring caller source unavailable");
                }
            }
            self.analyzer.relearn_until = 0;
        }
        // Without fresh IRQ source evidence there is nothing to credit. Keep
        // the boundary but avoid paying for the journal until it can be used.
        if source && waiting {
            self.idle_abandon();
            self.analyzer.recurrence.interval = Some(Interval {
                cpu: self.machine.cpu.snapshot(),
                input: self.input_state(),
                mark: self.machine.begin_recurrence(),
                start: now,
                source,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doslet_x86::flags;

    fn source(s: &mut Session) {
        for _ in 0..4 {
            s.machine.cpu.flags |= flags::INTERRUPT;
            s.machine.bus.pic.raise_edge(0);
            s.advance(8);
            s.machine.bus.pic.write_command(0x20);
        }
    }

    #[test]
    fn complete_countdown_episodes_include_update_work_and_do_not_reselect_the_caller() {
        let mut s = guarded::tests::session();
        // On zero, reset the counter for the next two IRQs before returning.
        for (i, byte) in [
            0x83, 0x3d, 0, 0x30, 0, 0, 0, 0x75, 10, 0xc7, 5, 0, 0x30, 0, 0, 2, 0, 0, 0, 0xc3,
        ]
        .into_iter()
        .enumerate()
        {
            s.machine.bus.write_u8(0x10010 + i as u32, byte).unwrap();
        }
        s.machine.bus.pit.set_gate(0, false).unwrap();
        s.machine.bus.write_u32(0x3000, 2).unwrap();
        s.machine.cpu.registers.ip = 0x10000;
        s.set_synchronization_analysis(true);
        s.advance(100);
        for _ in 0..24 {
            s.advance(20_000);
            s.machine.cpu.flags |= flags::INTERRUPT;
            s.machine.bus.pic.raise_edge(0);
            s.advance(8);
            s.machine.bus.pic.write_command(0x20);
        }
        s.advance(20);
        let candidate = s
            .synchronization_analysis()
            .candidates
            .into_iter()
            .find(|c| c.head == 0x10010)
            .unwrap();
        assert!(candidate.episodes >= 8, "{candidate:?}");
        assert!(candidate.wait_nanos > candidate.work_nanos, "{candidate:?}");
        assert!(
            candidate.work_nanos > 0,
            "update and IRQ work must be included"
        );
        assert!(
            candidate
                .predicted_mhz
                .is_some_and(|mhz| mhz < s.hardware_config().cpu_mhz),
            "{candidate:?}"
        );
        assert!(s.analyzer.recurrence.follows(0x10010));
        let anchor = s.analyzer.recurrence.anchor.unwrap();
        s.analyzer_caller_begin(0x10010, 0x12345, anchor.context, anchor.sp);
        assert_eq!(s.analyzer.recurrence.anchor.unwrap().caller, 0x10000);
        assert_eq!(s.synchronization_analysis().caller.caller, 0x10000);
    }

    #[test]
    fn recurring_intervals_need_source_and_preserve_cpu_memory_time_without_allocating() {
        for dynamic in [false, true] {
            let mut s = guarded::tests::session();
            let mut reference = guarded::tests::session();
            s.set_dynamic_core(dynamic);
            reference.set_synchronization_analysis(false);
            reference.set_dynamic_core(dynamic);
            for session in [&mut s, &mut reference] {
                // A separate hot register block exercises native execution
                // once countdown waiting ends. Valid journals select decoded
                // batches because their stores require old-byte capture.
                for (i, b) in [0xe9, 0xf6, 1, 0, 0].into_iter().enumerate() {
                    session.machine.bus.write_u8(0x10005 + i as u32, b).unwrap();
                }
                for (i, b) in [0x46, 0x4e, 0xe9, 0xf9, 0xfd, 0xff, 0xff]
                    .into_iter()
                    .enumerate()
                {
                    session.machine.bus.write_u8(0x10200 + i as u32, b).unwrap();
                }
                session.machine.cpu.registers.ip = 0x10000;
            }
            s.set_synchronization_analysis(true);
            s.advance(100);
            reference.advance(100);
            assert_eq!(
                s.synchronization_analysis().recurrence.measured_wait_nanos,
                0
            );
            s.analyzer.remaining = 0;
            s.analyzer.next = u64::MAX;
            s.advance(1000);
            reference.advance(1000);
            s.prepare_dynamic(16);
            reference.prepare_dynamic(16);
            source(&mut s);
            source(&mut reference);
            // Compilation is explicit, outside the measured allocation scope.
            s.prepare_dynamic(16);
            reference.prepare_dynamic(16);
            let native_before = s.dynamic_stats().executed_steps;
            let allocations = allocation_counter::measure(|| {
                for turn in 0..40 {
                    if turn == 20 {
                        s.machine.bus.write_u32(0x3000, 0).unwrap();
                        reference.machine.bus.write_u32(0x3000, 0).unwrap();
                    }
                    s.advance(100);
                    reference.advance(100);
                    assert!(s.machine.cpu.matches(&reference.machine.cpu.snapshot()));
                    assert_eq!(s.emulated_nanos(), reference.emulated_nanos());
                    assert_eq!(
                        s.machine.bus.ram_view(0..0xa0000),
                        reference.machine.bus.ram_view(0..0xa0000)
                    );
                    assert_eq!(
                        s.machine.bus.cmos.snapshot(),
                        reference.machine.bus.cmos.snapshot()
                    );
                }
            });
            assert_eq!(allocations.count_total, 0);
            let r = s.synchronization_analysis().recurrence;
            assert!(r.intervals > 100, "{r:?}");
            assert!(r.measured_wait_nanos > 0, "{r:?}");
            assert_eq!(s.synchronization_analysis().proven_idle_nanos, 0);
            assert!(s.analyzer.confirmed_wait_nanos >= r.measured_wait_nanos);
            #[cfg(feature = "dynamic-core")]
            if dynamic {
                assert!(s.dynamic_stats().executed_steps > native_before);
            }
            #[cfg(not(feature = "dynamic-core"))]
            let _ = native_before;
            s.set_synchronization_analysis(false);
            assert!(!s.analyzer_recurrence_recording());
            assert!(s.machine.bus.native_ram_mut().is_some());
        }
    }

    #[test]
    fn changed_input_service_and_lost_boundary_cannot_credit_an_interval() {
        let mut s = guarded::tests::session();
        source(&mut s);
        // Stop exactly after the method has started a journal at an anchor.
        for _ in 0..30 {
            s.advance(1);
            if s.analyzer_recurrence_recording() {
                break;
            }
        }
        assert!(s.analyzer_recurrence_recording());
        let before = s.synchronization_analysis().recurrence.measured_wait_nanos;
        s.mouse.x = s.mouse.x.wrapping_add(1);
        let rejected = s.synchronization_analysis().recurrence.rejected;
        for _ in 0..20 {
            s.advance(1);
            if s.synchronization_analysis().recurrence.rejected != rejected {
                break;
            }
        }
        assert_eq!(
            s.synchronization_analysis().recurrence.rejected,
            rejected + 1
        );
        assert_eq!(
            s.synchronization_analysis().recurrence.measured_wait_nanos,
            before
        );
        assert!(s.analyzer_recurrence_recording());
        // Even a rejected service terminates the journal before dispatch.
        assert!(s.service(0x2f).is_err());
        assert!(!s.analyzer_recurrence_recording());
        assert!(s.machine.bus.native_ram_mut().is_some());
        s.machine.cpu.registers.ip = 0x10180;
        s.machine.bus.write_u8(0x10180, 0xeb).unwrap();
        s.machine.bus.write_u8(0x10181, 0xfe).unwrap();
        s.advance(1000);
        assert_eq!(
            s.synchronization_analysis().recurrence.measured_wait_nanos,
            before
        );
    }
}
