//! PC24-only recovery of a long, exact prefix from a rejected candidate.
use super::*;
use crate::wasm::memory_procedures::{MemoryProcedureLimits, PreparedMemoryProcedures};
use doslet_x86_wasm::static_program::procedure_memory::{
    ProcedureCompileOptions, ProcedureTerminatorKind,
};

fn prefix(count: usize, tail: &[u8]) -> Vec<u8> {
    let mut code = vec![0xdb, 0x00]; // FILD integer32[EAX].
    code.resize(count + 1, 0x43); // Remaining instructions are INC EBX.
    code.extend_from_slice(tail);
    code
}

fn prepare(code: &[u8], limits: MemoryProcedureLimits, pc24: bool) -> PreparedMemoryProcedures {
    let image = image(code, DefaultBitness::Bits32);
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    if pc24 {
        PreparedMemoryProcedures::from_bound_graph_with_pc24_fragments(&graph, &binding, limits, 32)
    } else {
        PreparedMemoryProcedures::from_bound_graph_with_long_fragments(&graph, &binding, limits, 32)
    }
    .unwrap()
}

fn assert_prefix(prepared: &PreparedMemoryProcedures, count: usize) {
    let index = prepared
        .entries()
        .iter()
        .position(|entry| *entry == at(0))
        .expect("retained root");
    let function = &prepared.program().functions[index];
    assert_eq!(function.blocks.len(), count + 1);
    assert_eq!(
        function
            .blocks
            .iter()
            .map(|block| block.instructions.len())
            .sum::<usize>(),
        count
    );
    assert!(function.blocks[..count].iter().all(|block| matches!(
        block.terminator.kind,
        ProcedureTerminatorKind::Fallthrough(_)
    )));
    let boundary = function.blocks.last().unwrap();
    assert!(boundary.instructions.is_empty());
    assert_eq!(boundary.terminator.kind, ProcedureTerminatorKind::Exit);
    assert_eq!(boundary.entry.offset, 0x10000 + count as u32 + 1);
    let sources = prepared.sources(index).unwrap();
    assert_eq!(sources.len(), count + 1);
    assert_eq!(sources.last(), Some(&at(count as u32 + 1)));
    let compiled = prepared
        .program()
        .compile_with_pc24_locals_and_work_limits(
            &ProcedureCompileOptions::default(),
            prepared.max_work_per_call().unwrap(),
            4096,
        )
        .unwrap();
    assert_eq!(compiled.maximum_work(function.id), Some(count as u64));
    assert_eq!(compiled.finite_maximum_work(function.id), None);
    assert_eq!(compiled.x87(function.id).unwrap().read_mask, 0);
    assert_eq!(compiled.x87(function.id).unwrap().write_mask, 0x80);
}

#[test]
fn instruction_bound_salvages_mixed_prefix_with_boundary_and_ordinary_work_cap() {
    for (node_limit, work_limit, expected) in [(40, 128, 39), (80, 64, 64)] {
        let prepared = prepare(
            &prefix(200, &[0xc3]),
            MemoryProcedureLimits {
                max_nodes_per_procedure: node_limit,
                max_work_per_call: Some(work_limit),
                ..Default::default()
            },
            true,
        );
        assert_prefix(&prepared, expected);
    }
}

#[test]
fn unsupported_cyclic_diamond_salvages_only_before_its_first_branch() {
    let prepared = prepare(
        &prefix(32, &[0x74, 4, 0xeb, 0xfe, 0xcc, 0xcc, 0xeb, 0xfe]),
        MemoryProcedureLimits {
            max_work_per_call: Some(128),
            ..Default::default()
        },
        true,
    );
    assert_prefix(&prepared, 32);
}

