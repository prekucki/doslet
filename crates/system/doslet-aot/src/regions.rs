//! Bounded straight-line regions over instruction identities, including overlap.
//!
//! Grouping does not establish instruction support or executable runtime code.
//! Only a unique, resolved `Next` fallthrough may stay inside a region. Branch
//! destinations, call/service continuations, joins and explicit boundaries start
//! new regions. A consumer can request additional boundaries for tier changes
//! or unsupported instructions without adding reachability to the graph.

use alloc::vec::Vec;
use core::{fmt, mem::size_of, ops::Range};

use crate::{
    ObjectAddress,
    cfg::{EdgeKind, EdgeTarget, InstructionFlow, InstructionNode, StaticCfg},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegionLimits {
    pub max_regions: usize,
    pub max_instructions: usize,
    /// A positive structural bound, independent of any host instruction budget.
    pub max_instructions_per_region: usize,
    pub max_boundaries: usize,
    /// Requested storage, including temporary indexes; allocator overhead excluded.
    pub max_owned_bytes: usize,
}

impl Default for RegionLimits {
    fn default() -> Self {
        Self {
            max_regions: 1_000_000,
            max_instructions: 1_000_000,
            max_instructions_per_region: 64,
            max_boundaries: 1_000_000,
            max_owned_bytes: 128 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub entry: ObjectAddress,
    nodes: Range<usize>,
}

impl Region {
    pub fn instruction_count(&self) -> usize {
        self.nodes.len()
    }
}

/// A deterministic partition: every discovered instruction belongs to exactly
/// one region. Source bytes may belong to multiple instructions in the CFG.
#[derive(Debug)]
pub struct RegionPlan<'cfg, 'image> {
    graph: &'cfg StaticCfg<'image>,
    regions: Vec<Region>,
    node_indices: Vec<usize>,
}

impl<'cfg, 'image> RegionPlan<'cfg, 'image> {
    /// Boundaries must name existing instruction starts. They split regions;
    /// they never make profile entries or unknown source bytes reachable.
    pub fn build(
        graph: &'cfg StaticCfg<'image>,
        boundaries: &[ObjectAddress],
        limits: RegionLimits,
    ) -> Result<Self, RegionError> {
        if limits.max_instructions_per_region == 0 {
            return Err(RegionError::ZeroRegionLength);
        }
        let nodes = graph.nodes();
        if nodes.len() > limits.max_instructions {
            return Err(RegionError::InstructionLimit);
        }
        if boundaries.len() > limits.max_boundaries {
            return Err(RegionError::BoundaryLimit);
        }
        let mut budget = Budget(limits.max_owned_bytes);
        let mut states = Vec::new();
        budget.reserve(&mut states, nodes.len())?;
        states.resize(nodes.len(), State::default());
        for &entry in graph.entries() {
            // A CFG entry may legitimately be a stopped path, not a node.
            if let Some(index) = node_index(graph, entry) {
                states[index].leader = true;
            }
        }
        for &entry in boundaries {
            let index = node_index(graph, entry).ok_or(RegionError::UnknownBoundary(entry))?;
            states[index].leader = true;
        }

        // Cfg edges are sorted by source, so counting a node's outgoing edges
        // needs one linear pass rather than a scan per instruction.
        let mut cursor = 0;
        for (index, node) in nodes.iter().enumerate() {
            while graph
                .edges()
                .get(cursor)
                .is_some_and(|e| e.source < node.address)
            {
                cursor += 1;
            }
            let start = cursor;
            while graph
                .edges()
                .get(cursor)
                .is_some_and(|e| e.source == node.address)
            {
                cursor += 1;
            }
            let edges = &graph.edges()[start..cursor];
            for edge in edges {
                let EdgeTarget::Address(address) = edge.target else {
                    continue;
                };
                let Some(target) = node_index(graph, address) else {
                    continue;
                };
                // Only distinguishing zero, one and many is necessary.
                states[target].incoming = states[target].incoming.saturating_add(1);
                let linear = node.flow == InstructionFlow::Next
                    && edges.len() == 1
                    && edge.kind == EdgeKind::Fallthrough
                    && nodes[target].bitness == node.bitness;
                if linear {
                    states[index].next = Some(target);
                } else {
                    states[target].leader = true;
                }
            }
        }
        for state in &mut states {
            state.leader |= state.incoming != 1;
        }
        let mut plan = Self {
            graph,
            regions: Vec::new(),
            node_indices: Vec::new(),
        };
        budget.reserve(&mut plan.regions, nodes.len().min(limits.max_regions))?;
        budget.reserve(&mut plan.node_indices, nodes.len())?;
        // Process explicit leaders first. The second pass covers any residual
        // closed component without depending on worklist/decoder visitation order.
        for leaders_only in [true, false] {
            for start in 0..nodes.len() {
                if states[start].assigned || (leaders_only && !states[start].leader) {
                    continue;
                }
                let mut next = Some(start);
                while let Some(first) = next {
                    if states[first].assigned {
                        break;
                    }
                    if plan.regions.len() == limits.max_regions {
                        return Err(RegionError::RegionLimit);
                    }
                    let begin = plan.node_indices.len();
                    let mut index = first;
                    loop {
                        states[index].assigned = true;
                        plan.node_indices.push(index);
                        next = states[index].next;
                        let Some(candidate) = next else { break };
                        if states[candidate].assigned || states[candidate].leader {
                            next = None;
                            break;
                        }
                        if plan.node_indices.len() - begin == limits.max_instructions_per_region {
                            break;
                        }
                        index = candidate;
                    }
                    plan.regions.push(Region {
                        entry: nodes[first].address,
                        nodes: begin..plan.node_indices.len(),
                    });
                }
            }
        }
        plan.regions.sort_unstable_by_key(|region| region.entry);
        Ok(plan)
    }

    pub fn graph(&self) -> &'cfg StaticCfg<'image> {
        self.graph
    }

    pub fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// Node indices refer to `graph().nodes()` and preserve execution order.
    pub fn node_indices(&self, region_index: usize) -> Option<&[usize]> {
        let region = self.regions.get(region_index)?;
        Some(&self.node_indices[region.nodes.clone()])
    }

    pub fn instructions(
        &self,
        region_index: usize,
    ) -> Option<impl ExactSizeIterator<Item = &InstructionNode>> {
        Some(
            self.node_indices(region_index)?
                .iter()
                .map(|&index| &self.graph.nodes()[index]),
        )
    }

    pub fn region_index(&self, entry: ObjectAddress) -> Option<usize> {
        self.regions
            .binary_search_by_key(&entry, |region| region.entry)
            .ok()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegionError {
    ZeroRegionLength,
    InstructionLimit,
    RegionLimit,
    BoundaryLimit,
    UnknownBoundary(ObjectAddress),
    OwnedByteLimit,
    Allocation,
}

impl fmt::Display for RegionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownBoundary(address) => {
                write!(f, "region boundary is not a CFG instruction: {address:?}")
            }
            other => write!(f, "static region preparation: {other:?}"),
        }
    }
}

impl core::error::Error for RegionError {}

#[derive(Clone, Copy, Default)]
struct State {
    incoming: u8,
    leader: bool,
    assigned: bool,
    next: Option<usize>,
}

struct Budget(usize);

impl Budget {
    fn reserve<T>(&mut self, values: &mut Vec<T>, count: usize) -> Result<(), RegionError> {
        let bytes = count
            .checked_mul(size_of::<T>())
            .ok_or(RegionError::OwnedByteLimit)?;
        self.0 = self
            .0
            .checked_sub(bytes)
            .ok_or(RegionError::OwnedByteLimit)?;
        values
            .try_reserve_exact(count)
            .map_err(|_| RegionError::Allocation)
    }
}

fn node_index(graph: &StaticCfg<'_>, address: ObjectAddress) -> Option<usize> {
    graph
        .nodes()
        .binary_search_by_key(&address, |node| node.address)
        .ok()
}

#[cfg(test)]
mod tests;
