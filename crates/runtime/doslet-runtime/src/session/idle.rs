//! Proven idle loops: busy-wait passes accounted for as time.
//!
//! Games wait in loops that poll a counter an interrupt handler advances, or
//! ask the keyboard or mouse whether anything happened. One pass of such a
//! loop is proven idle when, single-stepped from a starting point, it returns
//! to an identical CPU state while every byte it wrote holds its previous
//! value, it touched no port or device memory, took no interrupt, saw no input
//! change and called only pure query services. Each further pass then repeats
//! exactly until a device event, so the machine accounts whole passes up to
//! the next event without executing them. Guest state, steps and time are the
//! same as executing those passes; only host work is saved.
use super::{RuntimeError, Session};
use doslet_pc::IdleProofMark;
use doslet_x86::CpuSnapshot;

/// Steps a proof may single-step before it gives up.
const PROOF_STEPS: u32 = 4096;
/// Steps between attempts at arbitrary points, doubling after each failure.
const BACKOFF_MIN: u64 = 20_000;
const BACKOFF_MAX: u64 = 2_000_000;
/// A skip at least this long shows that proofs pay for themselves here.
/// Device events such as digital-audio samples can bound every skip to a few
/// hundred steps; then a successful proof must not reset the backoff.
const USEFUL_SKIP: u32 = 4096;

/// Counters for diagnostics; they do not affect guest state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdleLoopStats {
    pub proofs: u64,
    pub failures: u64,
    /// Failures by what started the proof: a known loop head, an empty
    /// input poll, or the periodic backoff.
    pub head_failures: u64,
    pub poll_failures: u64,
    pub skips: u64,
    pub skipped_steps: u64,
    /// Guest time advanced by proven idle passes, independent of the analyzer.
    pub skipped_nanos: u64,
    /// The same idle time already included in execution-tier counters.
    pub skipped_tier_nanos: [u64; 4],
    pub last_failure: &'static str,
}

/// Input-driver state visible to services but kept outside guest RAM.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct InputState {
    mouse: [u16; 3],
    motion: (i16, i16),
    counts: [u16; 6],
    pending: u16,
    callback_active: bool,
    extended_key: Option<u8>,
}

struct Proof {
    /// Started at the last proven loop head rather than an arbitrary point.
    at_head: bool,
    /// Started right after an empty keyboard or mouse poll.
    from_poll: bool,
    /// Whether one initial pass already refreshed the baseline at `start`.
    warmed_up: bool,
    start: (u16, u32),
    cpu: CpuSnapshot,
    input: InputState,
    mark: IdleProofMark,
    device_accesses: u64,
    steps: u32,
    limit: u32,
    failure: Option<&'static str>,
    tier_counts: [u64; 4],
}

struct Proven {
    cpu: CpuSnapshot,
    input: InputState,
    pass: u32,
    tier_counts: [u64; 4],
    ram_writes: u64,
    device_accesses: u64,
}

pub(super) struct IdleLoops {
    pub(super) enabled: bool,
    proof: Option<Proof>,
    proven: Option<Proven>,
    /// CS selector and IP where the last proof succeeded: a pass is likely
    /// idle again once interrupts have changed the counters it waits for.
    pub(super) head: Option<(u16, u32)>,
    head_proven: bool,
    head_skipped: u32,
    next_attempt: u64,
    backoff: u64,
    /// Polls in loops too long or busy to prove must not retry each frame:
    /// poll-triggered attempts have their own backoff.
    poll_attempt: u64,
    poll_backoff: u64,
    poll_pending: bool,
    pub(super) stats: IdleLoopStats,
}

impl Default for IdleLoops {
    fn default() -> Self {
        Self {
            enabled: true,
            proof: None,
            proven: None,
            head: None,
            head_proven: false,
            head_skipped: 0,
            next_attempt: 0,
            backoff: BACKOFF_MIN,
            poll_attempt: 0,
            poll_backoff: BACKOFF_MIN,
            poll_pending: false,
            stats: IdleLoopStats::default(),
        }
    }
}

