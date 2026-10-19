//! Brief read pulses from monotonic guest I/O counters; no independent timer.
use web_time::{Duration, Instant};

#[derive(Default)]
pub(crate) struct Activity {
    baseline: Option<(u64, u64)>,
    last_read: Option<Instant>,
}

impl Activity {
    pub fn new(epoch: u64, bytes: u64) -> Self {
        Self {
            baseline: Some((epoch, bytes)),
            last_read: None,
        }
    }
    pub fn observe(&mut self, epoch: u64, bytes: u64, now: Instant) {
        if let Some((previous_epoch, previous_bytes)) = self.baseline {
            if epoch != previous_epoch || bytes < previous_bytes {
                self.last_read = None;
            } else if bytes > previous_bytes {
                self.last_read = Some(now);
            }
        }
        self.baseline = Some((epoch, bytes));
    }

    pub fn reading(&self, active: bool, now: Instant) -> bool {
        active
            && self
                .last_read
                .is_some_and(|at| now.saturating_duration_since(at) < Duration::from_millis(120))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_pulse_expires_and_pause_or_new_machine_clears_it() {
        let mut a = Activity::default();
        let now = Instant::now();
        a.observe(1, 4096, now);
        assert!(!a.reading(true, now));
        a.observe(1, 8192, now);
        assert!(a.reading(true, now));
        assert!(!a.reading(false, now));
        assert!(!a.reading(true, now + Duration::from_millis(120)));
        a.observe(2, 16384, now);
        assert!(!a.reading(true, now));
        a.observe(2, 20480, now);
        assert!(a.reading(true, now));
        a.observe(2, 0, now);
        assert!(!a.reading(true, now));
    }
}
