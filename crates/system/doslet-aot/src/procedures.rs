//! Bounded offline discovery of contiguous, straight-line integer procedures.
//!
//! Only trusted CFG entries and resolved direct CALL targets are roots. These
//! summaries describe decoded effects, not a C signature, compiler identity,
//! live memory mapping or permission to execute atomically. In particular,
//! arbitrary memory writes still require source-code/alias guards in a consumer.
//! Every branch is excluded, including externally dependent memory waits.

mod calls;
mod effects;
pub use calls::{
    CallProcedure, CallProcedureDiagnostic, CallProcedureLimits, CallProcedurePlan,
    CallProcedureRejection, ProcedureCall, ProcedureClosureEffects,
};

use alloc::vec::Vec;
use core::{fmt, mem::size_of, ops::Range};
use iced_x86::{
    Decoder, DecoderOptions, Instruction, InstructionInfoFactory, UsedMemory, UsedRegister,
};

use crate::{
    DefaultBitness, ObjectAddress,
    cfg::{
        EdgeKind, EdgeTarget, InstructionFlow, InstructionNode, StaticCfg, StopReason,
        UnresolvedReason,
    },
    waits::{WaitCandidates, WaitError, WaitEvidence, WaitLimits},
};

pub const MAX_PROCEDURE_INSTRUCTIONS: u16 = 128;
/// Register indexes in the commonly used Watcom32 argument order. This is only
/// a presentation hint; actual read-before-write masks determine the inputs.
pub const WATCOM32_REGISTER_ORDER: [usize; 4] = [0, 2, 3, 1];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureLimits {
    pub max_instructions: usize,
    pub max_edges: usize,
    pub max_overlaps: usize,
    pub max_cfg_diagnostics: usize,
    /// Root occurrences before deduplication, including all trusted entries.
    pub max_roots: usize,
    pub max_procedures: usize,
    /// Includes the final RET; positive and at most MAX_PROCEDURE_INSTRUCTIONS.
    pub max_instructions_per_procedure: u16,
    /// Input records, candidate instructions and decoded patch records visited.
    /// Binary searches are separately bounded by the input record limits.
    pub max_scan_steps: usize,
    /// Requested storage, including scratch and iced's fixed info-factory
    /// vectors, charged before reservation. Allocator overhead/static tables
    /// are excluded. Half the remaining byte/scan budget is conservatively
    /// reserved once for WaitCandidates, including its temporary indexes.
    pub max_owned_bytes: usize,
}

