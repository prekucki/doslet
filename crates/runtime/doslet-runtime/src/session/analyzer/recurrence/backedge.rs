//! Discover complete repeated execution without assuming a game's event layout.
use super::*;

impl Session {
    fn analyzer_backedge_source(&self, now: u64) -> bool {
        self.analyzer.candidates.iter().any(|c| {
            c.stats.port
                && c.stats.boundary_accounting
                && c.stats.episodes >= 8
                && c.stats.reference_period_nanos != 0
                && !c.stats.variable_cadence
                // A retune discards the open work episode, so its replacement
                // may finish two periods after the last complete one. Actual
                // VGA exits keep the source fresh while that work is measured.
                // This does not qualify a budget or refresh a missing boundary.
                && now.saturating_sub(c.work_start.unwrap_or(c.stats.last_episode_nanos)) <= FRESH
                && c.predicate
                    .is_some_and(|p| self.machine.bus.code_valid(&p.version))
        })
    }

    /// Called only for an executed, taken direct backedge in a scout.
    /// It installs an observation boundary, never a skip or a lower CPU budget.
    pub(in crate::session) fn analyzer_backedge_discover(
        &mut self,
        pc: u32,
        length: u8,
        context: Context,
        sp: u32,
    ) {
        let head = self.registers().ip;
        let now = self.emulated_nanos();
        if self
            .analyzer
            .recurrence
            .anchor
            .is_some_and(|a| matches!(a.kind, AnchorKind::Backedge { .. }))
            || self.analyzer.irq_depth != 0
            || head >= pc
            || self.registers().get32(Gpr32::Esp) != sp
            || !self.analyzer_backedge_source(now)
            || self
                .analyzer
                .recurrence
                .rejected
                .iter()
                .flatten()
                .any(|&(h, until)| h == head && now < until)
            || self.analyzer.candidates.iter().any(|c| {
                c.predicate
                    .is_some_and(|p| p.port && head >= p.head && pc <= p.branch)
            })
        {
            return;
        }
        let (Some(head_code), Some(branch_code)) = (
            self.machine.bus.code_check(head, 1),
            self.machine.bus.code_check(pc, length),
        ) else {
            return;
        };
        if let Some(anchor) = self.analyzer.recurrence.anchor {
            // A frequently visited guard can lack observed IRQ writers
            // indefinitely. It must not monopolize discovery when an
            // independently timed backedge is available. Existing backedges
            // and guards with fresh source evidence retain their boundary.
            if self
                .analyzer
                .guards
                .recurring_source(
                    anchor.head,
                    anchor.caller,
                    anchor.context,
                    &self.machine.bus,
                    now,
                )
                .is_some()
            {
                return;
            }
            self.analyzer_recurrence_forget();
        }
        self.idle_abandon();
        self.analyzer.recurrence.anchor = Some(Anchor {
            head,
            caller: pc,
            context,
            ip: head,
            sp,
            last_arrival: now,
            kind: AnchorKind::Backedge {
                code: [head_code, branch_code],
                unchanged: 0,
                rejected: 0,
                qualified: false,
                last_unchanged: now,
            },
        });
        let stats = &mut self.analyzer.recurrence.stats;
        stats.backedge = true;
        stats.head = head;
        stats.caller = pc;
        self.machine.set_batch_yield_ip(Some(head));
        self.analyzer_recurrence_boundary();
    }

