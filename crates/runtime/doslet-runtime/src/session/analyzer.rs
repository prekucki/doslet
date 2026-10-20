//! Bounded synchronization evidence. Missing observations never authorize a budget.
mod caller;
mod guarded;
mod recurrence;
use super::{RuntimeError, Session};
pub use caller::{CallerEffect, CallerStats};
use doslet_bus::{CodeCheck, MemoryBus, observation::Kind};
use doslet_x86::{
    Gpr32,
    analysis::{Context, Coverage, Effect, Flow, Graph, Operand},
};
pub use guarded::GuardStats;
pub use recurrence::RecurrenceStats;

const CANDIDATES: usize = 4;
const SCOUT: u64 = 4096;
const WINDOW: u64 = 2_000_000;
const PERIOD: u64 = 8_000_000;
const FRESH: u64 = 250_000_000;

#[derive(Clone, Copy, Debug, Default)]
pub struct CandidateStats {
    /// Time outside confirmed VGA waits is conservatively charged as work.
    pub boundary_accounting: bool,
    pub head: u32,
    pub branch: u32,
    pub operand: u32,
    pub width: u8,
    pub port: bool,
    pub state: &'static str,
    pub reason: &'static str,
    pub writer_pc: Option<u32>,
    pub writer_kind: &'static str,
    pub writer_irq: Option<u8>,
    pub writer_address: Option<u32>,
    pub writer_width: u8,
    pub writer_value: Option<u32>,
    pub writer_nanos: Option<u64>,
    pub writes: u64,
    pub partial_writes: u64,
    pub source_transitions: u64,
    pub source_period_nanos: Option<u64>,
    pub episodes: u64,
    pub work_nanos: u64,
    pub wait_nanos: u64,
    pub work_max_nanos: u64,
    pub period_nanos: u64,
    /// Cadence mean excluding one longest period, measured at the ceiling.
    pub reference_period_nanos: u64,
    pub reference_period_max_nanos: u64,
    /// Current cadence mean excluding one longest period, if eight are known.
    pub cadence_mean_nanos: Option<u64>,
    /// Ceiling-speed periods varied too much to establish a cadence deadline.
    pub variable_cadence: bool,
    /// Conservative floor after a lower throughput measurably slowed cadence.
    pub cadence_floor_mhz: u16,
    pub missed_deadlines: u64,
    pub work_histogram: [u64; 8],
    pub last_episode_nanos: u64,
    pub measured_mhz: u16,
    pub predicted_mhz: Option<u16>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct AnalyzerStats {
    pub enabled: bool,
    pub controller_enabled: bool,
    pub configured_mhz: u16,
    /// Frontend host capacity cap, or zero.
    pub host_limit_mhz: u16,
    pub effective_mhz: u16,
    pub reductions: u64,
    pub recoveries: u64,
    pub last_recovery_reason: &'static str,
    pub audio_irq_deadlines: u64,
    pub audio_irq_deadline_misses: u64,
    pub reason: &'static str,
    pub graph_reason: &'static str,
    pub graph_nodes: u32,
    pub backedges: u32,
    pub unknown_effects: u32,
    pub observed_steps: u64,
    /// Guest time skipped by the independent, unchanged-state idle proof.
    pub proven_idle_nanos: u64,
    pub guard_irq_steps: u64,
    pub guard_interrupted_traces: u64,
    pub guards: [GuardStats; 2],
    pub caller: CallerStats,
    pub recurrence: RecurrenceStats,
    pub gaps: u64,
    pub last_gap_reason: &'static str,
    pub discovery_windows: u64,
    pub extended_windows: u64,
    pub invalidations: u64,
    pub dropped_candidates: u64,
    pub unobserved_service_writes: u64,
    pub unobserved_service_device_accesses: u64,
    pub unobserved_device_writes: u64,
    pub candidates: [CandidateStats; CANDIDATES],
}
#[derive(Clone, Copy)]
struct Predicate {
    head: u32,
    branch: u32,
    end: u32,
    operand: u32,
    width: u8,
    port: bool,
    version: CodeCheck,
}
struct SuspendedVga {
    predicate: Predicate,
    began: u64,
    source_phase_end: Option<u64>,
    credit_wait: bool,
    irq_began: u64,
    return_to: (u16, u32, u32),
    saw_iret: bool,
}
struct VgaStep {
    predicate: Predicate,
    began: u64,
    source_phase_end: Option<u64>,
    credit_wait: bool,
    before: u64,
    pc: u32,
    cs: u16,
    sp: u32,
}
#[derive(Clone, Copy, Default)]
struct Candidate {
    predicate: Option<Predicate>,
    stats: CandidateStats,
    work_start: Option<u64>,
    wait_start: Option<u64>,
    last_write: Option<(u64, u32, u32, u8)>,
    qualified_episodes: u32,
    recent_max: u64,
    recent_work: [u64; 16],
    recent_periods: [u64; 16],
    period_count: usize,
    period_next: usize,
    recent_next: usize,
    /// Measured slowdown that justified the current floor, not a new deadline.
    floor_failure_mean: u64,
    overdue: bool,
    wait_counter_start: Option<u64>,
    port_exit_phase: Option<u64>,
}
impl Candidate {
    fn gap(&mut self, reason: &'static str) {
        self.work_start = None;
        self.wait_start = None;
        self.wait_counter_start = None;
        self.port_exit_phase = None;
        self.stats.predicted_mhz = None;
        self.stats.reason = reason;
        self.qualified_episodes = 0;
        self.recent_max = 0;
        self.recent_work.fill(0);
        self.recent_periods.fill(0);
        self.period_count = 0;
        self.period_next = 0;
        self.recent_next = 0;
        self.last_write = None;
        self.stats.source_transitions = 0;
        self.stats.reference_period_nanos = 0;
        self.stats.reference_period_max_nanos = 0;
        self.stats.variable_cadence = false;
        self.stats.cadence_floor_mhz = 0;
        self.floor_failure_mean = 0;
    }
    fn relearn(&mut self, reason: &'static str) {
        let reference = self.stats.reference_period_nanos;
        let reference_max = self.stats.reference_period_max_nanos;
        let variable_cadence = self.stats.variable_cadence;
        let floor = self.stats.cadence_floor_mhz;
        let floor_failure_mean = self.floor_failure_mean;
        let phase = self.port_exit_phase;
        let cadence = (self.recent_periods, self.period_count, self.period_next);
        self.gap(reason);
        self.stats.reference_period_nanos = reference;
        self.stats.reference_period_max_nanos = reference_max;
        self.stats.variable_cadence = variable_cadence;
        self.stats.cadence_floor_mhz = floor;
        self.floor_failure_mean = floor_failure_mean;
        self.port_exit_phase = phase;
        (self.recent_periods, self.period_count, self.period_next) = cadence;
    }
    fn cadence_mean(&self) -> Option<u64> {
        // A single delayed presentation occurs even at the configured ceiling.
        // Preserve this hardware-time window across gradual speed changes;
        // work-cost qualification is separate and still resets on retuning.
        (self.period_count >= 8).then(|| {
            let sum = self.recent_periods.iter().sum::<u64>();
            let longest = self.recent_periods.iter().copied().max().unwrap_or(0);
            (sum - longest) / (self.period_count as u64 - 1)
        })
    }
    fn cadence_reference(&self) -> Option<u64> {
        let mean = self.cadence_mean()?;
        // A VGA read is a device boundary, not evidence of a fixed frame rate.
        // Admit one delayed presentation, as for the cadence mean, but require
        // the remaining ceiling-speed periods to fit a 12.5% spread. The
        // current mean uses a tighter slowdown tolerance than individual
        // frames. Otherwise no cadence deadline is established.
        let mut periods = self.recent_periods;
        let periods = &mut periods[..self.period_count];
        periods.sort_unstable();
        let shortest = periods[0];
        let longest = periods[periods.len() - 2];
        (longest <= shortest.saturating_mul(9) / 8 + 100_000).then_some(mean)
    }
    #[cfg(test)]
    fn branch(&mut self, waiting: bool, now: u64, mhz: u16) {
        self.branch_measured(waiting, now, mhz, None, None);
    }
    fn branch_measured(
        &mut self,
        waiting: bool,
        now: u64,
        mhz: u16,
        total_wait: Option<u64>,
        source_phase_end: Option<u64>,
    ) {
        if self.stats.measured_mhz != mhz {
            if mhz > self.stats.measured_mhz {
                // Recovery must not blame the new, faster speed for delayed
                // periods produced before it. The ceiling reference remains.
                self.recent_periods.fill(0);
                self.period_count = 0;
                self.period_next = 0;
            }
            self.stats.work_max_nanos = 0;
            self.stats.work_histogram = [0; 8];
            self.relearn("relearning at changed MHz");
            self.stats.measured_mhz = mhz;
        }
        if waiting {
            // The initial observed wait establishes a synchronization phase,
            // even though it cannot finish an episode before a work start.
            // Losing it would miss all later immediate VGA exits.
            if self.wait_start.is_none() {
                self.wait_start = Some(now);
            }
        } else {
            // A VGA poll can find retrace active on its first read. That is
            // still a presentation boundary, even without a taken iteration.
            // Joining it to the next wait would invent a doubled frame period.
            // Repeated exits within one hardware phase are catch-up work, not
            // separate frames. Require a previously observed waiting episode;
            // an always-false predicate has no established synchronization.
            let port_boundary = self.stats.boundary_accounting
                && self.stats.port
                && self
                    .port_exit_phase
                    .zip(source_phase_end)
                    .is_some_and(|(old, new)| old != new);
            if let Some(start) = self.work_start
                && (self.wait_start.is_some() || port_boundary)
            {
                let period = now.saturating_sub(start);
                let waiting = match (total_wait, self.wait_counter_start) {
                    (Some(total), Some(previous)) => total.saturating_sub(previous).min(period),
                    _ => self.wait_start.map_or(0, |wait| now.saturating_sub(wait)),
                };
                let work = period.saturating_sub(waiting);
                self.stats.episodes += 1;
                self.qualified_episodes += 1;
                self.recent_work[self.recent_next] = work;
                self.recent_periods[self.period_next] = period;
                self.period_next = (self.period_next + 1) % self.recent_periods.len();
                self.period_count = (self.period_count + 1).min(self.recent_periods.len());
                self.recent_next = (self.recent_next + 1) % self.recent_work.len();
                self.recent_max = self.recent_work.iter().copied().max().unwrap_or(0);
                self.stats.work_nanos = work;
                self.stats.wait_nanos = waiting;
                self.stats.period_nanos = now.saturating_sub(start);
                self.stats.work_max_nanos = self.stats.work_max_nanos.max(work);
                self.stats.work_histogram[((work.max(1).ilog2() / 4) as usize).min(7)] += 1;
                self.stats.last_episode_nanos = now;
                self.stats.state = "complete episodes";
            }
            if self.wait_start.is_some() || self.work_start.is_none() || port_boundary {
                self.work_start = Some(now);
                self.wait_counter_start = total_wait;
            }
            if self.wait_start.is_some() || self.port_exit_phase.is_some() {
                self.port_exit_phase = source_phase_end;
            }
            self.wait_start = None;
            self.overdue = false;
        }
    }
    fn budget(&mut self, now: u64, ceiling: u16) -> Option<u16> {
        self.stats.predicted_mhz = None;
        let reason = if self.qualified_episodes < 8 {
            "fewer than eight complete episodes"
        } else if now.saturating_sub(self.stats.last_episode_nanos) > FRESH {
            "stale episodes"
        } else if self.stats.port && self.stats.variable_cadence {
            "variable VGA cadence; no presentation deadline"
        } else if !self.stats.port && self.stats.source_transitions < 4 {
            "clock writer not established"
        } else if self.stats.period_nanos == 0 || self.stats.wait_nanos == 0 {
            "no measured waiting"
        } else {
            "prediction available; throughput scaling remains heuristic"
        };
        self.stats.reason = reason;
        if reason != "prediction available; throughput scaling remains heuristic" {
            return None;
        }
        // Reserve 25% of the period plus a 100-us IRQ-service margin.
        let period = if self.stats.reference_period_nanos != 0 {
            self.stats
                .period_nanos
                .min(self.stats.reference_period_nanos)
        } else {
            self.stats.period_nanos
        };
        let available = period.saturating_mul(3) / 4;
        let available = available.saturating_sub(100_000);
        if available == 0 {
            return None;
        }
        let mhz = (u128::from(self.stats.measured_mhz) * u128::from(self.recent_max))
            .div_ceil(u128::from(available))
            .min(u128::from(ceiling)) as u16;
        self.stats.predicted_mhz = Some(mhz.max(1).max(self.stats.cadence_floor_mhz).min(ceiling));
        self.stats.predicted_mhz
    }
}
pub(super) struct Analyzer {
    guards: guarded::GuardProbe,
    caller: caller::CallerProbe,
    recurrence: recurrence::Recurrence,
    pub enabled: bool,
    pub controller: bool,
    pub ceiling: u16,
    /// Host capacity cap below the user ceiling; zero means none.
    host_limit: u16,
    next: u64,
    remaining: u64,
    extended: bool,
    boundary_pending: Option<(Predicate, u64, Option<u64>, bool)>,
    boundary_suspended: Option<SuspendedVga>,
    confirmed_wait_nanos: u64,
    graph: Graph,
    candidates: [Candidate; CANDIDATES],
    pending: Option<(Predicate, u32)>,
    irq_stack: [Option<(u8, u16, u32, u32)>; 8],
    irq_depth: usize,
    stats: AnalyzerStats,
    adjustment_at: u64,
    relearn_until: u64,
    audio_deadline_misses: u64,
}
impl Default for Analyzer {
    fn default() -> Self {
        Self {
            guards: guarded::GuardProbe::default(),
            caller: caller::CallerProbe::default(),
            recurrence: recurrence::Recurrence::default(),
            enabled: false,
            controller: false,
            ceiling: 0,
            host_limit: 0,
            next: 0,
            remaining: 0,
            extended: false,
            boundary_pending: None,
            boundary_suspended: None,
            confirmed_wait_nanos: 0,
            graph: Graph::default(),
            candidates: [Candidate::default(); CANDIDATES],
            pending: None,
            irq_stack: [None; 8],
            irq_depth: 0,
            stats: AnalyzerStats::default(),
            adjustment_at: 0,
            relearn_until: 0,
            audio_deadline_misses: 0,
        }
    }
}
impl Analyzer {
    /// Highest rate the controller may select: the user ceiling, or a lower
    /// host capacity cap while the frontend cannot keep pace.
    fn limit(&self) -> u16 {
        match self.host_limit {
            0 => self.ceiling,
            limit => self.ceiling.min(limit),
        }
    }
    fn gap(&mut self, reason: &'static str) {
        self.stats.gaps += 1;
        self.stats.last_gap_reason = reason;
        // Long traces are worthwhile only while they can complete an episode.
        // Do not spend millions of interpreter steps on unsupported game paths.
        if self.extended {
            self.remaining = self.remaining.min(SCOUT);
        }
        if !self.candidates.iter().any(|c| c.stats.boundary_accounting) {
            self.relearn_until = 0;
        }
        self.pending = None;
        self.irq_depth = 0;
        self.irq_stack.fill(None);
        for c in &mut self.candidates {
            if !c.stats.boundary_accounting {
                c.gap(reason);
            }
        }
    }
    fn invalidate(&mut self) {
        self.gap("invalidated code or context");
        for c in &mut self.candidates {
            if !c.stats.boundary_accounting {
                *c = Candidate::default();
            }
        }
        self.graph.coverage = Coverage::Invalidated;
        self.stats.invalidations += 1;
    }
    fn record_branch(
        &mut self,
        p: Predicate,
        waiting: bool,
        now: u64,
        mhz: u16,
        total_wait: Option<u64>,
        source_phase_end: Option<u64>,
    ) {
        let index = self
            .candidates
            .iter()
            .position(|c| {
                c.predicate
                    .is_some_and(|old| old.head == p.head && old.version == p.version)
            })
            .or_else(|| self.candidates.iter().position(|c| c.predicate.is_none()));
        let Some(index) = index else {
            self.stats.dropped_candidates += 1;
            return;
        };
        let limit = self.limit();
        let c = &mut self.candidates[index];
        if c.predicate.is_none() {
            c.predicate = Some(p);
            c.stats = CandidateStats {
                head: p.head,
                branch: p.branch,
                operand: p.operand,
                width: p.width,
                port: p.port,
                state: "structurally understood",
                reason: "source and complete episodes required",
                ..Default::default()
            };
        }
        c.stats.boundary_accounting = total_wait.is_some();
        let previous_episodes = c.stats.episodes;
        c.branch_measured(waiting, now, mhz, total_wait, source_phase_end);
        if mhz == limit
            && c.qualified_episodes >= c.recent_periods.len() as u32
            && c.stats.episodes != previous_episodes
        {
            // Use a full ceiling-speed window even for the first reference.
            // An early, unusually fast partial window could otherwise freeze
            // the reference as soon as the controller reduces throughput.
            let reference = if c.stats.port {
                c.cadence_reference()
            } else {
                c.cadence_mean()
            };
            c.stats.variable_cadence = c.stats.port && reference.is_none();
            c.stats.reference_period_nanos = reference.unwrap_or(0);
            if c.stats.variable_cadence
                || (c.floor_failure_mean != 0
                    && c.floor_failure_mean
                        <= c.stats.reference_period_nanos.saturating_mul(33) / 32 + 100_000)
            {
                // Only a full window measured at the user ceiling can show
                // that an earlier floor was based on an unusually fast
                // reference. Never relax it using reduced-speed periods.
                c.stats.cadence_floor_mhz = 0;
                c.floor_failure_mean = 0;
            }
            c.stats.reference_period_max_nanos =
                c.recent_periods.iter().copied().max().unwrap_or(0);
        }
    }
}
impl Session {
    /// Only the strict idle engine may supply this evidence. Empty input polls
    /// and cooperative waits do not establish an unchanged-state interval.
    pub(super) fn analyzer_proven_idle(&mut self, elapsed: u64) {
        if self.analyzer.enabled {
            self.analyzer.confirmed_wait_nanos =
                self.analyzer.confirmed_wait_nanos.saturating_add(elapsed);
            self.analyzer.stats.proven_idle_nanos = self
                .analyzer
                .stats
                .proven_idle_nanos
                .saturating_add(elapsed);
        }
    }

