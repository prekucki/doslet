use super::*;
use crate::{FileBackedSpan, RelocationTarget, Site, SymbolicObject};
use alloc::{vec, vec::Vec};
use doslet_executable::{LeFixupSourceType, LeHeader, LeLocation, LeObject, LeObjectOffset};

fn address(offset: u32) -> ObjectAddress {
    ObjectAddress {
        object_number: 1,
        offset,
    }
}

fn image(code: &[u8], bitness: DefaultBitness) -> SymbolicImage {
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
                flags: 4 | if bitness == DefaultBitness::Bits32 {
                    0x2000
                } else {
                    0
                },
                first_page_number: 1,
                page_count: 1,
            },
            bytes: code.to_vec(),
            file_backed_spans: vec![FileBackedSpan {
                object_offset: 0,
                file_offset: 123,
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

fn offsets(graph: &StaticCfg<'_>) -> Vec<u32> {
    graph
        .nodes()
        .iter()
        .map(|node| node.address.offset)
        .collect()
}

fn has_edge(graph: &StaticCfg<'_>, source: u32, kind: EdgeKind, target: EdgeTarget) -> bool {
    graph.edges().contains(&CfgEdge {
        source: address(source),
        kind,
        target,
    })
}

fn unresolved(
    graph: &StaticCfg<'_>,
    source: u32,
    kind: EdgeKind,
    reason: UnresolvedReason,
) -> bool {
    has_edge(graph, source, kind, EdgeTarget::Unresolved(reason))
}

fn add_fixup(image: &mut SymbolicImage, source: u32, kind: LeFixupSourceType, target: u32) {
    image.sites.push(Site {
        source: address(source),
        kind,
        target: RelocationTarget {
            object_number: 1,
            offset: Some(target),
        },
        is_alias: false,
        provenance: 0..0,
    });
}

#[test]
fn traverses_direct_call_and_both_unprofiled_paths() {
    // CALL 10; JNZ 9; NOP; RET; padding; RET; INC EAX; RET
    let image = image(
        &[0xe8, 5, 0, 0, 0, 0x75, 2, 0x90, 0xc3, 0xc3, 0x40, 0xc3],
        DefaultBitness::Bits32,
    );
    let graph = analyze(&image);
    assert!(core::ptr::eq(graph.image(), &image));
    assert_eq!(offsets(&graph), [0, 5, 7, 8, 9, 10, 11]);
    for (source, kind, target) in [
        (0, EdgeKind::Call, 10),
        (0, EdgeKind::CallContinuation, 5),
        (5, EdgeKind::Branch, 9),
        (5, EdgeKind::Fallthrough, 7),
    ] {
        assert!(has_edge(
            &graph,
            source,
            kind,
            EdgeTarget::Address(address(target))
        ));
    }
    assert!(graph.diagnostics().is_empty());
}

#[test]
fn shared_tails_and_cycles_decode_once_in_deterministic_order() {
    let image = image(&[0x75, 2, 0xeb, 0xfc, 0xeb, 0xfa], DefaultBitness::Bits32);
    let first = analyze(&image);
    let second = analyze(&image);
    assert_eq!(offsets(&first), [0, 2, 4]);
    assert_eq!(first.nodes(), second.nodes());
    assert_eq!(first.edges(), second.edges());
    assert_eq!(first.edges().len(), 4);
}

#[test]
fn operand_sizes_do_not_select_code_bitness() {
    let image = image(
        &[0xb0, 0x7f, 0x66, 0xb8, 0x34, 0x12, 0xb8, 1, 2, 3, 4, 0xc3],
        DefaultBitness::Bits32,
    );
    let graph = analyze(&image);
    assert_eq!(offsets(&graph), [0, 2, 6, 11]);
    assert_eq!(
        graph
            .nodes()
            .iter()
            .map(|node| node.byte_len)
            .collect::<Vec<_>>(),
        [2, 4, 5, 1]
    );
    assert!(
        graph
            .nodes()
            .iter()
            .all(|node| node.bitness == DefaultBitness::Bits32)
    );
    let image = super::tests::image(
        &[0xb0, 0x7f, 0xb8, 0x34, 0x12, 0x66, 0xb8, 1, 2, 3, 4, 0xc3],
        DefaultBitness::Bits16,
    );
    let graph = analyze(&image);
    assert_eq!(offsets(&graph), [0, 2, 5, 11]);
    assert_eq!(
        graph
            .nodes()
            .iter()
            .map(|node| node.byte_len)
            .collect::<Vec<_>>(),
        [2, 3, 6, 1]
    );
    assert!(
        graph
            .nodes()
            .iter()
            .all(|node| node.bitness == DefaultBitness::Bits16)
    );
}

#[test]
fn loop_and_address_override_keep_both_edges() {
    for code in [
        &[0xe2, 0xfe, 0xc3][..],
        &[0x67, 0xe2, 0xfd, 0xc3][..],
        &[0xe3, 0xfe, 0xc3][..],
    ] {
        let image = image(code, DefaultBitness::Bits32);
        let graph = analyze(&image);
        assert_eq!(offsets(&graph), [0, code.len() as u32 - 1]);
        assert!(has_edge(
            &graph,
            0,
            EdgeKind::Branch,
            EdgeTarget::Address(address(0))
        ));
        assert!(has_edge(
            &graph,
            0,
            EdgeKind::Fallthrough,
            EdgeTarget::Address(address(code.len() as u32 - 1))
        ));
    }
}

#[test]
fn indirect_calls_keep_continuation_and_indirect_jumps_stop() {
    let image = image(&[0xff, 0xd0, 0xff, 0x20, 0x90], DefaultBitness::Bits32);
    let graph = analyze(&image);
    assert_eq!(offsets(&graph), [0, 2]);
    assert!(unresolved(
        &graph,
        0,
        EdgeKind::Call,
        UnresolvedReason::IndirectTarget
    ));
    assert!(has_edge(
        &graph,
        0,
        EdgeKind::CallContinuation,
        EdgeTarget::Address(address(2))
    ));
    assert!(unresolved(
        &graph,
        2,
        EdgeKind::Branch,
        UnresolvedReason::IndirectTarget
    ));
}

#[test]
fn direct_and_indirect_far_transfers_remain_unresolved() {
    for code in [
        &[0x9a, 0, 0, 0, 0, 0x18, 0, 0xc3][..],
        &[0xff, 0x18, 0xc3][..],
    ] {
        let image = image(code, DefaultBitness::Bits32);
        let graph = analyze(&image);
        assert_eq!(graph.nodes()[0].flow, InstructionFlow::FarCall);
        assert!(unresolved(
            &graph,
            0,
            EdgeKind::Call,
            UnresolvedReason::FarTarget
        ));
        assert_eq!(graph.nodes().len(), 2);
    }
    for code in [
        &[0xea, 0, 0, 0, 0, 0x18, 0, 0x90][..],
        &[0xff, 0x28, 0x90][..],
    ] {
        let image = image(code, DefaultBitness::Bits32);
        let graph = analyze(&image);
        assert_eq!(graph.nodes().len(), 1);
        assert_eq!(graph.nodes()[0].flow, InstructionFlow::FarJump);
        assert!(unresolved(
            &graph,
            0,
            EdgeKind::Branch,
            UnresolvedReason::FarTarget
        ));
    }
}

#[test]
fn interrupts_and_halt_record_exit_and_reachable_continuation() {
    let image = image(
        &[0xcd, 0x21, 0xcc, 0xce, 0xf4, 0xc3],
        DefaultBitness::Bits32,
    );
    let graph = analyze(&image);
    assert_eq!(offsets(&graph), [0, 2, 3, 4, 5]);
    for (source, next) in [(0, 2), (2, 3), (3, 4)] {
        assert!(unresolved(
            &graph,
            source,
            EdgeKind::Exit,
            UnresolvedReason::RuntimeService
        ));
        assert!(has_edge(
            &graph,
            source,
            EdgeKind::ServiceContinuation,
            EdgeTarget::Address(address(next))
        ));
    }
    assert!(unresolved(
        &graph,
        4,
        EdgeKind::Exit,
        UnresolvedReason::Halt
    ));
    assert!(has_edge(
        &graph,
        4,
        EdgeKind::Resume,
        EdgeTarget::Address(address(5))
    ));
}

#[test]
fn returns_iret_exceptions_and_system_transfers_stop() {
    for (code, flow, reason) in [
        (
            &[0xc3][..],
            InstructionFlow::Return,
            UnresolvedReason::ReturnTarget,
        ),
        (
            &[0xcb][..],
            InstructionFlow::Return,
            UnresolvedReason::ReturnTarget,
        ),
        (
            &[0xcf][..],
            InstructionFlow::InterruptReturn,
            UnresolvedReason::InterruptReturnTarget,
        ),
        (
            &[0x0f, 0x0b][..],
            InstructionFlow::Exception,
            UnresolvedReason::Exception,
        ),
        (
            &[0x0f, 0x34][..],
            InstructionFlow::SystemTransfer,
            UnresolvedReason::SystemTransfer,
        ),
        (
            &[0x0f, 0x35][..],
            InstructionFlow::SystemTransfer,
            UnresolvedReason::SystemTransfer,
        ),
        (
            &[0xc7, 0xf8, 0, 0, 0, 0][..],
            InstructionFlow::TransactionalTransfer,
            UnresolvedReason::TransactionalTransfer,
        ),
        (
            &[0xc6, 0xf8, 0][..],
            InstructionFlow::TransactionalTransfer,
            UnresolvedReason::TransactionalTransfer,
        ),
        (
            &[0x0f, 0x01, 0xd5][..],
            InstructionFlow::TransactionalTransfer,
            UnresolvedReason::TransactionalTransfer,
        ),
    ] {
        let image = image(code, DefaultBitness::Bits32);
        let graph = analyze(&image);
        assert_eq!(graph.nodes().len(), 1);
        assert_eq!(graph.nodes()[0].flow, flow);
        assert_eq!(graph.edges().len(), 1);
        assert!(unresolved(&graph, 0, EdgeKind::Exit, reason));
    }
}

#[test]
fn adjacent_backed_spans_allow_one_instruction_across_pages() {
    let mut image = image(&[0xb8, 1, 2, 3, 4, 0xc3], DefaultBitness::Bits32);
    image.objects[0].file_backed_spans = vec![
        FileBackedSpan {
            object_offset: 0,
            file_offset: 900,
            byte_len: 2,
        },
        FileBackedSpan {
            object_offset: 2,
            file_offset: 100,
            byte_len: 4,
        },
    ];
    let graph = analyze(&image);
    assert_eq!(offsets(&graph), [0, 5]);
    assert_eq!(graph.nodes()[0].byte_len, 5);
}

#[test]
fn bss_and_holes_never_supply_instruction_bytes() {
    let mut image = image(&[0xb8, 1, 2, 3, 4, 0xc3], DefaultBitness::Bits32);
    image.objects[0].file_backed_spans[0].byte_len = 2;
    image.objects[0].file_backed_spans.push(FileBackedSpan {
        object_offset: 5,
        file_offset: 999,
        byte_len: 1,
    });
    let graph =
        StaticCfg::analyze(&image, &[address(5), address(3)], CfgLimits::default()).unwrap();
    assert_eq!(offsets(&graph), [5]);
    assert_eq!(
        graph.diagnostics(),
        [
            CfgDiagnostic {
                address: address(0),
                reason: StopReason::TruncatedInstruction
            },
            CfgDiagnostic {
                address: address(3),
                reason: StopReason::NotFileBacked
            },
        ]
    );
}

#[test]
fn fallthrough_to_bss_or_object_end_is_explicit() {
    let mut image = image(&[0x90, 0x90], DefaultBitness::Bits32);
    image.objects[0].file_backed_spans[0].byte_len = 1;
    let graph = analyze(&image);
    assert_eq!(offsets(&graph), [0]);
    assert!(unresolved(
        &graph,
        0,
        EdgeKind::Fallthrough,
        UnresolvedReason::Unavailable {
            address: address(1),
            reason: StopReason::NotFileBacked
        }
    ));
    let image = super::tests::image(&[0x90], DefaultBitness::Bits32);
    let graph = analyze(&image);
    assert!(unresolved(
        &graph,
        0,
        EdgeKind::Fallthrough,
        UnresolvedReason::Unavailable {
            address: address(1),
            reason: StopReason::OutsideObject
        }
    ));
}

#[test]
fn invalid_and_truncated_paths_stop_without_byte_sweep() {
    for (code, reason) in [
        (&[0xf0, 0x90, 0xc3][..], StopReason::InvalidInstruction),
        (&[0xb8, 1, 2][..], StopReason::TruncatedInstruction),
        (&[0x66; 15][..], StopReason::InvalidInstruction),
    ] {
        let image = image(code, DefaultBitness::Bits32);
        let graph = analyze(&image);
        assert!(graph.nodes().is_empty());
        assert_eq!(
            graph.diagnostics(),
            [CfgDiagnostic {
                address: address(0),
                reason
            }]
        );
    }
}

#[test]
fn sixteen_bit_targets_need_binding_in_both_default_modes() {
    for (code, bitness) in [
        (&[0xe9, 0, 0][..], DefaultBitness::Bits16),
        (&[0xeb, 0xfe][..], DefaultBitness::Bits16),
        (&[0x66, 0xe9, 0, 0][..], DefaultBitness::Bits32),
        (&[0x66, 0x75, 0, 0xc3][..], DefaultBitness::Bits32),
        (&[0x66, 0xe2, 0, 0xc3][..], DefaultBitness::Bits32),
    ] {
        let image = image(code, bitness);
        let graph = analyze(&image);
        assert!(unresolved(
            &graph,
            0,
            EdgeKind::Branch,
            UnresolvedReason::BindingDependent16BitTarget
        ));
    }
}

#[test]
fn code16_operand_override_near32_is_object_relative() {
    for code in [
        &[0x66, 0xe9, 0, 0, 0, 0, 0xc3][..],
        &[0x66, 0xeb, 0, 0xc3][..],
        &[0x66, 0xe2, 0, 0xc3][..],
    ] {
        let image = image(code, DefaultBitness::Bits16);
        let graph = analyze(&image);
        assert_eq!(graph.nodes().len(), 2);
        assert!(has_edge(
            &graph,
            0,
            EdgeKind::Branch,
            EdgeTarget::Address(address(code.len() as u32 - 1))
        ));
    }
}

#[test]
fn code16_does_not_invent_ip_wrap_at_the_64k_boundary() {
    let mut code = vec![0; 0x1_0002];
    code[0xffff] = 0x90;
    let mut image = image(&code, DefaultBitness::Bits16);
    image.header.entry.as_mut().unwrap().offset = 0xffff;
    let graph = analyze(&image);
    assert_eq!(offsets(&graph), [0xffff]);
    assert!(unresolved(
        &graph,
        0xffff,
        EdgeKind::Fallthrough,
        UnresolvedReason::BindingDependent16BitFallthrough
    ));
    image.objects[0].bytes[0xffff] = 0xb8;
    let graph = analyze(&image);
    assert!(graph.nodes().is_empty());
    assert_eq!(graph.diagnostics()[0].reason, StopReason::Code16Boundary);
    image.header.entry.as_mut().unwrap().offset = 0x1_0000;
    assert_eq!(
        analyze(&image).diagnostics()[0].reason,
        StopReason::Code16Boundary
    );
}

#[test]
fn code16_near_wrap_remains_binding_dependent_instead_of_assuming_zero_origin() {
    let mut code = vec![0; 0x1_0000];
    code[0xfffd..].copy_from_slice(&[0xe9, 1, 0]);
    let mut image = image(&code, DefaultBitness::Bits16);
    image.header.entry.as_mut().unwrap().offset = 0xfffd;
    let graph = analyze(&image);
    assert_eq!(offsets(&graph), [0xfffd]);
    assert!(unresolved(
        &graph,
        0xfffd,
        EdgeKind::Branch,
        UnresolvedReason::BindingDependent16BitTarget
    ));
}

#[test]
fn outside_targets_are_not_resolved_through_preferred_bases() {
    let mut image = image(&[0xe9, 0xff, 0xff, 0xff, 0xff], DefaultBitness::Bits32);
    // The interior target is proven reachable, but FF alone is truncated.
    let graph = analyze(&image);
    assert_eq!(offsets(&graph), [0]);
    assert_eq!(graph.diagnostics()[0].address, address(4));
    assert_eq!(
        graph.diagnostics()[0].reason,
        StopReason::TruncatedInstruction
    );
    assert!(graph.overlaps().is_empty());
    image.objects[0].bytes[1..].copy_from_slice(&(-6_i32).to_le_bytes());
    let graph = analyze(&image);
    assert!(unresolved(
        &graph,
        0,
        EdgeKind::Branch,
        UnresolvedReason::RelativeTargetOutsideObject { offset: -1 }
    ));
    image.objects[0].bytes[1..].copy_from_slice(&100_i32.to_le_bytes());
    let graph = analyze(&image);
    assert!(unresolved(
        &graph,
        0,
        EdgeKind::Branch,
        UnresolvedReason::RelativeTargetOutsideObject { offset: 105 }
    ));
}

#[test]
fn strict_policy_rejects_only_verified_overlapping_instructions() {
    let image = image(
        &[0x66, 0xa9, 0x89, 0xcf, 0xeb, 0xfc],
        DefaultBitness::Bits32,
    );
    assert_eq!(
        StaticCfg::analyze_with_overlap_policy(
            &image,
            &[],
            CfgLimits::default(),
            OverlapPolicy::Reject,
        )
        .unwrap_err(),
        CfgError::OverlappingInstructions {
            instruction: address(2),
            other: address(0),
            byte: address(2)
        }
    );
}

#[test]
fn trusted_queued_interior_entry_decodes_independently() {
    let image = image(&[0x66, 0xa9, 0x89, 0xcf, 0xc3], DefaultBitness::Bits32);
    let graph = StaticCfg::analyze(&image, &[address(2)], CfgLimits::default()).unwrap();
    assert_eq!(offsets(&graph), [0, 2, 4]);
    assert_eq!(
        graph.overlaps(),
        [InstructionOverlap {
            first: address(0),
            second: address(2),
            start: address(2),
            byte_len: 2,
        }]
    );
}

#[test]
fn test_immediate_and_mov_streams_preserve_both_paths_and_immutable_bytes() {
    // JNZ 5; DEC EDI; TEST AX, CF89h; RET. The taken path executes MOV EDI, ECX
    // at byte 5, sharing the TEST immediate, and reaches the same RET at byte 7.
    let code = [0x75, 3, 0x4f, 0x66, 0xa9, 0x89, 0xcf, 0xc3];
    let image = image(&code, DefaultBitness::Bits32);
    let graph = analyze(&image);
    assert_eq!(offsets(&graph), [0, 2, 3, 5, 7]);
    assert_eq!(
        graph
            .nodes()
            .iter()
            .map(|node| node.byte_len)
            .collect::<Vec<_>>(),
        [2, 1, 4, 2, 1]
    );
    for (source, kind, target) in [
        (0, EdgeKind::Branch, 5),
        (0, EdgeKind::Fallthrough, 2),
        (2, EdgeKind::Fallthrough, 3),
        (3, EdgeKind::Fallthrough, 7),
        (5, EdgeKind::Fallthrough, 7),
    ] {
        assert!(has_edge(
            &graph,
            source,
            kind,
            EdgeTarget::Address(address(target))
        ));
    }
    assert_eq!(graph.edges().len(), 6);
    assert_eq!(
        graph.overlaps(),
        [InstructionOverlap {
            first: address(3),
            second: address(5),
            start: address(5),
            byte_len: 2,
        }]
    );
    assert!(graph.diagnostics().is_empty());
    assert!(core::ptr::eq(graph.image(), &image));
    assert_eq!(graph.image().objects()[0].bytes(), code);
}

#[test]
fn overlapping_streams_share_tails_cycles_and_exact_starts_without_redecoding() {
    let image = image(
        &[
            0x75, 3, 0x4f, 0x66, 0xa9, 0x89, 0xcf, 0x75, 0xf7, 0xeb, 0xfe,
        ],
        DefaultBitness::Bits32,
    );
    let graph = StaticCfg::analyze(
        &image,
        &[address(0), address(3), address(5), address(5)],
        CfgLimits {
            max_instructions: 6,
            ..CfgLimits::default()
        },
    )
    .unwrap();
    assert_eq!(offsets(&graph), [0, 2, 3, 5, 7, 9]);
    assert_eq!(graph.overlaps().len(), 1);
    assert_eq!(graph.edges().len(), 8);
    for (source, target) in [(7, 0), (9, 9)] {
        assert!(has_edge(
            &graph,
            source,
            EdgeKind::Branch,
            EdgeTarget::Address(address(target))
        ));
    }
}

#[test]
fn all_pairs_in_a_maximum_length_overlap_chain_are_order_independent() {
    // Fifteen valid starts, all ending after the same NOP. This also exercises
    // the earliest possible intersecting start, fourteen bytes before a node.
    let mut code = [0x66; 16];
    code[14] = 0x90;
    code[15] = 0xc3;
    let image = image(&code, DefaultBitness::Bits32);
    let entries = (1..15).map(address).collect::<Vec<_>>();
    let forward = StaticCfg::analyze(&image, &entries, CfgLimits::default()).unwrap();
    let mut builder = Builder::new(
        &image,
        &entries,
        CfgLimits::default(),
        OverlapPolicy::Record,
    )
    .unwrap();
    // Prequeue the same trusted roots in the opposite DFS order. build() sees
    // them already queued, so the higher starts are now decoded first.
    for offset in 0..15 {
        builder.schedule(address(offset)).unwrap();
    }
    let reverse = builder.build().unwrap();
    assert_eq!(forward.nodes().len(), 16);
    assert_eq!(forward.overlaps().len(), 105);
    assert_eq!(forward.entries(), reverse.entries());
    assert_eq!(forward.nodes(), reverse.nodes());
    assert_eq!(forward.edges(), reverse.edges());
    assert_eq!(forward.diagnostics(), reverse.diagnostics());
    assert_eq!(forward.overlaps(), reverse.overlaps());
    for overlap in forward.overlaps() {
        assert!(overlap.first < overlap.second);
        assert_eq!(overlap.start, overlap.second);
        assert_eq!(u32::from(overlap.byte_len), 15 - overlap.second.offset);
    }
}

#[test]
fn crossing_instruction_ranges_record_exact_intersections_in_both_decode_orders() {
    // MOV-immediate instructions at 0, 2 and 4 end at distinct RETs (5, 7, 9).
    // Some RETs are also interior starts in the other MOV streams.
    let image = image(
        &[0xb8, 0x90, 0xb8, 0x90, 0xb8, 0xc3, 0x90, 0xc3, 0x90, 0xc3],
        DefaultBitness::Bits32,
    );
    let entries = [address(2), address(4)];
    let forward = StaticCfg::analyze(&image, &entries, CfgLimits::default()).unwrap();
    let mut builder = Builder::new(
        &image,
        &entries,
        CfgLimits::default(),
        OverlapPolicy::Record,
    )
    .unwrap();
    for offset in [0, 2, 4] {
        builder.schedule(address(offset)).unwrap();
    }
    let reverse = builder.build().unwrap();
    assert_eq!(offsets(&forward), [0, 2, 4, 5, 7, 9]);
    assert_eq!(forward.nodes(), reverse.nodes());
    assert_eq!(forward.edges(), reverse.edges());
    assert_eq!(forward.overlaps(), reverse.overlaps());
    let expected = [
        (0, 2, 3),
        (0, 4, 1),
        (2, 4, 3),
        (2, 5, 1),
        (4, 5, 1),
        (4, 7, 1),
    ]
    .map(|(first, second, byte_len)| InstructionOverlap {
        first: address(first),
        second: address(second),
        start: address(second),
        byte_len,
    });
    assert_eq!(forward.overlaps(), expected);
}

#[test]
fn adjacent_instructions_do_not_overlap_and_interior_bytes_are_not_roots() {
    let image = image(
        &[0xb8, 0x90, 0x90, 0x90, 0x90, 0xc3],
        DefaultBitness::Bits32,
    );
    let graph = StaticCfg::analyze(
        &image,
        &[address(5)],
        CfgLimits {
            max_overlaps: 0,
            ..CfgLimits::default()
        },
    )
    .unwrap();
    assert_eq!(offsets(&graph), [0, 5]);
    assert!(graph.overlaps().is_empty());
}

#[test]
fn invalid_or_truncated_interior_entries_do_not_claim_overlaps_or_erase_nodes() {
    for (code, reason) in [
        (
            &[0xb8, 0xf0, 0x90, 0, 0, 0xc3][..],
            StopReason::InvalidInstruction,
        ),
        (&[0xb8, 0xb8, 0, 0, 0][..], StopReason::TruncatedInstruction),
    ] {
        let image = image(code, DefaultBitness::Bits32);
        for policy in [OverlapPolicy::Record, OverlapPolicy::Reject] {
            for interior_first in [false, true] {
                let mut builder =
                    Builder::new(&image, &[address(1)], CfgLimits::default(), policy).unwrap();
                if interior_first {
                    builder.schedule(address(0)).unwrap();
                    builder.schedule(address(1)).unwrap();
                }
                let graph = builder.build().unwrap();
                assert_eq!(graph.nodes()[0].address, address(0));
                assert_eq!(graph.nodes()[0].byte_len, 5);
                assert!(graph.overlaps().is_empty());
                assert_eq!(
                    graph.diagnostics(),
                    [CfgDiagnostic {
                        address: address(1),
                        reason
                    }]
                );
            }
        }
    }
}

#[test]
fn relocated_interior_encoding_remains_a_stop_in_both_overlap_policies() {
    let mut image = image(&[0xb8, 0xb8, 0, 0, 0, 0xc3], DefaultBitness::Bits32);
    add_fixup(&mut image, 1, LeFixupSourceType::Offset32, 0);
    for policy in [OverlapPolicy::Record, OverlapPolicy::Reject] {
        let graph = StaticCfg::analyze_with_overlap_policy(
            &image,
            &[address(1)],
            CfgLimits::default(),
            policy,
        )
        .unwrap();
        assert_eq!(offsets(&graph), [0, 5]);
        assert_eq!(
            graph.diagnostics(),
            [CfgDiagnostic {
                address: address(1),
                reason: StopReason::RelocatedEncoding
            }]
        );
        assert!(graph.overlaps().is_empty());
    }
}

#[test]
fn overlap_pair_limit_fails_instead_of_returning_an_incomplete_graph() {
    let image = image(&[0x66, 0x66, 0x90, 0xc3], DefaultBitness::Bits32);
    for max_overlaps in [0, 1, 2] {
        assert_eq!(
            StaticCfg::analyze(
                &image,
                &[address(1), address(2)],
                CfgLimits {
                    max_overlaps,
                    ..CfgLimits::default()
                }
            )
            .unwrap_err(),
            CfgError::Limit(CfgResource::Overlaps)
        );
    }
    let graph = StaticCfg::analyze(
        &image,
        &[address(1), address(2)],
        CfgLimits {
            max_overlaps: 3,
            ..CfgLimits::default()
        },
    )
    .unwrap();
    assert_eq!(graph.overlaps().len(), 3);
}

#[test]
fn overlap_storage_is_charged_to_the_owned_byte_budget() {
    let image = image(&[0x66, 0xa9, 0x89, 0xcf, 0xc3], DefaultBitness::Bits32);
    let storage_before_overlap = 2 * size_of::<ObjectAddress>()
        + size_of::<ObjectState>()
        + image.objects[0].bytes().len() * size_of::<u32>()
        + 2 * size_of::<ObjectAddress>()
        + 3 * size_of::<InstructionNode>()
        + 3 * size_of::<CfgEdge>();
    let limits = CfgLimits {
        max_instructions: 3,
        max_edges: 3,
        max_worklist: 2,
        max_overlaps: 1,
        max_owned_bytes: storage_before_overlap,
    };
    assert_eq!(
        StaticCfg::analyze(&image, &[address(2)], limits).unwrap_err(),
        CfgError::OwnedByteLimit(CfgStorage::Overlaps)
    );
    let graph = StaticCfg::analyze(
        &image,
        &[address(2)],
        CfgLimits {
            max_owned_bytes: storage_before_overlap + size_of::<InstructionOverlap>(),
            ..limits
        },
    )
    .unwrap();
    assert_eq!(graph.overlaps().len(), 1);
}

#[test]
fn extra_entries_are_explicit_sorted_deduplicated_and_checked() {
    let mut image = image(&[0xc3, 0xc3], DefaultBitness::Bits32);
    let graph = StaticCfg::analyze(
        &image,
        &[address(1), address(0), address(1)],
        CfgLimits::default(),
    )
    .unwrap();
    assert_eq!(graph.entries(), [address(0), address(1)]);
    assert_eq!(offsets(&graph), [0, 1]);
    image.header.entry = None;
    assert_eq!(
        StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap_err(),
        CfgError::MissingEntry
    );
    let graph = StaticCfg::analyze(
        &image,
        &[
            ObjectAddress {
                object_number: 0,
                offset: 0,
            },
            address(2),
        ],
        CfgLimits::default(),
    )
    .unwrap();
    assert_eq!(
        graph
            .diagnostics()
            .iter()
            .map(|diagnostic| diagnostic.reason)
            .collect::<Vec<_>>(),
        [StopReason::MissingObject, StopReason::OutsideObject]
    );
    image.objects[0].metadata.flags = 0x2000;
    assert_eq!(
        StaticCfg::analyze(&image, &[address(0)], CfgLimits::default())
            .unwrap()
            .diagnostics()[0]
            .reason,
        StopReason::NotExecutable
    );
}

#[test]
fn relocated_constants_allow_layout_but_never_seed_code() {
    let mut image = image(&[0xb8, 1, 0, 0, 0, 0xc3, 0xc3], DefaultBitness::Bits32);
    add_fixup(&mut image, 1, LeFixupSourceType::Offset32, 6);
    let graph = analyze(&image);
    assert_eq!(offsets(&graph), [0, 5]);
    assert!(graph.diagnostics().is_empty());
}

#[test]
fn relocated_relative_control_target_is_an_exit_with_call_continuation() {
    let mut image = image(&[0xe8, 1, 0, 0, 0, 0xc3, 0xc3], DefaultBitness::Bits32);
    add_fixup(&mut image, 1, LeFixupSourceType::SelfRelative32, 6);
    let graph = analyze(&image);
    assert_eq!(offsets(&graph), [0, 5]);
    assert!(unresolved(
        &graph,
        0,
        EdgeKind::Call,
        UnresolvedReason::RelocatedControlTarget
    ));
}

#[test]
fn relocation_affecting_encoding_or_crossing_instruction_stops() {
    let mut image = image(&[0x90, 0xc3, 0xc3, 0xc3], DefaultBitness::Bits32);
    add_fixup(&mut image, 0, LeFixupSourceType::Byte, 0);
    let graph = analyze(&image);
    assert!(graph.nodes().is_empty());
    assert_eq!(graph.diagnostics()[0].reason, StopReason::RelocatedEncoding);
    image.sites[0].kind = LeFixupSourceType::Offset32;
    assert_eq!(
        analyze(&image).diagnostics()[0].reason,
        StopReason::RelocatedEncoding
    );
}

#[test]
fn every_limit_is_explicit_and_never_returns_partial_success() {
    let image = image(&[0x75, 1, 0xc3, 0xc3], DefaultBitness::Bits32);
    for (limits, resource) in [
        (
            CfgLimits {
                max_instructions: 1,
                ..CfgLimits::default()
            },
            CfgResource::Instructions,
        ),
        (
            CfgLimits {
                max_edges: 1,
                ..CfgLimits::default()
            },
            CfgResource::Edges,
        ),
        (
            CfgLimits {
                max_worklist: 1,
                ..CfgLimits::default()
            },
            CfgResource::Worklist,
        ),
    ] {
        assert_eq!(
            StaticCfg::analyze(&image, &[], limits).unwrap_err(),
            CfgError::Limit(resource)
        );
    }
    assert_eq!(
        StaticCfg::analyze(
            &image,
            &[],
            CfgLimits {
                max_owned_bytes: 0,
                ..CfgLimits::default()
            }
        )
        .unwrap_err(),
        CfgError::OwnedByteLimit(CfgStorage::Entries)
    );
    let entries_storage = size_of::<ObjectAddress>();
    let object_storage = size_of::<ObjectState>();
    assert_eq!(
        StaticCfg::analyze(
            &image,
            &[],
            CfgLimits {
                max_owned_bytes: entries_storage + object_storage,
                ..CfgLimits::default()
            }
        )
        .unwrap_err(),
        CfgError::OwnedByteLimit(CfgStorage::ByteStates)
    );
}

#[test]
fn malformed_instruction_attempts_are_bounded_too() {
    let image = image(&[0xb8, 0xc3], DefaultBitness::Bits32);
    assert_eq!(
        StaticCfg::analyze(
            &image,
            &[address(1)],
            CfgLimits {
                max_instructions: 1,
                ..CfgLimits::default()
            }
        )
        .unwrap_err(),
        CfgError::Limit(CfgResource::Instructions)
    );
}
