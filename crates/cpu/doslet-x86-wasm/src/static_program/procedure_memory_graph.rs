//! Acyclic whole procedures, stackified without a runtime dispatch loop.

use super::*;
use crate::static_program::Condition;

#[path = "procedure_memory_bounded.rs"]
pub(in crate::static_program) mod bounded;
#[path = "procedure_memory_finite.rs"]
pub(in crate::static_program) mod finite;

/// Bounds structured WASM nesting independently of the instruction count.
pub const MAX_PROCEDURE_BLOCKS: usize = 256;
pub const MAX_TOTAL_PROCEDURE_BLOCKS: usize = 65_536;
/// Maximum event-free instruction reservation accepted by bounded procedures.
pub const MAX_WORK_PER_CALL: u32 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureTerminatorKind {
    /// Resume the interpreter at the terminator address, without executing or
    /// charging that instruction. All completed prefix effects remain live.
    Exit,
    /// Synthetic subdivision, costing no guest work. The terminator address
    /// must equal this target's entry: it names the continuation boundary.
    Fallthrough(Address),
    Jump(Address),
    Branch {
        condition: Condition,
        taken: Address,
        not_taken: Address,
    },
    Return {
        width: Width,
        pop_bytes: u16,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureTerminator {
    /// Guest instruction address, or continuation boundary for Fallthrough.
    pub address: Address,
    pub kind: ProcedureTerminatorKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcedureBlock {
    pub entry: Address,
    pub instructions: Vec<ProcedureInstruction>,
    pub terminator: ProcedureTerminator,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcedureGraphFunction {
    pub id: ProcedureId,
    pub entry: Address,
    /// Input order is irrelevant. Every block must be reachable from entry;
    /// overlapping source ranges and unresolved targets are rejected. Cycles
    /// require explicit bounded or computation compilation. Guest addresses
    /// need not increase along edges.
    pub blocks: Vec<ProcedureBlock>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcedureGraphProgram {
    pub functions: Vec<ProcedureGraphFunction>,
}

/// Admission policy used before WASM generation. Disabled features retain the
/// ordinary acyclic compiler's supported scope.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcedureAnalysisOptions {
    pub ordinary_work_limit: Option<u32>,
    pub copy_work_limit: Option<u32>,
    pub computation_kernels: bool,
    pub pc24_locals: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ProcedureExecutionStrategy {
    Acyclic,
    BoundedLoop,
    Finite,
    GuardedFinite,
    Copy,
    Rle,
}

/// Proven execution behavior, available without generating WASM. Declared exits
/// describe IR terminators; memory refusals and bounded grants can additionally
/// resume at intermediate instruction boundaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcedureColor {
    pub id: ProcedureId,
    pub entry: Address,
    pub strategy: ProcedureExecutionStrategy,
    pub maximum_work: u64,
    pub interpreter_exits: Vec<Address>,
    pub returns: Vec<ProcedureReturn>,
    pub x87: Option<x87::X87Effects>,
}

/// Immutable IR and private proof plans. Public colors are observations and
/// cannot alter the lowering subsequently emitted by `compile`.
pub struct PreparedProcedureGraphProgram<'a> {
    program: &'a ProcedureGraphProgram,
    options: ProcedureCompileOptions,
    by_id: Vec<usize>,
    plans: Vec<GraphPlan>,
    procedures: Vec<Metadata>,
    colors: Vec<ProcedureColor>,
}

impl core::fmt::Debug for PreparedProcedureGraphProgram<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("PreparedProcedureGraphProgram")
            .field("options", &self.options)
            .field("colors", &self.colors)
            .finish_non_exhaustive()
    }
}

impl PreparedProcedureGraphProgram<'_> {
    /// Colors are sorted by procedure ID, independent of input order.
    pub fn colors(&self) -> &[ProcedureColor] {
        &self.colors
    }

    pub fn color(&self, id: ProcedureId) -> Option<&ProcedureColor> {
        self.colors
            .binary_search_by_key(&id, |color| color.id)
            .ok()
            .map(|index| &self.colors[index])
    }
}

