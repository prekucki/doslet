//! Recent guest activity relative to Grey throughput, never host utilization.
#[derive(Clone, Copy)]
pub(crate) struct Counters {
    pub epoch: u64,
    pub guest: u64,
    pub idle: u64,
    /// Actually executed scheduling units, excluding accelerated idle passes.
    pub executed: [u64; 4],
    /// None selects the ordinary detected-idle meter.
    pub grey_mhz: Option<u16>,
}

#[derive(Clone, Copy)]
pub(crate) struct Activity {
    /// Hot, cold, Grey (including IRQ-labelled code), remaining Grey capacity.
    /// Values sum to 100; overload scales active colors proportionally.
    pub shares: [f32; 4],
}

impl Activity {
    pub fn busy(self) -> f32 {
        (100.0 - self.shares[3]).clamp(0.0, 100.0)
    }
}

#[derive(Default)]
pub(crate) struct Meter {
    baseline: Option<Counters>,
    activity: Option<Activity>,
}

impl Meter {
    pub fn sample(&mut self, running: bool, now: Counters) -> Option<Activity> {
        if !running {
            self.baseline = None;
            self.activity = None;
            return None;
        }
        let previous = self.baseline;
        let reset = previous.is_none_or(|p| {
            p.epoch != now.epoch
                || p.grey_mhz != now.grey_mhz
                || now.guest < p.guest
                || now.idle < p.idle
                || (0..4).any(|i| now.executed[i] < p.executed[i])
        });
        if reset {
            self.baseline = Some(now);
            self.activity = None;
            return None;
        }
        let before = previous.unwrap();
        let elapsed = now.guest - before.guest;
        if elapsed >= 250_000_000 {
            let shares = if let Some(mhz) = now.grey_mhz.filter(|m| *m > 0) {
                // The PC timing model charges four clocks per scheduling unit.
                // At Grey MHz the window can execute elapsed * MHz / 4000 units.
                // Actual counters also handle uncapped Hot without inventing MHz.
                let scale = 100.0 * 4000.0 / (elapsed as f64 * f64::from(mhz));
                let active: [f64; 4] =
                    std::array::from_fn(|i| (now.executed[i] - before.executed[i]) as f64 * scale);
                let total = active.iter().sum::<f64>();
                let busy = total.clamp(0.0, 100.0);
                let fit = if total > 100.0 { 100.0 / total } else { 1.0 };
                [
                    (active[0] * fit) as f32,
                    (active[1] * fit) as f32,
                    ((active[2] + active[3]) * fit) as f32,
                    (100.0 - busy) as f32,
                ]
            } else {
                let idle = (100.0 * (now.idle - before.idle) as f64 / elapsed as f64)
                    .clamp(0.0, 100.0) as f32;
                [0.0, 0.0, 100.0 - idle, idle]
            };
            self.activity = Some(Activity { shares });
            self.baseline = Some(now);
        }
        self.activity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counters(guest: u64) -> Counters {
        Counters {
            epoch: 1,
            guest,
            idle: 0,
            executed: [0; 4],
            grey_mhz: Some(100),
        }
    }

    fn activity(executed: [u64; 4]) -> Activity {
        let mut meter = Meter::default();
        assert!(meter.sample(true, counters(0)).is_none());
        let mut now = counters(1_000_000_000);
        now.executed = executed;
        meter.sample(true, now).unwrap()
    }

    fn close(a: f32, b: f32) {
        assert!((a - b).abs() < 0.001, "{a} != {b}");
    }

    #[test]
    fn all_cold_at_8_mhz_uses_eight_percent_of_100_mhz_grey() {
        let a = activity([0, 2_000_000, 0, 0]);
        close(a.busy(), 8.0);
        close(a.shares[1], 8.0);
        close(a.shares[3], 92.0);
    }

    #[test]
    fn five_percent_hot_at_500_and_the_rest_cold_at_8() {
        let a = activity([6_250_000, 1_900_000, 0, 0]);
        close(a.shares[0], 25.0);
        close(a.shares[1], 7.6);
        close(a.busy(), 32.6);
        close(a.shares[3], 67.4);
    }

    #[test]
    fn hot_overload_clamps_and_preserves_relative_colors() {
        let a = activity([125_000_000, 0, 0, 0]);
        close(a.busy(), 100.0);
        close(a.shares[0], 100.0);
        close(a.shares[3], 0.0);
        let mixed = activity([25_000_000, 25_000_000, 0, 0]);
        assert_eq!(mixed.shares, [50.0, 50.0, 0.0, 0.0]);
    }

    #[test]
    fn grey_irq_and_zero_work_use_the_same_reference_budget() {
        assert_eq!(
            activity([0, 0, 10_000_000, 15_000_000]).shares,
            [0.0, 0.0, 100.0, 0.0]
        );
        assert_eq!(activity([0; 4]).shares, [0.0, 0.0, 0.0, 100.0]);
    }

    #[test]
    fn normal_mode_keeps_detected_idle_and_reset_clears_samples() {
        let mut meter = Meter::default();
        let mut now = counters(0);
        now.grey_mhz = None;
        assert!(meter.sample(true, now).is_none());
        now.guest = 250_000_000;
        now.idle = 200_000_000;
        close(meter.sample(true, now).unwrap().busy(), 20.0);
        assert!(meter.sample(false, now).is_none());
        assert!(meter.sample(true, now).is_none());
        now.guest += 250_000_000;
        assert!(meter.sample(true, now).is_some());
        now.epoch += 1;
        assert!(meter.sample(true, now).is_none());
        now.guest += 250_000_000;
        assert!(meter.sample(true, now).is_some());
        now.grey_mhz = Some(100);
        assert!(meter.sample(true, now).is_none());
        now.grey_mhz = Some(133);
        assert!(meter.sample(true, now).is_none());
        now.guest = 0;
        assert!(meter.sample(true, now).is_none());
    }
}
