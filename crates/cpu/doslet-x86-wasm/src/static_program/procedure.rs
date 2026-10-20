//! Opt-in compiler for explicitly supplied, finite register-only procedures.
//!
//! This is a CPU IR API, not an x86 procedure recognizer. A function contains
//! scalar register operations, direct calls and one implicit final guest RET.
//! Calls form a DAG; every invocation executes its complete body. There are no
//! imports, memory, tables, runtime callbacks, hidden fallbacks or guest stack
//! effects. Every explicit SP/ESP operand is rejected, including reads; ESP is
//! passed through unchanged. LEA and all memory/stack operations are excluded.
//! The accepted Execute variants are Nop, Move, Extend, SignedMultiply,
//! WideMultiply, Alu, Shift, DoubleShift, Inc, Dec, Neg, Not and Exchange.
//! They reuse the region compiler's scalar arithmetic and complete flag logic,
//! including byte/high-byte and word alias preservation. Operand widths and
//! immediates must match; word DoubleShift with CL or an immediate masked count
//! above 16 is rejected, matching the interpreter's undefined-count boundary.
//!
//! Each `procedure_<decimal id>` export takes nine i32 values: EAX, ECX, EDX,
//! EBX, ESP, EBP, ESI, EDI and EFLAGS. It returns those nine i32 values followed
//! by one i64 unsigned work count. Internal native WASM calls pass only the nine
//! register/flag values, without a guest frame or scheduling state. The export
//! appends its compile-known expanded cost once: each Execute, each guest CALL
//! and each implicit guest RET costs one, including repeated shared callees.
//! No tail-call extension is required. Generated scalar shifts can contain
//! structured flag/count conditionals, but the IR has no guest branches/loops.
//!
//! The owner must reserve the entire reported cost before entry and reconcile
//! actual device time at the return boundary. There is no execution-time yield.
//! Compiling this IR does **not** prove that real x86 CALL/RET stack effects can
//! be elided: return-address observability, stack bytes/call frames, source-code
//! guards and a top-level interpreter adapter remain separate obligations.
//! Source validity may be enforced entirely by the interpreter's decoded-entry
//! binding, including all callee dependencies; generated functions need not
//! repeat that validation while the binding remains valid.
//! This API alone does not establish execution of game procedures.
//!
//! All input lengths are checked before allocating compiler indexes. Index,
//! metadata and output reservations are fallible. `wasm-encoder`'s internal
//! allocations are infallible but bounded by these hard limits and by checking
//! output growth after each scalar operation and before appending each body.

use super::{Operation, Register, ShiftCount, Source, Width, emit};
use alloc::{string::String, vec::Vec};
use core::fmt::Write;
use wasm_encoder::{
    CodeSection, ExportKind, ExportSection, Function, FunctionSection, Instruction as W, Module,
    Section, TypeSection, ValType,
};

