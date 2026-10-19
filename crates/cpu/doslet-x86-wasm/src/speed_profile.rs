//! Convert observed instruction entries, never blindly decode their 15-byte windows.
use crate::{Block, MAX_BLOCKS, MAX_INSTRUCTIONS, Translation};
use alloc::{collections::BTreeMap, vec::Vec};
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
            let weight = row["high_count"]
                .as_u64()
                .or_else(|| row["count"].as_u64())
                .unwrap_or(0);
            if let Some(old) = nodes.get_mut(&pc) {
                old.conflict |= conflict || old.bytes != signature;
                old.weight = old.weight.max(weight);
            } else {
                nodes.insert(
                    pc,
                    Node {
                        bytes: signature,
                        weight,
                        conflict,
                    },
                );
            }
        }
        let mut starts: Vec<_> = nodes
            .iter()
            .filter_map(|(&pc, node)| {
                if node.conflict {
                    report.conflicting_nodes += 1;
                    None
                } else if translation::decode(&node.bytes).is_none() {
                    report.unsupported_nodes += 1;
                    None
                } else {
                    Some((pc, node.weight))
                }
            })
            .collect();
        // Frequency controls admission at bounded capacity, independently of tiers.
        starts.sort_unstable_by_key(|&(pc, weight)| (core::cmp::Reverse(weight), pc));
        report.omitted_entries = starts.len().saturating_sub(MAX_BLOCKS);
        let mut blocks = Vec::new();
        for (start, _) in starts.into_iter().take(MAX_BLOCKS) {
            let mut pc = start;
            let mut code = Vec::new();
            let mut instructions = Vec::new();
            while instructions.len() < MAX_INSTRUCTIONS {
                let Some(node) = nodes.get(&pc).filter(|n| !n.conflict) else {
                    break;
                };
                let Some(i) = translation::decode(&node.bytes) else {
                    break;
                };
                let Some(next) = pc.checked_add(u32::from(i.length)) else {
                    break;
                };
                code.extend_from_slice(&node.bytes[..usize::from(i.length)]);
                instructions.push(i);
                pc = next;
                if matches!(i.operation, Operation::Branch { .. }) {
                    break;
                }
            }
            if !instructions.is_empty() {
                blocks.push(Block {
                    address_hint: start,
                    bytes: code,
                    instructions,
                });
            }
        }
        report.compiled_entries = blocks.len();
        Ok(Self {
            blocks,
            profile: bytes.to_vec(),
            report,
        })
    }
}
