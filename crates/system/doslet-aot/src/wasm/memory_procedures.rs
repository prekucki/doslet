//! Optional bound-image pass for acyclic or explicitly bounded leaves.
//!
//! Each reachable x86 instruction retains its source identity. The CPU compiler
//! keeps registers live across internal branches and enforces memory proofs.
//! Calls, unresolved control flow and overlapping decode streams decline
//! admission. Cycles require an explicit finite work limit and a supported
//! structured-loop shape in the CPU compiler. This is frozen-image planning,
//! not a live-code proof, a source of permanent speed tiers or a device scheduler.

use super::{
    Budget, Lowered, LoweringError, decode_node_with_offsets, lower_instruction,
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
        MAX_PROCEDURE_BLOCKS, MAX_PROCEDURE_OPERATIONS, MAX_PROCEDURES, MAX_TOTAL_PROCEDURE_BLOCKS,
        MAX_WORK_PER_CALL, ProcedureBlock, ProcedureCompileError, ProcedureGraphFunction,
        ProcedureGraphProgram, ProcedureId, ProcedureInstruction, ProcedureTerminator,
        ProcedureTerminatorKind,
    },
};

const ROOT_FULL: u8 = 1;
const ROOT_STATUS_CONTINUATION: u8 = 2;

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
    /// Conservative reservations: retained output accumulates, and per-root
    /// scratch, reused for every root, counts once at its bound.
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
        Self::from_bound_graph_impl(graph, binding, limits, false, false, None, false)
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
        Self::from_bound_graph_impl(graph, binding, limits, true, false, None, false)
    }

    /// Also retain cyclic fragments ending before an unsupported instruction,
    /// without requiring a proof that the guest loop terminates. The caller
    /// must explicitly provide `limits.max_work_per_call` and compile with that
    /// same work limit. Every such fragment must satisfy the ordinary bounded
    /// loop validator; this does not authorize an atomic computation or extend
    /// the owner's hardware grant. Existing finite computations remain eligible.
    pub fn from_bound_graph_with_bounded_fragments(
        graph: &StaticCfg<'_>,
        binding: &DosFlatBindingPlan<'_>,
        limits: MemoryProcedureLimits,
    ) -> Result<Self, MemoryProcedureError> {
        if limits.max_work_per_call.is_none() {
            return Err(MemoryProcedureError::InputLimit);
        }
        Self::from_bound_graph_impl(graph, binding, limits, true, true, None, false)
    }

    /// Also admit acyclic interpreter-exit fragments whose static longest path
    /// completes at least `minimum_instructions` guest instructions. This is a
    /// setup filter, not a guarantee of live work: shorter branches and memory
    /// refusals still exit precisely. Acyclic calls reserve their entire longest
    /// path under ordinary hardware admission, without runtime quota checks.
    /// Cyclic fragments retain the explicit bounded policy and require the same
    /// caller-provided work limit. Existing APIs and root discovery are unchanged.
    pub fn from_bound_graph_with_long_fragments(
        graph: &StaticCfg<'_>,
        binding: &DosFlatBindingPlan<'_>,
        limits: MemoryProcedureLimits,
        minimum_instructions: u32,
    ) -> Result<Self, MemoryProcedureError> {
        if limits.max_work_per_call.is_none()
            || !(1..=MAX_PROCEDURE_OPERATIONS as u32).contains(&minimum_instructions)
        {
            return Err(MemoryProcedureError::InputLimit);
        }
        Self::from_bound_graph_impl(
            graph,
            binding,
            limits,
            true,
            true,
            Some(minimum_instructions),
            false,
        )
    }

    /// Experimental exact-PC24 procedure discovery. This admits the supported
    /// x87 forms into ordinary bounded regions; the caller must use the matching
    /// PC24 compiler, transfer ABI and runtime guards. Existing APIs stay scalar.
    pub fn from_bound_graph_with_pc24_fragments(
        graph: &StaticCfg<'_>,
        binding: &DosFlatBindingPlan<'_>,
        limits: MemoryProcedureLimits,
        minimum_instructions: u32,
    ) -> Result<Self, MemoryProcedureError> {
        if limits.max_work_per_call.is_none()
            || !(1..=MAX_PROCEDURE_OPERATIONS as u32).contains(&minimum_instructions)
        {
            return Err(MemoryProcedureError::InputLimit);
        }
        Self::from_bound_graph_impl(
            graph,
            binding,
            limits,
            true,
            true,
            Some(minimum_instructions),
            true,
        )
    }

    fn from_bound_graph_impl(
        graph: &StaticCfg<'_>,
        binding: &DosFlatBindingPlan<'_>,
        limits: MemoryProcedureLimits,
        computations: bool,
        bounded_fragments: bool,
        minimum_fragment_instructions: Option<u32>,
        pc24: bool,
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
        let node_index = |address| {
            graph
                .nodes()
                .binary_search_by_key(&address, |node| node.address)
                .ok()
        };
        // Status seeds must preserve complete graph and CALL entry provenance.
        let mut root_kinds = Vec::<u8>::new();
        if pc24 {
            budget.reserve(&mut root_kinds, graph.nodes().len())?;
            root_kinds.resize(graph.nodes().len(), 0);
            for &root in &roots {
                if let Some(index) = node_index(root) {
                    root_kinds[index] |= ROOT_FULL;
                }
            }
        }
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
                let (decoded, offsets, ip) = decode_node_with_offsets(node, graph, binding)?;
                let x87 = pc24
                    .then(|| {
                        super::x87::lower(
                            &decoded,
                            &offsets,
                            ip,
                            node,
                            graph,
                            binding,
                            &virtual_registers,
                        )
                    })
                    .flatten();
                let status = matches!(
                    x87,
                    Some(doslet_x86_wasm::static_program::Operation::X87(
                        doslet_x86_wasm::static_program::X87Instruction {
                            operation: doslet_x86_wasm::static_program::X87Operation::StoreStatusAx,
                            ..
                        }
                    ))
                );
                if !status
                    && !virtual_registers.is_code_writer(index)
                    && (x87.is_some()
                        || !matches!(
                            lower_instruction(&decoded, node, graph, binding, true),
                            Lowered::Fallback | Lowered::Call { .. }
                        ))
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
                    if status && let Some(index) = node_index(next) {
                        root_kinds[index] |= ROOT_STATUS_CONTINUATION;
                    }
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
        // Per-root scratch, reserved once and cleared for every root: large
        // programs have many roots, and their scratch is never retained.
        let mut work = Vec::<usize>::new();
        let mut records = Vec::<(Lowered, [Option<usize>; 2])>::new();
        budget.reserve(&mut work, limits.max_nodes_per_procedure)?;
        budget.reserve(&mut records, limits.max_nodes_per_procedure)?;
        if limits.max_work_per_call.is_some() {
            // The CPU validator's documented scratch bound for one graph plan;
            // each validation frees its scratch before the next root.
            let scratch = limits
                .max_nodes_per_procedure
                .checked_mul(16 * size_of::<usize>() + 2)
                .ok_or(LoweringError::OwnedByteLimit)?;
            budget.0 = budget
                .0
                .checked_sub(scratch)
                .ok_or(LoweringError::OwnedByteLimit)?;
        }
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
            let retry_status = pc24
                && !loop_fragment
                && node_index(entry).is_some_and(|index| {
                    root_kinds[index] & (ROOT_FULL | ROOT_STATUS_CONTINUATION)
                        == ROOT_STATUS_CONTINUATION
                });
            for attempt in 0..=usize::from(retry_status) {
                if attempt != 0
                    && result
                        .diagnostics
                        .last()
                        .is_some_and(|diagnostic| diagnostic.entry == entry)
                {
                    result.diagnostics.pop();
                }
                let entry_floor = loop_fragment || attempt != 0;
                let allow_prefix_salvage = !retry_status || attempt != 0;
                let epoch = root_number
                    .checked_mul(2)
                    .and_then(|number| number.checked_add(attempt + 1))
                    .ok_or(MemoryProcedureError::InputLimit)?;
                work.clear();
                records.clear();
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
                    if pc24
                        && matches!(lowered, Lowered::Fallback)
                        && let Some(operation) = super::x87::lower(
                            &decoded,
                            &offsets,
                            ip,
                            node,
                            graph,
                            binding,
                            &virtual_registers,
                        )
                    {
                        lowered = Lowered::Operation(operation);
                    }
                    if computations
                        && ((entry_floor && node.address < entry)
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
                                rejection = Some((
                                    node.address,
                                    MemoryProcedureRejection::InstructionBound,
                                ));
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
                if matches!(
                    rejection,
                    Some((_, MemoryProcedureRejection::InstructionBound))
                ) && pc24
                    && allow_prefix_salvage
                    && let Some(minimum) = minimum_fragment_instructions
                    && salvage_prefix(
                        graph,
                        &mut work,
                        &mut records,
                        minimum,
                        limits
                            .max_work_per_call
                            .expect("PC24 requires a work limit"),
                        &mut scans,
                        limits.max_scan_steps,
                    )?
                    .is_some()
                {
                    rejection = None;
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
                                graph.nodes()
                                    [work[records[local].1[0].expect("validated successor")]]
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
                        Lowered::Fallback if computations => {
                            (source, ProcedureTerminatorKind::Exit)
                        }
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
                let mut function = ProcedureGraphFunction {
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
                    // do not warrant an accelerated entry. Legacy APIs require a
                    // cycle; long-fragment opt-in can instead admit an acyclic DAG
                    // by its longest actual instruction path (Exit/fallthrough
                    // cost zero). Reuse the topological scratch for that bound.
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
                    let admitted = if cyclic {
                        bounded_fragments || function.finite_maximum_work().is_ok()
                    } else if let Some(minimum) = minimum_fragment_instructions {
                        for &index in queue[..removed].iter().rev() {
                            let block = &function.blocks[index];
                            let own = block.instructions.len()
                                + usize::from(!matches!(
                                    block.terminator.kind,
                                    ProcedureTerminatorKind::Exit
                                        | ProcedureTerminatorKind::Fallthrough(_)
                                ));
                            incoming[index] = own
                                + records[index]
                                    .1
                                    .iter()
                                    .flatten()
                                    .map(|&target| incoming[target])
                                    .max()
                                    .unwrap_or(0);
                        }
                        incoming[0] >= minimum as usize
                    } else {
                        false
                    };
                    if !admitted {
                        result.diagnostics.push(MemoryProcedureDiagnostic {
                            entry,
                            instruction: entry,
                            reason: MemoryProcedureRejection::UnsupportedLoop,
                        });
                        continue;
                    }
                }
                if let Some(limit) = limits.max_work_per_call {
                    // The validator's scratch, charged once below its bound.
                    let has_x87 = function.blocks.iter().any(|block| {
                        block.instructions.iter().any(|instruction| {
                            matches!(
                                instruction.operation,
                                doslet_x86_wasm::static_program::Operation::X87(_)
                            )
                        })
                    });
                    let mut validation = if pc24 && has_x87 {
                        function.validate_with_pc24_work_limit(limit)
                    } else {
                        function.validate_with_work_limit(limit)
                    };
                    // A valid straight prefix remains useful when a later CFG or
                    // x87 TOP join is unsupported. Existing scalar APIs and every
                    // already accepted candidate retain their original behavior.
                    let semantic = matches!(validation, Err(ProcedureCompileError::CyclicGraph(_)))
                        && computations
                        && !(fragment && bounded_fragments)
                        && (function.recognize_rle_kernel().is_some()
                            || function.finite_maximum_work().is_ok());
                    if validation.is_err()
                        && !semantic
                        && pc24
                        && allow_prefix_salvage
                        && let Some(minimum) = minimum_fragment_instructions
                        && let Some(count) = salvage_prefix(
                            graph,
                            &mut work,
                            &mut records,
                            minimum,
                            limit,
                            &mut scans,
                            limits.max_scan_steps,
                        )?
                    {
                        sources.truncate(count + 1);
                        function.blocks.truncate(count + 1);
                        let boundary = &mut function.blocks[count];
                        boundary.instructions.clear();
                        boundary.terminator = ProcedureTerminator {
                            address: boundary.entry,
                            kind: ProcedureTerminatorKind::Exit,
                        };
                        validation = function.validate_with_pc24_work_limit(limit);
                    }
                    match validation {
                        Ok(()) => {}
                        Err(ProcedureCompileError::CyclicGraph(_))
                            if computations
                                && !(fragment && bounded_fragments)
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
                        Err(_) if has_x87 => {
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
                break;
            }
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

/// Retain only consecutive original instructions before the first control
/// transfer. The existing next node becomes a guarded, zero-work interpreter
/// exit. Reuse reserved per-root scratch, including when the node bound left
/// that boundary without a completed lowering record.
fn salvage_prefix(
    graph: &StaticCfg<'_>,
    work: &mut Vec<usize>,
    records: &mut Vec<(Lowered, [Option<usize>; 2])>,
    minimum: u32,
    work_limit: u32,
    scans: &mut usize,
    max_scans: usize,
) -> Result<Option<usize>, MemoryProcedureError> {
    let mut count = 0;
    while count < work_limit as usize {
        *scans = scans
            .checked_add(1)
            .filter(|&count| count <= max_scans)
            .ok_or(MemoryProcedureError::ScanLimit)?;
        let Some(&(Lowered::Operation(_), [Some(next), None])) = records.get(count) else {
            break;
        };
        if next != count + 1 || graph.nodes()[work[count]].flow != InstructionFlow::Next {
            break;
        }
        count += 1;
    }
    if count < minimum.max(32) as usize || count >= work.len() {
        return Ok(None);
    }
    work.truncate(count + 1);
    records.truncate(count);
    records.push((Lowered::Fallback, [None, None]));
    Ok(Some(count))
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