impl IdleLoops {
    /// A service that is not a pure query ends the running proof.
    pub(super) fn note_service(&mut self, vector: u8, ax: u16, dl: u8, motion: (i16, i16)) {
        // Reading zero relative motion and resetting it to zero changes nothing.
        let pure = repeatable_service(vector, ax, dl, motion);
        if let Some(proof) = &mut self.proof
            && !pure
        {
            proof.failure.get_or_insert("service");
        }
    }

    /// A keyboard or mouse query found nothing: the caller probably waits.
    pub(super) fn note_poll(&mut self, now: u64) {
        if self.proof.is_none() && now >= self.poll_attempt {
            self.poll_pending = true;
        }
    }
}

/// Services that only read guest RAM or input state and set registers or
/// RAM through the observed bus: their repeated results are proof-visible.
fn pure_service(vector: u8, ax: u16, dl: u8) -> bool {
    match (vector, (ax >> 8) as u8) {
        (0x16, 0x01 | 0x02 | 0x11 | 0x12) => true,
        (0x21, 0x0B) => true,
        (0x21, 0x06) => dl == 0xFF,
        (0x33, _) => ax == 3,
        (0x1A, 0x00) => true,
        (0x10, 0x03 | 0x0F) => true,
        _ => false,
    }
}

/// State changes of these services are covered by CPU, RAM and input snapshots.
pub(super) fn repeatable_service(vector: u8, ax: u16, dl: u8, motion: (i16, i16)) -> bool {
    pure_service(vector, ax, dl) || (vector, ax, motion) == (0x33, 0x0B, (0, 0))
}

impl Session {
    /// Bounded interpreter execution starting at a statically suggested wait.
    /// A suggestion only starts the ordinary CPU/RAM recurrence proof: every
    /// omitted pass must still be proven, and skips stop before hardware events.
    /// Other execution policies remain selected by the caller. When they are
    /// active, this operation simply takes the ordinary runtime path.
    pub(super) fn idle_candidate_advance(&mut self, budget: u32, proof_steps: u32) -> u32 {
        let budget = budget.min(65_536);
        if budget == 0 {
            return 0;
        }
        if self.idle.enabled
            || self.analyzer.enabled
            || self.observer.enabled
            || self.menu_idle_enabled()
            || self.adaptive_idle_enabled()
            || self.continuation.is_some()
            || self.storage_wait
        {
            return self.advance(budget);
        }
        // Keep the normal strategy's proof/backoff state intact. This temporary
        // interpreter slice owns its proof; none survives generated execution.
        let previous = core::mem::take(&mut self.idle);
        self.idle.next_attempt = u64::MAX;
        self.idle_start_bounded(false, false, proof_steps.clamp(1, PROOF_STEPS));
        let completed = self.advance(budget);
        self.idle_abandon();
        let stats = self.idle.stats;
        self.idle = previous;
        let total = &mut self.idle.stats;
        total.proofs = total.proofs.saturating_add(stats.proofs);
        total.failures = total.failures.saturating_add(stats.failures);
        total.head_failures = total.head_failures.saturating_add(stats.head_failures);
        total.poll_failures = total.poll_failures.saturating_add(stats.poll_failures);
        total.skips = total.skips.saturating_add(stats.skips);
        total.skipped_steps = total.skipped_steps.saturating_add(stats.skipped_steps);
        total.skipped_nanos = total.skipped_nanos.saturating_add(stats.skipped_nanos);
        for (total, extra) in total
            .skipped_tier_nanos
            .iter_mut()
            .zip(stats.skipped_tier_nanos)
        {
            *total = total.saturating_add(extra);
        }
        if stats.failures != 0 {
            total.last_failure = stats.last_failure;
        }
        completed
    }

