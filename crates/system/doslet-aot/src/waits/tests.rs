use super::*;
use crate::{
    FileBackedSpan, RelocationTarget, Site, SymbolicImage, SymbolicObject,
    binding::{BindingLimits, DosFlatBindingPlan, DosFlatObjectBinding, DosFlatSelector},
    cfg::{CfgLimits, StopReason},
};
use alloc::{vec, vec::Vec};
use doslet_executable::{LeFixupSourceType, LeHeader, LeLocation, LeObject, LeObjectOffset};

fn address(offset: u32) -> ObjectAddress {
    ObjectAddress {
        object_number: 1,
        offset,
    }
}

fn image(code: &[u8]) -> SymbolicImage {
    SymbolicImage {
        format: crate::ImageFormat::Le,
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

fn analyze(image: &SymbolicImage) -> StaticCfg<'_> {
    StaticCfg::analyze(image, &[], CfgLimits::default()).unwrap()
}

// MOV EAX,[1000h]; MOV [ESP],EAX; MOV EAX,[ESP]; CMP EAX,[2000h]; JE 0; RET.
// The compare reads a second variable; the stack store might restore its old
// value. Neither fact proves that a particular execution repeats unchanged.
const VARIABLE_WAIT: &[u8] = &[
    0xa1, 0, 0x10, 0, 0, 0x89, 0x04, 0x24, 0x8b, 0x04, 0x24, 0x3b, 0x05, 0, 0x20, 0, 0, 0x74, 0xed,
    0xc3,
];

#[test]
fn variable_comparison_and_same_value_store_are_profile_independent_candidates() {
    let image = image(VARIABLE_WAIT);
    let graph = analyze(&image);
    let plan = WaitCandidates::discover(&graph, WaitLimits::default()).unwrap();
    assert!(core::ptr::eq(plan.graph(), &graph));
    assert_eq!(plan.candidates().len(), 1);
    let candidate = &plan.candidates()[0];
    assert_eq!(candidate.head, address(0));
    assert_eq!(candidate.backedge, address(17));
    assert_eq!(candidate.byte_len, 19);
    assert_eq!(candidate.instruction_count(), 5);
    assert_eq!(candidate.recommended_proof_steps, 10);
    assert_eq!(candidate.memory_read_instructions, 3);
    assert_eq!(candidate.memory_write_instructions, 1);
    assert_eq!(plan.node_indices(0), Some(&[0, 1, 2, 3, 4][..]));
    assert_eq!(
        plan.instructions(0)
            .unwrap()
            .map(|node| node.address.offset)
            .collect::<Vec<_>>(),
        [0, 5, 8, 11, 17]
    );
    let source = plan.source_bytes(0).unwrap();
    assert_eq!(source, &VARIABLE_WAIT[..19]);
    assert!(core::ptr::eq(
        source.as_ptr(),
        image.objects()[0].bytes().as_ptr()
    ));
    assert!(plan.node_indices(1).is_none());
    assert!(plan.instructions(1).is_none());
    assert!(plan.source_bytes(1).is_none());
}

#[test]
fn work_mutating_loop_is_only_a_candidate() {
    // INC dword [1000h]; CMP dword [1000h],0; JNE 0; RET.
    let image = image(&[
        0xff, 0x05, 0, 0x10, 0, 0, 0x83, 0x3d, 0, 0x10, 0, 0, 0, 0x75, 0xf1, 0xc3,
    ]);
    let graph = analyze(&image);
    let plan = WaitCandidates::discover(&graph, WaitLimits::default()).unwrap();
    assert_eq!(plan.candidates().len(), 1);
    assert_eq!(plan.candidates()[0].memory_write_instructions, 1);
    assert_eq!(plan.candidates()[0].memory_read_instructions, 2);
}

#[test]
fn test_and_scalar_sizes_are_admitted() {
    for code in [
        &[0xf6, 0x05, 0, 0x10, 0, 0, 1, 0x74, 0xf7, 0xc3][..],
        &[0x66, 0x83, 0x3d, 0, 0x10, 0, 0, 0, 0x74, 0xf6, 0xc3][..],
        // CMP [1000h],EAX; LEA EBX,[EBX+1]; JNE 0. LEA preserves flags.
        &[0x39, 0x05, 0, 0x10, 0, 0, 0x8d, 0x5b, 1, 0x75, 0xf5, 0xc3][..],
    ] {
        let image = image(code);
        let graph = analyze(&image);
        assert_eq!(
            WaitCandidates::discover(&graph, WaitLimits::default())
                .unwrap()
                .candidates()
                .len(),
            1
        );
    }
}

fn loop_with_prefix(prefix: &[u8]) -> Vec<u8> {
    let mut code = prefix.to_vec();
    code.extend_from_slice(&[0x83, 0x3d, 0, 0x10, 0, 0, 0]); // CMP [1000h],0
    code.extend_from_slice(&[0x74, (-(code.len() as i16 + 2)) as u8, 0xc3]);
    code
}

#[test]
fn calls_io_privileged_rep_and_uncertain_effects_are_rejected() {
    for prefix in [
        &[0xe8, 0, 0, 0, 0][..], // CALL comparison itself.
        &[0xec][..],             // IN AL,DX
        &[0xee][..],             // OUT DX,AL
        &[0xfa][..],             // CLI
        &[0xfb][..],             // STI
        &[0x8e, 0xd8][..],       // MOV DS,AX
        &[0xf3, 0xa5][..],       // REP MOVSD
        &[0x0f, 0x31][..],       // RDTSC
        &[0xd9, 0xe8][..],       // FLD1
        &[0x50][..],             // PUSH EAX
        &[0xf0, 0xff, 0x00][..], // LOCK INC [EAX]
        &[0x87, 0x00][..],       // Implicitly locked XCHG [EAX],EAX
    ] {
        let image = image(&loop_with_prefix(prefix));
        let graph = analyze(&image);
        assert!(
            WaitCandidates::discover(&graph, WaitLimits::default())
                .unwrap()
                .candidates()
                .is_empty(),
            "prefix {prefix:x?}"
        );
    }
}

#[test]
fn internal_branches_and_joins_are_rejected() {
    // JZ 4; NOP; NOP; CMP [1000h],0; JE 0; RET.
    let image = image(&[
        0x74, 2, 0x90, 0x90, 0x83, 0x3d, 0, 0x10, 0, 0, 0, 0x74, 0xf3, 0xc3,
    ]);
    let graph = analyze(&image);
    assert!(
        WaitCandidates::discover(&graph, WaitLimits::default())
            .unwrap()
            .candidates()
            .is_empty()
    );

    // A trusted alternate entry into an otherwise straight loop rejects it.
    let image = super::tests::image(VARIABLE_WAIT);
    let graph = StaticCfg::analyze(&image, &[address(5)], CfgLimits::default()).unwrap();
    assert!(
        WaitCandidates::discover(&graph, WaitLimits::default())
            .unwrap()
            .candidates()
            .is_empty()
    );

    // An external resolved edge into the loop interior also rejects it.
    let mut code = VARIABLE_WAIT.to_vec();
    code.extend_from_slice(&[0xe9, 0xec, 0xff, 0xff, 0xff]); // JMP 5 from 20.
    let image = super::tests::image(&code);
    let graph = StaticCfg::analyze(&image, &[address(20)], CfgLimits::default()).unwrap();
    assert!(
        WaitCandidates::discover(&graph, WaitLimits::default())
            .unwrap()
            .candidates()
            .is_empty()
    );
}

#[test]
fn overlapping_and_stopped_alternative_streams_are_rejected() {
    let image = image(VARIABLE_WAIT);
    let graph = StaticCfg::analyze(&image, &[address(1)], CfgLimits::default()).unwrap();
    assert!(!graph.overlaps().is_empty());
    assert!(
        WaitCandidates::discover(&graph, WaitLimits::default())
            .unwrap()
            .candidates()
            .is_empty()
    );

    // Invalid LOCK prefix at an interior start does not form an overlap node,
    // but still makes a stopped alternative instruction stream explicit.
    let code = loop_with_prefix(&[0xb8, 0xf0, 0x90, 0, 0]);
    let image = super::tests::image(&code);
    let graph = StaticCfg::analyze(&image, &[address(1)], CfgLimits::default()).unwrap();
    assert!(graph.overlaps().is_empty());
    assert_eq!(
        graph.diagnostics()[0].reason,
        StopReason::InvalidInstruction
    );
    assert_eq!(
        WaitCandidates::discover(
            &graph,
            WaitLimits {
                max_diagnostics: 0,
                ..WaitLimits::default()
            }
        )
        .unwrap_err(),
        WaitError::Limit(WaitResource::Diagnostics)
    );
    assert!(
        WaitCandidates::discover(&graph, WaitLimits::default())
            .unwrap()
            .candidates()
            .is_empty()
    );
}

#[test]
fn comparison_and_real_memory_read_must_supply_the_conditional_branch() {
    for code in [
        &[0x39, 0xd8, 0x74, 0xfc, 0xc3][..], // No memory read.
        &[0xa1, 0, 0x10, 0, 0, 0x40, 0x75, 0xf8, 0xc3][..], // No comparison.
        &[0x8d, 0x05, 0, 0x10, 0, 0, 0x39, 0xd8, 0x74, 0xf6, 0xc3][..], // LEA is no read.
        &[0x83, 0x3d, 0, 0x10, 0, 0, 0, 0x40, 0x74, 0xf6, 0xc3][..], // INC overwrites flags.
        &[0x83, 0x3d, 0, 0x10, 0, 0, 0, 0xe2, 0xf7, 0xc3][..], // LOOP uses ECX.
        &[0x83, 0x3d, 0, 0x10, 0, 0, 0, 0xe3, 0xf7, 0xc3][..], // JECXZ uses ECX.
        &[0x83, 0x3d, 0, 0x10, 0, 0, 0, 0xeb, 0xf7, 0xc3][..], // Unconditional backedge.
    ] {
        let image = image(code);
        let graph = analyze(&image);
        assert!(
            WaitCandidates::discover(&graph, WaitLimits::default())
                .unwrap()
                .candidates()
                .is_empty(),
            "code {code:x?}"
        );
    }
}

#[test]
fn relocation_of_constants_is_allowed_but_encoding_or_control_changes_are_not() {
    let mut image = image(VARIABLE_WAIT);
    image.sites.push(Site {
        source: address(1),
        kind: LeFixupSourceType::Offset32,
        target: RelocationTarget {
            object_number: 1,
            offset: Some(10),
        },
        is_alias: false,
        provenance: 0..0,
    });
    let graph = analyze(&image);
    let plan = WaitCandidates::discover(&graph, WaitLimits::default()).unwrap();
    assert_eq!(plan.candidates().len(), 1);
    assert_eq!(plan.source_bytes(0).unwrap(), &VARIABLE_WAIT[..19]);
    image.sites[0].source = address(18);
    image.sites[0].kind = LeFixupSourceType::Byte;
    let graph = analyze(&image);
    assert!(
        WaitCandidates::discover(&graph, WaitLimits::default())
            .unwrap()
            .candidates()
            .is_empty()
    );
    image.sites[0].source = address(0);
    let graph = analyze(&image);
    assert!(
        WaitCandidates::discover(&graph, WaitLimits::default())
            .unwrap()
            .candidates()
            .is_empty()
    );
}

#[test]
fn discovery_never_sweeps_unreachable_bytes_or_uses_preferred_bases() {
    let mut code = vec![0xc3];
    code.extend_from_slice(VARIABLE_WAIT);
    let mut image = image(&code);
    image.objects[0].metadata.preferred_base = 0;
    let graph = analyze(&image);
    assert!(
        WaitCandidates::discover(&graph, WaitLimits::default())
            .unwrap()
            .candidates()
            .is_empty()
    );
    let graph = StaticCfg::analyze(&image, &[address(1)], CfgLimits::default()).unwrap();
    let plan = WaitCandidates::discover(&graph, WaitLimits::default()).unwrap();
    assert_eq!(plan.candidates()[0].head, address(1));
    assert_eq!(plan.candidates()[0].backedge, address(18));
}

#[test]
fn smaller_shape_bounds_reject_candidates_without_truncation() {
    let image = image(VARIABLE_WAIT);
    let graph = analyze(&image);
    let defaults = WaitLimits::default();
    for limits in [
        WaitLimits {
            max_instructions_per_loop: 4,
            ..defaults
        },
        WaitLimits {
            max_bytes_per_loop: 18,
            ..defaults
        },
    ] {
        assert!(
            WaitCandidates::discover(&graph, limits)
                .unwrap()
                .candidates()
                .is_empty()
        );
    }
    assert_eq!(
        WaitCandidates::discover(
            &graph,
            WaitLimits {
                max_instructions_per_loop: 5,
                max_bytes_per_loop: 19,
                ..defaults
            }
        )
        .unwrap()
        .candidates()
        .len(),
        1
    );
}

#[test]
fn hard_instruction_and_byte_boundaries_are_inclusive() {
    for (padding, expected) in [(30, 1), (31, 0)] {
        let prefix = vec![0x90; padding];
        let image = image(&loop_with_prefix(&prefix));
        let graph = analyze(&image);
        assert_eq!(
            WaitCandidates::discover(&graph, WaitLimits::default())
                .unwrap()
                .candidates()
                .len(),
            expected
        );
    }
    // 24 MOV EAX,0 instructions, CMP EAX,[1000h], JE 0: exactly 128 bytes.
    let mut code = Vec::new();
    for _ in 0..24 {
        code.extend_from_slice(&[0xb8, 0, 0, 0, 0]);
    }
    code.extend_from_slice(&[0x3b, 0x05, 0, 0x10, 0, 0, 0x74, 0x80, 0xc3]);
    let fixture = image(&code);
    let graph = analyze(&fixture);
    let plan = WaitCandidates::discover(&graph, WaitLimits::default()).unwrap();
    assert_eq!(plan.candidates().len(), 1);
    assert_eq!(plan.candidates()[0].byte_len, MAX_LOOP_BYTES);
    // Replace the final short branch by a six-byte near branch to the same
    // head. It remains within the instruction bound but exceeds the byte bound.
    code.truncate(126);
    code.extend_from_slice(&[0x0f, 0x84, 0x7c, 0xff, 0xff, 0xff, 0xc3]);
    let fixture = image(&code);
    let graph = analyze(&fixture);
    assert!(
        WaitCandidates::discover(&graph, WaitLimits::default())
            .unwrap()
            .candidates()
            .is_empty()
    );
}

#[test]
fn resource_and_policy_limits_are_explicit_without_partial_success() {
    let image = image(VARIABLE_WAIT);
    let graph = analyze(&image);
    let defaults = WaitLimits::default();
    for (limits, error) in [
        (
            WaitLimits {
                max_instructions_per_loop: 0,
                ..defaults
            },
            WaitError::InvalidInstructionBound,
        ),
        (
            WaitLimits {
                max_instructions_per_loop: 33,
                ..defaults
            },
            WaitError::InvalidInstructionBound,
        ),
        (
            WaitLimits {
                max_bytes_per_loop: 0,
                ..defaults
            },
            WaitError::InvalidByteBound,
        ),
        (
            WaitLimits {
                max_bytes_per_loop: 129,
                ..defaults
            },
            WaitError::InvalidByteBound,
        ),
        (
            WaitLimits {
                max_instructions: graph.nodes().len() - 1,
                ..defaults
            },
            WaitError::Limit(WaitResource::Instructions),
        ),
        (
            WaitLimits {
                max_edges: graph.edges().len() - 1,
                ..defaults
            },
            WaitError::Limit(WaitResource::Edges),
        ),
        (
            WaitLimits {
                max_candidates: 0,
                ..defaults
            },
            WaitError::Limit(WaitResource::Candidates),
        ),
        (
            WaitLimits {
                max_scan_steps: 0,
                ..defaults
            },
            WaitError::Limit(WaitResource::ScanSteps),
        ),
        (
            WaitLimits {
                max_owned_bytes: 0,
                ..defaults
            },
            WaitError::OwnedByteLimit(WaitStorage::Index),
        ),
        (
            WaitLimits {
                max_owned_bytes: graph.nodes().len() * size_of::<State>(),
                ..defaults
            },
            WaitError::OwnedByteLimit(WaitStorage::Instructions),
        ),
    ] {
        assert_eq!(WaitCandidates::discover(&graph, limits).unwrap_err(), error);
    }
    let graph = StaticCfg::analyze(&image, &[address(1)], CfgLimits::default()).unwrap();
    assert_eq!(
        WaitCandidates::discover(
            &graph,
            WaitLimits {
                max_overlaps: 0,
                ..defaults
            }
        )
        .unwrap_err(),
        WaitError::Limit(WaitResource::Overlaps)
    );
}

#[test]
fn scan_and_owned_storage_limits_are_charged_before_work_and_reservation() {
    let image = image(VARIABLE_WAIT);
    let graph = analyze(&image);
    let defaults = WaitLimits::default();
    let scan_steps =
        graph.entries().len() + graph.overlaps().len() + graph.edges().len() * 2 + 1 + 5;
    assert_eq!(
        WaitCandidates::discover(
            &graph,
            WaitLimits {
                max_scan_steps: scan_steps - 1,
                ..defaults
            }
        )
        .unwrap_err(),
        WaitError::Limit(WaitResource::ScanSteps)
    );
    assert_eq!(
        WaitCandidates::discover(
            &graph,
            WaitLimits {
                max_scan_steps: scan_steps,
                ..defaults
            }
        )
        .unwrap()
        .candidates()
        .len(),
        1
    );
    // The first index-vector growth requests at most the input node bound.
    let storage = graph.nodes().len() * (size_of::<State>() + size_of::<usize>())
        + size_of::<WaitCandidate>();
    assert_eq!(
        WaitCandidates::discover(
            &graph,
            WaitLimits {
                max_owned_bytes: storage - 1,
                ..defaults
            }
        )
        .unwrap_err(),
        WaitError::OwnedByteLimit(WaitStorage::Candidates)
    );
    assert_eq!(
        WaitCandidates::discover(
            &graph,
            WaitLimits {
                max_owned_bytes: storage,
                ..defaults
            }
        )
        .unwrap()
        .candidates()
        .len(),
        1
    );
}

#[test]
fn unresolved_code16_backedge_cannot_become_a_candidate() {
    let mut image = image(&[0x83, 0x3e, 0, 0x10, 0, 0x74, 0xf9, 0xc3]);
    image.objects[0].metadata.flags &= !0x2000;
    let graph = analyze(&image);
    assert!(
        WaitCandidates::discover(&graph, WaitLimits::default())
            .unwrap()
            .candidates()
            .is_empty()
    );
}

#[test]
fn multiple_loops_are_deterministically_ordered_by_instruction_identity() {
    let mut code = VARIABLE_WAIT.to_vec();
    code.extend_from_slice(VARIABLE_WAIT);
    let image = image(&code);
    for entries in [[address(20), address(0)], [address(0), address(20)]] {
        let graph = StaticCfg::analyze(&image, &entries, CfgLimits::default()).unwrap();
        let plan = WaitCandidates::discover(&graph, WaitLimits::default()).unwrap();
        assert_eq!(
            plan.candidates()
                .iter()
                .map(|candidate| candidate.head)
                .collect::<Vec<_>>(),
            [address(0), address(20)]
        );
        assert_eq!(
            plan.candidates()
                .iter()
                .map(WaitCandidate::instruction_count)
                .sum::<usize>(),
            10
        );
        assert_eq!(plan.source_bytes(0), plan.source_bytes(1));
    }
}

fn predicate_loop(body: &[u8]) -> Vec<u8> {
    let mut code = body.to_vec();
    assert!(body.len() + 2 <= 128);
    code.extend_from_slice(&[0x74, (-(body.len() as i16 + 2)) as u8, 0xc3]);
    code
}

fn classify(body: &[u8]) -> (WaitEvidence, Vec<u32>) {
    let fixture = image(&predicate_loop(body));
    let graph = analyze(&fixture);
    let plan = WaitCandidates::discover(&graph, WaitLimits::default()).unwrap();
    assert_eq!(plan.candidates().len(), 1, "body {body:x?}");
    (
        plan.candidates()[0].evidence,
        plan.predicate_read_instructions(0)
            .unwrap()
            .map(|node| node.address.offset)
            .collect(),
    )
}

#[test]
fn predicate_provenance_supports_variable_to_variable_compares() {
    for (body, reads) in [
        // MOV EAX,[1000h]; CMP EAX,EDX.
        (&[0xa1, 0, 0x10, 0, 0, 0x39, 0xd0][..], vec![0]),
        // MOV EAX,[1000h]; CMP EAX,[2000h].
        (
            &[0xa1, 0, 0x10, 0, 0, 0x3b, 0x05, 0, 0x20, 0, 0][..],
            vec![0, 5],
        ),
        // MOV EDX,1; MOV EAX,[1000h]; CMP EAX,EDX.
        (
            &[0xba, 1, 0, 0, 0, 0xa1, 0, 0x10, 0, 0, 0x39, 0xd0][..],
            vec![5],
        ),
    ] {
        assert_eq!(classify(body), (WaitEvidence::ExternalMemory, reads));
    }
}

#[test]
fn local_temporary_forwarding_preserves_original_external_read_identities() {
    // CalcTics-like shape: MOV EAX,[1000h]; MOV [1004h],EAX;
    // MOV EAX,[1004h]; CMP EAX,[1008h]. All accesses use the same DS.
    let body = [
        0xa1, 0, 0x10, 0, 0, 0xa3, 4, 0x10, 0, 0, 0xa1, 4, 0x10, 0, 0, 0x3b, 0x05, 8, 0x10, 0, 0,
    ];
    assert_eq!(classify(&body), (WaitEvidence::ExternalMemory, vec![0, 15]));
    // An independent same-value temporary store is also allowed.
    assert_eq!(
        classify(&[
            0xc7, 0x05, 4, 0x10, 0, 0, 7, 0, 0, 0, 0xa1, 0, 0x10, 0, 0, 0x39, 0xd0,
        ])
        .0,
        WaitEvidence::ExternalMemory
    );
    // An intervening unknown write prevents forwarding the temporary.
    let mut interrupted = body[..10].to_vec();
    interrupted.extend_from_slice(&[0x89, 0x0b]); // MOV [EBX],ECX.
    interrupted.extend_from_slice(&body[10..]);
    assert_eq!(classify(&interrupted).0, WaitEvidence::LocalProgress);
}

#[test]
fn local_induction_and_address_traversal_do_not_become_external_waits() {
    for body in [
        // Useful traversal: MOV EAX,[ESI]; ADD ESI,4; CMP ESI,EDI.
        &[0x8b, 0x06, 0x83, 0xc6, 4, 0x39, 0xfe][..],
        // Even a memory-dependent predicate observes a changing location.
        &[0x8b, 0x06, 0x83, 0xc6, 4, 0x39, 0xd0][..],
        // An unrelated read does not make a local counter predicate external.
        &[0xa1, 0, 0x10, 0, 0, 0x41, 0x39, 0xf9][..],
        // A predicate mixing external data and a local induction variable.
        &[0x42, 0xa1, 0, 0x10, 0, 0, 0x39, 0xd0][..],
        // Same-register comparison is independent of the earlier read.
        &[0xa1, 0, 0x10, 0, 0, 0x39, 0xc0][..],
        // TEST with zero and explicit overwrite both remove the dependency.
        &[0xa1, 0, 0x10, 0, 0, 0xa9, 0, 0, 0, 0][..],
        &[0xa1, 0, 0x10, 0, 0, 0x31, 0xc0, 0x39, 0xd0][..],
    ] {
        assert_eq!(classify(body).0, WaitEvidence::LocalProgress, "{body:x?}");
    }
    // An extra local counter can coexist with an independent waiting predicate.
    assert_eq!(
        classify(&[0x41, 0xa1, 0, 0x10, 0, 0, 0x39, 0xd0]).0,
        WaitEvidence::ExternalMemory
    );
}

#[test]
fn full_access_widths_and_partial_overlap_are_checked() {
    for (read, store, expected) in [
        // Dword read, last-byte overlap and immediately adjacent byte store.
        (
            &[0xa1, 0, 0x10, 0, 0][..],
            &[0xc6, 0x05, 3, 0x10, 0, 0, 0][..],
            WaitEvidence::LocalProgress,
        ),
        (
            &[0xa1, 0, 0x10, 0, 0][..],
            &[0xc6, 0x05, 4, 0x10, 0, 0, 0][..],
            WaitEvidence::ExternalMemory,
        ),
        // Word read and a dword store beginning one byte earlier.
        (
            &[0x66, 0xa1, 0, 0x10, 0, 0][..],
            &[0xc7, 0x05, 0xff, 0x0f, 0, 0, 0, 0, 0, 0][..],
            WaitEvidence::LocalProgress,
        ),
        // Byte read and an adjacent word store.
        (
            &[0xa0, 0, 0x10, 0, 0][..],
            &[0x66, 0xc7, 0x05, 1, 0x10, 0, 0, 0, 0][..],
            WaitEvidence::ExternalMemory,
        ),
    ] {
        let mut body = read.to_vec();
        body.extend_from_slice(store);
        body.extend_from_slice(&[0x84, 0xc0]); // TEST AL,AL.
        assert_eq!(classify(&body).0, expected, "{body:x?}");
    }
    // A store before a complete overwrite of the observed memory prevents an
    // external claim too; the read forwards a local value and loses its source.
    assert_eq!(
        classify(&[0x89, 0x15, 0, 0x10, 0, 0, 0xa1, 0, 0x10, 0, 0, 0x39, 0xd8,]).0,
        WaitEvidence::LocalProgress
    );
}

#[test]
fn unknown_pointers_and_distinct_segments_remain_uncertain() {
    for body in [
        // MOV [EBX],ECX may alias the direct DS read.
        &[0xa1, 0, 0x10, 0, 0, 0x89, 0x0b, 0x39, 0xd0][..],
        // A different FS offset does not prove a different linear address.
        &[0xa1, 0, 0x10, 0, 0, 0x64, 0xa3, 0, 0x20, 0, 0, 0x39, 0xd0][..],
        // SS-based stack storage does not imply disjointness from DS globals.
        &[0xa1, 0, 0x10, 0, 0, 0x89, 0x04, 0x24, 0x39, 0xd0][..],
        // The predicate dereferences a pointer fetched from memory each time.
        &[0x8b, 0x35, 0, 0x20, 0, 0, 0x83, 0x3e, 0][..],
        // Incoming flags can affect a variable compared with external memory.
        &[
            0xa1, 0, 0x10, 0, 0, 0xba, 0, 0, 0, 0, 0x83, 0xda, 0, 0x39, 0xd0,
        ][..],
    ] {
        assert_eq!(classify(body).0, WaitEvidence::Uncertain, "{body:x?}");
    }
    assert_eq!(classify(&VARIABLE_WAIT[..17]).0, WaitEvidence::Uncertain);
}

#[test]
fn symbolic_and_constant_addresses_can_prove_unrelated_temporary_stores() {
    for body in [
        // MOV EAX,[EBX]; MOV [EBX+4],EAX; CMP EAX,EDX.
        &[0x8b, 0x03, 0x89, 0x43, 4, 0x39, 0xd0][..],
        // Copying a base register preserves its identity.
        &[0x89, 0xd9, 0x8b, 0x03, 0x89, 0x41, 4, 0x39, 0xd0][..],
        // A constant pointer write can be resolved to a disjoint address.
        &[
            0xbb, 0, 0x20, 0, 0, 0xa1, 0, 0x10, 0, 0, 0x89, 0x0b, 0x39, 0xd0,
        ][..],
        // A zero-offset LEA preserves the same address identity.
        &[0x8d, 0x0b, 0x8b, 0x03, 0x89, 0x41, 4, 0x39, 0xd0][..],
    ] {
        assert_eq!(classify(body).0, WaitEvidence::ExternalMemory, "{body:x?}");
    }
    // Identical symbolic bases still overlap when complete widths intersect.
    assert_eq!(
        classify(&[0x8b, 0x03, 0x89, 0x43, 3, 0x39, 0xd0]).0,
        WaitEvidence::LocalProgress
    );
}

#[test]
fn byte_and_word_register_aliases_preserve_only_actual_dependencies() {
    for body in [
        // Replacing AH leaves the read in AL.
        &[0xa1, 0, 0x10, 0, 0, 0xb4, 1, 0x38, 0xd0][..],
        // Modifying AH is independent of the AL waiting predicate.
        &[0xa0, 0, 0x10, 0, 0, 0xfe, 0xc4, 0x38, 0xd0][..],
        // Zero-extension retains just the loaded byte dependency.
        &[0x0f, 0xb6, 0x05, 0, 0x10, 0, 0, 0x39, 0xd0][..],
        // Sign-extension retains the loaded word dependency.
        &[0x0f, 0xbf, 0x05, 0, 0x10, 0, 0, 0x39, 0xd0][..],
    ] {
        assert_eq!(classify(body).0, WaitEvidence::ExternalMemory, "{body:x?}");
    }
    for body in [
        // Replacing AX removes the memory dependency in AL.
        &[0xa1, 0, 0x10, 0, 0, 0x66, 0xb8, 1, 0, 0x38, 0xd0][..],
        // Changing DL changes the compared EDX input.
        &[0xfe, 0xc2, 0xa1, 0, 0x10, 0, 0, 0x39, 0xd0][..],
        // Changing BL changes an EBX-based read address.
        &[0x8b, 0x03, 0xfe, 0xc3, 0x39, 0xd0][..],
    ] {
        assert_eq!(classify(body).0, WaitEvidence::LocalProgress, "{body:x?}");
    }
}

fn flat_binding(image: &SymbolicImage, base: u32) -> DosFlatBindingPlan<'_> {
    DosFlatBindingPlan::new(
        image,
        &[DosFlatObjectBinding {
            object_number: 1,
            linear_base: base,
            normal: Some(DosFlatSelector {
                value: 0x28,
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
fn bound_predicate_analysis_uses_relocated_bytes_for_overlap_proofs() {
    // MOV EAX,[0]; MOV [3000h],EDX; CMP EAX,ECX.
    let mut fixture = image(&predicate_loop(&[
        0xa1, 0, 0, 0, 0, 0x89, 0x15, 0, 0x30, 0, 0, 0x39, 0xc8,
    ]));
    fixture.sites.push(Site {
        source: address(1),
        kind: LeFixupSourceType::Offset32,
        target: RelocationTarget {
            object_number: 1,
            offset: Some(0),
        },
        is_alias: false,
        provenance: 0..0,
    });
    // Raw constants are never a relocation-independent alias proof.
    let graph = analyze(&fixture);
    let plan = WaitCandidates::discover(&graph, WaitLimits::default()).unwrap();
    assert_eq!(plan.candidates()[0].evidence, WaitEvidence::Uncertain);
    for (base, expected) in [
        (0x2000, WaitEvidence::ExternalMemory),
        (0x3000, WaitEvidence::LocalProgress),
    ] {
        let binding = flat_binding(&fixture, base);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let plan = WaitCandidates::discover(&graph, WaitLimits::default()).unwrap();
        assert_eq!(plan.candidates().len(), 1);
        assert_eq!(plan.candidates()[0].evidence, expected);
        assert_eq!(plan.source_bytes(0).unwrap()[1..5], [0, 0, 0, 0]);
    }
}

#[test]
fn a_flat_cs_binding_still_does_not_prove_ds_ss_disjointness() {
    let fixture = image(VARIABLE_WAIT);
    let binding = flat_binding(&fixture, 0x3000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let plan = WaitCandidates::discover(&graph, WaitLimits::default()).unwrap();
    assert_eq!(plan.candidates()[0].evidence, WaitEvidence::Uncertain);
    assert!(plan.predicate_read_instructions(1).is_none());
}
