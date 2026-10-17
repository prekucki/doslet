//! Paired diagnostic replays on one independent owner. Never instruments live execution.
use crate::speed_tiers::{Node, Profile, Tier};
use doslet_runtime::{Session, SessionStatus, session::WorkingState};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

type Key = (u32, bool);
const WINDOW: Duration = Duration::from_secs(1);
const MAX_NODES: usize = 100_000;
const MAX_JOB: Duration = Duration::from_secs(120);
const MAX_PENDING: usize = 4;

struct Pending {
    snapshot: WorkingState,
    epoch: u64,
    pc: u32,
    nanos: u64,
}

pub(super) struct ResultSet {
    epoch: u64,
    pub nodes: BTreeMap<Key, Node>,
    pub seconds: f64,
    #[cfg(test)]
    direct_calls: [u64; 2],
}
struct Job {
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<ResultSet, String>>>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

pub(super) struct Learner {
    job: Option<Job>,
    pending: VecDeque<Pending>,
    epoch: u64,
    previous: [u64; 4],
    sampled: Instant,
    next: Instant,
    backoff: Duration,
    pub jobs: u64,
    pub changes: u64,
    pub grey_percent: f64,
    pub capture_micros: u64,
}
impl Default for Learner {
    fn default() -> Self {
        Self {
            job: None,
            pending: VecDeque::new(),
            epoch: 0,
            previous: [0; 4],
            sampled: Instant::now(),
            next: Instant::now(),
            backoff: WINDOW,
            jobs: 0,
            changes: 0,
            grey_percent: 0.0,
            capture_micros: 0,
        }
    }
}
impl Learner {
    pub fn running(&self) -> bool {
        self.job.is_some()
    }
    pub fn pending(&self) -> usize {
        self.pending.len()
    }
    pub fn cancel(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Relaxed);
        }
        self.epoch = 0;
        self.pending.clear();
    }
    pub fn maintain(
        &mut self,
        session: &mut Session,
        profile: &mut Profile,
        grey_mhz: u16,
    ) -> bool {
        let new_epoch = self.epoch != session.epoch();
        let startup = new_epoch && session.steps() == 0;
        if new_epoch {
            self.cancel();
            self.epoch = session.epoch();
            self.previous = session.executed_tier_counts();
            self.sampled = Instant::now();
            self.backoff = WINDOW;
            self.next = Instant::now();
        }
        if self
            .job
            .as_ref()
            .is_some_and(|j| j.thread.as_ref().unwrap().is_finished())
        {
            let mut job = self.job.take().unwrap();
            let cancelled = job.cancel.load(Ordering::Relaxed);
            let handle = job.thread.take().unwrap();
            match handle
                .join()
                .unwrap_or_else(|_| Err("profiling worker panicked".into()))
            {
                Ok(result) if !cancelled && result.epoch == session.epoch() => {
                    let changed = apply(profile, session, result.nodes);
                    self.changes += changed as u64;
                    eprintln!(
                        "[INFO doslet] tier learning: {} new HOT/COLD/IRQ labels in {:.2}s host time",
                        changed, result.seconds
                    );
                    self.backoff = if changed == 0 {
                        (self.backoff * 2).min(Duration::from_secs(60))
                    } else {
                        WINDOW
                    };
                    self.next = Instant::now() + self.backoff;
                    self.previous = session.executed_tier_counts();
                    self.sampled = Instant::now();
                    return changed != 0;
                }
                Ok(_) => {}
                Err(_) if cancelled => {}
                Err(error) => {
                    eprintln!("[WARN doslet] tier learning discarded: {error}");
                    self.next = Instant::now() + Duration::from_secs(30);
                }
            }
        }
        self.launch_pending(grey_mhz);
        let now = Instant::now();
        if !startup && now.duration_since(self.sampled) < WINDOW {
            return false;
        }
        let counts = session.executed_tier_counts();
        let delta: [u64; 4] = std::array::from_fn(|i| counts[i].saturating_sub(self.previous[i]));
        self.previous = counts;
        self.sampled = now;
        let total: u64 = delta.iter().sum();
        self.grey_percent = 100.0 * delta[2] as f64 / total.max(1) as f64;
        if self.pending.len() >= MAX_PENDING
            || (!startup && (now < self.next || total < 10_000 || delta[2] <= total / 10))
            || session.status() != SessionStatus::Running
        {
            return false;
        }
        let start = Instant::now();
        let snapshot = match session.capture_working_state() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                eprintln!("[WARN doslet] tier learning capture deferred: {error}");
                self.next = now + Duration::from_secs(30);
                return false;
            }
        };
        self.capture_micros = start.elapsed().as_micros() as u64;
        self.pending.push_back(Pending {
            snapshot,
            epoch: session.epoch(),
            pc: session.debug_pc(),
            nanos: session.emulated_nanos(),
        });
        self.launch_pending(grey_mhz);
        false
    }
    fn launch_pending(&mut self, grey_mhz: u16) {
        if self.job.is_some() {
            return;
        }
        let Some(Pending {
            snapshot,
            epoch,
            pc,
            nanos,
        }) = self.pending.pop_front()
        else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let low = grey_mhz.min(333);
        match thread::Builder::new()
            .name("doslet-tier-profile".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(move || profile_pair(snapshot, epoch, low, low * 3, 1_000_000_000, &token))
        {
            Ok(thread) => {
                self.jobs += 1;
                self.job = Some(Job {
                    cancel,
                    thread: Some(thread),
                });
                eprintln!(
                    "[INFO doslet] tier learning #{}: GREY {:.1}%, capture {} us, paired 1s at {}/{} MHz; snapshot {} ns pc {:x}; {} pending",
                    self.jobs,
                    self.grey_percent,
                    self.capture_micros,
                    low,
                    low * 3,
                    nanos,
                    pc,
                    self.pending.len()
                );
            }
            Err(error) => {
                eprintln!("[WARN doslet] cannot start tier learning: {error}");
                self.next = Instant::now() + Duration::from_secs(30);
            }
        }
    }
}