    pub fn set_idle_loops(&mut self, enabled: bool) {
        let enabled = enabled && !self.v86_enabled();
        if !enabled {
            self.cancel_menu_idle();
            self.idle_abandon();
            self.idle.proven = None;
            self.idle.head = None;
            self.idle.head_proven = false;
            self.idle.head_skipped = 0;
        }
        self.idle.enabled = enabled;
    }

    pub fn idle_loops_enabled(&self) -> bool {
        self.idle.enabled
    }

    /// A foreign engine must leave this boundary to ordinary execution: a proof
    /// observes every step, and a known head or proven state is where the
    /// next proof starts or proven passes are skipped.
    pub(super) fn idle_needs_boundary(&self) -> bool {
        let r = &self.machine.cpu.registers;
        self.idle.enabled
            && (self.idle.proof.is_some()
                || self.idle.head == Some((r.cs, r.ip))
                || self
                    .idle
                    .proven
                    .as_ref()
                    .is_some_and(|proven| self.machine.cpu.matches(&proven.cpu)))
    }

    pub(super) fn idle_yield_ip(&self) -> Option<u32> {
        if self.idle.enabled
            && self.idle.proof.is_none()
            && self.idle.proven.is_none()
            && self.machine.batch_yield_ips().is_none()
            && let Some((head_cs, head_ip)) = self.idle.head
            && self.machine.cpu.registers.cs == head_cs
        {
            Some(head_ip)
        } else {
            None
        }
    }

    pub fn idle_loop_stats(&self) -> IdleLoopStats {
        self.idle.stats
    }

    pub(super) fn input_state(&self) -> InputState {
        let mouse = &self.mouse;
        InputState {
            mouse: [mouse.x, mouse.y, mouse.buttons],
            motion: mouse.motion,
            counts: core::array::from_fn(|index| {
                if index < 3 {
                    mouse.presses[index].count
                } else {
                    mouse.releases[index - 3].count
                }
            }),
            pending: mouse.pending,
            callback_active: mouse.callback_active,
            extended_key: self.extended_key,
        }
    }

    pub(super) fn idle_abandon(&mut self) {
        if let Some(proof) = self.idle.proof.take() {
            self.machine.end_idle_proof(proof.mark);
        }
    }

    fn idle_fail(&mut self, now: u64, reason: &'static str) {
        let (at_head, from_poll) = self
            .idle
            .proof
            .as_ref()
            .map_or((false, false), |proof| (proof.at_head, proof.from_poll));
        if at_head && self.idle.head_proven {
            // A proven wait loop eventually wakes up and leaves its loop head.
            // Retain the head for the next wait; if the next visit fails on
            // entry without proving a pass, that failure will clear `head`.
            self.idle.head_proven = false;
            self.idle.head_skipped = 0;
            self.idle_abandon();
            return;
        }
        // A loop head whose own proof fails no longer bypasses the backoff.
        if at_head {
            self.idle.head = None;
            self.idle.head_skipped = 0;
        }
        self.idle.stats.head_failures += u64::from(at_head);
        self.idle.stats.poll_failures += u64::from(from_poll);
        if from_poll {
            self.idle.poll_attempt = now + self.idle.poll_backoff;
            self.idle.poll_backoff = (self.idle.poll_backoff * 2).min(BACKOFF_MAX);
        } else {
            self.idle.next_attempt = now + self.idle.backoff;
            self.idle.backoff = (self.idle.backoff * 2).min(BACKOFF_MAX);
        }
        self.idle_abandon();
        self.idle.stats.failures += 1;
        self.idle.stats.last_failure = reason;
    }

