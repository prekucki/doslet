use super::*;
use crate::{
    FileBackedSpan, RelocationTarget, Site, SymbolicImage, SymbolicObject,
    binding::{BindingLimits, DosFlatBindingPlan, DosFlatObjectBinding, DosFlatSelector},
    cfg::CfgLimits,
};
use alloc::{vec, vec::Vec};
use doslet_executable::{LeFixupSourceType, LeHeader, LeLocation, LeObject, LeObjectOffset};

pub(super) fn address(offset: u32) -> ObjectAddress {
    ObjectAddress {
        object_number: 1,
        offset,
    }
}
pub(super) fn image(code: &[u8]) -> SymbolicImage {
    SymbolicImage {
        header: LeHeader {
            location: LeLocation {
                container_offset: 0,
                header_offset: 0,
            },
            cpu_type: 2,
            os_type: 0,
            module_version: 0,
            module_flags: 0,
            page_count: 1,
            page_size: 4096,
            last_page_bytes: 0,
            object_count: 1,
            entry: Some(LeObjectOffset {
                object_number: 1,
                offset: 0,
            }),
            stack: None,
            auto_data_object: None,
            data_pages_offset: 0,
        },
        objects: vec![SymbolicObject {
            object_number: 1,
            metadata: LeObject {
                virtual_size: code.len() as u32,
                preferred_base: 0x1234_0000,
                flags: 0x2004,
                first_page_number: 1,
                page_count: 1,
            },
            bytes: code.to_vec(),
            file_backed_spans: vec![FileBackedSpan {
                object_offset: 0,
                file_offset: 0,
                byte_len: code.len() as u32,
            }],
        }],
        sites: Vec::new(),
        provenance: Vec::new(),
    }
}
pub(super) fn analyze(image: &SymbolicImage) -> StaticCfg<'_> {
    StaticCfg::analyze(image, &[], CfgLimits::default()).unwrap()
}
fn discover<'cfg, 'image>(graph: &'cfg StaticCfg<'image>) -> ProcedurePlan<'cfg, 'image> {
    ProcedurePlan::discover(graph, ProcedureLimits::default()).unwrap()
}

