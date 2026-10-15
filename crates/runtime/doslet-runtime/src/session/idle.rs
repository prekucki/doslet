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

/// Counters for diagnostics; they do not affect guest state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdleLoopStats {
    pub proofs: u64,
    pub failures: u64,
    pub skips: u64,
    pub skipped_steps: u64,
    pub last_failure: &'static str,
}

/// Input-driver state visible to services but kept outside guest RAM.
#[derive(Clone, Copy, PartialEq)]
struct InputState {
    mouse: [u16; 3],
    motion: (i16, i16),
    counts: [u16; 6],
    pending: u16,
    callback_active: bool,
    extended_key: Option<u8>,
}

struct Proof {
    cpu: CpuSnapshot,
    input: InputState,
    mark: IdleProofMark,
    steps: u32,
    failure: Option<&'static str>,
}

struct Proven {
    cpu: CpuSnapshot,
    input: InputState,
    pass: u32,
    ram_writes: u64,
    device_accesses: u64,
}

pub(super) struct IdleLoops {
    pub(super) enabled: bool,
    proof: Option<Proof>,
    proven: Option<Proven>,
    /// CS selector and IP where the last proof succeeded: a pass is likely
    /// idle again once interrupts have changed the counters it waits for.
    head: Option<(u16, u32)>,
    next_attempt: u64,
    backoff: u64,
    pub(super) stats: IdleLoopStats,
}

impl Default for IdleLoops {
    fn default() -> Self {
        Self {
            enabled: true,
            proof: None,
            proven: None,
            head: None,
            next_attempt: 0,
            backoff: BACKOFF_MIN,
            stats: IdleLoopStats::default(),
        }
    }
}

impl IdleLoops {
    /// A service that is not a pure query ends the running proof.
    pub(super) fn note_service(&mut self, vector: u8, ax: u16, dl: u8) {
        if let Some(proof) = &mut self.proof
            && !pure_service(vector, ax, dl)
        {
            proof.failure.get_or_insert("service");
        }
    }

    /// A keyboard or mouse query found nothing: the caller probably waits.
    pub(super) fn note_poll(&mut self, now: u64) {
        if self.proof.is_none() && self.backoff < BACKOFF_MAX {
            self.next_attempt = self.next_attempt.min(now);
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

impl Session {
    pub fn set_idle_loops(&mut self, enabled: bool) {
        if !enabled {
            self.idle_abandon();
            self.idle.proven = None;
            self.idle.head = None;
        }
        self.idle.enabled = enabled;
    }

    pub fn idle_loop_stats(&self) -> IdleLoopStats {
        self.idle.stats
    }

    fn input_state(&self) -> InputState {
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

    fn idle_abandon(&mut self) {
        if let Some(proof) = self.idle.proof.take() {
            self.machine.end_idle_proof(proof.mark);
        }
    }

    fn idle_fail(&mut self, now: u64, reason: &'static str) {
        self.idle_abandon();
        self.idle.stats.failures += 1;
        self.idle.stats.last_failure = reason;
        self.idle.next_attempt = now + self.idle.backoff;
        self.idle.backoff = (self.idle.backoff * 2).min(BACKOFF_MAX);
    }

    /// Skip proven passes, single-step a running proof or start one. Returns
    /// the steps consumed, or None when ordinary execution should continue.
    pub(super) fn idle_advance(
        &mut self,
        now: u64,
        budget: u32,
    ) -> Result<Option<u32>, RuntimeError> {
        if !self.idle.enabled || self.continuation.is_some() || self.storage_wait {
            self.idle_abandon();
            return Ok(None);
        }
        if self.idle.proof.is_none() {
            if let Some(proven) = &self.idle.proven
                && self.machine.cpu.matches(&proven.cpu)
            {
                if self.machine.ram_writes() == proven.ram_writes
                    && self.machine.device_accesses() == proven.device_accesses
                    && self.input_state() == proven.input
                {
                    let skipped = self.machine.skip_idle_passes(proven.pass, budget)?;
                    if skipped != 0 {
                        self.idle.stats.skips += 1;
                        self.idle.stats.skipped_steps += u64::from(skipped);
                        return Ok(Some(skipped));
                    }
                }
                // The next pass crosses an event or follows a change: prove it
                // again as it runs instead of executing it unobserved.
                self.idle.proven = None;
                self.idle_start();
            } else {
                let r = &self.machine.cpu.registers;
                let at_head = self.idle.head == Some((r.cs, r.ip));
                if at_head || now >= self.idle.next_attempt {
                    self.idle_start();
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
        let proof = self.idle.proof.as_mut().expect("running proof");
        proof.steps += consumed;
        let failure = if let Some(reason) = proof.failure {
            Some(reason)
        } else if consumed != 1 || self.continuation.is_some() || self.storage_wait {
            Some("schedule")
        } else if self.machine.cpu.matches(&proof.cpu) {
            let proof = self.idle.proof.take().expect("running proof");
            let unchanged = self.machine.end_idle_proof(proof.mark);
            if unchanged && self.input_state() == proof.input {
                let r = &self.machine.cpu.registers;
                self.idle.head = Some((r.cs, r.ip));
                self.idle.proven = Some(Proven {
                    cpu: proof.cpu,
                    input: proof.input,
                    pass: proof.steps,
                    ram_writes: self.machine.ram_writes(),
                    device_accesses: self.machine.device_accesses(),
                });
                self.idle.stats.proofs += 1;
                self.idle.backoff = BACKOFF_MIN;
                self.idle.next_attempt = now + BACKOFF_MIN;
                None
            } else {
                Some("changed")
            }
        } else if proof.steps >= PROOF_STEPS {
            Some("length")
        } else {
            None
        };
        if let Some(reason) = failure {
            self.idle_fail(now, reason);
        }
        Ok(Some(consumed))
    }

    fn idle_start(&mut self) {
        if self.machine.interrupt_pending() || self.machine.cpu.is_halted() {
            return;
        }
        self.idle.proof = Some(Proof {
            cpu: self.machine.cpu.snapshot(),
            input: self.input_state(),
            mark: self.machine.begin_idle_proof(),
            steps: 0,
            failure: None,
        });
    }

    /// Hooks for `service`: classify the call and note empty input polls.
    pub(super) fn idle_service(&mut self, vector: u8, ax: u16) {
        let dl = self.machine.cpu.registers.get_byte(2);
        self.idle.note_service(vector, ax, dl);
        let poll = matches!(
            (vector, (ax >> 8) as u8),
            (0x16, 0x01 | 0x11) | (0x21, 0x0B)
        ) || (vector, ax >> 8, dl) == (0x21, 6, 0xFF)
            || (vector, ax) == (0x33, 3);
        if poll {
            let now = self.steps;
            self.idle.note_poll(now);
        }
    }
}