#[test]
fn conflicting_static_top_join_salvages_exact_pre_branch_stack_delta() {
    // One branch duplicates ST0 before the shared RET; the other executes NOP.
    let prepared = prepare(
        &prefix(32, &[0x74, 4, 0xd9, 0xc0, 0xeb, 1, 0x90, 0xc3]),
        MemoryProcedureLimits {
            max_work_per_call: Some(128),
            ..Default::default()
        },
        true,
    );
    assert_prefix(&prepared, 32);
}

#[test]
fn rejected_prefix_below_32_or_below_minimum_work_grant_stays_declined() {
    for (count, limit) in [(31, 128), (40, 31)] {
        let prepared = prepare(
            &prefix(count, &[0x74, 4, 0xeb, 0xfe, 0xcc, 0xcc, 0xeb, 0xfe]),
            MemoryProcedureLimits {
                max_work_per_call: Some(limit),
                ..Default::default()
            },
            true,
        );
        assert!(!prepared.entries().contains(&at(0)));
    }
}

#[test]
fn legacy_discovery_keeps_x87_boundaries_and_successful_pc24_candidates_unchanged() {
    let limits = MemoryProcedureLimits {
        max_work_per_call: Some(128),
        ..Default::default()
    };
    let rejected = prefix(32, &[0x74, 4, 0xeb, 0xfe, 0xcc, 0xcc, 0xeb, 0xfe]);
    let legacy = prepare(&rejected, limits, false);
    assert!(!legacy.entries().contains(&at(0)));
    let accepted = prepare(&prefix(32, &[0xc3]), limits, true);
    let index = accepted
        .entries()
        .iter()
        .position(|entry| *entry == at(0))
        .unwrap();
    let function = &accepted.program().functions[index];
    assert!(matches!(
        function.blocks.last().unwrap().terminator.kind,
        ProcedureTerminatorKind::Return { .. }
    ));
    assert_eq!(
        function
            .blocks
            .iter()
            .map(|block| block.instructions.len())
            .sum::<usize>(),
        32
    );
}

#[test]
fn prefix_recovery_consumes_the_existing_global_scan_budget() {
    let image = image(&prefix(200, &[0xc3]), DefaultBitness::Bits32);
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let result = PreparedMemoryProcedures::from_bound_graph_with_pc24_fragments(
        &graph,
        &binding,
        MemoryProcedureLimits {
            max_nodes_per_procedure: 40,
            max_work_per_call: Some(128),
            max_scan_steps: 40,
            ..Default::default()
        },
        32,
    );
    assert!(matches!(
        result,
        Err(crate::wasm::memory_procedures::MemoryProcedureError::ScanLimit)
    ));
}

fn status_loop() -> Vec<u8> {
    vec![
        0xd9, 0xc0, // FLD ST0: an outer backedge changes the static TOP.
        0xdf, 0xe0, // FNSTSW AX; continuation starts at offset 4.
        0xf6, 0xc4, 1, // TEST AH,1.
        0x74, 0xf7, // JZ offset 0.
        0xb9, 3, 0, 0, 0, // MOV ECX,3.
        0x43, 0x49, 0x75, 0xfc, // Inner loop: INC EBX; DEC ECX; JNZ 14.
        0x4a, 0x75, 0xeb, // DEC EDX; JNZ offset 0.
        0xc3,
    ]
}

#[test]
fn status_continuation_retries_with_lower_exits_and_retains_useful_inner_loop() {
    let limits = MemoryProcedureLimits {
        max_work_per_call: Some(128),
        ..Default::default()
    };
    let prepared = prepare(&status_loop(), limits, true);
    let index = prepared
        .entries()
        .iter()
        .position(|entry| *entry == at(4))
        .expect("status continuation");
    let function = &prepared.program().functions[index];
    let boundary = function
        .blocks
        .iter()
        .find(|block| block.entry.offset == 0x10000)
        .expect("lower exit keeps its source");
    assert_eq!(boundary.terminator.kind, ProcedureTerminatorKind::Exit);
    assert!(boundary.instructions.is_empty());
    assert!(
        function
            .blocks
            .iter()
            .any(|block| block.entry.offset == 0x10000 + 14 && !block.instructions.is_empty())
    );
    assert!(function.blocks.iter().any(|block| matches!(block.terminator.kind, ProcedureTerminatorKind::Branch { taken, .. } if taken.offset == 0x10000 + 14)));
    function.validate_with_pc24_work_limit(128).unwrap();
    assert!(
        !prepared
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.entry == at(4))
    );
    let legacy = prepare(&status_loop(), limits, false);
    let legacy_index = legacy
        .entries()
        .iter()
        .position(|entry| *entry == at(4))
        .expect("legacy status boundary");
    assert_eq!(
        function.blocks,
        legacy.program().functions[legacy_index].blocks
    );
}

