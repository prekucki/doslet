//! Direct structured loop with one full-iteration grant test and a precise tail.

use super::*;
use crate::static_program::memory;

fn guard(f: &mut Function, address: Address, depth: u32, prefix: usize) {
    get(f, COMPLETED);
    if prefix != 0 {
        c(f, prefix as u32);
        ins(f, W::I32Add);
    }
    get(f, QUOTA);
    ins(f, W::I32GeU);
    ins_hinted(f, false, W::If(BlockType::Empty));
    refused(f, address, depth, prefix);
    ins(f, W::End);
}

fn iteration(
    f: &mut Function,
    vregs: &VirtualRegisters,
    procedure: &ProcedureGraphFunction,
    plan: &GraphPlan,
    outer_depth: u32,
    check_each: bool,
    max_output: usize,
) -> Result<(), ProcedureCompileError> {
    let loop_plan = plan.bounded.as_ref().unwrap();
    let graph_position = plan.positions[loop_plan.blocks[0]];
    let depth = outer_depth + 2; // enclosing WASM loop and fast/partial if
    let mut prefix = 0;
    for (position, &index) in loop_plan.blocks.iter().enumerate() {
        let block = &procedure.blocks[index];
        let mut top = plan.x87.as_ref().map_or(0, |p| p.tops[index]);
        for instruction in &block.instructions {
            if check_each {
                guard(f, instruction.address, depth, prefix);
            }
            x87_emit::dispatch(
                f,
                vregs,
                plan.x87.as_ref().map(|_| x87_emit::Locals::new(vregs)),
                instruction.address,
                instruction.operation,
                &mut top,
                |f| refused(f, instruction.address, depth, prefix),
            );
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
                if let ProcedureTerminatorKind::Branch { condition: cc, .. } = block.terminator.kind
                {
                    let next = loop_plan.blocks[position + 1];
                    let [taken, not_taken] = plan.targets[index];
                    if taken != not_taken {
                        condition(f, cc);
                        let target = if taken == Some(next) {
                            ins(f, W::I32Eqz);
                            not_taken.unwrap()
                        } else {
                            debug_assert_eq!(not_taken, Some(next));
                            taken.unwrap()
                        };
                        ins(f, W::If(BlockType::Empty));
                        // Include this actual Jcc and skip the remaining body
                        // and latch. The exit arm is outside the loop's SCC.
                        completed(f, prefix + 1);
                        let target_position = plan.positions[target];
                        debug_assert!(target_position > graph_position);
                        // Exit the early-exit if, fast/partial if and WASM loop,
                        // then the ordinary forward CFG labels to the target.
                        ins(f, W::Br((target_position - graph_position + 2) as u32));
                        ins(f, W::End);
                    }
                }
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
    vregs: &VirtualRegisters,
    procedure: &ProcedureGraphFunction,
    plan: &GraphPlan,
    position: usize,
    depth: u32,
    max_output: usize,
) -> Result<(), ProcedureCompileError> {
    let loop_plan = plan.bounded.as_ref().unwrap();
    let header = loop_plan.blocks[0];
    let latch = *loop_plan.blocks.last().unwrap();
    let latch_kind = procedure.blocks[latch].terminator.kind;
    if !matches!(
        latch_kind,
        ProcedureTerminatorKind::Jump(_) | ProcedureTerminatorKind::Branch { .. }
    ) {
        return Err(ProcedureCompileError::CyclicGraph(procedure.id));
    }
    ins(f, W::Loop(BlockType::Empty));
    get(f, COMPLETED);
    c(f, loop_plan.work as u32);
    ins(f, W::I32Add);
    get(f, QUOTA);
    ins(f, W::I32LeU);
    ins_hinted(f, true, W::If(BlockType::Empty));
    iteration(f, vregs, procedure, plan, depth, false, max_output)?;
    ins(f, W::Else);
    iteration(f, vregs, procedure, plan, depth, true, max_output)?;
    ins(f, W::End);
    completed(f, 1); // actual latch JMP/Jcc, after its grant was established
    let exit = match latch_kind {
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
                ins_hinted(f, true, W::BrIf(0));
                Some(target)
            }
        }
        _ => return Err(ProcedureCompileError::CyclicGraph(procedure.id)),
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
    let vregs = VirtualRegisters::collect(
        procedure
            .blocks
            .iter()
            .flat_map(|block| block.instructions.iter().map(|i| i.operation)),
    )?;
    let x87_locals = plan.x87.as_ref().map(|_| x87_emit::Locals::new(&vregs));
    let mut f = initialize_with_x87(
        procedure.blocks.iter().any(|block| {
            block.instructions.iter().any(|instruction| {
                matches!(
                    instruction.operation,
                    Operation::StoreMemory { .. } | Operation::StringStore { .. }
                )
            })
        }),
        &vregs,
        x87_locals.is_some(),
    );
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
    if let (Some(locals), Some(x87)) = (x87_locals, plan.x87.as_ref()) {
        x87_emit::initialize(&mut f, locals, x87.permissions, |f| {
            refused(f, procedure.entry, 0, 0)
        });
    }
    vregs.emit_prologue(&mut f, procedure.entry);
    for _ in 1..plan.order.len() {
        ins(&mut f, W::Block(BlockType::Empty));
    }
    for (position, &index) in plan.order.iter().enumerate() {
        let remaining = (plan.order.len() - position - 1) as u32;
        if index == loop_plan.blocks[0] {
            loop_body(
                &mut f, &vregs, procedure, plan, position, remaining, max_output,
            )?;
        } else {
            let block = &procedure.blocks[index];
            let mut top = plan.x87.as_ref().map_or(0, |p| p.tops[index]);
            for (prefix, instruction) in block.instructions.iter().enumerate() {
                guard(&mut f, instruction.address, remaining, prefix);
                x87_emit::dispatch(
                    &mut f,
                    &vregs,
                    x87_locals,
                    instruction.address,
                    instruction.operation,
                    &mut top,
                    |f| refused(f, instruction.address, remaining, prefix),
                );
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
    if let Some(locals) = x87_locals {
        x87_emit::finish(&mut f, locals);
    }
    finish(&mut f, procedure.entry);
    if f.byte_len() > max_output {
        return Err(ProcedureCompileError::OutputTooLarge);
    }
    Ok(f)
}
