//! Budget arithmetic for an explicit cooperative waiting experiment.
//! A caller must independently qualify the complete path and its wake sources.
//! This module neither discovers waiting nor proves instruction equivalence.

/// Stateless admission limits, all in guest nanoseconds. `None` denies waiting.
#[derive(Clone, Copy)]
pub struct WaitLimits {
    pub service_horizon: Option<u64>,
    pub work_slack: Option<u64>,
    pub input_latency: u64,
    pub reserve: u64,
}

/// One-use credit from actually executed empty iterations, never inserted idle.
/// Gradual growth, immediate revocation, and bounded probation need no allocator.
pub struct WaitBudget {
    cap: u64,
    limit: u64,
    quiet: u8,
    blocked_until: u64,
}

impl WaitBudget {
    pub fn new(limit: u64) -> Self {
        Self {
            cap: 0,
            limit: limit.min(250_000),
            quiet: 0,
            blocked_until: 0,
        }
    }

    /// Revoke credit on work, an IRQ, input, or an unrecognized execution path.
    pub fn revoke(&mut self) {
        self.cap = 0;
        self.quiet = 0;
    }

    /// A deadline failure forces at least 250 ms of ordinary execution.
    pub fn miss(&mut self, now: u64) {
        self.revoke();
        self.blocked_until = now.saturating_add(250_000_000);
    }

    /// Call exactly once per admitted complete, naturally executed iteration.
    /// No unused credit survives this call. A third of natural waiting above
    /// the reserve is an experimental ceiling, not an inferred safe duration.
    pub fn completed_empty(&mut self, now: u64, natural: u64, limits: WaitLimits) -> u64 {
        if now < self.blocked_until || natural == 0 {
            return 0;
        }
        let Some(service) = limits.service_horizon else {
            self.revoke();
            return 0;
        };
        let Some(work) = limits.work_slack else {
            self.revoke();
            return 0;
        };
        self.quiet = self.quiet.saturating_add(1);
        if self.quiet < 8 {
            return 0;
        }
        self.cap = self.cap.saturating_add(1_000).min(self.limit);
        (natural.saturating_sub(limits.reserve) / 3)
            .min(self.cap)
            .min(service.saturating_sub(limits.reserve))
            .min(work.saturating_sub(limits.reserve))
            .min(limits.input_latency)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn limits() -> WaitLimits {
        WaitLimits {
            service_horizon: Some(1_000_000),
            work_slack: Some(2_000_000),
            input_latency: 100_000,
            reserve: 10_000,
        }
    }
    #[test]
    fn natural_credit_is_bounded_and_never_accumulated() {
        let mut b = WaitBudget::new(250_000);
        for i in 0..1000 {
            let amount = b.completed_empty(i, 100_000, limits());
            assert!(amount <= 30_000);
            if i < 7 {
                assert_eq!(amount, 0);
            }
        }
        assert_eq!(b.completed_empty(1001, 10_000, limits()), 0);
        assert_eq!(b.completed_empty(1002, 0, limits()), 0);
        assert_eq!(b.completed_empty(1003, 100_000, limits()), 30_000);
    }
    #[test]
    fn every_horizon_can_veto_waiting_and_unknown_is_not_slack() {
        for l in [
            WaitLimits {
                service_horizon: None,
                ..limits()
            },
            WaitLimits {
                work_slack: None,
                ..limits()
            },
            WaitLimits {
                service_horizon: Some(10_000),
                ..limits()
            },
            WaitLimits {
                work_slack: Some(1),
                ..limits()
            },
            WaitLimits {
                input_latency: 0,
                ..limits()
            },
        ] {
            let mut b = WaitBudget::new(250_000);
            for i in 0..100 {
                assert_eq!(b.completed_empty(i, 100_000, l), 0);
            }
        }
    }
    #[test]
    fn miss_and_invalidation_require_fresh_probation() {
        let mut b = WaitBudget::new(250_000);
        for i in 0..100 {
            b.completed_empty(i, 100_000, limits());
        }
        b.revoke();
        for i in 0..7 {
            assert_eq!(b.completed_empty(100 + i, 100_000, limits()), 0);
        }
        assert_eq!(b.completed_empty(108, 100_000, limits()), 1000);
        b.miss(200);
        assert_eq!(b.completed_empty(250_000_199, 100_000, limits()), 0);
        for i in 0..7 {
            assert_eq!(b.completed_empty(250_000_200 + i, 100_000, limits()), 0);
        }
        assert_eq!(b.completed_empty(250_000_208, 100_000, limits()), 1000);
    }
}
