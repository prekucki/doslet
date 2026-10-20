//! Short diagnostic interpreter bursts. Observed predicates are not idle proofs.
use super::{RuntimeError, Session};
use doslet_x86::{Access, Gpr16, SegmentRegister};

const BURST: u32 = 512;
const PERIOD: u64 = 2_000_000;

#[derive(Clone, Copy, Debug, Default)]
pub struct SynchronizationStats {
    pub poll_waits: u64,
    pub poll_cpu_peak_nanos: u64,
    pub traced_steps: u64,
    pub conditions: u64,
    pub learned_ip: Option<u32>,
    pub learned_candidates: Option<[u32; 2]>,
    pub learned_hits: u64,
    pub learned_reason: &'static str,
    pub learned_checked_ip: u32,
    pub learned_ready: u64,
    pub learned_due: u64,
    pub learned_deadline_rejections: u64,
    pub learned_other_rejections: u64,
    pub learned_horizon_nanos: Option<u64>,
    pub learned_reserve_nanos: u64,
    pub cs: u16,
    pub ip: u32,
    pub operand: u32,
    pub port: bool,
    pub width: u8,
    pub value: u32,
    pub comparison: u32,
    pub taken: u64,
    pub fallthrough: u64,
    pub value_changes: u64,
    pub observed_changes: u64,
    pub change_operand: u32,
    pub change_cs: u16,
    pub change_ip: u32,
    pub change_with_irq: bool,
    pub change_with_device_access: bool,
    pub recurrence_samples: u32,
    pub recurrence_peak_nanos: u64,
    pub recurrence_mhz: u16,
    pub recurrence_at: u64,
    pub horizon_nanos: Option<u64>,
    pub condition_event_nanos: Option<u64>,
    pub horizon_source: &'static str,
    pub reserve_nanos: u64,
    pub provisional_slack_nanos: Option<u64>,
    pub provisional_slack_steps: Option<u64>,
}
#[derive(Clone, Copy, Default, PartialEq)]
struct Condition {
    code32: bool,
    cs: u16,
    ip: u32,
    base: u32,
    branch: u32,
    target: u32,
    end: u32,
    operand: u32,
    port: bool,
    width: u8,
    comparison: u32,
    not_equal: bool,
    returns_to_caller: bool,
}
#[derive(Clone, Copy, Default)]
struct Entry {
    condition: Condition,
    value: u32,
    taken: u64,
    fallthrough: u64,
    changes: u64,
    decreases: u32,
    increases: u32,
    seen: u32,
    last_time: u64,
    last_burst: u64,
    recurrence_samples: u32,
    recurrence_peak: u64,
    mhz: u16,
}
impl Entry {
    fn refresh(&mut self, value: u32, now: u64, mhz: u16) {
        if now.saturating_sub(self.last_time) > 1_000_000_000 || self.mhz != mhz {
            self.seen = 0;
            self.decreases = 0;
            self.increases = 0;
            self.recurrence_peak = 0;
        }
        if self.seen > 0 {
            self.decreases = self.decreases.saturating_add(u32::from(value < self.value));
            self.increases = self.increases.saturating_add(u32::from(value > self.value));
        }
        self.value = value;
        self.seen = self.seen.saturating_add(1);
        self.last_time = now;
        self.mhz = mhz;
    }
    fn qualified(&self) -> bool {
        self.seen >= 8 && self.decreases > 0 && self.increases > 0
    }
}
#[derive(Default)]
pub(super) struct Synchronization {
    next: u64,
    remaining: u32,
    pending: Option<(Condition, u32)>,
    entries: [Entry; 32],
    learned: [Option<Entry>; 2],
    stats: SynchronizationStats,
}
impl Synchronization {
    fn record(&mut self, condition: Condition, value: u32, taken: bool, nanos: u64, mhz: u16) {
        self.stats.conditions += 1;
        let index = self
            .entries
            .iter()
            .position(|e| e.seen != 0 && e.condition == condition)
            .or_else(|| {
                self.entries
                    .iter()
                    .position(|e| e.taken + e.fallthrough == 0)
            });
        let Some(index) = index else {
            return;
        };
        let e = &mut self.entries[index];
        if e.condition != condition {
            *e = Entry::default();
        }
        if e.seen != 0 && e.value != value {
            e.changes += 1;
            e.decreases = e.decreases.saturating_add(u32::from(value < e.value));
            e.increases = e.increases.saturating_add(u32::from(value > e.value));
        }
        e.seen = e.seen.saturating_add(1);
        if e.mhz != mhz {
            e.recurrence_samples = 0;
            e.recurrence_peak = 0;
            e.last_burst = 0;
        }
        if e.last_burst == self.next && nanos > e.last_time {
            e.recurrence_peak = e.recurrence_peak.max(nanos - e.last_time);
            e.recurrence_samples = e.recurrence_samples.saturating_add(1);
        }
        e.mhz = mhz;
        e.last_time = nanos;
        e.last_burst = self.next;
        e.condition = condition;
        e.value = value;
        e.taken += u64::from(taken);
        e.fallthrough += u64::from(!taken);
        if !condition.port
            && condition.code32
            && condition.base == 0
            && condition.comparison == 0
            && condition.not_equal
            && condition.returns_to_caller
            && condition.target >= condition.end
            && condition.target - condition.end <= 64
        {
            // A first structural match installs an observation point, not permission
            // to wait. Keep arrival evidence rather than overwriting it with sparse traces.
            if let Some(old) = self
                .learned
                .iter_mut()
                .flatten()
                .find(|old| old.condition == condition)
            {
                if old.mhz == e.mhz {
                    old.recurrence_peak = old.recurrence_peak.max(e.recurrence_peak);
                }
            } else if let Some(slot) = self.learned.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(*e);
            } else if let Some(slot) = self
                .learned
                .iter_mut()
                .filter(|slot| {
                    slot.is_some_and(|old| nanos.saturating_sub(old.last_time) > 1_000_000_000)
                })
                .min_by_key(|slot| slot.unwrap().last_time)
            {
                *slot = Some(*e);
            }
        }
        let top = self
            .entries
            .iter()
            .max_by_key(|e| e.taken + e.fallthrough)
            .unwrap();
        self.stats = SynchronizationStats {
            traced_steps: self.stats.traced_steps,
            conditions: self.stats.conditions,
            cs: top.condition.cs,
            ip: top.condition.ip,
            operand: top.condition.operand,
            port: top.condition.port,
            width: top.condition.width,
            value: top.value,
            comparison: top.condition.comparison,
            taken: top.taken,
            fallthrough: top.fallthrough,
            value_changes: top.changes,
            recurrence_samples: top.recurrence_samples,
            recurrence_peak_nanos: top.recurrence_peak,
            recurrence_mhz: top.mhz,
            recurrence_at: top.last_time,
            ..self.stats
        };
    }
}