#[test]
fn watcom_order_is_only_a_hint_over_all_actual_register_inputs() {
    // ADD EAX,EDX; ADD EAX,EBX; ADD EAX,ECX; ADD EAX,ESI; RET.
    let image = image(&[1, 0xd0, 1, 0xd8, 1, 0xc8, 1, 0xf0, 0xc3]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert!(core::ptr::eq(plan.graph(), &graph));
    assert_eq!(plan.procedures().len(), 1);
    let procedure = &plan.procedures()[0];
    assert_eq!(procedure.entry, address(0));
    assert_eq!(procedure.ret, address(8));
    assert_eq!(procedure.byte_len, 9);
    assert_eq!(procedure.return_operand_width, 4);
    assert_eq!(procedure.return_pop_bytes, 0);
    assert_eq!(procedure.instruction_count(), 5);
    assert_eq!(procedure.input_gpr_bytes, [15, 15, 15, 15, 15, 0, 15, 0]);
    assert_eq!(procedure.modified_gpr_bytes, [15, 0, 0, 0, 15, 0, 0, 0]);
    assert_eq!(
        procedure.watcom32_register_inputs().collect::<Vec<_>>(),
        [(0, 15), (2, 15), (3, 15), (1, 15)]
    );
    assert_eq!(plan.node_indices(0), Some(&[0, 1, 2, 3, 4][..]));
    assert_eq!(
        plan.instructions(0)
            .unwrap()
            .map(|node| node.address)
            .collect::<Vec<_>>(),
        [address(0), address(2), address(4), address(6), address(8)]
    );
    assert!(plan.instructions(1).is_none());
    assert!(plan.stack_reads(1).is_none());
    assert!(plan.diagnostics().is_empty());
}

#[test]
fn stack_prologue_reads_are_offsets_and_never_inferred_c_types() {
    // PUSH EBP; MOV EBP,ESP; PUSH EBX; MOV EAX,[EBP+8]; ADD EAX,[EBP+12];
    // POP EBX; MOV ESP,EBP; POP EBP; RET 8.
    let image = image(&[
        0x55, 0x89, 0xe5, 0x53, 0x8b, 0x45, 8, 3, 0x45, 12, 0x5b, 0x89, 0xec, 0x5d, 0xc2, 8, 0,
    ]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    let procedure = &plan.procedures()[0];
    assert_eq!(procedure.input_gpr_bytes, [0, 0, 0, 15, 15, 15, 0, 0]);
    assert_eq!(procedure.modified_gpr_bytes, [15, 0, 0, 15, 15, 15, 0, 0]);
    assert_eq!(procedure.return_pop_bytes, 8);
    assert_eq!(procedure.memory_read_instructions, 5);
    assert_eq!(procedure.memory_write_instructions, 2);
    assert!(!procedure.stack_reads_uncertain);
    assert_eq!(
        plan.stack_reads(0)
            .unwrap()
            .iter()
            .map(|read| (read.instruction.offset, read.offset, read.byte_len))
            .collect::<Vec<_>>(),
        [(4, 4, 4), (7, 8, 4), (10, -8, 4), (13, -4, 4), (14, 0, 4)]
    );
}

#[test]
fn byte_aliases_track_only_read_before_write_bytes() {
    let image = image(&[0xb0, 1, 0xb4, 2, 1, 0xd0, 0xc3]); // AL=1; AH=2; ADD EAX,EDX.
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert_eq!(
        plan.procedures()[0].input_gpr_bytes,
        [12, 0, 15, 0, 15, 0, 0, 0]
    );
    assert_eq!(
        plan.procedures()[0].modified_gpr_bytes,
        [15, 0, 0, 0, 15, 0, 0, 0]
    );
}

#[test]
fn zero_idiom_and_implicit_multiply_aliases_use_iced_effects() {
    for (bytes, inputs, outputs) in [
        (
            &[0x31, 0xc0, 0x11, 0xd0, 0xc3][..],
            [0, 0, 15, 0, 15, 0, 0, 0],
            [15, 0, 0, 0, 15, 0, 0, 0],
        ),
        (
            &[0xf7, 0xe2, 0xc3][..],
            [15, 0, 15, 0, 15, 0, 0, 0],
            [15, 0, 15, 0, 15, 0, 0, 0],
        ),
        (
            &[0xf6, 0xe4, 0xc3][..],
            [3, 0, 0, 0, 15, 0, 0, 0],
            [3, 0, 0, 0, 15, 0, 0, 0],
        ),
        (
            &[0x6b, 0xc2, 5, 0xc3][..],
            [0, 0, 15, 0, 15, 0, 0, 0],
            [15, 0, 0, 0, 15, 0, 0, 0],
        ),
        (
            &[0x87, 0xd0, 0xc3][..],
            [15, 0, 15, 0, 15, 0, 0, 0],
            [15, 0, 15, 0, 15, 0, 0, 0],
        ),
    ] {
        let image = image(bytes);
        let graph = analyze(&image);
        let plan = discover(&graph);
        assert_eq!(plan.procedures()[0].input_gpr_bytes, inputs, "{bytes:x?}");
        assert_eq!(
            plan.procedures()[0].modified_gpr_bytes,
            outputs,
            "{bytes:x?}"
        );
    }
}

#[test]
fn architectural_flag_masks_preserve_undefined_and_zero_count_dependencies() {
    let cases: &[(&[u8], u32)] = &[
        (&[0x11, 0xd0, 0xc3], 1), // ADC reads architectural CF bit zero.
        (&[0x31, 0xc0, 0x11, 0xd0, 0xc3], 0x10), // XOR defines CF, retains undefined AF.
        (&[0x40, 0x11, 0xd0, 0xc3], 1), // INC preserves CF.
        (&[0xd3, 0xe0, 0x11, 0xd0, 0xc3], 0x8d5), // Count zero can preserve every arithmetic flag.
    ];
    for &(bytes, input_flags) in cases {
        let image = image(bytes);
        let graph = analyze(&image);
        let plan = discover(&graph);
        assert_eq!(plan.procedures()[0].input_flags, input_flags, "{bytes:x?}");
        assert_eq!(plan.procedures()[0].modified_flags, 0x8d5, "{bytes:x?}");
    }
}

#[test]
fn zero_masked_immediate_shift_is_not_a_definite_definition() {
    // SHL EAX,32 masks to zero; all destination bytes and flags are retained.
    let image = image(&[0xc1, 0xe0, 32, 0xc3]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert_eq!(plan.procedures()[0].definitely_written_gpr_bytes[0], 0);
    assert_eq!(plan.procedures()[0].definitely_written_flags, 0);
}

#[test]
fn implicit_stack_definitions_do_not_assume_live_stack_address_width() {
    for (bytes, definite_esp) in [
        (&[0x50, 0x5b, 0xc3][..], 3), // PUSH EAX; POP EBX; RET.
        (&[0x5c, 0xc3][..], 15),      // Explicit POP ESP defines the full register.
    ] {
        let image = image(bytes);
        let graph = analyze(&image);
        let plan = discover(&graph);
        assert_eq!(
            plan.procedures()[0].definitely_written_gpr_bytes[4],
            definite_esp
        );
    }
}

#[test]
fn near_return_operand_width_and_cleanup_are_independent() {
    let image = image(&[0x66, 0xc2, 0x34, 0x12]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    let procedure = &plan.procedures()[0];
    assert_eq!(procedure.return_operand_width, 2);
    assert_eq!(procedure.return_pop_bytes, 0x1234);
    assert_eq!(procedure.instruction_count(), 1);
    assert_eq!(plan.stack_reads(0).unwrap()[0].byte_len, 2);
}

#[test]
fn only_trusted_entries_and_resolved_direct_calls_seed_procedures() {
    // CALL 6; RET; ADD EAX,EDX; RET; unreachable NOP; RET.
    let image = image(&[0xe8, 1, 0, 0, 0, 0xc3, 1, 0xd0, 0xc3, 0x90, 0xc3]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert_eq!(plan.procedures().len(), 1);
    assert_eq!(plan.procedures()[0].entry, address(6));
    assert_eq!(
        plan.diagnostics(),
        &[ProcedureDiagnostic {
            entry: address(0),
            instruction: address(0),
            reason: ProcedureRejection::ControlFlow(InstructionFlow::DirectCall)
        }]
    );
    let graph =
        StaticCfg::analyze(&image, &[address(5), address(5)], CfgLimits::default()).unwrap();
    let plan = discover(&graph);
    assert_eq!(
        plan.procedures()
            .iter()
            .map(|procedure| procedure.entry)
            .collect::<Vec<_>>(),
        [address(5), address(6)]
    );
}

#[test]
fn trusted_interior_entry_rejects_outer_procedure() {
    let image = image(&[0xb8, 1, 0, 0, 0, 1, 0xd0, 0xc3]);
    let graph = StaticCfg::analyze(&image, &[address(5)], CfgLimits::default()).unwrap();
    let plan = discover(&graph);
    assert_eq!(plan.procedures().len(), 1);
    assert_eq!(plan.procedures()[0].entry, address(5));
    assert_eq!(
        plan.diagnostics()[0].reason,
        ProcedureRejection::AlternateInteriorEntry
    );
}

#[test]
fn independently_reachable_overlapping_streams_are_rejected() {
    let image = image(&[0xb8, 0x90, 0x90, 0x90, 0xc3, 0xc3]);
    let graph = StaticCfg::analyze(&image, &[address(1)], CfgLimits::default()).unwrap();
    let plan = discover(&graph);
    assert!(plan.procedures().is_empty());
    assert_eq!(plan.diagnostics().len(), 2);
    assert!(
        plan.diagnostics()
            .iter()
            .all(|diagnostic| diagnostic.reason == ProcedureRejection::OverlappingInstruction)
    );
}

#[test]
fn stopped_interior_entry_is_not_silently_swept() {
    let image = image(&[0xb8, 0x0f, 0x0f, 0x0f, 0x0f, 0xc3]);
    let graph = StaticCfg::analyze(&image, &[address(1)], CfgLimits::default()).unwrap();
    assert!(!graph.diagnostics().is_empty());
    let plan = discover(&graph);
    assert!(plan.procedures().is_empty());
    assert!(
        plan.diagnostics()
            .iter()
            .all(|diagnostic| matches!(diagnostic.reason, ProcedureRejection::Stopped(_)))
    );
}

#[test]
fn branches_calls_io_fpu_rep_segments_and_unsupported_scalar_are_explicit() {
    let cases: &[(&[u8], ProcedureRejection)] = &[
        (
            &[0x85, 0xc0, 0x74, 1, 0x90, 0xc3],
            ProcedureRejection::ControlFlow(InstructionFlow::ConditionalBranch),
        ),
        (
            &[0xeb, 0, 0xc3],
            ProcedureRejection::ControlFlow(InstructionFlow::DirectJump),
        ),
        (
            &[0xff, 0xd0, 0xc3],
            ProcedureRejection::ControlFlow(InstructionFlow::IndirectCall),
        ),
        (&[0xec, 0xc3], ProcedureRejection::UnsupportedInstruction),
        (
            &[0xd9, 0xe8, 0xc3],
            ProcedureRejection::UnsupportedInstruction,
        ),
        (&[0xf3, 0xa4, 0xc3], ProcedureRejection::UnsupportedPrefix),
        (
            &[0x8e, 0xd0, 0xc3],
            ProcedureRejection::UnsupportedInstruction,
        ),
        (
            &[0xf7, 0xf1, 0xc3],
            ProcedureRejection::UnsupportedInstruction,
        ),
        (&[0xcb], ProcedureRejection::UnsupportedInstruction),
        (
            &[0xf4],
            ProcedureRejection::ControlFlow(InstructionFlow::Halt),
        ),
    ];
    for &(bytes, reason) in cases {
        let image = image(bytes);
        let graph = analyze(&image);
        let plan = discover(&graph);
        assert!(plan.procedures().is_empty(), "{bytes:x?}");
        assert_eq!(plan.diagnostics()[0].reason, reason, "{bytes:x?}");
    }
}

#[test]
fn external_memory_wait_is_distinguished_from_computational_loop() {
    for (bytes, expected) in [
        (
            &[0xa1, 0, 0x10, 0, 0, 0x39, 0xd0, 0x74, 0xf7, 0xc3][..],
            ProcedureRejection::ExternalMemoryWait,
        ),
        (
            &[0x49, 0x85, 0xc9, 0x75, 0xfb, 0xc3][..],
            ProcedureRejection::ControlFlow(InstructionFlow::ConditionalBranch),
        ),
    ] {
        let image = image(bytes);
        let graph = analyze(&image);
        let plan = discover(&graph);
        assert!(plan.procedures().is_empty());
        assert_eq!(plan.diagnostics()[0].reason, expected);
    }
}

#[test]
fn unknown_stack_offsets_do_not_reject_the_machine_contract() {
    // MOV ESP,EAX; MOV EAX,[ESP+4]; RET.
    let image = image(&[0x89, 0xc4, 0x8b, 0x44, 0x24, 4, 0xc3]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert_eq!(plan.procedures().len(), 1);
    assert!(plan.procedures()[0].stack_reads_uncertain);
    assert!(plan.stack_reads(0).unwrap().is_empty());
}

#[test]
fn pop_sp_invalidates_full_entry_stack_offset_hint() {
    let image = image(&[0x66, 0x5c, 0x8b, 0x44, 0x24, 4, 0xc3]);
    let graph = analyze(&image);
    let plan = discover(&graph);
    assert_eq!(plan.procedures().len(), 1);
    assert!(plan.procedures()[0].stack_reads_uncertain);
    assert_eq!(plan.stack_reads(0).unwrap().len(), 1);
    assert_eq!(plan.stack_reads(0).unwrap()[0].instruction, address(0));
}

pub(super) fn flat_binding(image: &SymbolicImage) -> DosFlatBindingPlan<'_> {
    DosFlatBindingPlan::new(
        image,
        &[DosFlatObjectBinding {
            object_number: 1,
            linear_base: 0x1000,
            normal: Some(DosFlatSelector {
                value: 8,
                base: 0,
                limit: u32::MAX,
                default_bitness: DefaultBitness::Bits32,
            }),
            alias: None,
        }],
        BindingLimits::default(),
    )
    .unwrap()
}

#[test]
fn exact_bound_relocation_bytes_drive_stack_hints() {
    // ADD ESP,relocated imm32; MOV EAX,[ESP+4]; RET.
    let mut image = image(&[0x81, 0xc4, 0, 0, 0, 0, 0x8b, 0x44, 0x24, 4, 0xc3]);
    image.sites.push(Site {
        source: address(2),
        kind: LeFixupSourceType::Offset32,
        target: RelocationTarget {
            object_number: 1,
            offset: Some(0),
        },
        is_alias: false,
        provenance: 0..0,
    });
    let original = analyze(&image);
    let plan = discover(&original);
    assert_eq!(plan.stack_reads(0).unwrap()[0].offset, 4);
    let binding = flat_binding(&image);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let plan = discover(&graph);
    assert!(core::ptr::eq(plan.graph().binding().unwrap(), &binding));
    assert_eq!(plan.stack_reads(0).unwrap()[0].offset, 0x1004);
    assert_eq!(plan.stack_reads(0).unwrap()[1].offset, 0x1000);
    assert_eq!(image.objects()[0].bytes()[2..6], [0, 0, 0, 0]);
}

#[test]
fn relocated_call_target_is_a_root_only_after_cfg_resolves_it() {
    let mut image = image(&[0xe8, 0, 0, 0, 0, 0xc3, 1, 0xd0, 0xc3]);
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
    let original = analyze(&image);
    assert!(discover(&original).procedures().is_empty());
    let binding = flat_binding(&image);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let plan = discover(&graph);
    assert_eq!(plan.procedures().len(), 1);
    assert_eq!(plan.procedures()[0].entry, address(6));
}

#[test]
fn code16_and_stopped_roots_are_explicit() {
    let mut image = image(&[0xc3]);
    image.objects[0].metadata.flags &= !0x2000;
    let graph = analyze(&image);
    assert_eq!(
        discover(&graph).diagnostics()[0].reason,
        ProcedureRejection::UnsupportedCodeMode
    );
    let image = image_fixture_truncated();
    let graph = analyze(&image);
    assert!(matches!(
        discover(&graph).diagnostics()[0].reason,
        ProcedureRejection::Stopped(StopReason::TruncatedInstruction)
    ));
}
fn image_fixture_truncated() -> SymbolicImage {
    image(&[0x0f])
}

#[test]
fn per_procedure_bound_includes_return_and_no_partial_candidate_is_published() {
    let image = image(&[0x90, 0x90, 0xc3]);
    let graph = analyze(&image);
    let plan = ProcedurePlan::discover(
        &graph,
        ProcedureLimits {
            max_instructions_per_procedure: 2,
            ..ProcedureLimits::default()
        },
    )
    .unwrap();
    assert!(plan.procedures().is_empty());
    assert_eq!(
        plan.diagnostics()[0].reason,
        ProcedureRejection::InstructionBound
    );
    assert_eq!(plan.diagnostics()[0].instruction, address(2));
    assert_eq!(discover(&graph).procedures()[0].instruction_count(), 3);
}

#[test]
fn limits_are_independent_and_charged_before_reservation() {
    let image = image(&[0xe8, 1, 0, 0, 0, 0xc3, 0xc3]);
    let graph = analyze(&image);
    for (limits, expected) in [
        (
            ProcedureLimits {
                max_instructions: 0,
                ..ProcedureLimits::default()
            },
            ProcedureError::Limit(ProcedureResource::Instructions),
        ),
        (
            ProcedureLimits {
                max_edges: 0,
                ..ProcedureLimits::default()
            },
            ProcedureError::Limit(ProcedureResource::Edges),
        ),
        (
            ProcedureLimits {
                max_roots: 1,
                ..ProcedureLimits::default()
            },
            ProcedureError::Limit(ProcedureResource::Roots),
        ),
        (
            ProcedureLimits {
                max_procedures: 0,
                ..ProcedureLimits::default()
            },
            ProcedureError::Limit(ProcedureResource::Procedures),
        ),
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
                max_instructions_per_procedure: 0,
                ..ProcedureLimits::default()
            },
            ProcedureError::InvalidInstructionBound,
        ),
        (
            ProcedureLimits {
                max_instructions_per_procedure: MAX_PROCEDURE_INSTRUCTIONS + 1,
                ..ProcedureLimits::default()
            },
            ProcedureError::InvalidInstructionBound,
        ),
    ] {
        assert_eq!(
            ProcedurePlan::discover(&graph, limits).unwrap_err(),
            expected
        );
    }
    let mut budget = Budget(usize::MAX);
    let mut values = Vec::<u64>::new();
    assert_eq!(
        budget.reserve(&mut values, usize::MAX, ProcedureStorage::Instructions),
        Err(ProcedureError::OwnedByteLimit(
            ProcedureStorage::Instructions
        ))
    );
    assert_eq!(values.capacity(), 0);
}