impl ProcedureGraphFunction {
    /// Prove a finite computation without a runtime instruction grant. Register
    /// induction loops may branch and nest; every cycle needs a countdown or
    /// wrap-to-zero latch. One unknown dword counter uses a single entry guard
    /// against the derived trip cap, before loop effects. No guest data is
    /// inspected here. Returns a conservative whole-function reservation.
    pub fn finite_maximum_work(&self) -> Result<u32, ProcedureCompileError> {
        if self.blocks.iter().any(|b| {
            b.instructions
                .iter()
                .any(|i| matches!(i.operation, Operation::X87(_)))
        }) {
            return Err(ProcedureCompileError::CyclicGraph(self.id));
        }
        finite::plan(self, &validate_structure(self)?).map(|plan| plan.maximum_work as u32)
    }

    /// Validate the explicit PC24 ordinary graph and its balanced static stack
    /// topology without allocating WASM output.
    pub fn validate_with_pc24_work_limit(&self, limit: u32) -> Result<(), ProcedureCompileError> {
        self.validate_with_work_limit_policy(limit, true)?;
        let structure = validate_structure(self)?;
        x87::plan(self, &structure.targets).map(|_| ())
    }

    /// Check IR validity and supported bounded control flow without emitting a
    /// module. Setup scratch reservations use at most 16 usize values and two
    /// booleans per block, cumulatively; all input counts are bounded first.
    pub fn validate_with_work_limit(&self, limit: u32) -> Result<(), ProcedureCompileError> {
        self.validate_with_work_limit_policy(limit, false)
    }

    fn validate_with_work_limit_policy(
        &self,
        limit: u32,
        pc24: bool,
    ) -> Result<(), ProcedureCompileError> {
        if !(1..=MAX_WORK_PER_CALL).contains(&limit) {
            return Err(ProcedureCompileError::InvalidOptions);
        }
        if self.blocks.is_empty() {
            return Err(ProcedureCompileError::ProcedureHasNoBlocks(self.id));
        }
        if self.blocks.len() > MAX_PROCEDURE_BLOCKS {
            return Err(ProcedureCompileError::ProcedureHasTooManyBlocks(self.id));
        }
        let mut operations = 0usize;
        for block in &self.blocks {
            operations = operations
                .checked_add(block.instructions.len())
                .and_then(|n| n.checked_add(real_terminator(block.terminator.kind)))
                .filter(|&n| n <= MAX_PROCEDURE_OPERATIONS)
                .ok_or(ProcedureCompileError::ProcedureTooLong(self.id))?;
        }
        if !pc24
            && self.blocks.iter().any(|b| {
                b.instructions
                    .iter()
                    .any(|i| matches!(i.operation, Operation::X87(_)))
            })
        {
            return Err(ProcedureCompileError::InvalidOperation {
                procedure: self.id,
                operation: 0,
                reason: CompileError::InvalidMemoryOperation(RegionId(self.id.0)),
            });
        }
        validate(self, Some(limit)).map(|_| ())
    }
}

pub(in crate::static_program) struct GraphPlan {
    pub order: Vec<usize>,
    pub positions: Vec<usize>,
    pub targets: Vec<[Option<usize>; 2]>,
    pub maximum_work: u64,
    pub bounded: Option<bounded::LoopPlan>,
    pub finite: Option<finite::FinitePlan>,
    pub x87: Option<x87::Plan>,
}

fn real_terminator(kind: ProcedureTerminatorKind) -> usize {
    usize::from(!matches!(
        kind,
        ProcedureTerminatorKind::Fallthrough(_) | ProcedureTerminatorKind::Exit
    ))
}

impl ProcedureGraphProgram {
    /// Compile an opt-in whole acyclic procedure using the same sole-memory
    /// import, exports, frame/descriptor ABI and ownership contract as
    /// [`ProcedureProgram`]. Each completed JMP/Jcc/RET costs one instruction;
    /// the frame is charged for the actual path, while maximum_work reserves
    /// the longest path. Options count all operations and real terminators for
    /// graphs; existing straight-line option semantics remain unchanged.
    pub fn compile(
        &self,
        options: &ProcedureCompileOptions,
    ) -> Result<CompiledProcedureProgram, ProcedureCompileError> {
        self.compile_impl(options, None, None, false, false)
    }

