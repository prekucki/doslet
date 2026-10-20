//! Integer time, independent of wall clocks and UI refresh rates.
#![no_std]

/// Nanoseconds in the emulated timeline, with no dependency on a host wall clock.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct EmuTime(u64);

impl EmuTime {
    pub const ZERO: Self = Self(0);
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }
    pub const fn as_nanos(self) -> u64 {
        self.0
    }
    pub fn checked_advance(self, nanos: u64) -> Option<Self> {
        self.0.checked_add(nanos).map(Self)
    }
}

/// Converts ticks to nanoseconds without discarding fractional time between batches.
#[derive(Debug)]
pub struct ClockRate {
    hz: core::num::NonZeroU32,
    remainder: u64,
}

impl ClockRate {
    pub const fn new(hz: core::num::NonZeroU32) -> Self {
        Self { hz, remainder: 0 }
    }
    pub fn advance(&mut self, ticks: u32) -> u64 {
        let numerator = u64::from(ticks) * 1_000_000_000 + self.remainder;
        let hz = u64::from(self.hz.get());
        self.remainder = numerator % hz;
        numerator / hz
    }
    /// Smallest tick count whose next advance reaches this nanosecond delta.
    /// Saturate only beyond u64 ticks; callers must apply their own finite budget.
    pub fn ticks_until_nanos(&self, nanos: u64) -> u64 {
        let needed = (u128::from(nanos) * u128::from(self.hz.get()))
            .saturating_sub(u128::from(self.remainder));
        needed.div_ceil(1_000_000_000).min(u128::from(u64::MAX)) as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadline_query_rounds_up_with_existing_fraction_without_advancing_time() {
        let hz = core::num::NonZeroU32::new(33_000_000).unwrap();
        for prefix in 0..33 {
            let mut clock = ClockRate::new(hz);
            clock.advance(prefix * 4);
            let original = clock.remainder;
            for nanos in [0, 1, 121, 122, 838, 1000000] {
                let ticks = clock.ticks_until_nanos(nanos);
                assert_eq!(clock.remainder, original);
                let mut target = ClockRate::new(hz);
                target.advance(prefix * 4);
                assert!(target.advance(ticks as u32) >= nanos);
                if ticks > 0 {
                    let mut before = ClockRate::new(hz);
                    before.advance(prefix * 4);
                    assert!(before.advance(ticks as u32 - 1) < nanos);
                }
            }
        }
    }
    #[test]
    fn fractional_time_survives_batch_boundaries() {
        let hz = core::num::NonZeroU32::new(1_193_182).unwrap();
        let mut batched = ClockRate::new(hz);
        let mut fine = ClockRate::new(hz);
        let expected = batched.advance(hz.get());
        let actual: u64 = (0..hz.get()).map(|_| fine.advance(1)).sum();
        assert_eq!(actual, expected);
        assert_eq!(actual, 1_000_000_000);
    }
}

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
