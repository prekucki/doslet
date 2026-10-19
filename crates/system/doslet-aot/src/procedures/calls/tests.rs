use super::super::tests::{address, analyze, flat_binding, image as make_image};
use super::*;
use crate::{RelocationTarget, Site, cfg::CfgLimits};
use alloc::{vec, vec::Vec};
use doslet_executable::LeFixupSourceType;

fn discover<'cfg, 'image>(graph: &'cfg StaticCfg<'image>) -> CallProcedurePlan<'cfg, 'image> {
    CallProcedurePlan::discover(graph, CallProcedureLimits::default()).unwrap()
}

fn chain() -> crate::SymbolicImage {
    // A: CALL B; RET. B: CALL C; RET. C: MOV EAX,7; RET.
    make_image(&[
        0xe8, 1, 0, 0, 0, 0xc3, 0xe8, 1, 0, 0, 0, 0xc3, 0xb8, 7, 0, 0, 0, 0xc3,
    ])
}

#[test]
fn transitive_chain_has_closed_calls_callee_first_order_and_architectural_effects() {
    let image = chain();
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert!(core::ptr::eq(plan.graph(), &graph));
    assert!(plan.diagnostics().is_empty());
    assert_eq!(
        plan.procedures()
            .iter()
            .map(|value| (
                value.body.entry,
                value.body.instruction_count(),
                value.expanded_instruction_count,
                value.max_call_depth
            ))
            .collect::<Vec<_>>(),
        [
            (address(0), 2, 6, 2),
            (address(6), 2, 4, 1),
            (address(12), 2, 2, 0)
        ]
    );
    assert_eq!(plan.topological_order(), [2, 1, 0]);
    assert_eq!(
        plan.callsites(0),
        Some(
            &[ProcedureCall {
                instruction: address(0),
                callee: 1
            }][..]
        )
    );
    assert_eq!(
        plan.callsites(1),
        Some(
            &[ProcedureCall {
                instruction: address(6),
                callee: 2
            }][..]
        )
    );
    assert_eq!(plan.callsites(2), Some(&[][..]));
    assert_eq!(plan.procedure_index(address(6)), Some(1));
    assert_eq!(plan.procedure_index(address(5)), None);
    assert_eq!(
        plan.instructions(0)
            .unwrap()
            .map(|value| value.address)
            .collect::<Vec<_>>(),
        [address(0), address(5)]
    );
    assert_eq!(plan.node_indices(0).unwrap().len(), 2);
    assert!(plan.node_indices(3).is_none());
    assert!(plan.instructions(3).is_none());
    assert!(plan.stack_reads(3).is_none());
    assert!(plan.callsites(3).is_none());
    let a = &plan.procedures()[0];
    assert_eq!(a.body.modified_gpr_bytes[0], 0);
    assert_eq!(a.closure.modified_gpr_bytes[0], 15);
    assert_eq!(a.closure.definitely_written_gpr_bytes[0], 15);
    assert_eq!(a.closure.input_gpr_bytes[0], 0);
    assert_eq!(a.closure.input_gpr_bytes[4], 15);
    assert_eq!(a.closure.modified_gpr_bytes[4], 15);
    assert_eq!(a.closure.memory_write_instructions, 2); // Both guest return-address stores.
    assert_eq!(a.closure.memory_read_instructions, 3); // Every guest RET still reads memory.
    assert!(a.closure.stack_reads_uncertain);
    assert!(plan.stack_reads(0).unwrap().is_empty());
    // The opt-in call plan does not change original leaf discovery.
    let leaves = ProcedurePlan::discover(&graph, ProcedureLimits::default()).unwrap();
    assert_eq!(leaves.procedures().len(), 1);
    assert_eq!(leaves.procedures()[0].entry, address(12));
    assert_eq!(leaves.diagnostics().len(), 2);
}

