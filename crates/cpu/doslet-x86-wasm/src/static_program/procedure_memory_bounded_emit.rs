//! Direct structured loop with one full-iteration grant test and a precise tail.

use super::*;
use crate::static_program::{memory, procedure_memory::graph::bounded::LoopPlan};

fn guard(f: &mut Function, address: Address, depth: u32, prefix: usize) {
    get(f, COMPLETED);
    if prefix != 0 {
        c(f, prefix as u32);
        ins(f, W::I32Add);
    }
    get(f, QUOTA);
    ins(f, W::I32GeU);
    ins(f, W::If(BlockType::Empty));
    refused(f, address, depth, prefix);
    ins(f, W::End);
}

fn iteration(
    f: &mut Function,
    procedure: &ProcedureGraphFunction,
    loop_plan: &LoopPlan,
    outer_depth: u32,
    check_each: bool,
    max_output: usize,
) -> Result<(), ProcedureCompileError> {
    let depth = outer_depth + 2; // enclosing WASM loop and fast/partial if
    let mut prefix = 0;
    for (position, &index) in loop_plan.blocks.iter().enumerate() {
        let block = &procedure.blocks[index];
        for instruction in &block.instructions {
            if check_each {
                guard(f, instruction.address, depth, prefix);
            }
            instruction_body(f, instruction.operation, |f| {
                refused(f, instruction.address, depth, prefix)
            });
            prefix += 1;
            if f.byte_len() > max_output {
                return Err(ProcedureCompileError::OutputTooLarge);
            }
        }
        if !matches!(
            block.terminator.kind,
            ProcedureTerminatorKind::Fallthrough(_)
        ) {
            if check_each {
                guard(f, block.terminator.address, depth, prefix);
            }
            if position + 1 != loop_plan.blocks.len() {
                prefix += 1;
            }
        }
    }
    debug_assert_eq!(prefix + 1, loop_plan.work);
    completed(f, prefix);
    Ok(())
}

fn loop_body(
    f: &mut Function,
    procedure: &ProcedureGraphFunction,
    plan: &GraphPlan,
    position: usize,
    depth: u32,
    max_output: usize,
) -> Result<(), ProcedureCompileError> {
    let loop_plan = plan.bounded.as_ref().unwrap();
    let header = loop_plan.blocks[0];
    let latch = *loop_plan.blocks.last().unwrap();
    ins(f, W::Loop(BlockType::Empty));
    get(f, COMPLETED);
    c(f, loop_plan.work as u32);
    ins(f, W::I32Add);
    get(f, QUOTA);
    ins(f, W::I32LeU);
    ins(f, W::If(BlockType::Empty));
    iteration(f, procedure, loop_plan, depth, false, max_output)?;
    ins(f, W::Else);
    iteration(f, procedure, loop_plan, depth, true, max_output)?;
    ins(f, W::End);
    completed(f, 1); // actual latch JMP/Jcc, after its grant was established
    let exit = match procedure.blocks[latch].terminator.kind {
        ProcedureTerminatorKind::Jump(_) => {
            ins(f, W::Br(0));
            None
        }
        ProcedureTerminatorKind::Branch { condition: cc, .. } => {
            let [taken, not_taken] = plan.targets[latch];
            if taken == not_taken {
                ins(f, W::Br(0));
                None
            } else {
                condition(f, cc);
                let target = if taken == Some(header) {
                    not_taken.unwrap()
                } else {
                    ins(f, W::I32Eqz);
                    taken.unwrap()
                };
                ins(f, W::BrIf(0));
                Some(target)
            }
        }
        _ => unreachable!("validated backward latch"),
    };
    ins(f, W::End);
    if let Some(target) = exit {
        edge(f, position, plan.positions[target]);
    }
    Ok(())
}

