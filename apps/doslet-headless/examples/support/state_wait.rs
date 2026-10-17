//! Offline, fixture-described cooperative path experiment. Not a production detector.
use doslet_runtime::{
    Session,
    session::wait_budget::{WaitBudget, WaitLimits},
};
use doslet_x86::SegmentRegister;
use serde_json::{Value, json};
use std::{error::Error, fs, path::Path};

const MAX_PATH: usize = 16_384;
const SEGMENTS: [SegmentRegister; 6] = [
    SegmentRegister::Cs,
    SegmentRegister::Ds,
    SegmentRegister::Es,
    SegmentRegister::Ss,
    SegmentRegister::Fs,
    SegmentRegister::Gs,
];

#[derive(Clone, PartialEq)]
struct Edge {
    pc: u32,
    next: u32,
    code: [u8; 15],
}

struct Guard {
    address: u32,
    width: usize,
    nonzero: bool,
}

pub struct Experiment {
    head: u32,
    backedge: u32,
    work: Vec<u32>,
    guards: Vec<Guard>,
    tick: u32,
    release: u32,
    completion: u32,
    counter: u32,
    expected_cs: u16,
    context: Option<[doslet_x86::SegmentCache; 6]>,
    stack: Option<u32>,
    template: Vec<Edge>,
    candidate: Vec<Edge>,
    active: Option<u64>,
    index: usize,
    budget: WaitBudget,
    limit: u64,
    pending: bool,
    previous_misses: u64,
    loops: u64,
    matched: u64,
    rejected: u64,
    irq_rejections: u64,
    natural_ns: u64,
    idle_ns: u64,
    idle_calls: u64,
    max_idle_ns: u64,
    deadline_misses: u64,
    completed_work: u64,
    guard_rejections: u64,
}

fn number(config: &Value, key: &str) -> Result<u32, Box<dyn Error>> {
    Ok(u32::try_from(
        config[key]
            .as_u64()
            .ok_or_else(|| format!("missing integer {key}"))?,
    )?)
}

impl Experiment {
    pub fn load(path: &Path, s: &Session) -> Result<Self, Box<dyn Error>> {
        let c: Value = serde_json::from_slice(&fs::read(path)?)?;
        Self::from_config(&c, s)
    }

    fn from_config(c: &Value, s: &Session) -> Result<Self, Box<dyn Error>> {
        let mut guards = Vec::new();
        for g in c["guards"].as_array().ok_or("missing guards")? {
            let width = number(g, "width")? as usize;
            if ![1, 2, 4].contains(&width) {
                return Err("guard width must be 1, 2 or 4".into());
            }
            guards.push(Guard {
                address: number(g, "address")?,
                width,
                nonzero: g["nonzero"].as_bool().ok_or("missing nonzero")?,
            });
        }
        let work = c["work_pcs"]
            .as_array()
            .ok_or("missing work_pcs")?
            .iter()
            .map(|v| Ok(u32::try_from(v.as_u64().ok_or("work PC must be integer")?)?))
            .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
        if guards.is_empty() || guards.len() > 16 || work.is_empty() || work.len() > 64 {
            return Err("experiment requires 1..16 guards and 1..64 work PCs".into());
        }
        let limit = u64::from(number(c, "max_idle_ns")?);
        if limit > 250_000 {
            return Err("max_idle_ns exceeds 250 us".into());
        }
        Ok(Self {
            head: number(c, "head")?,
            backedge: number(c, "backedge")?,
            work,
            guards,
            tick: number(c, "tick_pc")?,
            release: number(c, "release_pc")?,
            completion: number(c, "completion_pc")?,
            counter: number(c, "counter")?,
            expected_cs: u16::try_from(number(c, "cs")?)?,
            context: None,
            stack: None,
            template: Vec::with_capacity(MAX_PATH),
            candidate: Vec::with_capacity(MAX_PATH),
            active: None,
            index: 0,
            budget: WaitBudget::new(limit),
            limit,
            pending: false,
            previous_misses: s.audio_status().dsp.block_irq_deadline_misses,
            loops: 0,
            matched: 0,
            rejected: 0,
            irq_rejections: 0,
            natural_ns: 0,
            idle_ns: 0,
            idle_calls: 0,
            max_idle_ns: 0,
            deadline_misses: 0,
            completed_work: 0,
            guard_rejections: 0,
        })
    }

    fn value(s: &Session, address: u32, width: usize) -> Option<u32> {
        let mut bytes = [0; 4];
        s.debug_read_memory(address, &mut bytes[..width])
            .then(|| u32::from_le_bytes(bytes))
    }

    fn guards_hold(&self, s: &Session) -> bool {
        let r = s.registers();
        r.cs == self.expected_cs
            && self
                .context
                .is_none_or(|old| old == SEGMENTS.map(|seg| r.segment_cache(seg)))
            && self.guards.iter().all(|g| {
                Self::value(s, g.address, g.width).is_some_and(|value| (value != 0) == g.nonzero)
            })
    }

    fn reject(&mut self) {
        self.rejected += u64::from(self.active.take().is_some());
        self.candidate.clear();
        self.budget.revoke();
    }

    pub fn before(&mut self, s: &Session, pc: u32, sp: u32) {
        if pc != self.head {
            return;
        }
        if self.active.is_some() {
            self.reject();
        }
        if s.registers().cs != self.expected_cs
            || self.stack.is_some_and(|old| old != sp)
            || !self.guards_hold(s)
        {
            self.guard_rejections += 1;
            self.budget.revoke();
            return;
        }
        self.stack = Some(sp);
        self.context = Some(SEGMENTS.map(|seg| s.registers().segment_cache(seg)));
        self.active = Some(s.emulated_nanos());
        self.index = 0;
    }

