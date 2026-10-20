//! Presentation-only sampling of adaptive idle time, outside CPU execution.
#[derive(Default)]
pub(crate) struct Meter {
    baseline: Option<(u64, u64)>,
    percent: u8,
}
impl Meter {
    pub(crate) fn sample(&mut self, running: bool, guest: u64, idle: u64) -> u8 {
        if !running {
            self.baseline = None;
            self.percent = 0;
            return 0;
        }
        let Some((before, saved)) = self.baseline else {
            self.baseline = Some((guest, idle));
            return 0;
        };
        if guest < before || idle < saved {
            self.baseline = Some((guest, idle));
            self.percent = 0;
        } else if guest - before >= 250_000_000 {
            self.percent =
                ((u128::from(idle - saved) * 100 / u128::from(guest - before)).min(100)) as u8;
            self.baseline = Some((guest, idle));
        }
        self.percent
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn samples_recent_activity_and_clears_on_pause_and_counter_reset() {
        let mut m = Meter::default();
        assert_eq!(m.sample(true, 0, 0), 0);
        assert_eq!(m.sample(true, 250_000_000, 200_000_000), 80);
        assert_eq!(m.sample(true, 300_000_000, 200_000_000), 80);
        assert_eq!(m.sample(true, 500_000_000, 200_000_000), 0);
        assert_eq!(m.sample(true, 750_000_000, 450_000_000), 100);
        assert_eq!(m.sample(false, 750_000_000, 450_000_000), 0);
        assert_eq!(m.sample(true, 750_000_000, 450_000_000), 0);
        assert_eq!(m.sample(true, 1_000_000_000, 700_000_000), 100);
        assert_eq!(m.sample(true, 1_000_000_000, 0), 0);
        assert_eq!(m.sample(true, 0, 0), 0);
    }
}