    /// Also admit a single-entry linear loop ending in a backward JMP/Jcc.
    /// Other blocks may branch and join outside the loop. Cyclic calls execute
    /// at most min(descriptor.inline_grant, max_work_per_call) instructions,
    /// then SIDE_EXIT at the next instruction with all completed effects live.
    /// The owner must reserve maximum_work before entry; no callbacks or IRQ
    /// polls occur inside the function. Acyclic output remains unchanged.
    pub fn compile_with_work_limit(
        &self,
        options: &ProcedureCompileOptions,
        max_work_per_call: u32,
    ) -> Result<CompiledProcedureProgram, ProcedureCompileError> {
        if !(1..=MAX_WORK_PER_CALL).contains(&max_work_per_call) {
            return Err(ProcedureCompileError::InvalidOptions);
        }
        self.compile_impl(options, Some(max_work_per_call), None, false, false)
    }

    /// Also recognize the complete paired strided-byte-copy idiom. Recognized
    /// procedures use one native WASM copy loop after a single atomic preflight;
    /// other procedures retain the ordinary bounded lowering. The owner must
    /// reserve [`CopyKernel::actual_work`] before entry and exclusively own the
    /// device and mappings until completion. A failed proof returns SIDE_EXIT
    /// at entry with no completed work or effects. No IRQ polls occur inside.
    pub fn compile_with_copy_kernels(
        &self,
        options: &ProcedureCompileOptions,
        max_work_per_call: u32,
    ) -> Result<CompiledProcedureProgram, ProcedureCompileError> {
        if !(1..=MAX_WORK_PER_CALL).contains(&max_work_per_call) {
            return Err(ProcedureCompileError::InvalidOptions);
        }
        self.compile_impl(
            options,
            Some(max_work_per_call),
            Some(max_work_per_call),
            false,
            false,
        )
    }

    /// Admit semantic copy/RLE and finite reducible register-induction graphs,
    /// with nested loops and internal branches. Finite graphs reserve their
    /// whole static work bound and contain no instruction-grant checks. One
    /// unknown dword induction can use a derived trip-cap guard at loop entry;
    /// refusal preserves the completed prefix and resumes at that header.
    /// RLE calls require the owner's live finite-prefix/ordinary-memory proof
    /// and [`RleCall`] header. Other graphs retain ordinary bounded lowering.
    pub fn compile_with_computation_kernels(
        &self,
        options: &ProcedureCompileOptions,
        ordinary_work_limit: u32,
    ) -> Result<CompiledProcedureProgram, ProcedureCompileError> {
        self.compile_with_computation_kernels_and_work_limits(
            options,
            ordinary_work_limit,
            ordinary_work_limit,
        )
    }

    /// Compile ordinary bounded graphs and recognized semantic copies with
    /// separate reservations. This permits a shorter ordinary event-free grant
    /// while retaining the copy kernel's existing whole-computation contract.
    /// Both limits must be in `1..=MAX_WORK_PER_CALL`. Proven finite and RLE
    /// computations keep their independently derived reservations unchanged.
    /// The owner must still admit each function using its reported metadata.
    pub fn compile_with_computation_kernels_and_work_limits(
        &self,
        options: &ProcedureCompileOptions,
        ordinary_work_limit: u32,
        copy_work_limit: u32,
    ) -> Result<CompiledProcedureProgram, ProcedureCompileError> {
        if !(1..=MAX_WORK_PER_CALL).contains(&ordinary_work_limit)
            || !(1..=MAX_WORK_PER_CALL).contains(&copy_work_limit)
        {
            return Err(ProcedureCompileError::InvalidOptions);
        }
        self.compile_impl(
            options,
            Some(ordinary_work_limit),
            Some(copy_work_limit),
            true,
            false,
        )
    }

