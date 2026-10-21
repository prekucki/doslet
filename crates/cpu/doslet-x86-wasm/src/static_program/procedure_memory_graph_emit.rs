//! Forward structured edges keep all architectural state in the same locals.

use super::procedure_memory_emit::{
    COMPLETED, EXIT_STATUS, PC, VirtualRegisters, finish, initialize_with_vregs, instruction_body,
};
use super::*;
use crate::static_program::procedure_memory::{
    COMPLETE, ProcedureCompileError, ProcedureGraphFunction, ProcedureTerminatorKind, SIDE_EXIT,
    graph::GraphPlan,
};

#[path = "procedure_memory_bounded_emit.rs"]
mod bounded;
#[path = "procedure_memory_finite_emit.rs"]
mod finite;

fn completed(f: &mut Function, count: usize) {
    if count == 0 {
        return;
    }
    get(f, COMPLETED);
    c(f, count as u32);
    ins(f, W::I32Add);
    set(f, COMPLETED);
}

fn refused(f: &mut Function, address: Address, remaining_blocks: u32, prefix: usize) {
    completed(f, prefix);
    c(f, address.offset);
    set(f, PC);
    c(f, SIDE_EXIT);
    set(f, EXIT_STATUS);
    // The shared preflight still has one success block around this callback.
    ins(f, W::Br(remaining_blocks + 1));
}

fn edge(f: &mut Function, position: usize, target: usize) {
    debug_assert!(target > position);
    if target != position + 1 {
        ins(f, W::Br((target - position - 1) as u32));
    }
}

pub(in crate::static_program) fn body(
    procedure: &ProcedureGraphFunction,
    plan: &GraphPlan,
    max_output: usize,
) -> Result<Function, ProcedureCompileError> {
    if plan.finite.is_some() {
        return finite::body(procedure, plan, max_output);
    }
    if plan.bounded.is_some() {
        return bounded::body(procedure, plan, max_output);
    }
    let vregs = VirtualRegisters::collect(
        procedure
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter().map(|i| i.operation)),
    )?;
    let mut f = initialize_with_vregs(
        procedure.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.operation,
                    Operation::StoreMemory { .. } | Operation::StringStore { .. }
                )
            })
        }),
        &vregs,
    );
    ins(&mut f, W::Block(BlockType::Empty)); // common exit, outside all CFG labels
    vregs.emit_prologue(&mut f, procedure.entry);
    for _ in 1..plan.order.len() {
        ins(&mut f, W::Block(BlockType::Empty));
    }
    for (position, &index) in plan.order.iter().enumerate() {
        let block = &procedure.blocks[index];
        let remaining_blocks = (plan.order.len() - position - 1) as u32;
        for (prefix, instruction) in block.instructions.iter().enumerate() {
            instruction_body(&mut f, &vregs, instruction.operation, |f| {
                refused(f, instruction.address, remaining_blocks, prefix)
            });
            if f.byte_len() > max_output {
                return Err(ProcedureCompileError::OutputTooLarge);
            }
        }
        let terminator = block.terminator;
        let target = |arm: usize| plan.positions[plan.targets[index][arm].unwrap()];
        match terminator.kind {
            ProcedureTerminatorKind::Exit => {
                completed(&mut f, block.instructions.len());
                c(&mut f, terminator.address.offset);
                set(&mut f, PC);
                c(&mut f, SIDE_EXIT);
                set(&mut f, EXIT_STATUS);
                ins(&mut f, W::Br(remaining_blocks));
            }
            ProcedureTerminatorKind::Fallthrough(_) => {
                completed(&mut f, block.instructions.len());
                edge(&mut f, position, target(0));
            }
            ProcedureTerminatorKind::Jump(_) => {
                completed(&mut f, block.instructions.len() + 1);
                edge(&mut f, position, target(0));
            }
            ProcedureTerminatorKind::Branch { condition: cc, .. } => {
                completed(&mut f, block.instructions.len() + 1);
                let taken = target(0);
                let not_taken = target(1);
                if taken == not_taken {
                    edge(&mut f, position, taken);
                } else if taken == position + 1 {
                    condition(&mut f, cc);
                    ins(&mut f, W::I32Eqz);
                    ins(&mut f, W::BrIf((not_taken - position - 1) as u32));
                } else {
                    condition(&mut f, cc);
                    ins(&mut f, W::BrIf((taken - position - 1) as u32));
                    edge(&mut f, position, not_taken);
                }
            }
            ProcedureTerminatorKind::Return { width, pop_bytes } => {
                stack_emit::procedure_preflight(&mut f, width, false, |f| {
                    refused(
                        f,
                        terminator.address,
                        remaining_blocks,
                        block.instructions.len(),
                    )
                });
                stack_emit::near_return(&mut f, width, pop_bytes);
                get(&mut f, RESULT);
                set(&mut f, PC);
                completed(&mut f, block.instructions.len() + 1);
                c(&mut f, COMPLETE);
                set(&mut f, EXIT_STATUS);
                ins(&mut f, W::Br(remaining_blocks));
            }
        }
        if position + 1 < plan.order.len() {
            ins(&mut f, W::End);
        }
        if f.byte_len() > max_output {
            return Err(ProcedureCompileError::OutputTooLarge);
        }
    }
    ins(&mut f, W::End); // common exit
    finish(&mut f, procedure.entry);
    if f.byte_len() > max_output {
        return Err(ProcedureCompileError::OutputTooLarge);
    }
    Ok(f)
}
