//! Conservative, profile-independent candidates for short memory wait loops.
//!
//! A candidate is a source-layout and instruction-effects hint, never evidence
//! of inactivity or permission to skip instructions. In particular, stores and
//! useful work may occur in its body. The runtime must execute and compare CPU
//! state and all RAM effects, validate actual code, mappings and device/IRQ
//! boundaries, and establish its own bounded event-wait proof before yielding.
//! Profile labels may guide a caller's selection but never establish coverage.

mod evidence;

use alloc::vec::Vec;
use core::{fmt, mem::size_of, ops::Range};
use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, OpKind};

use crate::{
    DefaultBitness, ObjectAddress,
    cfg::{CfgEdge, EdgeKind, EdgeTarget, InstructionFlow, InstructionNode, StaticCfg},
};

pub const MAX_LOOP_INSTRUCTIONS: u8 = 32;
pub const MAX_LOOP_BYTES: u16 = 128;

/// Static predicate dependencies, separate from the broad source-shape hint.
/// None of these variants proves inactivity, an IRQ writer, or safe skipping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitEvidence {
    /// The predicate reads stable memory locations and loop stores are proven
    /// disjoint from those reads. Its register inputs are iteration-invariant.
    ExternalMemory,
    /// The predicate has no memory dependency, or a loop-carried register,
    /// address or overlapping store can supply local progress.
    LocalProgress,
    /// A possible alias, changing memory-derived address, unresolved relocation
    /// or unsupported value dependency prevents the stronger classification.
    Uncertain,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WaitLimits {
    pub max_instructions: usize,
    pub max_edges: usize,
    pub max_overlaps: usize,
    pub max_diagnostics: usize,
    pub max_candidates: usize,
    /// Positive, at most `MAX_LOOP_INSTRUCTIONS`, including the backedge.
    pub max_instructions_per_loop: u8,
    /// Positive, at most `MAX_LOOP_BYTES`, including the backedge's bytes.
    pub max_bytes_per_loop: u16,
    /// Input records visited and candidate instructions examined. Binary-search
    /// comparisons are bounded separately by the input-count limits.
    pub max_scan_steps: usize,
    /// Requested vector storage, including temporary indexes. Allocator
    /// bookkeeping and iced's static decoder tables are excluded.
    pub max_owned_bytes: usize,
}

impl Default for WaitLimits {
    fn default() -> Self {
        Self {
            max_instructions: 1_000_000,
            max_edges: 3_000_000,
            max_overlaps: 1_000_000,
            max_diagnostics: 1_000_000,
            max_candidates: 65_536,
            max_instructions_per_loop: MAX_LOOP_INSTRUCTIONS,
            max_bytes_per_loop: MAX_LOOP_BYTES,
            max_scan_steps: 16_000_000,
            max_owned_bytes: 128 * 1024 * 1024,
        }
    }
}

/// No field of this record establishes that an execution is idle. Even a
/// changing counter or memory-mutating work loop can have this source shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitCandidate {
    pub head: ObjectAddress,
    pub backedge: ObjectAddress,
    pub byte_len: u16,
    pub memory_read_instructions: u8,
    pub memory_write_instructions: u8,
    pub evidence: WaitEvidence,
    /// Two complete iterations from `head`, including their branches. This is
    /// only a suggested reference-execution budget; exhaustion or completion is
    /// never a proof. The runtime chooses and validates its recurrence policy.
    pub recommended_proof_steps: u32,
    nodes: Range<usize>,
    predicate_reads: u32,
}

impl WaitCandidate {
    pub fn instruction_count(&self) -> usize {
        self.nodes.len()
    }
}

/// Borrows the exact immutable graph/image and owns only candidate identities.
/// Candidates are ordered by backedge address, then head, independent of CFG
/// traversal order. Discovery neither seeds new code nor reads profile bytes.
#[derive(Debug)]
pub struct WaitCandidates<'cfg, 'image> {
    graph: &'cfg StaticCfg<'image>,
    candidates: Vec<WaitCandidate>,
    node_indices: Vec<usize>,
}

