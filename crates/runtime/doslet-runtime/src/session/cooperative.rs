//! Explicit, caller-selected menu polling experiment. Unlike idle proofs this
//! changes polling cadence; it never claims instruction/state equivalence.
use super::{RuntimeError, Session, idle::InputState};
use doslet_x86::{Access, CpuSnapshot, Gpr32, SegmentRegister};

#[derive(Clone, Copy)]
pub struct MenuIdleSite {
    pub head: u32,
    pub call: u32,
    pub backedge: u32,
    pub event_offset: u32,
    pub callback_offset: u32,
    pub callback: u32,
    pub timer: u32,
    pub guard: u32,
}

/// Caller-selected, completed event-list traversal. This remains a heuristic.
#[derive(Clone, Copy)]
pub struct EventIdleSite {
    pub head: u32,
    pub call: u32,
    pub backedge: u32,
    pub queue: u32,
    pub context: u32,
}

const MIN_WAIT: u64 = 125_000;
const MAX_WAIT: u64 = 1_000_000;
const INPUT_GRACE: u64 = 100_000_000;
const VIDEO_GRACE: u64 = 2_000_000;

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use doslet_bus::MemoryBus;
    use doslet_x86::flags;
    #[test]
    fn empty_poll_budget_subtracts_own_waits_and_backs_off_for_work_and_deadlines() {
        let mut meter = PollBudget::default();
        for i in 0..12 {
            meter.observe(i * 300_000, i * 200_000, 33);
        }
        assert_eq!(meter.budget(3_300_000, 33, 1_000_000), Some(150_000));
        assert_eq!(meter.budget(3_300_000, 33, 1_000_000), None);
        meter.observe(3_600_000, 2_400_000, 33);
        assert_eq!(meter.budget(3_600_000, 33, 200_000), None);
        meter.observe(4_600_000, 2_400_000, 33);
        assert_eq!(meter.budget(4_600_000, 33, 2_000_000), None);
        meter.observe(4_700_000, 2_400_000, 66);
        assert_eq!(meter.samples, 0);
    }
    #[test]
    fn general_empty_mouse_poll_waits_without_game_addresses_and_remains_allocation_free() {
        // Repeated empty motion reads interleaved with RAM writes cannot use a strict proof.
        let mut s =
            Session::new(&[0xb8, 0x0b, 0, 0xcd, 0x33, 0xff, 0x06, 0, 2, 0xeb, 0xf5]).unwrap();
        s.set_adaptive_idle(true);
        s.start();
        let measured = allocation_counter::measure(|| {
            for _ in 0..200 {
                s.advance(10000);
            }
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(s.status(), super::super::SessionStatus::Running);
        assert!(
            s.menu_idle_stats().poll_waits > 0,
            "{:?}",
            s.menu_idle_stats()
        );
        assert!(s.menu_idle_stats().nanos > 0);
        s.pointer_move(20, 20);
        assert_eq!(s.menu_idle.poll.samples, 0);
        let before = s.menu_idle.stats.poll_waits;
        s.advance(10000);
        assert_eq!(s.menu_idle.stats.poll_waits, before);
        s.set_adaptive_idle(false);
        assert!(s.menu_idle.poll.last.is_none());
    }
    pub(crate) fn session() -> Session {
        let mut s = Session::new(&[0xeb, 0xfe]).unwrap();
        let m = &mut s.machine;
        m.bus.set_a20(true);
        m.cpu.registers.cs = 0;
        m.cpu.registers.ip = 0;
        for (address, bytes) in [
            (0x818, &[0xff, 0xff, 0, 0, 0, 0x9a, 0xcf, 0][..]),
            (0x820, &[0xff, 0xff, 0, 0, 0, 0x92, 0xcf, 0][..]),
            (
                0,
                &[
                    0xb8, 0x15, 0, 0x0f, 1, 0xf0, 0x90, 0x66, 0xea, 0, 0, 1, 0, 0x18, 0,
                ][..],
            ),
            (0x10000, &[0xe8, 0xfb, 0, 0, 0][..]),
            (0x10010, &[0x0f, 0x84, 0xea, 0xff, 0xff, 0xff][..]),
        ] {
            for (i, byte) in bytes.iter().enumerate() {
                m.bus.write_u8(address + i as u32, *byte).unwrap();
            }
        }
        m.cpu.control.gdtr.base = 0x800;
        m.cpu.control.gdtr.limit = 0x7f;
        for _ in 0..4 {
            m.cpu.step(&mut m.bus).unwrap();
        }
        for seg in [SegmentRegister::Ds, SegmentRegister::Ss] {
            m.cpu.load_segment(&mut m.bus, seg, 0x20).unwrap();
        }
        m.bus.write_u8(0x5000, 0x90).unwrap();
        m.cpu.registers.ip = 0x5000;
        m.cpu.step(&mut m.bus).unwrap();
        m.cpu.registers.ip = 0x10000;
        m.cpu.registers.set32(Gpr32::Esp, 0x9000);
        m.cpu.registers.set32(Gpr32::Edi, 0);
        m.cpu.flags = flags::INTERRUPT | 2;
        m.bus.write_u16(0x9010, 0).unwrap();
        m.bus.write_u32(0x9028, 0x12000).unwrap();
        m.bus.write_u32(0x3000, 2).unwrap();
        m.bus.write_u8(0x3004, 0).unwrap();
        s.set_menu_idle_site(Some(MenuIdleSite {
            head: 0x10000,
            call: 0x10100,
            backedge: 0x10010,
            event_offset: 0x10,
            callback_offset: 0x28,
            callback: 0x12000,
            timer: 0x3000,
            guard: 0x3004,
        }));
        s
    }
    #[test]
    fn explicit_menu_wait_preserves_cpu_and_cancels_on_writes_debug_and_deadline() {
        let mut s = session();
        assert!(s.menu_candidate(s.menu_idle.site.unwrap()).is_some());
        let cpu = s.machine.cpu.snapshot();
        assert_eq!(s.menu_idle_advance(10000).unwrap(), None);
        assert!(s.menu_idle_advance(10000).unwrap().unwrap() > 0);
        assert!(s.machine.cpu.matches(&cpu));
        assert!(s.menu_idle_stats().nanos > 0);
        s.machine.bus.write_u8(0x4000, 1).unwrap();
        assert_eq!(s.menu_idle_advance(10000).unwrap(), None);
        s.set_idle_loops(false);
        assert_eq!(s.menu_idle_advance(10000).unwrap(), None);
        s.set_idle_loops(true);
        assert_eq!(s.menu_idle_advance(1).unwrap(), None);
        let before = s.emulated_nanos();
        assert_eq!(s.menu_idle_advance(1000).unwrap(), None);
        while s.menu_idle_advance(1000).unwrap().is_some() {}
        assert!((MIN_WAIT..=MIN_WAIT + 122).contains(&(s.emulated_nanos() - before)));
    }
    #[test]
    fn menu_rejects_event_changed_code_callback_due_timer_and_bad_stack() {
        for case in 0..6 {
            let mut s = session();
            match case {
                0 => s.machine.bus.write_u16(0x9010, 1).unwrap(),
                1 => s.machine.bus.write_u8(0x10000, 0x90).unwrap(),
                2 => s.machine.bus.write_u32(0x9028, 0x12004).unwrap(),
                3 => s.machine.bus.write_u32(0x3000, 0).unwrap(),
                4 => s.machine.cpu.registers.set32(Gpr32::Esp, u32::MAX),
                _ => s.machine.bus.write_u8(0x3004, 1).unwrap(),
            }
            for _ in 0..3 {
                assert_eq!(s.menu_idle_advance(10000).unwrap(), None, "case={case}");
            }
            assert_eq!(s.menu_idle_stats().steps, 0);
        }
    }
    #[test]
    fn adaptive_wait_grows_is_bounded_and_backs_off_on_video_and_input() {
        let mut s = session();
        for _ in 0..40 {
            // Model successive completed empty iterations at the same head.
            s.menu_idle.parked = None;
            s.menu_idle_advance(100).unwrap();
        }
        assert_eq!(s.menu_idle_stats().interval_nanos, MAX_WAIT);
        s.menu_idle.parked = None;
        s.menu_idle.video_revision = Some(s.presentation_frame().revision().wrapping_add(1));
        assert_eq!(s.menu_idle_advance(100).unwrap(), None);
        assert_eq!(s.menu_idle_stats().interval_nanos, MIN_WAIT);
        assert_eq!(s.menu_idle.active_until, s.emulated_nanos() + VIDEO_GRACE);
        s.pointer_move(30, 30);
        assert_eq!(s.menu_idle.active_until, s.emulated_nanos() + INPUT_GRACE);
        let time = s.emulated_nanos();
        for _ in 0..10 {
            assert_eq!(s.menu_idle_advance(100).unwrap(), None);
        }
        assert_eq!(s.emulated_nanos(), time);
        assert_eq!(s.hardware_config().cpu_mhz, 33);
        s.menu_idle.active_until = time;
        assert_eq!(s.menu_idle_advance(100).unwrap(), None);
        assert!(s.menu_idle_advance(100).unwrap().is_some());
    }
    #[test]
    fn completed_event_loop_requires_exact_code_context_and_non_scrolling_pointer() {
        let mut s = session();
        let site = EventIdleSite {
            head: 0x11000,
            call: 0x11100,
            backedge: 0x11020,
            queue: 0x3000,
            context: 0x4000,
        };
        for (address, bytes) in [
            (
                0x11000,
                &[0xb8, 0, 0x30, 0, 0, 0x89, 0xe6, 0xe8, 0xf4, 0, 0, 0][..],
            ),
            (0x11020, &[0xe9, 0xdb, 0xff, 0xff, 0xff][..]),
        ] {
            for (i, b) in bytes.iter().enumerate() {
                s.machine.bus.write_u8(address + i as u32, *b).unwrap();
            }
        }
        s.machine.cpu.registers.ip = site.head;
        s.machine.cpu.registers.set32(Gpr32::Ebx, site.context);
        s.machine.cpu.registers.set32(Gpr32::Ecx, 0);
        s.mouse.x = 100;
        s.mouse.y = 100;
        s.set_event_idle_site(Some(site));
        assert!(s.event_candidate(site).is_some());
        for case in 0..6 {
            match case {
                0 => s.machine.bus.write_u16(0x9000, 1).unwrap(),
                1 => s.machine.bus.write_u32(0x4008, 1).unwrap(),
                2 => s.machine.cpu.registers.set32(Gpr32::Ecx, 1),
                3 => s.mouse.x = 0,
                4 => s.mouse.buttons = 1,
                _ => s.machine.bus.write_u8(site.head, 0x90).unwrap(),
            }
            assert!(s.event_candidate(site).is_none(), "case={case}");
            s.machine.bus.write_u16(0x9000, 0).unwrap();
            s.machine.bus.write_u32(0x4008, 0).unwrap();
            s.machine.cpu.registers.set32(Gpr32::Ecx, 0);
            s.mouse.x = 100;
            s.mouse.buttons = 0;
        }
    }

    #[test]
    fn heroes_map_short_backedge_matches_live_code_and_rejects_wrong_target() {
        let mut s = session();
        let site = EventIdleSite {
            head: 0x219d1a,
            call: 0x213f4c,
            backedge: 0x219d8f,
            queue: 0x2832f0,
            context: 0x2832c4,
        };
        // Bytes and state captured at the map loop head from the running game.
        for (address, bytes) in [
            (
                site.head,
                &[
                    0xb8, 0xf0, 0x32, 0x28, 0, 0x89, 0xe6, 0xe8, 0x26, 0xa2, 0xff, 0xff,
                ][..],
            ),
            (site.backedge, &[0xeb, 0x89][..]),
        ] {
            for (i, byte) in bytes.iter().enumerate() {
                s.machine.bus.write_u8(address + i as u32, *byte).unwrap();
            }
        }
        s.machine.cpu.registers.ip = site.head;
        s.machine.cpu.registers.set32(Gpr32::Ebx, site.context);
        s.machine.cpu.registers.set32(Gpr32::Ecx, 0x100);
        s.mouse.x = 100;
        s.mouse.y = 100;
        s.set_event_idle_site(Some(site));
        assert!(s.event_candidate(site).is_some());
        assert_eq!(s.menu_idle_advance(100).unwrap(), None);
        assert!(s.menu_idle_advance(100).unwrap().is_some());
        s.machine.bus.write_u8(site.backedge + 1, 0x88).unwrap();
        assert!(s.event_candidate(site).is_none());
        s.machine.bus.write_u8(site.backedge, 0x90).unwrap();
        assert!(s.event_candidate(site).is_none());
    }

    #[test]
    fn adaptive_observation_and_wait_do_not_allocate() {
        let mut s = session();
        let measured = allocation_counter::measure(|| {
            for _ in 0..40 {
                s.menu_idle.parked = None;
                s.menu_idle_advance(100).unwrap();
            }
        });
        assert!(s.menu_idle_stats().nanos > 0);
        assert_eq!(measured.count_total, 0);
        assert_eq!(measured.bytes_total, 0);
    }
    #[test]
    fn ineligible_cpu_cannot_park_or_advance_time() {
        let mut s = session();
        s.machine.cpu.flags &= !flags::INTERRUPT;
        let before = s.emulated_nanos();
        for _ in 0..4 {
            assert_eq!(s.menu_idle_advance(1000).unwrap(), None);
        }
        assert_eq!(s.emulated_nanos(), before);
        assert_eq!(s.menu_idle_stats().nanos, 0);
        assert!(s.menu_idle.parked.is_none());
    }

    #[test]
    fn input_wakes_parked_menu_before_more_time_passes() {
        let mut s = session();
        s.menu_idle_advance(1000).unwrap();
        assert!(s.menu_idle_advance(1000).unwrap().is_some());
        let time = s.emulated_nanos();
        s.pointer_move(20, 20);
        assert_eq!(s.menu_idle_advance(1000).unwrap(), None);
        assert_eq!(s.emulated_nanos(), time);
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct MenuIdleStats {
    pub poll_waits: u64,
    pub poll_cpu_peak_nanos: u64,
    pub waits: u64,
    pub steps: u64,
    pub nanos: u64,
    pub interval_nanos: u64,
    pub input_wakes: u64,
    pub work_wakes: u64,
    pub interrupt_wakes: u64,
    pub last_head: u32,
}

/// Experimental throughput budget at confirmed empty relative-motion queries.
/// CPU work between queries excludes our own waits; this is not an idle proof.
#[derive(Default)]
struct PollBudget {
    last: Option<(u64, u64, u16)>,
    peak: u64,
    samples: u8,
    pending: bool,
}
impl PollBudget {
    fn observe(&mut self, now: u64, idle: u64, mhz: u16) {
        if let Some((then, old_idle, old_mhz)) = self.last {
            let elapsed = now.saturating_sub(then);
            if elapsed > 250_000_000 || old_mhz != mhz {
                *self = Self::default();
            } else {
                if self.samples == 32 {
                    self.samples = 0;
                    self.peak = 0;
                }
                self.peak = self
                    .peak
                    .max(elapsed.saturating_sub(idle.saturating_sub(old_idle)));
                self.samples += 1;
            }
        }
        self.last = Some((now, idle, mhz));
        self.pending = true;
    }
    fn budget(&mut self, now: u64, mhz: u16, horizon: u64) -> Option<u64> {
        if !core::mem::take(&mut self.pending) || self.samples < 8 {
            return None;
        }
        let (then, _, old_mhz) = self.last?;
        if now.saturating_sub(then) > 1_000_000 || old_mhz != mhz {
            return None;
        }
        // Keep at least 4,000 polls/guest second when work is cheap, and reserve
        // two observed work intervals before the next known service deadline.
        let slack = 250_000u64
            .saturating_sub(self.peak)
            .min(horizon.saturating_sub(self.peak.saturating_mul(2).max(100_000)));
        (slack >= 25_000).then_some(slack)
    }
}

struct Parked {
    cpu: CpuSnapshot,
    input: InputState,
    writes: u64,
    until: u64,
}

#[derive(Default)]
pub(super) struct MenuIdle {
    poll: PollBudget,
    pub(super) adaptive: bool,
    site: Option<MenuIdleSite>,
    event_site: Option<EventIdleSite>,
    quiet: u8,
    interval: u64,
    active_until: u64,
    video_revision: Option<u64>,
    parked: Option<Parked>,
    observations: u8,
    pub(super) stats: MenuIdleStats,
}

impl Session {
    pub(super) fn note_empty_motion_poll(&mut self) {
        if self.menu_idle.adaptive {
            self.menu_idle.poll.observe(
                self.emulated_nanos(),
                self.menu_idle.stats.nanos,
                self.hardware_config().cpu_mhz,
            );
            self.menu_idle.stats.poll_cpu_peak_nanos = self.menu_idle.poll.peak;
        }
    }
    /// General countdown-guard heuristic; mutually exclusive with fixed recipes.
    pub fn set_adaptive_idle(&mut self, enabled: bool) {
        if enabled && self.analyzer.controller {
            self.set_adaptive_cpu(false);
        }
        self.set_menu_idle_site(None);
        self.set_idle_observation(true);
        self.menu_idle.adaptive = enabled;
    }
    pub fn adaptive_idle_enabled(&self) -> bool {
        self.menu_idle.adaptive
    }
    pub fn set_menu_idle_site(&mut self, site: Option<MenuIdleSite>) {
        if site.is_some() && self.analyzer.controller {
            self.set_adaptive_cpu(false);
        }
        self.machine.set_batch_yield_ip(site.map(|s| s.head));
        self.menu_idle = MenuIdle {
            site,
            ..Default::default()
        };
    }
    /// Add a second known event loop without changing the ordinary CPU path.
    pub fn set_event_idle_site(&mut self, site: Option<EventIdleSite>) {
        if site.is_some() && self.analyzer.controller {
            self.set_adaptive_cpu(false);
        }
        self.cancel_menu_idle();
        self.menu_idle.event_site = site;
        let heads = match (self.menu_idle.site, site) {
            (Some(menu), Some(event)) => Some([menu.head, event.head]),
            (Some(menu), None) => Some([menu.head; 2]),
            (None, Some(event)) => Some([event.head; 2]),
            (None, None) => None,
        };
        self.machine.set_batch_yield_ips(heads);
    }
    pub(super) fn menu_idle_input(&mut self) {
        self.observer.input();
        if self.menu_idle_enabled() {
            self.menu_idle.poll = PollBudget::default();
            self.cancel_menu_idle();
            self.menu_idle.active_until = self.emulated_nanos().saturating_add(INPUT_GRACE);
            self.menu_idle.stats.input_wakes += 1;
        }
    }
    pub fn menu_idle_stats(&self) -> MenuIdleStats {
        self.menu_idle.stats
    }
    /// Parking holds the CPU at a head; work there must wake it normally.
    pub(super) fn menu_idle_parked(&self) -> bool {
        self.menu_idle.parked.is_some()
    }
    pub fn menu_idle_enabled(&self) -> bool {
        self.menu_idle.adaptive
            || self.menu_idle.site.is_some()
            || self.menu_idle.event_site.is_some()
    }
    pub(super) fn cancel_menu_idle(&mut self) {
        self.menu_idle.parked = None;
        self.menu_idle.observations = 0;
        self.menu_idle.quiet = 0;
        self.menu_idle.interval = MIN_WAIT;
        self.menu_idle.stats.interval_nanos = MIN_WAIT;
    }
    fn menu_ram(&self, segment: SegmentRegister, offset: u32, length: u8) -> Option<&[u8]> {
        let address = self
            .machine
            .cpu
            .registers
            .address(
                segment,
                offset,
                length,
                if segment == SegmentRegister::Cs {
                    Access::Execute
                } else {
                    Access::Read
                },
            )
            .ok()?;
        self.machine
            .bus
            .ram_view(address as usize..address.checked_add(u32::from(length))? as usize)
    }
    fn menu_candidate(&self, site: MenuIdleSite) -> Option<()> {
        let r = self.registers();
        if !r.code32() || r.ip != site.head || r.get32(Gpr32::Edi) & 0xffff != 0 {
            return None;
        }
        // This recipe requires flat 32-bit code/data/stack, not aliases.
        for segment in [
            SegmentRegister::Cs,
            SegmentRegister::Ds,
            SegmentRegister::Ss,
        ] {
            let cache = r.segment_cache(segment);
            if cache.base != 0 || !cache.default32 {
                return None;
            }
        }
        let call = site
            .call
            .wrapping_sub(site.head.wrapping_add(5))
            .to_le_bytes();
        if self.menu_ram(SegmentRegister::Cs, site.head, 5)?
            != [0xe8, call[0], call[1], call[2], call[3]]
        {
            return None;
        }
        let branch = site
            .head
            .wrapping_sub(site.backedge.wrapping_add(6))
            .to_le_bytes();
        if self.menu_ram(SegmentRegister::Cs, site.backedge, 6)?
            != [0x0f, 0x84, branch[0], branch[1], branch[2], branch[3]]
        {
            return None;
        }
        let stack = r.get32(Gpr32::Esp);
        if self.menu_ram(
            SegmentRegister::Ss,
            stack.checked_add(site.event_offset)?,
            2,
        )? != [0, 0]
            || self.menu_ram(
                SegmentRegister::Ss,
                stack.checked_add(site.callback_offset)?,
                4,
            )? != site.callback.to_le_bytes()
            || self.menu_ram(SegmentRegister::Ds, site.guard, 1)? != [0]
        {
            return None;
        }
        let timer = u32::from_le_bytes(
            self.menu_ram(SegmentRegister::Ds, site.timer, 4)?
                .try_into()
                .ok()?,
        );
        (timer > 0 && timer <= 2).then_some(())
    }
    fn event_backedge(&self, site: EventIdleSite, near: [u8; 4]) -> bool {
        let Some(bytes) = self.menu_ram(SegmentRegister::Cs, site.backedge, 2) else {
            return false;
        };
        match bytes[0] {
            0xeb => {
                site.backedge
                    .wrapping_add(2)
                    .wrapping_add_signed(i32::from(bytes[1] as i8))
                    == site.head
            }
            0xe9 => {
                self.menu_ram(SegmentRegister::Cs, site.backedge, 5)
                    == Some([0xe9, near[0], near[1], near[2], near[3]].as_slice())
            }
            _ => false,
        }
    }
    fn event_candidate(&self, site: EventIdleSite) -> Option<()> {
        let r = self.registers();
        if !r.code32()
            || r.ip != site.head
            || r.get32(Gpr32::Ecx) & 0xff != 0
            || r.get32(Gpr32::Ebx) != site.context
        {
            return None;
        }
        for segment in [
            SegmentRegister::Cs,
            SegmentRegister::Ds,
            SegmentRegister::Ss,
        ] {
            let cache = r.segment_cache(segment);
            if cache.base != 0 || !cache.default32 {
                return None;
            }
        }
        let q = site.queue.to_le_bytes();
        let call = site
            .call
            .wrapping_sub(site.head.wrapping_add(12))
            .to_le_bytes();
        let branch = site
            .head
            .wrapping_sub(site.backedge.wrapping_add(5))
            .to_le_bytes();
        if self.menu_ram(SegmentRegister::Cs, site.head, 12)?
            != [
                0xb8, q[0], q[1], q[2], q[3], 0x89, 0xe6, 0xe8, call[0], call[1], call[2], call[3],
            ]
            || !self.event_backedge(site, branch)
            || self.menu_ram(SegmentRegister::Ss, r.get32(Gpr32::Esp), 2)? != [0, 0]
            || self.menu_ram(SegmentRegister::Ds, site.context.checked_add(8)?, 4)? != [0; 4]
        {
            return None;
        }
        // A stationary pointer at an edge can continuously scroll the map.
        let ((left, right), (top, bottom)) = self.pointer_ranges();
        let (x, y) = self.pointer_position();
        if self.mouse.buttons != 0
            || x <= left.saturating_add(8)
            || x >= right.saturating_sub(8)
            || y <= top.saturating_add(8)
            || y >= bottom.saturating_sub(8)
        {
            return None;
        }
        Some(())
    }
    pub(super) fn menu_idle_advance(&mut self, budget: u32) -> Result<Option<u32>, RuntimeError> {
        if !self.menu_idle_enabled() {
            return Ok(None);
        }
        if budget <= 1 || !self.idle.enabled || self.continuation.is_some() || self.storage_wait {
            self.cancel_menu_idle();
            return Ok(None);
        }
        let now = self.emulated_nanos();
        if now < self.menu_idle.active_until {
            return Ok(None);
        }
        if let Some(parked) = &self.menu_idle.parked {
            if self.input_state() != parked.input {
                self.menu_idle_input();
                return Ok(None);
            }
            if !self.machine.cpu.matches(&parked.cpu) || self.machine.ram_writes() != parked.writes
            {
                self.cancel_menu_idle();
                self.menu_idle.stats.work_wakes += 1;
                return Ok(None);
            }
            if now >= parked.until {
                self.menu_idle.parked = None;
                return Ok(None); // Execute a real polling iteration before parking again.
            }
        } else {
            let ip = self.registers().ip;
            let poll_budget = if self.menu_idle.adaptive
                && self.machine.cpu.accepts_maskable_interrupt()
                && !self.machine.interrupt_pending()
            {
                let horizon = self.machine.diagnostic_service_horizon().0.unwrap_or(0);
                self.menu_idle
                    .poll
                    .budget(now, self.hardware_config().cpu_mhz, horizon)
            } else {
                None
            };
            let learned_budget = if self.menu_idle.adaptive {
                if let Some(poll) = poll_budget {
                    Some(poll)
                } else if self.learned_idle_head() == Some(ip) {
                    self.learned_idle_budget()
                } else {
                    return Ok(None);
                }
            } else {
                None
            };
            let candidate = if self.menu_idle.adaptive {
                learned_budget.map(|_| ())
            } else if let Some(site) = self.menu_idle.site.filter(|s| s.head == ip) {
                self.menu_candidate(site)
            } else if let Some(site) = self.menu_idle.event_site.filter(|s| s.head == ip) {
                self.event_candidate(site)
            } else {
                return Ok(None);
            };
            if candidate.is_none() {
                self.cancel_menu_idle();
                self.menu_idle.stats.work_wakes += 1;
                return Ok(None);
            }
            let revision = self.presentation_frame().revision();
            if !self.menu_idle.adaptive
                && self
                    .menu_idle
                    .video_revision
                    .replace(revision)
                    .is_some_and(|old| old != revision)
            {
                self.cancel_menu_idle();
                self.menu_idle.active_until = now.saturating_add(VIDEO_GRACE);
                self.menu_idle.stats.work_wakes += 1;
                return Ok(None);
            }
            if self.menu_idle.stats.last_head != ip {
                self.cancel_menu_idle();
                self.menu_idle.stats.last_head = ip;
            }
            self.menu_idle.quiet = self.menu_idle.quiet.saturating_add(1);
            if let Some(poll) = poll_budget {
                self.menu_idle.quiet = 2;
                self.menu_idle.interval = poll;
                self.menu_idle.stats.poll_waits += 1;
            }
            if self.menu_idle.quiet < 2 {
                return Ok(None);
            }
            if self.menu_idle.quiet >= 8 {
                self.menu_idle.interval = (self.menu_idle.interval * 2).clamp(MIN_WAIT, MAX_WAIT);
                self.menu_idle.quiet = 2;
            }
            self.menu_idle.interval = self.menu_idle.interval.max(MIN_WAIT);
            if let Some(limit) = learned_budget {
                self.menu_idle.interval = self.menu_idle.interval.min(limit);
            }
            self.menu_idle.stats.interval_nanos = self.menu_idle.interval;
            self.idle_abandon();
            self.menu_idle.observations = 0;
            self.menu_idle.parked = Some(Parked {
                cpu: self.machine.cpu.snapshot(),
                input: self.input_state(),
                writes: self.machine.ram_writes(),
                until: now.saturating_add(self.menu_idle.interval),
            });
        }
        let until = self.menu_idle.parked.as_ref().expect("parked above").until;
        let steps = self.machine.advance_cooperative_idle(budget, until - now)?;
        if steps == 0 {
            // Hardware IRQs must run promptly, but are not evidence of user work.
            self.menu_idle.parked = None;
            self.menu_idle.stats.interrupt_wakes += 1;
            return Ok(None);
        }
        if self.menu_idle.observations == 0 {
            self.menu_idle.stats.waits += 1;
            self.menu_idle.observations = 1;
        }
        self.menu_idle.stats.steps += u64::from(steps);
        self.menu_idle.stats.nanos += self.emulated_nanos() - now;
        Ok(Some(steps))
    }
}
