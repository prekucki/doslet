//! Presentation sampling only. Execution contributes counters, not UI work.
use crate::native_speed_tiers::Stats;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(crate) struct Shares {
    pub cpu: [f64; 4],
    pub guest: [f64; 4],
    pub seconds: f64,
}

#[derive(Default)]
pub(crate) struct Meter {
    baseline: Option<(Instant, Stats)>,
    pub shares: Option<Shares>,
}

fn percent(values: [u64; 4]) -> [f64; 4] {
    let total: f64 = values.iter().map(|v| *v as f64).sum();
    values.map(|value| {
        if total == 0.0 {
            0.0
        } else {
            100.0 * value as f64 / total
        }
    })
}

impl Meter {
    pub fn sample(&mut self, now: Instant, stats: Option<Stats>, running: bool) {
        let Some(stats) = stats.filter(|s| s.enabled && running) else {
            self.baseline = None;
            self.shares = None;
            return;
        };
        let Some((before, previous)) = self.baseline else {
            self.baseline = Some((now, stats));
            return;
        };
        // Reset/reload/toggle clears counters; do not display a stale interval.
        if (0..4).any(|i| stats.steps[i] < previous.steps[i] || stats.nanos[i] < previous.nanos[i])
        {
            self.baseline = Some((now, stats));
            self.shares = None;
            return;
        }
        if now.duration_since(before) >= Duration::from_secs(1) {
            self.shares = Some(Shares {
                cpu: percent(std::array::from_fn(|i| stats.steps[i] - previous.steps[i])),
                guest: percent(std::array::from_fn(|i| stats.nanos[i] - previous.nanos[i])),
                seconds: now.duration_since(before).as_secs_f64(),
            });
            self.baseline = Some((now, stats));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recent_execution_and_guest_shares_are_distinct_and_reset_on_pause() {
        let start = Instant::now();
        let mut meter = Meter::default();
        let mut stats = Stats {
            enabled: true,
            ..Stats::default()
        };
        meter.sample(start, Some(stats), true);
        stats.steps = [70, 15, 5, 10];
        stats.nanos = [10, 70, 10, 10];
        meter.sample(start + Duration::from_millis(500), Some(stats), true);
        assert!(meter.shares.is_none());
        meter.sample(start + Duration::from_secs(1), Some(stats), true);
        assert_eq!(meter.shares.unwrap().cpu, [70.0, 15.0, 5.0, 10.0]);
        assert_eq!(meter.shares.unwrap().guest, [10.0, 70.0, 10.0, 10.0]);
        stats.steps = [70, 115, 5, 10];
        stats.nanos = [10, 170, 10, 10];
        meter.sample(start + Duration::from_secs(2), Some(stats), true);
        assert_eq!(meter.shares.unwrap().cpu, [0.0, 100.0, 0.0, 0.0]);
        meter.sample(start + Duration::from_secs(3), Some(stats), false);
        assert!(meter.shares.is_none());
        meter.sample(start + Duration::from_secs(4), Some(stats), true);
        stats.steps = [0; 4];
        stats.nanos = [0; 4];
        meter.sample(start + Duration::from_secs(5), Some(stats), true);
        assert!(meter.shares.is_none());
        meter.sample(start + Duration::from_secs(6), Some(stats), true);
        assert_eq!(meter.shares.unwrap().cpu, [0.0; 4]);
    }
}