pub(super) fn body(
    procedure: &ProcedureGraphFunction,
    plan: &GraphPlan,
    max_output: usize,
) -> Result<Function, ProcedureCompileError> {
    let loop_plan = plan.bounded.as_ref().unwrap();
    let mut f = initialize(procedure.blocks.iter().any(|block| {
        block.instructions.iter().any(|instruction| {
            matches!(
                instruction.operation,
                Operation::StoreMemory { .. } | Operation::StringStore { .. }
            )
        })
    }));
    // QUOTA is an unused i32 padding local in the common procedure ABI.
    load(&mut f, memory::OFFSET + memory::INLINE_GRANT, false);
    set(&mut f, QUOTA);
    get(&mut f, QUOTA);
    c(&mut f, loop_plan.limit);
    ins(&mut f, W::I32GtU);
    ins(&mut f, W::If(BlockType::Empty));
    c(&mut f, loop_plan.limit);
    set(&mut f, QUOTA);
    ins(&mut f, W::End);
    ins(&mut f, W::Block(BlockType::Empty)); // common exit
    for _ in 1..plan.order.len() {
        ins(&mut f, W::Block(BlockType::Empty));
    }
    for (position, &index) in plan.order.iter().enumerate() {
        let remaining = (plan.order.len() - position - 1) as u32;
        if index == loop_plan.blocks[0] {
            loop_body(&mut f, procedure, plan, position, remaining, max_output)?;
        } else {
            let block = &procedure.blocks[index];
            for (prefix, instruction) in block.instructions.iter().enumerate() {
                guard(&mut f, instruction.address, remaining, prefix);
                instruction_body(&mut f, instruction.operation, |f| {
                    refused(f, instruction.address, remaining, prefix)
                });
                if f.byte_len() > max_output {
                    return Err(ProcedureCompileError::OutputTooLarge);
                }
            }
            let terminator = block.terminator;
            if !matches!(
                terminator.kind,
                ProcedureTerminatorKind::Fallthrough(_) | ProcedureTerminatorKind::Exit
            ) {
                guard(
                    &mut f,
                    terminator.address,
                    remaining,
                    block.instructions.len(),
                );
            }
            let target = |arm: usize| plan.positions[plan.targets[index][arm].unwrap()];
            match terminator.kind {
                ProcedureTerminatorKind::Exit => {
                    completed(&mut f, block.instructions.len());
                    c(&mut f, terminator.address.offset);
                    set(&mut f, PC);
                    c(&mut f, SIDE_EXIT);
                    set(&mut f, EXIT_STATUS);
                    ins(&mut f, W::Br(remaining));
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
                    if target(0) == target(1) {
                        edge(&mut f, position, target(0));
                    } else if target(0) == position + 1 {
                        condition(&mut f, cc);
                        ins(&mut f, W::I32Eqz);
                        ins(&mut f, W::BrIf((target(1) - position - 1) as u32));
                    } else {
                        condition(&mut f, cc);
                        ins(&mut f, W::BrIf((target(0) - position - 1) as u32));
                        edge(&mut f, position, target(1));
                    }
                }
                ProcedureTerminatorKind::Return { width, pop_bytes } => {
                    stack_emit::procedure_preflight(&mut f, width, false, |f| {
                        refused(f, terminator.address, remaining, block.instructions.len())
                    });
                    stack_emit::near_return(&mut f, width, pop_bytes);
                    get(&mut f, RESULT);
                    set(&mut f, PC);
                    completed(&mut f, block.instructions.len() + 1);
                    c(&mut f, COMPLETE);
                    set(&mut f, EXIT_STATUS);
                    ins(&mut f, W::Br(remaining));
                }
            }
        }
        if position + 1 < plan.order.len() {
            ins(&mut f, W::End);
        }
        if f.byte_len() > max_output {
            return Err(ProcedureCompileError::OutputTooLarge);
        }
    }
    ins(&mut f, W::End);
    finish(&mut f, procedure.entry);
    if f.byte_len() > max_output {
        return Err(ProcedureCompileError::OutputTooLarge);
    }
    Ok(f)
}