    /// Opt in to exact masked-PC24 locals in ordinary acyclic/bounded graphs.
    /// Scalar functions retain the existing semantic-kernel recognition. The
    /// owner must export/commit each function's declared x87 boundary and use
    /// its extended frame; unsupported numeric state refuses transactionally.
    pub fn compile_with_pc24_locals_and_work_limits(
        &self,
        options: &ProcedureCompileOptions,
        ordinary_work_limit: u32,
        copy_work_limit: u32,
    ) -> Result<CompiledProcedureProgram, ProcedureCompileError> {
        if !(1..=MAX_WORK_PER_CALL).contains(&ordinary_work_limit)
            || !(1..=MAX_WORK_PER_CALL).contains(&copy_work_limit)
        {
            return Err(ProcedureCompileError::InvalidOptions);
        }
        self.compile_impl(
            options,
            Some(ordinary_work_limit),
            Some(copy_work_limit),
            true,
            true,
        )
    }

    fn compile_impl(
        &self,
        options: &ProcedureCompileOptions,
        max_work_per_call: Option<u32>,
        copy_work_limit: Option<u32>,
        computation_kernels: bool,
        pc24: bool,
    ) -> Result<CompiledProcedureProgram, ProcedureCompileError> {
        self.prepare(
            options,
            &ProcedureAnalysisOptions {
                ordinary_work_limit: max_work_per_call,
                copy_work_limit,
                computation_kernels,
                pc24_locals: pc24,
            },
        )?
        .compile()
    }

