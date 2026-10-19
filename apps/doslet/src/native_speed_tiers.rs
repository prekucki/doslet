//! Native weighted scheduling. Labels are decoded/compiled once; blocks stay intact.
#[path = "native_speed_learning.rs"]
mod learning;
#[path = "native_speed_persistence.rs"]
mod persistence;
#[path = "native_speed_tags.rs"]
mod tags;
use crate::speed_tiers::{Profile, Tier};
use doslet_runtime::{Session, SessionStatus, session::debug::ExecutionSettings};
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Default)]
pub(crate) struct Stats {
    pub enabled: bool,
    pub learning: bool,
    pub learning_running: bool,
    pub learning_pending: usize,
    pub learning_jobs: u64,
    pub learning_changes: u64,
    pub learning_grey_percent: f64,
    pub learning_capture_micros: u64,
    pub restored_labels: u64,
    pub saved_updates: u64,
    pub save_pending: bool,
    pub hot_mhz: Option<u16>,
    pub cold_mhz: u16,
    pub grey_mhz: u16,
    pub nanos: [u64; 4],
    pub steps: [u64; 4],
    pub execution_nanos: u64,
}
impl Stats {
    pub fn percent(&self, tier: usize) -> f64 {
        100.0 * self.nanos[tier] as f64 / self.nanos.iter().sum::<u64>().max(1) as f64
    }
}
pub(crate) struct Controller {
    profile_path: Option<std::path::PathBuf>,
    profile: Profile,
    learner: Option<learning::Learner>,
    persistence: Option<persistence::Persistence>,
    pub stats: Stats,
    settings: Option<ExecutionSettings>,
    epoch: u64,
    pending_nanos: u64,
    max_nanos: u64,
}
impl Controller {
    pub fn profile_info(&self) -> crate::cpu_config::ProfileInfo {
        crate::cpu_config::ProfileInfo {
            path: self
                .profile_path
                .as_deref()
                .or_else(|| self.persistence.as_ref().map(|p| p.path()))
                .map(|p| p.to_string_lossy().into_owned()),
            labels: self.profile.nodes.len(),
        }
    }
    pub fn suggested_path(name: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
        let name: String = name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
            .take(48)
            .collect();
        Ok(persistence::data_directory()?
            .join("profiles")
            .join(format!(
                "{}-speed-tiers.json",
                if name.is_empty() { "game" } else { &name }
            )))
    }
    pub fn export_new(
        &self,
        path: &Path,
        rates: [Option<u16>; 3],
    ) -> Result<(), Box<dyn std::error::Error>> {
        persistence::export_new(path, rates, &self.profile.nodes)
    }
    pub fn load_ui(
        path: &Path,
        fallback: [Option<u16>; 3],
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let value = persistence::read_value(path)?.ok_or("Profile file does not exist")?;
        let rates: [Option<u16>; 3] = match value.get("rates") {
            Some(value) => serde_json::from_value(value.clone())?,
            None => fallback,
        };
        let cold = rates[1].ok_or("Cold speed is required")?;
        let grey = rates[2].ok_or("Grey speed is required")?;
        Self::from_profile(
            path,
            Profile::from_json(&value, None, false)?,
            cold,
            rates[0],
            grey,
        )
    }
    /// MAX execution can leave device time to settle after a profile switch.
    pub fn inherit_pending(&mut self, old: &mut Self, session: &Session) {
        if old.epoch == session.epoch() {
            self.epoch = old.epoch;
            self.pending_nanos = core::mem::take(&mut old.pending_nanos);
        }
    }
    pub fn untrained() -> Self {
        Self {
            profile_path: None,
            profile: Profile::from_json(
                &serde_json::json!({"unknown":"grey", "irq_policy":"static-grey", "nodes":[]}),
                None,
                false,
            )
            .expect("empty grey profile"),
            learner: None,
            persistence: None,
            stats: Stats {
                hot_mhz: Some(500),
                cold_mhz: 8,
                grey_mhz: 133,
                ..Stats::default()
            },
            settings: None,
            epoch: 0,
            pending_nanos: 0,
            max_nanos: 0,
        }
    }
    /// Change rates while disabled; the next enable installs them together.
    pub fn configure(&mut self, hot: Option<u16>, grey: u16, cold: u16, learn: bool) {
        debug_assert!(!self.stats.enabled);
        self.stats.hot_mhz = hot;
        self.stats.grey_mhz = grey;
        self.stats.cold_mhz = cold;
        if let Some(p) = &mut self.persistence {
            p.set_rates([hot, Some(cold), Some(grey)]);
        }
        self.set_learning(learn);
    }
    pub fn load(
        path: &Path,
        cold_mhz: u16,
        hot_mhz: Option<u16>,
        grey_mhz: u16,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::from_profile(
            path,
            Profile::load_labels(path)?,
            cold_mhz,
            hot_mhz,
            grey_mhz,
        )
    }
    fn from_profile(
        path: &Path,
        mut profile: Profile,
        cold_mhz: u16,
        hot_mhz: Option<u16>,
        grey_mhz: u16,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if profile.unknown != Tier::Grey
            || !profile.irq_grey
            || [Some(cold_mhz), hot_mhz, Some(grey_mhz)]
                .into_iter()
                .flatten()
                .any(|r| !(1..=1000).contains(&r))
        {
            return Err(
                "live tiers require unknown=grey, irq_policy=static-grey and rates 1..1000 MHz"
                    .into(),
            );
        }
        let persistence = persistence::Persistence::open(
            path,
            &mut profile,
            [hot_mhz, Some(cold_mhz), Some(grey_mhz)],
        )?;
        if persistence.loaded != 0 {
            eprintln!(
                "[INFO doslet] restored {} learned labels for {}",
                persistence.loaded,
                path.display()
            );
        }
        let restored_labels = persistence.loaded;
        Ok(Self {
            profile_path: Some(path.into()),
            profile,
            learner: None,
            persistence: Some(persistence),
            stats: Stats {
                cold_mhz,
                hot_mhz,
                grey_mhz,
                restored_labels,
                ..Stats::default()
            },
            settings: None,
            epoch: 0,
            pending_nanos: 0,
            max_nanos: 0,
        })
    }
    pub fn bind_game(
        &mut self,
        exe: &Path,
        directory: Option<&Path>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut persistence = persistence::Persistence::game(
            exe,
            directory,
            &mut self.profile,
            [
                self.stats.hot_mhz,
                Some(self.stats.cold_mhz),
                Some(self.stats.grey_mhz),
            ],
        )?;
        persistence.changed();
        self.stats.restored_labels = persistence.loaded;
        self.persistence = Some(persistence);
        Ok(())
    }
    pub fn bind_game_bytes(
        &mut self,
        bytes: &[u8],
        directory: Option<&Path>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut persistence = persistence::Persistence::game_bytes(
            bytes,
            directory,
            &mut self.profile,
            [
                self.stats.hot_mhz,
                Some(self.stats.cold_mhz),
                Some(self.stats.grey_mhz),
            ],
        )?;
        persistence.changed();
        self.stats.restored_labels = persistence.loaded;
        self.persistence = Some(persistence);
        Ok(())
    }
    pub fn cached_game(
        exe: &Path,
        directory: Option<&Path>,
    ) -> Result<Option<Self>, Box<dyn std::error::Error>> {
        let Some((path, rates, _learning)) = persistence::cached_game(exe, directory)? else {
            return Ok(None);
        };
        let mut controller = Self::load(&path, rates[1].unwrap(), rates[0], rates[2].unwrap())?;
        controller.bind_game(exe, directory)?;
        // Restoring labels is passive. Background replays always require opt-in.
        eprintln!(
            "[INFO doslet] automatic speed-tier profile: {}",
            path.display()
        );
        Ok(Some(controller))
    }
    pub fn enable(&mut self, session: &mut Session) {
        if self.stats.enabled && self.epoch == session.epoch() {
            return;
        }
        if self.epoch != session.epoch() {
            self.pending_nanos = 0;
        }
        self.epoch = session.epoch();
        self.max_nanos = 0;
        self.stats = Stats {
            enabled: true,
            learning: self.learner.is_some(),
            restored_labels: self.stats.restored_labels,
            saved_updates: self.stats.saved_updates,
            save_pending: self.stats.save_pending,
            cold_mhz: self.stats.cold_mhz,
            hot_mhz: self.stats.hot_mhz,
            grey_mhz: self.stats.grey_mhz,
            ..Stats::default()
        };
        self.settings = Some(session.debug_execution_settings());
        session.set_adaptive_idle(false);
        session.set_menu_idle_site(None);
        session.set_event_idle_site(None);
        session.set_idle_observation(false);
        session.set_synchronization_analysis(false);
        let labels = self
            .profile
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
        session
            .set_execution_tiers(
                Some(labels),
                [
                    self.stats.hot_mhz,
                    Some(self.stats.cold_mhz),
                    Some(self.stats.grey_mhz),
                ],
            )
            .expect("validated tier profile");
        // Capture fresh program entry before the first live execution batch.
        self.maintain_learning(session);
    }
    pub fn learn_in_background(&mut self) {
        self.set_learning(true);
    }
    pub fn set_learning(&mut self, enabled: bool) {
        if enabled && self.learner.is_none() {
            self.learner = Some(learning::Learner::default());
        } else if !enabled {
            if let Some(mut learner) = self.learner.take() {
                learner.cancel();
            }
            self.stats.learning_running = false;
            self.stats.learning_pending = 0;
        }
        self.stats.learning = enabled;
        if let Some(p) = &mut self.persistence
            && p.learning != enabled
        {
            p.learning = enabled;
            p.changed();
        }
    }
    pub fn maintain_learning(&mut self, session: &mut Session) {
        self.maintain_persistence();
        if !self.stats.enabled {
            return;
        }
        let Some(learner) = &mut self.learner else {
            return;
        };
        if learner.maintain(session, &mut self.profile, self.stats.grey_mhz) {
            let labels = self
                .profile
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
            session
                .replace_execution_labels(labels)
                .expect("bounded learned labels");
            if let Some(p) = &mut self.persistence {
                p.changed();
            }
        }
        self.stats.learning_running = learner.running();
        self.stats.learning_pending = learner.pending();
        self.stats.learning_jobs = learner.jobs;
        self.stats.learning_changes = learner.changes;
        self.stats.learning_grey_percent = learner.grey_percent;
        self.stats.learning_capture_micros = learner.capture_micros;
        self.maintain_persistence();
    }
    fn maintain_persistence(&mut self) {
        if let Some(p) = &mut self.persistence {
            p.poll(&self.profile);
            self.stats.restored_labels = p.loaded;
            self.stats.saved_updates = p.writes;
            self.stats.save_pending = p.pending();
        }
    }
    pub fn take_persistence_error(&mut self) -> Option<String> {
        self.persistence.as_mut().and_then(|p| p.error.take())
    }
    pub fn disable(&mut self, session: &mut Session) {
        if let Some(learner) = &mut self.learner {
            learner.cancel();
        }
        if self.epoch == session.epoch() {
            session
                .set_execution_tiers(
                    None,
                    [None, Some(self.stats.cold_mhz), Some(self.stats.grey_mhz)],
                )
                .expect("validated rates");
            if let Some(settings) = self.settings.take() {
                session.debug_restore_accelerations(settings);
            }
        }
        self.settings = None;
        self.stats.enabled = false;
    }
    pub fn has_pending_time(&self) -> bool {
        self.pending_nanos != 0
    }
    pub fn settle(&mut self, session: &mut Session) -> Result<(), String> {
        if self.pending_nanos != 0 {
            let used = session
                .debug_advance_device_time(self.pending_nanos.min(250_000))
                .map_err(|e| e.to_string())?;
            if used == 0 {
                return Err("speed-tier device clock stopped".into());
            }
            self.pending_nanos -= used;
        }
        Ok(())
    }
    pub fn advance(
        &mut self,
        session: &mut Session,
        budget: u32,
        target: u64,
    ) -> Result<u32, String> {
        if self.epoch != session.epoch() {
            self.enable(session);
        }
        self.settle(session)?;
        if self.pending_nanos != 0 {
            return Ok(0);
        }
        let start = Instant::now();
        let before = session.emulated_nanos();
        let guest_end = target.min(before.saturating_add(250_000));
        let hot_before = session.execution_tier_stats().0[0];
        let mut used = 0;
        while used < budget
            && session.emulated_nanos() < guest_end
            && session.status() == SessionStatus::Running
        {
            // Existing CPU/device boundaries bound each block. Colour never
            // forces an exit. Quantum overshoot remains in the guest clock.
            let completed = session.advance((budget - used).min(256));
            used += completed;
            if completed == 0
                || session.timeslice_yielded()
                || start.elapsed() >= Duration::from_micros(250)
            {
                break;
            }
        }
        let elapsed = start.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        self.stats.execution_nanos += elapsed;
        let (counts, mut nanos) = session.execution_tier_stats();
        // MAX consumes no instruction budget. A host-work quantum still moves
        // hardware time forward, so a pure HOT loop cannot starve timers/input.
        if self.stats.hot_mhz.is_none() && counts[0] > hot_before {
            let extra = elapsed.saturating_sub(session.emulated_nanos() - before);
            self.pending_nanos += extra;
            self.max_nanos += extra;
            self.settle(session)?;
        }
        nanos[0] += self.max_nanos;
        self.stats.steps = counts;
        self.stats.nanos = nanos;
        Ok(used)
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        if let Some(p) = &mut self.persistence {
            p.finish(&self.profile);
            if let Some(error) = p.error.take() {
                eprintln!("[ERROR doslet] {error}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doslet_x86::Gpr32;
    use std::collections::{BTreeMap, BTreeSet};
    #[test]
    fn exported_profiles_roundtrip_rates_labels_and_manual_guards() {
        let fixture = crate::profile::tests::Fixture::new("");
        let path = fixture.root.join("snapshot.json");
        let mut c = grey_controller();
        for (i, tier) in [Tier::Fast, Tier::Slow, Tier::Grey, Tier::Irq]
            .into_iter()
            .enumerate()
        {
            c.profile.nodes.insert(
                (0x10000 + i as u32, false),
                crate::speed_tiers::Node {
                    tier,
                    code: [0x90; 15],
                    manual: i == 0,
                    locked: i == 2,
                },
            );
        }
        c.export_new(&path, [Some(500), Some(8), Some(100)])
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(c.export_new(&path, [None, Some(8), Some(133)]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        let loaded = Controller::load_ui(&path, [None, Some(16), Some(33)]).unwrap();
        assert_eq!(
            (
                loaded.stats.hot_mhz,
                loaded.stats.cold_mhz,
                loaded.stats.grey_mhz
            ),
            (Some(500), 8, 100)
        );
        assert!(!loaded.stats.learning);
        for (key, expected) in &c.profile.nodes {
            let node = &loaded.profile.nodes[key];
            assert!(node.tier == expected.tier);
            assert_eq!(
                (node.code, node.manual, node.locked),
                (expected.code, expected.manual, expected.locked)
            );
        }
    }
    #[test]
    fn replacing_a_profile_retains_max_device_time_debt_without_advancing_the_guest() {
        let mut session = Session::new(&[0x90, 0xeb, 0xfd]).unwrap();
        let mut old = grey_controller();
        old.enable(&mut session);
        old.pending_nanos = 10_000;
        let nanos = session.emulated_nanos();
        old.disable(&mut session);
        let mut new = grey_controller();
        new.inherit_pending(&mut old, &session);
        new.enable(&mut session);
        assert!(!old.has_pending_time());
        assert!(new.has_pending_time());
        assert_eq!(session.emulated_nanos(), nanos);
        new.settle(&mut session).unwrap();
        assert!(!new.has_pending_time());
        assert_eq!(session.emulated_nanos(), nanos + 10_000);
    }
    #[test]
    fn manual_tags_are_guarded_live_and_preserve_guest_state_and_counters() {
        use crate::speed_tiers::Node;
        let mut s = Session::new(&[0x90, 0xeb, 0xfd]).unwrap();
        let pc = s.debug_pc();
        let mut c = grey_controller();
        for i in 0..5 {
            let mut code = [0; 15];
            assert!(s.debug_read_memory(pc + i, &mut code));
            if i == 2 {
                code[0] ^= 1;
            }
            c.profile.nodes.insert(
                (pc + i, false),
                Node {
                    code,
                    tier: if i == 3 { Tier::Irq } else { Tier::Slow },
                    locked: i == 4,
                    manual: false,
                },
            );
        }
        c.enable(&mut s);
        s.start();
        s.advance(50);
        let before = s.save_state().unwrap();
        let counts = s.execution_tier_stats();
        let view = c.code_tags(&mut s, pc, pc + 5, 16, None).unwrap();
        assert_eq!(view["matching"], 4);
        assert_eq!(view["code_mismatches"], 1);
        assert_eq!(view["protected"], 2);
        let result = c
            .code_tags(&mut s, pc, pc + 5, 16, Some(Tier::Fast))
            .unwrap();
        assert_eq!(result["changed"], 2);
        assert_eq!(s.save_state().unwrap(), before);
        assert_eq!(s.execution_tier_stats(), counts);
        let prior = s.executed_tier_counts();
        s.advance(50);
        assert!(s.executed_tier_counts()[0] > prior[0]);
        assert_eq!(
            c.code_tags(&mut s, pc, pc + 5, 16, Some(Tier::Fast))
                .unwrap()["changed"],
            0
        );
        c.code_tags(&mut s, pc, pc + 2, 16, Some(Tier::Grey))
            .unwrap();
        assert!(c.profile.nodes[&(pc, false)].manual);
        assert!(c.profile.nodes[&(pc, false)].tier == Tier::Grey);
        assert!(c.profile.nodes[&(pc + 2, false)].tier == Tier::Slow);
        assert!(c.profile.nodes[&(pc + 3, false)].tier == Tier::Irq);
        assert!(c.profile.nodes[&(pc + 4, false)].locked);
        for (lo, hi, bits) in [(pc, pc, 16), (pc, pc + 65537, 16), (pc, pc + 1, 8)] {
            assert!(c.code_tags(&mut s, lo, hi, bits, Some(Tier::Fast)).is_err());
        }
    }
    fn grey_controller() -> Controller {
        Controller {
            profile_path: None,
            learner: None,
            persistence: None,
            profile: Profile {
                nodes: BTreeMap::new(),
                probes: BTreeSet::new(),
                unknown: Tier::Grey,
                irq_grey: true,
                prepared: Vec::new(),
                index: crate::speed_tiers::PlanIndex::default(),
            },
            stats: Stats {
                cold_mhz: 8,
                grey_mhz: 133,
                ..Stats::default()
            },
            settings: None,
            epoch: 0,
            max_nanos: 0,
            pending_nanos: 0,
        }
    }
    #[test]
    fn recolouring_preserves_clock_fraction_counts_and_separates_skipped_work() {
        let mut a = Session::new(&[0x90, 0xeb, 0xfd]).unwrap();
        let mut b = Session::new(&[0x90, 0xeb, 0xfd]).unwrap();
        for s in [&mut a, &mut b] {
            s.set_dynamic_core(false);
            s.set_execution_tiers(Some(Vec::new()), [Some(500), Some(8), Some(133)])
                .unwrap();
            s.start();
            s.advance(20_001);
        }
        let before = a.execution_tier_stats();
        assert!(a.executed_tier_counts()[2] < before.0[2]);
        a.replace_execution_labels(Vec::new()).unwrap();
        assert_eq!(a.execution_tier_stats(), before);
        for _ in 0..10 {
            a.advance(101);
            b.advance(101);
            assert_eq!(a.emulated_nanos(), b.emulated_nanos());
            assert_eq!(a.execution_tier_stats(), b.execution_tier_stats());
            assert_eq!(a.debug_pc(), b.debug_pc());
        }
    }

    #[test]
    fn mixed_tiers_use_configured_rates_and_keep_idle_acceleration() {
        use doslet_x86::ExecutionLabel;
        let code = [0x90, 0x90, 0x90, 0xeb, 0xfb];
        let mut s = Session::new(&code).unwrap();
        s.set_dynamic_core(false);
        let base = s.debug_pc();
        let labels = [(0, 0), (1, 1), (2, 2), (3, 3)]
            .map(|(offset, tier)| {
                let mut bytes = [0; 15];
                assert!(s.debug_read_memory(base + offset, &mut bytes));
                ExecutionLabel {
                    address: base + offset,
                    code32: false,
                    bytes,
                    tier,
                }
            })
            .to_vec();
        s.set_execution_tiers(Some(labels), [Some(500), Some(8), Some(100)])
            .unwrap();
        s.start();
        let before = s.emulated_nanos();
        assert_eq!(s.advance(4000), 4000);
        let (counts, nanos) = s.execution_tier_stats();
        assert_eq!(counts, [1000; 4]);
        assert_eq!(nanos, [8000, 500000, 40000, 40000]);
        assert_eq!(s.emulated_nanos() - before, 588000);
        assert!(s.idle_loop_stats().skipped_steps > 0);
        let idle = s.idle_loop_stats();
        assert_eq!(
            idle.skipped_tier_nanos.iter().sum::<u64>(),
            idle.skipped_nanos
        );
        assert!(idle.skipped_nanos <= s.emulated_nanos() - before);
        for (skipped, total) in idle.skipped_tier_nanos.into_iter().zip(nanos) {
            assert!(skipped <= total);
        }
    }

    #[test]
    fn live_grey_clock_and_policy_toggle_preserve_cpu_progress() {
        let mut s = Session::new(&[0x40, 0xeb, 0xfd]).unwrap();
        s.set_dynamic_core(false);
        s.start();
        let mut c = grey_controller();
        c.enable(&mut s);
        let time = s.emulated_nanos();
        let eax = s.registers().get32(Gpr32::Eax);
        let mut total = 0;
        while total < 133 {
            total += c.advance(&mut s, 133 - total, time + 10_000).unwrap();
        }
        assert_eq!(s.emulated_nanos() - time, 4000);
        assert_eq!(s.registers().get32(Gpr32::Eax), eax + 67);
        assert_eq!(c.stats.steps, [0, 0, 133, 0]);
        c.disable(&mut s);
        assert!(!s.dynamic_core_enabled());
        let before = s.emulated_nanos();
        s.advance(1);
        assert!(s.emulated_nanos() > before);
    }
    #[test]
    fn long_host_stall_drains_bounded_device_time_before_next_instruction() {
        let mut s = Session::new(&[0x90, 0xeb, 0xfd]).unwrap();
        s.start();
        let mut c = grey_controller();
        c.enable(&mut s);
        c.pending_nanos = 900_000;
        let steps = s.steps();
        assert_eq!(c.advance(&mut s, 100, 1_000_000).unwrap(), 0);
        assert_eq!(s.steps(), steps);
        assert_eq!(s.emulated_nanos(), 250_000);
        c.disable(&mut s);
        c.enable(&mut s);
        assert_eq!(c.pending_nanos, 650_000);
        while c.has_pending_time() {
            c.settle(&mut s).unwrap();
        }
        assert_eq!(s.emulated_nanos(), 900_000);
    }
}
