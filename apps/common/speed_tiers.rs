//! Native host speed-tier profile and bounded batch planner, shared by replay and UI.
use doslet_runtime::{Session, session::debug::CodeVersion};
use doslet_x86::{
    Gpr32,
    analysis::{self, Effect, Flow},
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fs,
    path::Path,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tier {
    Fast,
    Slow,
    Grey,
    Irq,
}
impl Tier {
    fn parse(value: &Value) -> Result<Self, Box<dyn Error>> {
        match value.as_str() {
            Some("fast") => Ok(Self::Fast),
            Some("slow") => Ok(Self::Slow),
            Some("grey") => Ok(Self::Grey),
            Some("irq") => Ok(Self::Irq),
            _ => Err("speed tier must be fast, slow, grey or irq".into()),
        }
    }
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Slow => "slow",
            Self::Grey => "grey",
            Self::Irq => "irq",
        }
    }
}
#[derive(Clone)]
pub(crate) struct Node {
    pub(crate) locked: bool,
    pub(crate) code: [u8; 15],
    pub(crate) tier: Tier,
}
pub(crate) struct Plan {
    pub(crate) steps: u32,
    pub(crate) tier: Tier,
    pub(crate) invalid: bool,
}
pub(crate) struct Profile {
    pub(crate) nodes: BTreeMap<(u32, bool), Node>,
    pub(crate) probes: BTreeSet<u32>,
    pub(crate) unknown: Tier,
    pub(crate) irq_grey: bool,
    pub(crate) prepared: Vec<Prepared>,
    pub(crate) index: PlanIndex,
}
// Sparse address index, allocated at load time; no tree search during execution.
type CodePages = Vec<Option<Box<[u64]>>>;
#[derive(Default)]
pub(crate) struct PlanIndex {
    groups: Vec<Option<CodePages>>,
}
impl PlanIndex {
    fn insert(&mut self, pc: u32, wide: bool, index: usize) {
        if self.groups.is_empty() {
            self.groups.resize_with(1024, || None);
        }
        let group = self.groups[(pc >> 22) as usize]
            .get_or_insert_with(|| std::iter::repeat_with(|| None).take(1024).collect());
        let page = group[((pc >> 12) & 1023) as usize]
            .get_or_insert_with(|| vec![0; 4096].into_boxed_slice());
        let slot = &mut page[(pc & 4095) as usize];
        let shift = if wide { 32 } else { 0 };
        *slot = (*slot & !(u64::from(u32::MAX) << shift)) | ((index as u64 + 1) << shift);
    }
    fn get(&self, pc: u32, wide: bool) -> Option<usize> {
        let group = self.groups.get((pc >> 22) as usize)?.as_ref()?;
        let page = group[((pc >> 12) & 1023) as usize].as_ref()?;
        let id = (page[(pc & 4095) as usize] >> if wide { 32 } else { 0 }) as u32;
        id.checked_sub(1).map(|id| id as usize)
    }
}
pub(crate) struct Prepared {
    tier: Tier,
    steps: u32,
    rep: bool,
    code: Box<[u8]>,
    version: Option<CodeVersion>,
    valid: bool,
}
impl Profile {
    pub(crate) fn load(path: &Path, probes: Option<&Path>) -> Result<Self, Box<dyn Error>> {
        Self::load_mode(path, probes, true)
    }
    pub(crate) fn load_labels(path: &Path) -> Result<Self, Box<dyn Error>> {
        Self::load_mode(path, None, false)
    }
    fn load_mode(
        path: &Path,
        probes: Option<&Path>,
        prepare: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let c: Value = serde_json::from_slice(&fs::read(path)?)?;
        Self::from_json(&c, probes, prepare)
    }
    pub(crate) fn from_json(
        c: &Value,
        probes: Option<&Path>,
        prepare: bool,
    ) -> Result<Self, Box<dyn Error>> {
        let mut nodes = BTreeMap::new();
        for n in c["nodes"].as_array().ok_or("profile nodes")? {
            let pc = u32::try_from(n["pc"].as_u64().ok_or("pc")?)?;
            let wide = match n["bits"].as_u64() {
                Some(16) => false,
                Some(32) => true,
                _ => return Err("invalid bitness".into()),
            };
            let code: Vec<u8> = serde_json::from_value(n["bytes"].clone())?;
            let tier = Tier::parse(&n["tier"])?;
            nodes.insert(
                (pc, wide),
                Node {
                    // Legacy graph IRQ/scene guesses are not permanent evidence.
                    locked: matches!(n["reason"].as_str(), Some("code-conflict")),
                    code: code.try_into().map_err(|_| "code length")?,
                    tier,
                },
            );
            if nodes.len() > 100_000 {
                return Err("profile node limit".into());
            }
        }
        let p: Value = if let Some(path) = probes {
            serde_json::from_slice(&fs::read(path)?)?
        } else {
            serde_json::json!({"pcs": []})
        };
        let probes = p["pcs"]
            .as_array()
            .ok_or("probe PCs")?
            .iter()
            .map(|v| Ok(u32::try_from(v.as_u64().ok_or("probe integer")?)?))
            .collect::<Result<BTreeSet<_>, Box<dyn Error>>>()?;
        let unknown = Tier::parse(&c["unknown"])?;
        let mut profile = Self {
            nodes,
            probes,
            unknown,
            irq_grey: c["irq_policy"] == "static-grey",
            prepared: Vec::new(),
            index: PlanIndex::default(),
        };
        if prepare {
            profile.prepare();
        }
        Ok(profile)
    }