    /// Skip proven passes, single-step a running proof or start one. Returns
    /// the steps consumed, or None when ordinary execution should continue.
    pub(super) fn idle_advance(
        &mut self,
        now: u64,
        budget: u32,
    ) -> Result<Option<u32>, RuntimeError> {
        if !self.idle.enabled
            || self.continuation.is_some()
            || self.storage_wait
            || self.analyzer_recurrence_recording()
        {
            self.idle_abandon();
            return Ok(None);
        }
        if self.idle.proof.as_ref().is_some_and(|proof| !proof.at_head) {
            let r = &self.machine.cpu.registers;
            if self.idle.head == Some((r.cs, r.ip)) {
                self.idle_abandon();
            }
        }
        if self.idle.proof.is_none() {
            if let Some(proven) = &self.idle.proven
                && self.machine.cpu.matches(&proven.cpu)
            {
                if self.machine.ram_writes() == proven.ram_writes
                    && self.machine.device_accesses() == proven.device_accesses
                    && self.input_state() == proven.input
                {
                    let before = self.machine.time().as_nanos();
                    let tiers_before = self.machine.execution_tier_stats().1;
                    let skipped = self.machine.skip_idle_passes_counted(
                        proven.pass,
                        budget,
                        proven.tier_counts,
                    )?;
                    if skipped != 0 {
                        let elapsed = self.machine.time().as_nanos() - before;
                        self.idle.stats.skipped_nanos += elapsed;
                        let tiers_after = self.machine.execution_tier_stats().1;
                        for i in 0..4 {
                            self.idle.stats.skipped_tier_nanos[i] +=
                                tiers_after[i].saturating_sub(tiers_before[i]);
                        }
                        self.analyzer_proven_idle(elapsed);
                        self.idle.head_skipped = self.idle.head_skipped.saturating_add(skipped);
                        if self.idle.head_skipped >= USEFUL_SKIP {
                            self.idle.backoff = BACKOFF_MIN;
                            self.idle.poll_backoff = BACKOFF_MIN;
                        }
                        self.idle.stats.skips += 1;
                        self.idle.stats.skipped_steps += u64::from(skipped);
                        return Ok(Some(skipped));
                    }
                }
                // The next pass crosses an event or follows a change: prove it
                // again as it runs instead of executing it unobserved.
                self.idle.proven = None;
                self.idle_start(true, false);
            } else {
                let r = &self.machine.cpu.registers;
                let at_head = self.idle.head == Some((r.cs, r.ip));
                let from_poll = core::mem::take(&mut self.idle.poll_pending);
                if at_head || from_poll || now >= self.idle.next_attempt {
                    self.idle_start(at_head, from_poll);
                }
            }
        }
        if self.idle.proof.is_none() {
            return Ok(None);
        }
        if self.machine.interrupt_pending() || self.machine.cpu.is_halted() {
            // Ordinary execution delivers the interrupt or idles in HLT.
            self.idle_abandon();
            return Ok(None);
        }
        let consumed = self.execute_step(1)?;
        let input = self.input_state();
        let device_accesses = self.machine.device_accesses();
        let proof = self.idle.proof.as_mut().expect("running proof");
        proof.steps += consumed;
        let r = &self.machine.cpu.registers;
        let at_start = (r.cs, r.ip) == proof.start;
        let failure = if let Some(reason) = proof.failure {
            Some(reason)
        } else if consumed != 1 || self.continuation.is_some() || self.storage_wait {
            Some("schedule")
        } else if self.machine.cpu.matches(&proof.cpu) {
            let mut proof = self.idle.proof.take().expect("running proof");
            let devices_unchanged = device_accesses == proof.device_accesses;
            let unchanged = self.machine.end_idle_proof(proof.mark);
            let input_unchanged = input == proof.input;
            if unchanged && input_unchanged {
                let r = &self.machine.cpu.registers;
                let head = (r.cs, r.ip);
                if self.idle.head != Some(head) {
                    self.idle.head = Some(head);
                    self.idle.head_skipped = 0;
                }
                self.idle.head_proven = true;
                self.idle.proven = Some(Proven {
                    cpu: proof.cpu,
                    input: proof.input,
                    pass: proof.steps,
                    tier_counts: core::array::from_fn(|i| {
                        self.machine.cpu.execution_counts()[i] - proof.tier_counts[i]
                    }),
                    ram_writes: self.machine.ram_writes(),
                    device_accesses,
                });
                self.idle.stats.proofs += 1;
                if proof.from_poll {
                    self.idle.poll_backoff = BACKOFF_MIN;
                }
                self.idle.next_attempt = now + self.idle.backoff;
                None
            } else if !proof.warmed_up
                && devices_unchanged
                && input_unchanged
                && proof.steps.saturating_mul(2) <= proof.limit
            {
                proof.warmed_up = true;
                proof.limit -= proof.steps;
                proof.steps = 0;
                proof.cpu = self.machine.cpu.snapshot();
                proof.input = input;
                proof.mark = self.machine.begin_idle_proof();
                proof.device_accesses = device_accesses;
                proof.tier_counts = self.machine.cpu.execution_counts();
                self.idle.proof = Some(proof);
                None
            } else {
                let reason = if !devices_unchanged {
                    "device access"
                } else if !unchanged {
                    "RAM changed or write-log capacity"
                } else {
                    "input state changed"
                };
                // Keep the proof so the failure applies to its trigger; the
                // write log has already ended and ending it again is a no-op.
                self.idle.proof = Some(proof);
                Some(reason)
            }
        } else if at_start
            && !proof.warmed_up
            && device_accesses == proof.device_accesses
            && input == proof.input
            && proof.steps.saturating_mul(2) <= proof.limit
        {
            let mut proof = self.idle.proof.take().expect("running proof");
            let _ = self.machine.end_idle_proof(proof.mark);
            proof.warmed_up = true;
            proof.limit -= proof.steps;
            proof.steps = 0;
            proof.cpu = self.machine.cpu.snapshot();
            proof.input = input;
            proof.mark = self.machine.begin_idle_proof();
            proof.device_accesses = device_accesses;
            proof.tier_counts = self.machine.cpu.execution_counts();
            self.idle.proof = Some(proof);
            None
        } else if proof.steps >= proof.limit {
            Some("length")
        } else {
            None
        };
        if let Some(reason) = failure {
            self.idle_fail(now, reason);
        }
        Ok(Some(consumed))
    }

