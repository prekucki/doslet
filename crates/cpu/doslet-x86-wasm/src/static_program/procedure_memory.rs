//! Opt-in finite procedures over live registers and ordinary RAM.
//!
//! This compiler accepts explicit CPU IR, without decoding or recognizing x86.
//! Each function has ordered instruction source addresses and one final near
//! RET. Its `memory_procedure_<id>(frame: i32) -> i32` export imports only the
//! unshared wasm32 `doslet_static.memory` and uses the unchanged [`super::Frame`]
//! followed by [`super::memory::Descriptor`]. No runtime callbacks, guest CALLs,
//! source checks or IRQ polls occur inside the body.
//! [`ProcedureGraphProgram`] separately admits reachable acyclic blocks with
//! addressed JMP/Jcc/RET terminators, joins and synthetic fallthroughs. It uses
//! forward structured WASM branches and keeps registers/flags live across edges;
//! the original acyclic control-flow emitter remains unchanged. Explicit
//! [`ProcedureGraphProgram::compile_with_work_limit`] also supports one linear
//! natural loop with a conditional or unconditional latch. Cyclic functions
//! stop before exceeding the descriptor grant or compiled per-call limit.
//! Whole loop iterations check the grant once; a short final prefix checks
//! each remaining instruction. Unsupported cyclic shapes are rejected.
//! [`ProcedureGraphProgram::compile_with_computation_kernels`] additionally
//! proves reducible INC/DEC induction graphs with internal branches and nested
//! loops. Narrow counters terminate by wrapping to zero; dword counters need a
//! dominating constant or one derived trip-cap guard per dynamic loop entry.
//! Proven bodies contain no instruction-budget, fuel or IRQ checks. A failed
//! entry guard resumes precisely at the loop header with prior effects live.
//! Terminal [`ProcedureTerminatorKind::Exit`] resumes the interpreter without
//! executing or charging the instruction at its address.
//!
//! The owner must bind every source byte/CPU context to a valid frozen proof,
//! mark every compiled source page in the descriptor's code-page table, reserve
//! [`CompiledProcedureProgram::maximum_work`] before entry, and exclusively own
//! stable frame, mappings, RAM and metadata until return. The initial frame
//! address must match the selected function entry; frame pointer zero is valid.
//! Store proofs reject
//! protected source pages and exhausted generations before changing anything.
//! RAM backing, frame/descriptor, generation and protection tables must be
//! disjoint; generated store proofs enforce their full advertised spans.
//!
//! GPRs/EFLAGS load once and flush at one common exit. [`COMPLETE`] means the
//! actual guest RET read its target and adjusted SP/ESP (including `pop_bytes`).
//! [`SIDE_EXIT`] means a memory proof failed, or a bounded cyclic invocation
//! exhausted its grant, before the next instruction's effects. The frame
//! address identifies that precise instruction, all prior stores
//! and local register effects remain committed, and only the executed prefix is
//! charged. The owner can interpret the current instruction without replaying
//! the prefix. The frame's charged counter saturates with COUNTER_OVERFLOW;
//! budget and reserved words are unchanged. Descriptor RAM-write/page-generation
//! effects update inline, while device latches/read counts and inline admission
//! grant/reserved fields remain untouched. RET charges work only on completion.
//!
//! Every existing scalar/memory Operation is accepted with the shared region
//! compiler operand validation, alias handling and arithmetic/flag emitter.
//! Reads, stores and stack operations accept ordinary RAM only; MMIO declines.
//! The shared store proof requires RAM linear_base >= 0x500: advertise the
//! ordinary span above the interrupt-vector/BIOS area, adjusting its backing
//! pointer accordingly. Generation/protection indices remain absolute physical
//! 4-KiB page indices, independent of that advertised span base.
//! Word/dword RET and PUSH/POP use the descriptor's independent live SS.B width.
//! Input function/operation limits are checked before compiler allocations;
//! fallible reservations and a bounded output cap limit compiler memory use.

use super::{Address, CompileError, Operation, RegionId, Width, abi, emit};
use alloc::{string::String, vec::Vec};
use core::fmt::Write;
use wasm_encoder::{
    CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection, ImportSection,
    MemoryType, Module, Section, TypeSection, ValType,
};

