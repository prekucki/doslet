//! Optional bounded call-graph closure over the same local-body discovery.
//!
//! This is an offline eligibility analysis, not runtime admission. The complete
//! guest CALL/RET, ESP, cleanup and memory effects remain in the summaries.
//! Native/WASM calls, removing guest stack writes and mid-procedure fallback
//! need a later emitter and separate escape/deoptimization proofs. Even bytes
//! left below ESP can be observable in an exact saved-state comparison. Live
//! source invalidation, aliases, stack/segment checks and hardware service
//! boundaries remain the consumer's responsibility.
//! Expanded work and ordered effects assume each call returns along its CFG
//! continuation. This analysis does not prove that the guest preserves the
//! return slot/ESP or that the dynamic RET target equals that continuation.

use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallProcedureLimits {
    /// Shared input, local-body, scan and total allocation bounds. Preparation
    /// and wait analysis run once, using this single budget for all stages.
    pub procedures: ProcedureLimits,
    /// Resolved call occurrences in locally eligible bodies, before closure
    /// rejection. Repeated calls count separately.
    pub max_call_edges: usize,
    /// Number of nested calls; a leaf has depth zero. Limits affect eligibility,
    /// never the host call stack: SCC discovery is iterative.
    pub max_call_depth: u16,
    /// Per-procedure expanded guest work, including every CALL/RET and every
    /// repeated callee invocation. Checked addition precedes publication.
    pub max_expanded_instructions: u64,
}

impl Default for CallProcedureLimits {
    fn default() -> Self {
        Self {
            procedures: ProcedureLimits::default(),
            max_call_edges: 1_000_000,
            max_call_depth: 64,
            max_expanded_instructions: 1_000_000,
        }
    }
}

/// Effects of structural sequential expansion, including all called bodies,
/// conditional on the guest following each recorded call continuation.
/// Masks use the same byte/architectural-EFLAGS numbering as [`Procedure`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcedureClosureEffects {
    pub input_gpr_bytes: [u8; 8],
    pub modified_gpr_bytes: [u8; 8],
    pub definitely_written_gpr_bytes: [u8; 8],
    pub input_flags: u32,
    pub modified_flags: u32,
    pub definitely_written_flags: u32,
    pub memory_read_instructions: u64,
    pub memory_write_instructions: u64,
    /// Call boundaries conservatively discard stack-relative affine hints.
    /// Callee hint records are not expanded into the caller's local hint list.
    pub stack_reads_uncertain: bool,
}

impl ProcedureClosureEffects {
    fn local(effects: &effects::Effects) -> Self {
        Self {
            input_gpr_bytes: effects.inputs,
            modified_gpr_bytes: effects.modified,
            definitely_written_gpr_bytes: effects.defined,
            input_flags: effects.input_flags,
            modified_flags: effects.modified_flags,
            definitely_written_flags: effects.defined_flags,
            memory_read_instructions: u64::from(effects.reads),
            memory_write_instructions: u64::from(effects.writes),
            stack_reads_uncertain: effects.stack_reads_uncertain,
        }
    }

