//! Convert observed instruction entries, never blindly decode their 15-byte windows.
use crate::{Block, MAX_BLOCKS, MAX_INSTRUCTIONS, MAX_REGIONS, Region, Translation};
use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};
use doslet_x86::translation::{self, Operation};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Default)]
pub struct ProfileReport {
    pub input_nodes: usize,
    pub code16_nodes: usize,
    pub conflicting_nodes: usize,
    pub unsupported_nodes: usize,
    pub compiled_entries: usize,
    pub omitted_entries: usize,
}
struct Node {
    bytes: [u8; 15],
    weight: u64,
    conflict: bool,
    /// Partitioning hint only; the runtime independently checks every live tier.
    tier: u8,
}
impl Translation {
    /// Accept standalone and learned speed-tiers profiles. Tier labels never
    /// authorize omitted execution, IRQ attribution or changed guest timing.
    /// Compilation currently admits only observed, stable 32-bit register code.
    pub fn from_speed_tiers(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > 64 * 1024 * 1024 {
            return Err("speed-tiers profile exceeds 64 MiB");
        }
        let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid speed-tiers JSON")?;
        for key in ["version", "learning_version"] {
            if value.get(key).is_some_and(|v| v.as_u64() != Some(1)) {
                return Err("unsupported speed-tiers version");
            }
        }
        let rows = value["nodes"]
            .as_array()
            .ok_or("missing speed-tiers nodes")?;
        if rows.len() > 100_000 {
            return Err("speed-tiers node limit");
        }
        let mut report = ProfileReport {
            input_nodes: rows.len(),
            ..ProfileReport::default()
        };
        let mut nodes = BTreeMap::<u32, Node>::new();
        for row in rows {
            let pc = row["pc"]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or("invalid node PC")?;
            let wide = match row["bits"].as_u64() {
                Some(16) => false,
                Some(32) => true,
                _ => return Err("invalid node bitness"),
            };
            let code = row["bytes"].as_array().ok_or("invalid node bytes")?;
            if code.len() != 15 {
                return Err("speed-tiers signatures must contain 15 bytes");
            }
            let mut signature = [0; 15];
            for (dst, src) in signature.iter_mut().zip(code) {
                *dst = src
                    .as_u64()
                    .and_then(|v| u8::try_from(v).ok())
                    .ok_or("invalid signature byte")?;
            }
            if !wide {
                report.code16_nodes += 1;
                continue;
            }
            let conflict = row["reason"] == "code-conflict" || row["changed"] == true;
            let tier = match row["tier"].as_str() {
                Some("fast") => 0,
                Some("slow") => 1,
                Some("irq") => 3,
                _ => 2,
            };
            let weight = row["high_count"]
                .as_u64()
                .or_else(|| row["count"].as_u64())
                .unwrap_or(0);
            if let Some(old) = nodes.get_mut(&pc) {
                old.conflict |= conflict || old.bytes != signature;
                old.weight = old.weight.max(weight);
                if old.tier != tier {
                    old.tier = 2;
                }
            } else {
                nodes.insert(
                    pc,
                    Node {
                        bytes: signature,
                        weight,
                        conflict,
                        tier,
                    },
                );
            }
        }
        let regions = regions(&value)?;
        let region_of = |pc: u32| regions.iter().position(|r| r.contains(pc));
        let mut supported = BTreeMap::new();
        for (&pc, node) in &nodes {
            if node.conflict {
                report.conflicting_nodes += 1;
            } else if let Some(i) = translation::decode(&node.bytes) {
                supported.insert(pc, i);
            } else {
                report.unsupported_nodes += 1;
            }
        }
        // Blocks start at leaders: code not reached by fallthrough from another
        // supported instruction of the same tier and region, and local branch
        // targets. Ordinary suffix entries would only duplicate a longer block.
        let continues = |pc: u32, next: u32| {
            supported.contains_key(&next)
                && nodes[&pc].tier == nodes[&next].tier
                && region_of(pc) == region_of(next)
        };
        let mut followers = BTreeSet::new();
        let mut leaders = BTreeSet::new();
        for (&pc, i) in &supported {
            let Some(next) = pc.checked_add(u32::from(i.length)) else {
                continue;
            };
            if let Operation::Branch {
                condition,
                displacement,
            } = i.operation
            {
                leaders.insert(next.wrapping_add(displacement as u32));
                if condition.is_some() {
                    leaders.insert(next);
                }
            } else if continues(pc, next) {
                followers.insert(next);
            }
        }
        leaders.retain(|pc| supported.contains_key(pc));
        leaders.extend(supported.keys().filter(|pc| !followers.contains(pc)));
        let mut pending: Vec<u32> = leaders.iter().copied().collect();
        let mut blocks = Vec::new();
        while let Some(start) = pending.pop() {
            let mut pc = start;
            let mut code = Vec::new();
            let mut instructions = Vec::new();
            loop {
                let i = supported[&pc];
                code.extend_from_slice(&nodes[&pc].bytes[..usize::from(i.length)]);
                instructions.push(i);
                let Some(next) = pc.checked_add(u32::from(i.length)) else {
                    break;
                };
                if matches!(i.operation, Operation::Branch { .. }) || !continues(pc, next) {
                    break;
                }
                if instructions.len() == MAX_INSTRUCTIONS {
                    // A full block continues at its own leader.
                    if leaders.insert(next) {
                        pending.push(next);
                    }
                    break;
                }
                pc = next;
            }
            blocks.push((
                nodes[&start].weight,
                Block {
                    address_hint: start,
                    bytes: code,
                    instructions,
                },
            ));
        }
        // Frequency controls admission at bounded capacity, independently of
        // tiers; unweighted static code prefers longer blocks.
        blocks.sort_unstable_by_key(|(weight, block)| {
            (
                core::cmp::Reverse(*weight),
                core::cmp::Reverse(block.instructions.len()),
                block.address_hint,
            )
        });
        report.omitted_entries = blocks.len().saturating_sub(MAX_BLOCKS);
        let blocks: Vec<_> = blocks
            .into_iter()
            .take(MAX_BLOCKS)
            .map(|(_, block)| block)
            .collect();
        report.compiled_entries = blocks.len();
        Ok(Self {
            blocks,
            regions,
            profile: bytes.to_vec(),
            report,
        })
    }
}

/// Optional `relocatable: [{base, size}]` spans, at preferred image addresses.
fn regions(value: &Value) -> Result<Vec<Region>, &'static str> {
    let Some(rows) = value.get("relocatable") else {
        return Ok(Vec::new());
    };
    let rows = rows.as_array().ok_or("invalid relocatable regions")?;
    if rows.len() > MAX_REGIONS {
        return Err("too many relocatable regions");
    }
    let mut regions = Vec::with_capacity(rows.len());
    for row in rows {
        let field = |key: &str| {
            row[key]
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or("invalid relocatable region")
        };
        let region = Region {
            base: field("base")?,
            size: field("size")?,
        };
        if region.size == 0 || region.base.checked_add(region.size - 1).is_none() {
            return Err("invalid relocatable region");
        }
        regions.push(region);
    }
    regions.sort_unstable_by_key(|r| r.base);
    if regions
        .windows(2)
        .any(|pair| pair[0].base + (pair[0].size - 1) >= pair[1].base)
    {
        return Err("overlapping relocatable regions");
    }
    Ok(regions)
}