impl Default for ProcedureLimits {
    fn default() -> Self {
        Self {
            max_instructions: 1_000_000,
            max_edges: 3_000_000,
            max_overlaps: 1_000_000,
            max_cfg_diagnostics: 1_000_000,
            max_roots: 1_000_000,
            max_procedures: 65_536,
            max_instructions_per_procedure: MAX_PROCEDURE_INSTRUCTIONS,
            max_scan_steps: 32_000_000,
            max_owned_bytes: 128 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Procedure {
    pub entry: ObjectAddress,
    pub ret: ObjectAddress,
    /// Contiguous source span beginning at entry, including RET's bytes.
    pub byte_len: u32,
    /// Near return address operand width in bytes, independently of SS.B.
    pub return_operand_width: u8,
    /// Additional RET imm16 cleanup, excluding the popped return address.
    pub return_pop_bytes: u16,
    /// One bit per byte of EAX, ECX, EDX, EBX, ESP, EBP, ESI, EDI.
    /// Reads before a definite write; saved registers can remain inputs.
    pub input_gpr_bytes: [u8; 8],
    /// All possibly modified bytes, including caller-saved/end values.
    pub modified_gpr_bytes: [u8; 8],
    /// Bytes definitely defined by an instruction on this straight-line path.
    /// A possible/conditional write alone does not establish a definition.
    /// Implicit stack updates define only SP without a live SS.B proof.
    pub definitely_written_gpr_bytes: [u8; 8],
    /// Architectural EFLAGS bit positions, NOT iced_x86::RflagsBits. Includes
    /// reads before a definition and retained undefined/conditional outputs.
    /// Entirely untouched flag bits remain owner state outside this mask.
    pub input_flags: u32,
    /// Architectural EFLAGS bit positions; includes undefined flag effects.
    pub modified_flags: u32,
    /// Architectural bits definitely defined, excluding retained undefined and
    /// possibly zero-count flag effects.
    pub definitely_written_flags: u32,
    pub memory_read_instructions: u16,
    pub memory_write_instructions: u16,
    /// Some reads lack a proved entry-stack-relative offset. Stack hints are
    /// conditional on a live 32-bit stack and nonwrapping arithmetic.
    pub stack_reads_uncertain: bool,
    nodes: Range<usize>,
    stack_reads: Range<usize>,
}

impl Procedure {
    pub fn instruction_count(&self) -> usize {
        self.nodes.len()
    }

    /// Ordered register indexes and byte masks, without inferred types or
    /// claims that other input registers are absent.
    pub fn watcom32_register_inputs(&self) -> impl Iterator<Item = (usize, u8)> + '_ {
        WATCOM32_REGISTER_ORDER.into_iter().filter_map(|index| {
            let mask = self.input_gpr_bytes[index];
            (mask != 0).then_some((index, mask))
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackRead {
    pub instruction: ObjectAddress,
    /// Byte offset relative to entry ESP, conditional on live SS.B=32 and no
    /// address wrap. Includes local/saved values and the RET return address;
    /// it is not proof that the read names an argument.
    pub offset: i32,
    pub byte_len: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureDiagnostic {
    pub entry: ObjectAddress,
    pub instruction: ObjectAddress,
    pub reason: ProcedureRejection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureRejection {
    Stopped(StopReason),
    MissingInstruction,
    UnsupportedCodeMode,
    OverlappingInstruction,
    AlternateInteriorEntry,
    NonContiguousFlow,
    ControlFlow(InstructionFlow),
    /// No local progress writer was established. This does not identify a
    /// particular IRQ or authorize idle skipping. The procedure must yield.
    ExternalMemoryWait,
    UncertainMemoryWait,
    UnsupportedInstruction,
    UnsupportedPrefix,
    InstructionBound,
    /// Call-body discovery permits only direct near32 CALLs. Leaf discovery
    /// retains its original ControlFlow rejection for every call form.
    UnresolvedCall(UnresolvedReason),
    IndirectCall,
    FarCall,
    UnsupportedCallWidth,
}

#[derive(Debug)]
pub struct ProcedurePlan<'cfg, 'image> {
    graph: &'cfg StaticCfg<'image>,
    procedures: Vec<Procedure>,
    node_indices: Vec<usize>,
    stack_reads: Vec<StackRead>,
    diagnostics: Vec<ProcedureDiagnostic>,
}

impl<'cfg, 'image> ProcedurePlan<'cfg, 'image> {
    pub fn discover(
        graph: &'cfg StaticCfg<'image>,
        limits: ProcedureLimits,
    ) -> Result<Self, ProcedureError> {
        let mut scan = Scan(limits.max_scan_steps);
        let mut budget = Budget(limits.max_owned_bytes);
        let (states, roots, mut factory) = prepare(graph, limits, &mut scan, &mut budget)?;
        let mut plan = Self {
            graph,
            procedures: Vec::new(),
            node_indices: Vec::new(),
            stack_reads: Vec::new(),
            diagnostics: Vec::new(),
        };
        for entry in roots {
            scan.step()?;
            match discover_body(
                graph,
                &states,
                entry,
                limits,
                false,
                &mut scan,
                &mut factory,
            )? {
                Ok(body) => {
                    if plan.procedures.len() == limits.max_procedures {
                        return Err(ProcedureError::Limit(ProcedureResource::Procedures));
                    }
                    let node_start = plan.node_indices.len();
                    let read_start = plan.stack_reads.len();
                    for &index in &body.indices[..body.len] {
                        budget.push(
                            &mut plan.node_indices,
                            index,
                            limits.max_instructions,
                            ProcedureStorage::Instructions,
                        )?;
                    }
                    for &read in &body.effects.stack_reads[..body.effects.stack_read_count] {
                        budget.push(
                            &mut plan.stack_reads,
                            read,
                            limits.max_instructions,
                            ProcedureStorage::StackReads,
                        )?;
                    }
                    let summary = body.effects;
                    budget.push(
                        &mut plan.procedures,
                        Procedure {
                            entry,
                            ret: graph.nodes()[body.indices[body.len - 1]].address,
                            byte_len: body.byte_len,
                            return_operand_width: body.return_width,
                            return_pop_bytes: body.pop_bytes,
                            input_gpr_bytes: summary.inputs,
                            modified_gpr_bytes: summary.modified,
                            definitely_written_gpr_bytes: summary.defined,
                            input_flags: summary.input_flags,
                            modified_flags: summary.modified_flags,
                            definitely_written_flags: summary.defined_flags,
                            memory_read_instructions: summary.reads,
                            memory_write_instructions: summary.writes,
                            stack_reads_uncertain: summary.stack_reads_uncertain,
                            nodes: node_start..plan.node_indices.len(),
                            stack_reads: read_start..plan.stack_reads.len(),
                        },
                        limits.max_procedures,
                        ProcedureStorage::Procedures,
                    )?;
                }
                Err(diagnostic) => budget.push(
                    &mut plan.diagnostics,
                    diagnostic,
                    limits.max_roots,
                    ProcedureStorage::Diagnostics,
                )?,
            }
        }
        Ok(plan)
    }

    pub fn graph(&self) -> &'cfg StaticCfg<'image> {
        self.graph
    }
    pub fn procedures(&self) -> &[Procedure] {
        &self.procedures
    }
    pub fn diagnostics(&self) -> &[ProcedureDiagnostic] {
        &self.diagnostics
    }
    pub fn node_indices(&self, index: usize) -> Option<&[usize]> {
        Some(&self.node_indices[self.procedures.get(index)?.nodes.clone()])
    }
    pub fn instructions(
        &self,
        index: usize,
    ) -> Option<impl Iterator<Item = &'cfg InstructionNode> + '_> {
        Some(
            self.node_indices(index)?
                .iter()
                .map(|&index| &self.graph.nodes()[index]),
        )
    }
    pub fn stack_reads(&self, index: usize) -> Option<&[StackRead]> {
        Some(&self.stack_reads[self.procedures.get(index)?.stack_reads.clone()])
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureResource {
    Instructions,
    Edges,
    Overlaps,
    CfgDiagnostics,
    Roots,
    Procedures,
    ScanSteps,
    CallEdges,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureStorage {
    Index,
    Roots,
    Instructions,
    Procedures,
    StackReads,
    Diagnostics,
    WaitAnalysis,
    InstructionInfo,
    CallCandidates,
    Calls,
    CallGraph,
    TopologicalOrder,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureError {
    InvalidInstructionBound,
    Limit(ProcedureResource),
    OwnedByteLimit(ProcedureStorage),
    Allocation(ProcedureStorage),
    InvalidInstruction(ObjectAddress),
    WaitAnalysis(WaitError),
}
impl fmt::Display for ProcedureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "static procedure discovery: {self:?}")
    }
}
impl core::error::Error for ProcedureError {}

#[derive(Clone, Default)]
struct State {
    edges: Range<usize>,
    incoming: u8,
    root: bool,
    overlap: bool,
    wait: Option<WaitEvidence>,
}
struct Body {
    indices: [usize; MAX_PROCEDURE_INSTRUCTIONS as usize],
    len: usize,
    byte_len: u32,
    return_width: u8,
    pop_bytes: u16,
    effects: effects::Effects,
    calls: [BodyCall; MAX_PROCEDURE_INSTRUCTIONS as usize],
    call_count: usize,
}

#[derive(Clone, Copy)]
struct BodyCall {
    instruction: ObjectAddress,
    callee: ObjectAddress,
}

impl BodyCall {
    const EMPTY: Self = Self {
        instruction: ObjectAddress {
            object_number: 0,
            offset: 0,
        },
        callee: ObjectAddress {
            object_number: 0,
            offset: 0,
        },
    };
}

fn prepare(
    graph: &StaticCfg<'_>,
    limits: ProcedureLimits,
    scan: &mut Scan,
    budget: &mut Budget,
) -> Result<(Vec<State>, Vec<ObjectAddress>, InstructionInfoFactory), ProcedureError> {
    if limits.max_instructions_per_procedure == 0
        || limits.max_instructions_per_procedure > MAX_PROCEDURE_INSTRUCTIONS
    {
        return Err(ProcedureError::InvalidInstructionBound);
    }
    for (count, bound, resource) in [
        (
            graph.nodes().len(),
            limits.max_instructions,
            ProcedureResource::Instructions,
        ),
        (
            graph.edges().len(),
            limits.max_edges,
            ProcedureResource::Edges,
        ),
        (
            graph.overlaps().len(),
            limits.max_overlaps,
            ProcedureResource::Overlaps,
        ),
        (
            graph.diagnostics().len(),
            limits.max_cfg_diagnostics,
            ProcedureResource::CfgDiagnostics,
        ),
        (
            graph.entries().len(),
            limits.max_roots,
            ProcedureResource::Roots,
        ),
    ] {
        if count > bound {
            return Err(ProcedureError::Limit(resource));
        }
    }
    let mut states = Vec::new();
    budget.reserve(&mut states, graph.nodes().len(), ProcedureStorage::Index)?;
    states.resize(graph.nodes().len(), State::default());
    let mut roots = Vec::new();
    for &entry in graph.entries() {
        scan.step()?;
        budget.push(&mut roots, entry, limits.max_roots, ProcedureStorage::Roots)?;
    }
    for (index, edge) in graph.edges().iter().enumerate() {
        scan.step()?;
        let source = node_index(graph, edge.source)
            .ok_or(ProcedureError::InvalidInstruction(edge.source))?;
        if states[source].edges.is_empty() {
            states[source].edges.start = index;
        }
        states[source].edges.end = index + 1;
        if let EdgeTarget::Address(target) = edge.target {
            if let Some(target_index) = node_index(graph, target) {
                states[target_index].incoming =
                    states[target_index].incoming.saturating_add(1).min(2);
            }
            if edge.kind == EdgeKind::Call
                && graph.nodes()[source].flow == InstructionFlow::DirectCall
            {
                if roots.len() == limits.max_roots {
                    return Err(ProcedureError::Limit(ProcedureResource::Roots));
                }
                budget.push(
                    &mut roots,
                    target,
                    limits.max_roots,
                    ProcedureStorage::Roots,
                )?;
            }
        }
    }
    roots.sort_unstable();
    roots.dedup();
    for &root in &roots {
        scan.step()?;
        if let Some(index) = node_index(graph, root) {
            states[index].root = true;
        }
    }
    for overlap in graph.overlaps() {
        scan.step()?;
        for address in [overlap.first, overlap.second] {
            if let Some(index) = node_index(graph, address) {
                states[index].overlap = true;
            }
        }
    }
    // One bounded wait analysis for all roots, with its scratch allowance
    // charged before it runs. Neither plan reruns it per procedure.
    let wait_bytes = budget.0 / 2;
    budget.charge(wait_bytes, ProcedureStorage::WaitAnalysis)?;
    let wait_steps = scan.0 / 2;
    scan.charge(wait_steps)?;
    let waits = WaitCandidates::discover(
        graph,
        WaitLimits {
            max_instructions: limits.max_instructions,
            max_edges: limits.max_edges,
            max_overlaps: limits.max_overlaps,
            max_diagnostics: limits.max_cfg_diagnostics,
            max_candidates: limits.max_instructions,
            max_scan_steps: wait_steps,
            max_owned_bytes: wait_bytes,
            ..WaitLimits::default()
        },
    )
    .map_err(ProcedureError::WaitAnalysis)?;
    for candidate in waits.candidates() {
        scan.step()?;
        for address in [candidate.head, candidate.backedge] {
            if let Some(index) = node_index(graph, address) {
                states[index].wait = Some(candidate.evidence);
            }
        }
    }
    // iced 1.21.0 creates these reusable capacities. The admitted scalar and
    // direct CALL forms do not grow either vector when info() is called.
    budget.charge(
        10 * size_of::<UsedRegister>() + 8 * size_of::<UsedMemory>(),
        ProcedureStorage::InstructionInfo,
    )?;
    Ok((states, roots, InstructionInfoFactory::new()))
}

#[allow(clippy::too_many_arguments)]
fn discover_body(
    graph: &StaticCfg<'_>,
    states: &[State],
    entry: ObjectAddress,
    limits: ProcedureLimits,
    allow_calls: bool,
    scan: &mut Scan,
    factory: &mut InstructionInfoFactory,
) -> Result<Result<Body, ProcedureDiagnostic>, ProcedureError> {
    let reject = |instruction, reason| {
        Ok(Err(ProcedureDiagnostic {
            entry,
            instruction,
            reason,
        }))
    };
    let Some(mut index) = node_index(graph, entry) else {
        let reason = graph
            .diagnostics()
            .binary_search_by_key(&entry, |value| value.address)
            .ok()
            .map(|index| ProcedureRejection::Stopped(graph.diagnostics()[index].reason))
            .unwrap_or(ProcedureRejection::MissingInstruction);
        return reject(entry, reason);
    };
    let mut body = Body {
        indices: [0; MAX_PROCEDURE_INSTRUCTIONS as usize],
        len: 0,
        byte_len: 0,
        return_width: 0,
        pop_bytes: 0,
        effects: effects::Effects::new(),
        calls: [BodyCall::EMPTY; MAX_PROCEDURE_INSTRUCTIONS as usize],
        call_count: 0,
    };
    loop {
        scan.step()?;
        let node = &graph.nodes()[index];
        let address = node.address;
        if body.len == usize::from(limits.max_instructions_per_procedure) {
            return reject(address, ProcedureRejection::InstructionBound);
        }
        if node.bitness != DefaultBitness::Bits32 {
            return reject(address, ProcedureRejection::UnsupportedCodeMode);
        }
        if states[index].overlap {
            return reject(address, ProcedureRejection::OverlappingInstruction);
        }
        match states[index].wait {
            Some(WaitEvidence::ExternalMemory) => {
                return reject(address, ProcedureRejection::ExternalMemoryWait);
            }
            Some(WaitEvidence::Uncertain) => {
                return reject(address, ProcedureRejection::UncertainMemoryWait);
            }
            _ => (),
        }
        if body.len != 0 && (states[index].root || states[index].incoming != 1) {
            return reject(address, ProcedureRejection::AlternateInteriorEntry);
        }
        let end = address
            .offset
            .checked_add(u32::from(node.byte_len))
            .ok_or(ProcedureError::InvalidInstruction(address))?;
        let stopped = graph
            .diagnostics()
            .partition_point(|value| value.address < address);
        if let Some(diagnostic) = graph.diagnostics().get(stopped)
            && diagnostic.address.object_number == address.object_number
            && diagnostic.address.offset < end
        {
            return reject(
                diagnostic.address,
                ProcedureRejection::Stopped(diagnostic.reason),
            );
        }
        if allow_calls {
            match node.flow {
                InstructionFlow::IndirectCall => {
                    return reject(address, ProcedureRejection::IndirectCall);
                }
                InstructionFlow::FarCall => return reject(address, ProcedureRejection::FarCall),
                _ => (),
            }
        }
        let direct_call = allow_calls && node.flow == InstructionFlow::DirectCall;
        if !matches!(node.flow, InstructionFlow::Next | InstructionFlow::Return) && !direct_call {
            let reason = match states[index].wait {
                Some(WaitEvidence::ExternalMemory) => ProcedureRejection::ExternalMemoryWait,
                Some(WaitEvidence::Uncertain) => ProcedureRejection::UncertainMemoryWait,
                _ => ProcedureRejection::ControlFlow(node.flow),
            };
            return reject(address, reason);
        }
        let instruction = decode(graph, node, scan)?;
        if instruction.has_lock_prefix()
            || instruction.has_rep_prefix()
            || instruction.has_repne_prefix()
        {
            return reject(address, ProcedureRejection::UnsupportedPrefix);
        }
        if direct_call {
            if !instruction.is_call_near()
                || instruction.op0_kind() != iced_x86::OpKind::NearBranch32
                || instruction.stack_pointer_increment() != -4
            {
                return reject(address, ProcedureRejection::UnsupportedCallWidth);
            }
            let edges = &graph.edges()[states[index].edges.clone()];
            scan.charge(edges.len())?;
            if edges.len() != 2 {
                return reject(address, ProcedureRejection::NonContiguousFlow);
            }
            let Some(edge) = edges.iter().find(|edge| edge.kind == EdgeKind::Call) else {
                return reject(address, ProcedureRejection::NonContiguousFlow);
            };
            let callee = match edge.target {
                EdgeTarget::Address(callee) => callee,
                EdgeTarget::Unresolved(reason) => {
                    return reject(address, ProcedureRejection::UnresolvedCall(reason));
                }
            };
            body.calls[body.call_count] = BodyCall {
                instruction: address,
                callee,
            };
            body.call_count += 1;
        } else if !effects::supported(&instruction) {
            return reject(address, ProcedureRejection::UnsupportedInstruction);
        }
        body.effects.add(&instruction, address, factory);
        if direct_call {
            body.effects.invalidate_call_stack_hints();
        }
        body.indices[body.len] = index;
        body.len += 1;
        body.byte_len = end - entry.offset;
        if node.flow == InstructionFlow::Return {
            let Some((width, pop_bytes)) = effects::near_return(&instruction) else {
                return reject(address, ProcedureRejection::UnsupportedInstruction);
            };
            body.return_width = width;
            body.pop_bytes = pop_bytes;
            return Ok(Ok(body));
        }
        let edges = &graph.edges()[states[index].edges.clone()];
        scan.charge(edges.len())?;
        let next = ObjectAddress {
            object_number: address.object_number,
            offset: end,
        };
        let continuation = if direct_call {
            EdgeKind::CallContinuation
        } else {
            EdgeKind::Fallthrough
        };
        if edges.len() != if direct_call { 2 } else { 1 }
            || !edges
                .iter()
                .any(|edge| edge.kind == continuation && edge.target == EdgeTarget::Address(next))
        {
            return reject(address, ProcedureRejection::NonContiguousFlow);
        }
        let Some(next_index) = node_index(graph, next) else {
            let reason = graph
                .diagnostics()
                .binary_search_by_key(&next, |value| value.address)
                .ok()
                .map(|index| ProcedureRejection::Stopped(graph.diagnostics()[index].reason))
                .unwrap_or(ProcedureRejection::MissingInstruction);
            return reject(next, reason);
        };
        index = next_index;
    }
}

fn decode(
    graph: &StaticCfg<'_>,
    node: &InstructionNode,
    scan: &mut Scan,
) -> Result<Instruction, ProcedureError> {
    let invalid = ProcedureError::InvalidInstruction(node.address);
    let start = usize::try_from(node.address.offset).map_err(|_| invalid)?;
    let end = start
        .checked_add(usize::from(node.byte_len))
        .ok_or(invalid)?;
    let bytes = graph
        .image()
        .object(node.address.object_number)
        .ok_or(invalid)?
        .bytes()
        .get(start..end)
        .ok_or(invalid)?;
    if bytes.is_empty() || bytes.len() > 15 {
        return Err(invalid);
    }
    let mut window = [0; 15];
    window[..bytes.len()].copy_from_slice(bytes);
    let mut ip = node.address.offset;
    if let Some(binding) = graph.binding() {
        if !graph.matches_binding(binding) {
            return Err(invalid);
        }
        let patches = binding
            .object_patches(node.address.object_number)
            .ok_or(invalid)?;
        let first = patches.partition_point(|patch| {
            u64::from(patch.source.offset) + patch.bytes().len() as u64
                <= u64::from(node.address.offset)
        });
        for patch in &patches[first..] {
            scan.step()?;
            let patch_start = usize::try_from(patch.source.offset).map_err(|_| invalid)?;
            if patch_start >= end {
                break;
            }
            for (index, &byte) in patch.bytes().iter().enumerate() {
                let at = patch_start.checked_add(index).ok_or(invalid)?;
                if at >= start && at < end {
                    window[at - start] = byte;
                }
            }
        }
        ip = binding
            .object_binding(node.address.object_number)
            .ok_or(invalid)?
            .linear_base
            .wrapping_add(ip);
    }
    let mut decoder = Decoder::with_ip(
        32,
        &window[..bytes.len()],
        u64::from(ip),
        DecoderOptions::NONE,
    );
    let instruction = decoder.decode();
    if instruction.is_invalid() || instruction.len() != bytes.len() {
        return Err(invalid);
    }
    Ok(instruction)
}

fn node_index(graph: &StaticCfg<'_>, address: ObjectAddress) -> Option<usize> {
    graph
        .nodes()
        .binary_search_by_key(&address, |node| node.address)
        .ok()
}
struct Scan(usize);
impl Scan {
    fn step(&mut self) -> Result<(), ProcedureError> {
        self.charge(1)
    }
    fn charge(&mut self, steps: usize) -> Result<(), ProcedureError> {
        self.0 = self
            .0
            .checked_sub(steps)
            .ok_or(ProcedureError::Limit(ProcedureResource::ScanSteps))?;
        Ok(())
    }
}
struct Budget(usize);
impl Budget {
    fn charge(&mut self, bytes: usize, storage: ProcedureStorage) -> Result<(), ProcedureError> {
        self.0 = self
            .0
            .checked_sub(bytes)
            .ok_or(ProcedureError::OwnedByteLimit(storage))?;
        Ok(())
    }
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        capacity: usize,
        storage: ProcedureStorage,
    ) -> Result<(), ProcedureError> {
        let additional = capacity.saturating_sub(values.capacity());
        let bytes = additional
            .checked_mul(size_of::<T>())
            .ok_or(ProcedureError::OwnedByteLimit(storage))?;
        self.charge(bytes, storage)?;
        if additional != 0 {
            values
                .try_reserve_exact(capacity - values.len())
                .map_err(|_| ProcedureError::Allocation(storage))?;
        }
        Ok(())
    }
    fn push<T>(
        &mut self,
        values: &mut Vec<T>,
        value: T,
        limit: usize,
        storage: ProcedureStorage,
    ) -> Result<(), ProcedureError> {
        if values.len() == values.capacity() {
            let needed = values
                .len()
                .checked_add(1)
                .ok_or(ProcedureError::OwnedByteLimit(storage))?;
            let desired = values.capacity().saturating_mul(2).max(16).min(limit);
            let capacity = desired.min(values.capacity().saturating_add(self.0 / size_of::<T>()));
            if capacity < needed {
                return Err(ProcedureError::OwnedByteLimit(storage));
            }
            self.reserve(values, capacity, storage)?;
        }
        values.push(value);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