    pub(super) fn analyzer_backedge_boundary(&mut self, anchor: Anchor) {
        let AnchorKind::Backedge {
            code,
            mut unchanged,
            mut rejected,
            mut qualified,
            mut last_unchanged,
        } = anchor.kind
        else {
            return;
        };
        let now = self.emulated_nanos();
        let rejection = if !code.iter().all(|code| self.machine.bus.code_valid(code)) {
            Some("recurring boundary code changed")
        } else if now.saturating_sub(anchor.last_arrival) > FRESH {
            Some("recurring boundary absent for 250 ms")
        } else if qualified && now.saturating_sub(last_unchanged) > FRESH {
            Some("no unchanged interval for 250 ms")
        } else {
            None
        };
        if let Some(reason) = rejection {
            self.analyzer.recurrence.stats.last_probe_rejection_reason = reason;
            self.analyzer_recurrence_forget();
            return;
        }
        // A long animation/update interval is work. Cancel its journal, but
        // retain the boundary so the next frame can still be measured.
        if self
            .analyzer
            .recurrence
            .interval
            .as_ref()
            .is_some_and(|i| now.saturating_sub(i.start) > INTERVAL_LIMIT)
        {
            self.analyzer_recurrence_cancel();
            return;
        }
        if self.registers().ip != anchor.ip || self.registers().get32(Gpr32::Esp) != anchor.sp {
            return;
        }
        // Only a complete arrival can credit waiting. Validate its source
        // there, instead of walking VGA candidates on every intervening
        // instruction/IRQ step. Journal and absent-boundary time limits above
        // still apply away from the anchor, and no interval gets credit before
        // these checks and the complete state comparison below.
        let source = self.analyzer_backedge_source(now);
        let rejection = if !source {
            Some("fresh stable VGA source unavailable")
        } else {
            None
        };
        if let Some(reason) = rejection {
            self.analyzer.recurrence.stats.last_probe_rejection_reason = reason;
            self.analyzer_recurrence_forget();
            return;
        }
        if Context::new(
            self.registers(),
            self.machine.cpu.control.cr0,
            self.machine.cpu.control.cr3,
        ) != Some(anchor.context)
        {
            self.analyzer.recurrence.stats.last_probe_rejection_reason =
                "execution context changed";
            self.analyzer_recurrence_forget();
            return;
        }
        if self
            .analyzer
            .recurrence
            .interval
            .as_ref()
            .is_some_and(|i| i.start == now)
        {
            return;
        }
        if let Some(interval) = self.analyzer.recurrence.interval.take() {
            let same = self.machine.end_recurrence(interval.mark)
                && self.machine.cpu.matches(&interval.cpu)
                && self.input_state() == interval.input;
            let stats = &mut self.analyzer.recurrence.stats;
            stats.intervals += 1;
            stats.unchanged += u64::from(same);
            stats.rejected += u64::from(!same);
            unchanged = if same { unchanged.saturating_add(1) } else { 0 };
            rejected = if same { 0 } else { rejected.saturating_add(1) };
            qualified |= unchanged >= 8;
            if same {
                last_unchanged = now;
            }
            // No extrapolation: even after qualification, each interval must
            // independently restore every observed CPU/RAM/input/device effect.
            if same && qualified && interval.source {
                let elapsed = now.saturating_sub(interval.start);
                stats.last_wait_nanos = elapsed;
                stats.measured_wait_nanos = stats.measured_wait_nanos.saturating_add(elapsed);
                self.analyzer.confirmed_wait_nanos =
                    self.analyzer.confirmed_wait_nanos.saturating_add(elapsed);
            }
            // A known recurring loop may run several changing animation or
            // input handlers in sequence. They remain work; retain its boundary
            // for at most FRESH without another unchanged interval. Unqualified
            // probes still stop after eight failed attempts.
            if !qualified && rejected >= 8 {
                stats.last_probe_rejection_reason =
                    "eight consecutive changed or interrupted intervals";
                self.analyzer_recurrence_forget();
                return;
            }
        }
        let active = self.analyzer.recurrence.anchor.as_mut().unwrap();
        active.last_arrival = now;
        active.kind = AnchorKind::Backedge {
            code,
            unchanged,
            rejected,
            qualified,
            last_unchanged,
        };
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

#[cfg(test)]
mod tests {
    use super::*;
    use doslet_bus::PortBus;

    fn session() -> Session {
        let mut s = guarded::tests::session();
        s.set_synchronization_analysis(false);
        // An event dispatcher calls indirectly, touches stack scratch, and
        // repeats. It has no direct CMP-zero returning countdown guard.
        for (start, bytes) in [
            (0x10000, &[0xb8, 0, 1, 1, 0, 0xff, 0xd0, 0xeb, 0xf7][..]),
            (0x10100, &[0x46, 0x4e, 0xc3][..]),
            (0x10200, &[0xec, 0xa8, 8, 0x74, 0xfb, 0xc3][..]),
            (0x10300, &[0xeb, 0xfe][..]),
            (0x10400, &[0x50, 0x58, 0xcf][..]),
            (0x8040, &[0, 4, 0x18, 0, 0, 0x8e, 1, 0][..]),
        ] {
            for (i, &byte) in bytes.iter().enumerate() {
                s.machine.bus.write_u8(start + i as u32, byte).unwrap();
            }
        }
        s.machine.cpu.registers.ip = 0x10000;
        s.machine.cpu.registers.set32(Gpr32::Esp, 0x7000);
        s
    }
    fn source(s: &mut Session) {
        let c = &mut s.analyzer.candidates[0];
        c.predicate = Some(Predicate {
            head: 0x10200,
            branch: 0x10203,
            end: 0x10205,
            operand: 0x3da,
            width: 1,
            port: true,
            version: s.machine.bus.code_check(0x10200, 5).unwrap(),
        });
        c.stats.port = true;
        c.stats.boundary_accounting = true;
        c.stats.episodes = 8;
        c.stats.reference_period_nanos = 16_683_216;
        c.stats.period_nanos = 16_683_216;
        c.stats.last_episode_nanos = s.machine.time().as_nanos();
        c.stats.measured_mhz = s.machine.hardware_config().cpu_mhz;
    }
    fn recording(s: &mut Session) {
        for _ in 0..40 {
            s.advance(1);
            if s.analyzer_recurrence_recording() {
                return;
            }
        }
        panic!("no interval: {:?}", s.analyzer.recurrence.stats);
    }

    #[test]
    fn taken_conditional_backedge_measures_complete_intervals_and_stops_on_exit() {
        for dynamic in [false, true] {
            let mut s = session();
            let mut reference = session();
            for session in [&mut s, &mut reference] {
                // The callee restores ESI and leaves ZF set when ESI is zero.
                // The conditional loop exits normally when ESI changes.
                session.machine.bus.write_u8(0x10007, 0x74).unwrap();
                session.machine.bus.write_u8(0x10009, 0xf4).unwrap();
                session.machine.cpu.registers.set32(Gpr32::Esi, 0);
                session.set_dynamic_core(dynamic);
                session.advance(1000);
                session.prepare_dynamic(16);
                session.advance(1000);
            }
            s.set_synchronization_analysis(true);
            source(&mut s);
            for _ in 0..20 {
                s.advance(100);
                reference.advance(100);
                assert!(s.machine.cpu.matches(&reference.machine.cpu.snapshot()));
                assert_eq!(s.emulated_nanos(), reference.emulated_nanos());
                assert_eq!(
                    s.machine.bus.ram_view(0..0xa0000),
                    reference.machine.bus.ram_view(0..0xa0000)
                );
            }
            assert!(s.analyzer.recurrence.stats.measured_wait_nanos > 0);
            let waiting = s.analyzer.recurrence.stats.measured_wait_nanos;
            for session in [&mut s, &mut reference] {
                session.machine.cpu.registers.set32(Gpr32::Esi, 1);
                session.advance(100);
            }
            assert!(s.machine.cpu.matches(&reference.machine.cpu.snapshot()));
            assert_eq!(s.registers().ip, 0x1000a);
            assert_eq!(s.analyzer.recurrence.stats.measured_wait_nanos, waiting);
        }
    }

    #[test]
    fn unverified_guard_cannot_starve_a_presentation_backedge() {
        let mut s = session();
        s.set_synchronization_analysis(true);
        source(&mut s);
        let context = Context::new(
            s.registers(),
            s.machine.cpu.control.cr0,
            s.machine.cpu.control.cr3,
        )
        .unwrap();
        s.analyzer_recurrence_learn(0x10010, 0x10020, context);
        assert!(!s.analyzer.recurrence.stats.backedge);
        let sp = s.registers().get32(Gpr32::Esp);
        // The scout has executed the dispatcher's direct backward branch.
        s.analyzer_backedge_discover(0x10007, 2, context, sp);
        assert!(s.analyzer.recurrence.stats.backedge);
        s.advance(2000);
        assert!(s.analyzer.recurrence.stats.measured_wait_nanos > 0);
        let anchor = s.analyzer.recurrence.anchor.unwrap();
        // A later caller trace must not replace the established observation.
        s.analyzer_recurrence_learn(0x10010, 0x10020, context);
        assert_eq!(s.analyzer.recurrence.anchor.unwrap().head, anchor.head);
        assert!(s.analyzer.recurrence.stats.backedge);
    }

    #[test]
    fn indirect_dispatch_recurrence_is_discovered_without_an_irq_guard() {
        for dynamic in [false, true] {
            let mut s = session();
            let mut reference = session();
            for session in [&mut s, &mut reference] {
                session.set_dynamic_core(dynamic);
                session.advance(1000);
                session.prepare_dynamic(16);
                session.advance(1000);
            }
            let native_before = s.dynamic_stats().executed_steps;
            s.set_synchronization_analysis(true);
            source(&mut s);
            let measured = allocation_counter::measure(|| {
                for _ in 0..40 {
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
            assert_eq!(measured.count_total, 0);
            let r = s.synchronization_analysis().recurrence;
            assert!(
                r.backedge && r.intervals > 100 && r.measured_wait_nanos > 0,
                "{r:?}"
            );
            assert_eq!(r.head, 0x10000);
            assert_eq!(s.synchronization_analysis().proven_idle_nanos, 0);
            assert!(
                s.synchronization_analysis()
                    .guards
                    .iter()
                    .all(|g| g.head == 0)
            );
            s.set_synchronization_analysis(false);
            assert!(s.machine.bus.native_ram_mut().is_some());
            s.advance(1000);
            #[cfg(feature = "dynamic-core")]
            if dynamic {
                assert!(s.dynamic_stats().executed_steps > native_before);
            }
            #[cfg(not(feature = "dynamic-core"))]
            let _ = native_before;
        }
    }

    #[test]
    fn recurring_execution_without_a_fresh_presentation_source_is_not_waiting() {
        for case in 0..4 {
            let mut s = session();
            s.set_synchronization_analysis(true);
            source(&mut s);
            match case {
                0 => s.analyzer.candidates[0].stats.reference_period_nanos = 0,
                1 => s.analyzer.candidates[0].stats.variable_cadence = true,
                2 => s.analyzer.candidates[0].stats.episodes = 7,
                _ => s.analyzer.candidates[0].predicate = None,
            }
            s.advance(2000);
            assert_eq!(s.analyzer.recurrence.stats.measured_wait_nanos, 0);
            assert!(s.analyzer.recurrence.anchor.is_none());
        }
    }

    #[test]
    fn retuning_keeps_a_fresh_presentation_boundary_until_the_next_complete_episode() {
        let mut s = session();
        s.set_synchronization_analysis(true);
        source(&mut s);
        let c = &mut s.analyzer.candidates[0];
        c.stats.reference_period_nanos = 166_000_000;
        c.stats.last_episode_nanos = 1_000_000_000;
        c.relearn("validating changed throughput");
        // Retuning invalidates the open episode. The next observed VGA exit
        // starts a new one; its completion is two frame periods after the
        // previous complete episode, exceeding the 250-ms freshness limit.
        c.branch_measured(false, 1_166_000_000, 125, Some(0), Some(1_167_000_000));
        assert!(s.analyzer_backedge_source(1_251_000_000));
        assert_eq!(s.analyzer.candidates[0].qualified_episodes, 0);
        assert_eq!(s.analyzer.candidates[0].budget(1_251_000_000, 133), None);
        assert!(!s.analyzer_backedge_source(1_416_000_001));
        s.analyzer.candidates[0].gap("VGA source changed");
        assert!(!s.analyzer_backedge_source(1_251_000_000));
    }

    #[test]
    fn changed_ram_input_device_effects_and_irqs_are_charged_as_work() {
        for case in 0..5 {
            let mut s = session();
            s.set_synchronization_analysis(true);
            source(&mut s);
            s.advance(1000);
            recording(&mut s);
            let before = s.analyzer.recurrence.stats.measured_wait_nanos;
            let rejected = s.analyzer.recurrence.stats.rejected;
            match case {
                0 => s.machine.bus.write_u8(0x4000, 1).unwrap(),
                1 => s.mouse.x = s.mouse.x.wrapping_add(1),
                2 => {
                    s.machine.bus.read_port_u8(0x70).unwrap();
                }
                3 => {
                    s.machine.cpu.flags |= doslet_x86::flags::INTERRUPT;
                    s.machine.bus.pic.raise_edge(0);
                }
                _ => {
                    for i in 0..2049 {
                        s.machine.bus.write_u8(0x4000 + i, 0).unwrap();
                    }
                }
            }
            let cancelled = s.analyzer.recurrence.stats.cancelled;
            for _ in 0..100 {
                s.advance(1);
                if s.analyzer.recurrence.stats.rejected != rejected
                    || s.analyzer.recurrence.stats.cancelled != cancelled
                {
                    break;
                }
            }
            assert_eq!(
                s.analyzer.recurrence.stats.measured_wait_nanos, before,
                "case {case}"
            );
            assert!(
                s.analyzer.recurrence.stats.rejected > rejected
                    || s.analyzer.recurrence.stats.cancelled > cancelled,
                "case {case}"
            );
        }
    }

    #[test]
    fn lost_context_mutated_code_and_missing_boundary_release_observation() {
        for case in 0..4 {
            let mut s = session();
            s.set_synchronization_analysis(true);
            source(&mut s);
            s.advance(1000);
            recording(&mut s);
            let before = s.analyzer.recurrence.stats.measured_wait_nanos;
            match case {
                0 => s.machine.bus.write_u8(0x10007, 0xeb).unwrap(),
                1 => {
                    s.machine.cpu.control.cr3 = 0x1000;
                    // Context is compared at the next complete loop boundary.
                    let anchor = s.analyzer.recurrence.anchor.unwrap();
                    s.machine.cpu.registers.ip = anchor.ip;
                    s.machine.cpu.registers.set32(Gpr32::Esp, anchor.sp);
                }
                2 => {
                    s.analyzer.candidates[0].stats.reference_period_nanos = 0;
                    // A source is required before crediting the next complete
                    // interval. Intervening execution may keep the journal,
                    // but it must not add any measured waiting.
                    s.advance(10);
                }
                _ => {
                    s.machine.cpu.registers.ip = 0x10300;
                    s.advance(200_000);
                }
            }
            s.analyzer_recurrence_boundary();
            assert!(!s.analyzer_recurrence_recording(), "case {case}");
            assert!(s.machine.bus.native_ram_mut().is_some());
            assert_eq!(s.analyzer.recurrence.stats.measured_wait_nanos, before);
        }
    }

    #[test]
    fn long_animation_work_does_not_erase_the_next_frames_boundary() {
        let mut s = session();
        s.set_synchronization_analysis(true);
        source(&mut s);
        s.advance(1000);
        recording(&mut s);
        let anchor = s.analyzer.recurrence.anchor.unwrap();
        assert!(matches!(
            anchor.kind,
            AnchorKind::Backedge {
                qualified: true,
                ..
            }
        ));
        let before = s.analyzer.recurrence.stats.measured_wait_nanos;
        s.machine.bus.write_u8(0x4000, 1).unwrap();
        s.machine.cpu.registers.ip = 0x10300;
        s.advance(200_000);
        assert_eq!(s.analyzer.recurrence.stats.measured_wait_nanos, before);
        assert!(!s.analyzer_recurrence_recording());
        assert!(s.analyzer.recurrence.anchor.is_some());
        s.machine.cpu.registers.ip = anchor.ip;
        s.machine.cpu.registers.set32(Gpr32::Esp, anchor.sp);
        s.advance(1000);
        assert!(s.analyzer.recurrence.stats.measured_wait_nanos > before);
        assert_eq!(s.machine.bus.ram_view(0x4000..0x4001), Some(&[1][..]));
    }

    #[test]
    fn a_burst_of_animation_changes_keeps_a_previously_qualified_boundary() {
        let mut s = session();
        s.set_synchronization_analysis(true);
        source(&mut s);
        s.advance(1000);
        let wait = s.analyzer.recurrence.stats.measured_wait_nanos;
        for value in 1..=16 {
            recording(&mut s);
            let intervals = s.analyzer.recurrence.stats.intervals;
            s.machine.bus.write_u8(0x4000, value).unwrap();
            for _ in 0..40 {
                s.advance(1);
                if s.analyzer.recurrence.stats.intervals != intervals {
                    break;
                }
            }
            assert!(
                s.analyzer.recurrence.anchor.is_some(),
                "animation update {value}"
            );
            assert_eq!(s.analyzer.recurrence.stats.measured_wait_nanos, wait);
        }
        s.advance(1000);
        assert!(s.analyzer.recurrence.stats.measured_wait_nanos > wait);
    }

    #[test]
    fn qualified_boundary_expires_when_work_never_becomes_repeatable_again() {
        for service in [false, true] {
            let mut s = session();
            s.set_synchronization_analysis(true);
            source(&mut s);
            s.advance(1000);
            recording(&mut s);
            let wait = s.analyzer.recurrence.stats.measured_wait_nanos;
            let began = s.emulated_nanos();
            s.machine.set_cpu_mhz(1).unwrap();
            for value in 1..=12_000 {
                source(&mut s); // Keep presentation evidence fresh independently.
                for _ in 0..40 {
                    if s.analyzer_recurrence_recording() || s.analyzer.recurrence.anchor.is_none() {
                        break;
                    }
                    s.advance(1);
                }
                if s.analyzer.recurrence.anchor.is_none() {
                    break;
                }
                assert!(s.analyzer_recurrence_recording());
                if service {
                    s.analyzer_recurrence_service(0x21, 0x3d00);
                } else {
                    let intervals = s.analyzer.recurrence.stats.intervals;
                    s.machine.bus.write_u8(0x4000, value as u8).unwrap();
                    for _ in 0..40 {
                        s.advance(1);
                        if s.analyzer.recurrence.stats.intervals != intervals
                            || s.analyzer.recurrence.anchor.is_none()
                        {
                            break;
                        }
                    }
                }
                if s.analyzer.recurrence.anchor.is_none() {
                    break;
                }
            }
            assert!(s.analyzer.recurrence.anchor.is_none(), "service={service}");
            assert!(s.emulated_nanos() - began > FRESH);
            assert!(s.emulated_nanos() - began < FRESH + 1_000_000);
            assert_eq!(s.analyzer.recurrence.stats.measured_wait_nanos, wait);
            assert!(s.machine.bus.native_ram_mut().is_some());
        }
    }

    #[test]
    fn changing_dispatch_and_cancelled_services_do_not_pin_an_unusable_probe() {
        let mut s = session();
        s.machine.bus.write_u8(0x10101, 0x90).unwrap(); // Never restores ESI.
        s.set_synchronization_analysis(true);
        source(&mut s);
        s.advance(1000);
        assert!(s.analyzer.recurrence.stats.probe_rejections > 0);
        assert!(s.analyzer.recurrence.anchor.is_none());
        assert_eq!(s.analyzer.recurrence.stats.measured_wait_nanos, 0);

        let mut s = session();
        s.set_synchronization_analysis(true);
        source(&mut s);
        recording(&mut s);
        for i in 0..8 {
            s.analyzer_recurrence_service(0x21, 0x3d00); // A nonrepeatable DOS service.
            if i < 7 {
                recording(&mut s);
            }
        }
        assert!(s.analyzer.recurrence.anchor.is_none());
        assert!(s.analyzer.recurrence.stats.probe_rejections > 0);
        assert_eq!(s.analyzer.recurrence.stats.measured_wait_nanos, 0);
    }
}
