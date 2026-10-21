use super::*;
use crate::static_program::memory::{MemoryAddress, Segment};
use alloc::vec;

fn at(offset: u32) -> Address {
    Address { space: 0, offset }
}
fn block(offset: u32, operations: &[X87Operation]) -> ProcedureBlock {
    ProcedureBlock {
        entry: at(offset),
        instructions: operations
            .iter()
            .enumerate()
            .map(|(i, &operation)| ProcedureInstruction {
                address: at(offset + i as u32),
                operation: Operation::X87(X87Instruction {
                    opcode: 0,
                    operation,
                }),
            })
            .collect(),
        terminator: ProcedureTerminator {
            address: at(offset + operations.len() as u32),
            kind: ProcedureTerminatorKind::Exit,
        },
    }
}
fn summary(blocks: Vec<ProcedureBlock>, targets: &[[Option<usize>; 2]]) -> X87Effects {
    let function = ProcedureGraphFunction {
        id: ProcedureId(0),
        entry: blocks[0].entry,
        blocks,
    };
    // Summary tests isolate signed topology from opcode admission. Masks come
    // from the same physical-slot effect analysis used by the planner.
    let mut top = 0;
    let (mut read, mut defined, mut writes) = (0, 0, 0);
    for instruction in &function.blocks[0].instructions {
        if let Operation::X87(instruction) = instruction.operation {
            let (r, d, w, next) = effects(instruction, top);
            read |= r & !defined;
            defined |= d;
            writes |= w;
            top = next;
        }
    }
    let order: Vec<_> = (0..function.blocks.len()).collect();
    summarize(
        &function,
        targets,
        &order,
        X87Procedure {
            read_mask: read,
            write_mask: writes,
        },
    )
    .unwrap()
}
fn push() -> X87Operation {
    X87Operation::Duplicate { index: 0 }
}
fn operand() -> MemoryAddress {
    MemoryAddress {
        base: None,
        index: None,
        scale: 1,
        displacement: 0,
        address16: false,
        segment: Segment::Ds,
    }
}
fn pop() -> X87Operation {
    X87Operation::StoreInteger32 {
        address: operand(),
        pop: true,
    }
}

#[test]
fn live_input_and_peak_depth_are_explicit() {
    let effect = summary(
        vec![block(0, &[push(), push(), pop(), pop()])],
        &[[None, None]],
    );
    assert!(effect.initial_values_required);
    assert_eq!(effect.stack_min, Some(0));
    assert_eq!(effect.stack_max, Some(2));
    assert!(effect.declared_exits_balanced);
    assert!(!effect.all_boundaries_balanced);
    assert!(!effect.preserves_full_state);
}

#[test]
fn status_only_reads_metadata_without_value_slots() {
    let effect = summary(
        vec![block(0, &[X87Operation::StoreStatusAx])],
        &[[None, None]],
    );
    assert_eq!(effect.read_mask, 0);
    assert!(effect.reads_status);
    assert!(!effect.writes_status);
    assert!(!effect.writes_pointers);
    assert_eq!(effect.class, X87EffectClass::MetadataOnly);
    assert!(effect.preserves_full_state);
}

#[test]
fn eight_push_cycle_is_unknown_despite_modulo_top_balance() {
    let effect = summary(vec![block(0, &[push(); 8])], &[[Some(0), None]]);
    assert!(!effect.summary_known);
    assert_eq!(effect.stack_max, None);
    assert!(!effect.declared_exits_balanced);
}

#[test]
fn divergent_exit_balances_are_retained() {
    let effect = summary(
        vec![block(0, &[]), block(10, &[push()]), block(20, &[])],
        &[[Some(1), Some(2)], [None, None], [None, None]],
    );
    assert_eq!(
        effect
            .exit_balances
            .iter()
            .map(|exit| exit.delta)
            .collect::<Vec<_>>(),
        vec![Some(1), Some(0)]
    );
    assert!(!effect.declared_exits_balanced);
}

#[test]
fn memory_input_balanced_stack_does_not_require_initial_values() {
    let effect = summary(
        vec![block(
            0,
            &[X87Operation::LoadReal32 { address: operand() }, pop()],
        )],
        &[[None, None]],
    );
    assert_eq!(effect.class, X87EffectClass::IndependentBalanced);
    assert!(!effect.initial_values_required);
    assert!(effect.reads_control);
    assert!(effect.writes_status);
    assert!(effect.writes_pointers);
    assert!(!effect.preserves_full_state);
}