    fn idle_start(&mut self, at_head: bool, from_poll: bool) {
        self.idle_start_bounded(at_head, from_poll, PROOF_STEPS);
    }

    fn idle_start_bounded(&mut self, at_head: bool, from_poll: bool, limit: u32) {
        if self.machine.interrupt_pending() || self.machine.cpu.is_halted() {
            return;
        }
        let r = &self.machine.cpu.registers;
        self.idle.proof = Some(Proof {
            at_head,
            from_poll,
            warmed_up: false,
            start: (r.cs, r.ip),
            cpu: self.machine.cpu.snapshot(),
            input: self.input_state(),
            mark: self.machine.begin_idle_proof(),
            device_accesses: self.machine.device_accesses(),
            steps: 0,
            limit,
            failure: None,
            tier_counts: self.machine.cpu.execution_counts(),
        });
    }

    /// Hooks for `service`: classify the call and note empty input polls.
    pub(super) fn idle_service(&mut self, vector: u8, ax: u16) {
        if (vector, ax, self.mouse.motion, self.mouse.buttons) == (0x33, 0x0B, (0, 0), 0) {
            self.note_empty_motion_poll();
        }
        let dl = self.machine.cpu.registers.get_byte(2);
        self.idle.note_service(vector, ax, dl, self.mouse.motion);
        let poll = matches!(
            (vector, (ax >> 8) as u8),
            (0x16, 0x01 | 0x11) | (0x21, 0x0B)
        ) || (vector, ax >> 8, dl) == (0x21, 6, 0xFF)
            || (vector, ax) == (0x33, 3)
            || (vector, ax, self.mouse.motion) == (0x33, 0x0B, (0, 0));
        if poll {
            self.observer.query();
            let now = self.steps;
            self.idle.note_poll(now);
        }
    }
}

#[cfg(test)]
mod candidate_tests;