    // All control transfers, I/O and unknown effects end a batch. Probe PCs
    // execute alone. REP MOVS/STOS are bounded by both ECX and 64 microsteps.
    fn prepare(&mut self) {
        self.prepared = self
            .nodes
            .iter()
            .map(|(&(pc, wide), initial)| self.prepare_at(pc, wide, initial.tier))
            .collect();
        self.index = PlanIndex::default();
        for (id, &(pc, wide)) in self.nodes.keys().enumerate() {
            self.index.insert(pc, wide, id);
        }
    }

    fn prepare_at(&self, start: u32, wide: bool, tier: Tier) -> Prepared {
        let mut pc = start;
        let mut steps = 0;
        let mut expected = Vec::new();
        let mut rep = false;
        while let Some(n) = self.nodes.get(&(pc, wide)) {
            if n.tier != tier || (steps > 0 && self.probes.contains(&pc)) {
                break;
            }
            let offset = (pc - start) as usize;
            // One CodeCheck covers this span; bounded to the bus contract.
            if offset + 15 > usize::from(u8::MAX) {
                break;
            }
            expected.resize(offset + 15, 0);
            expected[offset..offset + 15].copy_from_slice(&n.code);
            let code = n.code;
            if steps == 0
                && !self.probes.contains(&pc)
                && wide
                && code[0] == 0xf3
                && matches!(code[1], 0xa4 | 0xa5 | 0xaa | 0xab)
            {
                rep = true;
                steps = 64;
                break;
            }
            if !wide {
                break;
            }
            let Some(d) = analysis::decode(&code, pc) else {
                break;
            };
            if d.flow != Flow::Next
                || matches!(d.effect, Effect::Unknown | Effect::Input | Effect::Output)
            {
                break;
            }
            steps += 1;
            if self.probes.contains(&pc) || steps == 64 {
                break;
            }
            let Some(next) = pc.checked_add(u32::from(d.length)) else {
                break;
            };
            pc = next;
        }
        Prepared {
            steps: steps.max(1),
            tier,
            rep,
            code: expected.into_boxed_slice(),
            version: None,
            valid: false,
        }
    }

    pub(crate) fn plan(&mut self, s: &Session) -> Plan {
        let grey = Plan {
            steps: 1,
            tier: Tier::Grey,
            invalid: false,
        };
        if !s.a20_enabled() || s.control_registers().cr0 & (1 << 31) != 0 {
            return grey;
        }
        let pc = s.debug_pc();
        let Some(id) = self.index.get(pc, s.registers().code32()) else {
            return Plan {
                tier: self.unknown,
                ..grey
            };
        };
        let p = &mut self.prepared[id];
        // No decoding or byte reads on a version hit. CPU stores and mapping
        // changes already invalidate these tokens for the JIT's code cache.
        if !p.version.is_some_and(|v| s.debug_code_valid(&v)) {
            let mut bytes = [0; 255];
            p.valid = s.debug_read_memory(pc, &mut bytes[..p.code.len()])
                && bytes[..p.code.len()] == *p.code;
            p.version = s.debug_code_version(pc, p.code.len() as u8);
        }
        if !p.valid {
            return Plan {
                invalid: true,
                ..grey
            };
        }
        Plan {
            steps: if p.rep {
                s.registers().get32(Gpr32::Ecx).clamp(1, p.steps)
            } else {
                p.steps
            },
            tier: p.tier,
            invalid: false,
        }
    }
}