    fn append(&mut self, next: &Self) -> Option<()> {
        // Do every fallible arithmetic operation before mutating the summary.
        let reads = self
            .memory_read_instructions
            .checked_add(next.memory_read_instructions)?;
        let writes = self
            .memory_write_instructions
            .checked_add(next.memory_write_instructions)?;
        for index in 0..8 {
            self.input_gpr_bytes[index] |=
                next.input_gpr_bytes[index] & !self.definitely_written_gpr_bytes[index];
            self.modified_gpr_bytes[index] |= next.modified_gpr_bytes[index];
            self.definitely_written_gpr_bytes[index] |= next.definitely_written_gpr_bytes[index];
        }
        self.input_flags |= next.input_flags & !self.definitely_written_flags;
        self.modified_flags |= next.modified_flags;
        self.definitely_written_flags |= next.definitely_written_flags;
        self.memory_read_instructions = reads;
        self.memory_write_instructions = writes;
        self.stack_reads_uncertain |= next.stack_reads_uncertain;
        Some(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallProcedure {
    /// Only this body's source instructions and local effects. In particular,
    /// these masks exclude callees; use `closure` for transitive dependencies.
    pub body: Procedure,
    pub closure: ProcedureClosureEffects,
    /// Structural guest-work count, including each repeated invocation. It is
    /// conditional on return/stack checks, not an execution/accounting grant.
    pub expanded_instruction_count: u64,
    pub max_call_depth: u16,
    callsites: Range<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureCall {
    pub instruction: ObjectAddress,
    /// Index into this plan's accepted, closed `procedures()` slice.
    pub callee: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallProcedureDiagnostic {
    pub entry: ObjectAddress,
    pub instruction: ObjectAddress,
    pub reason: CallProcedureRejection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallProcedureRejection {
    Body(ProcedureRejection),
    UnsupportedCallee {
        callee: ObjectAddress,
    },
    /// This procedure belongs to a recursive SCC. A caller outside that SCC
    /// instead receives UnsupportedCallee, even when it reaches the cycle.
    RecursiveCallCycle,
    ReturnWidthMismatch {
        callee: ObjectAddress,
        expected: u8,
        actual: u8,
    },
    CallDepthBound,
    ExpandedInstructionBound,
}

#[derive(Debug)]
pub struct CallProcedurePlan<'cfg, 'image> {
    graph: &'cfg StaticCfg<'image>,
    procedures: Vec<CallProcedure>,
    node_indices: Vec<usize>,
    stack_reads: Vec<StackRead>,
    callsites: Vec<ProcedureCall>,
    topological_order: Vec<usize>,
    diagnostics: Vec<CallProcedureDiagnostic>,
}

impl<'cfg, 'image> CallProcedurePlan<'cfg, 'image> {
    pub fn discover(
        graph: &'cfg StaticCfg<'image>,
        limits: CallProcedureLimits,
    ) -> Result<Self, ProcedureError> {
        let mut scan = Scan(limits.procedures.max_scan_steps);
        let mut budget = Budget(limits.procedures.max_owned_bytes);
        let (states, roots, mut factory) =
            prepare(graph, limits.procedures, &mut scan, &mut budget)?;
        let mut plan = Self {
            graph,
            procedures: Vec::new(),
            node_indices: Vec::new(),
            stack_reads: Vec::new(),
            callsites: Vec::new(),
            topological_order: Vec::new(),
            diagnostics: Vec::new(),
        };
        let mut candidates = Vec::new();
        budget.reserve(
            &mut candidates,
            roots.len(),
            ProcedureStorage::CallCandidates,
        )?;
        let mut edges = Vec::new();
        for &entry in &roots {
            scan.step()?;
            let mut candidate = Candidate::new(entry, edges.len());
            match discover_body(
                graph,
                &states,
                entry,
                limits.procedures,
                true,
                &mut scan,
                &mut factory,
            )? {
                Ok(body) => {
                    let node_start = plan.node_indices.len();
                    let read_start = plan.stack_reads.len();
                    for &index in &body.indices[..body.len] {
                        scan.step()?;
                        budget.push(
                            &mut plan.node_indices,
                            index,
                            limits.procedures.max_instructions,
                            ProcedureStorage::Instructions,
                        )?;
                    }
                    for &read in &body.effects.stack_reads[..body.effects.stack_read_count] {
                        scan.step()?;
                        budget.push(
                            &mut plan.stack_reads,
                            read,
                            limits.procedures.max_instructions,
                            ProcedureStorage::StackReads,
                        )?;
                    }
                    for call in &body.calls[..body.call_count] {
                        scan.step()?;
                        if edges.len() == limits.max_call_edges {
                            return Err(ProcedureError::Limit(ProcedureResource::CallEdges));
                        }
                        // Preparation made every resolved direct-call target a
                        // root. Binary search is bounded by max_roots.
                        let callee = roots
                            .binary_search(&call.callee)
                            .map_err(|_| ProcedureError::InvalidInstruction(call.callee))?;
                        budget.push(
                            &mut edges,
                            CandidateCall {
                                instruction: call.instruction,
                                callee,
                            },
                            limits.max_call_edges,
                            ProcedureStorage::Calls,
                        )?;
                    }
                    candidate.calls.end = edges.len();
                    let effects = body.effects;
                    candidate.body = Some(Procedure {
                        entry,
                        ret: graph.nodes()[body.indices[body.len - 1]].address,
                        byte_len: body.byte_len,
                        return_operand_width: body.return_width,
                        return_pop_bytes: body.pop_bytes,
                        input_gpr_bytes: effects.inputs,
                        modified_gpr_bytes: effects.modified,
                        definitely_written_gpr_bytes: effects.defined,
                        input_flags: effects.input_flags,
                        modified_flags: effects.modified_flags,
                        definitely_written_flags: effects.defined_flags,
                        memory_read_instructions: effects.reads,
                        memory_write_instructions: effects.writes,
                        stack_reads_uncertain: effects.stack_reads_uncertain,
                        nodes: node_start..plan.node_indices.len(),
                        stack_reads: read_start..plan.stack_reads.len(),
                    });
                }
                Err(diagnostic) => candidate.reject(
                    diagnostic.instruction,
                    CallProcedureRejection::Body(diagnostic.reason),
                ),
            }
            // Exact capacity was charged above; one record per deduplicated root.
            candidates.push(candidate);
        }
        let order = strongly_connected_order(&mut candidates, &edges, &mut scan, &mut budget)?;
        for &index in &order {
            scan.step()?;
            if candidates[index].rejection.is_some() {
                continue;
            }
            match compose(
                index,
                &candidates,
                &edges,
                &plan.node_indices,
                graph,
                limits,
                &mut scan,
                &mut factory,
            )? {
                Ok(summary) => candidates[index].summary = Some(summary),
                Err((at, reason)) => candidates[index].reject(at, reason),
            }
        }
        // Publish in root/address order; topology and callsite IDs are remapped
        // only after every accepted candidate has its stable output identity.
        for candidate in &mut candidates {
            scan.step()?;
            if let Some(diagnostic) = candidate.rejection {
                budget.push(
                    &mut plan.diagnostics,
                    diagnostic,
                    limits.procedures.max_roots,
                    ProcedureStorage::Diagnostics,
                )?;
                continue;
            }
            let Some(summary) = candidate.summary else {
                continue;
            };
            if plan.procedures.len() == limits.procedures.max_procedures {
                return Err(ProcedureError::Limit(ProcedureResource::Procedures));
            }
            candidate.output = Some(plan.procedures.len());
            budget.push(
                &mut plan.procedures,
                CallProcedure {
                    body: candidate.body.as_ref().expect("summarized body").clone(),
                    closure: summary.effects,
                    expanded_instruction_count: summary.work,
                    max_call_depth: summary.depth,
                    callsites: 0..0,
                },
                limits.procedures.max_procedures,
                ProcedureStorage::Procedures,
            )?;
        }
        for candidate in &candidates {
            scan.step()?;
            let Some(output) = candidate.output else {
                continue;
            };
            let start = plan.callsites.len();
            for call in &edges[candidate.calls.clone()] {
                scan.step()?;
                budget.push(
                    &mut plan.callsites,
                    ProcedureCall {
                        instruction: call.instruction,
                        callee: candidates[call.callee].output.expect("closed callee"),
                    },
                    limits.max_call_edges,
                    ProcedureStorage::Calls,
                )?;
            }
            plan.procedures[output].callsites = start..plan.callsites.len();
        }
        for index in order {
            scan.step()?;
            if let Some(output) = candidates[index].output {
                budget.push(
                    &mut plan.topological_order,
                    output,
                    limits.procedures.max_procedures,
                    ProcedureStorage::TopologicalOrder,
                )?;
            }
        }
        Ok(plan)
    }

    pub fn graph(&self) -> &'cfg StaticCfg<'image> {
        self.graph
    }
    /// Address-sorted members of the accepted closed call DAG. Every callsite
    /// references another member; no rejected or unresolved dependency is kept.
    pub fn procedures(&self) -> &[CallProcedure] {
        &self.procedures
    }
    pub fn diagnostics(&self) -> &[CallProcedureDiagnostic] {
        &self.diagnostics
    }
    pub fn procedure_index(&self, entry: ObjectAddress) -> Option<usize> {
        self.procedures
            .binary_search_by_key(&entry, |value| value.body.entry)
            .ok()
    }
    /// Deterministic callee-before-caller indexes, including standalone leaves.
    pub fn topological_order(&self) -> &[usize] {
        &self.topological_order
    }
    pub fn node_indices(&self, index: usize) -> Option<&[usize]> {
        Some(&self.node_indices[self.procedures.get(index)?.body.nodes.clone()])
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
    /// Local source reads only. No callee offsets or return-stack restoration
    /// are inferred; hints after a call are conservatively unavailable.
    pub fn stack_reads(&self, index: usize) -> Option<&[StackRead]> {
        Some(&self.stack_reads[self.procedures.get(index)?.body.stack_reads.clone()])
    }
    pub fn callsites(&self, index: usize) -> Option<&[ProcedureCall]> {
        Some(&self.callsites[self.procedures.get(index)?.callsites.clone()])
    }
}

struct Candidate {
    entry: ObjectAddress,
    body: Option<Procedure>,
    calls: Range<usize>,
    rejection: Option<CallProcedureDiagnostic>,
    summary: Option<Summary>,
    output: Option<usize>,
    visit: Option<usize>,
    low: usize,
    on_stack: bool,
}
impl Candidate {
    fn new(entry: ObjectAddress, call_start: usize) -> Self {
        Self {
            entry,
            body: None,
            calls: call_start..call_start,
            rejection: None,
            summary: None,
            output: None,
            visit: None,
            low: 0,
            on_stack: false,
        }
    }
    fn reject(&mut self, instruction: ObjectAddress, reason: CallProcedureRejection) {
        self.rejection = Some(CallProcedureDiagnostic {
            entry: self.entry,
            instruction,
            reason,
        });
    }
}
#[derive(Clone, Copy)]
struct CandidateCall {
    instruction: ObjectAddress,
    callee: usize,
}
#[derive(Clone, Copy)]
struct Summary {
    effects: ProcedureClosureEffects,
    work: u64,
    depth: u16,
}
struct Frame {
    node: usize,
    next: usize,
}

/// Iterative Tarjan walk. Each call edge and SCC member is visited a bounded
/// number of times, and all three scratch vectors are charged before growing.
fn strongly_connected_order(
    candidates: &mut [Candidate],
    edges: &[CandidateCall],
    scan: &mut Scan,
    budget: &mut Budget,
) -> Result<Vec<usize>, ProcedureError> {
    let mut frames = Vec::<Frame>::new();
    let mut stack = Vec::<usize>::new();
    let mut order = Vec::<usize>::new();
    let mut clock = 0;
    for root in 0..candidates.len() {
        scan.step()?;
        if candidates[root].body.is_none() || candidates[root].visit.is_some() {
            continue;
        }
        candidates[root].visit = Some(clock);
        candidates[root].low = clock;
        candidates[root].on_stack = true;
        clock += 1;
        budget.push(
            &mut frames,
            Frame {
                node: root,
                next: candidates[root].calls.start,
            },
            candidates.len(),
            ProcedureStorage::CallGraph,
        )?;
        budget.push(
            &mut stack,
            root,
            candidates.len(),
            ProcedureStorage::CallGraph,
        )?;
        while let Some(frame) = frames.last_mut() {
            scan.step()?;
            let node = frame.node;
            if frame.next < candidates[node].calls.end {
                let callee = edges[frame.next].callee;
                frame.next += 1;
                if candidates[callee].body.is_none() {
                    continue;
                }
                if let Some(visit) = candidates[callee].visit {
                    if candidates[callee].on_stack {
                        candidates[node].low = candidates[node].low.min(visit);
                    }
                } else {
                    candidates[callee].visit = Some(clock);
                    candidates[callee].low = clock;
                    candidates[callee].on_stack = true;
                    clock += 1;
                    budget.push(
                        &mut frames,
                        Frame {
                            node: callee,
                            next: candidates[callee].calls.start,
                        },
                        candidates.len(),
                        ProcedureStorage::CallGraph,
                    )?;
                    budget.push(
                        &mut stack,
                        callee,
                        candidates.len(),
                        ProcedureStorage::CallGraph,
                    )?;
                }
                continue;
            }
            frames.pop();
            if let Some(parent) = frames.last() {
                candidates[parent.node].low = candidates[parent.node].low.min(candidates[node].low);
            }
            if candidates[node].visit == Some(candidates[node].low) {
                let start = order.len();
                loop {
                    scan.step()?;
                    let member = stack.pop().expect("active SCC root");
                    candidates[member].on_stack = false;
                    budget.push(
                        &mut order,
                        member,
                        candidates.len(),
                        ProcedureStorage::CallGraph,
                    )?;
                    if member == node {
                        break;
                    }
                }
                scan.charge(candidates[node].calls.len())?;
                let recursive = order.len() - start > 1
                    || edges[candidates[node].calls.clone()]
                        .iter()
                        .any(|call| call.callee == node);
                if recursive {
                    for &member in &order[start..] {
                        scan.step()?;
                        let entry = candidates[member].entry;
                        candidates[member]
                            .reject(entry, CallProcedureRejection::RecursiveCallCycle);
                    }
                }
            }
        }
    }
    Ok(order)
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn compose(
    index: usize,
    candidates: &[Candidate],
    edges: &[CandidateCall],
    nodes: &[usize],
    graph: &StaticCfg<'_>,
    limits: CallProcedureLimits,
    scan: &mut Scan,
    factory: &mut InstructionInfoFactory,
) -> Result<Result<Summary, (ObjectAddress, CallProcedureRejection)>, ProcedureError> {
    let candidate = &candidates[index];
    let body = candidate.body.as_ref().expect("locally eligible body");
    let mut work = body.instruction_count() as u64;
    let mut depth = 0;
    if work > limits.max_expanded_instructions {
        return Ok(Err((
            candidate.entry,
            CallProcedureRejection::ExpandedInstructionBound,
        )));
    }
    for call in &edges[candidate.calls.clone()] {
        scan.step()?;
        let callee = &candidates[call.callee];
        let Some(summary) = callee.summary else {
            return Ok(Err((
                call.instruction,
                CallProcedureRejection::UnsupportedCallee {
                    callee: callee.entry,
                },
            )));
        };
        let actual = callee
            .body
            .as_ref()
            .expect("summarized callee")
            .return_operand_width;
        if actual != 4 {
            return Ok(Err((
                call.instruction,
                CallProcedureRejection::ReturnWidthMismatch {
                    callee: callee.entry,
                    expected: 4,
                    actual,
                },
            )));
        }
        let Some(next_depth) = summary
            .depth
            .checked_add(1)
            .filter(|&value| value <= limits.max_call_depth)
        else {
            return Ok(Err((
                call.instruction,
                CallProcedureRejection::CallDepthBound,
            )));
        };
        depth = depth.max(next_depth);
        let Some(next_work) = work
            .checked_add(summary.work)
            .filter(|&value| value <= limits.max_expanded_instructions)
        else {
            return Ok(Err((
                call.instruction,
                CallProcedureRejection::ExpandedInstructionBound,
            )));
        };
        work = next_work;
    }
    let mut effects = ProcedureClosureEffects::default();
    let mut local = effects::Effects::new();
    let mut next_call = candidate.calls.start;
    for &node in &nodes[body.nodes.clone()] {
        scan.step()?;
        let node = &graph.nodes()[node];
        let instruction = decode(graph, node, scan)?;
        local.clear_access_summary();
        local.add(&instruction, node.address, factory);
        // Each instruction contributes at most one read/write and the expanded
        // work was checked above; checked additions still defend this invariant.
        let mut added = effects.append(&ProcedureClosureEffects::local(&local));
        if node.flow == InstructionFlow::DirectCall {
            scan.step()?;
            let call = &edges[next_call];
            debug_assert_eq!(call.instruction, node.address);
            next_call += 1;
            added = added.and_then(|()| {
                effects.append(
                    &candidates[call.callee]
                        .summary
                        .expect("closed callee")
                        .effects,
                )
            });
            local.invalidate_call_stack_hints();
            effects.stack_reads_uncertain = true;
        }
        if added.is_none() {
            return Ok(Err((
                node.address,
                CallProcedureRejection::ExpandedInstructionBound,
            )));
        }
    }
    Ok(Ok(Summary {
        effects,
        work,
        depth,
    }))
}

#[cfg(test)]
mod tests;