    /// Validate and color procedures before allocating or emitting WASM.
    /// The returned plan borrows immutable input and owns the proven lowering.
    pub fn prepare<'a>(
        &'a self,
        options: &ProcedureCompileOptions,
        analysis: &ProcedureAnalysisOptions,
    ) -> Result<PreparedProcedureGraphProgram<'a>, ProcedureCompileError> {
        let max_work_per_call = analysis.ordinary_work_limit;
        let copy_work_limit = analysis.copy_work_limit;
        let computation_kernels = analysis.computation_kernels;
        let pc24 = analysis.pc24_locals;
        if [max_work_per_call, copy_work_limit]
            .into_iter()
            .flatten()
            .any(|limit| !(1..=MAX_WORK_PER_CALL).contains(&limit))
        {
            return Err(ProcedureCompileError::InvalidOptions);
        }
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
        // Bound all input-driven compiler allocations before reserving any.
        let mut total_operations = 0usize;
        let mut total_blocks = 0usize;
        for function in &self.functions {
            if function.blocks.is_empty() {
                return Err(ProcedureCompileError::ProcedureHasNoBlocks(function.id));
            }
            if function.blocks.len() > MAX_PROCEDURE_BLOCKS {
                return Err(ProcedureCompileError::ProcedureHasTooManyBlocks(
                    function.id,
                ));
            }
            total_blocks = total_blocks
                .checked_add(function.blocks.len())
                .filter(|&n| n <= MAX_TOTAL_PROCEDURE_BLOCKS)
                .ok_or(ProcedureCompileError::TooManyBlocks)?;
            let mut operations = 0usize;
            for block in &function.blocks {
                operations = operations
                    .checked_add(block.instructions.len())
                    .and_then(|n| n.checked_add(real_terminator(block.terminator.kind)))
                    .filter(|&n| n <= options.max_procedure_operations)
                    .ok_or(ProcedureCompileError::ProcedureTooLong(function.id))?;
            }
            total_operations = total_operations
                .checked_add(operations)
                .filter(|&n| n <= options.max_total_operations)
                .ok_or(ProcedureCompileError::TooManyOperations)?;
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
        let mut plans = reserved(by_id.len())?;
        let mut procedures = reserved(by_id.len())?;
        let mut colors = reserved(by_id.len())?;
        for &index in &by_id {
            let function = &self.functions[index];
            let structure = validate_structure(function)?;
            let x87_plan = x87::plan(function, &structure.targets)?;
            if x87_plan.is_some() && !pc24 {
                return Err(ProcedureCompileError::InvalidOperation {
                    procedure: function.id,
                    operation: 0,
                    reason: CompileError::InvalidMemoryOperation(RegionId(function.id.0)),
                });
            }
            let has_x87 = x87_plan.is_some();
            let rle_kernel = (computation_kernels && !has_x87)
                .then(|| rle_kernel::recognize(function))
                .flatten();
            let semantic_kernel = copy_work_limit
                .filter(|_| !has_x87)
                .and_then(|limit| copy_kernel::recognize(function, limit));
            // A recognized copy retains the same structural plan and metadata
            // as the legacy equal-limit API. Only ordinary graphs use the
            // shorter grant; changing it must not shrink atomic copy contracts.
            let plan_limit = if semantic_kernel.is_some() {
                copy_work_limit
            } else {
                max_work_per_call
            };
            let mut plan = if rle_kernel.is_some() {
                GraphPlan {
                    order: Vec::new(),
                    positions: Vec::new(),
                    targets: structure.targets,
                    maximum_work: u64::from(u32::MAX),
                    bounded: None,
                    finite: None,
                    x87: None,
                }
            } else {
                // A failed termination proof is not an admission: preserve the
                // ordinary bounded fallback, including its original errors.
                if computation_kernels && semantic_kernel.is_none() && !has_x87 {
                    match finite::plan(function, &structure) {
                        Ok(proven) if !proven.finite.as_ref().unwrap().loops.is_empty() => proven,
                        Ok(_) | Err(ProcedureCompileError::CyclicGraph(_)) => {
                            plan(function, structure, plan_limit)?
                        }
                        Err(error) => return Err(error),
                    }
                } else {
                    plan(function, structure, plan_limit)?
                }
            };
            plan.x87 = x87_plan;
            let mut export_name = String::new();
            export_name
                .try_reserve_exact("memory_procedure_".len() + 10)
                .map_err(|_| ProcedureCompileError::Allocation)?;
            write!(export_name, "memory_procedure_{}", function.id.0)
                .map_err(|_| ProcedureCompileError::Allocation)?;
            let finite_maximum_work = (plan.finite.is_some() && semantic_kernel.is_none())
                .then_some(plan.maximum_work as u32);
            let mut finite_loop_guards = Vec::new();
            if let Some(finite) = plan
                .finite
                .as_ref()
                .filter(|_| finite_maximum_work.is_some())
            {
                finite_loop_guards = reserved(finite.loops.iter().filter(|lp| lp.guarded).count())?;
                for lp in finite.loops.iter().filter(|lp| lp.guarded) {
                    finite_loop_guards.push(FiniteLoopGuard {
                        entry: function.blocks[lp.header].entry,
                        counter: lp.counter,
                        decrement: lp.decrement,
                        maximum_trips: lp.trips,
                    });
                }
            }
            let strategy = if rle_kernel.is_some() {
                ProcedureExecutionStrategy::Rle
            } else if semantic_kernel.is_some() {
                ProcedureExecutionStrategy::Copy
            } else if plan.finite.is_some() {
                if finite_loop_guards.is_empty() {
                    ProcedureExecutionStrategy::Finite
                } else {
                    ProcedureExecutionStrategy::GuardedFinite
                }
            } else if plan.bounded.is_some() {
                ProcedureExecutionStrategy::BoundedLoop
            } else {
                ProcedureExecutionStrategy::Acyclic
            };
            let mut interpreter_exits = reserved(function.blocks.len())?;
            let mut returns = reserved(function.blocks.len())?;
            for block in &function.blocks {
                match block.terminator.kind {
                    ProcedureTerminatorKind::Exit => {
                        interpreter_exits.push(block.terminator.address)
                    }
                    ProcedureTerminatorKind::Return { width, pop_bytes } => {
                        returns.push(ProcedureReturn {
                            address: block.terminator.address,
                            width,
                            pop_bytes,
                        })
                    }
                    _ => (),
                }
            }
            interpreter_exits.sort_unstable();
            returns.sort_unstable_by_key(|ret| ret.address);
            colors.push(ProcedureColor {
                id: function.id,
                entry: function.entry,
                strategy,
                maximum_work: plan.maximum_work,
                interpreter_exits,
                returns,
                x87: plan
                    .x87
                    .as_ref()
                    .map(|plan| plan.effects.try_clone())
                    .transpose()?,
            });
            procedures.push(Metadata {
                x87: plan.x87.as_ref().map(|p| p.permissions),
                id: function.id,
                entry: function.entry,
                export_name,
                maximum_work: plan.maximum_work,
                semantic_kernel,
                rle_kernel,
                finite_maximum_work,
                finite_loop_guards,
            });
            plans.push(plan);
        }
        Ok(PreparedProcedureGraphProgram {
            program: self,
            options: *options,
            by_id,
            plans,
            procedures,
            colors,
        })
    }
}