#[test]
fn repeated_shared_callee_counts_each_execution_without_expanding_storage() {
    // A calls B twice. B calls C. C increments EAX.
    let image = make_image(&[
        0xe8, 6, 0, 0, 0, 0xe8, 1, 0, 0, 0, 0xc3, 0xe8, 1, 0, 0, 0, 0xc3, 0x40, 0xc3,
    ]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert_eq!(plan.procedures().len(), 3);
    let a = &plan.procedures()[0];
    assert_eq!(a.body.instruction_count(), 3);
    assert_eq!(a.expanded_instruction_count, 11);
    assert_eq!(a.max_call_depth, 2);
    assert_eq!(a.closure.memory_write_instructions, 4);
    assert_eq!(a.closure.memory_read_instructions, 5);
    assert_eq!(a.closure.input_gpr_bytes[0], 15);
    assert_eq!(
        plan.callsites(0)
            .unwrap()
            .iter()
            .map(|call| call.callee)
            .collect::<Vec<_>>(),
        [1, 1]
    );
}

#[test]
fn register_byte_inputs_compose_at_call_positions() {
    // A: MOV AL,1; CALL B; MOV EBX,EAX; RET. B: MOV AH,2; RET.
    let image = make_image(&[0xb0, 1, 0xe8, 3, 0, 0, 0, 0x89, 0xc3, 0xc3, 0xb4, 2, 0xc3]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    let a = &plan.procedures()[0];
    assert_eq!(a.body.input_gpr_bytes[0], 14);
    assert_eq!(a.closure.input_gpr_bytes[0], 12);
    assert_eq!(a.closure.modified_gpr_bytes[0], 3);
    assert_eq!(a.closure.definitely_written_gpr_bytes[0], 3);
    assert_eq!(a.closure.definitely_written_gpr_bytes[3], 15);
    // Reads in a callee before its own definition remain caller dependencies.
    let image = make_image(&[0xe8, 1, 0, 0, 0, 0xc3, 0x01, 0xd0, 0xb8, 7, 0, 0, 0, 0xc3]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert_eq!(plan.procedures()[0].closure.input_gpr_bytes[0], 15);
    assert_eq!(plan.procedures()[0].closure.input_gpr_bytes[2], 15);
}

#[test]
fn conditional_register_writes_and_retained_flags_are_not_definitions() {
    // A: CALL B; MOV EBX,EAX; ADC EDX,EDI; RET. B: SHL EAX,CL; RET.
    let image = make_image(&[
        0xe8, 5, 0, 0, 0, 0x89, 0xc3, 0x11, 0xfa, 0xc3, 0xd3, 0xe0, 0xc3,
    ]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    let a = &plan.procedures()[0];
    let b = &plan.procedures()[1];
    assert_eq!(b.body.modified_gpr_bytes[0], 15);
    assert_eq!(b.body.definitely_written_gpr_bytes[0], 0);
    assert_eq!(b.closure.definitely_written_gpr_bytes[0], 0);
    assert_eq!(b.closure.modified_flags, 0x8d5);
    assert_eq!(b.closure.definitely_written_flags, 0);
    assert_eq!(a.closure.input_gpr_bytes[0], 15);
    assert_eq!(a.closure.input_gpr_bytes[1], 1);
    assert_eq!(a.closure.input_flags, 0x8d5);
    assert_eq!(a.closure.definitely_written_gpr_bytes[0], 0);
    // MUL defines CF/OF and retains undefined arithmetic flag bits.
    let image = make_image(&[0xe8, 3, 0, 0, 0, 0x11, 0xd0, 0xc3, 0xf7, 0xe2, 0xc3]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert_eq!(plan.procedures()[1].closure.definitely_written_flags, 0x801);
    assert_eq!(plan.procedures()[1].closure.input_flags, 0xd4);
    assert_eq!(plan.procedures()[0].closure.input_flags, 0xd4);
}

#[test]
fn calls_preserve_cleanup_and_do_not_invent_stack_offsets() {
    // A: CALL B; MOV EAX,[ESP+4]; RET. B: RET 8.
    let image = make_image(&[0xe8, 5, 0, 0, 0, 0x8b, 0x44, 0x24, 4, 0xc3, 0xc2, 8, 0]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert!(plan.diagnostics().is_empty());
    assert_eq!(plan.procedures()[1].body.return_pop_bytes, 8);
    assert_eq!(plan.procedures()[0].body.return_pop_bytes, 0);
    assert_eq!(plan.procedures()[0].closure.memory_read_instructions, 3);
    assert_eq!(plan.procedures()[0].closure.memory_write_instructions, 1);
    assert!(plan.procedures()[0].closure.stack_reads_uncertain);
    assert!(plan.stack_reads(0).unwrap().is_empty());
}

#[test]
fn near32_calls_require_near32_returns_and_other_call_forms_are_explicit() {
    let image = make_image(&[0xe8, 1, 0, 0, 0, 0xc3, 0x66, 0xc3]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert_eq!(plan.procedures().len(), 1);
    assert_eq!(plan.procedures()[0].body.return_operand_width, 2);
    assert_eq!(
        plan.diagnostics()[0].reason,
        CallProcedureRejection::ReturnWidthMismatch {
            callee: address(6),
            expected: 4,
            actual: 2,
        }
    );
    for (bytes, rejection) in [
        (
            &[0x66, 0xe8, 0, 0, 0xc3][..],
            ProcedureRejection::UnsupportedCallWidth,
        ),
        (&[0xff, 0xd0, 0xc3][..], ProcedureRejection::IndirectCall),
        (
            &[0x9a, 0, 0, 0, 0, 8, 0, 0xc3][..],
            ProcedureRejection::FarCall,
        ),
    ] {
        let image = make_image(bytes);
        let graph = analyze(&image);
        let plan = discover(&graph);
        assert!(plan.procedures().is_empty());
        assert_eq!(
            plan.diagnostics()[0].reason,
            CallProcedureRejection::Body(rejection)
        );
    }
    let image = make_image(&[0xe8, 0xff, 0xff, 0xff, 0x7f, 0xc3]);
    let graph = analyze(&image);
    assert!(matches!(
        discover(&graph).diagnostics()[0].reason,
        CallProcedureRejection::Body(ProcedureRejection::UnresolvedCall(_))
    ));
}

#[test]
fn unsupported_and_indirect_callees_reject_their_callers() {
    for tail in [
        &[0xec, 0xc3][..],
        &[0xff, 0xd0, 0xc3][..],
        &[0xe8, 0xff, 0xff, 0xff, 0x7f, 0xc3][..],
    ] {
        let mut bytes = vec![0xe8, 1, 0, 0, 0, 0xc3];
        bytes.extend_from_slice(tail);
        let image = make_image(&bytes);
        let graph = analyze(&image);
        let plan = discover(&graph);
        assert!(plan.procedures().is_empty());
        assert_eq!(
            plan.diagnostics()[0].reason,
            CallProcedureRejection::UnsupportedCallee { callee: address(6) }
        );
        assert!(matches!(
            plan.diagnostics()[1].reason,
            CallProcedureRejection::Body(_)
        ));
    }
}

#[test]
fn recursive_scc_members_are_distinguished_from_dependent_callers() {
    let image = make_image(&[0xe8, 0xfb, 0xff, 0xff, 0xff, 0xc3]); // CALL self; RET.
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert!(plan.procedures().is_empty());
    assert_eq!(
        plan.diagnostics()[0].reason,
        CallProcedureRejection::RecursiveCallCycle
    );
    // A -> B -> C -> B. Only B and C belong to the recursive SCC.
    let image = make_image(&[
        0xe8, 1, 0, 0, 0, 0xc3, 0xe8, 1, 0, 0, 0, 0xc3, 0xe8, 0xf5, 0xff, 0xff, 0xff, 0xc3,
    ]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert!(plan.procedures().is_empty());
    assert_eq!(
        plan.diagnostics()
            .iter()
            .map(|value| value.reason)
            .collect::<Vec<_>>(),
        [
            CallProcedureRejection::UnsupportedCallee { callee: address(6) },
            CallProcedureRejection::RecursiveCallCycle,
            CallProcedureRejection::RecursiveCallCycle,
        ]
    );
}

#[test]
fn complete_scc_detection_handles_a_shared_already_visited_cycle_path() {
    // A -> B and C; B -> A; C -> B. Upstream D -> A is outside the SCC.
    let image = make_image(&[
        0xe8, 6, 0, 0, 0, 0xe8, 7, 0, 0, 0, 0xc3, 0xe8, 0xf0, 0xff, 0xff, 0xff, 0xc3, 0xe8, 0xf5,
        0xff, 0xff, 0xff, 0xc3, 0xe8, 0xe4, 0xff, 0xff, 0xff, 0xc3,
    ]);
    let graph = StaticCfg::analyze(&image, &[address(23)], CfgLimits::default()).unwrap();
    let plan = discover(&graph);
    assert_eq!(plan.diagnostics().len(), 4);
    assert!(
        plan.diagnostics()[..3]
            .iter()
            .all(|value| value.reason == CallProcedureRejection::RecursiveCallCycle)
    );
    assert_eq!(
        plan.diagnostics()[3].reason,
        CallProcedureRejection::UnsupportedCallee { callee: address(0) }
    );
}

#[test]
fn externally_dependent_wait_callee_is_not_admitted_as_atomic_work() {
    let image = make_image(&[
        0xe8, 1, 0, 0, 0, 0xc3, 0xa1, 0, 0x10, 0, 0, 0x39, 0xd0, 0x74, 0xf7, 0xc3,
    ]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert!(plan.procedures().is_empty());
    assert_eq!(
        plan.diagnostics()[0].reason,
        CallProcedureRejection::UnsupportedCallee { callee: address(6) }
    );
    assert_eq!(
        plan.diagnostics()[1].reason,
        CallProcedureRejection::Body(ProcedureRejection::ExternalMemoryWait)
    );
}

#[test]
fn call_and_expanded_work_limits_do_not_publish_partial_closures() {
    let image = chain();
    let graph = analyze(&image);
    assert_eq!(
        CallProcedurePlan::discover(
            &graph,
            CallProcedureLimits {
                max_call_edges: 0,
                ..CallProcedureLimits::default()
            }
        )
        .unwrap_err(),
        ProcedureError::Limit(ProcedureResource::CallEdges)
    );
    for (limits, expected) in [
        (
            CallProcedureLimits {
                max_call_depth: 0,
                ..CallProcedureLimits::default()
            },
            CallProcedureRejection::CallDepthBound,
        ),
        (
            CallProcedureLimits {
                max_expanded_instructions: 3,
                ..CallProcedureLimits::default()
            },
            CallProcedureRejection::ExpandedInstructionBound,
        ),
    ] {
        let plan = CallProcedurePlan::discover(&graph, limits).unwrap();
        assert_eq!(plan.procedures().len(), 1);
        assert_eq!(plan.procedures()[0].body.entry, address(12));
        assert_eq!(plan.topological_order(), [0]);
        assert_eq!(plan.diagnostics()[1].reason, expected);
        assert_eq!(
            plan.diagnostics()[0].reason,
            CallProcedureRejection::UnsupportedCallee { callee: address(6) }
        );
    }
    let plan = CallProcedurePlan::discover(
        &graph,
        CallProcedureLimits {
            max_expanded_instructions: 0,
            ..CallProcedureLimits::default()
        },
    )
    .unwrap();
    assert!(plan.procedures().is_empty());
    assert_eq!(
        plan.diagnostics()[2].reason,
        CallProcedureRejection::ExpandedInstructionBound
    );
    for (procedures, expected) in [
        (
            ProcedureLimits {
                max_scan_steps: 0,
                ..ProcedureLimits::default()
            },
            ProcedureError::Limit(ProcedureResource::ScanSteps),
        ),
        (
            ProcedureLimits {
                max_owned_bytes: 0,
                ..ProcedureLimits::default()
            },
            ProcedureError::OwnedByteLimit(ProcedureStorage::Index),
        ),
        (
            ProcedureLimits {
                max_procedures: 0,
                ..ProcedureLimits::default()
            },
            ProcedureError::Limit(ProcedureResource::Procedures),
        ),
    ] {
        assert_eq!(
            CallProcedurePlan::discover(
                &graph,
                CallProcedureLimits {
                    procedures,
                    ..CallProcedureLimits::default()
                }
            )
            .unwrap_err(),
            expected
        );
    }
}

#[test]
fn exponential_repeated_call_work_checks_u64_overflow_without_expansion() {
    // Sixty-four tiny bodies each call the next body twice; only their source
    // bytes are stored. W(leaf)=1; W(n)=3+2*W(n-1)=4*2^n-3.
    let mut bytes = Vec::new();
    for _ in 0..64 {
        bytes.extend_from_slice(&[0xe8, 6, 0, 0, 0, 0xe8, 1, 0, 0, 0, 0xc3]);
    }
    bytes.push(0xc3);
    let image = make_image(&bytes);
    let graph = analyze(&image);
    let plan = CallProcedurePlan::discover(
        &graph,
        CallProcedureLimits {
            max_call_depth: u16::MAX,
            max_expanded_instructions: u64::MAX,
            ..CallProcedureLimits::default()
        },
    )
    .unwrap();
    assert_eq!(plan.procedures().len(), 63);
    assert_eq!(plan.procedures()[0].body.entry, address(22));
    assert_eq!(
        plan.procedures()[0].expanded_instruction_count,
        u64::MAX - 2
    );
    assert_eq!(plan.procedures()[0].max_call_depth, 62);
    assert_eq!(
        plan.diagnostics()[0].reason,
        CallProcedureRejection::UnsupportedCallee {
            callee: address(11)
        }
    );
    assert_eq!(
        plan.diagnostics()[1].reason,
        CallProcedureRejection::ExpandedInstructionBound
    );
}

#[test]
fn deep_source_graph_is_walked_iteratively_despite_small_eligibility_depth() {
    let mut bytes = Vec::new();
    for _ in 0..2048 {
        bytes.extend_from_slice(&[0xe8, 1, 0, 0, 0, 0xc3]);
    }
    bytes.push(0xc3);
    let image = make_image(&bytes);
    let graph = analyze(&image);
    let plan = CallProcedurePlan::discover(
        &graph,
        CallProcedureLimits {
            max_call_depth: 8,
            ..CallProcedureLimits::default()
        },
    )
    .unwrap();
    assert_eq!(plan.procedures().len(), 9);
    assert_eq!(plan.procedures()[0].max_call_depth, 8);
    assert_eq!(plan.procedures()[0].expanded_instruction_count, 17);
    assert_eq!(plan.diagnostics().len(), 2040);
    assert_eq!(
        plan.diagnostics().last().unwrap().reason,
        CallProcedureRejection::CallDepthBound
    );
    assert!(plan.diagnostics()[..2039].iter().all(|diagnostic| matches!(
        diagnostic.reason,
        CallProcedureRejection::UnsupportedCallee { .. }
    )));
}

#[test]
fn bound_relocation_resolves_call_closure_using_original_image_without_mutation() {
    let mut image = make_image(&[0xe8, 0, 0, 0, 0, 0xc3, 0x40, 0xc3]);
    image.sites.push(Site {
        source: address(1),
        kind: LeFixupSourceType::SelfRelative32,
        target: RelocationTarget {
            object_number: 1,
            offset: Some(6),
        },
        is_alias: false,
        provenance: 0..0,
    });
    let graph = analyze(&image);
    assert!(discover(&graph).procedures().is_empty());
    let binding = flat_binding(&image);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let plan = discover(&graph);
    assert!(plan.diagnostics().is_empty());
    assert_eq!(plan.procedures()[0].expanded_instruction_count, 4);
    assert_eq!(plan.callsites(0).unwrap()[0].callee, 1);
    assert!(core::ptr::eq(plan.graph().binding().unwrap(), &binding));
    assert_eq!(&image.objects()[0].bytes()[1..5], [0, 0, 0, 0]);
}
