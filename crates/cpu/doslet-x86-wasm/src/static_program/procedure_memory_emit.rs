//! Whole straight-line procedure; shares all instruction and memory semantics.

use super::*;
use crate::static_program::memory;
use crate::static_program::procedure_memory::{
    COMPLETE, ProcedureCompileError, ProcedureFunction, SIDE_EXIT,
};

pub(super) const PC: u32 = WIDE + memory_emit::PROCEDURE_LOCAL_COUNT + 1;
pub(super) const COMPLETED: u32 = PC + 1;
pub(super) const EXIT_STATUS: u32 = COMPLETED + 1;

/// The common exit block encloses each preflight's success/failure blocks.
/// A refused operand reaches this code with only the success block remaining.
fn refused(f: &mut Function, address: Address, completed: usize) {
    c(f, address.offset);
    set(f, PC);
    c(f, completed as u32);
    set(f, COMPLETED);
    c(f, SIDE_EXIT);
    set(f, EXIT_STATUS);
    ins(f, W::Br(1));
}

pub(in crate::static_program) fn body(
    procedure: &ProcedureFunction,
    max_output: usize,
) -> Result<Function, ProcedureCompileError> {
    let mut f = initialize(procedure.instructions.iter().any(|instruction| {
        matches!(
            instruction.operation,
            Operation::StoreMemory { .. } | Operation::StringStore { .. }
        )
    }));
    ins(&mut f, W::Block(BlockType::Empty)); // common exit
    for (completed, instruction) in procedure.instructions.iter().enumerate() {
        instruction_body(&mut f, instruction.operation, |f| {
            refused(f, instruction.address, completed)
        });
        if f.byte_len() > max_output {
            return Err(ProcedureCompileError::OutputTooLarge);
        }
    }
    let terminator = procedure.terminator;
    stack_emit::procedure_preflight(&mut f, terminator.width, false, |f| {
        refused(f, terminator.address, procedure.instructions.len())
    });
    stack_emit::near_return(&mut f, terminator.width, terminator.pop_bytes);
    get(&mut f, RESULT);
    set(&mut f, PC);
    c(&mut f, procedure.instructions.len() as u32 + 1);
    set(&mut f, COMPLETED);
    c(&mut f, COMPLETE);
    set(&mut f, EXIT_STATUS);
    ins(&mut f, W::End);
    finish(&mut f, procedure.entry);
    if f.byte_len() > max_output {
        return Err(ProcedureCompileError::OutputTooLarge);
    }
    Ok(f)
}

pub(super) fn initialize(device_stores: bool) -> Function {
    // Match the shared region emitter's local identities and value types.
    // Only FRAME is a parameter. QUOTA/BUDGET are unused padding.
    let mut f = Function::new([
        (10, ValType::I32),
        (2, ValType::I64),
        (5, ValType::I32),
        (1, ValType::I64),
        (memory_emit::PROCEDURE_LOCAL_COUNT + 3, ValType::I32),
    ]);
    for i in 0..8 {
        load(&mut f, abi::GPR + i * 4, false);
        set(&mut f, GPR + i);
    }
    load(&mut f, abi::EFLAGS, false);
    set(&mut f, FLAGS);
    load(&mut f, abi::CHARGED_WORK, true);
    set(&mut f, WORK);
    load(&mut f, abi::STATUS, false);
    set(&mut f, STATUS);
    if device_stores {
        memory_emit::procedure_initialize(&mut f);
    }
    f
}

pub(super) fn instruction_body(
    f: &mut Function,
    operation_value: Operation,
    refused: impl Fn(&mut Function),
) {
    use memory_emit::Access;
    let access = match operation_value {
        Operation::LoadMemory { target, address }
        | Operation::SignedMultiplyMemory {
            target, address, ..
        }
        | Operation::AluMemory {
            target, address, ..
        } => Some((target.width(), address, Access::RamRead)),
        Operation::CompareMemory { width, address, .. }
        | Operation::ExtendMemory { width, address, .. }
        | Operation::WideMultiplyMemory { width, address, .. } => {
            Some((width, address, Access::RamRead))
        }
        Operation::StoreMemory { width, address, .. } => {
            Some((width, address, Access::ProcedureWrite))
        }
        Operation::StringLoad {
            width,
            address16,
            segment,
        } => Some((
            width,
            memory::MemoryAddress::string(true, address16, segment),
            Access::RamRead,
        )),
        Operation::StringStore { width, address16 } => Some((
            width,
            memory::MemoryAddress::string(false, address16, memory::Segment::Es),
            Access::ProcedureWrite,
        )),
        Operation::AluMemoryStore { width, address, .. }
        | Operation::UnaryMemory { width, address, .. } => {
            Some((width, address, Access::ReadModifyWrite))
        }
        _ => None,
    };
    if let Some((width, address, access)) = access {
        memory_emit::procedure_access(f, width, address, access, &refused);
    }
    match operation_value {
        Operation::Push { width, .. } => stack_emit::procedure_preflight(f, width, true, &refused),
        Operation::Pop { target } => {
            stack_emit::procedure_preflight(f, target.width(), false, &refused)
        }
        _ => {}
    }
    match operation_value {
        Operation::StoreMemory { width, source, .. } => {
            memory_emit::procedure_store_memory(f, width, source);
        }
        Operation::StringStore { width, address16 } => {
            memory_emit::procedure_store_memory(f, width, Source::Register(accumulator(width)));
            string_advance(f, false, width, address16);
        }
        _ => operation(f, operation_value),
    }
}

pub(super) fn finish(f: &mut Function, entry: Address) {
    // Charge precisely the completed prefix once, with the standard sticky
    // saturation contract. The owner reconciles its external reservation.
    get(f, WORK);
    wide(f, u64::MAX);
    get(f, COMPLETED);
    ins(f, W::I64ExtendI32U);
    ins(f, W::I64Sub);
    ins(f, W::I64GtU);
    ins(f, W::If(BlockType::Empty));
    wide(f, u64::MAX);
    set(f, WORK);
    get(f, STATUS);
    c(f, abi::COUNTER_OVERFLOW);
    ins(f, W::I32Or);
    set(f, STATUS);
    ins(f, W::Else);
    get(f, WORK);
    get(f, COMPLETED);
    ins(f, W::I64ExtendI32U);
    ins(f, W::I64Add);
    set(f, WORK);
    ins(f, W::End);
    for i in 0..8 {
        store(f, abi::GPR + i * 4, GPR + i, false);
    }
    store(f, abi::EFLAGS, FLAGS, false);
    store_constant(f, abi::SPACE, entry.space);
    store(f, abi::OFFSET, PC, false);
    store(f, abi::CHARGED_WORK, WORK, true);
    store(f, abi::STATUS, STATUS, false);
    get(f, EXIT_STATUS);
    ins(f, W::End);
}
