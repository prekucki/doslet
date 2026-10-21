//! Direct structured loop forests, with no runtime budget or instruction fuel.

use super::*;
use crate::static_program::procedure_memory::graph::finite::{FinitePlan, FiniteRegion};

#[allow(clippy::too_many_arguments)]
fn region_body(
    f: &mut Function,
    vregs: &VirtualRegisters,
    procedure: &ProcedureGraphFunction,
    graph: &GraphPlan,
    finite: &FinitePlan,
    owner: Option<usize>,
    outer_depth: u32,
    max_output: usize,
) -> Result<(), ProcedureCompileError> {
    let region: &FiniteRegion = &finite.regions[owner.map_or(0, |index| index + 1)];
    // Forward target labels surround this region. For a loop region, its
    // containing WASM loop is immediately outside these labels.
    for _ in 1..region.order.len() {
        ins(f, W::Block(BlockType::Empty));
    }
    for (position, &index) in region.order.iter().enumerate() {
        let remaining = (region.order.len() - position - 1) as u32;
        let depth = outer_depth + remaining;
        if let Some(child) = region.children[index] {
            let loop_plan = &finite.loops[child];
            if loop_plan.guarded {
                // One proof at this dynamic loop entry. The loop body contains
                // no quota, fuel or instruction-budget check. Zero represents
                // 2^32 trips and is deliberately declined before loop effects.
                if !loop_plan.decrement {
                    c(f, 0);
                }
                read_register(f, loop_plan.counter);
                if !loop_plan.decrement {
                    ins(f, W::I32Sub);
                }
                set(f, RESULT);
                get(f, RESULT);
                ins(f, W::I32Eqz);
                get(f, RESULT);
                c(f, loop_plan.trips);
                ins(f, W::I32GtU);
                ins(f, W::I32Or);
                ins_hinted(f, false, W::If(BlockType::Empty));
                refused(f, procedure.blocks[index].entry, depth, 0);
                ins(f, W::End);
            }
            ins(f, W::Loop(BlockType::Empty));
            region_body(
                f,
                vregs,
                procedure,
                graph,
                finite,
                Some(child),
                depth + 1,
                max_output,
            )?;
            ins(f, W::End);
            edge(f, position, region.positions[finite.loops[child].exit]);
        } else {
            let block = &procedure.blocks[index];
            for (prefix, instruction) in block.instructions.iter().enumerate() {
                instruction_body(f, vregs, instruction.operation, |f| {
                    refused(f, instruction.address, depth, prefix)
                });
                if f.byte_len() > max_output {
                    return Err(ProcedureCompileError::OutputTooLarge);
                }
            }
            let terminator = block.terminator;
            if owner.is_some_and(|loop_index| finite.loops[loop_index].latch == index) {
                completed(f, block.instructions.len() + 1);
                condition(f, Condition::NotEqual);
                // The sole loop continuation tests the flags produced by the
                // proven final induction update. Zero falls out of the loop.
                ins_hinted(f, true, W::BrIf(remaining));
            } else {
                let target = |arm: usize| region.positions[graph.targets[index][arm].unwrap()];
                match terminator.kind {
                    ProcedureTerminatorKind::Exit => {
                        completed(f, block.instructions.len());
                        c(f, terminator.address.offset);
                        set(f, PC);
                        c(f, SIDE_EXIT);
                        set(f, EXIT_STATUS);
                        ins(f, W::Br(depth));
                    }
                    ProcedureTerminatorKind::Fallthrough(_) => {
                        completed(f, block.instructions.len());
                        edge(f, position, target(0));
                    }
                    ProcedureTerminatorKind::Jump(_) => {
                        completed(f, block.instructions.len() + 1);
                        edge(f, position, target(0));
                    }
                    ProcedureTerminatorKind::Branch { condition: cc, .. } => {
                        completed(f, block.instructions.len() + 1);
                        if target(0) == target(1) {
                            edge(f, position, target(0));
                        } else if target(0) == position + 1 {
                            condition(f, cc);
                            ins(f, W::I32Eqz);
                            ins(f, W::BrIf((target(1) - position - 1) as u32));
                        } else {
                            condition(f, cc);
                            ins(f, W::BrIf((target(0) - position - 1) as u32));
                            edge(f, position, target(1));
                        }
                    }
                    ProcedureTerminatorKind::Return { width, pop_bytes } => {
                        stack_emit::procedure_preflight(f, width, false, |f| {
                            refused(f, terminator.address, depth, block.instructions.len())
                        });
                        stack_emit::near_return(f, width, pop_bytes);
                        get(f, RESULT);
                        set(f, PC);
                        completed(f, block.instructions.len() + 1);
                        c(f, COMPLETE);
                        set(f, EXIT_STATUS);
                        ins(f, W::Br(depth));
                    }
                }
            }
        }
        if position + 1 < region.order.len() {
            ins(f, W::End);
        }
        if f.byte_len() > max_output {
            return Err(ProcedureCompileError::OutputTooLarge);
        }
    }
    Ok(())
}

pub(super) fn body(
    procedure: &ProcedureGraphFunction,
    graph: &GraphPlan,
    max_output: usize,
) -> Result<Function, ProcedureCompileError> {
    let finite = graph.finite.as_ref().unwrap();
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
    ins(&mut f, W::Block(BlockType::Empty));
    vregs.emit_prologue(&mut f, procedure.entry);
    region_body(
        &mut f, &vregs, procedure, graph, finite, None, 0, max_output,
    )?;
    ins(&mut f, W::End);
    finish(&mut f, procedure.entry);
    if f.byte_len() > max_output {
        return Err(ProcedureCompileError::OutputTooLarge);
    }
    Ok(f)
}
