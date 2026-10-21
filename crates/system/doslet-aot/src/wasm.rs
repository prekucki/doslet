//! Static image/CFG adapter to the format-independent CPU WASM compiler.
//!
//! The caller supplies a complete explicit offline binding plan. Decoding uses
//! original instruction starts and a small patched window, never profile bytes.
//! Unsupported instructions stop before their effects and enter the runtime.
//! Generated code is valid only while the actual code and segment assumptions
//! match this plan; live guards and device scheduling belong to the runtime.

pub mod memory_procedures;
mod mutable_sources;
pub mod procedures;

use alloc::vec::Vec;
use core::{fmt, mem::size_of};
use doslet_x86_wasm::static_program::{
    Accounting, Address, Alu, Condition, DoubleShift, MemoryUnary, Operation, Program, Region,
    RegionId, Register, Shift, ShiftCount, Source, Terminator, Width,
};
use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, OpKind};

use crate::{
    DefaultBitness, ObjectAddress,
    binding::DosFlatBindingPlan,
    cfg::{EdgeKind, EdgeTarget, InstructionNode, StaticCfg},
    profile::{BoundGuidance, GuidanceStatus, InstructionGuidance},
    regions::{RegionError, RegionLimits, RegionPlan},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoweringLimits {
    /// Opt into the shared-memory descriptor ABI and guarded memory/stack effects.
    pub direct_memory: bool,
    /// Split potentially patched source instructions from unchanged neighbors,
    /// using reachable direct stores and profile source-byte conflicts only as
    /// partitioning hints. All exact-byte guards remain mandatory.
    pub isolate_mutable_sources: bool,
    /// Keep scalar register-immediate instructions in separate guarded regions.
    /// Useful when indirect code writes escape the static partitioning hints.
    pub isolate_immediates: bool,
    pub regions: RegionLimits,
    pub max_owned_bytes: usize,
}

impl Default for LoweringLimits {
    fn default() -> Self {
        Self {
            direct_memory: false,
            isolate_mutable_sources: false,
            isolate_immediates: false,
            regions: RegionLimits::default(),
            max_owned_bytes: 256 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoweringReport {
    pub mutable_source_instructions: usize,
    pub isolated_immediates: usize,
    pub memory_loads: usize,
    pub memory_stores: usize,
    pub memory_alu: usize,
    pub memory_updates: usize,
    pub memory_extensions: usize,
    pub memory_multiply: usize,
    pub stack_operations: usize,
    pub discovered_instructions: usize,
    pub register_instructions: usize,
    pub direct_transfers: usize,
    /// Bound full-flat32 near CALLs, each with one guarded return-address push.
    pub direct_calls: usize,
    pub near_returns: usize,
    pub fallback_instructions: usize,
    pub generated_regions: usize,
    pub external_stubs: usize,
}

/// Owns IR and source-node identities, not another copy of guest/object memory.
/// Source identities are retained for future live code/segment guards.
#[derive(Debug)]
pub struct PreparedProgram {
    program: Program,
    sources: Vec<RegionSources>,
    report: LoweringReport,
    guidance: Vec<InstructionGuidance>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegionSources {
    pub id: RegionId,
    nodes: Vec<ObjectAddress>,
}

impl RegionSources {
    pub fn instructions(&self) -> &[ObjectAddress] {
        &self.nodes
    }
}

impl PreparedProgram {
    pub fn from_bound_cfg(
        graph: &StaticCfg<'_>,
        binding: &DosFlatBindingPlan<'_>,
        limits: LoweringLimits,
    ) -> Result<Self, LoweringError> {
        Self::prepare(graph, binding, None, false, &[], limits)
    }

    /// Only verified HOT instructions may use uncounted accounting, and only
    /// with explicit opt-in. Static IRQ observations always remain counted.
    /// Per-instruction labels and observed weights remain available as metadata.
    pub fn from_guided_cfg(
        graph: &StaticCfg<'_>,
        binding: &DosFlatBindingPlan<'_>,
        guidance: &BoundGuidance<'_, '_, '_>,
        uncounted_hot: bool,
        limits: LoweringLimits,
    ) -> Result<Self, LoweringError> {
        if !guidance.matches(graph, binding) {
            return Err(LoweringError::DifferentGuidance);
        }
        Self::prepare(
            graph,
            binding,
            Some(guidance.instructions()),
            uncounted_hot,
            &[],
            limits,
        )
    }

    /// Force already-discovered entries through the runtime before effects.
    /// This is useful for wait candidates, whose safety proof belongs to the
    /// interpreter/runtime. Entries must be sorted, unique CFG identities;
    /// they establish neither reachability nor permission to omit guest work.
    pub fn with_runtime_entries(
        graph: &StaticCfg<'_>,
        binding: &DosFlatBindingPlan<'_>,
        guidance: Option<&BoundGuidance<'_, '_, '_>>,
        entries: &[ObjectAddress],
        limits: LoweringLimits,
    ) -> Result<Self, LoweringError> {
        if entries.len() > graph.nodes().len()
            || entries.windows(2).any(|pair| pair[0] >= pair[1])
            || entries.iter().any(|entry| {
                graph
                    .nodes()
                    .binary_search_by_key(entry, |node| node.address)
                    .is_err()
            })
        {
            return Err(LoweringError::InvalidRuntimeEntries);
        }
        if guidance.is_some_and(|value| !value.matches(graph, binding)) {
            return Err(LoweringError::DifferentGuidance);
        }
        Self::prepare(
            graph,
            binding,
            guidance.map(BoundGuidance::instructions),
            false,
            entries,
            limits,
        )
    }

    fn prepare(
        graph: &StaticCfg<'_>,
        binding: &DosFlatBindingPlan<'_>,
        guidance: Option<&[InstructionGuidance]>,
        uncounted_hot: bool,
        runtime_entries: &[ObjectAddress],
        limits: LoweringLimits,
    ) -> Result<Self, LoweringError> {
        if !core::ptr::eq(graph.image(), binding.image()) {
            return Err(LoweringError::DifferentImage);
        }
        if !graph.matches_binding(binding) {
            return Err(LoweringError::DifferentBinding);
        }
        let mut budget = Budget(limits.max_owned_bytes);
        let code_writes = if limits.isolate_mutable_sources {
            mutable_sources::discover(graph, binding, &mut budget)?
        } else {
            mutable_sources::CodeWrites::default()
        };
        let mut instruction_guidance = Vec::new();
        budget.reserve(&mut instruction_guidance, graph.nodes().len())?;
        if let Some(guidance) = guidance {
            instruction_guidance.extend_from_slice(guidance);
        } else {
            instruction_guidance.extend(
                graph
                    .nodes()
                    .iter()
                    .map(|node| InstructionGuidance::unknown(node.address)),
            );
        }
        let mut lowered = Vec::new();
        budget.reserve(&mut lowered, graph.nodes().len())?;
        let mut boundaries = Vec::new();
        let boundary_capacity = graph
            .nodes()
            .len()
            .checked_mul(
                if limits.isolate_mutable_sources || limits.isolate_immediates {
                    6
                } else {
                    4
                },
            )
            .ok_or(LoweringError::OwnedByteLimit)?;
        // CALL can contribute entry, continuation, target and a guidance
        // transition. Mutable-source partitioning can add two more boundaries.
        budget.reserve(&mut boundaries, boundary_capacity)?;
        let reserved_boundary_capacity = boundaries.capacity();
        let mut report = LoweringReport {
            discovered_instructions: graph.nodes().len(),
            ..LoweringReport::default()
        };
        for (index, node) in graph.nodes().iter().enumerate() {
            let source_conflict = limits.isolate_mutable_sources
                && matches!(
                    instruction_guidance[index].status,
                    GuidanceStatus::ByteMismatch
                        | GuidanceStatus::Conflict
                        | GuidanceStatus::ProfileCodeConflict
                );
            let mutable_source = source_conflict || code_writes.intersects(node, binding);
            if mutable_source {
                report.mutable_source_instructions += 1;
                boundaries.push(node.address);
                if let Some(next) = sequential(node, graph) {
                    boundaries.push(next);
                }
            }
            if let Some(next) = sequential(node, graph) {
                let next_index = graph
                    .nodes()
                    .binary_search_by_key(&next, |node| node.address)
                    .map_err(|_| LoweringError::AddressIndex)?;
                if node.address.object_number != next.object_number
                    || instruction_guidance[index].region_class()
                        != instruction_guidance[next_index].region_class()
                    || instruction_guidance[index].verified_hot()
                        != instruction_guidance[next_index].verified_hot()
                {
                    boundaries.push(next);
                }
            }
            let instruction = decode_node(node, graph, binding)?;
            let value = if runtime_entries.binary_search(&node.address).is_ok() {
                Lowered::Fallback
            } else {
                lower_instruction(&instruction, node, graph, binding, limits.direct_memory)
            };
            if limits.isolate_immediates
                && matches!(
                    value,
                    Lowered::Operation(
                        Operation::Move {
                            source: Source::Immediate(_),
                            ..
                        } | Operation::Alu {
                            source: Source::Immediate(_),
                            ..
                        }
                    )
                )
            {
                report.isolated_immediates += 1;
                // These instructions keep their original operands and exact
                // source checks. A changed immediate cannot invalidate the
                // preceding/following instructions' independent regions.
                if !mutable_source {
                    boundaries.push(node.address);
                    if let Some(next) = sequential(node, graph) {
                        boundaries.push(next);
                    }
                }
            }
            match value {
                Lowered::Operation(Operation::LoadMemory { .. } | Operation::StringLoad { .. }) => {
                    report.memory_loads += 1;
                    boundaries.push(node.address);
                    if let Some(next) = sequential(node, graph) {
                        boundaries.push(next);
                    }
                }
                Lowered::Operation(
                    Operation::StoreMemory { .. } | Operation::StringStore { .. },
                ) => {
                    report.memory_stores += 1;
                    boundaries.push(node.address);
                    if let Some(next) = sequential(node, graph) {
                        boundaries.push(next);
                    }
                }
                Lowered::Operation(Operation::ExtendMemory { .. }) => {
                    report.memory_extensions += 1;
                    boundaries.push(node.address);
                    if let Some(next) = sequential(node, graph) {
                        boundaries.push(next);
                    }
                }
                Lowered::Operation(
                    Operation::SignedMultiplyMemory { .. } | Operation::WideMultiplyMemory { .. },
                ) => {
                    report.memory_multiply += 1;
                    boundaries.push(node.address);
                    if let Some(next) = sequential(node, graph) {
                        boundaries.push(next);
                    }
                }
                Lowered::Operation(
                    Operation::AluMemory { .. } | Operation::CompareMemory { .. },
                ) => {
                    report.memory_alu += 1;
                    boundaries.push(node.address);
                    if let Some(next) = sequential(node, graph) {
                        boundaries.push(next);
                    }
                }
                Lowered::Operation(
                    Operation::AluMemoryStore { .. } | Operation::UnaryMemory { .. },
                ) => {
                    report.memory_updates += 1;
                    boundaries.push(node.address);
                    if let Some(next) = sequential(node, graph) {
                        boundaries.push(next);
                    }
                }
                Lowered::Operation(Operation::Push { .. } | Operation::Pop { .. }) => {
                    report.stack_operations += 1;
                    boundaries.push(node.address);
                    if let Some(next) = sequential(node, graph) {
                        boundaries.push(next);
                    }
                }
                Lowered::Call { .. } => {
                    report.direct_calls += 1;
                    // The complete stack proof belongs to this one guest
                    // instruction. Keep its continuation independently
                    // enterable when the runtime later interprets RET.
                    boundaries.push(node.address);
                    if let Some(next) = sequential(node, graph) {
                        boundaries.push(next);
                    }
                }
                Lowered::Return { .. } => {
                    report.near_returns += 1;
                    boundaries.push(node.address);
                }
                Lowered::Operation(_) => report.register_instructions += 1,
                Lowered::Jump(_) | Lowered::Branch { .. } => report.direct_transfers += 1,
                Lowered::Fallback => {
                    report.fallback_instructions += 1;
                    boundaries.push(node.address);
                    if let Some(next) = sequential(node, graph) {
                        boundaries.push(next);
                    }
                }
            }
            // Binding may resolve a near16 or relocated target that the
            // unbound CFG could not name. Split an already discovered stream
            // at that identity; unknown starts become fallback stubs only.
            let targets = match value {
                Lowered::Jump(to) | Lowered::Call { target: to, .. } => [Some(to), None],
                Lowered::Branch {
                    taken, not_taken, ..
                } => [Some(taken), Some(not_taken)],
                _ => [None, None],
            };
            for target in targets.into_iter().flatten() {
                if graph
                    .nodes()
                    .binary_search_by_key(&target, |node| node.address)
                    .is_ok()
                {
                    boundaries.push(target);
                }
            }
            lowered.push(value);
        }
        debug_assert_eq!(boundaries.capacity(), reserved_boundary_capacity);
        boundaries.sort_unstable();
        boundaries.dedup();
        let regions = RegionPlan::build(graph, &boundaries, limits.regions)?;
        let mut destinations = Vec::new();
        let max_destinations = regions
            .regions()
            .len()
            .checked_add(boundary_capacity)
            .ok_or(LoweringError::OwnedByteLimit)?;
        budget.reserve(&mut destinations, max_destinations)?;
        destinations.extend(regions.regions().iter().map(|region| region.entry));
        // Static transfers may land outside discovery. A zero-operation stub
        // delegates that exact address without decoding or claiming new code.
        for value in &lowered {
            match *value {
                Lowered::Jump(to) | Lowered::Call { target: to, .. } => destinations.push(to),
                Lowered::Branch {
                    taken, not_taken, ..
                } => {
                    destinations.push(taken);
                    destinations.push(not_taken);
                }
                _ => (),
            }
        }
        destinations.sort_unstable();
        destinations.dedup();
        if destinations.len() > limits.regions.max_regions {
            return Err(LoweringError::Regions(RegionError::RegionLimit));
        }
        let mut program = Program::default();
        let mut sources = Vec::new();
        budget.reserve(&mut program.regions, destinations.len())?;
        budget.reserve(&mut sources, destinations.len())?;
        for (index, &entry) in destinations.iter().enumerate() {
            let id = RegionId(u32::try_from(index).map_err(|_| LoweringError::AddressIndex)?);
            let mut operations = Vec::new();
            let mut node_addresses = Vec::new();
            let mut terminator = Terminator::Fallback(address(entry));
            let mut accounting = Accounting::Counted;
            if let Some(region_index) = regions.region_index(entry) {
                let indices = regions
                    .node_indices(region_index)
                    .ok_or(LoweringError::AddressIndex)?;
                if uncounted_hot
                    && !indices.is_empty()
                    && indices
                        .iter()
                        .all(|&index| instruction_guidance[index].verified_hot())
                {
                    accounting = Accounting::UncountedHot;
                }
                budget.reserve(&mut operations, indices.len())?;
                budget.reserve(&mut node_addresses, indices.len())?;
                for (position, &node_index) in indices.iter().enumerate() {
                    let node = &graph.nodes()[node_index];
                    node_addresses.push(node.address);
                    match lowered[node_index] {
                        Lowered::Operation(operation) => {
                            operations.push(operation);
                            if position + 1 == indices.len() {
                                // A sequential instruction can end at a stopped
                                // path or object boundary. Its completed effects
                                // still precede the exact next runtime address.
                                let next = if graph.binding().is_some() {
                                    graph_target(graph, node.address, EdgeKind::Fallthrough)
                                } else {
                                    None
                                }
                                .map(Ok)
                                .unwrap_or_else(|| next_address(node))?;
                                terminator = match destinations.binary_search(&next) {
                                    Ok(index) => Terminator::Fallthrough(region_id(index)?),
                                    Err(_) => Terminator::Fallback(address(next)),
                                };
                            }
                        }
                        Lowered::Jump(to) => {
                            terminator = Terminator::Jump(target_id(&destinations, to)?)
                        }
                        Lowered::Call {
                            width,
                            return_offset,
                            target,
                        } => {
                            operations.push(Operation::Push {
                                width,
                                source: Source::Immediate(return_offset),
                            });
                            // PUSH accounts for CALL itself. A subdivision
                            // edge transfers directly without a second charge.
                            terminator = Terminator::Fallthrough(target_id(&destinations, target)?);
                        }
                        Lowered::Return { width, pop_bytes } => {
                            terminator = Terminator::Return {
                                width,
                                pop_bytes,
                                target_space: 0,
                            };
                        }
                        Lowered::Branch {
                            condition,
                            taken,
                            not_taken,
                        } => {
                            terminator = Terminator::Branch {
                                condition,
                                taken: target_id(&destinations, taken)?,
                                not_taken: target_id(&destinations, not_taken)?,
                            }
                        }
                        Lowered::Fallback => {
                            terminator = Terminator::Fallback(address(node.address))
                        }
                    }
                }
            } else {
                report.external_stubs += 1;
            }
            sources.push(RegionSources {
                id,
                nodes: node_addresses,
            });
            program.regions.push(Region {
                id,
                entry: address(entry),
                operations,
                terminator,
                accounting,
            });
        }
        report.generated_regions = program.regions.len();
        Ok(Self {
            program,
            sources,
            report,
            guidance: instruction_guidance,
        })
    }

    pub fn program(&self) -> &Program {
        &self.program
    }
    pub fn sources(&self) -> &[RegionSources] {
        &self.sources
    }
    pub fn report(&self) -> LoweringReport {
        self.report
    }
    pub fn guidance(&self) -> &[InstructionGuidance] {
        &self.guidance
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoweringError {
    InvalidRuntimeEntries,
    DifferentImage,
    DifferentBinding,
    DifferentGuidance,
    ChangedInstructionEncoding(ObjectAddress),
    SequentialOverflow(ObjectAddress),
    AddressIndex,
    OwnedByteLimit,
    Allocation,
    Regions(RegionError),
}
impl From<RegionError> for LoweringError {
    fn from(value: RegionError) -> Self {
        Self::Regions(value)
    }
}
impl fmt::Display for LoweringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "static WASM lowering: {self:?}")
    }
}
impl core::error::Error for LoweringError {}

#[derive(Clone, Copy)]
enum Lowered {
    Operation(Operation),
    Jump(ObjectAddress),
    Call {
        width: Width,
        return_offset: u32,
        target: ObjectAddress,
    },
    Return {
        width: Width,
        pop_bytes: u16,
    },
    Branch {
        condition: Condition,
        taken: ObjectAddress,
        not_taken: ObjectAddress,
    },
    Fallback,
}

fn decode_node(
    node: &InstructionNode,
    graph: &StaticCfg<'_>,
    binding: &DosFlatBindingPlan<'_>,
) -> Result<Instruction, LoweringError> {
    decode_node_with_offsets(node, graph, binding).map(|(instruction, _, _)| instruction)
}

fn decode_node_with_offsets(
    node: &InstructionNode,
    graph: &StaticCfg<'_>,
    binding: &DosFlatBindingPlan<'_>,
) -> Result<(Instruction, iced_x86::ConstantOffsets, u32), LoweringError> {
    let object = graph
        .image()
        .object(node.address.object_number)
        .ok_or(LoweringError::AddressIndex)?;
    let start = usize::try_from(node.address.offset).map_err(|_| LoweringError::AddressIndex)?;
    let original = object
        .bytes()
        .get(start..)
        .ok_or(LoweringError::AddressIndex)?;
    let length = original.len().min(15);
    let mut window = [0; 15];
    window[..length].copy_from_slice(&original[..length]);
    let patches = binding
        .object_patches(node.address.object_number)
        .ok_or(LoweringError::AddressIndex)?;
    let first = patches.partition_point(|patch| {
        u64::from(patch.source.offset) + patch.bytes().len() as u64
            <= u64::from(node.address.offset)
    });
    for patch in &patches[first..] {
        let patch_start =
            usize::try_from(patch.source.offset).map_err(|_| LoweringError::AddressIndex)?;
        if patch_start >= start + length {
            break;
        }
        for (index, &byte) in patch.bytes().iter().enumerate() {
            let at = patch_start + index;
            if at >= start && at - start < length {
                window[at - start] = byte;
            }
        }
    }
    let object_binding = binding
        .object_binding(node.address.object_number)
        .ok_or(LoweringError::AddressIndex)?;
    let ip = match node.bitness {
        DefaultBitness::Bits16 => node.address.offset,
        DefaultBitness::Bits32 => object_binding.linear_base.wrapping_add(node.address.offset),
    };
    let bitness = match node.bitness {
        DefaultBitness::Bits16 => 16,
        DefaultBitness::Bits32 => 32,
    };
    let mut decoder = Decoder::with_ip(
        bitness,
        &window[..length],
        u64::from(ip),
        DecoderOptions::NONE,
    );
    let instruction = decoder.decode();
    if instruction.is_invalid() || instruction.len() != usize::from(node.byte_len) {
        return Err(LoweringError::ChangedInstructionEncoding(node.address));
    }
    let offsets = decoder.get_constant_offsets(&instruction);
    Ok((instruction, offsets, ip))
}

fn lower_instruction(
    instruction: &Instruction,
    node: &InstructionNode,
    graph: &StaticCfg<'_>,
    binding: &DosFlatBindingPlan<'_>,
    direct_memory: bool,
) -> Lowered {
    lower_instruction_with_virtual_registers(
        instruction,
        &iced_x86::ConstantOffsets::default(),
        0,
        node,
        graph,
        binding,
        direct_memory,
        &mutable_sources::VirtualRegisters::default(),
    )
}

#[allow(clippy::too_many_arguments)]
fn lower_instruction_with_virtual_registers(
    instruction: &Instruction,
    offsets: &iced_x86::ConstantOffsets,
    ip: u32,
    node: &InstructionNode,
    graph: &StaticCfg<'_>,
    binding: &DosFlatBindingPlan<'_>,
    direct_memory: bool,
    virtual_registers: &mutable_sources::VirtualRegisters,
) -> Lowered {
    if instruction.has_lock_prefix()
        || instruction.has_rep_prefix()
        || instruction.has_repne_prefix()
    {
        return Lowered::Fallback;
    }
    let vreg_eligible = direct_memory && node.bitness == DefaultBitness::Bits32 && ip >= 0x500;
    let disp_vreg =
        (vreg_eligible && offsets.has_displacement() && offsets.displacement_size() == 4)
            .then(|| ip.wrapping_add(offsets.displacement_offset() as u32))
            .filter(|&slot| slot >= 0x500 && virtual_registers.intersects_slot(slot));
    let imm_vreg = (vreg_eligible && offsets.has_immediate() && offsets.immediate_size() == 4)
        .then(|| ip.wrapping_add(offsets.immediate_offset() as u32))
        .filter(|&slot| slot >= 0x500 && virtual_registers.intersects_slot(slot));

    if let Some(operation) = register_operation_with_vreg(instruction, disp_vreg, imm_vreg) {
        return Lowered::Operation(operation);
    }
    if direct_memory && let Some(operation) = stack_operation_with_vreg(instruction, imm_vreg) {
        return Lowered::Operation(operation);
    }
    if direct_memory && let Some(operation) = string_operation(instruction) {
        return Lowered::Operation(operation);
    }
    if direct_memory
        && matches!(instruction.mnemonic(), Mnemonic::Movsx | Mnemonic::Movzx)
        && instruction.op0_kind() == OpKind::Register
        && instruction.op1_kind() == OpKind::Memory
        && let (Some(target), Some(address)) = (
            register(instruction.op0_register()),
            memory_address_with_vreg(instruction, disp_vreg),
        )
    {
        let width = match instruction.memory_size().size() {
            1 => Some(Width::Byte),
            2 => Some(Width::Word),
            _ => None,
        };
        if let Some(width) = width
            && extension_widths(target.width(), width)
        {
            return Lowered::Operation(Operation::ExtendMemory {
                target,
                width,
                address,
                signed: instruction.mnemonic() == Mnemonic::Movsx,
            });
        }
    }
    if direct_memory
        && graph.binding().is_some()
        && node.bitness == DefaultBitness::Bits32
        && let Some((width, pop_bytes)) = near_return(instruction)
    {
        // The bound graph proves CS.base=0 and a full flat32 limit. The
        // popped EIP therefore names the shared linear dispatcher space.
        return Lowered::Return { width, pop_bytes };
    }
    if direct_memory
        && instruction.mnemonic() == Mnemonic::Call
        && instruction.op0_kind() == OpKind::NearBranch32
        && node.bitness == DefaultBitness::Bits32
        && graph.binding().is_some()
        && let Some(target) = graph_target(graph, node.address, EdgeKind::Call)
    {
        // Bound traversal proves full flat32 CS and retains it across objects.
        // next_ip32 is the linear return EIP, not this object's byte offset.
        // rel16 remains unresolved in that graph; never infer a code16 view.
        return Lowered::Call {
            width: Width::Dword,
            return_offset: instruction.next_ip32(),
            target,
        };
    }
    if direct_memory
        && instruction.mnemonic() == Mnemonic::Mov
        && instruction.op0_kind() == OpKind::Register
        && instruction.op1_kind() == OpKind::Memory
        && let (Some(target), Some(address)) = (
            register(instruction.op0_register()),
            memory_address_with_vreg(instruction, disp_vreg),
        )
    {
        return Lowered::Operation(Operation::LoadMemory { target, address });
    }
    if direct_memory
        && instruction.mnemonic() == Mnemonic::Mov
        && instruction.op0_kind() == OpKind::Memory
    {
        use doslet_x86_wasm::static_program::Width;
        let width = match instruction.memory_size().size() {
            1 => Some(Width::Byte),
            2 => Some(Width::Word),
            4 => Some(Width::Dword),
            _ => None,
        };
        let source = match instruction.op1_kind() {
            OpKind::Register => register(instruction.op1_register()).map(Source::Register),
            OpKind::Immediate32 if width == Some(Width::Dword) && imm_vreg.is_some() => {
                imm_vreg.map(Source::VirtualRegister)
            }
            OpKind::Immediate8 | OpKind::Immediate16 | OpKind::Immediate32 => {
                width.map(|width| Source::Immediate(instruction.immediate(1) as u32 & width.mask()))
            }
            _ => None,
        };
        if let (Some(width), Some(source), Some(address)) = (
            width,
            source,
            memory_address_with_vreg(instruction, disp_vreg),
        ) {
            return Lowered::Operation(Operation::StoreMemory {
                address,
                width,
                source,
            });
        }
    }
    if direct_memory
        && let Some(operation) = memory_alu_operation_with_vreg(instruction, disp_vreg, imm_vreg)
    {
        return Lowered::Operation(operation);
    }
    if direct_memory
        && let Some(operation) = memory_update_operation_with_vreg(instruction, disp_vreg, imm_vreg)
    {
        return Lowered::Operation(operation);
    }
    if direct_memory
        && matches!(instruction.mnemonic(), Mnemonic::Mul | Mnemonic::Imul)
        && instruction.op_count() == 1
        && instruction.op0_kind() == OpKind::Memory
        && let Some(address) = memory_address_with_vreg(instruction, disp_vreg)
    {
        let width = match instruction.memory_size().size() {
            1 => Some(Width::Byte),
            2 => Some(Width::Word),
            4 => Some(Width::Dword),
            _ => None,
        };
        if let Some(width) = width {
            return Lowered::Operation(Operation::WideMultiplyMemory {
                signed: instruction.mnemonic() == Mnemonic::Imul,
                width,
                address,
            });
        }
    }
    if direct_memory
        && instruction.mnemonic() == Mnemonic::Imul
        && matches!(instruction.op_count(), 2 | 3)
        && instruction.op0_kind() == OpKind::Register
        && instruction.op1_kind() == OpKind::Memory
        && let (Some(target), Some(address)) = (
            register(instruction.op0_register()),
            memory_address_with_vreg(instruction, disp_vreg),
        )
        && matches!(target.width(), Width::Word | Width::Dword)
        && instruction.memory_size().size() == (target.width().bits() / 8) as usize
    {
        let source = if instruction.op_count() == 2 {
            Some(Source::Register(target))
        } else {
            operand_source_with_vreg(instruction, 2, target.width(), imm_vreg).filter(|source| {
                matches!(source, Source::Immediate(_) | Source::VirtualRegister(_))
            })
        };
        if let Some(source) = source {
            return Lowered::Operation(Operation::SignedMultiplyMemory {
                target,
                address,
                source,
            });
        }
    }
    if instruction.mnemonic() == Mnemonic::Jmp
        && matches!(
            instruction.op0_kind(),
            OpKind::NearBranch16 | OpKind::NearBranch32
        )
    {
        return branch_target(instruction, node, graph, binding)
            .map(Lowered::Jump)
            .unwrap_or(Lowered::Fallback);
    }
    if let Some(condition) = condition(instruction.mnemonic())
        && let (Some(taken), Some(not_taken)) = (
            branch_target(instruction, node, graph, binding),
            if graph.binding().is_some() {
                graph_target(graph, node.address, EdgeKind::Fallthrough)
            } else {
                fallthrough(node, binding)
            },
        )
    {
        return Lowered::Branch {
            condition,
            taken,
            not_taken,
        };
    }
    Lowered::Fallback
}

fn near_return(instruction: &Instruction) -> Option<(Width, u16)> {
    if instruction.mnemonic() != Mnemonic::Ret {
        return None;
    }
    let pop_bytes = match instruction.op_count() {
        0 => 0,
        1 if instruction.op0_kind() == OpKind::Immediate16 => instruction.immediate16(),
        _ => return None,
    };
    let width = match instruction.stack_pointer_increment() - i32::from(pop_bytes) {
        2 => Width::Word,
        4 => Width::Dword,
        _ => return None,
    };
    Some((width, pop_bytes))
}

fn string_operation(instruction: &Instruction) -> Option<Operation> {
    use doslet_x86_wasm::static_program::memory::Segment;
    let load = match instruction.mnemonic() {
        Mnemonic::Lodsb | Mnemonic::Lodsw | Mnemonic::Lodsd => true,
        Mnemonic::Stosb | Mnemonic::Stosw | Mnemonic::Stosd => false,
        _ => return None,
    };
    let width = match instruction.memory_size().size() {
        1 => Width::Byte,
        2 => Width::Word,
        4 => Width::Dword,
        _ => return None,
    };
    let memory_kind = if load {
        instruction.op1_kind()
    } else {
        instruction.op0_kind()
    };
    let address16 = match memory_kind {
        OpKind::MemorySegSI | OpKind::MemoryESDI => true,
        OpKind::MemorySegESI | OpKind::MemoryESEDI => false,
        _ => return None,
    };
    if load {
        let segment = match instruction.memory_segment() {
            iced_x86::Register::ES => Segment::Es,
            iced_x86::Register::CS => Segment::Cs,
            iced_x86::Register::SS => Segment::Ss,
            iced_x86::Register::DS => Segment::Ds,
            iced_x86::Register::FS => Segment::Fs,
            iced_x86::Register::GS => Segment::Gs,
            _ => return None,
        };
        Some(Operation::StringLoad {
            width,
            address16,
            segment,
        })
    } else {
        Some(Operation::StringStore { width, address16 })
    }
}

#[cfg(test)]
fn stack_operation(instruction: &Instruction) -> Option<Operation> {
    stack_operation_with_vreg(instruction, None)
}

fn stack_operation_with_vreg(
    instruction: &Instruction,
    imm_vreg: Option<u32>,
) -> Option<Operation> {
    if instruction.op_count() != 1 {
        return None;
    }
    match instruction.mnemonic() {
        Mnemonic::Push => {
            // iced derives operand width from the decoded encoding, including
            // 66h and sign-extended imm8. Address-size prefixes do not select
            // the stack width: the live descriptor supplies SS.B independently.
            let width = match instruction.stack_pointer_increment() {
                -2 => Width::Word,
                -4 => Width::Dword,
                _ => return None,
            };
            let source = match instruction.op0_kind() {
                OpKind::Register => {
                    let source = register(instruction.op0_register())?;
                    if source.width() != width {
                        return None;
                    }
                    Source::Register(source)
                }
                OpKind::Immediate32 if width == Width::Dword && imm_vreg.is_some() => {
                    Source::VirtualRegister(imm_vreg?)
                }
                OpKind::Immediate16
                | OpKind::Immediate32
                | OpKind::Immediate8to16
                | OpKind::Immediate8to32 => {
                    Source::Immediate(instruction.immediate(0) as u32 & width.mask())
                }
                _ => return None,
            };
            Some(Operation::Push { width, source })
        }
        Mnemonic::Pop if instruction.op0_kind() == OpKind::Register => {
            let target = register(instruction.op0_register())?;
            if !matches!(target.width(), Width::Word | Width::Dword) {
                return None;
            }
            Some(Operation::Pop { target })
        }
        _ => None,
    }
}

fn memory_address_with_vreg(
    instruction: &Instruction,
    disp_vreg: Option<u32>,
) -> Option<doslet_x86_wasm::static_program::memory::MemoryAddress> {
    use doslet_x86_wasm::static_program::memory::{MemoryAddress, Segment, VIRTUAL_DISPLACEMENT};
    use iced_x86::Register as R;
    let full = |value: R| -> Option<Option<Register>> {
        Some(match value {
            R::None => None,
            R::AX | R::EAX => Some(Register::Eax),
            R::CX | R::ECX => Some(Register::Ecx),
            R::DX | R::EDX => Some(Register::Edx),
            R::BX | R::EBX => Some(Register::Ebx),
            R::SP | R::ESP => Some(Register::Esp),
            R::BP | R::EBP => Some(Register::Ebp),
            R::SI | R::ESI => Some(Register::Esi),
            R::DI | R::EDI => Some(Register::Edi),
            _ => return None,
        })
    };
    let segment = match instruction.memory_segment() {
        R::ES => Segment::Es,
        R::CS => Segment::Cs,
        R::SS => Segment::Ss,
        R::DS => Segment::Ds,
        R::FS => Segment::Fs,
        R::GS => Segment::Gs,
        _ => return None,
    };
    let base = instruction.memory_base();
    let index = instruction.memory_index();
    // For absolute moffs/ModRM operands iced records the address width in the
    // displacement size; otherwise the decoded address register fixes it.
    let address16 = base.is_gpr16()
        || index.is_gpr16()
        || base == R::None && index == R::None && instruction.memory_displ_size() == 2;
    let mut scale = instruction.memory_index_scale() as u8;
    let mut displacement = instruction.memory_displacement32();
    if !address16 && let Some(slot) = disp_vreg {
        scale |= VIRTUAL_DISPLACEMENT;
        displacement = slot;
    }
    Some(MemoryAddress {
        base: full(base)?,
        index: full(index)?,
        scale,
        displacement,
        address16,
        segment,
    })
}

fn extension_widths(target: Width, source: Width) -> bool {
    matches!(target, Width::Word | Width::Dword)
        && matches!(source, Width::Byte | Width::Word)
        && target.bits() >= source.bits()
}

fn register_operation(instruction: &Instruction) -> Option<Operation> {
    register_operation_with_vreg(instruction, None, None)
}

fn register_operation_with_vreg(
    instruction: &Instruction,
    disp_vreg: Option<u32>,
    imm_vreg: Option<u32>,
) -> Option<Operation> {
    if let Some(operation) = shift_operation(instruction) {
        return Some(operation);
    }
    if matches!(instruction.mnemonic(), Mnemonic::Mul | Mnemonic::Imul)
        && instruction.op_count() == 1
        && instruction.op0_kind() == OpKind::Register
        && let Some(source) = register(instruction.op0_register())
    {
        return Some(Operation::WideMultiply {
            signed: instruction.mnemonic() == Mnemonic::Imul,
            width: source.width(),
            source: Source::Register(source),
        });
    }
    if instruction.mnemonic() == Mnemonic::Imul
        && matches!(instruction.op_count(), 2 | 3)
        && instruction.op0_kind() == OpKind::Register
        && instruction.op1_kind() == OpKind::Register
        && let Some(target) = register(instruction.op0_register())
        && matches!(target.width(), Width::Word | Width::Dword)
    {
        let source = operand_source_with_vreg(instruction, 1, target.width(), None)?;
        let (left, right) = if instruction.op_count() == 2 {
            (Source::Register(target), source)
        } else {
            let immediate = operand_source_with_vreg(instruction, 2, target.width(), imm_vreg)?;
            if !matches!(immediate, Source::Immediate(_) | Source::VirtualRegister(_)) {
                return None;
            }
            (source, immediate)
        };
        return Some(Operation::SignedMultiply {
            target,
            left,
            right,
        });
    }
    let target = || register(instruction.op0_register());
    let unary = |op: fn(Register) -> Operation| {
        (instruction.op_count() == 1 && instruction.op0_kind() == OpKind::Register)
            .then(|| target().map(op))
            .flatten()
    };
    match instruction.mnemonic() {
        Mnemonic::Nop if instruction.op_count() == 0 => Some(Operation::Nop),
        Mnemonic::Cbw if instruction.op_count() == 0 => Some(Operation::Extend {
            target: Register::Ax,
            source: Register::Al,
            signed: true,
        }),
        Mnemonic::Cwde if instruction.op_count() == 0 => Some(Operation::Extend {
            target: Register::Eax,
            source: Register::Ax,
            signed: true,
        }),
        mnemonic
            if matches!(mnemonic, Mnemonic::Movsx | Mnemonic::Movzx)
                && instruction.op_count() == 2
                && instruction.op0_kind() == OpKind::Register
                && instruction.op1_kind() == OpKind::Register =>
        {
            let target = target()?;
            let source = register(instruction.op1_register())?;
            if !extension_widths(target.width(), source.width()) {
                return None;
            }
            Some(Operation::Extend {
                target,
                source,
                signed: mnemonic == Mnemonic::Movsx,
            })
        }
        Mnemonic::Lea
            if instruction.op_count() == 2
                && instruction.op0_kind() == OpKind::Register
                && instruction.op1_kind() == OpKind::Memory =>
        {
            let target = target()?;
            if !matches!(target.width(), Width::Word | Width::Dword) {
                return None;
            }
            Some(Operation::Lea {
                target,
                address: memory_address_with_vreg(instruction, disp_vreg)?,
            })
        }
        Mnemonic::Inc => unary(Operation::Inc),
        Mnemonic::Dec => unary(Operation::Dec),
        Mnemonic::Neg => unary(Operation::Neg),
        Mnemonic::Not => unary(Operation::Not),
        Mnemonic::Xchg
            if instruction.op_count() == 2
                && instruction.op0_kind() == OpKind::Register
                && instruction.op1_kind() == OpKind::Register =>
        {
            Some(Operation::Exchange {
                left: target()?,
                right: register(instruction.op1_register())?,
            })
        }
        mnemonic if instruction.op_count() == 2 && instruction.op0_kind() == OpKind::Register => {
            let target = target()?;
            let source = match instruction.op1_kind() {
                OpKind::Register => Source::Register(register(instruction.op1_register())?),
                OpKind::Immediate32 if target.width() == Width::Dword && imm_vreg.is_some() => {
                    Source::VirtualRegister(imm_vreg?)
                }
                OpKind::Immediate8
                | OpKind::Immediate16
                | OpKind::Immediate32
                | OpKind::Immediate8to16
                | OpKind::Immediate8to32 => {
                    Source::Immediate(instruction.immediate(1) as u32 & target.width().mask())
                }
                _ => return None,
            };
            if mnemonic == Mnemonic::Mov {
                return Some(Operation::Move { target, source });
            }
            let kind = alu_kind(mnemonic)?;
            Some(Operation::Alu {
                kind,
                target,
                source,
            })
        }
        _ => None,
    }
}

fn alu_kind(mnemonic: Mnemonic) -> Option<Alu> {
    Some(match mnemonic {
        Mnemonic::Add => Alu::Add,
        Mnemonic::Adc => Alu::Adc,
        Mnemonic::Sub => Alu::Sub,
        Mnemonic::Sbb => Alu::Sbb,
        Mnemonic::And => Alu::And,
        Mnemonic::Or => Alu::Or,
        Mnemonic::Xor => Alu::Xor,
        Mnemonic::Cmp => Alu::Cmp,
        Mnemonic::Test => Alu::Test,
        _ => return None,
    })
}

fn memory_alu_operation_with_vreg(
    instruction: &Instruction,
    disp_vreg: Option<u32>,
    imm_vreg: Option<u32>,
) -> Option<Operation> {
    if instruction.op_count() != 2 {
        return None;
    }
    let kind = alu_kind(instruction.mnemonic())?;
    let address = memory_address_with_vreg(instruction, disp_vreg)?;
    if instruction.op0_kind() == OpKind::Register && instruction.op1_kind() == OpKind::Memory {
        return Some(Operation::AluMemory {
            kind,
            target: register(instruction.op0_register())?,
            address,
        });
    }
    if instruction.op0_kind() != OpKind::Memory || !matches!(kind, Alu::Cmp | Alu::Test) {
        return None;
    }
    let width = match instruction.memory_size().size() {
        1 => Width::Byte,
        2 => Width::Word,
        4 => Width::Dword,
        _ => return None,
    };
    let source = memory_source_with_vreg(instruction, width, imm_vreg)?;
    Some(Operation::CompareMemory {
        test: kind == Alu::Test,
        address,
        width,
        source,
    })
}

fn memory_source_with_vreg(
    instruction: &Instruction,
    width: Width,
    imm_vreg: Option<u32>,
) -> Option<Source> {
    operand_source_with_vreg(instruction, 1, width, imm_vreg)
}

fn operand_source_with_vreg(
    instruction: &Instruction,
    operand: u32,
    width: Width,
    imm_vreg: Option<u32>,
) -> Option<Source> {
    Some(match instruction.op_kind(operand) {
        OpKind::Register => {
            let source = register(instruction.op_register(operand))?;
            if source.width() != width {
                return None;
            }
            Source::Register(source)
        }
        OpKind::Immediate32 if width == Width::Dword && imm_vreg.is_some() => {
            Source::VirtualRegister(imm_vreg?)
        }
        OpKind::Immediate8
        | OpKind::Immediate16
        | OpKind::Immediate32
        | OpKind::Immediate8to16
        | OpKind::Immediate8to32 => {
            Source::Immediate(instruction.immediate(operand) as u32 & width.mask())
        }
        _ => return None,
    })
}

fn memory_update_operation_with_vreg(
    instruction: &Instruction,
    disp_vreg: Option<u32>,
    imm_vreg: Option<u32>,
) -> Option<Operation> {
    if instruction.op0_kind() != OpKind::Memory {
        return None;
    }
    let address = memory_address_with_vreg(instruction, disp_vreg)?;
    let width = match instruction.memory_size().size() {
        1 => Width::Byte,
        2 => Width::Word,
        4 => Width::Dword,
        _ => return None,
    };
    if instruction.op_count() == 1 {
        let kind = match instruction.mnemonic() {
            Mnemonic::Inc => MemoryUnary::Inc,
            Mnemonic::Dec => MemoryUnary::Dec,
            Mnemonic::Neg => MemoryUnary::Neg,
            Mnemonic::Not => MemoryUnary::Not,
            _ => return None,
        };
        return Some(Operation::UnaryMemory {
            kind,
            address,
            width,
        });
    }
    if instruction.op_count() != 2 {
        return None;
    }
    let kind = alu_kind(instruction.mnemonic())?;
    if matches!(kind, Alu::Cmp | Alu::Test) {
        return None;
    }
    Some(Operation::AluMemoryStore {
        kind,
        address,
        width,
        source: memory_source_with_vreg(instruction, width, imm_vreg)?,
    })
}

fn shift_count(instruction: &Instruction, operand: u32) -> Option<ShiftCount> {
    match instruction.op_kind(operand) {
        OpKind::Immediate8 => match instruction.immediate(operand) as u8 {
            1 => Some(ShiftCount::One),
            count => Some(ShiftCount::Immediate(count)),
        },
        OpKind::Register if instruction.op_register(operand) == iced_x86::Register::CL => {
            Some(ShiftCount::Cl)
        }
        _ => None,
    }
}

fn shift_operation(instruction: &Instruction) -> Option<Operation> {
    if instruction.op0_kind() != OpKind::Register {
        return None;
    }
    let target = register(instruction.op0_register())?;
    let kind = match instruction.mnemonic() {
        Mnemonic::Shl | Mnemonic::Sal => Some(Shift::Shl),
        Mnemonic::Shr => Some(Shift::Shr),
        Mnemonic::Sar => Some(Shift::Sar),
        Mnemonic::Rol => Some(Shift::Rol),
        Mnemonic::Ror => Some(Shift::Ror),
        Mnemonic::Rcl => Some(Shift::Rcl),
        Mnemonic::Rcr => Some(Shift::Rcr),
        _ => None,
    };
    if let Some(kind) = kind {
        if instruction.op_count() != 2 {
            return None;
        }
        return Some(Operation::Shift {
            kind,
            target,
            count: shift_count(instruction, 1)?,
        });
    }
    let kind = match instruction.mnemonic() {
        Mnemonic::Shld => DoubleShift::Left,
        Mnemonic::Shrd => DoubleShift::Right,
        _ => return None,
    };
    if instruction.op_count() != 3 || instruction.op1_kind() != OpKind::Register {
        return None;
    }
    let source = register(instruction.op1_register())?;
    let count = shift_count(instruction, 2)?;
    if target.width() == Width::Byte
        || target.width() != source.width()
        || target.width() == Width::Word
            && match count {
                ShiftCount::Cl => true,
                ShiftCount::Immediate(count) => count & 31 > 16,
                ShiftCount::One => false,
            }
    {
        return None;
    }
    Some(Operation::DoubleShift {
        kind,
        target,
        source,
        count,
    })
}

fn register(value: iced_x86::Register) -> Option<Register> {
    use iced_x86::Register as R;
    Some(match value {
        R::EAX => Register::Eax,
        R::ECX => Register::Ecx,
        R::EDX => Register::Edx,
        R::EBX => Register::Ebx,
        R::ESP => Register::Esp,
        R::EBP => Register::Ebp,
        R::ESI => Register::Esi,
        R::EDI => Register::Edi,
        R::AX => Register::Ax,
        R::CX => Register::Cx,
        R::DX => Register::Dx,
        R::BX => Register::Bx,
        R::SP => Register::Sp,
        R::BP => Register::Bp,
        R::SI => Register::Si,
        R::DI => Register::Di,
        R::AL => Register::Al,
        R::CL => Register::Cl,
        R::DL => Register::Dl,
        R::BL => Register::Bl,
        R::AH => Register::Ah,
        R::CH => Register::Ch,
        R::DH => Register::Dh,
        R::BH => Register::Bh,
        _ => return None,
    })
}

fn condition(value: Mnemonic) -> Option<Condition> {
    Some(match value {
        Mnemonic::Jo => Condition::Overflow,
        Mnemonic::Jno => Condition::NotOverflow,
        Mnemonic::Jb => Condition::Below,
        Mnemonic::Jae => Condition::AboveOrEqual,
        Mnemonic::Je => Condition::Equal,
        Mnemonic::Jne => Condition::NotEqual,
        Mnemonic::Jbe => Condition::BelowOrEqual,
        Mnemonic::Ja => Condition::Above,
        Mnemonic::Js => Condition::Sign,
        Mnemonic::Jns => Condition::NotSign,
        Mnemonic::Jp => Condition::Parity,
        Mnemonic::Jnp => Condition::NotParity,
        Mnemonic::Jl => Condition::Less,
        Mnemonic::Jge => Condition::GreaterOrEqual,
        Mnemonic::Jle => Condition::LessOrEqual,
        Mnemonic::Jg => Condition::Greater,
        _ => return None,
    })
}

fn near_target(
    instruction: &Instruction,
    node: &InstructionNode,
    plan: &DosFlatBindingPlan<'_>,
) -> Option<ObjectAddress> {
    let binding = plan.object_binding(node.address.object_number)?;
    let eip = u32::try_from(instruction.near_branch_target()).ok()?;
    if eip > binding.normal?.limit {
        return None;
    }
    let offset = match node.bitness {
        DefaultBitness::Bits16 => eip,
        DefaultBitness::Bits32 => eip.checked_sub(binding.linear_base)?,
    };
    (offset
        < plan
            .image()
            .object(node.address.object_number)?
            .metadata()
            .virtual_size)
        .then_some(ObjectAddress {
            offset,
            ..node.address
        })
}

/// A bound graph already resolved transfers while retaining the source CS.
/// Recomputing against the destination object's selector would lose that proof.
fn branch_target(
    instruction: &Instruction,
    node: &InstructionNode,
    graph: &StaticCfg<'_>,
    plan: &DosFlatBindingPlan<'_>,
) -> Option<ObjectAddress> {
    if graph.binding().is_some() {
        graph_target(graph, node.address, EdgeKind::Branch)
    } else {
        near_target(instruction, node, plan)
    }
}

fn graph_target(
    graph: &StaticCfg<'_>,
    source: ObjectAddress,
    kind: EdgeKind,
) -> Option<ObjectAddress> {
    let first = graph.edges().partition_point(|edge| edge.source < source);
    graph.edges()[first..]
        .iter()
        .take_while(|edge| edge.source == source)
        .find_map(|edge| match edge.target {
            EdgeTarget::Address(target) if edge.kind == kind => Some(target),
            _ => None,
        })
}

fn fallthrough(node: &InstructionNode, plan: &DosFlatBindingPlan<'_>) -> Option<ObjectAddress> {
    let offset = node.address.offset.checked_add(u32::from(node.byte_len))?;
    if node.bitness == DefaultBitness::Bits16 && offset > 0xffff {
        return None;
    }
    let binding = plan.object_binding(node.address.object_number)?;
    let eip = match node.bitness {
        DefaultBitness::Bits16 => offset,
        DefaultBitness::Bits32 => binding.linear_base.checked_add(offset)?,
    };
    (eip <= binding.normal?.limit
        && offset
            < plan
                .image()
                .object(node.address.object_number)?
                .metadata()
                .virtual_size)
        .then_some(ObjectAddress {
            offset,
            ..node.address
        })
}

fn sequential(node: &InstructionNode, graph: &StaticCfg<'_>) -> Option<ObjectAddress> {
    if graph.binding().is_some()
        && let Some(next) = [
            EdgeKind::Fallthrough,
            EdgeKind::CallContinuation,
            EdgeKind::ServiceContinuation,
            EdgeKind::Resume,
        ]
        .into_iter()
        .find_map(|kind| graph_target(graph, node.address, kind))
        .filter(|next| {
            graph
                .nodes()
                .binary_search_by_key(next, |node| node.address)
                .is_ok()
        })
    {
        return Some(next);
    }
    let next = next_address(node).ok()?;
    graph
        .nodes()
        .binary_search_by_key(&next, |node| node.address)
        .ok()
        .map(|_| next)
}

fn next_address(node: &InstructionNode) -> Result<ObjectAddress, LoweringError> {
    let next = node
        .address
        .offset
        .checked_add(u32::from(node.byte_len))
        .ok_or(LoweringError::SequentialOverflow(node.address))?;
    let offset = match node.bitness {
        DefaultBitness::Bits16 => next & 0xffff,
        DefaultBitness::Bits32 => next,
    };
    Ok(ObjectAddress {
        offset,
        ..node.address
    })
}

fn address(value: ObjectAddress) -> Address {
    Address {
        space: value.object_number,
        offset: value.offset,
    }
}
fn region_id(value: usize) -> Result<RegionId, LoweringError> {
    Ok(RegionId(
        u32::try_from(value).map_err(|_| LoweringError::AddressIndex)?,
    ))
}
fn target_id(
    destinations: &[ObjectAddress],
    target: ObjectAddress,
) -> Result<RegionId, LoweringError> {
    region_id(
        destinations
            .binary_search(&target)
            .map_err(|_| LoweringError::AddressIndex)?,
    )
}
struct Budget(usize);
impl Budget {
    fn reserve<T>(&mut self, values: &mut Vec<T>, count: usize) -> Result<(), LoweringError> {
        let bytes = count
            .checked_mul(size_of::<T>())
            .ok_or(LoweringError::OwnedByteLimit)?;
        self.0 = self
            .0
            .checked_sub(bytes)
            .ok_or(LoweringError::OwnedByteLimit)?;
        values
            .try_reserve_exact(count)
            .map_err(|_| LoweringError::Allocation)
    }
}

#[cfg(test)]
mod tests;