    /// Called after each real step, never after inserted hardware-only time.
    pub fn after(&mut self, s: &Session, pc: u32, code: [u8; 15], irq: bool) -> u64 {
        let now = s.emulated_nanos();
        let misses = s.audio_status().dsp.block_irq_deadline_misses;
        if misses != self.previous_misses {
            self.budget.miss(now);
            self.previous_misses = misses;
        }
        if irq {
            self.irq_rejections += u64::from(self.active.is_some());
            self.reject();
            return 0;
        }
        if pc == self.tick && self.pending {
            self.deadline_misses += 1;
            self.budget.miss(now);
        }
        if pc == self.release
            && s.registers().get32(doslet_x86::Gpr32::Eax) == 0
            && Self::value(s, self.counter, 4) == Some(0)
        {
            self.pending = true;
        }
        if pc == self.completion {
            self.pending = false;
            self.completed_work += 1;
        }
        if self.work.contains(&pc) {
            self.reject();
        }
        let Some(start) = self.active else {
            return 0;
        };
        if now.saturating_sub(start) > 1_000_000 || self.index == MAX_PATH {
            self.reject();
            return 0;
        }
        let edge = Edge {
            pc,
            next: s.debug_pc(),
            code,
        };
        if self.template.is_empty() {
            self.candidate.push(edge);
        } else if self.template.get(self.index) != Some(&edge) {
            self.reject();
            return 0;
        }
        self.index += 1;
        if pc != self.backedge || s.debug_pc() != self.head {
            return 0;
        }
        self.loops += 1;
        if !self.guards_hold(s)
            || self.pending
            || self.stack != Some(s.registers().get32(doslet_x86::Gpr32::Esp))
        {
            self.guard_rejections += 1;
            self.reject();
            return 0;
        }
        if self.template.is_empty() {
            core::mem::swap(&mut self.template, &mut self.candidate);
        } else if self.index != self.template.len() {
            self.reject();
            return 0;
        }
        self.active = None;
        self.matched += 1;
        let natural = now - start;
        self.natural_ns += natural;
        // Fixture contract: guarded work is released by IRQ callbacks. Stop
        // before their earliest service horizon, without predicting timer ticks
        // from the guest's potentially slowed software countdown.
        let horizon = s.debug_service_horizon();
        self.budget.completed_empty(
            now,
            natural,
            WaitLimits {
                service_horizon: horizon,
                work_slack: horizon,
                input_latency: 100_000,
                reserve: 10_000,
            },
        )
    }

    pub fn waited(&mut self, actual: u64) {
        if actual > 0 {
            self.idle_ns += actual;
            self.idle_calls += 1;
            self.max_idle_ns = self.max_idle_ns.max(actual);
        }
    }

    pub fn summary(&self) -> Value {
        json!({"experimental":true,"max_idle_ns":self.limit,"template_steps":self.template.len(),
            "loops":self.loops,"matched":self.matched,"rejected":self.rejected,
            "irq_rejections":self.irq_rejections,"guard_rejections":self.guard_rejections,
            "natural_wait_ns":self.natural_ns,"idle_ns":self.idle_ns,"idle_calls":self.idle_calls,
            "max_actual_idle_ns":self.max_idle_ns,"deadline_misses":self.deadline_misses,
            "completed_work":self.completed_work})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Session, Experiment) {
        let mut s = Session::new(&[0x90, 0xEB, 0xFD]).unwrap();
        s.debug_disable_accelerations();
        s.debug_write_memory(0x9000, &[1, 0, 0, 0]);
        s.start();
        let config = json!({"head":0x10100,"backedge":0x10101,"cs":0x1000,
            "work_pcs":[0x10400],"guards":[{"address":0x9000,"width":4,"nonzero":true}],
            "tick_pc":0x10410,"release_pc":0x10420,"completion_pc":0x10430,
            "counter":0x9000,"max_idle_ns":25000});
        let e = Experiment::from_config(&config, &s).unwrap();
        (s, e)
    }
    fn step(s: &mut Session, e: &mut Experiment) {
        let pc = s.debug_pc();
        let mut code = [0; 15];
        assert!(s.debug_read_memory(pc, &mut code));
        e.before(s, pc, s.registers().get32(doslet_x86::Gpr32::Esp));
        assert_eq!(s.advance(1), 1);
        e.after(s, pc, code, false);
    }
    #[test]
    fn changed_code_or_guard_never_reuses_the_learned_path() {
        let (mut s, mut e) = fixture();
        for _ in 0..20 {
            step(&mut s, &mut e);
        }
        assert_eq!(e.matched, 10);
        s.debug_write_memory(0x9000, &[0]);
        for _ in 0..4 {
            step(&mut s, &mut e);
        }
        assert_eq!(e.matched, 10);
        s.debug_write_memory(0x9000, &[1]);
        s.debug_write_memory(0x10100, &[0x40]); // INC AX, same successor, changed work.
        for _ in 0..20 {
            step(&mut s, &mut e);
        }
        assert_eq!(e.matched, 10);
        assert!(e.rejected >= 10);
    }
    #[test]
    fn irq_and_work_cancel_an_incomplete_iteration() {
        let (mut s, mut e) = fixture();
        step(&mut s, &mut e);
        e.after(&s, 0x10100, [0; 15], true);
        assert!(e.active.is_none());
        assert_eq!(e.irq_rejections, 1);
        step(&mut s, &mut e);
        step(&mut s, &mut e);
        assert!(e.active.is_some());
        e.after(&s, 0x10400, [0; 15], false);
        assert!(e.active.is_none());
        assert!(e.template.is_empty());
    }
}
