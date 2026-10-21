//! Optional bound-image pass for acyclic or explicitly bounded leaves.
//!
//! Each reachable x86 instruction retains its source identity. The CPU compiler
//! keeps registers live across internal branches and enforces memory proofs.
//! Calls, unresolved control flow and overlapping decode streams decline
//! admission. Cycles require an explicit finite work limit and a supported
//! structured-loop shape in the CPU compiler. This is frozen-image planning,
//! not a live-code proof, a source of permanent speed tiers or a device scheduler.

use super::{
    Budget, Lowered, LoweringError, decode_node, decode_node_with_offsets, lower_instruction,
    lower_instruction_with_virtual_registers,
};
use crate::{
    DefaultBitness, ObjectAddress,
    binding::DosFlatBindingPlan,
    cfg::{EdgeKind, EdgeTarget, InstructionFlow, StaticCfg},
};
use alloc::vec::Vec;
use doslet_x86_wasm::static_program::{
    Address,
    procedure_memory::{
        MAX_PROCEDURE_BLOCKS, MAX_PROCEDURES, MAX_TOTAL_PROCEDURE_BLOCKS, MAX_WORK_PER_CALL,
        ProcedureBlock, ProcedureCompileError, ProcedureGraphFunction, ProcedureGraphProgram,
        ProcedureId, ProcedureInstruction, ProcedureTerminator, ProcedureTerminatorKind,
    },
};