/// Narrow decoder: absolute CMP immediate and IN AL,DX + AND/TEST + JZ/JNZ.
/// No disassembly guess or arbitrary bytes behind the current PC are accepted.
fn decode(
    bytes: &[u8],
    code32: bool,
    ip: u32,
    cs: u16,
    base: u32,
    dx: u16,
    ah: u8,
) -> Option<Condition> {
    let mut at = 0;
    let mut width = if code32 { 4 } else { 2 };
    let mut address32 = code32;
    if bytes.get(at) == Some(&0x66) {
        width = if code32 { 2 } else { 4 };
        at += 1;
    }
    if bytes.get(at) == Some(&0x67) {
        address32 = !code32;
        at += 1;
    }
    let opcode = *bytes.get(at)?;
    at += 1;
    let (operand, comparison, port) = if matches!(opcode, 0x80 | 0x83) {
        if *bytes.get(at)? != if address32 { 0x3d } else { 0x3e } {
            return None;
        }
        at += 1;
        let address = if address32 {
            let a = u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
            at += 4;
            a
        } else {
            let a = u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?);
            at += 2;
            u32::from(a)
        };
        if opcode == 0x80 {
            width = 1;
        }
        let immediate = *bytes.get(at)?;
        at += 1;
        let mask = match width {
            1 => 0xff,
            2 => 0xffff,
            _ => u32::MAX,
        };
        (address, (i32::from(immediate as i8) as u32) & mask, false)
    } else if opcode == 0xec && at == 1 {
        width = 1;
        let mask = match bytes.get(at..at + 2)? {
            [0x24 | 0xa8, mask] => *mask,
            [0x84, 0xc4] => ah,
            _ => return None,
        };
        at += 2;
        (u32::from(dx), u32::from(mask), true)
    } else {
        return None;
    };
    let branch = ip.checked_add(at as u32)?;
    let not_equal =
        bytes.get(at) == Some(&0x75) || bytes.get(at..at + 2) == Some([0x0f, 0x85].as_slice());
    let displacement = match bytes.get(at..at + 2)? {
        [0x74 | 0x75, d] => {
            at += 2;
            i32::from(*d as i8)
        }
        [0x0f, 0x84 | 0x85] => {
            at += 2;
            if code32 {
                let d = i32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?);
                at += 4;
                d
            } else {
                let d = i16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?);
                at += 2;
                i32::from(d)
            }
        }
        _ => return None,
    };
    let end = ip.checked_add(at as u32)?;
    let target = end.checked_add_signed(displacement)?;
    // Wrapping transfers, overrides other than those above and aliases fall back.
    if !code32 && (end > 0xffff || target > 0xffff) {
        return None;
    }
    Some(Condition {
        code32,
        cs,
        ip,
        base,
        branch,
        target,
        end,
        operand,
        port,
        width,
        comparison,
        not_equal,
        returns_to_caller: false,
    })
}
impl Session {
    fn current_learned(&self) -> Option<Entry> {
        let r = self.registers();
        self.synchronization
            .learned
            .iter()
            .flatten()
            .find(|e| e.condition.ip == r.ip && e.condition.cs == r.cs)
            .copied()
    }
    pub(super) fn learned_idle_head(&self) -> Option<u32> {
        self.current_learned().map(|e| e.condition.ip)
    }
    fn learned_idle_heads(&self) -> Option<[u32; 2]> {
        let first = self
            .synchronization
            .learned
            .iter()
            .flatten()
            .next()?
            .condition
            .ip;
        Some(
            self.synchronization
                .learned
                .map(|slot| slot.map_or(first, |e| e.condition.ip)),
        )
    }
    /// Refresh a discovered guard at actual batch arrivals, independently of sparse traces.
    fn refresh_learned_guard(&mut self, c: Condition, value: u32) {
        let now = self.emulated_nanos();
        let mhz = self.hardware_config().cpu_mhz;
        let Some(e) = self
            .synchronization
            .learned
            .iter_mut()
            .flatten()
            .find(|e| e.condition == c)
        else {
            return;
        };
        if e.condition != c {
            return;
        }
        e.refresh(value, now, mhz);
        self.synchronization.stats.learned_hits += 1;
    }
    pub(super) fn learned_idle_budget(&mut self) -> Option<u64> {
        self.synchronization.stats.learned_checked_ip = self.registers().ip;
        self.synchronization.stats.learned_horizon_nanos = None;
        self.synchronization.stats.learned_reserve_nanos = 0;
        let result = self.learned_idle_budget_checked();
        let stats = &mut self.synchronization.stats;
        match result {
            Ok(_) => stats.learned_ready += 1,
            Err("counter due or negative") => stats.learned_due += 1,
            Err("service deadline too close") => stats.learned_deadline_rejections += 1,
            Err(_) => stats.learned_other_rejections += 1,
        }
        self.synchronization.stats.learned_reason = match result {
            Ok(_) => "guard ready",
            Err(reason) => reason,
        };
        result.ok()
    }
    /// Opt-in heuristic: recognize a changing countdown guard returning on nonzero.
    /// Does not prove that the caller has no work. Bound waiting and retain IRQ wake.
    fn learned_idle_budget_checked(&mut self) -> Result<u64, &'static str> {
        let e = self
            .current_learned()
            .ok_or("no learned guard at this PC")?;
        let r = self.registers();
        if r.cs != e.condition.cs || r.ip != e.condition.ip || !r.code32() {
            return Err("different execution context");
        }
        for seg in [
            SegmentRegister::Cs,
            SegmentRegister::Ds,
            SegmentRegister::Ss,
        ] {
            let cache = r.segment_cache(seg);
            if cache.base != 0 || !cache.default32 {
                return Err("unsupported segments");
            }
        }
        let (c, value) = self
            .synchronization_condition()
            .ok_or("guard code or memory unavailable")?;
        if c != e.condition {
            return Err("guard code changed");
        }
        // Validate before refreshing; no branch outcomes or trace costs are invented.
        self.refresh_learned_guard(c, value);
        let e = self.current_learned().unwrap();
        if !e.qualified() {
            return Err("relearning counter changes");
        }
        if self.observer.result.queries == 0 && self.observer.queries == 0 {
            return Err("no input queries");
        }
        if value == 0 || value >= (1u32 << (u32::from(c.width) * 8 - 1)) {
            return Err("counter due or negative");
        }
        let horizon = self
            .machine
            .diagnostic_service_horizon()
            .0
            .ok_or("unknown service deadline")?;
        let reserve = e.recurrence_peak.max(100_000);
        self.synchronization.stats.learned_horizon_nanos = Some(horizon);
        self.synchronization.stats.learned_reserve_nanos = reserve;
        let budget = horizon.saturating_sub(reserve).min(250_000);
        if budget < 25_000 {
            return Err("service deadline too close");
        }
        Ok(budget)
    }
    pub fn synchronization_stats(&self) -> SynchronizationStats {
        let mut stats = self.synchronization.stats;
        stats.poll_waits = self.menu_idle.stats.poll_waits;
        stats.poll_cpu_peak_nanos = self.menu_idle.stats.poll_cpu_peak_nanos;
        stats.learned_ip = self
            .synchronization
            .learned
            .iter()
            .flatten()
            .max_by_key(|e| (e.qualified(), e.last_time))
            .map(|e| e.condition.ip);
        stats.learned_candidates = self.learned_idle_heads();
        (stats.horizon_nanos, stats.horizon_source) = self.machine.diagnostic_service_horizon();
        // This supported port mask has a hardware-defined next transition.
        // RAM conditions still have no established causal event mapping.
        if stats.port
            && stats.comparison == 8
            && let Ok(port) = u16::try_from(stats.operand)
        {
            stats.condition_event_nanos = self.machine.bus.vga.retrace_deadline(port);
            if let Some(deadline) = stats.condition_event_nanos
                && stats.horizon_nanos.is_none_or(|h| deadline < h)
            {
                stats.horizon_nanos = Some(deadline);
                stats.horizon_source = "VGA retrace transition";
            }
        }
        let fresh = self.emulated_nanos().saturating_sub(stats.recurrence_at) <= 250_000_000;
        if fresh
            && stats.recurrence_samples >= 4
            && stats.recurrence_mhz == self.hardware_config().cpu_mhz
        {
            stats.reserve_nanos = (stats.recurrence_peak_nanos / 4).max(100_000);
            stats.provisional_slack_nanos = stats.horizon_nanos.map(|horizon| {
                horizon
                    .saturating_sub(stats.recurrence_peak_nanos)
                    .saturating_sub(stats.reserve_nanos)
            });
            stats.provisional_slack_steps = stats.provisional_slack_nanos.map(|nanos| {
                (u128::from(nanos) * u128::from(stats.recurrence_mhz) / 4000)
                    .min(u128::from(u64::MAX)) as u64
            });
        }
        stats
    }
    fn synchronization_value(&self, address: u32, width: u8) -> Option<u32> {
        if !matches!(width, 1 | 2 | 4) {
            return None;
        }
        let mut data = [0; 4];
        self.debug_read_memory(address, &mut data[..usize::from(width)])
            .then(|| u32::from_le_bytes(data))
    }
    fn synchronization_condition(&self) -> Option<(Condition, u32)> {
        if self.machine.cpu.control.cr0 & (1 << 31) != 0 {
            return None;
        }
        let r = self.registers();
        let address = r
            .address(SegmentRegister::Cs, r.ip, 24, Access::Execute)
            .ok()?;
        let mut bytes = [0; 24];
        if !self.debug_read_memory(address, &mut bytes) {
            return None;
        }
        let mut c = decode(
            &bytes,
            r.code32(),
            r.ip,
            r.cs,
            r.segment_cache(SegmentRegister::Cs).base,
            r.get(Gpr16::Dx),
            r.get_byte(4),
        )?;
        if !c.port
            && c.not_equal
            && c.comparison == 0
            && c.target >= c.end
            && c.target - c.end <= 64
            && let Ok(target) = r.address(SegmentRegister::Cs, c.target, 1, Access::Execute)
        {
            let mut ret = [0];
            c.returns_to_caller = self.debug_read_memory(target, &mut ret) && ret == [0xc3];
        }
        let value = if c.port {
            0
        } else {
            c.operand = r
                .address(SegmentRegister::Ds, c.operand, c.width, Access::Read)
                .ok()?;
            let mut data = [0; 4];
            if !self.debug_read_memory(c.operand, &mut data[..usize::from(c.width)]) {
                return None;
            }
            u32::from_le_bytes(data)
        };
        Some((c, value))
    }
    /// A sampling burst single-steps branch conditions.
    pub(super) fn synchronization_tracing(&self) -> bool {
        self.observer.enabled && self.synchronization.remaining != 0
    }
    pub(super) fn synchronization_advance(
        &mut self,
        now: u64,
        budget: u32,
    ) -> Result<Option<u32>, RuntimeError> {
        if !self.observer.enabled || budget <= 1 || self.continuation.is_some() || self.storage_wait
        {
            self.synchronization.pending = None;
            self.synchronization.remaining = 0;
            return Ok(None);
        }
        if self.synchronization.remaining == 0 {
            if now < self.synchronization.next {
                return Ok(None);
            }
            self.synchronization.next = now.saturating_add(PERIOD);
            self.synchronization.remaining = BURST;
            // Age evidence so startup conditions do not monopolize the table.
            for entry in &mut self.synchronization.entries {
                entry.taken /= 2;
                entry.fallthrough /= 2;
            }
            self.synchronization.pending = None;
            self.idle_abandon();
            self.cancel_menu_idle();
        }
        let pc = self.registers().ip;
        let cs = self.registers().cs;
        let irq = self.machine.interrupt_pending();
        let pending = self.synchronization.pending;
        if pending.is_none() && !irq {
            self.synchronization.pending = self.synchronization_condition();
        }
        let watch = self.synchronization.stats;
        let before_value = if watch.conditions != 0 && !watch.port {
            self.synchronization_value(watch.operand, watch.width)
        } else {
            None
        };
        let before_devices = self.machine.device_accesses();
        let consumed = self.execute_step(1)?;
        if let Some(before) = before_value
            && let Some(after) = self.synchronization_value(watch.operand, watch.width)
            && before != after
        {
            let stats = &mut self.synchronization.stats;
            stats.observed_changes += 1;
            stats.change_operand = watch.operand;
            stats.change_cs = cs;
            stats.change_ip = pc;
            stats.change_with_irq = irq;
            stats.change_with_device_access = self.machine.device_accesses() != before_devices;
        }
        self.synchronization.remaining -= 1;
        self.synchronization.stats.traced_steps += u64::from(consumed);
        if irq || consumed != 1 || self.registers().cs != cs || self.continuation.is_some() {
            self.synchronization.pending = None;
        } else if let Some((c, value)) = pending {
            if pc == c.branch {
                let ip = self.registers().ip;
                if self.registers().segment_cache(SegmentRegister::Cs).base == c.base
                    && (ip == c.target || ip == c.end)
                {
                    self.synchronization.record(
                        c,
                        value,
                        ip == c.target,
                        self.emulated_nanos(),
                        self.hardware_config().cpu_mhz,
                    );
                }
                self.synchronization.pending = None;
            } else if !c.port || pc != c.ip + 1 || self.registers().ip != c.branch {
                self.synchronization.pending = None;
            }
        } else if let Some((c, value)) = self.synchronization.pending {
            let expected = if c.port { c.ip + 1 } else { c.branch };
            if self.registers().ip != expected {
                self.synchronization.pending = None;
            } else if c.port {
                self.synchronization.pending = Some((c, u32::from(self.registers().get_byte(0))));
            } else {
                self.synchronization.pending = Some((c, value));
            }
        }
        if self.adaptive_idle_enabled() {
            self.machine.set_batch_yield_ips(self.learned_idle_heads());
        }
        Ok(Some(consumed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adjacent_static_guard_does_not_interrupt_counter_learning() {
        use doslet_bus::MemoryBus;
        let mut s = super::super::cooperative::tests::session();
        s.set_adaptive_idle(true);
        s.observer.queries = 10;
        for (address, code) in [
            (
                0x10000,
                &[0x83, 0x3d, 0, 0x30, 0, 0, 0, 0x75, 1, 0x90, 0xc3][..],
            ),
            (
                0x10020,
                &[0x80, 0x3d, 4, 0x30, 0, 0, 0, 0x75, 1, 0x90, 0xc3][..],
            ),
        ] {
            for (i, byte) in code.iter().enumerate() {
                s.machine.bus.write_u8(address + i as u32, *byte).unwrap();
            }
        }
        let measured = allocation_counter::measure(|| {
            for i in 0..24 {
                // City capture: long runs of 2, interleaved with due (0) calls.
                let value = if i % 6 == 0 { 0 } else { 2 };
                s.machine.bus.write_u32(0x3000, value).unwrap();
                s.machine.cpu.registers.ip = 0x10000;
                let (c, v) = s.synchronization_condition().unwrap();
                s.synchronization.record(c, v, v != 0, i, 33);
                let budget = s.learned_idle_budget();
                if i >= 8 && value != 0 {
                    assert!(budget.is_some());
                }
                s.machine.cpu.registers.ip = 0x10020;
                let (c, v) = s.synchronization_condition().unwrap();
                s.synchronization.record(c, v, false, i, 33);
                assert!(s.learned_idle_budget().is_none());
            }
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(s.learned_idle_heads(), Some([0x10000, 0x10020]));
        s.machine.cpu.registers.ip = 0x10000;
        let cpu = s.machine.cpu.snapshot();
        assert_eq!(s.menu_idle_advance(100).unwrap(), None);
        assert!(s.menu_idle_advance(100).unwrap().is_some());
        assert!(s.machine.cpu.matches(&cpu));
    }
    #[test]
    fn arrivals_keep_learning_alive_without_trace_hits_and_relearn_after_gaps() {
        let mut e = Entry::default();
        let measured = allocation_counter::measure(|| {
            for i in 0..40 {
                e.refresh(if i % 2 == 0 { 2 } else { 1 }, i * 100_000_000, 133);
            }
        });
        assert_eq!(measured.count_total, 0);
        assert!(e.qualified()); // Four seconds with no tracer refresh.
        e.refresh(2, 6_000_000_000, 133);
        assert!(!e.qualified()); // Returning from a different screen requires evidence again.
        for i in 0..16 {
            e.refresh(2, 6_000_000_000 + i, 133);
        }
        assert!(!e.qualified()); // Repeated constant flags cannot authorize a wait.
        for i in 0..8 {
            e.refresh(if i % 2 == 0 { 1 } else { 2 }, 6_100_000_000 + i, 133);
        }
        assert!(e.qualified());
        e.refresh(1, 6_200_000_000, 66);
        assert!(!e.qualified());
    }

    #[test]
    fn learned_guard_rejects_static_flags_and_one_way_counters() {
        let c = Condition {
            code32: true,
            width: 4,
            not_equal: true,
            returns_to_caller: true,
            target: 20,
            end: 10,
            ..Default::default()
        };
        for values in [[2; 16], core::array::from_fn(|i| i as u32)] {
            let mut sync = Synchronization::default();
            for (i, value) in values.into_iter().enumerate() {
                sync.record(c, value, true, i as u64 * 100, 33);
            }
            assert!(!sync.learned[0].unwrap().qualified());
        }
    }

    #[test]
    fn foreign_context_and_unproven_guards_cannot_replace_active_guard() {
        let c = Condition {
            code32: true,
            width: 4,
            not_equal: true,
            returns_to_caller: true,
            ip: 0x10000,
            end: 0x10009,
            target: 0x1000a,
            ..Default::default()
        };
        let mut sync = Synchronization::default();
        sync.record(c, 2, true, 0, 133);
        for i in 0..8 {
            sync.learned[0]
                .as_mut()
                .unwrap()
                .refresh(i % 2 + 1, u64::from(i), 133);
        }
        assert!(sync.learned[0].unwrap().qualified());
        let foreign = Condition {
            code32: false,
            cs: 0x18,
            ip: 0x283f,
            ..c
        };
        sync.record(foreign, 2, true, 10, 133);
        assert!(sync.learned[0].unwrap().condition == c);
        let other = Condition { ip: 0x20000, ..c };
        sync.record(other, 2, true, 20, 133);
        assert!(sync.learned[0].unwrap().condition == c);
        sync.record(other, 1, true, 2_000_000_000, 133);
        assert!(sync.learned[1].unwrap().condition == other);
        let mut cold = Synchronization::default();
        cold.record(foreign, 2, true, 0, 133);
        assert!(cold.learned.iter().all(Option::is_none));
    }

    #[test]
    fn learned_guard_waits_only_after_changing_counter_evidence_and_revalidates() {
        use doslet_bus::MemoryBus;
        let mut s = super::super::cooperative::tests::session();
        s.set_adaptive_idle(true);
        s.mouse.x = 100;
        s.mouse.y = 100;
        let code = [0x83, 0x3d, 0, 0x30, 0, 0, 0, 0x75, 1, 0x90, 0xc3];
        for (i, b) in code.iter().enumerate() {
            s.machine.bus.write_u8(0x10000 + i as u32, *b).unwrap();
        }
        s.observer.queries = 10;
        s.synchronization.next = 100;
        for i in 0..8 {
            s.machine
                .bus
                .write_u32(0x3000, if i % 2 == 0 { 2 } else { 1 })
                .unwrap();
            let (c, value) = s.synchronization_condition().unwrap();
            s.synchronization.record(c, value, true, i * 100, 33);
            assert_eq!(s.learned_idle_head(), Some(0x10000));
            s.learned_idle_budget();
        }
        assert_eq!(s.learned_idle_head(), Some(0x10000));
        assert!(s.learned_idle_budget().is_some());
        let cpu = s.machine.cpu.snapshot();
        assert_eq!(s.menu_idle_advance(100).unwrap(), None);
        assert!(s.menu_idle_advance(100).unwrap().is_some());
        assert!(s.machine.cpu.matches(&cpu));
        assert!(s.menu_idle_stats().nanos > 0);
        // Location is not evidence of CPU demand; edges use the same budget.
        s.pointer_move(0, 0);
        assert!(s.learned_idle_budget().is_some());
        s.pointer_move(110, 110);
        assert_eq!(s.menu_idle_advance(100).unwrap(), None);
        s.machine.bus.write_u32(0x3000, 0).unwrap();
        assert!(s.learned_idle_budget().is_none());
        s.machine.bus.write_u32(0x3000, 2).unwrap();
        s.machine.bus.write_u8(0x1000a, 0x90).unwrap();
        assert!(s.learned_idle_budget().is_none());
        s.set_idle_observation(false);
        assert!(!s.adaptive_idle_enabled());
    }

    #[test]
    fn vga_mask_links_condition_to_transition_but_ram_does_not() {
        let mut s = Session::new(&[0xeb, 0xfe]).unwrap();
        let port = s.machine.bus.vga.input_status_port();
        s.synchronization.stats.port = true;
        s.synchronization.stats.operand = u32::from(port);
        s.synchronization.stats.comparison = 8;
        let stats = s.synchronization_stats();
        assert_eq!(
            stats.condition_event_nanos,
            s.machine.bus.vga.retrace_deadline(port)
        );
        assert!(stats.horizon_nanos <= stats.condition_event_nanos);
        s.synchronization.stats.port = false;
        assert_eq!(s.synchronization_stats().condition_event_nanos, None);
    }
    #[test]
    fn provisional_budget_reserves_work_and_margin_and_rejects_stale_clock_data() {
        let mut s = Session::new(&[0xeb, 0xfe]).unwrap();
        assert!(s.synchronization_stats().provisional_slack_nanos.is_none());
        s.synchronization.stats.recurrence_samples = 4;
        s.synchronization.stats.recurrence_peak_nanos = 200_000;
        s.synchronization.stats.recurrence_mhz = s.hardware_config().cpu_mhz;
        s.synchronization.stats.recurrence_at = s.emulated_nanos();
        let stats = s.synchronization_stats();
        assert_eq!(stats.reserve_nanos, 100_000);
        assert_eq!(
            stats.provisional_slack_nanos,
            stats.horizon_nanos.map(|n| n.saturating_sub(300_000))
        );
        s.set_cpu_mhz(66).unwrap();
        assert!(s.synchronization_stats().provisional_slack_nanos.is_none());
        s.synchronization.stats.recurrence_mhz = 66;
        s.synchronization.stats.recurrence_peak_nanos = u64::MAX / 2;
        assert_eq!(s.synchronization_stats().provisional_slack_nanos, Some(0));
    }
    #[test]
    fn recurrence_cost_never_spans_untraced_gaps_and_tracks_slow_passes() {
        let mut sync = Synchronization {
            next: 100,
            ..Default::default()
        };
        let c = Condition {
            ip: 100,
            ..Default::default()
        };
        sync.record(c, 1, true, 1000, 33);
        sync.record(c, 1, true, 2000, 33);
        sync.record(c, 1, true, 5000, 33);
        assert_eq!(sync.stats.recurrence_peak_nanos, 3000);
        assert_eq!(sync.stats.recurrence_samples, 2);
        sync.next += PERIOD;
        sync.record(c, 1, true, 1_000_000_000, 33);
        assert_eq!(sync.stats.recurrence_peak_nanos, 3000);
        sync.record(c, 1, true, 1_000_001_000, 66);
        assert_eq!(sync.stats.recurrence_samples, 0);
    }

    #[test]
    fn storage_wait_and_pause_preserve_trace_history_but_explicit_reset_clears_it() {
        let mut s = Session::new(&[0xeb, 0xfe]).unwrap();
        s.set_idle_observation(true);
        s.start();
        s.advance(1000);
        let traced = s.synchronization_stats().traced_steps;
        assert!(traced > 0);
        s.storage_wait = true;
        s.observe_idle();
        assert_eq!(s.synchronization_stats().traced_steps, traced);
        assert_eq!(s.idle_observation().reason, "waiting for I/O");
        s.storage_wait = false;
        s.pause();
        s.observe_idle();
        assert_eq!(s.synchronization_stats().traced_steps, traced);
        s.set_idle_observation(false);
        assert_eq!(s.synchronization_stats().traced_steps, 0);
    }

    #[test]
    fn byte_compare_does_not_change_near_branch_width() {
        let bytes = [
            0x80, 0x3d, 0, 2, 0, 0, 0, 0x0f, 0x85, 0xf3, 0xff, 0xff, 0xff,
        ];
        let c = decode(&bytes, true, 0, 0, 0, 0, 0).unwrap();
        assert_eq!((c.width, c.branch, c.end, c.target), (1, 7, 13, 0));
        assert!(decode(&bytes[..12], true, 0, 0, 0, 0, 0).is_none());
        let mut prefixed = [0; 14];
        prefixed[0] = 0x66;
        prefixed[1..].copy_from_slice(&bytes);
        prefixed[1] = 0x83;
        prefixed[10] = 0xf2;
        let c = decode(&prefixed, true, 0, 0, 0, 0, 0).unwrap();
        assert_eq!((c.width, c.end, c.target), (2, 14, 0));
    }
    #[test]
    fn memory_predicate_records_branches_and_change_boundary_without_changing_guest() {
        let code = [
            0x80, 0x3e, 0, 2, 0, 0x75, 0xf9, // CMP byte [200],0; JNZ head
            0xc6, 0x06, 0, 2, 1, 0xeb, 0xf2,
        ]; // MOV byte [200],1; JMP head
        let mut observed = Session::new(&code).unwrap();
        let mut reference = Session::new(&code).unwrap();
        for s in [&mut observed, &mut reference] {
            s.set_idle_loops(false);
            s.start();
        }
        observed.set_idle_observation(true);
        let measured = allocation_counter::measure(|| {
            assert_eq!(observed.advance(10000), reference.advance(10000));
        });
        assert_eq!(measured.count_total, 0);
        let stats = observed.synchronization_stats();
        assert!(stats.taken > 2 && stats.fallthrough == 1, "{stats:?}");
        assert_eq!(stats.observed_changes, 1);
        assert_eq!(stats.change_ip, 0x107);
        assert_eq!(stats.value_changes, 1);
        assert!(!stats.port);
        assert!(
            observed
                .machine
                .cpu
                .matches(&reference.machine.cpu.snapshot())
        );
        assert_eq!(observed.emulated_nanos(), reference.emulated_nanos());
        assert_eq!(observed.steps(), reference.steps());
        assert_eq!(
            observed.machine.bus.ram_view(0..0xa0000),
            reference.machine.bus.ram_view(0..0xa0000)
        );
    }
    #[test]
    fn port_trace_observes_actual_reads_without_extra_device_accesses() {
        let code = [0xba, 0xda, 3, 0xec, 0xa8, 8, 0x74, 0xfb, 0xeb, 0xf9];
        let mut observed = Session::new(&code).unwrap();
        let mut reference = Session::new(&code).unwrap();
        for s in [&mut observed, &mut reference] {
            s.set_idle_loops(false);
            s.set_vga_wait_acceleration(false);
            s.start();
        }
        observed.set_idle_observation(true);
        observed.advance(10000);
        reference.advance(10000);
        let stats = observed.synchronization_stats();
        assert!(
            stats.port && stats.operand == 0x3da && stats.conditions > 2,
            "{stats:?}"
        );
        assert_eq!(stats.comparison, 8);
        assert_eq!(
            observed.machine.device_accesses(),
            reference.machine.device_accesses()
        );
        assert_eq!(observed.emulated_nanos(), reference.emulated_nanos());
        assert!(
            observed
                .machine
                .cpu
                .matches(&reference.machine.cpu.snapshot())
        );
    }
}