#[derive(Default)]
struct TraceNode {
    code: [u8; 15],
    count: u64,
    changed: bool,
    irq: bool,
}
#[derive(Default)]
struct Trace {
    nodes: HashMap<Key, TraceNode>,
    #[cfg(test)]
    direct_calls: u64,
}
impl Trace {
    fn irq_nodes(&self) -> BTreeSet<Key> {
        self.nodes
            .iter()
            .filter_map(|(&key, n)| n.irq.then_some(key))
            .collect()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Boundary {
    cs: u16,
    ip: u32,
    ss: u16,
    sp: u32,
}
impl Boundary {
    fn at(s: &Session) -> Self {
        let r = s.registers();
        Self {
            cs: r.cs,
            ip: r.ip,
            ss: r.ss,
            sp: r.get32(doslet_x86::Gpr32::Esp),
        }
    }
}
#[derive(Default)]
struct Interrupts {
    frames: Vec<(Boundary, bool)>,
    hardware_depth: usize,
    ready: bool,
}
impl Interrupts {
    fn enter(&mut self, boundary: Boundary, hardware: bool) -> Result<(), String> {
        if self.frames.len() >= 64 {
            return Err("interrupt nesting limit".into());
        }
        self.frames.push((boundary, hardware));
        self.hardware_depth += usize::from(hardware);
        Ok(())
    }
    fn returned(&mut self, boundary: Boundary) -> bool {
        let Some(index) = self.frames.iter().rposition(|(ret, _)| *ret == boundary) else {
            return false;
        };
        let mut hardware = false;
        for (_, hw) in self.frames.drain(index..) {
            self.hardware_depth -= usize::from(hw);
            hardware |= hw;
        }
        self.ready |= hardware && self.hardware_depth == 0;
        true
    }
}
fn software_interrupt_length(code: &[u8; 15]) -> Option<u32> {
    let index = code.iter().position(|b| {
        !matches!(
            b,
            0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67 | 0xf0 | 0xf2 | 0xf3
        )
    })?;
    match code[index] {
        0xcd => Some(index as u32 + 2),
        0xcc | 0xce => Some(index as u32 + 1),
        _ => None,
    }
}
fn trace(
    snapshot: &WorkingState,
    mhz: u16,
    nanos: u64,
    cancel: &AtomicBool,
    start: Instant,
) -> Result<Trace, String> {
    let mut s = snapshot.restore().map_err(|e| e.to_string())?;
    // WorkingState already drops native code and analyzer/controller proofs.
    // Keep the captured DPMI dispatch mode so learning follows live execution.
    s.set_idle_loops(false);
    s.set_vga_wait_acceleration(false);
    s.set_cpu_mhz(mhz).map_err(|e| e.to_string())?;
    s.start();
    #[cfg(test)]
    let direct_before = s.dpmi_stats().map_or(0, |stats| stats.direct);
    let deadline = s.emulated_nanos().saturating_add(nanos);
    let misses = s.audio_status().dsp.block_irq_deadline_misses;
    let mut trace = Trace::default();
    let mut pcm = [doslet_pcm::EMPTY; 1024];
    let mut midi = [0; 1024];
    let mut since_yield = Instant::now();
    let mut stalled = 0;
    let mut interrupts = Interrupts {
        // A fresh executable has executed no interrupt entry. Unlike arbitrary
        // scene snapshots, its initial prefix is known to be ordinary work.
        ready: s.steps() == 0,
        ..Default::default()
    };
    while s.emulated_nanos() < deadline {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        if start.elapsed() > MAX_JOB {
            return Err("120s host limit".into());
        }
        if s.control_registers().cr0 & (1 << 31) != 0 {
            return Err("profiling requires paging disabled".into());
        }
        for _ in 0..4096 {
            if s.emulated_nanos() >= deadline {
                break;
            }
            let key = (s.debug_pc(), s.registers().code32());
            if !s.a20_enabled() && key.0.saturating_add(15) > 0x100000 {
                return Err("A20 code alias".into());
            }
            let mut code = [0; 15];
            if !s.debug_read_memory(key.0, &mut code) {
                return Err("unreadable code".into());
            }
            let irq = s.debug_irq_entries();
            let boundary = Boundary::at(&s);
            let in_irq = interrupts.hardware_depth != 0;
            let record = interrupts.ready || in_irq;
            let used = s.advance(1);
            if s.status() != SessionStatus::Running {
                return Err(format!("replay stopped: {:?}", s.status()));
            }
            if used == 0 {
                stalled += 1;
                if stalled > 1000 {
                    return Err("replay stalled".into());
                }
                continue;
            }
            stalled = 0;
            if irq != s.debug_irq_entries() {
                interrupts.enter(boundary, true)?;
                continue;
            }
            // DPMI may cross IRET/RETF trampolines before reaching the interrupted
            // caller. Only the exact saved context closes a diagnostic frame.
            if !interrupts.frames.is_empty() {
                interrupts.returned(Boundary::at(&s));
            }
            if let Some(length) = software_interrupt_length(&code) {
                let after = Boundary::at(&s);
                let mut ret = boundary;
                ret.ip = ret.ip.wrapping_add(length);
                if !key.1 {
                    ret.ip &= 0xffff;
                }
                // Native DOS/BIOS interception does not install a guest interrupt frame.
                if after != ret && after.sp != boundary.sp {
                    interrupts.enter(ret, false)?;
                }
            }
            // A snapshot may begin within an IRQ. Do not classify its unobserved
            // prefix; wait until an observed IRQ has returned to its exact caller.
            if !record {
                continue;
            }
            if trace.nodes.len() >= MAX_NODES && !trace.nodes.contains_key(&key) {
                return Err("100000 node limit".into());
            }
            let node = trace.nodes.entry(key).or_default();
            if node.count == 0 {
                node.code = code;
            }
            node.changed |= node.code != code;
            node.count += 1;
            node.irq |= in_irq;
        }
        while s.drain_pcm_timed(&mut pcm) != 0 {}
        while s.drain_midi(&mut midi) != 0 {}
        // Keep the background replay below one saturated core; never delay live time.
        if since_yield.elapsed() >= Duration::from_millis(2) {
            thread::sleep(Duration::from_millis(2));
            since_yield = Instant::now();
        }
    }
    if s.audio_status().dsp.block_irq_deadline_misses > misses {
        return Err("replay missed an SB16 service deadline".into());
    }
    #[cfg(test)]
    {
        trace.direct_calls = s.dpmi_stats().map_or(0, |stats| stats.direct) - direct_before;
    }
    Ok(trace)
}
fn profile_pair(
    snapshot: WorkingState,
    epoch: u64,
    low: u16,
    high: u16,
    nanos: u64,
    cancel: &AtomicBool,
) -> Result<ResultSet, String> {
    let start = Instant::now();
    let a = trace(&snapshot, low, nanos, cancel, start)?;
    let b = trace(&snapshot, high, nanos, cancel, start)?;
    Ok(ResultSet {
        #[cfg(test)]
        direct_calls: [a.direct_calls, b.direct_calls],
        epoch,
        nodes: merge_traces(a, b),
        seconds: start.elapsed().as_secs_f64(),
    })
}
fn merge_traces(a: Trace, b: Trace) -> BTreeMap<Key, Node> {
    let mut irq = a.irq_nodes();
    irq.extend(b.irq_nodes());
    let mut nodes = BTreeMap::new();
    for (&key, second) in &b.nodes {
        if second.changed
            || a.nodes
                .get(&key)
                .is_some_and(|n| n.changed || n.code != second.code)
        {
            continue;
        }
        let interrupt = irq.contains(&key);
        if interrupt {
            nodes.insert(
                key,
                Node {
                    code: second.code,
                    tier: Tier::Irq,
                    locked: true,
                },
            );
            continue;
        }
        let Some(first) = a.nodes.get(&key) else {
            continue;
        };
        if first.changed || second.changed || first.code != second.code {
            continue;
        }
        let first = first.count;
        let second_count = second.count;
        let tier = classify(first, second_count);
        nodes.insert(
            key,
            Node {
                code: second.code,
                tier,
                locked: false,
            },
        );
    }
    // IRQ paths present only in the low-throughput replay also get IRQ labels.
    for key in irq {
        if let Some(node) = a.nodes.get(&key)
            && !node.changed
            && b.nodes
                .get(&key)
                .is_none_or(|n| !n.changed && n.code == node.code)
        {
            nodes.insert(
                key,
                Node {
                    code: node.code,
                    tier: Tier::Irq,
                    locked: true,
                },
            );
        }
    }
    nodes
}
fn classify(low: u64, high: u64) -> Tier {
    if low.min(high) < 32 {
        Tier::Grey
    } else if high * 2 >= low * 3 {
        Tier::Slow
    } else if high * 5 >= low * 4 && high * 5 <= low * 6 {
        Tier::Fast
    } else {
        Tier::Grey
    }
}
fn apply(profile: &mut Profile, _session: &Session, nodes: BTreeMap<Key, Node>) -> usize {
    let mut changed = 0;
    for (key, node) in nodes {
        // The same executable epoch may already have unloaded its startup code.
        // Keep stable replay evidence for the next launch; CPU decode guards
        // enforce the recorded bytes before any installed label takes effect.
        if let Some(previous) = profile.nodes.get(&key) {
            // Learning is monotonic: only previously GREY sites can gain colour.
            // A short new window never revokes an established HOT/COLD label.
            if previous.tier != Tier::Grey || previous.locked {
                continue;
            }
            if previous.tier == node.tier
                && previous.code == node.code
                && previous.locked == node.locked
            {
                continue;
            }
        } else if node.tier == Tier::Grey && !node.locked || profile.nodes.len() >= MAX_NODES {
            continue;
        }
        // A GREY bookkeeping entry changes no execution label: unknown is
        // already GREY. Do not flush CPU caches or report it as learned work.
        let coloured = node.tier != Tier::Grey;
        profile.nodes.insert(key, node);
        changed += usize::from(coloured);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[ignore = "local startup state and saved profile; paired diagnostic traces"]
    fn local_startup_learning_diagnostics() {
        use std::io::Read;
        let state = std::env::var("DOSLET_LEARNING_STATE").unwrap();
        let profile = std::env::var("DOSLET_LEARNING_PROFILE").unwrap();
        let mut live = if state == "boot" {
            let settings = crate::native_input::LaunchSettings {
                no_dynamic_core: true,
                preload_local_media: true,
                hardware: doslet_config::HardwareConfig {
                    cpu_mhz: 133,
                    mpu401: false,
                    ..Default::default()
                },
                cdrom: Some((b'D', "../../games/HoMnM/homm1.gog".into())),
                ..Default::default()
            };
            crate::native_input::load_game_with_settings(
                std::path::Path::new("../../games/HoMnM/HEROES.EXE"),
                &settings,
            )
            .unwrap()
        } else {
            let mut bytes = Vec::new();
            flate2::read::GzDecoder::new(std::fs::File::open(state).unwrap())
                .read_to_end(&mut bytes)
                .unwrap();
            Session::load_state(&bytes).unwrap()
        };
        if let Ok(ms) = std::env::var("DOSLET_LEARNING_AT_MS") {
            let mut c = crate::native_speed_tiers::Controller::load(
                std::path::Path::new(&profile),
                8,
                Some(500),
                100,
            )
            .unwrap();
            c.enable(&mut live);
            live.start();
            let end = ms.parse::<u64>().unwrap() * 1_000_000;
            let mut pcm = [doslet_pcm::EMPTY; 1024];
            let mut midi = [0; 1024];
            while live.emulated_nanos() < end {
                c.advance(&mut live, 10000, end).unwrap();
                while live.drain_pcm_timed(&mut pcm) != 0 {}
                while live.drain_midi(&mut midi) != 0 {}
            }
        }
        eprintln!(
            "CAPTURE guest_ns={} pc={:x}",
            live.emulated_nanos(),
            live.debug_pc()
        );
        let profile = Profile::load_labels(std::path::Path::new(&profile)).unwrap();
        let snapshot = live.capture_working_state().unwrap();
        if let Ok(path) = std::env::var("DOSLET_LEARNING_PROFILE") {
            let mut probe = snapshot.restore().unwrap();
            let mut c = crate::native_speed_tiers::Controller::load(
                std::path::Path::new(&path),
                8,
                Some(500),
                100,
            )
            .unwrap();
            c.enable(&mut probe);
            probe.start();
            let mut grey = BTreeMap::<Key, (u64, [u8; 15])>::new();
            for _ in 0..100_000 {
                let key = (probe.debug_pc(), probe.registers().code32());
                let before = probe.executed_tier_counts()[2];
                let mut code = [0; 15];
                probe.debug_read_memory(key.0, &mut code);
                probe.advance(1);
                if probe.executed_tier_counts()[2] > before {
                    let n = grey.entry(key).or_insert((0, code));
                    n.0 += 1;
                }
            }
            let mut grey: Vec<_> = grey.into_iter().collect();
            grey.sort_by_key(|(_, n)| std::cmp::Reverse(n.0));
            for (key, (n, code)) in grey.iter().take(15) {
                let old = profile.nodes.get(key);
                eprintln!(
                    "GREY pc={:x} wide={} n={n} stored={:?} matches={} bytes={code:?}",
                    key.0,
                    key.1,
                    old.map(|n| n.tier.name()),
                    old.is_some_and(|n| n.code == *code)
                );
            }
        }
        let start = Instant::now();
        let cancel = AtomicBool::new(false);
        let a = trace(&snapshot, 100, 1_000_000_000, &cancel, start).unwrap();
        let b = trace(&snapshot, 300, 1_000_000_000, &cancel, start).unwrap();
        let mut rows: Vec<_> = b.nodes.iter().map(|(key,n)| {
            let old = profile.nodes.get(key);
            let first = a.nodes.get(key);
            let matches = old.is_some_and(|v| v.code == n.code);
            serde_json::json!({"pc":key.0,"wide":key.1,"low":first.map(|n|n.count),"high":n.count,
                "tier":first.map(|f|classify(f.count,n.count).name()),"irq":n.irq,
                "changed":n.changed || first.is_some_and(|n|n.changed),
                "same_pair":first.is_some_and(|f|f.code==n.code),
                "stored":old.map(|n|n.tier.name()),"locked":old.is_some_and(|n|n.locked),
                "matches":matches,"bytes":n.code})
        }).collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r["high"].as_u64().unwrap()));
        let unknown: Vec<_> = rows
            .iter()
            .filter(|r| r["stored"] == "grey" || r["matches"] == false)
            .collect();
        eprintln!(
            "STARTUP nodes={} unknown={} host={:.2}s",
            rows.len(),
            unknown.len(),
            start.elapsed().as_secs_f64()
        );
        for row in unknown.iter().take(25) {
            eprintln!("{row}");
        }
        std::fs::write(
            "../../target/startup-learning-diagnostics.json",
            serde_json::to_vec_pretty(&rows).unwrap(),
        )
        .unwrap();
        if let Ok(output) = std::env::var("DOSLET_LEARNING_OUTPUT") {
            let mut profile = profile;
            let changed = apply(&mut profile, &live, merge_traces(a, b));
            let nodes:Vec<_>=profile.nodes.iter().map(|(&(pc,wide),n)|serde_json::json!({
                "pc":pc,"bits":if wide {32}else{16},"bytes":n.code,"tier":n.tier.name(),
                "reason":if n.tier==Tier::Irq {"interrupt-frame"}else if n.locked {"code-conflict"}else{"learned"}
            })).collect();
            std::fs::write(
                output,
                serde_json::to_vec(
                    &serde_json::json!({"nodes":nodes,"unknown":"grey","irq_policy":"static-grey"}),
                )
                .unwrap(),
            )
            .unwrap();
            eprintln!("LEARNED {changed} labels");
        }
    }
    #[test]
    fn fresh_entry_is_traced_before_the_first_hardware_interrupt() {
        let s = Session::new(&[0x90, 0xeb, 0xfd]).unwrap();
        let t = trace(
            &s.capture_working_state().unwrap(),
            1,
            1_000_000,
            &AtomicBool::new(false),
            Instant::now(),
        )
        .unwrap();
        assert!(t.nodes[&(s.debug_pc(), false)].count > 32);
        assert!(t.nodes.values().all(|n| !n.irq));
    }

    #[test]
    fn paired_learning_enters_dpmi_without_mutating_the_live_host() {
        use doslet_runtime::session::dpmi::Mode;
        let program = [
            0xb8, 0x87, 0x16, // MOV AX,1687h
            0xcd, 0x2f, // Discover the DPMI entry point in ES:DI.
            0x89, 0x3e, 0x00, 0x02, // MOV [0200h],DI
            0x8c, 0x06, 0x02, 0x02, // MOV [0202h],ES
            0xb8, 0x00, 0x80, 0x8e, 0xc0, // Private real-mode stack at ES=8000h.
            0xb8, 0x01, 0x00, // Select a 32-bit client.
            0xff, 0x1e, 0x00, 0x02, // CALL FAR [0200h]
            0x8c, 0xd8, 0x8e, 0xc0, // ES=DS: structure at selector-relative 0300h.
            0xb8, 0x00, 0x03, // AX=0300h: simulate a real-mode interrupt.
            0xbb, 0x16, 0x00, // BX=16h: nonblocking keyboard query.
            0x31, 0xc9, // No stack arguments.
            0x66, 0xbf, 0x00, 0x03, 0x00, 0x00, // EDI=0300h.
            0xcd, 0x31, 0xeb, 0xee, // Repeat INT 31h in protected 16-bit code.
        ];
        for mode in [Mode::Direct, Mode::Reflect] {
            let mut live = Session::new(&program).unwrap();
            live.enable_dpmi(mode).unwrap();
            assert!(live.debug_write_memory(0x1031c, &[0x00, 0x01]));
            assert!(live.debug_write_memory(0x10320, &[0x02, 0x02]));
            let snapshot = live.capture_working_state().unwrap();
            let mut probe = snapshot.restore().unwrap();
            probe.start();
            assert_eq!(probe.advance(64), 64);
            assert_eq!(probe.status(), SessionStatus::Running);
            assert!(probe.dpmi_stats().unwrap().entered);
            assert_ne!(probe.control_registers().cr0 & 1, 0);

            let result = profile_pair(
                snapshot,
                live.epoch(),
                1,
                3,
                4_000_000,
                &AtomicBool::new(false),
            )
            .unwrap();
            let loop_pc = live.debug_pc() + program.len() as u32 - 18;
            assert_eq!(result.epoch, live.epoch());
            assert!(result.nodes[&(loop_pc, false)].tier == Tier::Slow);
            match mode {
                Mode::Direct => assert!(result.direct_calls.iter().all(|n| *n > 32)),
                Mode::Reflect => assert_eq!(result.direct_calls, [0; 2]),
            }
            // Installing learned labels must retain the selected service path.
            let labels = result
                .nodes
                .iter()
                .map(|(&(address, code32), node)| doslet_x86::ExecutionLabel {
                    address,
                    code32,
                    bytes: node.code,
                    tier: match node.tier {
                        Tier::Fast => 0,
                        Tier::Slow => 1,
                        Tier::Grey => 2,
                        Tier::Irq => 3,
                    },
                })
                .collect();
            let mut labelled = live.capture_working_state().unwrap().restore().unwrap();
            labelled.set_idle_loops(false);
            labelled.set_vga_wait_acceleration(false);
            labelled
                .set_execution_tiers(Some(labels), [None, Some(8), Some(133)])
                .unwrap();
            labelled.start();
            assert_eq!(labelled.advance(1000), 1000);
            assert_eq!(labelled.status(), SessionStatus::Running);
            assert_eq!(labelled.executed_tier_counts().iter().sum::<u64>(), 1000);
            match mode {
                Mode::Direct => assert!(labelled.dpmi_stats().unwrap().direct > 32),
                Mode::Reflect => assert_eq!(labelled.dpmi_stats().unwrap().direct, 0),
            }
            assert_eq!(live.steps(), 0);
            assert_eq!(live.status(), SessionStatus::Ready);
            assert!(!live.dpmi_stats().unwrap().entered);
        }
    }

    #[test]
    #[ignore = "requires DOSLET_DPMI_GAME and optional DOSLET_DPMI_CD local game assets"]
    fn local_dpmi_game_learning() {
        let executable = std::env::var("DOSLET_DPMI_GAME").unwrap();
        let settings = crate::native_input::LaunchSettings {
            no_dynamic_core: true,
            preload_local_media: true,
            dpmi: Some(doslet_runtime::session::dpmi::Mode::Direct),
            cdrom: std::env::var_os("DOSLET_DPMI_CD").map(|path| (b'D', path.into())),
            ..Default::default()
        };
        let mut live = crate::native_input::load_game_with_settings(
            std::path::Path::new(&executable),
            &settings,
        )
        .unwrap();
        let startup = live.capture_working_state().unwrap();
        live.start();
        let mut pcm = [doslet_pcm::EMPTY; 1024];
        let mut midi = [0; 1024];
        for _ in 0..1000 {
            live.advance(10_000);
            assert_eq!(live.status(), SessionStatus::Running, "{:?}", live.fault());
            while live.drain_pcm_timed(&mut pcm) != 0 {}
            while live.drain_midi(&mut midi) != 0 {}
            if live.dpmi_stats().unwrap().entered {
                break;
            }
        }
        assert!(live.dpmi_stats().unwrap().entered);
        let stats = live.dpmi_stats();
        let steps = live.steps();
        for (stage, snapshot) in [
            ("startup", startup),
            ("active", live.capture_working_state().unwrap()),
        ] {
            let result = profile_pair(
                snapshot,
                live.epoch(),
                10,
                30,
                100_000_000,
                &AtomicBool::new(false),
            )
            .unwrap();
            let coloured = result
                .nodes
                .values()
                .filter(|n| n.tier != Tier::Grey)
                .count();
            assert!(coloured > 0, "{stage}: no learned labels");
            eprintln!(
                "DPMI {stage}: {} nodes, {coloured} coloured, direct calls {:?}, {:.2}s host time",
                result.nodes.len(),
                result.direct_calls,
                result.seconds
            );
        }
        assert_eq!(live.steps(), steps);
        assert_eq!(live.dpmi_stats(), stats);
    }
    #[test]
    fn transient_snapshot_queue_is_bounded_and_cancelled_with_epoch() {
        let mut s = Session::new(&[0x90, 0xeb, 0xfd]).unwrap();
        s.debug_disable_accelerations();
        s.set_execution_tiers(Some(vec![]), [Some(500), Some(8), Some(100)])
            .unwrap();
        s.start();
        s.advance(20000);
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let handle = thread::spawn(move || {
            while !token.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(1));
            }
            Err("cancelled".into())
        });
        let mut learner = Learner {
            epoch: s.epoch(),
            job: Some(Job {
                cancel: cancel.clone(),
                thread: Some(handle),
            }),
            ..Default::default()
        };
        let mut p = Profile::from_json(
            &serde_json::json!({"nodes":[],"unknown":"grey","irq_policy":"static-grey"}),
            None,
            false,
        )
        .unwrap();
        for _ in 0..MAX_PENDING + 2 {
            learner.sampled = Instant::now() - WINDOW;
            learner.previous = [0; 4];
            assert!(!learner.maintain(&mut s, &mut p, 1));
        }
        assert_eq!(learner.pending(), MAX_PENDING);
        learner.cancel();
        assert!(learner.pending.is_empty());
        assert!(cancel.load(Ordering::Relaxed));
        let result = learner
            .job
            .take()
            .unwrap()
            .thread
            .take()
            .unwrap()
            .join()
            .unwrap();
        assert!(result.is_err());
    }
    #[test]
    #[ignore = "local Heroes fixture, paired one-second replay"]
    fn local_city_learning() {
        use std::io::Read;
        let scene = std::env::var("DOSLET_LEARNING_SCENE").unwrap_or_else(|_| "city".into());
        assert!(["city", "menu", "puzzle"].contains(&scene.as_str()));
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(
            std::fs::File::open(format!("../../target/heroes-states/{scene}.pb.gz")).unwrap(),
        )
        .read_to_end(&mut bytes)
        .unwrap();
        let live = Session::load_state(&bytes).unwrap();
        drop(bytes);
        let capture = Instant::now();
        let snapshot = live.capture_working_state().unwrap();
        let micros = capture.elapsed().as_micros();
        let result = profile_pair(
            snapshot,
            live.epoch(),
            100,
            300,
            1_000_000_000,
            &AtomicBool::new(false),
        )
        .unwrap();
        let mut counts = [0; 4];
        for node in result.nodes.values() {
            counts[match node.tier {
                Tier::Fast => 0,
                Tier::Slow => 1,
                Tier::Grey => 2,
                Tier::Irq => 3,
            }] += 1;
        }
        eprintln!(
            "LEARNING capture_us={micros} host_seconds={} labels={counts:?}",
            result.seconds
        );
        assert!(counts[0] > 0 && counts[1] > 0);
        let path = std::path::Path::new(
            "../../target/tier-weighted-profiles/heroes-scenes-cold-first.json",
        );
        let mut before =
            crate::native_speed_tiers::Controller::load(path, 8, Some(500), 100).unwrap();
        let mut after =
            crate::native_speed_tiers::Controller::load(path, 8, Some(500), 100).unwrap();
        let changes = apply(&mut after.profile, &live, result.nodes);
        let rows: Vec<_> = after.profile.nodes.iter().map(|(&(pc, wide), n)| serde_json::json!({
            "pc":pc, "bits":if wide {32} else {16}, "bytes":n.code, "tier":n.tier.name(),
            "reason":if n.tier == Tier::Irq {"interrupt-frame"} else if n.locked {"code-conflict"} else {"replay"}
        })).collect();
        std::fs::write(
            format!("../../target/tier-weighted-profiles/{scene}-verified-irq.json"),
            serde_json::to_vec_pretty(
                &serde_json::json!({"nodes":rows,"unknown":"grey","irq_policy":"static-grey"}),
            )
            .unwrap(),
        )
        .unwrap();
        let retained = before
            .profile
            .nodes
            .iter()
            .filter(|(_, n)| n.tier != Tier::Grey)
            .count();
        assert!(
            before
                .profile
                .nodes
                .iter()
                .all(|(key, n)| n.tier == Tier::Grey || after.profile.nodes[key].tier == n.tier)
        );
        fn measure(live: &Session, c: &mut crate::native_speed_tiers::Controller) -> [u64; 4] {
            let mut guest = live.capture_working_state().unwrap().restore().unwrap();
            guest.set_idle_loops(true);
            guest.set_vga_wait_acceleration(true);
            c.enable(&mut guest);
            guest.start();
            let misses = guest.audio_status().dsp.block_irq_deadline_misses;
            let target = guest.emulated_nanos() + 2_000_000_000;
            let mut pcm = [doslet_pcm::EMPTY; 1024];
            let mut midi = [0; 1024];
            while guest.emulated_nanos() < target {
                c.advance(&mut guest, 10_000, target).unwrap();
                while guest.drain_pcm_timed(&mut pcm) != 0 {}
                while guest.drain_midi(&mut midi) != 0 {}
                assert!(guest.status() == SessionStatus::Running);
            }
            assert_eq!(guest.audio_status().dsp.block_irq_deadline_misses, misses);
            c.stats.steps
        }
        let a = measure(&live, &mut before);
        let b = measure(&live, &mut after);
        eprintln!(
            "MONOTONIC retained={retained} changes={changes} before={a:?} after={b:?} grey_before={:.2}% grey_after={:.2}%",
            100.0 * a[2] as f64 / a.iter().sum::<u64>() as f64,
            100.0 * b[2] as f64 / b.iter().sum::<u64>() as f64
        );
    }

    #[test]
    fn legacy_irq_and_scene_guesses_can_learn_but_code_conflicts_stay_guarded() {
        let live = Session::new(&[0x90, 0xeb, 0xfd]).unwrap();
        let key = (live.debug_pc(), false);
        let mut code = [0; 15];
        assert!(live.debug_read_memory(key.0, &mut code));
        let path =
            std::env::temp_dir().join(format!("doslet-legacy-irq-{}.json", std::process::id()));
        for (reason, expected) in [
            ("interrupt", 1),
            ("scene-conflict", 1),
            ("code-conflict", 0),
        ] {
            let json = serde_json::json!({"unknown":"grey", "irq_policy":"static-grey", "nodes":[{
                "pc":key.0,"bits":16,"bytes":code,"tier":"grey","reason":reason
            }]});
            std::fs::write(&path, serde_json::to_vec(&json).unwrap()).unwrap();
            for tier in [Tier::Slow, Tier::Irq] {
                let mut profile = Profile::load_labels(&path).unwrap();
                let candidate = BTreeMap::from([(
                    key,
                    Node {
                        code,
                        tier,
                        locked: tier == Tier::Irq,
                    },
                )]);
                assert_eq!(apply(&mut profile, &live, candidate), expected);
                assert!(profile.nodes[&key].tier == if expected == 1 { tier } else { Tier::Grey });
                if expected == 1 && tier == Tier::Irq {
                    assert_eq!(
                        apply(
                            &mut profile,
                            &live,
                            BTreeMap::from([(
                                key,
                                Node {
                                    code,
                                    tier: Tier::Slow,
                                    locked: false,
                                }
                            )])
                        ),
                        0
                    );
                    assert!(profile.nodes[&key].tier == Tier::Irq);
                }
            }
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn classification_requires_repeated_stable_or_growing_work() {
        assert!(classify(100, 100) == Tier::Fast);
        assert!(classify(100, 300) == Tier::Slow);
        assert!(classify(100, 130) == Tier::Grey);
        assert!(classify(5, 100) == Tier::Grey);
    }
    #[test]
    fn paired_replays_leave_live_guest_unchanged() {
        let mut live = Session::new(&[0x90, 0xeb, 0xfd]).unwrap();
        live.start();
        let before = live.save_state().unwrap();
        let result = profile_pair(
            live.capture_working_state().unwrap(),
            live.epoch(),
            1,
            3,
            60_000_000,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(live.save_state().unwrap(), before);
        assert!(result.nodes.values().any(|n| n.tier == Tier::Slow));
    }
    #[test]
    fn learning_never_revokes_hot_or_cold_and_checks_code() {
        let live = Session::new(&[0x90, 0xeb, 0xfd]).unwrap();
        let key = (live.debug_pc(), false);
        let mut code = [0; 15];
        live.debug_read_memory(key.0, &mut code);
        let nodes = |tier, locked| BTreeMap::from([(key, Node { code, tier, locked })]);
        for established in [Tier::Fast, Tier::Slow] {
            let mut p = Profile {
                nodes: nodes(established, false),
                probes: BTreeSet::new(),
                unknown: Tier::Grey,
                irq_grey: true,
                prepared: Vec::new(),
                index: Default::default(),
            };
            for candidate in [Tier::Fast, Tier::Slow, Tier::Grey, Tier::Irq] {
                for locked in [false, true] {
                    assert_eq!(apply(&mut p, &live, nodes(candidate, locked)), 0);
                    assert!(p.nodes[&key].tier == established);
                }
            }
        }
        let mut p = Profile {
            nodes: nodes(Tier::Grey, false),
            probes: BTreeSet::new(),
            unknown: Tier::Grey,
            irq_grey: true,
            prepared: Vec::new(),
            index: Default::default(),
        };
        let mut changed = nodes(Tier::Fast, false);
        changed.get_mut(&key).unwrap().code[0] ^= 1;
        assert_eq!(apply(&mut p, &live, changed), 1);
        let mut probe = live.capture_working_state().unwrap().restore().unwrap();
        probe
            .set_execution_tiers(
                Some(vec![doslet_x86::ExecutionLabel {
                    address: key.0,
                    code32: key.1,
                    bytes: p.nodes[&key].code,
                    tier: 0,
                }]),
                [Some(500), Some(8), Some(100)],
            )
            .unwrap();
        probe.start();
        probe.advance(1);
        assert_eq!(probe.executed_tier_counts(), [0, 0, 1, 0]);
        p.nodes.clear();
        assert_eq!(apply(&mut p, &live, nodes(Tier::Slow, false)), 1);
    }

    #[test]
    fn irq_tracking_uses_actual_returns_not_shared_callee_edges() {
        let main = Boundary {
            cs: 1,
            ip: 100,
            ss: 2,
            sp: 1000,
        };
        let handler = Boundary {
            ip: 200,
            sp: 988,
            ..main
        };
        let mut irq = Interrupts::default();
        irq.enter(main, true).unwrap();
        irq.enter(handler, false).unwrap();
        assert_eq!(irq.hardware_depth, 1);
        assert!(irq.returned(handler));
        assert_eq!(irq.hardware_depth, 1);
        assert!(irq.returned(main));
        assert_eq!(irq.hardware_depth, 0);
        assert!(irq.ready);
        assert!(!irq.returned(main));
    }
}