#[derive(Clone, Copy, Debug)]
pub struct MemoryProcedureLimits {
    pub max_input_nodes: usize,
    pub max_input_edges: usize,
    pub max_input_overlaps: usize,
    pub max_root_occurrences: usize,
    pub max_procedures: usize,
    /// One block per original instruction in this initial pass; includes RET.
    pub max_nodes_per_procedure: usize,
    pub max_total_nodes: usize,
    pub max_scan_steps: usize,
    /// Explicitly permit cyclic candidates; compile the resulting graph with
    /// compile_with_work_limit using this same finite instruction reservation.
    /// Backward branch targets also become roots for loop-head resumption.
    pub max_work_per_call: Option<u32>,
    /// Conservative cumulative reservations, including discarded scratch.
    pub max_owned_bytes: usize,
}
impl Default for MemoryProcedureLimits {
    fn default() -> Self {
        Self {
            max_input_nodes: 1_000_000,
            max_input_edges: 3_000_000,
            max_input_overlaps: 1_000_000,
            max_root_occurrences: 1_000_000,
            max_procedures: MAX_PROCEDURES,
            max_nodes_per_procedure: MAX_PROCEDURE_BLOCKS,
            max_total_nodes: MAX_TOTAL_PROCEDURE_BLOCKS,
            max_scan_steps: 32_000_000,
            max_work_per_call: None,
            max_owned_bytes: 128 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryProcedureRejection {
    MissingInstruction,
    UnsupportedMode,
    Overlap,
    UnsupportedInstruction,
    UnresolvedFlow,
    Call,
    Cycle,
    UnsupportedLoop,
    InstructionBound,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryProcedureDiagnostic {
    pub entry: ObjectAddress,
    pub instruction: ObjectAddress,
    pub reason: MemoryProcedureRejection,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryProcedureError {
    DifferentBinding,
    InputLimit,
    ScanLimit,
    Lowering(LoweringError),
    Compilation(ProcedureCompileError),
}
impl core::fmt::Display for MemoryProcedureError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "memory procedure planning: {self:?}")
    }
}
impl core::error::Error for MemoryProcedureError {}
impl From<LoweringError> for MemoryProcedureError {
    fn from(error: LoweringError) -> Self {
        Self::Lowering(error)
    }
}

#[derive(Debug)]
pub struct PreparedMemoryProcedures {
    program: ProcedureGraphProgram,
    entries: Vec<ObjectAddress>,
    sources: Vec<Vec<ObjectAddress>>,
    diagnostics: Vec<MemoryProcedureDiagnostic>,
    max_work_per_call: Option<u32>,
}

impl PreparedMemoryProcedures {
    pub fn from_bound_graph(
        graph: &StaticCfg<'_>,
        binding: &DosFlatBindingPlan<'_>,
        limits: MemoryProcedureLimits,
    ) -> Result<Self, MemoryProcedureError> {
        Self::from_bound_graph_impl(graph, binding, limits, false)
    }

    /// Also retain structurally recognized complete finite computations. Their
    /// live input/memory proof belongs to the owner; the ordinary work limit
    /// continues to govern instruction-by-instruction loop lowering only.
    pub fn from_bound_graph_with_computations(
        graph: &StaticCfg<'_>,
        binding: &DosFlatBindingPlan<'_>,
        mut limits: MemoryProcedureLimits,
    ) -> Result<Self, MemoryProcedureError> {
        limits.max_work_per_call.get_or_insert(MAX_WORK_PER_CALL);
        Self::from_bound_graph_impl(graph, binding, limits, true)
    }

    fn from_bound_graph_impl(
        graph: &StaticCfg<'_>,
        binding: &DosFlatBindingPlan<'_>,
        limits: MemoryProcedureLimits,
        computations: bool,
    ) -> Result<Self, MemoryProcedureError> {
        if graph.binding().is_none() || !graph.matches_binding(binding) {
            return Err(MemoryProcedureError::DifferentBinding);
        }
        if graph.nodes().len() > limits.max_input_nodes
            || graph.edges().len() > limits.max_input_edges
            || graph.overlaps().len() > limits.max_input_overlaps
            || limits.max_nodes_per_procedure == 0
            || limits.max_nodes_per_procedure > MAX_PROCEDURE_BLOCKS
            || limits.max_procedures > MAX_PROCEDURES
            || limits.max_total_nodes > MAX_TOTAL_PROCEDURE_BLOCKS
            || limits
                .max_work_per_call
                .is_some_and(|n| !(1..=MAX_WORK_PER_CALL).contains(&n))
        {
            return Err(MemoryProcedureError::InputLimit);
        }
        let mut budget = Budget(limits.max_owned_bytes);
        let virtual_registers = if computations {
            super::mutable_sources::discover_virtual_registers(graph, binding, &mut budget)?
        } else {
            super::mutable_sources::VirtualRegisters::default()
        };
        let loop_root = |edge: &crate::cfg::CfgEdge| {
            limits.max_work_per_call.is_some()
                && edge.kind == EdgeKind::Branch
                && matches!(edge.target, EdgeTarget::Address(target)
                    if target.object_number == edge.source.object_number
                        && target.offset <= edge.source.offset)
        };
        let root_count = graph
            .entries()
            .len()
            .checked_add(if computations { graph.nodes().len() } else { 0 })
            .ok_or(MemoryProcedureError::InputLimit)?
            .checked_add(
                graph
                    .edges()
                    .iter()
                    .filter(|edge| {
                        (edge.kind == EdgeKind::Call
                            && matches!(edge.target, EdgeTarget::Address(_)))
                            || loop_root(edge)
                    })
                    .count(),
            )
            .filter(|&n| n <= limits.max_root_occurrences)
            .ok_or(MemoryProcedureError::InputLimit)?;
        let mut roots = Vec::new();
        budget.reserve(&mut roots, root_count)?;
        roots.extend_from_slice(graph.entries());
        roots.extend(
            graph
                .edges()
                .iter()
                .filter_map(|edge| match (edge.kind, edge.target) {
                    (EdgeKind::Call, EdgeTarget::Address(address)) => Some(address),
                    _ => None,
                }),
        );
        let mut loop_entries = Vec::new();
        budget.reserve(
            &mut loop_entries,
            graph.edges().iter().filter(|edge| loop_root(edge)).count(),
        )?;
        loop_entries.extend(graph.edges().iter().filter_map(|edge| {
            if loop_root(edge) {
                match edge.target {
                    EdgeTarget::Address(address) => Some(address),
                    _ => None,
                }
            } else {
                None
            }
        }));
        loop_entries.sort_unstable();
        loop_entries.dedup();
        roots.extend_from_slice(&loop_entries);
        if computations {
            // A setup instruction or a call can be interpreted separately
            // from a subsequent finite computation. Discover its ordinary
            // continuation from decoded control flow, never from addresses
            // supplied by a game-specific list.
            for (index, node) in graph.nodes().iter().enumerate() {
                if node.bitness != DefaultBitness::Bits32 {
                    continue;
                }
                let decoded = decode_node(node, graph, binding)?;
                if !virtual_registers.is_code_writer(index)
                    && !matches!(
                        lower_instruction(&decoded, node, graph, binding, true),
                        Lowered::Fallback | Lowered::Call { .. }
                    )
                {
                    continue;
                }
                let start = graph
                    .edges()
                    .partition_point(|edge| edge.source < node.address);
                if let Some(next) = graph.edges()[start..]
                    .iter()
                    .take_while(|edge| edge.source == node.address)
                    .find_map(|edge| match (edge.kind, edge.target) {
                        (EdgeKind::Fallthrough, EdgeTarget::Address(target)) => Some(target),
                        _ => None,
                    })
                {
                    roots.push(next);
                }
            }
        }
        roots.sort_unstable();
        roots.dedup();
        let mut stamps = Vec::<usize>::new();
        let mut positions = Vec::<usize>::new();
        let mut overlaps = Vec::<bool>::new();
        budget.reserve(&mut stamps, graph.nodes().len())?;
        budget.reserve(&mut positions, graph.nodes().len())?;
        budget.reserve(&mut overlaps, graph.nodes().len())?;
        stamps.resize(graph.nodes().len(), 0);
        positions.resize(graph.nodes().len(), 0);
        overlaps.resize(graph.nodes().len(), false);
        let node_index = |address| {
            graph
                .nodes()
                .binary_search_by_key(&address, |node| node.address)
                .ok()
        };
        for overlap in graph.overlaps() {
            for address in [overlap.first, overlap.second] {
                if let Some(index) = node_index(address) {
                    overlaps[index] = true;
                }
            }
        }
        let mut result = Self {
            program: ProcedureGraphProgram::default(),
            entries: Vec::new(),
            sources: Vec::new(),
            diagnostics: Vec::new(),
            max_work_per_call: limits.max_work_per_call,
        };
        let capacity = roots.len().min(limits.max_procedures);
        budget.reserve(&mut result.program.functions, capacity)?;
        budget.reserve(&mut result.entries, capacity)?;
        budget.reserve(&mut result.sources, capacity)?;
        budget.reserve(&mut result.diagnostics, roots.len())?;
        let mut scans = 0usize;
        let mut total_nodes = 0usize;
        for (root_number, &entry) in roots.iter().enumerate() {
            let loop_fragment = computations
                && loop_entries.binary_search(&entry).is_ok()
                && cyclic_entry(
                    graph,
                    entry,
                    limits.max_nodes_per_procedure,
                    &mut scans,
                    limits.max_scan_steps,
                )?;
            let epoch = root_number
                .checked_add(1)
                .ok_or(MemoryProcedureError::InputLimit)?;
            let mut work = Vec::<usize>::new();
            let mut records = Vec::<(Lowered, [Option<usize>; 2])>::new();
            budget.reserve(&mut work, limits.max_nodes_per_procedure)?;
            budget.reserve(&mut records, limits.max_nodes_per_procedure)?;
            let mut rejection = None;
            if let Some(index) = node_index(entry) {
                stamps[index] = epoch;
                positions[index] = 0;
                work.push(index);
            } else {
                rejection = Some((entry, MemoryProcedureRejection::MissingInstruction));
            }
            let mut cursor = 0;
            while cursor < work.len() {
                scans = scans
                    .checked_add(1)
                    .filter(|&n| n <= limits.max_scan_steps)
                    .ok_or(MemoryProcedureError::ScanLimit)?;
                let index = work[cursor];
                let node = &graph.nodes()[index];
                let reason = if node.bitness != DefaultBitness::Bits32 {
                    Some(MemoryProcedureRejection::UnsupportedMode)
                } else if overlaps[index] {
                    Some(MemoryProcedureRejection::Overlap)
                } else {
                    None
                };
                if let Some(reason) = reason {
                    rejection = Some((node.address, reason));
                    break;
                }
                let (decoded, offsets, ip) = decode_node_with_offsets(node, graph, binding)?;
                let mut lowered = lower_instruction_with_virtual_registers(
                    &decoded,
                    &offsets,
                    ip,
                    node,
                    graph,
                    binding,
                    true,
                    &virtual_registers,
                );
                if computations
                    && ((loop_fragment && node.address < entry)
                        || virtual_registers.is_code_writer(index)
                        || matches!(lowered, Lowered::Fallback | Lowered::Call { .. }))
                {
                    lowered = Lowered::Fallback;
                }
                let targets = match lowered {
                    Lowered::Operation(_) if node.flow == InstructionFlow::Next => {
                        let start = graph
                            .edges()
                            .partition_point(|edge| edge.source < node.address);
                        let next = graph.edges()[start..]
                            .iter()
                            .take_while(|edge| edge.source == node.address)
                            .find_map(|edge| match (edge.kind, edge.target) {
                                (EdgeKind::Fallthrough, EdgeTarget::Address(target)) => {
                                    Some(target)
                                }
                                _ => None,
                            });
                        if let Some(next) = next {
                            [Some(next), None]
                        } else {
                            rejection =
                                Some((node.address, MemoryProcedureRejection::UnresolvedFlow));
                            break;
                        }
                    }
                    Lowered::Jump(target) => [Some(target), None],
                    Lowered::Branch {
                        taken, not_taken, ..
                    } => [Some(taken), Some(not_taken)],
                    Lowered::Return { .. } => [None, None],
                    Lowered::Fallback if computations => [None, None],
                    Lowered::Call { .. } => {
                        rejection = Some((node.address, MemoryProcedureRejection::Call));
                        break;
                    }
                    _ => {
                        rejection = Some((
                            node.address,
                            MemoryProcedureRejection::UnsupportedInstruction,
                        ));
                        break;
                    }
                };
                let mut successors = [None, None];
                for (slot, target) in targets.into_iter().enumerate() {
                    let Some(target) = target else {
                        continue;
                    };
                    let Some(next) = node_index(target) else {
                        rejection =
                            Some((node.address, MemoryProcedureRejection::MissingInstruction));
                        break;
                    };
                    if stamps[next] != epoch {
                        if work.len() == limits.max_nodes_per_procedure {
                            rejection =
                                Some((node.address, MemoryProcedureRejection::InstructionBound));
                            break;
                        }
                        stamps[next] = epoch;
                        positions[next] = work.len();
                        work.push(next);
                    }
                    successors[slot] = Some(positions[next]);
                }
                if rejection.is_some() {
                    break;
                }
                records.push((lowered, successors));
                cursor += 1;
            }
            // Cycles remain rejected unless the owner explicitly selects
            // bounded compilation and event-free per-call reservations.
            if rejection.is_none() && limits.max_work_per_call.is_none() && !computations {
                let mut incoming = Vec::<u16>::new();
                let mut ready = Vec::<usize>::new();
                budget.reserve(&mut incoming, work.len())?;
                budget.reserve(&mut ready, work.len())?;
                incoming.resize(work.len(), 0);
                for (_, targets) in &records {
                    for &target in targets.iter().flatten() {
                        incoming[target] = incoming[target]
                            .checked_add(1)
                            .ok_or(MemoryProcedureError::InputLimit)?;
                    }
                }
                ready.extend(
                    incoming
                        .iter()
                        .enumerate()
                        .filter_map(|(i, &degree)| (degree == 0).then_some(i)),
                );
                let mut consumed = 0;
                while consumed < ready.len() {
                    for &target in records[ready[consumed]].1.iter().flatten() {
                        incoming[target] -= 1;
                        if incoming[target] == 0 {
                            ready.push(target);
                        }
                    }
                    consumed += 1;
                }
                if consumed != work.len() {
                    rejection = Some((entry, MemoryProcedureRejection::Cycle));
                }
            }
            if let Some((instruction, reason)) = rejection {
                result.diagnostics.push(MemoryProcedureDiagnostic {
                    entry,
                    instruction,
                    reason,
                });
                continue;
            }
            let mut blocks = Vec::new();
            let mut sources = Vec::new();
            budget.reserve(&mut blocks, work.len())?;
            budget.reserve(&mut sources, work.len())?;
            for (local, &index) in work.iter().enumerate() {
                let node = &graph.nodes()[index];
                let source = live_address(node.address, binding)?;
                sources.push(node.address);
                let mut instructions = Vec::new();
                let (terminator_address, kind) = match records[local].0 {
                    Lowered::Operation(operation) => {
                        budget.reserve(&mut instructions, 1)?;
                        instructions.push(ProcedureInstruction {
                            address: source,
                            operation,
                        });
                        let next = live_address(
                            graph.nodes()[work[records[local].1[0].expect("validated successor")]]
                                .address,
                            binding,
                        )?;
                        (next, ProcedureTerminatorKind::Fallthrough(next))
                    }
                    Lowered::Jump(target) => (
                        source,
                        ProcedureTerminatorKind::Jump(live_address(target, binding)?),
                    ),
                    Lowered::Branch {
                        condition,
                        taken,
                        not_taken,
                    } => (
                        source,
                        ProcedureTerminatorKind::Branch {
                            condition,
                            taken: live_address(taken, binding)?,
                            not_taken: live_address(not_taken, binding)?,
                        },
                    ),
                    Lowered::Return { width, pop_bytes } => {
                        (source, ProcedureTerminatorKind::Return { width, pop_bytes })
                    }
                    Lowered::Fallback if computations => (source, ProcedureTerminatorKind::Exit),
                    _ => unreachable!("validated leaf instruction"),
                };
                blocks.push(ProcedureBlock {
                    entry: source,
                    instructions,
                    terminator: ProcedureTerminator {
                        address: terminator_address,
                        kind,
                    },
                });
            }
            let id = ProcedureId(result.entries.len() as u32);
            let function = ProcedureGraphFunction {
                id,
                entry: live_address(entry, binding)?,
                blocks,
            };
            let fragment = function
                .blocks
                .iter()
                .any(|block| matches!(block.terminator.kind, ProcedureTerminatorKind::Exit));
            if fragment {
                // Tiny prefixes ending at another unsupported instruction
                // do not warrant an accelerated entry. A fragment must cover
                // an actual cycle and independently prove finite execution.
                let mut incoming = [0usize; MAX_PROCEDURE_BLOCKS];
                let mut queue = [0usize; MAX_PROCEDURE_BLOCKS];
                for (_, targets) in &records {
                    for &target in targets.iter().flatten() {
                        incoming[target] += 1;
                    }
                }
                let mut ready = 0;
                for (index, &degree) in incoming[..records.len()].iter().enumerate() {
                    if degree == 0 {
                        queue[ready] = index;
                        ready += 1;
                    }
                }
                let mut removed = 0;
                while removed < ready {
                    for &target in records[queue[removed]].1.iter().flatten() {
                        incoming[target] -= 1;
                        if incoming[target] == 0 {
                            queue[ready] = target;
                            ready += 1;
                        }
                    }
                    removed += 1;
                }
                let cyclic = removed != records.len();
                if !cyclic || function.finite_maximum_work().is_err() {
                    result.diagnostics.push(MemoryProcedureDiagnostic {
                        entry,
                        instruction: entry,
                        reason: MemoryProcedureRejection::UnsupportedLoop,
                    });
                    continue;
                }
            }
            if let Some(limit) = limits.max_work_per_call {
                // Charge the CPU validator's documented cumulative scratch
                // bound before allocating its graph plan.
                let scratch = work
                    .len()
                    .checked_mul(16 * size_of::<usize>() + 2)
                    .ok_or(LoweringError::OwnedByteLimit)?;
                budget.0 = budget
                    .0
                    .checked_sub(scratch)
                    .ok_or(LoweringError::OwnedByteLimit)?;
                match function.validate_with_work_limit(limit) {
                    Ok(()) => {}
                    Err(ProcedureCompileError::CyclicGraph(_))
                        if computations
                            && (function.recognize_rle_kernel().is_some()
                                || function.finite_maximum_work().is_ok()) => {}
                    Err(ProcedureCompileError::CyclicGraph(_)) => {
                        result.diagnostics.push(MemoryProcedureDiagnostic {
                            entry,
                            instruction: entry,
                            reason: MemoryProcedureRejection::UnsupportedLoop,
                        });
                        continue;
                    }
                    Err(error) => return Err(MemoryProcedureError::Compilation(error)),
                }
            }
            total_nodes = total_nodes
                .checked_add(work.len())
                .filter(|&n| n <= limits.max_total_nodes)
                .ok_or(MemoryProcedureError::InputLimit)?;
            if result.entries.len() == limits.max_procedures {
                return Err(MemoryProcedureError::InputLimit);
            }
            result.program.functions.push(function);
            result.entries.push(entry);
            result.sources.push(sources);
        }
        Ok(result)
    }
    pub fn program(&self) -> &ProcedureGraphProgram {
        &self.program
    }
    pub fn entries(&self) -> &[ObjectAddress] {
        &self.entries
    }
    pub fn sources(&self, index: usize) -> Option<&[ObjectAddress]> {
        self.sources.get(index).map(Vec::as_slice)
    }
    pub fn diagnostics(&self) -> &[MemoryProcedureDiagnostic] {
        &self.diagnostics
    }
    pub fn max_work_per_call(&self) -> Option<u32> {
        self.max_work_per_call
    }
}

/// A backward branch target can be a shared error epilogue rather than a loop.
/// Only cut an enclosing region when this entry actually reaches itself without
/// a call/return or runtime boundary. Bounded setup scratch preserves ordinary
/// procedures whose epilogues happen to live below their entry address.
fn cyclic_entry(
    graph: &StaticCfg<'_>,
    entry: ObjectAddress,
    limit: usize,
    scans: &mut usize,
    max_scans: usize,
) -> Result<bool, MemoryProcedureError> {
    let index = |address| {
        graph
            .nodes()
            .binary_search_by_key(&address, |node| node.address)
            .ok()
    };
    let Some(first) = index(entry) else {
        return Ok(false);
    };
    let mut pending = [0usize; MAX_PROCEDURE_BLOCKS];
    pending[0] = first;
    let mut length = 1;
    let mut cursor = 0;
    while cursor < length {
        *scans = scans
            .checked_add(1)
            .filter(|&count| count <= max_scans)
            .ok_or(MemoryProcedureError::ScanLimit)?;
        let node = &graph.nodes()[pending[cursor]];
        cursor += 1;
        if !matches!(
            node.flow,
            InstructionFlow::Next
                | InstructionFlow::ConditionalBranch
                | InstructionFlow::DirectJump
        ) {
            continue;
        }
        let start = graph
            .edges()
            .partition_point(|edge| edge.source < node.address);
        for edge in graph.edges()[start..]
            .iter()
            .take_while(|edge| edge.source == node.address)
        {
            if !matches!(edge.kind, EdgeKind::Fallthrough | EdgeKind::Branch) {
                continue;
            }
            let EdgeTarget::Address(target) = edge.target else {
                continue;
            };
            if target == entry {
                return Ok(true);
            }
            let Some(next) = index(target) else {
                continue;
            };
            if pending[..length].contains(&next) {
                continue;
            }
            if length == limit {
                return Ok(false);
            }
            pending[length] = next;
            length += 1;
        }
    }
    Ok(false)
}

fn live_address(
    address: ObjectAddress,
    binding: &DosFlatBindingPlan<'_>,
) -> Result<Address, MemoryProcedureError> {
    let offset = binding
        .object_binding(address.object_number)
        .and_then(|object| object.linear_base.checked_add(address.offset))
        .ok_or(MemoryProcedureError::InputLimit)?;
    // Bound near transfers retain flat CS. Object selectors are not code spaces.
    Ok(Address { space: 0, offset })
}