#[test]
fn successful_unfloored_status_continuation_keeps_its_lower_return() {
    let code = [0xeb, 1, 0xc3, 0xdf, 0xe0, 0xeb, 0xfb];
    let prepared = prepare(
        &code,
        MemoryProcedureLimits {
            max_work_per_call: Some(128),
            ..Default::default()
        },
        true,
    );
    let index = prepared
        .entries()
        .iter()
        .position(|entry| *entry == at(5))
        .expect("status continuation");
    let function = &prepared.program().functions[index];
    assert!(
        function
            .blocks
            .iter()
            .any(|block| block.entry.offset == 0x10002
                && matches!(
                    block.terminator.kind,
                    ProcedureTerminatorKind::Return { .. }
                ))
    );
    assert!(
        !function
            .blocks
            .iter()
            .any(|block| matches!(block.terminator.kind, ProcedureTerminatorKind::Exit))
    );
}

#[test]
fn rejected_status_candidate_tries_useful_floor_before_long_prefix_salvage() {
    let original = status_loop();
    let mut code = original[..4].to_vec();
    code.extend_from_slice(&[0x43; 40]);
    code.extend_from_slice(&original[4..]);
    code[48] = (-49i8) as u8;
    code[60] = (-61i8) as u8;
    let prepared = prepare(
        &code,
        MemoryProcedureLimits {
            max_work_per_call: Some(128),
            ..Default::default()
        },
        true,
    );
    let index = prepared
        .entries()
        .iter()
        .position(|entry| *entry == at(4))
        .expect("status continuation");
    let function = &prepared.program().functions[index];
    assert!(function.blocks.iter().any(|block| matches!(block.terminator.kind, ProcedureTerminatorKind::Branch { taken, .. } if taken.offset == 0x10000 + 54)));
    assert!(
        function
            .blocks
            .iter()
            .any(|block| block.entry.offset == 0x10000 + 54 && !block.instructions.is_empty())
    );
    assert!(
        !function
            .blocks
            .iter()
            .any(|block| block.entry.offset == 0x10000 + 47
                && matches!(block.terminator.kind, ProcedureTerminatorKind::Exit))
    );
}

#[test]
fn status_seeds_never_floor_complete_graph_or_call_roots() {
    for call_root in [false, true] {
        let mut code = status_loop();
        let extra = if call_root {
            // An additional real caller reaches the same status continuation.
            let caller = code.len() as u32;
            code.push(0xe8);
            code.extend_from_slice(&(4i32 - (caller as i32 + 5)).to_le_bytes());
            code.push(0xc3);
            at(caller)
        } else {
            at(4)
        };
        let image = image(&code, DefaultBitness::Bits32);
        let binding = binding(&image, 0x10000);
        let graph = StaticCfg::analyze_bound(&binding, &[extra], CfgLimits::default()).unwrap();
        let prepared = PreparedMemoryProcedures::from_bound_graph_with_pc24_fragments(
            &graph,
            &binding,
            MemoryProcedureLimits {
                max_work_per_call: Some(128),
                ..Default::default()
            },
            32,
        )
        .unwrap();
        assert!(
            !prepared.entries().contains(&at(4)),
            "complete root must retain the unsupported full body"
        );
    }
}