pub(crate) fn charge_cpu_time(
    rate: Option<u64>,
    steps: u32,
    cpu_ns: u64,
    fraction: &mut u64,
    host_limit: bool,
) -> u64 {
    let charged = if let Some(rate) = rate {
        let numerator = 4000 * u64::from(steps) + *fraction;
        *fraction = numerator % rate;
        numerator / rate
    } else {
        cpu_ns
    };
    if host_limit {
        charged.max(cpu_ns)
    } else {
        charged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_grey_time_is_133_mhz_even_when_instrumented_execution_is_slower() {
        let mut fraction = 0;
        let total: u64 = (0..133)
            .map(|_| charge_cpu_time(Some(133), 1, 1000, &mut fraction, false))
            .sum();
        assert_eq!(total, 4000);
        assert_eq!(fraction, 0);
        assert_eq!(charge_cpu_time(None, 64, 321, &mut fraction, true), 321);
        assert_eq!(charge_cpu_time(Some(8), 1, 20, &mut fraction, true), 500);
        assert_eq!(charge_cpu_time(Some(8), 1, 700, &mut fraction, true), 700);
    }

    #[test]
    fn grey_covers_unknown_and_modified_code() {
        let mut program = [0x90; 24];
        program[..4].copy_from_slice(&[0xb0, 2, 0xe6, 0x92]);
        let mut s = Session::new(&program).unwrap();
        s.start();
        s.advance(2); // Enable A20, as required by the static native profile.
        assert!(s.a20_enabled());
        let mut profile = Profile {
            nodes: BTreeMap::new(),
            probes: BTreeSet::new(),
            unknown: Tier::Grey,
            irq_grey: true,
            prepared: Vec::new(),
            index: PlanIndex::default(),
        };
        let plan = profile.plan(&s);
        assert!(plan.tier == Tier::Grey && plan.steps == 1 && !plan.invalid);
        let pc = s.debug_pc();
        let mut code = [0; 15];
        assert!(s.debug_read_memory(pc, &mut code));
        profile.nodes.insert(
            (pc, false),
            Node {
                locked: false,
                code,
                tier: Tier::Slow,
            },
        );
        profile.prepare();
        assert!(profile.plan(&s).tier == Tier::Slow);
        assert!(s.debug_write_memory(pc, &[0x40]));
        let plan = profile.plan(&s);
        assert!(plan.tier == Tier::Grey && plan.steps == 1 && plan.invalid);
    }

    #[test]
    fn prepared_batches_stop_at_tier_changes_and_control_transfers() {
        let mut p = Profile {
            nodes: BTreeMap::new(),
            probes: BTreeSet::new(),
            unknown: Tier::Grey,
            irq_grey: true,
            prepared: Vec::new(),
            index: PlanIndex::default(),
        };
        for pc in 0..8 {
            p.nodes.insert(
                (pc, true),
                Node {
                    locked: false,
                    code: [0x90; 15],
                    tier: if pc < 3 { Tier::Fast } else { Tier::Slow },
                },
            );
        }
        p.nodes.get_mut(&(5, true)).unwrap().code[0] = 0xc3;
        p.prepare();
        assert_eq!(p.prepared[p.index.get(0, true).unwrap()].steps, 3);
        assert_eq!(p.prepared[p.index.get(3, true).unwrap()].steps, 2);
        assert_eq!(p.prepared[p.index.get(5, true).unwrap()].steps, 1);
    }

    #[test]
    fn radix_index_separates_bitness_and_page_boundaries() {
        let mut index = PlanIndex::default();
        for (id, pc) in [0, 4095, 4096, 0x400000, u32::MAX].into_iter().enumerate() {
            index.insert(pc, false, id);
            index.insert(pc, true, id + 5);
            assert_eq!(index.get(pc, false), Some(id));
            assert_eq!(index.get(pc, true), Some(id + 5));
        }
        assert_eq!(index.get(4097, true), None);
        assert_eq!(index.get(0x800000, false), None);
    }
}