impl<'cfg, 'image> WaitCandidates<'cfg, 'image> {
    /// Admit a resolved conditional backward edge with a single contiguous,
    /// straight-line body. Internal alternate entries, joins, overlapping or
    /// stopped instruction streams, calls and uncertain effects reject it.
    /// CMP/TEST must supply the branch flags and the body must read memory.
    /// Only explicit scalar integer operations are admitted; other effects are
    /// conservatively left for future discovery. Stores are not an idle claim.
    pub fn discover(graph: &'cfg StaticCfg<'image>, limits: WaitLimits) -> Result<Self, WaitError> {
        if limits.max_instructions_per_loop == 0
            || limits.max_instructions_per_loop > MAX_LOOP_INSTRUCTIONS
        {
            return Err(WaitError::InvalidInstructionBound);
        }
        if limits.max_bytes_per_loop == 0 || limits.max_bytes_per_loop > MAX_LOOP_BYTES {
            return Err(WaitError::InvalidByteBound);
        }
        for (count, limit, resource) in [
            (
                graph.nodes().len(),
                limits.max_instructions,
                WaitResource::Instructions,
            ),
            (graph.edges().len(), limits.max_edges, WaitResource::Edges),
            (
                graph.overlaps().len(),
                limits.max_overlaps,
                WaitResource::Overlaps,
            ),
            (
                graph.diagnostics().len(),
                limits.max_diagnostics,
                WaitResource::Diagnostics,
            ),
        ] {
            if count > limit {
                return Err(WaitError::Limit(resource));
            }
        }
        let mut budget = Budget(limits.max_owned_bytes);
        let mut scan = Scan(limits.max_scan_steps);
        let mut states = Vec::new();
        budget.reserve(&mut states, graph.nodes().len(), WaitStorage::Index)?;
        states.resize(graph.nodes().len(), State::default());
        for &entry in graph.entries() {
            scan.step()?;
            if let Some(index) = node_index(graph, entry) {
                states[index].entry = true;
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
        // Edges are sorted by source; every node's outgoing range is immutable.
        for (index, edge) in graph.edges().iter().enumerate() {
            scan.step()?;
            let source =
                node_index(graph, edge.source).ok_or(WaitError::InvalidInstruction(edge.source))?;
            if states[source].edges.is_empty() {
                states[source].edges.start = index;
            }
            states[source].edges.end = index + 1;
            if let EdgeTarget::Address(address) = edge.target
                && let Some(target) = node_index(graph, address)
            {
                states[target].incoming = states[target].incoming.saturating_add(1).min(2);
            }
        }
        let mut plan = Self {
            graph,
            candidates: Vec::new(),
            node_indices: Vec::new(),
        };
        for edge in graph.edges() {
            scan.step()?;
            let EdgeTarget::Address(head) = edge.target else {
                continue;
            };
            if edge.kind != EdgeKind::Branch
                || head.object_number != edge.source.object_number
                || head.offset >= edge.source.offset
            {
                continue;
            }
            let Some(body) = discover_loop(graph, &states, edge, limits, &mut scan)? else {
                continue;
            };
            if plan.candidates.len() == limits.max_candidates {
                return Err(WaitError::Limit(WaitResource::Candidates));
            }
            let start = plan.node_indices.len();
            // Admitted bodies are disjoint: every interior successor is Next,
            // and a conditional instruction has only one backward branch.
            for &index in &body.indices[..body.len] {
                budget.push(
                    &mut plan.node_indices,
                    index,
                    graph.nodes().len(),
                    WaitStorage::Instructions,
                )?;
            }
            budget.push(
                &mut plan.candidates,
                WaitCandidate {
                    head,
                    backedge: edge.source,
                    byte_len: body.byte_len,
                    memory_read_instructions: body.reads,
                    memory_write_instructions: body.writes,
                    evidence: body.evidence,
                    recommended_proof_steps: body.len as u32 * 2,
                    nodes: start..plan.node_indices.len(),
                    predicate_reads: body.predicate_reads,
                },
                limits.max_candidates.min(graph.nodes().len()),
                WaitStorage::Candidates,
            )?;
        }
        Ok(plan)
    }

    pub fn graph(&self) -> &'cfg StaticCfg<'image> {
        self.graph
    }

    pub fn candidates(&self) -> &[WaitCandidate] {
        &self.candidates
    }

    /// Node indices retain exact source instruction identities in execution
    /// order, including the terminal conditional branch.
    pub fn node_indices(&self, candidate_index: usize) -> Option<&[usize]> {
        let candidate = self.candidates.get(candidate_index)?;
        Some(&self.node_indices[candidate.nodes.clone()])
    }

    pub fn instructions(
        &self,
        candidate_index: usize,
    ) -> Option<impl ExactSizeIterator<Item = &InstructionNode>> {
        Some(
            self.node_indices(candidate_index)?
                .iter()
                .map(|&index| &self.graph.nodes()[index]),
        )
    }

    /// Exact memory-read instruction identities contributing to the terminal
    /// CMP/TEST, including address dependencies. Proven same-iteration local
    /// store forwarding retains the original source reads instead of the
    /// temporary reload. At most `MAX_LOOP_INSTRUCTIONS` records are returned.
    /// Inspect `evidence` before interpreting these identities as stable reads.
    pub fn predicate_read_instructions(
        &self,
        candidate_index: usize,
    ) -> Option<impl Iterator<Item = &InstructionNode>> {
        let candidate = self.candidates.get(candidate_index)?;
        Some(
            self.node_indices(candidate_index)?
                .iter()
                .enumerate()
                .filter(move |(index, _)| candidate.predicate_reads & (1 << index) != 0)
                .map(|(_, &index)| &self.graph.nodes()[index]),
        )
    }

    /// Exact unrelocated source bytes, borrowed from the original image. Fixups
    /// confined to constants may change them at runtime; this window never
    /// substitutes for runtime relocated-byte and mapping validation.
    pub fn source_bytes(&self, candidate_index: usize) -> Option<&'image [u8]> {
        let candidate = self.candidates.get(candidate_index)?;
        let start = usize::try_from(candidate.head.offset).ok()?;
        self.graph
            .image()
            .object(candidate.head.object_number)?
            .bytes()
            .get(start..start.checked_add(usize::from(candidate.byte_len))?)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitStorage {
    Index,
    Candidates,
    Instructions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitResource {
    Instructions,
    Edges,
    Overlaps,
    Diagnostics,
    Candidates,
    ScanSteps,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitError {
    InvalidInstructionBound,
    InvalidByteBound,
    Limit(WaitResource),
    OwnedByteLimit(WaitStorage),
    Allocation(WaitStorage),
    InvalidInstruction(ObjectAddress),
}

impl fmt::Display for WaitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "static wait candidate discovery: {self:?}")
    }
}

impl core::error::Error for WaitError {}

#[derive(Clone, Default)]
struct State {
    edges: Range<usize>,
    incoming: u8,
    entry: bool,
    overlap: bool,
}

struct Body {
    indices: [usize; MAX_LOOP_INSTRUCTIONS as usize],
    len: usize,
    byte_len: u16,
    reads: u8,
    writes: u8,
    evidence: WaitEvidence,
    predicate_reads: u32,
}

fn discover_loop(
    graph: &StaticCfg<'_>,
    states: &[State],
    edge: &CfgEdge,
    limits: WaitLimits,
    scan: &mut Scan,
) -> Result<Option<Body>, WaitError> {
    let EdgeTarget::Address(head) = edge.target else {
        return Ok(None);
    };
    let Some(mut index) = node_index(graph, head) else {
        return Ok(None);
    };
    let backedge_index =
        node_index(graph, edge.source).ok_or(WaitError::InvalidInstruction(edge.source))?;
    let backedge = graph.nodes()[backedge_index];
    if backedge.flow != InstructionFlow::ConditionalBranch {
        return Ok(None);
    }
    let Some(end) = backedge
        .address
        .offset
        .checked_add(u32::from(backedge.byte_len))
    else {
        return Ok(None);
    };
    let byte_len = end - head.offset;
    if byte_len > u32::from(limits.max_bytes_per_loop) {
        return Ok(None);
    }
    scan.step()?;
    let stopped = graph
        .diagnostics()
        .partition_point(|diagnostic| diagnostic.address < head);
    if graph.diagnostics().get(stopped).is_some_and(|diagnostic| {
        diagnostic.address.object_number == head.object_number && diagnostic.address.offset < end
    }) {
        return Ok(None);
    }
    let mut body = Body {
        indices: [0; MAX_LOOP_INSTRUCTIONS as usize],
        len: 0,
        byte_len: byte_len as u16,
        reads: 0,
        writes: 0,
        evidence: WaitEvidence::Uncertain,
        predicate_reads: 0,
    };
    let mut comparison_flags = 0;
    let mut analysis = evidence::Analysis::new();
    loop {
        scan.step()?;
        let node = graph.nodes()[index];
        let state = &states[index];
        if body.len == usize::from(limits.max_instructions_per_loop)
            || state.overlap
            || node.bitness != backedge.bitness
            || (body.len != 0 && (state.entry || state.incoming != 1))
        {
            return Ok(None);
        }
        let instruction = decode(graph, &node)?;
        if instruction.is_privileged()
            || instruction.has_lock_prefix()
            || instruction.has_rep_prefix()
            || instruction.has_repne_prefix()
        {
            return Ok(None);
        }
        body.indices[body.len] = index;
        body.len += 1;
        let edges = &graph.edges()[state.edges.clone()];
        let next = ObjectAddress {
            offset: node
                .address
                .offset
                .checked_add(u32::from(node.byte_len))
                .ok_or(WaitError::InvalidInstruction(node.address))?,
            ..node.address
        };
        if index == backedge_index {
            let flags = instruction.rflags_read();
            let valid_edges = edges.len() == 2
                && edges.iter().any(|value| value == edge)
                && edges.iter().any(|value| {
                    value.kind == EdgeKind::Fallthrough && value.target == EdgeTarget::Address(next)
                });
            let (mut evidence, predicate_reads) = analysis.finish();
            // An unbound immediate or displacement can relocate onto a store.
            // Do not turn raw source constants into an alias proof.
            if evidence == WaitEvidence::ExternalMemory && graph.binding().is_none() {
                let sites = &graph.image().sites;
                let first = sites.partition_point(|site| {
                    site.source.object_number < head.object_number
                        || (site.source.object_number == head.object_number
                            && u64::from(site.source.offset) + u64::from(site.kind.width())
                                <= u64::from(head.offset))
                });
                if sites.get(first).is_some_and(|site| {
                    site.source.object_number == head.object_number && site.source.offset < end
                }) {
                    evidence = WaitEvidence::Uncertain;
                }
            }
            body.evidence = evidence;
            body.predicate_reads = predicate_reads;
            return Ok((instruction.is_jcc_short_or_near()
                && flags != 0
                && flags & comparison_flags == flags
                && valid_edges
                && node_index(graph, next).is_some()
                && body.reads != 0)
                .then_some(body));
        }
        if node.flow != InstructionFlow::Next
            || edges.len() != 1
            || edges[0].kind != EdgeKind::Fallthrough
            || edges[0].target != EdgeTarget::Address(next)
            || next.offset > backedge.address.offset
        {
            return Ok(None);
        }
        let Some(effects) = scalar_effects(&instruction) else {
            return Ok(None);
        };
        body.reads += u8::from(effects.read);
        body.writes += u8::from(effects.write);
        // Each admitted instruction has at most one memory operand; dependency
        // and alias work uses fixed arrays capped by MAX_LOOP_INSTRUCTIONS.
        analysis.visit(&instruction, body.len - 1);
        if matches!(instruction.mnemonic(), Mnemonic::Cmp | Mnemonic::Test) {
            comparison_flags = instruction.rflags_modified() & !instruction.rflags_undefined();
        } else if instruction.rflags_modified() != 0 {
            comparison_flags = 0;
        }
        let Some(next_index) = node_index(graph, next) else {
            return Ok(None);
        };
        index = next_index;
    }
}

fn decode(graph: &StaticCfg<'_>, node: &InstructionNode) -> Result<Instruction, WaitError> {
    let invalid = WaitError::InvalidInstruction(node.address);
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
    let mut window = [0; 15];
    window[..bytes.len()].copy_from_slice(bytes);
    let mut ip = node.address.offset;
    if let Some(binding) = graph.binding() {
        let patches = binding
            .object_patches(node.address.object_number)
            .ok_or(invalid)?;
        let first = patches.partition_point(|patch| {
            u64::from(patch.source.offset) + patch.bytes().len() as u64
                <= u64::from(node.address.offset)
        });
        for patch in &patches[first..] {
            let patch_start = usize::try_from(patch.source.offset).map_err(|_| invalid)?;
            if patch_start >= end {
                break;
            }
            for (index, &byte) in patch.bytes().iter().enumerate() {
                let at = patch_start + index;
                if at >= start && at < end {
                    window[at - start] = byte;
                }
            }
        }
        if node.bitness == DefaultBitness::Bits32 {
            ip = binding
                .object_binding(node.address.object_number)
                .ok_or(invalid)?
                .linear_base
                .wrapping_add(ip);
        }
    }
    let mut decoder = Decoder::with_ip(
        match node.bitness {
            DefaultBitness::Bits16 => 16,
            DefaultBitness::Bits32 => 32,
        },
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

#[derive(Default)]
struct Effects {
    read: bool,
    write: bool,
}

fn scalar_effects(instruction: &Instruction) -> Option<Effects> {
    use Mnemonic as M;
    let mnemonic = instruction.mnemonic();
    if mnemonic == M::Nop && instruction.op_count() == 0 {
        return Some(Effects::default());
    }
    if !matches!(
        mnemonic,
        M::Mov
            | M::Movsx
            | M::Movzx
            | M::Lea
            | M::Cmp
            | M::Test
            | M::Add
            | M::Adc
            | M::Sub
            | M::Sbb
            | M::And
            | M::Or
            | M::Xor
            | M::Inc
            | M::Dec
            | M::Neg
            | M::Not
    ) {
        return None;
    }
    let mut effects = Effects::default();
    for operand in 0..instruction.op_count() {
        match instruction.op_kind(operand) {
            OpKind::Register => {
                let register = instruction.op_register(operand);
                if !register.is_gpr8() && !register.is_gpr16() && !register.is_gpr32() {
                    return None;
                }
            }
            OpKind::Memory => {
                if mnemonic == M::Lea {
                    continue;
                }
                if !matches!(instruction.memory_size().size(), 1 | 2 | 4) {
                    return None;
                }
                let store = operand == 0 && !matches!(mnemonic, M::Cmp | M::Test);
                effects.write |= store;
                effects.read |= !store || mnemonic != M::Mov;
            }
            OpKind::Immediate8
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32 => (),
            _ => return None,
        }
    }
    Some(effects)
}

fn node_index(graph: &StaticCfg<'_>, address: ObjectAddress) -> Option<usize> {
    graph
        .nodes()
        .binary_search_by_key(&address, |node| node.address)
        .ok()
}

struct Scan(usize);
impl Scan {
    fn step(&mut self) -> Result<(), WaitError> {
        self.0 = self
            .0
            .checked_sub(1)
            .ok_or(WaitError::Limit(WaitResource::ScanSteps))?;
        Ok(())
    }
}

struct Budget(usize);
impl Budget {
    fn reserve<T>(
        &mut self,
        values: &mut Vec<T>,
        capacity: usize,
        storage: WaitStorage,
    ) -> Result<(), WaitError> {
        let additional = capacity.saturating_sub(values.capacity());
        let bytes = additional
            .checked_mul(size_of::<T>())
            .ok_or(WaitError::OwnedByteLimit(storage))?;
        self.0 = self
            .0
            .checked_sub(bytes)
            .ok_or(WaitError::OwnedByteLimit(storage))?;
        if additional != 0 {
            values
                .try_reserve_exact(capacity - values.len())
                .map_err(|_| WaitError::Allocation(storage))?;
        }
        Ok(())
    }
    fn push<T>(
        &mut self,
        values: &mut Vec<T>,
        value: T,
        limit: usize,
        storage: WaitStorage,
    ) -> Result<(), WaitError> {
        if values.len() == values.capacity() {
            let needed = values
                .len()
                .checked_add(1)
                .ok_or(WaitError::OwnedByteLimit(storage))?;
            let desired = values.capacity().saturating_mul(2).max(16).min(limit);
            let capacity = desired.min(values.capacity().saturating_add(self.0 / size_of::<T>()));
            if capacity < needed {
                return Err(WaitError::OwnedByteLimit(storage));
            }
            self.reserve(values, capacity, storage)?;
        }
        values.push(value);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