impl PreparedProcedureGraphProgram<'_> {
    /// Emit only the validated, immutable plan owned by this preparation.
    pub fn compile(self) -> Result<CompiledProcedureProgram, ProcedureCompileError> {
        let Self {
            program,
            options,
            by_id,
            plans,
            procedures,
            ..
        } = self;
        let bytes = encode_procedures(&procedures, options.max_output_bytes, |position| {
            if procedures[position].rle_kernel.is_some() {
                return emit::memory_rle_kernel_body(
                    program.functions[by_id[position]].entry,
                    options.max_output_bytes,
                );
            }
            if let Some(kernel) = procedures[position].semantic_kernel {
                return emit::memory_copy_kernel_body(
                    program.functions[by_id[position]].entry,
                    kernel,
                    options.max_output_bytes,
                );
            }
            emit::memory_procedure_graph_body(
                &program.functions[by_id[position]],
                &plans[position],
                options.max_output_bytes,
            )
        })?;
        Ok(CompiledProcedureProgram { bytes, procedures })
    }
}

fn validate(
    function: &ProcedureGraphFunction,
    max_work_per_call: Option<u32>,
) -> Result<GraphPlan, ProcedureCompileError> {
    plan(function, validate_structure(function)?, max_work_per_call)
}

pub(super) struct GraphStructure {
    targets: Vec<[Option<usize>; 2]>,
    by_entry: Vec<usize>,
    entry: usize,
}

/// Validate all source and operation invariants independently of the supported
/// ordinary loop shapes. Semantic recognition never bypasses these checks.
pub(super) fn validate_structure(
    function: &ProcedureGraphFunction,
) -> Result<GraphStructure, ProcedureCompileError> {
    let invalid = || ProcedureCompileError::InvalidAddresses(function.id);
    let count = function.blocks.len();
    if count == 0 {
        return Err(ProcedureCompileError::ProcedureHasNoBlocks(function.id));
    }
    if count > MAX_PROCEDURE_BLOCKS {
        return Err(ProcedureCompileError::ProcedureHasTooManyBlocks(
            function.id,
        ));
    }
    let operations = function.blocks.iter().try_fold(0usize, |total, block| {
        total
            .checked_add(block.instructions.len())?
            .checked_add(real_terminator(block.terminator.kind))
    });
    if operations.is_none_or(|n| n > MAX_PROCEDURE_OPERATIONS) {
        return Err(ProcedureCompileError::ProcedureTooLong(function.id));
    }
    let mut by_entry = reserved(count)?;
    by_entry.extend(0..count);
    by_entry.sort_unstable_by_key(|&i| function.blocks[i].entry);
    for pair in by_entry.windows(2) {
        if function.blocks[pair[0]].entry == function.blocks[pair[1]].entry {
            return Err(ProcedureCompileError::DuplicateBlock {
                procedure: function.id,
                entry: function.blocks[pair[0]].entry,
            });
        }
    }
    let resolve = |target: Address| {
        by_entry
            .binary_search_by_key(&target, |&i| function.blocks[i].entry)
            .map(|i| by_entry[i])
            .map_err(|_| ProcedureCompileError::UnresolvedTarget {
                procedure: function.id,
                target,
            })
    };
    let entry = resolve(function.entry)?;
    let mut targets = reserved(count)?;
    targets.resize(count, [None; 2]);
    let mut previous_end = None;
    let mut operation = 0;
    for &index in &by_entry {
        let block = &function.blocks[index];
        if block.entry.space != function.entry.space
            || block.terminator.address.space != function.entry.space
            || previous_end.is_some_and(|end| block.entry.offset <= end)
        {
            return Err(invalid());
        }
        let mut previous = None;
        for (position, instruction) in block.instructions.iter().enumerate() {
            if instruction.address.space != function.entry.space
                || (position == 0 && instruction.address != block.entry)
                || previous.is_some_and(|offset| instruction.address.offset <= offset)
            {
                return Err(invalid());
            }
            previous = Some(instruction.address.offset);
            super::super::validate_operation(&instruction.operation, RegionId(function.id.0), true)
                .map_err(|reason| ProcedureCompileError::InvalidOperation {
                    procedure: function.id,
                    operation,
                    reason,
                })?;
            operation += 1;
        }
        if previous.map_or(block.terminator.address != block.entry, |offset| {
            block.terminator.address.offset <= offset
        }) {
            return Err(invalid());
        }
        let address = block.terminator.address;
        let edges = match block.terminator.kind {
            ProcedureTerminatorKind::Exit => {
                previous_end = Some(address.offset);
                [None; 2]
            }
            ProcedureTerminatorKind::Fallthrough(target) => {
                // A synthetic edge has no source instruction of its own.
                if target != address || block.instructions.is_empty() {
                    return Err(invalid());
                }
                previous_end = address.offset.checked_sub(1);
                [Some(resolve(target)?), None]
            }
            ProcedureTerminatorKind::Jump(target) => {
                previous_end = Some(address.offset);
                [Some(resolve(target)?), None]
            }
            ProcedureTerminatorKind::Branch {
                taken, not_taken, ..
            } => {
                previous_end = Some(address.offset);
                [Some(resolve(taken)?), Some(resolve(not_taken)?)]
            }
            ProcedureTerminatorKind::Return { width, .. } => {
                if width == Width::Byte {
                    return Err(ProcedureCompileError::InvalidReturn(function.id));
                }
                previous_end = Some(address.offset);
                [None; 2]
            }
        };
        targets[index] = edges;
    }
    let mut reachable = reserved(count)?;
    reachable.resize(count, false);
    let mut stack = reserved(count)?;
    stack.push(entry);
    reachable[entry] = true;
    while let Some(index) = stack.pop() {
        for target in targets[index].into_iter().flatten() {
            if !reachable[target] {
                reachable[target] = true;
                stack.push(target);
            }
        }
    }
    for &index in &by_entry {
        if !reachable[index] {
            return Err(ProcedureCompileError::UnreachableBlock {
                procedure: function.id,
                entry: function.blocks[index].entry,
            });
        }
    }
    Ok(GraphStructure {
        targets,
        by_entry,
        entry,
    })
}