    /// Diagnostic mode remains independent of the opt-in throughput controller.
    pub fn set_synchronization_analysis(&mut self, enabled: bool) {
        self.analyzer_recurrence_forget();
        let ceiling = if self.analyzer.ceiling == 0 {
            self.hardware_config().cpu_mhz
        } else {
            self.analyzer.ceiling
        };
        if self.analyzer.controller {
            let _ = self.machine.set_cpu_mhz(ceiling);
        }
        if enabled {
            self.set_menu_idle_site(None);
            self.set_idle_observation(false);
            self.machine.set_memory_wait_sites(None);
        }
        self.analyzer = Analyzer {
            enabled,
            ceiling,
            audio_deadline_misses: self.machine.bus.dsp.diagnostics().block_irq_deadline_misses,
            ..Default::default()
        };
    }
    /// No discovery window, probe, recording or VGA boundary is active, so the
    /// analyzer only counts steps until its next window.
    pub(super) fn analyzer_quiescent(&self) -> bool {
        let a = &self.analyzer;
        !a.enabled
            || (a.remaining == 0
                && a.pending.is_none()
                && a.boundary_pending.is_none()
                && a.boundary_suspended.is_none()
                && a.irq_depth == 0
                && a.guards.idle()
                && a.caller.idle()
                && a.recurrence.idle())
    }
    /// Cap adaptive throughput for host capacity without replacing the user
    /// ceiling. The controller restores at most this rate; zero or a value at
    /// the ceiling removes the cap. Ignored without the adaptive controller.
    pub fn set_host_cpu_limit(&mut self, mhz: u16) -> Result<(), RuntimeError> {
        if !self.analyzer.controller {
            return Ok(());
        }
        self.analyzer.host_limit = if mhz >= self.analyzer.ceiling { 0 } else { mhz };
        let limit = self.analyzer.limit();
        if self.hardware_config().cpu_mhz > limit {
            self.machine.set_cpu_mhz(limit)?;
            self.analyzer.adjustment_at = self.emulated_nanos();
        }
        Ok(())
    }
    pub fn host_cpu_limit(&self) -> u16 {
        self.analyzer.host_limit
    }
    /// Configured MHz is the ceiling. This controller does not authorize skipping.
    pub fn set_adaptive_cpu(&mut self, enabled: bool) {
        if enabled {
            self.set_synchronization_analysis(true);
            self.set_adaptive_idle(false);
            self.set_idle_observation(false);
            self.machine.set_memory_wait_sites(None);
            self.set_menu_idle_site(None);
            self.set_event_idle_site(None);
        } else if self.analyzer.ceiling != 0 {
            let _ = self.machine.set_cpu_mhz(self.analyzer.ceiling);
        }
        self.analyzer.controller = enabled;
    }
    pub fn synchronization_analysis(&self) -> AnalyzerStats {
        let mut s = self.analyzer.stats;
        let audio = self.machine.bus.dsp.diagnostics();
        s.audio_irq_deadlines = audio.block_irq_deadlines;
        s.audio_irq_deadline_misses = audio.block_irq_deadline_misses;
        s.enabled = self.analyzer.enabled;
        s.controller_enabled = self.analyzer.controller;
        s.host_limit_mhz = self.analyzer.host_limit;
        s.configured_mhz = if self.analyzer.ceiling == 0 {
            self.hardware_config().cpu_mhz
        } else {
            self.analyzer.ceiling
        };
        s.effective_mhz = self.hardware_config().cpu_mhz;
        s.graph_nodes = self.analyzer.graph.len as u32;
        s.backedges = self.analyzer.graph.backedges;
        s.unknown_effects = self.analyzer.graph.unknown_effects;
        s.graph_reason = self.analyzer.graph.coverage.reason();
        s.candidates = self.analyzer.candidates.map(|c| {
            let mut stats = c.stats;
            stats.cadence_mean_nanos = c.cadence_mean();
            stats
        });
        s.guards = self.analyzer.guards.stats();
        s.caller = self.analyzer.caller.stats;
        s.recurrence = self.analyzer.recurrence.stats;
        s.guard_irq_steps = self.analyzer.guards.irq_steps;
        s.guard_interrupted_traces = self.analyzer.guards.interrupted_traces;
        s
    }
    fn analyzer_predicate(&self) -> Option<Predicate> {
        let r = self.registers();
        let ip = r.ip;
        let bytes = self.machine.bus.code_bytes(ip, 24)?;
        let d = doslet_x86::analysis::decode(bytes, ip)?;
        let (operand, width, port, offset) = match d.effect {
            Effect::Compare {
                left: Operand::Absolute { segment, address },
                right: Operand::Immediate(_),
                ..
            } => {
                if r.segment_cache(segment).base != 0 {
                    return None;
                }
                if !self.machine.bus.is_plain_ram(address, d.width) {
                    return None;
                }
                (address, d.width, false, u32::from(d.length))
            }
            Effect::Move {
                target: Operand::Register(reg),
                source: Operand::Absolute { segment, address },
            } => {
                if self.registers().segment_cache(segment).base != 0
                    || !self.machine.bus.is_plain_ram(address, d.width)
                {
                    return None;
                }
                let cmp = doslet_x86::analysis::decode(
                    bytes.get(usize::from(d.length)..)?,
                    ip + u32::from(d.length),
                )?;
                if !matches!(cmp.effect,Effect::Compare {left:Operand::Register(r),right:Operand::Immediate(_),..} if r==reg)
                    || cmp.width != d.width
                {
                    return None;
                }
                (
                    address,
                    d.width,
                    false,
                    u32::from(d.length) + u32::from(cmp.length),
                )
            }
            Effect::Input => {
                // The execution decoder supplies the following TEST/AND length;
                // only the documented VGA retrace mask has a modeled transition.
                let test = doslet_x86::analysis::decode(bytes.get(1..)?, ip + 1)?;
                let immediate_mask = matches!(
                    test.effect,
                    Effect::Compare {
                        left: Operand::Register(0),
                        right: Operand::Immediate(8),
                        test: true
                    } | Effect::Mask {
                        target: Operand::Register(0),
                        mask: 8
                    }
                );
                // IN AL preserves AH. Some guests keep the retrace mask in AH
                // rather than encoding it as an immediate TEST operand.
                let register_mask = test.width == 1
                    && matches!(
                        test.effect,
                        Effect::Compare {
                            left: Operand::Register(0),
                            right: Operand::Register(4),
                            test: true,
                        } | Effect::Compare {
                            left: Operand::Register(4),
                            right: Operand::Register(0),
                            test: true,
                        }
                    )
                    && r.get_byte(4) == 8;
                if d.length != 1 || d.width != 1 || !(immediate_mask || register_mask) {
                    return None;
                }
                let port = r.get(super::Gpr16::Dx);
                self.machine.bus.vga.retrace_deadline(port)?;
                (u32::from(port), 1, true, 1 + u32::from(test.length))
            }
            _ => return None,
        };
        let branch = ip.checked_add(offset)?;
        let d = doslet_x86::analysis::decode(bytes.get(offset as usize..)?, branch)?;
        let Flow::Branch {
            condition: Some(4 | 5),
            target,
        } = d.flow
        else {
            return None;
        };
        // An immediately returning guard cannot stand in for the caller's frame.
        if target != ip {
            return None;
        }
        let end = branch.checked_add(u32::from(d.length))?;
        Some(Predicate {
            head: ip,
            branch,
            end,
            operand,
            width,
            port,
            version: self.machine.bus.code_check(ip, (end - ip) as u8)?,
        })
    }
    fn analyzer_vga_gap(&mut self, p: Predicate) {
        for c in &mut self.analyzer.candidates {
            if c.stats.boundary_accounting && c.stats.head == p.head {
                // An unobserved exit must not join two presentation periods.
                // Retain the calibrated source, but require fresh episodes.
                c.relearn("interrupted VGA condition; episode incomplete");
            }
        }
    }
    fn analyzer_interrupt_vga_boundary(&mut self) {
        if let Some((p, _, _, _)) = self.analyzer.boundary_pending.take() {
            self.analyzer_vga_gap(p);
        }
        if let Some(suspended) = self.analyzer.boundary_suspended.take() {
            self.analyzer_vga_gap(suspended.predicate);
        }
    }
    fn analyzer_suspend_vga_boundary(&mut self, cs: u16, ip: u32, sp: u32) {
        if let Some((predicate, began, source_phase_end, credit_wait)) =
            self.analyzer.boundary_pending.take()
        {
            self.analyzer.boundary_suspended = Some(SuspendedVga {
                predicate,
                began,
                source_phase_end,
                credit_wait,
                irq_began: self.emulated_nanos(),
                return_to: (cs, ip, sp),
                saw_iret: false,
            });
        }
    }
    fn analyzer_resume_vga_boundary(&mut self) {
        let Some(mut suspended) = self.analyzer.boundary_suspended.take() else {
            return;
        };
        let now = self.emulated_nanos();
        if now.saturating_sub(suspended.irq_began) > 20_000_000
            || !self.machine.bus.code_valid(&suspended.predicate.version)
        {
            self.analyzer_vga_gap(suspended.predicate);
            return;
        }
        let r = self.registers();
        if suspended.saw_iret && (r.cs, r.ip, r.get32(Gpr32::Esp)) == suspended.return_to {
            // Preserve the observed condition across the real interrupt return,
            // but charge the entire interrupt (including nested IRQs) as work.
            self.analyzer.boundary_pending = Some((
                suspended.predicate,
                suspended.began.saturating_add(now - suspended.irq_began),
                suspended.source_phase_end,
                suspended.credit_wait,
            ));
            return;
        }
        let pc = self.debug_pc();
        suspended.saw_iret = self.machine.bus.code_bytes(pc, 1) == Some(&[0xcf][..])
            || self.machine.bus.code_bytes(pc, 2) == Some(&[0x66, 0xcf][..]);
        self.analyzer.boundary_suspended = Some(suspended);
    }
    fn analyzer_vga_prepare(&mut self, credit_wait: bool) -> Option<VgaStep> {
        let pc = self.registers().ip;
        let cs = self.registers().cs;
        let sp = self.registers().get32(Gpr32::Esp);
        let before = self.emulated_nanos();
        let pending = self.analyzer.boundary_pending.take();
        let (p, began, source_phase_end, credit_wait) =
            if let Some((p, began, source_phase_end, eligible)) = pending {
                if !self.machine.bus.code_valid(&p.version)
                    || pc <= p.head
                    || pc > p.branch
                    || Context::new(
                        self.registers(),
                        self.machine.cpu.control.cr0,
                        self.machine.cpu.control.cr3,
                    )
                    .is_none()
                {
                    self.analyzer_vga_gap(p);
                    return None;
                }
                (p, began, source_phase_end, credit_wait && eligible)
            } else {
                // Cheap admission before invoking the shared decoder. Segmented
                // code is not a candidate, but its elapsed time still counts as work.
                if !self.registers().code32()
                    || self.machine.bus.code_bytes(pc, 1) != Some(&[0xec][..])
                    || Context::new(
                        self.registers(),
                        self.machine.cpu.control.cr0,
                        self.machine.cpu.control.cr3,
                    )
                    .is_none()
                {
                    return None;
                }
                let p = self.analyzer_predicate().filter(|p| p.port)?;
                // The branch consumes the earlier IN result. Its source phase
                // must be captured at that read, not after TEST/Jcc or an IRQ,
                // when hardware may already have crossed a retrace edge.
                let phase = self
                    .machine
                    .bus
                    .vga
                    .retrace_deadline(p.operand as u16)
                    .map(|deadline| before.saturating_add(deadline));
                (p, before, phase, credit_wait)
            };
        Some(VgaStep {
            predicate: p,
            began,
            source_phase_end,
            credit_wait,
            before,
            pc,
            cs,
            sp,
        })
    }
    fn analyzer_vga_complete(&mut self, step: VgaStep, observation: &doslet_pc::StepObservation) {
        let VgaStep {
            predicate: p,
            began,
            source_phase_end,
            credit_wait,
            before,
            pc,
            cs,
            sp,
        } = step;
        let after = self.emulated_nanos();
        if observation.irq.is_some() {
            if pc > p.head {
                self.analyzer.boundary_suspended = Some(SuspendedVga {
                    predicate: p,
                    began,
                    source_phase_end,
                    credit_wait,
                    irq_began: before,
                    return_to: (cs, pc, sp),
                    saw_iret: false,
                });
            }
            return;
        }
        if observation.trace.incomplete
            || observation.service_writes_unobserved
            || observation.service_device_accesses_unobserved
            || self.registers().cs != cs
            || !self.machine.bus.code_valid(&p.version)
        {
            self.analyzer_vga_gap(p);
            return;
        }
        if pc == p.branch && [p.head, p.end].contains(&self.registers().ip) {
            let waiting = self.registers().ip == p.head;
            if waiting && credit_wait {
                self.analyzer.confirmed_wait_nanos = self
                    .analyzer
                    .confirmed_wait_nanos
                    .saturating_add(after - began);
            }
            self.analyzer.record_branch(
                p,
                waiting,
                after,
                self.hardware_config().cpu_mhz,
                Some(self.analyzer.confirmed_wait_nanos),
                source_phase_end,
            );
        } else if pc == p.head {
            // Source evidence is the value actually returned by the port read.
            if observation.trace.events[..observation.trace.len]
                .iter()
                .flatten()
                .any(|e| e.kind == Kind::PortRead && e.address == p.operand && e.width == 1)
            {
                self.analyzer.boundary_pending = Some((p, began, source_phase_end, credit_wait));
            }
        } else if self.registers().ip == p.branch {
            self.analyzer.boundary_pending = Some((p, began, source_phase_end, credit_wait));
        }
    }
    /// All instruction observers share VGA boundaries. A caller/IRQ probe
    /// must not hide a completed port loop from the episode model. Its traced
    /// time remains work, even when the port predicate is waiting.
    fn analyzer_observe_step(
        &mut self,
        observation: &mut doslet_pc::StepObservation,
        credit_wait: bool,
    ) -> Result<u32, RuntimeError> {
        let step = self.analyzer_vga_prepare(credit_wait);
        let used = self.execute_step_observed(observation)?;
        if let Some(step) = step {
            self.analyzer_vga_complete(step, observation);
        }
        Ok(used)
    }
    /// I/O cannot be hidden inside native RAM/register batches. Other traced
    /// execution paths use the same port observer through analyzer_observe_step.
    fn analyzer_vga_boundary(&mut self, budget: u32) -> Result<Option<u32>, RuntimeError> {
        let Some(step) = self.analyzer_vga_prepare(true) else {
            return Ok(None);
        };
        if step.pc == step.predicate.head {
            self.analyzer
                .gap("VGA boundary accounting outside instruction trace");
            if let Some(used) = self.machine.try_vga_wait_batch(budget)? {
                let after = self.emulated_nanos();
                self.analyzer.confirmed_wait_nanos = self
                    .analyzer
                    .confirmed_wait_nanos
                    .saturating_add(after - step.before);
                self.analyzer.record_branch(
                    step.predicate,
                    true,
                    after,
                    self.hardware_config().cpu_mhz,
                    Some(self.analyzer.confirmed_wait_nanos),
                    None,
                );
                return Ok(Some(used));
            }
        }
        let mut observation = doslet_pc::StepObservation::default();
        let used = self.execute_step_observed(&mut observation)?;
        self.analyzer.stats.observed_steps += u64::from(used);
        self.analyzer_vga_complete(step, &observation);
        Ok(Some(used))
    }
    pub(super) fn analyzer_advance(
        &mut self,
        steps: u64,
        budget: u32,
    ) -> Result<Option<u32>, RuntimeError> {
        if !self.analyzer.enabled {
            return Ok(None);
        }
        if self.storage_wait || self.continuation.is_some() {
            self.analyzer_interrupt_vga_boundary();
            self.analyzer_recurrence_cancel();
            self.analyzer.gap("runtime service continuation");
            self.analyzer.guards.stop();
            self.analyzer
                .caller
                .stop("runtime service continuation", self.emulated_nanos());
            return Ok(None);
        }
        self.analyzer_resume_vga_boundary();
        self.analyzer_recurrence_boundary();
        if let Some(used) = self.analyzer_caller_advance()? {
            return Ok(Some(used));
        }
        if let Some(used) = self.analyzer_guard_irq()? {
            return Ok(Some(used));
        }
        if self.analyzer.boundary_suspended.is_some() {
            return Ok(None);
        }
        if let Some(used) = self.analyzer_vga_boundary(budget)? {
            return Ok(Some(used));
        }
        if self.analyzer.remaining == 0 {
            if steps < self.analyzer.next {
                return Ok(None);
            }
            self.analyzer.gap("observation window boundary");
            self.analyzer.next = steps.saturating_add(PERIOD);
            self.analyzer.remaining = SCOUT;
            self.analyzer.extended = false;
            self.analyzer.stats.discovery_windows += 1;
            self.idle_abandon();
            self.cancel_menu_idle();
            self.analyzer.graph.analyze(
                &self.machine.bus,
                &self.machine.cpu.registers,
                self.machine.cpu.control.cr0,
                self.machine.cpu.control.cr3,
            );
        }
        let context = Context::new(
            self.registers(),
            self.machine.cpu.control.cr0,
            self.machine.cpu.control.cr3,
        );
        // Validate code/context every traced boundary. Unsupported IRQ contexts
        // interrupt episodes; returning to supported code starts fresh learning.
        if context.is_none() {
            self.analyzer.gap("unsupported execution context");
        } else if self.analyzer.irq_depth == 0
            && (self.analyzer.graph.context != context
                || !self.analyzer.graph.valid(&self.machine.bus, context))
        {
            self.analyzer.invalidate();
            self.analyzer.graph.analyze(
                &self.machine.bus,
                &self.machine.cpu.registers,
                self.machine.cpu.control.cr0,
                self.machine.cpu.control.cr3,
            );
        }
        let pc = self.registers().ip;
        let cs = self.registers().cs;
        let sp = self.registers().get32(Gpr32::Esp);
        let before = self.emulated_nanos();
        let mhz = self.hardware_config().cpu_mhz;
        let decoded = self
            .machine
            .bus
            .code_bytes(pc, 15)
            .and_then(|b| doslet_x86::analysis::decode(b, pc));
        if decoded
            .is_none_or(|d| matches!(d.effect, Effect::Unknown) || matches!(d.flow, Flow::Unknown))
        {
            self.analyzer.gap("unmodeled executed instruction");
        }
        let pending = self.analyzer.pending;
        let new = if context.is_some() && pending.is_none() {
            self.analyzer_predicate()
        } else {
            None
        };
        if new.is_some() && !self.analyzer.extended {
            // Root the static model at the discovered synchronization site,
            // rather than at an arbitrary sampling-window entry instruction.
            self.analyzer.extended = true;
            self.analyzer.graph.analyze(
                &self.machine.bus,
                &self.machine.cpu.registers,
                self.machine.cpu.control.cr0,
                self.machine.cpu.control.cr3,
            );
            if self.analyzer.graph.coverage == Coverage::CompleteGraph {
                self.analyzer.remaining = WINDOW;
                self.analyzer.stats.extended_windows += 1;
            }
        }
        let mut observation = doslet_pc::StepObservation::default();
        let used = self.analyzer_observe_step(&mut observation, self.analyzer.irq_depth == 0)?;
        self.analyzer.remaining -= 1;
        self.analyzer.stats.observed_steps += u64::from(used);
        self.analyzer_guard_discover(&observation, pc, context, sp);
        self.analyzer_guard_trace(&observation, pc, before);
        if let Some(vector) = observation.irq {
            self.analyzer.pending = None;
            if self.analyzer.irq_depth == 8 {
                self.analyzer.gap("interrupt nesting capacity");
            } else {
                self.analyzer.irq_stack[self.analyzer.irq_depth] = Some((vector, cs, pc, sp));
                self.analyzer.irq_depth += 1;
            }
        }
        let irq = self
            .analyzer
            .irq_depth
            .checked_sub(1)
            .and_then(|i| self.analyzer.irq_stack[i].map(|x| x.0));
        if observation.trace.incomplete || observation.device_writes_unobserved {
            if observation.device_writes_unobserved {
                self.analyzer.stats.unobserved_device_writes += 1;
            }
            self.analyzer
                .gap("unobserved device write or incomplete transaction");
        }
        for event in observation.trace.events[..observation.trace.len]
            .iter()
            .flatten()
        {
            if event.kind == Kind::Write {
                for c in &mut self.analyzer.candidates {
                    let Some(p) = c.predicate else {
                        continue;
                    };
                    if p.port
                        || u64::from(event.address) >= u64::from(p.operand) + u64::from(p.width)
                        || u64::from(p.operand) >= u64::from(event.address) + u64::from(event.width)
                    {
                        continue;
                    }
                    c.stats.writes += 1;
                    c.stats.writer_pc = observation.irq.is_none().then_some(pc);
                    c.stats.writer_kind = if observation.irq.is_some() {
                        "IRQ entry"
                    } else if irq.is_some() {
                        "guest instruction in observed IRQ"
                    } else {
                        "guest instruction; interrupt ownership unobserved"
                    };
                    c.stats.writer_irq = irq;
                    c.stats.writer_address = Some(event.address);
                    c.stats.writer_width = event.width;
                    c.stats.writer_value = Some(event.value);
                    c.stats.writer_nanos = Some(before);
                    if event.address != p.operand || event.width != p.width {
                        c.stats.partial_writes += 1;
                        c.last_write = None;
                        c.stats.source_transitions = 0;
                        c.gap("partial predicate write");
                        continue;
                    }
                    if observation.irq.is_none()
                        && let Some(vector) = irq
                    {
                        let decreased = observation.trace.events[..observation.trace.len]
                            .iter()
                            .flatten()
                            .any(|e| {
                                e.kind == Kind::Read
                                    && e.address == p.operand
                                    && e.width == p.width
                                    && event.value < e.value
                            });
                        if let Some((time, _old, writer, old_irq)) = c.last_write
                            && writer == pc
                            && old_irq == vector
                            && decreased
                            && before > time
                        {
                            c.stats.source_transitions += 1;
                            c.stats.source_period_nanos = Some(before - time);
                            c.stats.state = "observed IRQ writer";
                        }
                        c.last_write = Some((before, event.value, pc, vector));
                    }
                }
            }
        }
        if observation.service_writes_unobserved || observation.service_device_accesses_unobserved {
            self.analyzer.stats.unobserved_service_writes +=
                u64::from(observation.service_writes_unobserved);
            self.analyzer.stats.unobserved_service_device_accesses +=
                u64::from(observation.service_device_accesses_unobserved);
            self.analyzer.gap("unobserved runtime service effects");
        }
        let after = self.emulated_nanos();
        if observation.irq.is_none()
            && let Some(context) = context
            && self.registers().cs == cs
        {
            if let Some(decoded) = decoded
                && let Flow::Branch { target, .. } = decoded.flow
                && target < pc
                && self.registers().ip == target
                && !observation.trace.incomplete
            {
                self.analyzer_backedge_discover(pc, decoded.length, context, sp);
            }
            if let Some((p, value)) = pending {
                if !self.machine.bus.code_valid(&p.version) {
                    self.analyzer.invalidate();
                } else if pc == p.branch && [p.head, p.end].contains(&self.registers().ip) {
                    self.analyzer.record_branch(
                        p,
                        self.registers().ip == p.head,
                        after,
                        mhz,
                        None,
                        None,
                    );
                    self.analyzer.pending = None;
                } else if pc > p.head && pc < p.branch && self.registers().ip == p.branch {
                    self.analyzer.pending = Some((p, value));
                } else {
                    self.analyzer.pending = None;
                }
            } else if let Some(p) = new {
                let kind = if p.port { Kind::PortRead } else { Kind::Read };
                if let Some(e) = observation.trace.events[..observation.trace.len]
                    .iter()
                    .flatten()
                    .find(|e| e.kind == kind && e.address == p.operand && e.width == p.width)
                {
                    self.analyzer.pending = Some((p, e.value));
                }
            }
        } else {
            self.analyzer.pending = None;
        }
        if decoded.is_some_and(|d| d.flow == Flow::InterruptReturn)
            && let Some(i) = self.analyzer.irq_depth.checked_sub(1)
            && let Some((_, return_cs, return_pc, return_sp)) = self.analyzer.irq_stack[i]
            && self.registers().cs == return_cs
            && self.registers().ip == return_pc
            && self.registers().get32(Gpr32::Esp) == return_sp
        {
            self.analyzer.irq_stack[i] = None;
            self.analyzer.irq_depth -= 1;
        }
        if self.analyzer.remaining == 0 {
            self.analyzer.gap("trace budget exhausted");
        }
        Ok(Some(used))
    }
    pub(super) fn analyzer_control(&mut self) {
        if !self.analyzer.enabled {
            return;
        }
        let actual_mhz = self.hardware_config().cpu_mhz;
        for c in &mut self.analyzer.candidates {
            if c.predicate.is_none() {
                continue;
            }
            if c.predicate
                .is_some_and(|p| !self.machine.bus.code_valid(&p.version))
            {
                c.gap("predicate code invalidated");
                c.predicate = None;
                c.stats.state = "invalidated";
                self.analyzer.relearn_until = 0;
                continue;
            }
            if c.predicate.is_some_and(|p| {
                p.port
                    && self
                        .machine
                        .bus
                        .vga
                        .retrace_deadline(p.operand as u16)
                        .is_none()
            }) {
                c.gap("VGA source changed");
                self.analyzer.relearn_until = 0;
            }
            if c.stats.measured_mhz != actual_mhz {
                c.relearn("relearning at changed MHz");
            }
        }
        let now = self.emulated_nanos();
        let ceiling = self.analyzer.limit();
        let audio_misses = self.machine.bus.dsp.diagnostics().block_irq_deadline_misses;
        let audio_missed = audio_misses > self.analyzer.audio_deadline_misses;
        self.analyzer.audio_deadline_misses = audio_misses;
        let mut predicted = None;
        let mut missed = audio_missed;
        let mut awaiting_cadence = false;
        let mut validating_cadence = false;
        let mut cadence_floor = 0;
        for c in &mut self.analyzer.candidates {
            if c.predicate.is_some() && c.stats.period_nanos != 0 {
                awaiting_cadence |=
                    c.stats.reference_period_nanos == 0 && !c.stats.variable_cadence;
                validating_cadence |= c.stats.port
                    && c.stats.reference_period_nanos != 0
                    && (c.qualified_episodes
                        < if actual_mhz == ceiling {
                            c.recent_periods.len() as u32
                        } else {
                            2
                        }
                        || c.cadence_mean().is_none());
                // A countdown guard can span several callers and update paths;
                // its period bounds work but is not a presentation cadence.
                // VGA cadence must not relax as throughput falls.
                if c.stats.port
                    && actual_mhz < ceiling
                    && c.stats.measured_mhz == actual_mhz
                    && c.stats.reference_period_nanos != 0
                    && c.qualified_episodes >= 2
                    && c.cadence_mean().is_some_and(|period| {
                        period > c.stats.reference_period_nanos.saturating_mul(33) / 32 + 100_000
                    })
                {
                    if !c.overdue {
                        c.stats.missed_deadlines += 1;
                        c.overdue = true;
                    }
                    // Heavy work already raises the ordinary budget. Keep a
                    // polling-frequency floor only when work still fits; a
                    // transient expensive scene must not pin idle scenes high.
                    if c.recent_max < c.stats.reference_period_nanos.saturating_mul(3) / 4 {
                        let floor = actual_mhz
                            .saturating_add((actual_mhz / 4).max(1))
                            .min(ceiling);
                        if floor >= c.stats.cadence_floor_mhz {
                            let failure = c.cadence_mean().unwrap_or(0);
                            c.floor_failure_mean = if floor == c.stats.cadence_floor_mhz {
                                c.floor_failure_mean.max(failure)
                            } else {
                                failure
                            };
                            c.stats.cadence_floor_mhz = floor;
                        }
                    }
                    missed = true;
                }
                cadence_floor = cadence_floor.max(c.stats.cadence_floor_mhz);
            }
            if !c.stats.variable_cadence
                && (c.qualified_episodes >= 8 || c.stats.reference_period_nanos != 0)
                && c.work_start.is_some_and(|start| {
                    let period = if c.stats.reference_period_nanos != 0 {
                        c.stats.reference_period_nanos
                    } else {
                        c.stats.period_nanos
                    };
                    let elapsed = now.saturating_sub(start);
                    if let Some(previous) = c.wait_counter_start {
                        let waiting = self.analyzer.confirmed_wait_nanos.saturating_sub(previous);
                        let work = elapsed.saturating_sub(waiting);
                        period != 0 && work > (period.saturating_mul(3) / 4).saturating_sub(100_000)
                    } else {
                        c.wait_start.is_none() && period != 0 && elapsed > period
                    }
                })
            {
                if !c.overdue {
                    c.stats.missed_deadlines += 1;
                    c.overdue = true;
                }
                missed = true;
            }
            if c.predicate.is_none() {
                continue;
            }
            if !c.stats.boundary_accounting
                && self.analyzer.graph.coverage != Coverage::CompleteGraph
            {
                c.stats.predicted_mhz = None;
                continue;
            }
            if let Some(mhz) = c.budget(now, ceiling) {
                predicted = Some(predicted.unwrap_or(0).max(mhz));
            }
        }
        // For VGA boundary accounting, unknown code is charged in full as work,
        // never excluded as idle. Sparse instruction traces need their own CFG.
        if missed || awaiting_cadence {
            predicted = None;
        }
        predicted = predicted.map(|mhz| mhz.max(cadence_floor));
        self.analyzer.stats.reason = if audio_missed {
            "guest DMA block IRQ acknowledgement deadline missed"
        } else if missed {
            "measured cadence or work deadline exceeded"
        } else if awaiting_cadence {
            "learning cadence at configured ceiling"
        } else if validating_cadence {
            "validating presentation cadence"
        } else if predicted.is_some() {
            "measured budget available"
        } else {
            "insufficient complete evidence; configured ceiling"
        };
        if !self.analyzer.controller {
            return;
        }
        let current = self.hardware_config().cpu_mhz;
        if predicted.is_none()
            && !missed
            && now < self.analyzer.relearn_until
            && (self.analyzer.graph.coverage == Coverage::CompleteGraph
                || self
                    .analyzer
                    .candidates
                    .iter()
                    .any(|c| c.predicate.is_some() && c.stats.boundary_accounting))
        {
            self.analyzer.stats.reason = "bounded relearning after speed adjustment";
            return;
        }
        let target = predicted.unwrap_or(ceiling);
        if target > current {
            self.analyzer.stats.recoveries += 1;
            self.analyzer.stats.last_recovery_reason = if audio_missed {
                "guest DMA block IRQ acknowledgement deadline missed"
            } else if missed {
                "episode deadline exceeded"
            } else if predicted.is_some() {
                "measured work requires higher throughput"
            } else if now >= self.analyzer.relearn_until && self.analyzer.relearn_until != 0 {
                "new throughput did not qualify before relearning expired"
            } else {
                "required model evidence unavailable"
            };
            let _ = self.machine.set_cpu_mhz(ceiling);
            self.analyzer.adjustment_at = now;
        } else if now.saturating_sub(self.analyzer.adjustment_at) >= 100_000_000
            && !validating_cadence
            && target.saturating_add(current / 8) < current
        {
            let next = current
                .saturating_sub((current / 16).max(1))
                .max(target)
                .max(1);
            let _ = self.machine.set_cpu_mhz(next);
            self.analyzer.stats.reductions += 1;
            self.analyzer.adjustment_at = now;
            // Allow the first partial frame plus eight complete episodes and
            // observed ceiling-speed jitter. The last single period alone
            // can expire just before valid slow-cadence evidence completes.
            let relearn = self
                .analyzer
                .candidates
                .iter()
                .filter(|c| c.stats.predicted_mhz.is_some())
                .map(|c| {
                    c.stats
                        .period_nanos
                        .max(c.stats.reference_period_max_nanos)
                        .saturating_mul(10)
                })
                .max()
                .unwrap_or(0)
                .clamp(500_000_000, 2_000_000_000);
            self.analyzer.relearn_until = now.saturating_add(relearn);
            for c in &mut self.analyzer.candidates {
                c.relearn("validating changed throughput");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doslet_x86::flags;
    pub(super) fn session(code: &[u8]) -> Session {
        let mut s = super::super::cooperative::tests::session();
        s.set_menu_idle_site(None);
        s.set_idle_loops(false);
        s.set_vga_wait_acceleration(false);
        s.machine.cpu.flags = 2;
        for (i, &b) in code.iter().enumerate() {
            s.machine.bus.write_u8(0x10000 + i as u32, b).unwrap();
        }
        s.machine.cpu.registers.ip = 0x10000;
        s
    }
    #[test]
    fn graph_rejects_contexts_and_reports_limits_without_execution() {
        let mut s = session(&[0x83, 0x3d, 0, 0x30, 0, 0, 0, 0x75, 0xf7, 0xc3]);
        let mut graph = Graph::default();
        let analyze = |g: &mut Graph, s: &Session| {
            g.analyze(&s.machine.bus, s.registers(), s.machine.cpu.control.cr0, 0)
        };
        analyze(&mut graph, &s);
        assert_eq!(graph.coverage, Coverage::CompleteGraph);
        assert_eq!(graph.backedges, 1);
        assert_eq!(graph.predicates, 1);
        let context = graph.context;
        assert!(graph.valid(&s.machine.bus, context));
        s.machine.bus.write_u8(0x10000, 0x90).unwrap();
        assert!(!graph.valid(&s.machine.bus, context));
        s.machine.bus.set_a20(false);
        assert!(!graph.valid(&s.machine.bus, context));
        let real = Session::new(&[0xeb, 0xfe]).unwrap();
        analyze(&mut graph, &real);
        assert_eq!(graph.coverage, Coverage::UnsupportedContext);
        for (code, reason) in [
            (&[0x90; 80][..], Coverage::Capacity),
            (&[0xff, 0xe0][..], Coverage::UnknownEffect),
            (&[0x0f, 0x0b][..], Coverage::UnsupportedInstruction),
        ] {
            let s = session(code);
            analyze(&mut graph, &s);
            assert_eq!(graph.coverage, reason);
        }
    }
    #[test]
    fn vga_register_mask_requires_the_observed_retrace_bit() {
        // Generic IN AL,DX / TEST AL,AH / JZ loop, with no game addresses.
        let mut s = session(&[0xec, 0x84, 0xe0, 0x74, 0xfb, 0xeb, 0xf9]);
        s.machine.cpu.registers.set(super::super::Gpr16::Dx, 0x3da);
        s.machine.cpu.registers.set_byte(4, 8);
        assert!(s.analyzer_predicate().is_some());
        s.set_synchronization_analysis(true);
        s.start();
        s.advance(10);
        assert!(s.synchronization_analysis().candidates[0].port);
        s.machine.cpu.registers.ip = 0x10000;
        s.machine.cpu.registers.set_byte(4, 1);
        assert!(s.analyzer_predicate().is_none());
        s.machine.bus.write_u8(0x10002, 0xc4).unwrap();
        s.machine.cpu.registers.set_byte(4, 8);
        assert!(s.analyzer_predicate().is_some());
    }
    #[test]
    fn only_strictly_proven_passes_contribute_idle_time() {
        for (code, expected_idle) in [(&[0xeb, 0xfe][..], true), (&[0x40, 0xeb, 0xfd][..], false)] {
            let mut a = session(code);
            let mut b = session(code);
            a.set_idle_loops(true);
            a.set_synchronization_analysis(true);
            a.start();
            b.start();
            let allocated = allocation_counter::measure(|| {
                for _ in 0..30 {
                    assert_eq!(a.advance(10000), b.advance(10000));
                    assert!(a.machine.cpu.matches(&b.machine.cpu.snapshot()));
                    assert_eq!(a.emulated_nanos(), b.emulated_nanos());
                }
            });
            assert_eq!(allocated.count_total, 0);
            let stats = a.synchronization_analysis();
            assert_eq!(stats.proven_idle_nanos != 0, expected_idle);
            assert!(stats.proven_idle_nanos <= a.emulated_nanos());
            assert_eq!(stats.proven_idle_nanos, a.analyzer.confirmed_wait_nanos);
            // A fixed point alone is not a synchronization source or episode.
            assert!(stats.candidates.iter().all(|c| c.predicted_mhz.is_none()));
        }
    }
    #[test]
    fn vga_boundaries_preserve_batches_and_charge_unknown_code_as_work() {
        // PUSH/POP are deliberately outside the static effect inventory. They
        // are still real work, not a reason to lose machine-clock accounting.
        let code = [0xec, 0x24, 8, 0x74, 0xfb, 0x50, 0x58, 0x40, 0xeb, 0xf6];
        let mut a = session(&code);
        let mut b = session(&code);
        for s in [&mut a, &mut b] {
            s.machine.cpu.registers.set(super::super::Gpr16::Dx, 0x3da);
            s.set_vga_wait_acceleration(true);
            s.start();
        }
        a.set_synchronization_analysis(true);
        let allocations = allocation_counter::measure(|| {
            for _ in 0..200 {
                assert_eq!(a.advance(10000), b.advance(10000));
                assert!(a.machine.cpu.matches(&b.machine.cpu.snapshot()));
                assert_eq!(a.emulated_nanos(), b.emulated_nanos());
            }
        });
        assert_eq!(allocations.count_total, 0);
        assert_eq!(
            a.machine.bus.ram_view(0..0xa0000),
            b.machine.bus.ram_view(0..0xa0000)
        );
        let stats = a.synchronization_analysis();
        let c = stats.candidates[0];
        assert!(c.boundary_accounting);
        assert!(c.episodes >= 8, "{stats:?}");
        assert!(c.work_nanos > 0 && c.work_nanos < c.period_nanos);
        assert!(c.predicted_mhz.is_some(), "{stats:?}");
        assert!(stats.observed_steps < 500_000, "{stats:?}");
    }
    #[test]
    fn observation_is_allocation_free_and_matches_reference() {
        for dynamic in [false, true] {
            let code = [0x83, 0x3d, 0, 0x30, 0, 0, 0, 0x75, 0xf7, 0x40, 0xeb, 0xf4];
            let mut a = session(&code);
            let mut b = session(&code);
            a.set_dynamic_core(dynamic);
            b.set_dynamic_core(dynamic);
            a.start();
            b.start();
            assert_eq!(a.advance(10000), b.advance(10000));
            let compiled_a = a.prepare_dynamic(64);
            let compiled_b = b.prepare_dynamic(64);
            assert_eq!(compiled_a, compiled_b);
            #[cfg(all(
                feature = "dynamic-core",
                not(target_arch = "wasm32"),
                any(target_arch = "x86_64", target_arch = "aarch64")
            ))]
            if dynamic {
                assert!(
                    compiled_b > 0,
                    "comparison must exercise compiled native code"
                );
            }
            a.set_synchronization_analysis(true);
            let measured = allocation_counter::measure(|| {
                for budget in [1, 2, 7, 31, 256, 1000, 10000] {
                    assert_eq!(a.advance(budget), b.advance(budget));
                }
            });
            assert_eq!(measured.count_total, 0);
            assert!(a.machine.cpu.matches(&b.machine.cpu.snapshot()));
            assert_eq!(a.emulated_nanos(), b.emulated_nanos());
            assert_eq!(a.steps(), b.steps());
            assert_eq!(
                a.machine.bus.ram_view(0..0xa0000),
                b.machine.bus.ram_view(0..0xa0000)
            );
            assert_eq!(a.vram_view(), b.vram_view());
            assert!(a.synchronization_analysis().observed_steps > 0);
            assert_eq!(a.synchronization_analysis().candidates[0].episodes, 0);
        }
    }
    #[test]
    fn incomplete_and_catchup_episodes_never_invent_spare_work() {
        let mut c = Candidate::default();
        c.stats.port = true;
        c.branch(false, 0, 100);
        c.branch(false, 500, 100); // catch-up remains work
        c.branch(true, 1000, 100);
        c.branch(false, 10_000_000, 100);
        assert_eq!(c.stats.work_nanos, 1000);
        assert_eq!(c.stats.episodes, 1);
        c.branch(true, 10_000_001, 100);
        c.gap("trace budget exhausted");
        c.branch(false, 20_000_000, 100);
        assert_eq!(c.stats.episodes, 1);
        assert_eq!(c.budget(20_000_000, 100), None);
        for n in 2..12 {
            c.branch(true, n * 10_000_000 + 1000, 100);
            c.branch(false, (n + 1) * 10_000_000, 100);
        }
        assert!(c.budget(120_000_000, 100).is_some());
        assert_eq!(c.budget(120_000_000 + FRESH + 1, 100), None);
        c.branch(false, 130_000_000, 50);
        assert_eq!(c.budget(130_000_000, 100), None);
    }
    #[test]
    fn recent_work_window_recovers_after_a_completed_heavy_phase() {
        let mut c = Candidate::default();
        c.stats.port = true;
        c.branch(false, 0, 100);
        c.branch(true, 9_000_000, 100);
        c.branch(false, 10_000_000, 100);
        for episode in 1..16 {
            c.branch(true, episode * 10_000_000 + 1_000_000, 100);
            c.branch(false, (episode + 1) * 10_000_000, 100);
        }
        assert_eq!(c.budget(160_000_000, 100), Some(100));
        c.branch(true, 161_000_000, 100);
        c.branch(false, 170_000_000, 100);
        assert_eq!(c.recent_max, 1_000_000);
        assert_eq!(c.stats.work_max_nanos, 9_000_000);
        assert!(c.budget(170_000_000, 100).unwrap() < 100);
    }
    #[test]
    fn foreground_writes_are_not_clock_sources_and_partial_writes_are_explicit() {
        // The CPU first waits at a discovered predicate, then is steered to a
        // synthetic writer. No IRQ is pending or entered.
        let mut s = session(&[0x83, 0x3d, 0, 0x30, 0, 0, 0, 0x75, 0xf7, 0xeb, 0xfe]);
        for (i, b) in [0xc6, 0x05, 1, 0x30, 0, 0, 7, 0xeb, 0xfe]
            .into_iter()
            .enumerate()
        {
            s.machine.bus.write_u8(0x10100 + i as u32, b).unwrap();
        }
        s.set_synchronization_analysis(true);
        s.start();
        s.advance(10);
        s.machine.cpu.registers.ip = 0x10100;
        s.advance(1);
        let c = s.synchronization_analysis().candidates[0];
        assert_eq!(c.writer_pc, Some(0x10100));
        assert_eq!(c.writer_irq, None);
        assert_eq!(c.partial_writes, 1);
        assert_eq!(c.predicted_mhz, None);
    }
    #[test]
    fn actual_irq_entry_attributes_counter_store() {
        let mut s = session(&[0x83, 0x3d, 0, 0x30, 0, 0, 0, 0x75, 0xf7, 0xeb, 0xfe]);
        // Protected interrupt gate for vector 8, code selector 18h.
        s.machine.cpu.control.idtr.base = 0x8000;
        s.machine.cpu.control.idtr.limit = 0x7ff;
        for (i, b) in [0, 1, 0x18, 0, 0, 0x8e, 1, 0].into_iter().enumerate() {
            s.machine.bus.write_u8(0x8040 + i as u32, b).unwrap();
        }
        for (i, b) in [0xff, 0x0d, 0, 0x30, 0, 0, 0xcf].into_iter().enumerate() {
            s.machine.bus.write_u8(0x10100 + i as u32, b).unwrap();
        }
        s.set_synchronization_analysis(true);
        s.start();
        s.advance(10);
        s.machine.cpu.flags |= flags::INTERRUPT;
        s.machine.bus.pic.raise_edge(0);
        s.advance(2);
        assert_eq!(s.fault(), None);
        let c = s.synchronization_analysis().candidates[0];
        assert_eq!(c.writer_pc, Some(0x10100));
        assert_eq!(c.writer_irq, Some(8));
        assert_eq!(c.writer_address, Some(0x3000));
        assert_eq!(c.writer_width, 4);
        assert!(c.writer_value.is_some());
        assert!(c.writer_nanos.is_some());
        assert_eq!(c.writes, 1);
        assert_eq!(c.source_transitions, 0);
    }
    #[test]
    fn stale_or_missing_model_keeps_controller_at_user_ceiling() {
        let mut s = session(&[0x40, 0xeb, 0xfd]);
        s.set_cpu_mhz(133).unwrap();
        s.set_adaptive_cpu(true);
        s.start();
        let measured = allocation_counter::measure(|| {
            s.advance(1000);
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(s.hardware_config().cpu_mhz, 133);
        assert_eq!(s.synchronization_analysis().configured_mhz, 133);
        s.set_adaptive_cpu(false);
        assert_eq!(s.hardware_config().cpu_mhz, 133);
    }
}

#[cfg(test)]
mod controller_tests {
    use super::*;
    #[test]
    fn host_capacity_cap_bounds_recovery_without_replacing_the_user_ceiling() {
        let mut s = super::tests::session(&[0x40, 0xeb, 0xfd]);
        s.set_cpu_mhz(266).unwrap();
        s.set_adaptive_cpu(true);
        s.set_host_cpu_limit(80).unwrap();
        assert_eq!(s.hardware_config().cpu_mhz, 80);
        // Missing evidence restores throughput, but only to host capacity.
        s.machine.set_cpu_mhz(40).unwrap();
        s.analyzer_control();
        assert_eq!(s.hardware_config().cpu_mhz, 80);
        let stats = s.synchronization_analysis();
        assert_eq!((stats.configured_mhz, stats.host_limit_mhz), (266, 80));
        // Without the cap the controller may return to the user ceiling.
        s.set_host_cpu_limit(0).unwrap();
        s.analyzer_control();
        assert_eq!(s.hardware_config().cpu_mhz, 266);
        // Fixed rates ignore host caps.
        s.set_adaptive_cpu(false);
        s.set_host_cpu_limit(10).unwrap();
        assert_eq!(s.hardware_config().cpu_mhz, 266);
    }
    #[test]
    fn a_new_guest_dma_ack_miss_recovers_but_history_and_diagnostics_do_not_retune() {
        let mut s = super::tests::session(&[0x40, 0xeb, 0xfd]);
        for byte in [0xc6, 0, 0, 0] {
            s.machine.bus.dsp.write_command(byte).unwrap();
        }
        for _ in 0..2 {
            s.machine.bus.dsp.accept_sample(0).unwrap();
        }
        assert_eq!(s.machine.bus.dsp.diagnostics().block_irq_deadline_misses, 1);
        s.set_adaptive_cpu(true);
        s.analyzer.graph.coverage = Coverage::CompleteGraph;
        s.analyzer.relearn_until = 1_000_000_000;
        s.machine.set_cpu_mhz(20).unwrap();
        s.analyzer_control();
        assert_eq!(
            s.hardware_config().cpu_mhz,
            20,
            "old misses are the baseline"
        );
        let allocated = allocation_counter::measure(|| {
            s.machine.bus.dsp.accept_sample(0).unwrap();
            s.analyzer_control();
        });
        assert_eq!(allocated.count_total, 0);
        assert_eq!(s.hardware_config().cpu_mhz, 33);
        assert_eq!(s.analyzer.stats.recoveries, 1);
        assert_eq!(
            s.analyzer.stats.last_recovery_reason,
            "guest DMA block IRQ acknowledgement deadline missed"
        );
        s.machine.set_cpu_mhz(20).unwrap();
        s.analyzer_control();
        assert_eq!(s.hardware_config().cpu_mhz, 20, "one miss is consumed once");
        s.set_synchronization_analysis(true);
        s.machine.set_cpu_mhz(20).unwrap();
        s.machine.bus.dsp.accept_sample(0).unwrap();
        s.analyzer_control();
        assert_eq!(s.hardware_config().cpu_mhz, 20, "diagnostics do not retune");
    }
    #[test]
    fn ceiling_reference_waits_for_a_full_cadence_window() {
        let mut s = super::super::cooperative::tests::session();
        s.set_adaptive_cpu(true);
        let p = Predicate {
            head: 0x10000,
            branch: 0x10003,
            end: 0x10005,
            operand: 0x3da,
            width: 1,
            port: true,
            version: s.machine.bus.code_check(0x10000, 5).unwrap(),
        };
        let (mut now, mut wait) = (0, 0);
        s.analyzer
            .record_branch(p, false, now, 33, Some(wait), None);
        // A short initial run can understate the normal presentation period.
        // Both halves fit the admitted spread and belong in the reference.
        for (index, period) in core::iter::repeat_n(10_000_000, 8)
            .chain(core::iter::repeat_n(11_000_000, 8))
            .enumerate()
        {
            s.analyzer
                .record_branch(p, true, now + 1_000_000, 33, Some(wait), None);
            now += period;
            wait += period - 1_000_000;
            s.analyzer
                .record_branch(p, false, now, 33, Some(wait), None);
            if index < 15 {
                assert_eq!(s.analyzer.candidates[0].stats.reference_period_nanos, 0);
            }
        }
        let c = &s.analyzer.candidates[0];
        assert_eq!(c.stats.reference_period_nanos, 10_466_666);
        assert!(!c.stats.variable_cadence);
    }

    #[test]
    fn irregular_ceiling_periods_cannot_establish_or_retain_a_cadence_floor() {
        let mut s = super::super::cooperative::tests::session();
        s.set_adaptive_cpu(true);
        let p = Predicate {
            head: 0x10000,
            branch: 0x10003,
            end: 0x10005,
            operand: 0x3da,
            width: 1,
            port: true,
            version: s.machine.bus.code_check(0x10000, 5).unwrap(),
        };
        let (mut now, mut wait) = (0, 0);
        s.analyzer
            .record_branch(p, false, now, 33, Some(wait), None);
        for period in core::iter::repeat_n(10_000_000, 16)
            .chain([10_000_000, 20_000_000].into_iter().cycle().take(16))
        {
            s.analyzer
                .record_branch(p, true, now + 1_000_000, 33, Some(wait), None);
            now += period;
            wait += period - 1_000_000;
            s.analyzer
                .record_branch(p, false, now, 33, Some(wait), None);
            if now == 160_000_000 {
                let c = &mut s.analyzer.candidates[0];
                assert_eq!(c.stats.reference_period_nanos, 10_000_000);
                c.stats.cadence_floor_mhz = 33;
            }
        }
        let c = &mut s.analyzer.candidates[0];
        assert!(c.stats.variable_cadence);
        assert_eq!(c.stats.reference_period_nanos, 0);
        assert_eq!(c.stats.cadence_floor_mhz, 0);
        assert_eq!(c.budget(now, 33), None);
        assert_eq!(
            c.stats.reason,
            "variable VGA cadence; no presentation deadline"
        );
        c.relearn("throughput change");
        assert!(
            c.stats.variable_cadence,
            "retuning cannot invent a deadline"
        );
    }

    #[test]
    fn both_edges_of_paired_vga_polling_keep_complete_periods() {
        let mut s = super::super::cooperative::tests::session();
        s.set_menu_idle_site(None);
        s.set_idle_loops(false);
        s.machine.cpu.flags = 2;
        s.machine.bus.vga.set_vbe_101h(false);
        s.machine.cpu.registers.set(super::super::Gpr16::Dx, 0x3da);
        s.machine.cpu.registers.set_byte(4, 8);
        // Retrace end, NOP, retrace start, then some foreground work.
        for (i, b) in [
            0xec, 0x84, 0xc4, 0x75, 0xfb, 0x90, 0xec, 0x84, 0xc4, 0x74, 0xfb, 0xb9, 0xe8, 3, 0, 0,
            0x49, 0x75, 0xfd, 0xeb, 0xeb,
        ]
        .into_iter()
        .enumerate()
        {
            s.machine.bus.write_u8(0x10000 + i as u32, b).unwrap();
        }
        s.set_synchronization_analysis(true);
        s.start();
        // Seed a taken first loop after its initial observation. Later calls
        // arrive outside retrace and normally exit it immediately.
        s.advance(20);
        while s.registers().ip != 0x10000 {
            s.advance(1);
        }
        s.machine.bus.vga.set_vbe_101h(false);
        s.machine.bus.vga.advance_pixel_ticks(800 * 490);
        while s.emulated_nanos() < 300_000_000 {
            s.advance(10000);
            assert_eq!(s.fault(), None);
        }
        let candidates = s.synchronization_analysis().candidates;
        let a = candidates.iter().find(|c| c.head == 0x10000).unwrap();
        let b = candidates.iter().find(|c| c.head == 0x10006).unwrap();
        assert!(a.episodes >= 8 && b.episodes >= 8, "{candidates:?}");
        assert!(a.episodes.abs_diff(b.episodes) <= 1, "{candidates:?}");
        assert!(
            a.period_nanos < 20_000_000 && b.period_nanos < 20_000_000,
            "{candidates:?}"
        );
    }
    #[test]
    fn vga_exit_keeps_the_phase_of_its_actual_read_across_a_device_edge() {
        let mut s = super::super::cooperative::tests::session();
        s.set_menu_idle_site(None);
        s.machine.cpu.flags = 2;
        s.machine.bus.vga.set_vbe_101h(false);
        s.machine.bus.vga.advance_pixel_ticks(800 * 490);
        s.machine.cpu.registers.set(super::super::Gpr16::Dx, 0x3da);
        for (i, b) in [0xec, 0xa8, 8, 0x74, 0xfb].into_iter().enumerate() {
            s.machine.bus.write_u8(0x10000 + i as u32, b).unwrap();
        }
        s.set_synchronization_analysis(true);
        let p = s.analyzer_predicate().unwrap();
        s.analyzer.record_branch(p, false, 0, 33, Some(0), None);
        s.analyzer.record_branch(p, true, 0, 33, Some(0), None);
        let phase = s.emulated_nanos() + s.machine.bus.vga.retrace_deadline(0x3da).unwrap();
        s.analyzer_vga_boundary(1).unwrap(); // actual IN reads retrace active
        assert_eq!(s.registers().get_byte(0) & 8, 8);
        s.machine.bus.vga.advance_pixel_ticks(800 * 3); // now retrace is inactive
        s.analyzer_vga_boundary(1).unwrap(); // TEST uses the retained AL
        s.analyzer_vga_boundary(1).unwrap(); // JZ exits using those flags
        assert_eq!(s.analyzer.candidates[0].port_exit_phase, Some(phase));
        assert_ne!(
            phase,
            s.emulated_nanos() + s.machine.bus.vga.retrace_deadline(0x3da).unwrap()
        );
    }
    #[test]
    fn isolated_long_period_does_not_redefine_cadence_after_retuning() {
        let mut c = Candidate::default();
        c.stats.port = true;
        c.stats.boundary_accounting = true;
        let (mut now, mut wait) = (0, 0);
        c.branch_measured(false, now, 33, Some(wait), None);
        for _ in 0..16 {
            c.branch_measured(true, now + 1_000_000, 33, Some(wait), None);
            now += 10_000_000;
            wait += 9_000_000;
            c.branch_measured(false, now, 33, Some(wait), None);
        }
        assert_eq!(c.cadence_mean(), Some(10_000_000));
        c.branch_measured(false, now, 20, Some(wait), None);
        for duration in [20_000_000, 10_000_000, 10_000_000] {
            c.branch_measured(true, now + 1_000_000, 20, Some(wait), None);
            now += duration;
            wait += duration - 1_000_000;
            c.branch_measured(false, now, 20, Some(wait), None);
        }
        assert_eq!(c.cadence_mean(), Some(10_000_000));
        assert_eq!(c.qualified_episodes, 3, "work still needs requalification");
        c.stats.reference_period_nanos = 10_000_000;
        c.branch_measured(false, now, 33, Some(wait), None);
        assert_eq!(
            c.cadence_mean(),
            None,
            "recovery discards slower-speed history"
        );
        assert_eq!(c.stats.reference_period_nanos, 10_000_000);
        c.gap("changed source");
        assert_eq!(c.cadence_mean(), None);
    }
    #[test]
    fn inflight_wait_is_excluded_but_expensive_work_recovers_immediately() {
        let mut s = super::tests::session(&[0x40, 0xeb, 0xfd]);
        s.start();
        s.advance(200_000);
        let now = s.emulated_nanos();
        assert!(now > 20_000_000);
        s.set_adaptive_cpu(true);
        s.machine.set_cpu_mhz(20).unwrap();
        let p = Predicate {
            head: 0x10000,
            branch: 0x10001,
            end: 0x10003,
            operand: 0x3da,
            width: 1,
            port: true,
            version: s.machine.bus.code_check(0x10000, 3).unwrap(),
        };
        s.analyzer
            .record_branch(p, false, now - 20_000_000, 20, Some(0), None);
        let c = &mut s.analyzer.candidates[0];
        c.stats.reference_period_nanos = 10_000_000;
        c.stats.period_nanos = 10_000_000;
        s.analyzer.relearn_until = now + 1_000_000_000;
        s.analyzer.confirmed_wait_nanos = 19_000_000;
        s.analyzer_control();
        assert_eq!(s.hardware_config().cpu_mhz, 20, "only 1 ms of work");
        s.analyzer.confirmed_wait_nanos = 12_000_000;
        s.analyzer_control();
        assert_eq!(
            s.hardware_config().cpu_mhz,
            33,
            "8 ms exceeds reserved budget"
        );
        assert_eq!(s.analyzer.candidates[0].stats.cadence_floor_mhz, 0);
    }
    #[test]
    fn vga_poll_without_taken_iteration_still_finishes_its_period() {
        let mut c = Candidate::default();
        c.stats.port = true;
        c.stats.boundary_accounting = true;
        c.branch_measured(false, 0, 33, Some(0), Some(10));
        c.branch_measured(true, 50, 33, Some(0), None);
        c.branch_measured(false, 100, 33, Some(50), Some(110));
        assert_eq!(c.stats.episodes, 1);
        c.branch_measured(false, 105, 33, Some(50), Some(110));
        assert_eq!(c.stats.episodes, 1, "same-phase catch-up is still work");
        // The next poll immediately exits; there is no waiting branch.
        c.branch_measured(false, 200, 33, Some(50), Some(210));
        assert_eq!(c.stats.episodes, 2);
        assert_eq!(c.stats.period_nanos, 100);
        assert_eq!(c.stats.work_nanos, 100);
        assert_eq!(c.stats.wait_nanos, 0);
        assert_eq!(c.budget(200, 33), None);
        c.branch_measured(true, 250, 33, Some(50), None);
        c.branch_measured(false, 300, 33, Some(100), Some(310));
        assert_eq!(c.stats.episodes, 3);
        assert_eq!(c.stats.period_nanos, 100);
        assert_eq!(c.stats.work_nanos, 50);
        assert_eq!(c.stats.wait_nanos, 50);
        // Changing CPU throughput does not erase the established VGA source.
        c.branch_measured(false, 400, 20, Some(100), Some(410));
        c.branch_measured(false, 500, 20, Some(100), Some(510));
        assert_eq!(c.stats.episodes, 4);
        assert_eq!(c.stats.period_nanos, 100);
        assert_eq!(c.stats.wait_nanos, 0);
    }
    #[test]
    fn abandoned_vga_condition_does_not_merge_presentation_episodes() {
        let mut s = guarded::tests::session();
        for (i, b) in [0xec, 0xa8, 8, 0x74, 0xfb].into_iter().enumerate() {
            s.machine.bus.write_u8(0x12000 + i as u32, b).unwrap();
        }
        s.machine.cpu.registers.ip = 0x12000;
        s.machine.cpu.registers.set(super::super::Gpr16::Dx, 0x3da);
        let p = s.analyzer_predicate().unwrap();
        s.analyzer.record_branch(p, false, 0, 33, Some(0), None);
        s.analyzer.record_branch(p, true, 10, 33, Some(0), None);
        s.analyzer.record_branch(p, false, 100, 33, Some(90), None);
        s.analyzer.record_branch(p, true, 110, 33, Some(90), None);
        assert!(s.analyzer_vga_boundary(1).unwrap().is_some());
        assert!(s.analyzer.boundary_pending.is_some());
        s.machine.cpu.flags |= doslet_x86::flags::INTERRUPT;
        s.machine.bus.pic.raise_edge(0);
        assert!(s.analyzer_guard_irq().unwrap().is_some());
        assert!(s.analyzer.boundary_pending.is_none());
        assert!(s.analyzer.boundary_suspended.is_some());
        s.analyzer_interrupt_vga_boundary();
        s.analyzer.record_branch(p, false, 300, 33, Some(250), None);
        let c = s
            .analyzer
            .candidates
            .iter()
            .find(|c| c.stats.head == p.head)
            .unwrap();
        assert_eq!(c.stats.episodes, 1, "the interrupted episode is incomplete");
        assert_eq!(c.qualified_episodes, 0);
        s.analyzer.record_branch(p, true, 310, 33, Some(250), None);
        s.analyzer.record_branch(p, false, 400, 33, Some(340), None);
        let c = s
            .analyzer
            .candidates
            .iter()
            .find(|c| c.stats.head == p.head)
            .unwrap();
        assert_eq!(c.stats.episodes, 2);
        assert_eq!(c.stats.period_nanos, 100);
    }

    #[test]
    fn paired_vga_boundaries_remain_observed_during_bounded_irq_tracing() {
        let mut s = guarded::tests::session();
        s.set_cpu_mhz(1).unwrap();
        s.machine.cpu.registers.set(super::super::Gpr16::Dx, 0x3da);
        s.machine.cpu.registers.set_byte(4, 8);
        // The timer handler calls both retrace phases. The first finishes
        // inside the IRQ trace; the second outlives its bounded instruction
        // window. Both still delimit actual VGA episodes.
        for (i, b) in [
            0xec, 0x84, 0xc4, 0x75, 0xfb, 0x90, 0xec, 0x84, 0xc4, 0x74, 0xfb, 0xcf,
        ]
        .into_iter()
        .enumerate()
        {
            s.machine.bus.write_u8(0x12000 + i as u32, b).unwrap();
        }
        s.machine.bus.write_u16(0x8040, 0x2000).unwrap();
        s.analyzer.remaining = 0;
        s.analyzer.next = u64::MAX;
        for _ in 0..12 {
            s.machine.bus.vga.set_vbe_101h(false);
            s.machine.bus.vga.advance_pixel_ticks(800 * 490);
            s.machine.cpu.flags |= doslet_x86::flags::INTERRUPT;
            s.machine.bus.pic.raise_edge(0);
            let until = s.emulated_nanos() + 30_000_000;
            while s.emulated_nanos() < until {
                s.advance(1000);
                assert_eq!(s.fault(), None);
            }
            s.machine.bus.pic.write_command(0x20);
        }
        let candidates = s.synchronization_analysis().candidates;
        let first = candidates.iter().find(|c| c.head == 0x12000);
        let second = candidates.iter().find(|c| c.head == 0x12006);
        assert!(first.is_some() && second.is_some(), "{candidates:?}");
        let (first, second) = (first.unwrap(), second.unwrap());
        assert!(first.episodes >= 8, "{candidates:?}");
        assert!(
            first.episodes.abs_diff(second.episodes) <= 1,
            "{candidates:?}"
        );
    }

    #[test]
    fn vga_condition_resumes_after_exact_iret_and_excludes_irq_service_time() {
        let mut s = guarded::tests::session();
        for (i, b) in [0xec, 0xa8, 8, 0x74, 0xfb].into_iter().enumerate() {
            s.machine.bus.write_u8(0x12000 + i as u32, b).unwrap();
        }
        s.machine.cpu.registers.ip = 0x12000;
        s.machine.cpu.registers.set(super::super::Gpr16::Dx, 0x3da);
        let before = s.emulated_nanos();
        s.analyzer_vga_boundary(1).unwrap().unwrap();
        let after_read = s.emulated_nanos();
        s.machine.cpu.flags |= doslet_x86::flags::INTERRUPT;
        s.machine.bus.pic.raise_edge(0);
        s.analyzer_guard_irq().unwrap().unwrap();
        assert!(s.analyzer.boundary_suspended.is_some());
        s.advance(6); // PUSH, load/decrement/store, POP, IRET.
        assert_eq!(s.registers().ip, 0x12001);
        s.analyzer_resume_vga_boundary();
        assert!(s.analyzer.boundary_suspended.is_none());
        let (_, began, _, _) = s.analyzer.boundary_pending.unwrap();
        assert_eq!(s.emulated_nanos() - began, after_read - before);
        assert!(began > before, "the handler duration must be excluded");
    }

    #[test]
    fn vga_condition_survives_an_intermediate_iret_bridge() {
        let mut s = guarded::tests::session();
        for (i, b) in [0xec, 0xa8, 8, 0x74, 0xfb].into_iter().enumerate() {
            s.machine.bus.write_u8(0x12000 + i as u32, b).unwrap();
        }
        // A bridge reuses the interrupt frame and finally returns to the
        // original foreground instruction. Its first IRET is not that return.
        for (i, b) in [0x83, 0xec, 12, 0xc7, 0x04, 0x24, 1, 0x20, 1, 0, 0xcf]
            .into_iter()
            .enumerate()
        {
            s.machine.bus.write_u8(0x13000 + i as u32, b).unwrap();
        }
        s.machine.cpu.registers.ip = 0x12000;
        s.machine.cpu.registers.set(super::super::Gpr16::Dx, 0x3da);
        s.analyzer_vga_boundary(1).unwrap().unwrap();
        s.machine.cpu.flags |= doslet_x86::flags::INTERRUPT;
        s.machine.bus.pic.raise_edge(0);
        s.analyzer_guard_irq().unwrap().unwrap();
        let sp = s.registers().get32(Gpr32::Esp);
        s.machine.bus.write_u32(sp, 0x13000).unwrap();
        s.advance(6);
        assert_eq!(s.registers().ip, 0x13000);
        assert_eq!(s.synchronization_analysis().guard_interrupted_traces, 1);
        assert!(s.analyzer.boundary_suspended.is_some());
        s.advance(3);
        assert_eq!(s.registers().ip, 0x12001);
        s.analyzer_resume_vga_boundary();
        assert!(s.analyzer.boundary_suspended.is_none());
        assert!(s.analyzer.boundary_pending.is_some());
    }

    #[test]
    fn reduced_throughput_cannot_redefine_the_reference_cadence() {
        for slower_period in [10_500_000, 20_000_000] {
            let mut s = super::super::cooperative::tests::session();
            s.set_adaptive_cpu(true);
            let p = Predicate {
                head: 0x10000,
                branch: 0x10003,
                end: 0x10005,
                operand: 0x3da,
                width: 1,
                port: true,
                version: s.machine.bus.code_check(0x10000, 5).unwrap(),
            };
            s.analyzer.record_branch(p, false, 0, 33, Some(0), None);
            for n in 0..16 {
                s.analyzer.record_branch(
                    p,
                    true,
                    n * 10_000_000 + 1_000_000,
                    33,
                    Some(n * 9_000_000),
                    None,
                );
                s.analyzer.record_branch(
                    p,
                    false,
                    (n + 1) * 10_000_000,
                    33,
                    Some((n + 1) * 9_000_000),
                    None,
                );
            }
            assert_eq!(
                s.analyzer.candidates[0].stats.reference_period_nanos,
                10_000_000
            );
            s.machine.set_cpu_mhz(20).unwrap();
            s.analyzer
                .record_branch(p, false, 160_000_000, 20, Some(144_000_000), None);
            // Replace the entire rolling cadence window, including the old
            // faster periods, before checking the sustained 5% slowdown.
            for n in 0..16 {
                s.analyzer.record_branch(
                    p,
                    true,
                    160_000_000 + n * slower_period + 1_000_000,
                    20,
                    Some(144_000_000 + n * (slower_period - 1_000_000)),
                    None,
                );
                s.analyzer.record_branch(
                    p,
                    false,
                    160_000_000 + (n + 1) * slower_period,
                    20,
                    Some(144_000_000 + (n + 1) * (slower_period - 1_000_000)),
                    None,
                );
            }
            let c = &s.analyzer.candidates[0];
            assert_eq!(c.stats.reference_period_nanos, 10_000_000);
            assert_eq!(c.stats.period_nanos, slower_period);
            s.analyzer_control();
            assert_eq!(s.hardware_config().cpu_mhz, 33);
            let c = &mut s.analyzer.candidates[0];
            assert_eq!(c.stats.cadence_floor_mhz, 25);
            assert_eq!(c.stats.missed_deadlines, 1);
            c.relearn("new throughput");
            assert_eq!(c.stats.cadence_floor_mhz, 25);
            assert_eq!(c.stats.reference_period_nanos, 10_000_000);
            // Recovery may reveal that the initial ceiling estimate was too
            // short. Only fresh ceiling-speed evidence may disprove the old
            // failure; a still-excessive period must retain its floor.
            let mut at = 160_000_000 + 16 * slower_period;
            let mut wait = 144_000_000 + 16 * (slower_period - 1_000_000);
            s.analyzer.record_branch(p, false, at, 33, Some(wait), None);
            for _ in 0..16 {
                s.analyzer
                    .record_branch(p, true, at + 1_000_000, 33, Some(wait), None);
                at += 10_250_000;
                wait += 9_250_000;
                s.analyzer.record_branch(p, false, at, 33, Some(wait), None);
            }
            let c = &mut s.analyzer.candidates[0];
            assert_eq!(c.stats.reference_period_nanos, 10_250_000);
            assert_eq!(
                c.stats.cadence_floor_mhz,
                if slower_period == 10_500_000 { 0 } else { 25 }
            );
            c.gap("changed source");
            assert_eq!(c.stats.cadence_floor_mhz, 0);
            assert_eq!(c.stats.reference_period_nanos, 0);
        }
    }

    #[test]
    fn two_phase_vga_poll_tracks_exits_even_when_the_first_phase_never_waits_again() {
        use doslet_bus::PortBus;
        for dynamic in [false, true] {
            let mut s = super::tests::session(&[
                0xec, 0xa8, 8, 0x75, 0xfb, // Wait until retrace is clear.
                0xec, 0xa8, 8, 0x74, 0xfb, // Wait until retrace is set.
                0xb9, 0x40, 0x1f, 0, 0, // 8000 loop iterations of frame work.
                0x49, 0x75, 0xfd, 0xeb, 0xec,
            ]);
            s.set_synchronization_analysis(false);
            s.set_cpu_mhz(1).unwrap();
            s.set_dynamic_core(dynamic);
            s.machine.cpu.registers.set(super::super::Gpr16::Dx, 0x3da);
            s.start();
            // Start at an actually active VGA retrace, so both predicates can
            // establish a waiting branch before later immediate first exits.
            for _ in 0..100_000 {
                if s.machine.bus.read_port_u8(0x3da).unwrap() & 8 != 0 {
                    break;
                }
                s.advance(1);
            }
            s.machine.cpu.registers.ip = 0x10000;
            s.set_synchronization_analysis(true);
            for _ in 0..50 {
                s.advance(10_000);
                s.prepare_dynamic(8);
            }
            let candidates = s.synchronization_analysis().candidates;
            let first = candidates.iter().find(|c| c.head == 0x10000).unwrap();
            let second = candidates.iter().find(|c| c.head == 0x10005).unwrap();
            assert!(first.episodes >= 16, "jit={dynamic}: {candidates:?}");
            assert!(
                first.episodes.abs_diff(second.episodes) <= 1,
                "jit={dynamic}: {candidates:?}"
            );
            assert!(
                first.period_nanos.abs_diff(second.period_nanos) < 2_000_000,
                "jit={dynamic}: {candidates:?}"
            );
        }
    }

    #[test]
    fn slow_presentation_relearning_includes_partial_frame_and_ceiling_jitter() {
        let mut s = super::tests::session(&[0xeb, 0xfe]);
        s.set_cpu_mhz(1).unwrap();
        s.start();
        s.advance(800_000);
        s.set_cpu_mhz(133).unwrap();
        s.set_adaptive_cpu(true);
        let now = s.emulated_nanos();
        let p = Predicate {
            head: 0x10000,
            branch: 0x10000,
            end: 0x10002,
            operand: 0x3da,
            width: 1,
            port: true,
            version: s.machine.bus.code_check(0x10000, 2).unwrap(),
        };
        let period = 166_000_000;
        let start = now - 16 * period - 16_000_000;
        s.analyzer
            .record_branch(p, false, start, 133, Some(0), None);
        let mut at = start;
        for n in 0..16 {
            s.analyzer
                .record_branch(p, true, at + 17_000_000, 133, Some(n * 149_000_000), None);
            at += period + if n == 0 { 16_000_000 } else { 0 };
            s.analyzer
                .record_branch(p, false, at, 133, Some((n + 1) * 149_000_000), None);
        }
        s.analyzer.confirmed_wait_nanos = 16 * 149_000_000;
        s.analyzer_control();
        let mhz = s.hardware_config().cpu_mhz;
        assert!(mhz < 133);
        let until = s.analyzer.relearn_until;
        let c = &mut s.analyzer.candidates[0];
        // First exit after retuning starts a full episode; it cannot complete
        // the partial frame at a different speed. One later delayed frame is
        // already admitted by the observed ceiling-speed cadence.
        at = now + period;
        c.branch_measured(false, at, mhz, Some(0), None);
        for n in 0..8 {
            assert_eq!(c.budget(at, 133), None);
            c.branch_measured(true, at + 20_000_000, mhz, Some(n * 146_000_000), None);
            at += period + if n == 0 { 16_000_000 } else { 0 };
            c.branch_measured(false, at, mhz, Some((n + 1) * 146_000_000), None);
        }
        assert!(c.budget(at, 133).is_some());
        assert!(
            at < until,
            "evidence completes at {at}, but retuning expired at {until}"
        );
    }

    #[test]
    fn measured_vga_episodes_reduce_and_missing_evidence_restores_ceiling() {
        let mut s = super::super::cooperative::tests::session();
        s.set_menu_idle_site(None);
        s.set_idle_loops(false);
        s.set_vga_wait_acceleration(false);
        s.machine.cpu.flags = 2;
        s.machine.cpu.registers.set(super::super::Gpr16::Dx, 0x3da);
        for (i, b) in [0xec, 0xa8, 8, 0x74, 0xfb, 0x40, 0xeb, 0xf8]
            .into_iter()
            .enumerate()
        {
            s.machine.bus.write_u8(0x10000 + i as u32, b).unwrap();
        }
        s.set_adaptive_cpu(true);
        s.start();
        let mut pcm = [[0i16; 2]; 1024];
        let mut lowest = 33;
        let allocated = allocation_counter::measure(|| {
            for _ in 0..250 {
                s.advance(10000);
                while s.drain_pcm(&mut pcm) != 0 {}
                lowest = lowest.min(s.hardware_config().cpu_mhz);
                assert_eq!(s.fault(), None);
            }
        });
        assert_eq!(allocated.count_total, 0);
        assert!(
            s.synchronization_analysis().candidates[0].episodes >= 16,
            "{:?}",
            s.synchronization_analysis()
        );
        assert!(lowest < 33, "{:?}", s.synchronization_analysis());
        // A patch invalidates model/code generations and promptly recovers MHz.
        s.debug_write_memory(0x10000, &[0x40, 0xeb, 0xfd]);
        s.machine.cpu.registers.ip = 0x10000;
        s.advance(10);
        assert_eq!(s.hardware_config().cpu_mhz, 33);
    }
}
