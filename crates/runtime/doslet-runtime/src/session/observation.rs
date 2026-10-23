//! Bounded, address-independent observations; never authorizes skipping work.
use super::{Session, SessionStatus};

const WINDOW: u64 = 250_000_000;
const SAMPLE_STEPS: u64 = 20_011; // Minimum interval; sampling may still alias guest loops.

#[derive(Clone, Copy, Debug, Default)]
pub struct IdleObservation {
    pub enabled: bool,
    pub candidate: bool,
    pub reason: &'static str,
    pub samples: u32,
    pub repeated: u32,
    pub cs: u16,
    pub ip: u32,
    pub queries: u64,
    pub writes: u64,
    pub device_accesses: u64,
}

#[derive(Clone, Copy, Default, PartialEq)]
struct Site {
    cs: u16,
    ip: u32,
    base: u32,
    code32: bool,
}
#[derive(Clone, Copy, Default)]
struct Entry {
    site: Site,
    count: u32,
}

pub(super) struct Observer {
    pub(super) enabled: bool,
    next_sample: u64,
    start: Option<(u64, u64, u64, u64)>,
    entries: [Entry; 64],
    samples: u32,
    pub(super) queries: u64,
    input: bool,
    display_changed: bool,
    pub(super) result: IdleObservation,
}
impl Default for Observer {
    fn default() -> Self {
        Self {
            enabled: false,
            next_sample: 0,
            start: None,
            entries: [Entry::default(); 64],
            samples: 0,
            queries: 0,
            input: false,
            display_changed: false,
            result: IdleObservation::default(),
        }
    }
}
impl Observer {
    pub(super) fn input(&mut self) {
        if self.enabled {
            self.input = true;
        }
    }
    pub(super) fn query(&mut self) {
        if self.enabled {
            self.queries += 1;
        }
    }
    fn record(&mut self, site: Site) {
        self.samples += 1;
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| e.count != 0 && e.site == site)
        {
            entry.count += 1;
        } else if let Some(entry) = self.entries.iter_mut().find(|e| e.count == 0) {
            *entry = Entry { site, count: 1 };
        }
        // Saturation drops unknown sites rather than inflating recurrence counts.
    }
    fn finish(&mut self, writes: u64, devices: u64) {
        let top = self
            .entries
            .iter()
            .max_by_key(|e| e.count)
            .copied()
            .unwrap_or_default();
        let repeated = top.count >= 4 && self.samples >= 8;
        let candidate = repeated && self.queries > 0 && !self.input && !self.display_changed;
        let reason = if self.input {
            "input activity"
        } else if self.display_changed {
            "display changed"
        } else if !repeated {
            "insufficient recurrence"
        } else if self.queries == 0 {
            "repeated code without input queries"
        } else if devices != 0 {
            "polling candidate: device effects unverified"
        } else if writes != 0 {
            "polling candidate: RAM effects unverified"
        } else {
            "polling candidate: sampled evidence only"
        };
        self.result = IdleObservation {
            enabled: true,
            candidate,
            reason,
            samples: self.samples,
            repeated: top.count,
            cs: top.site.cs,
            ip: top.site.ip,
            queries: self.queries,
            writes,
            device_accesses: devices,
        };
        self.entries.fill(Entry::default());
        self.samples = 0;
        self.queries = 0;
        self.input = false;
        self.display_changed = false;
    }
}
impl Session {
    /// Observe sampled recurrence and input queries without changing execution policy.
    pub fn set_idle_observation(&mut self, enabled: bool) {
        let enabled = enabled && !self.v86_enabled();
        self.synchronization = super::synchronization::Synchronization::default();
        if self.adaptive_idle_enabled() {
            self.cancel_menu_idle();
            self.machine.set_batch_yield_ip(None);
            if !enabled {
                self.menu_idle.adaptive = false;
            }
        }
        self.observer = Observer {
            enabled,
            result: IdleObservation {
                enabled,
                reason: if enabled { "collecting" } else { "off" },
                ..Default::default()
            },
            ..Default::default()
        };
    }
    pub fn idle_observation(&self) -> IdleObservation {
        self.observer.result
    }
    pub(super) fn observe_idle(&mut self) {
        if !self.observer.enabled {
            return;
        }
        if self.status != SessionStatus::Running || self.storage_wait {
            // Discard the interrupted sampling window, not synchronization history.
            self.observer = Observer {
                enabled: true,
                result: IdleObservation {
                    enabled: true,
                    reason: if self.storage_wait {
                        "waiting for I/O"
                    } else {
                        "not running"
                    },
                    ..Default::default()
                },
                ..Default::default()
            };
            return;
        }
        if self.steps < self.observer.next_sample {
            return;
        }
        self.observer.next_sample = self.steps.saturating_add(SAMPLE_STEPS);
        let now = self.emulated_nanos();
        let writes = self.machine.ram_writes();
        let devices = self.machine.device_accesses();
        let revision = self.presentation_frame().revision();
        let r = self.registers();
        let site = Site {
            cs: r.cs,
            ip: r.ip,
            base: r.segment_cache(doslet_x86::SegmentRegister::Cs).base,
            code32: r.code32(),
        };
        let (began, old_writes, old_devices, old_revision) = *self
            .observer
            .start
            .get_or_insert((now, writes, devices, revision));
        self.observer.display_changed |= revision != old_revision;
        self.observer.record(site);
        if now.saturating_sub(began) >= WINDOW {
            self.observer.finish(
                writes.wrapping_sub(old_writes),
                devices.wrapping_sub(old_devices),
            );
            self.observer.start = Some((now, writes, devices, revision));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recurrence_needs_queries_and_quiet_input_and_display() {
        for case in 0..5 {
            let mut o = Observer {
                enabled: true,
                ..Default::default()
            };
            for _ in 0..8 {
                o.record(Site {
                    ip: 123,
                    ..Default::default()
                });
            }
            if case != 0 {
                o.query();
            }
            if case == 2 {
                o.input();
            }
            if case == 3 {
                o.display_changed = true;
            }
            o.finish(12, 3);
            assert_eq!(o.result.candidate, case == 1 || case == 4);
            assert_eq!(o.result.writes, 12);
            assert_eq!(o.result.device_accesses, 3);
        }
    }
    #[test]
    fn bounded_table_does_not_invent_recurrence_on_overflow_or_segment_aliases() {
        let mut o = Observer::default();
        for base in 0..100 {
            o.record(Site {
                ip: 123,
                base,
                ..Default::default()
            });
        }
        o.queries = 10;
        o.finish(0, 0);
        assert_eq!(o.result.samples, 100);
        assert_eq!(o.result.repeated, 1);
        assert!(!o.result.candidate);
    }
    #[test]
    fn observation_preserves_execution_and_allocates_nothing() {
        let mut reference = Session::new(&[0x40, 0xeb, 0xfd]).unwrap();
        let mut observed = Session::new(&[0x40, 0xeb, 0xfd]).unwrap();
        observed.set_idle_observation(true);
        reference.start();
        observed.start();
        let allocations = allocation_counter::measure(|| {
            for _ in 0..1000 {
                assert_eq!(reference.advance(10000), observed.advance(10000));
            }
        });
        assert_eq!(allocations.count_total, 0);
        assert!(
            observed
                .machine
                .cpu
                .matches(&reference.machine.cpu.snapshot())
        );
        assert_eq!(observed.steps(), reference.steps());
        assert_eq!(
            observed.machine.ram_writes(),
            reference.machine.ram_writes()
        );
        assert_eq!(
            observed.machine.device_accesses(),
            reference.machine.device_accesses()
        );
        assert_eq!(observed.idle_loop_stats(), reference.idle_loop_stats());
        assert_eq!(observed.emulated_nanos(), reference.emulated_nanos());
        assert_eq!(
            observed.machine.bus.ram_view(0..0xa0000),
            reference.machine.bus.ram_view(0..0xa0000)
        );
        assert!(!observed.idle_observation().candidate);
        observed.set_idle_observation(false);
        assert!(!observed.idle_observation().enabled);
    }
}