#[path = "procedure_memory_bundle.rs"]
pub mod bundle;
#[path = "copy_kernel.rs"]
pub(super) mod copy_kernel;
#[path = "procedure_memory_graph.rs"]
pub(super) mod graph;
#[path = "rle_kernel.rs"]
pub(super) mod rle_kernel;
pub use copy_kernel::CopyKernel;
pub use graph::{
    MAX_PROCEDURE_BLOCKS, MAX_TOTAL_PROCEDURE_BLOCKS, MAX_WORK_PER_CALL, ProcedureBlock,
    ProcedureGraphFunction, ProcedureGraphProgram, ProcedureTerminator, ProcedureTerminatorKind,
};
pub use rle_kernel::{RleCall, RleKernel};

/// A finite computation's dynamic loop-entry proof, exposed for setup-time
/// manifests and independent original-instruction reference execution.
/// A decrement uses the live register value as its trip count; an increment
/// uses its wrapping negation. Zero represents 2^32 trips and must decline.
/// Before executing the instruction at `entry`, refuse counts outside
/// `1..=maximum_trips`, preserving all completed prefix effects and work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FiniteLoopGuard {
    pub entry: Address,
    pub counter: super::Register,
    pub decrement: bool,
    pub maximum_trips: u32,
}

pub const COMPLETE: u32 = 0;
pub const SIDE_EXIT: u32 = 1;
pub const MAX_PROCEDURES: usize = 4096;
pub const MAX_PROCEDURE_OPERATIONS: usize = 4096;
pub const MAX_TOTAL_OPERATIONS: usize = 65_536;
pub const MAX_OUTPUT_BYTES: usize = super::MAX_OUTPUT_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProcedureId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureInstruction {
    /// Actual guest EIP before this instruction, in the entry's code space.
    pub address: Address,
    pub operation: Operation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureReturn {
    pub address: Address,
    pub width: Width,
    pub pop_bytes: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcedureFunction {
    pub id: ProcedureId,
    pub entry: Address,
    pub instructions: Vec<ProcedureInstruction>,
    pub terminator: ProcedureReturn,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcedureProgram {
    pub functions: Vec<ProcedureFunction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureCompileOptions {
    pub max_procedures: usize,
    pub max_procedure_operations: usize,
    /// Counts explicit operations; each function's final RET costs one extra.
    pub max_total_operations: usize,
    pub max_output_bytes: usize,
}
impl Default for ProcedureCompileOptions {
    fn default() -> Self {
        Self {
            max_procedures: MAX_PROCEDURES,
            max_procedure_operations: MAX_PROCEDURE_OPERATIONS,
            max_total_operations: MAX_TOTAL_OPERATIONS,
            max_output_bytes: MAX_OUTPUT_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureCompileError {
    InvalidOptions,
    TooManyProcedures,
    TooManyOperations,
    ProcedureTooLong(ProcedureId),
    DuplicateId(ProcedureId),
    InvalidAddresses(ProcedureId),
    InvalidReturn(ProcedureId),
    TooManyBlocks,
    ProcedureHasNoBlocks(ProcedureId),
    ProcedureHasTooManyBlocks(ProcedureId),
    DuplicateBlock {
        procedure: ProcedureId,
        entry: Address,
    },
    UnresolvedTarget {
        procedure: ProcedureId,
        target: Address,
    },
    UnreachableBlock {
        procedure: ProcedureId,
        entry: Address,
    },
    CyclicGraph(ProcedureId),
    InvalidOperation {
        procedure: ProcedureId,
        operation: usize,
        reason: CompileError,
    },
    OutputTooLarge,
    Allocation,
}
impl core::fmt::Display for ProcedureCompileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "memory procedure compilation: {self:?}")
    }
}
impl core::error::Error for ProcedureCompileError {}

#[derive(Debug)]
struct Metadata {
    id: ProcedureId,
    entry: Address,
    export_name: String,
    maximum_work: u64,
    semantic_kernel: Option<CopyKernel>,
    rle_kernel: Option<RleKernel>,
    finite_maximum_work: Option<u32>,
    finite_loop_guards: Vec<FiniteLoopGuard>,
}

#[derive(Debug)]
pub struct CompiledProcedureProgram {
    bytes: Vec<u8>,
    procedures: Vec<Metadata>,
}
impl CompiledProcedureProgram {
    pub fn wasm(&self) -> &[u8] {
        &self.bytes
    }
    pub fn into_wasm(self) -> Vec<u8> {
        self.bytes
    }
    pub fn procedure_ids(&self) -> impl ExactSizeIterator<Item = ProcedureId> + '_ {
        self.procedures.iter().map(|p| p.id)
    }
    pub fn export_name(&self, id: ProcedureId) -> Option<&str> {
        self.metadata(id).map(|p| p.export_name.as_str())
    }
    pub fn entry(&self, id: ProcedureId) -> Option<Address> {
        self.metadata(id).map(|p| p.entry)
    }
    /// Longest acyclic path including RET, or the declared per-call limit for
    /// bounded cyclic graphs. A semantic RLE kernel advertises the u32 work
    /// ceiling; its exact live work is established by the owner before entry.
    /// Execution charges only completed instructions.
    pub fn maximum_work(&self, id: ProcedureId) -> Option<u64> {
        self.metadata(id).map(|p| p.maximum_work)
    }
    /// A statically proven no-grant computation, including an acyclic prefix
    /// ending at an interpreter exit. Reserve this whole bound before entry;
    /// execution still charges only the actual completed path.
    pub fn finite_maximum_work(&self, id: ProcedureId) -> Option<u32> {
        self.metadata(id).and_then(|p| p.finite_maximum_work)
    }
    /// Entry guards for the selected finite computation. An empty slice means
    /// all loop trip bounds were proved statically; None means this ID has no
    /// selected finite computation. Original-instruction references must apply
    /// every guard at each dynamic entry before executing its instruction.
    pub fn finite_loop_guards(&self, id: ProcedureId) -> Option<&[FiniteLoopGuard]> {
        let metadata = self.metadata(id)?;
        metadata
            .finite_maximum_work
            .map(|_| metadata.finite_loop_guards.as_slice())
    }
    /// An explicitly enabled, all-or-decline semantic replacement. Its dynamic
    /// work must be reserved by the owner before calling the exported function.
    pub fn semantic_kernel(&self, id: ProcedureId) -> Option<CopyKernel> {
        self.metadata(id).and_then(|p| p.semantic_kernel)
    }
    /// An opt-in whole RLE procedure. The owner must establish a fresh finite
    /// source-prefix proof and populate [`RleCall`] before every invocation.
    pub fn rle_kernel(&self, id: ProcedureId) -> Option<RleKernel> {
        self.metadata(id).and_then(|p| p.rle_kernel)
    }
    fn metadata(&self, id: ProcedureId) -> Option<&Metadata> {
        self.procedures
            .binary_search_by_key(&id, |p| p.id)
            .ok()
            .map(|i| &self.procedures[i])
    }
}

fn reserved<T>(capacity: usize) -> Result<Vec<T>, ProcedureCompileError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| ProcedureCompileError::Allocation)?;
    Ok(values)
}

impl ProcedureProgram {
    /// Function order does not affect output. All addresses in each function
    /// must share its code space and strictly increase, starting at its entry;
    /// an empty body starts with its RET. Address wrapping is unsupported.
    pub fn compile(
        &self,
        options: &ProcedureCompileOptions,
    ) -> Result<CompiledProcedureProgram, ProcedureCompileError> {
        if options.max_procedures > MAX_PROCEDURES
            || options.max_procedure_operations > MAX_PROCEDURE_OPERATIONS
            || options.max_total_operations > MAX_TOTAL_OPERATIONS
            || options.max_output_bytes > MAX_OUTPUT_BYTES
        {
            return Err(ProcedureCompileError::InvalidOptions);
        }
        if self.functions.len() > options.max_procedures {
            return Err(ProcedureCompileError::TooManyProcedures);
        }
        let mut total = 0usize;
        for function in &self.functions {
            if function.instructions.len() > options.max_procedure_operations {
                return Err(ProcedureCompileError::ProcedureTooLong(function.id));
            }
            total = total
                .checked_add(function.instructions.len())
                .ok_or(ProcedureCompileError::TooManyOperations)?;
            if total > options.max_total_operations {
                return Err(ProcedureCompileError::TooManyOperations);
            }
        }
        let mut by_id = reserved(self.functions.len())?;
        by_id.extend(0..self.functions.len());
        by_id.sort_unstable_by_key(|&i| self.functions[i].id);
        for pair in by_id.windows(2) {
            if self.functions[pair[0]].id == self.functions[pair[1]].id {
                return Err(ProcedureCompileError::DuplicateId(
                    self.functions[pair[0]].id,
                ));
            }
        }
        // Validate every body before allocating/emitting the module.
        for &index in &by_id {
            let function = &self.functions[index];
            let mut previous = None;
            for (position, instruction) in function.instructions.iter().enumerate() {
                if instruction.address.space != function.entry.space
                    || (position == 0 && instruction.address != function.entry)
                    || previous.is_some_and(|offset| instruction.address.offset <= offset)
                {
                    return Err(ProcedureCompileError::InvalidAddresses(function.id));
                }
                previous = Some(instruction.address.offset);
                super::validate_operation(&instruction.operation, RegionId(function.id.0), true)
                    .map_err(|reason| ProcedureCompileError::InvalidOperation {
                        procedure: function.id,
                        operation: position,
                        reason,
                    })?;
            }
            if function.terminator.address.space != function.entry.space
                || previous.map_or(function.terminator.address != function.entry, |offset| {
                    function.terminator.address.offset <= offset
                })
            {
                return Err(ProcedureCompileError::InvalidAddresses(function.id));
            }
            if function.terminator.width == Width::Byte {
                return Err(ProcedureCompileError::InvalidReturn(function.id));
            }
        }
        let mut procedures = reserved(by_id.len())?;
        for &index in &by_id {
            let function = &self.functions[index];
            let mut export_name = String::new();
            export_name
                .try_reserve_exact("memory_procedure_".len() + 10)
                .map_err(|_| ProcedureCompileError::Allocation)?;
            write!(export_name, "memory_procedure_{}", function.id.0)
                .map_err(|_| ProcedureCompileError::Allocation)?;
            procedures.push(Metadata {
                id: function.id,
                entry: function.entry,
                export_name,
                maximum_work: function.instructions.len() as u64 + 1,
                semantic_kernel: None,
                rle_kernel: None,
                finite_maximum_work: None,
                finite_loop_guards: Vec::new(),
            });
        }
        let bytes = module(self, options, &by_id, &procedures)?;
        Ok(CompiledProcedureProgram { bytes, procedures })
    }
}

fn leb_len(mut value: usize) -> usize {
    let mut count = 1;
    while value >= 128 {
        value >>= 7;
        count += 1;
    }
    count
}

fn module(
    program: &ProcedureProgram,
    options: &ProcedureCompileOptions,
    by_id: &[usize],
    metadata: &[Metadata],
) -> Result<Vec<u8>, ProcedureCompileError> {
    encode_procedures(metadata, options.max_output_bytes, |position| {
        emit::memory_procedure_body(
            &program.functions[by_id[position]],
            options.max_output_bytes,
        )
    })
}

fn encode_procedures(
    metadata: &[Metadata],
    max_output_bytes: usize,
    mut body: impl FnMut(usize) -> Result<Function, ProcedureCompileError>,
) -> Result<Vec<u8>, ProcedureCompileError> {
    let mut types = TypeSection::new();
    types.ty().function([ValType::I32], [ValType::I32]);
    let mut imports = ImportSection::new();
    imports.import(
        abi::IMPORT_MODULE,
        abi::MEMORY_IMPORT,
        EntityType::Memory(MemoryType {
            minimum: 1,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        }),
    );
    let mut functions = FunctionSection::new();
    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    let mut code = CodeSection::new();
    for (position, procedure) in metadata.iter().enumerate() {
        functions.function(0);
        exports.export(&procedure.export_name, ExportKind::Func, position as u32);
        let body = body(position)?;
        code.byte_len()
            .checked_add(leb_len(body.byte_len()))
            .and_then(|n| n.checked_add(body.byte_len()))
            .filter(|&n| n <= max_output_bytes)
            .ok_or(ProcedureCompileError::OutputTooLarge)?;
        code.function(&body);
    }
    let header_capacity = metadata
        .len()
        .checked_mul(48)
        .and_then(|n| n.checked_add(160))
        .ok_or(ProcedureCompileError::OutputTooLarge)?;
    let mut bytes = reserved(header_capacity)?;
    bytes.extend_from_slice(&Module::HEADER);
    types.append_to(&mut bytes);
    imports.append_to(&mut bytes);
    functions.append_to(&mut bytes);
    exports.append_to(&mut bytes);
    let payload = code
        .byte_len()
        .checked_add(leb_len(code.len() as usize))
        .ok_or(ProcedureCompileError::OutputTooLarge)?;
    let total = bytes
        .len()
        .checked_add(1)
        .and_then(|n| n.checked_add(leb_len(payload)))
        .and_then(|n| n.checked_add(payload))
        .filter(|&n| n <= max_output_bytes)
        .ok_or(ProcedureCompileError::OutputTooLarge)?;
    bytes
        .try_reserve_exact(total - bytes.len())
        .map_err(|_| ProcedureCompileError::Allocation)?;
    code.append_to(&mut bytes);
    debug_assert_eq!(bytes.len(), total);
    Ok(bytes)
}

#[cfg(test)]
#[path = "procedure_memory_tests.rs"]
mod tests;