fn plan(
    function: &ProcedureGraphFunction,
    structure: GraphStructure,
    max_work_per_call: Option<u32>,
) -> Result<GraphPlan, ProcedureCompileError> {
    let GraphStructure {
        targets,
        by_entry,
        entry,
    } = structure;
    let count = function.blocks.len();
    let mut indegree = reserved(count)?;
    indegree.resize(count, 0usize);
    for edges in &targets {
        for &target in edges.iter().flatten() {
            indegree[target] += 1;
        }
    }
    let mut order = reserved(count)?;
    let mut positions = reserved(count)?;
    positions.resize(count, usize::MAX);
    // Small bounded graph: select the lowest-address ready block so input
    // order cannot affect bytes. This is a host setup pass, never guest work.
    for position in 0..count {
        let Some(index) = by_entry
            .iter()
            .copied()
            .find(|&i| positions[i] == usize::MAX && indegree[i] == 0)
        else {
            return match max_work_per_call {
                Some(limit) => bounded::plan(function, targets, &by_entry, entry, limit),
                None => Err(ProcedureCompileError::CyclicGraph(function.id)),
            };
        };
        positions[index] = position;
        order.push(index);
        for target in targets[index].into_iter().flatten() {
            indegree[target] -= 1;
        }
    }
    debug_assert_eq!(order[0], entry);
    let mut longest = reserved(count)?;
    longest.resize(count, 0u64);
    for &index in order.iter().rev() {
        let continuation = targets[index]
            .into_iter()
            .flatten()
            .map(|i| longest[i])
            .max()
            .unwrap_or(0);
        let block = &function.blocks[index];
        longest[index] = continuation
            + block.instructions.len() as u64
            + real_terminator(block.terminator.kind) as u64;
    }
    Ok(GraphPlan {
        order,
        positions,
        targets,
        maximum_work: longest[entry],
        bounded: None,
        finite: None,
        x87: None,
    })
}

#[cfg(test)]
#[path = "procedure_colors_tests.rs"]
mod color_tests;