pub const MAX_PROCEDURES: usize = 4096;
pub const MAX_PROCEDURE_OPERATIONS: usize = 4096;
pub const MAX_TOTAL_OPERATIONS: usize = 65_536;
/// Includes the outermost procedure, so a leaf has depth one.
pub const MAX_CALL_DEPTH: usize = 64;
pub const MAX_EXPANDED_WORK: u64 = 1_048_576;
pub const MAX_OUTPUT_BYTES: usize = super::MAX_OUTPUT_BYTES;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProcedureId(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureOperation {
    Execute(Operation),
    /// One guest CALL followed by the complete callee, including its RET.
    Call(ProcedureId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcedureFunction {
    pub id: ProcedureId,
    /// One implicit guest RET follows the final operation, even in an empty body.
    pub operations: Vec<ProcedureOperation>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcedureProgram {
    pub functions: Vec<ProcedureFunction>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureCompileOptions {
    pub max_procedures: usize,
    pub max_procedure_operations: usize,
    /// Raw Execute/Call input entries; expanded callee work has its own limit.
    pub max_total_operations: usize,
    pub max_call_depth: usize,
    /// Independent upper bound on each export's full Execute/CALL/RET cost.
    pub max_expanded_work: u64,
    pub max_output_bytes: usize,
}
impl Default for ProcedureCompileOptions {
    fn default() -> Self {
        Self {
            max_procedures: MAX_PROCEDURES,
            max_procedure_operations: MAX_PROCEDURE_OPERATIONS,
            max_total_operations: MAX_TOTAL_OPERATIONS,
            max_call_depth: MAX_CALL_DEPTH,
            max_expanded_work: MAX_EXPANDED_WORK,
            max_output_bytes: MAX_OUTPUT_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureOperationError {
    Unsupported,
    StackRegister,
    WidthMismatch,
    ImmediateOutOfRange,
    InvalidShiftCount,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureCompileError {
    InvalidOptions,
    TooManyProcedures,
    TooManyOperations,
    ProcedureTooLong(ProcedureId),
    DuplicateId(ProcedureId),
    InvalidOperation {
        procedure: ProcedureId,
        operation: usize,
        reason: ProcedureOperationError,
    },
    UnknownCallee {
        from: ProcedureId,
        target: ProcedureId,
    },
    Cycle {
        from: ProcedureId,
        target: ProcedureId,
    },
    CallDepthExceeded(ProcedureId),
    ExpandedWorkExceeded(ProcedureId),
    OutputTooLarge,
    Allocation,
}
impl core::fmt::Display for ProcedureCompileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "procedure compilation: {self:?}")
    }
}
impl core::error::Error for ProcedureCompileError {}

#[derive(Debug)]
struct ProcedureMetadata {
    id: ProcedureId,
    export_name: String,
    expanded_work: u64,
    call_depth: usize,
}

#[derive(Debug)]
pub struct CompiledProcedureProgram {
    bytes: Vec<u8>,
    procedures: Vec<ProcedureMetadata>,
}
impl CompiledProcedureProgram {
    pub fn wasm(&self) -> &[u8] {
        &self.bytes
    }
    pub fn into_wasm(self) -> Vec<u8> {
        self.bytes
    }
    /// IDs in ascending order, independent of the input function order.
    pub fn procedure_ids(&self) -> impl ExactSizeIterator<Item = ProcedureId> + '_ {
        self.procedures.iter().map(|p| p.id)
    }
    pub fn export_name(&self, id: ProcedureId) -> Option<&str> {
        self.metadata(id).map(|p| p.export_name.as_str())
    }
    pub fn expanded_work(&self, id: ProcedureId) -> Option<u64> {
        self.metadata(id).map(|p| p.expanded_work)
    }
    /// Includes the selected outermost function and each nested callee.
    pub fn max_call_depth(&self, id: ProcedureId) -> Option<usize> {
        self.metadata(id).map(|p| p.call_depth)
    }
    fn metadata(&self, id: ProcedureId) -> Option<&ProcedureMetadata> {
        self.procedures
            .binary_search_by_key(&id, |p| p.id)
            .ok()
            .map(|index| &self.procedures[index])
    }
}

impl ProcedureProgram {
    /// Validate every body and the entire call DAG before emitting any code.
    /// Function order does not affect bytes, metadata or validation errors.
    pub fn compile(
        &self,
        options: &ProcedureCompileOptions,
    ) -> Result<CompiledProcedureProgram, ProcedureCompileError> {
        let checked = CheckedProcedures::new(self, options)?;
        let mut procedures = reserved(checked.by_id.len())?;
        for (position, &index) in checked.by_id.iter().enumerate() {
            let id = self.functions[index].id;
            let mut export_name = String::new();
            export_name
                .try_reserve_exact("procedure_".len() + 10)
                .map_err(|_| ProcedureCompileError::Allocation)?;
            // The reserved capacity covers every decimal u32, so formatting
            // cannot allocate or fail for this String sink.
            write!(export_name, "procedure_{}", id.0)
                .map_err(|_| ProcedureCompileError::Allocation)?;
            procedures.push(ProcedureMetadata {
                id,
                export_name,
                expanded_work: checked.work[position],
                call_depth: checked.depth[position],
            });
        }
        let bytes = module(&checked, &procedures)?;
        Ok(CompiledProcedureProgram { bytes, procedures })
    }
}

fn reserved<T>(capacity: usize) -> Result<Vec<T>, ProcedureCompileError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| ProcedureCompileError::Allocation)?;
    Ok(values)
}

struct CheckedProcedures<'a> {
    program: &'a ProcedureProgram,
    options: &'a ProcedureCompileOptions,
    by_id: Vec<usize>,
    work: Vec<u64>,
    depth: Vec<usize>,
}
#[derive(Clone, Copy)]
struct Visit {
    position: usize,
    next: usize,
    work: u64,
    depth: usize,
}
impl<'a> CheckedProcedures<'a> {
    fn new(
        program: &'a ProcedureProgram,
        options: &'a ProcedureCompileOptions,
    ) -> Result<Self, ProcedureCompileError> {
        if options.max_procedures > MAX_PROCEDURES
            || options.max_procedure_operations > MAX_PROCEDURE_OPERATIONS
            || options.max_total_operations > MAX_TOTAL_OPERATIONS
            || options.max_call_depth == 0
            || options.max_call_depth > MAX_CALL_DEPTH
            || options.max_expanded_work == 0
            || options.max_expanded_work > MAX_EXPANDED_WORK
            || options.max_output_bytes > MAX_OUTPUT_BYTES
        {
            return Err(ProcedureCompileError::InvalidOptions);
        }
        if program.functions.len() > options.max_procedures {
            return Err(ProcedureCompileError::TooManyProcedures);
        }
        // Reject unbounded input before reserving or copying compiler indexes.
        let mut total = 0usize;
        for function in &program.functions {
            total = total
                .checked_add(function.operations.len())
                .filter(|&n| n <= options.max_total_operations)
                .ok_or(ProcedureCompileError::TooManyOperations)?;
        }
        let count = program.functions.len();
        let mut by_id = reserved(count)?;
        by_id.extend(0..count);
        by_id.sort_unstable_by_key(|&index| program.functions[index].id);
        for pair in by_id.windows(2) {
            let id = program.functions[pair[0]].id;
            if id == program.functions[pair[1]].id {
                return Err(ProcedureCompileError::DuplicateId(id));
            }
        }
        let mut checked = Self {
            program,
            options,
            by_id,
            work: reserved(count)?,
            depth: reserved(count)?,
        };
        checked.work.resize(count, 0);
        checked.depth.resize(count, 0);
        for &index in &checked.by_id {
            let function = &program.functions[index];
            if function.operations.len() > options.max_procedure_operations {
                return Err(ProcedureCompileError::ProcedureTooLong(function.id));
            }
            for (operation, &op) in function.operations.iter().enumerate() {
                match op {
                    ProcedureOperation::Execute(op) => {
                        validate_operation(op).map_err(|reason| {
                            ProcedureCompileError::InvalidOperation {
                                procedure: function.id,
                                operation,
                                reason,
                            }
                        })?
                    }
                    ProcedureOperation::Call(target) => {
                        if checked.position(target).is_none() {
                            return Err(ProcedureCompileError::UnknownCallee {
                                from: function.id,
                                target,
                            });
                        }
                    }
                }
            }
        }
        checked.analyze_dag()?;
        Ok(checked)
    }
    fn position(&self, id: ProcedureId) -> Option<usize> {
        self.by_id
            .binary_search_by_key(&id, |&index| self.program.functions[index].id)
            .ok()
    }
    fn function(&self, position: usize) -> &ProcedureFunction {
        &self.program.functions[self.by_id[position]]
    }
    fn add_work(
        &self,
        position: usize,
        current: u64,
        cost: u64,
    ) -> Result<u64, ProcedureCompileError> {
        current
            .checked_add(cost)
            .filter(|&work| work <= self.options.max_expanded_work)
            .ok_or_else(|| ProcedureCompileError::ExpandedWorkExceeded(self.function(position).id))
    }
    fn analyze_dag(&mut self) -> Result<(), ProcedureCompileError> {
        let mut states = reserved(self.by_id.len())?;
        states.resize(self.by_id.len(), 0u8);
        let mut stack: Vec<Visit> = reserved(self.by_id.len().min(self.options.max_call_depth))?;
        for root in 0..self.by_id.len() {
            if states[root] != 0 {
                continue;
            }
            states[root] = 1;
            stack.push(Visit {
                position: root,
                next: 0,
                work: 1,
                depth: 1,
            });
            while let Some(visit) = stack.last().copied() {
                let function = self.function(visit.position);
                let Some(&op) = function.operations.get(visit.next) else {
                    self.work[visit.position] = visit.work;
                    self.depth[visit.position] = visit.depth;
                    states[visit.position] = 2;
                    stack.pop();
                    continue;
                };
                let (cost, depth) = match op {
                    ProcedureOperation::Execute(_) => (1, 1),
                    ProcedureOperation::Call(target) => {
                        let callee = self.position(target).expect("validated callee");
                        if states[callee] == 1 {
                            return Err(ProcedureCompileError::Cycle {
                                from: function.id,
                                target,
                            });
                        }
                        if states[callee] == 0 {
                            if stack.len() >= self.options.max_call_depth {
                                return Err(ProcedureCompileError::CallDepthExceeded(function.id));
                            }
                            states[callee] = 1;
                            stack.push(Visit {
                                position: callee,
                                next: 0,
                                work: 1,
                                depth: 1,
                            });
                            continue;
                        }
                        let cost = self.add_work(visit.position, self.work[callee], 1)?;
                        let depth = self.depth[callee]
                            .checked_add(1)
                            .filter(|&n| n <= self.options.max_call_depth)
                            .ok_or(ProcedureCompileError::CallDepthExceeded(function.id))?;
                        (cost, depth)
                    }
                };
                let work = self.add_work(visit.position, visit.work, cost)?;
                let current = stack.last_mut().expect("active visit");
                current.work = work;
                current.depth = current.depth.max(depth);
                current.next += 1;
            }
        }
        Ok(())
    }
}

fn validate_register(register: Register) -> Result<(), ProcedureOperationError> {
    if register.index() == Register::Esp.index() {
        Err(ProcedureOperationError::StackRegister)
    } else {
        Ok(())
    }
}
fn validate_source(width: Width, source: Source) -> Result<(), ProcedureOperationError> {
    match source {
        Source::Register(register) => {
            validate_register(register)?;
            if register.width() != width {
                return Err(ProcedureOperationError::WidthMismatch);
            }
        }
        Source::Immediate(value) if value & !width.mask() != 0 => {
            return Err(ProcedureOperationError::ImmediateOutOfRange);
        }
        Source::Immediate(_) => {}
    }
    Ok(())
}
fn validate_operation(op: Operation) -> Result<(), ProcedureOperationError> {
    match op {
        Operation::Nop => {}
        Operation::Move { target, source } | Operation::Alu { target, source, .. } => {
            validate_register(target)?;
            validate_source(target.width(), source)?;
        }
        Operation::Extend { target, source, .. } => {
            validate_register(target)?;
            validate_register(source)?;
            if target.width() == Width::Byte
                || source.width() == Width::Dword
                || source.width().bits() > target.width().bits()
            {
                return Err(ProcedureOperationError::WidthMismatch);
            }
        }
        Operation::SignedMultiply {
            target,
            left,
            right,
        } => {
            validate_register(target)?;
            if target.width() == Width::Byte {
                return Err(ProcedureOperationError::WidthMismatch);
            }
            validate_source(target.width(), left)?;
            validate_source(target.width(), right)?;
        }
        Operation::WideMultiply { width, source, .. } => validate_source(width, source)?,
        Operation::Shift { target, .. }
        | Operation::Inc(target)
        | Operation::Dec(target)
        | Operation::Neg(target)
        | Operation::Not(target) => validate_register(target)?,
        Operation::DoubleShift {
            target,
            source,
            count,
            ..
        } => {
            validate_register(target)?;
            validate_register(source)?;
            if target.width() == Width::Byte || target.width() != source.width() {
                return Err(ProcedureOperationError::WidthMismatch);
            }
            if target.width() == Width::Word
                && match count {
                    ShiftCount::Cl => true,
                    ShiftCount::Immediate(n) => n & 31 > 16,
                    ShiftCount::One => false,
                }
            {
                return Err(ProcedureOperationError::InvalidShiftCount);
            }
        }
        Operation::Exchange { left, right } => {
            validate_register(left)?;
            validate_source(left.width(), Source::Register(right))?;
        }
        Operation::Lea { .. }
        | Operation::LoadMemory { .. }
        | Operation::ExtendMemory { .. }
        | Operation::SignedMultiplyMemory { .. }
        | Operation::WideMultiplyMemory { .. }
        | Operation::StoreMemory { .. }
        | Operation::StringLoad { .. }
        | Operation::StringStore { .. }
        | Operation::Push { .. }
        | Operation::Pop { .. }
        | Operation::AluMemory { .. }
        | Operation::CompareMemory { .. }
        | Operation::AluMemoryStore { .. }
        | Operation::UnaryMemory { .. } => return Err(ProcedureOperationError::Unsupported),
    }
    Ok(())
}

fn body(
    checked: &CheckedProcedures<'_>,
    position: usize,
) -> Result<Function, ProcedureCompileError> {
    // Nine compact parameters occupy 0..8. Reuse the established scalar
    // emitter's GPR2..9/FLAGS10 and scratch14..18 layout without changing its
    // instruction bytes. 11..13 are unused padding, not scheduling state.
    let mut f = Function::new([(emit::WIDE - 9, ValType::I32), (1, ValType::I64)]);
    f.instruction(&W::LocalGet(8));
    f.instruction(&W::LocalSet(emit::FLAGS));
    // Capture higher overlapping parameters before lower destinations replace them.
    for register in (0..8).rev() {
        f.instruction(&W::LocalGet(register));
        f.instruction(&W::LocalSet(emit::GPR + register));
    }
    for &op in &checked.function(position).operations {
        match op {
            ProcedureOperation::Execute(op) => emit::operation(&mut f, op),
            ProcedureOperation::Call(target) => {
                for local in emit::GPR..=emit::FLAGS {
                    f.instruction(&W::LocalGet(local));
                }
                f.instruction(&W::Call(
                    checked.position(target).expect("validated callee") as u32,
                ));
                for local in (emit::GPR..=emit::FLAGS).rev() {
                    f.instruction(&W::LocalSet(local));
                }
            }
        }
        if f.byte_len() > checked.options.max_output_bytes {
            return Err(ProcedureCompileError::OutputTooLarge);
        }
    }
    for local in emit::GPR..=emit::FLAGS {
        f.instruction(&W::LocalGet(local));
    }
    f.instruction(&W::End);
    Ok(f)
}

fn leb_len(mut value: usize) -> usize {
    let mut count = 1;
    while value >= 128 {
        value >>= 7;
        count += 1;
    }
    count
}
fn append_body(
    code: &mut CodeSection,
    f: &Function,
    max_output: usize,
) -> Result<(), ProcedureCompileError> {
    code.byte_len()
        .checked_add(leb_len(f.byte_len()))
        .and_then(|n| n.checked_add(f.byte_len()))
        .filter(|&n| n <= max_output)
        .ok_or(ProcedureCompileError::OutputTooLarge)?;
    code.function(f);
    Ok(())
}
fn module(
    checked: &CheckedProcedures<'_>,
    metadata: &[ProcedureMetadata],
) -> Result<Vec<u8>, ProcedureCompileError> {
    let mut types = TypeSection::new();
    types.ty().function([ValType::I32; 9], [ValType::I32; 9]);
    let mut results = [ValType::I32; 10];
    results[9] = ValType::I64;
    types.ty().function([ValType::I32; 9], results);
    let mut functions = FunctionSection::new();
    let mut exports = ExportSection::new();
    let count = checked.by_id.len() as u32;
    for _ in 0..count {
        functions.function(0);
    }
    for (position, procedure) in metadata.iter().enumerate() {
        functions.function(1);
        exports.export(
            &procedure.export_name,
            ExportKind::Func,
            count + position as u32,
        );
    }
    let mut code = CodeSection::new();
    for position in 0..checked.by_id.len() {
        append_body(
            &mut code,
            &body(checked, position)?,
            checked.options.max_output_bytes,
        )?;
    }
    // Only external wrappers return accounting. Native procedure calls carry
    // registers/flags alone; no counters are updated inside any procedure.
    for (position, procedure) in metadata.iter().enumerate() {
        let mut wrapper = Function::new([]);
        for local in 0..9 {
            wrapper.instruction(&W::LocalGet(local));
        }
        wrapper.instruction(&W::Call(position as u32));
        wrapper.instruction(&W::I64Const(procedure.expanded_work as i64));
        wrapper.instruction(&W::End);
        append_body(&mut code, &wrapper, checked.options.max_output_bytes)?;
    }
    // The metadata upper bound covers names, indices, counts and section sizes.
    let header_capacity = metadata
        .len()
        .checked_mul(40)
        .and_then(|n| n.checked_add(128))
        .ok_or(ProcedureCompileError::OutputTooLarge)?;
    let mut bytes = reserved(header_capacity)?;
    bytes.extend_from_slice(&Module::HEADER);
    types.append_to(&mut bytes);
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
        .filter(|&n| n <= checked.options.max_output_bytes)
        .ok_or(ProcedureCompileError::OutputTooLarge)?;
    bytes
        .try_reserve_exact(total - bytes.len())
        .map_err(|_| ProcedureCompileError::Allocation)?;
    code.append_to(&mut bytes);
    debug_assert_eq!(bytes.len(), total);
    Ok(bytes)
}
