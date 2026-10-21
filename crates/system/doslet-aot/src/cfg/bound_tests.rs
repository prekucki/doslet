use super::*;
use crate::{
    FileBackedSpan, RelocationTarget, Site, SymbolicObject,
    binding::{BindingLimits, DosFlatObjectBinding, DosFlatSelector},
};
use alloc::{vec, vec::Vec};
use doslet_executable::{LeHeader, LeLocation, LeObject, LeObjectOffset};

fn at(object_number: u32, offset: u32) -> ObjectAddress {
    ObjectAddress {
        object_number,
        offset,
    }
}

fn image(objects: &[(&[u8], bool)]) -> SymbolicImage {
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
            page_count: 0,
            page_size: 4096,
            last_page_bytes: 0,
            object_count: objects.len() as u32,
            entry: Some(LeObjectOffset {
                object_number: 1,
                offset: 0,
            }),
            stack: None,
            auto_data_object: None,
            data_pages_offset: 0,
        },
        objects: objects
            .iter()
            .enumerate()
            .map(|(index, &(bytes, executable))| SymbolicObject {
                object_number: index as u32 + 1,
                metadata: LeObject {
                    virtual_size: bytes.len() as u32,
                    // Deliberately unrelated to every declared load base.
                    preferred_base: 0x7654_0000,
                    flags: 0x2003 | if executable { 4 } else { 0 },
                    first_page_number: 0,
                    page_count: 0,
                },
                bytes: bytes.to_vec(),
                file_backed_spans: vec![FileBackedSpan {
                    object_offset: 0,
                    file_offset: 123,
                    byte_len: bytes.len() as u32,
                }],
            })
            .collect(),
        sites: Vec::new(),
        provenance: Vec::new(),
    }
}

fn fixup(
    image: &mut SymbolicImage,
    source: ObjectAddress,
    kind: LeFixupSourceType,
    target: ObjectAddress,
) {
    image.sites.push(Site {
        source,
        kind,
        target: RelocationTarget {
            object_number: target.object_number,
            offset: (kind != LeFixupSourceType::Selector16).then_some(target.offset),
        },
        is_alias: false,
        provenance: 0..0,
    });
    image.sites.sort_unstable_by_key(|site| site.source);
}

fn declarations(image: &SymbolicImage, bases: &[u32]) -> Vec<DosFlatObjectBinding> {
    image
        .objects()
        .iter()
        .zip(bases)
        .map(|(object, &base)| {
            let bitness = object.default_bitness();
            DosFlatObjectBinding {
                object_number: object.object_number(),
                linear_base: base,
                normal: Some(DosFlatSelector {
                    value: 0x28 + object.object_number() as u16 * 8,
                    base: if bitness == DefaultBitness::Bits32 {
                        0
                    } else {
                        base
                    },
                    limit: if bitness == DefaultBitness::Bits32 {
                        u32::MAX
                    } else {
                        0xffff
                    },
                    default_bitness: bitness,
                }),
                alias: None,
            }
        })
        .collect()
}

fn binding<'a>(image: &'a SymbolicImage, bases: &[u32]) -> DosFlatBindingPlan<'a> {
    DosFlatBindingPlan::new(image, &declarations(image, bases), BindingLimits::default()).unwrap()
}

fn analyze<'a>(binding: &'a DosFlatBindingPlan<'a>) -> StaticCfg<'a> {
    StaticCfg::analyze_bound(binding, &[], CfgLimits::default()).unwrap()
}

fn addresses(graph: &StaticCfg<'_>) -> Vec<ObjectAddress> {
    graph.nodes().iter().map(|node| node.address).collect()
}

fn edge(graph: &StaticCfg<'_>, source: ObjectAddress, kind: EdgeKind, target: EdgeTarget) -> bool {
    graph.edges().contains(&CfgEdge {
        source,
        kind,
        target,
    })
}

#[test]
fn address_only_objects_cannot_establish_independent_bound_roots() {
    let image = image(&[(&[0xc3], true), (&[0xc3], true)]);
    let mut supplied = declarations(&image, &[0x1000, 0x2000]);
    supplied[0].normal = None;
    supplied[1].normal = None;
    let bound = DosFlatBindingPlan::new(&image, &supplied, BindingLimits::default()).unwrap();
    let graph = StaticCfg::analyze_bound(&bound, &[at(2, 0)], CfgLimits::default()).unwrap();
    assert!(graph.nodes().is_empty());
    assert_eq!(
        graph.diagnostics(),
        [
            CfgDiagnostic {
                address: at(1, 0),
                reason: StopReason::UnsupportedCodeContext,
            },
            CfgDiagnostic {
                address: at(2, 0),
                reason: StopReason::UnsupportedCodeContext,
            },
        ]
    );
}

#[test]
fn near_transfer_retains_flat_source_cs_for_an_address_only_destination() {
    let image = image(&[(&[0xe9, 0, 0, 0, 0], true), (&[0xc3], false)]);
    let mut supplied = declarations(&image, &[0x1000, 0x1005]);
    supplied[1].normal = None;
    let bound = DosFlatBindingPlan::new(&image, &supplied, BindingLimits::default()).unwrap();
    let graph = analyze(&bound);
    assert_eq!(addresses(&graph), [at(1, 0), at(2, 0)]);
    assert!(edge(
        &graph,
        at(1, 0),
        EdgeKind::Branch,
        EdgeTarget::Address(at(2, 0))
    ));
    assert!(graph.diagnostics().is_empty());
}

#[test]
fn relocated_call_reaches_data32_under_source_cs_and_keeps_continuation() {
    let mut image = image(&[
        (
            &[0xe8, 0, 0, 0, 0, 0xc3, 0xe8, 0, 0, 0, 0, 0xc3, 0, 0, 0, 0],
            true,
        ),
        (&[0xc3], false),
        (&[0x40, 0xc3, 0x41, 0xc3], false),
    ]);
    fixup(
        &mut image,
        at(1, 1),
        LeFixupSourceType::SelfRelative32,
        at(3, 0),
    );
    // Neither unreachable CALL fixups nor ordinary relocated pointers add roots.
    fixup(
        &mut image,
        at(1, 7),
        LeFixupSourceType::SelfRelative32,
        at(3, 2),
    );
    fixup(&mut image, at(1, 12), LeFixupSourceType::Offset32, at(3, 2));
    let mut supplied = declarations(&image, &[0x9000, 0x7000, 0x1000]);
    // Destination data selector is deliberately not a full-flat execution view.
    supplied[2].normal.as_mut().unwrap().limit = 0x1003;
    let bound = DosFlatBindingPlan::new(&image, &supplied, BindingLimits::default()).unwrap();
    let graph = analyze(&bound);
    assert_eq!(addresses(&graph), [at(1, 0), at(1, 5), at(3, 0), at(3, 1)]);
    assert!(edge(
        &graph,
        at(1, 0),
        EdgeKind::Call,
        EdgeTarget::Address(at(3, 0))
    ));
    assert!(edge(
        &graph,
        at(1, 0),
        EdgeKind::CallContinuation,
        EdgeTarget::Address(at(1, 5))
    ));
    assert!(graph.diagnostics().is_empty());
    assert_eq!(image.object(1).unwrap().bytes()[1..5], [0, 0, 0, 0]);
    let old = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    assert_eq!(addresses(&old), [at(1, 0), at(1, 5)]);
    assert!(edge(
        &old,
        at(1, 0),
        EdgeKind::Call,
        EdgeTarget::Unresolved(UnresolvedReason::RelocatedControlTarget)
    ));
}

#[test]
fn bound_identity_cannot_silently_accept_another_layout_or_plan() {
    let image = image(&[(&[0xc3], true)]);
    let first = binding(&image, &[0x1000]);
    let equal = binding(&image, &[0x1000]);
    let changed = binding(&image, &[0x2000]);
    let graph = analyze(&first);
    assert!(core::ptr::eq(graph.binding().unwrap(), &first));
    assert!(graph.matches_binding(&first));
    assert!(!graph.matches_binding(&equal));
    assert!(!graph.matches_binding(&changed));
    let unbound = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    assert!(unbound.binding().is_none());
    assert!(unbound.matches_binding(&changed));
    let other_image = self::image(&[(&[0xc3], true)]);
    let other = binding(&other_image, &[0x1000]);
    assert!(!graph.matches_binding(&other));
    assert!(!unbound.matches_binding(&other));
}

#[test]
fn actual_unrelocated_near_targets_use_declared_mapping_including_wrap() {
    // At FFFFFFF0, the five-byte JMP +11 wraps to another object's linear zero.
    let image = image(&[(&[0xe9, 11, 0, 0, 0], true), (&[0xc3], false)]);
    let bound = binding(&image, &[0xffff_fff0, 0]);
    let graph = analyze(&bound);
    assert_eq!(addresses(&graph), [at(1, 0), at(2, 0)]);
    assert!(edge(
        &graph,
        at(1, 0),
        EdgeKind::Branch,
        EdgeTarget::Address(at(2, 0))
    ));
}

#[test]
fn conditional_branch_and_fallthrough_can_reach_adjacent_objects() {
    let image = image(&[(&[0x75, 1], true), (&[0x90, 0xc3], false)]);
    let bound = binding(&image, &[0x1000, 0x1002]);
    let graph = analyze(&bound);
    assert_eq!(addresses(&graph), [at(1, 0), at(2, 0), at(2, 1)]);
    assert!(edge(
        &graph,
        at(1, 0),
        EdgeKind::Branch,
        EdgeTarget::Address(at(2, 1))
    ));
    assert!(edge(
        &graph,
        at(1, 0),
        EdgeKind::Fallthrough,
        EdgeTarget::Address(at(2, 0))
    ));
}

#[test]
fn relocated_near_jump_and_conditional_branch_follow_actual_patched_fields() {
    for code in [
        &[0xe9, 0, 0, 0, 0, 0xc3][..],
        &[0x0f, 0x85, 0, 0, 0, 0, 0xc3][..],
    ] {
        let mut image = image(&[(code, true), (&[0xc3], false)]);
        let source = if code[0] == 0xe9 { 1 } else { 2 };
        fixup(
            &mut image,
            at(1, source),
            LeFixupSourceType::SelfRelative32,
            at(2, 0),
        );
        let bound = binding(&image, &[0x1000, 0x9000]);
        let graph = analyze(&bound);
        assert!(edge(
            &graph,
            at(1, 0),
            EdgeKind::Branch,
            EdgeTarget::Address(at(2, 0))
        ));
        assert!(graph.diagnostics().is_empty());
    }
}

#[test]
fn bss_and_unmapped_relocation_targets_remain_explicit_without_sweeping() {
    for (target, expected) in [
        (
            0,
            UnresolvedReason::Unavailable {
                address: at(2, 0),
                reason: StopReason::NotFileBacked,
            },
        ),
        (1, UnresolvedReason::UnmappedLinearTarget { linear: 0x9001 }),
    ] {
        let mut image = image(&[(&[0xe8, 0, 0, 0, 0, 0xc3], true), (&[0xc3], false)]);
        image.objects[1].file_backed_spans.clear();
        fixup(
            &mut image,
            at(1, 1),
            LeFixupSourceType::SelfRelative32,
            at(2, target),
        );
        let bound = binding(&image, &[0x1000, 0x9000]);
        let graph = analyze(&bound);
        assert_eq!(addresses(&graph), [at(1, 0), at(1, 5)]);
        assert!(edge(
            &graph,
            at(1, 0),
            EdgeKind::Call,
            EdgeTarget::Unresolved(expected)
        ));
    }
}

#[test]
fn mixed_bitness_targets_and_non_full_flat_roots_are_refused() {
    let mut image = image(&[(&[0xe8, 0, 0, 0, 0, 0xc3], true), (&[0xc3], false)]);
    image.objects[1].metadata.flags &= !0x2000;
    fixup(
        &mut image,
        at(1, 1),
        LeFixupSourceType::SelfRelative32,
        at(2, 0),
    );
    let bound = binding(&image, &[0x1000, 0x9000]);
    let graph = analyze(&bound);
    assert!(edge(
        &graph,
        at(1, 0),
        EdgeKind::Call,
        EdgeTarget::Unresolved(UnresolvedReason::Unavailable {
            address: at(2, 0),
            reason: StopReason::UnsupportedCodeContext
        })
    ));
    let mut supplied = declarations(&image, &[0x1000, 0x9000]);
    supplied[0].normal.as_mut().unwrap().limit = 0x1005;
    let limited = DosFlatBindingPlan::new(&image, &supplied, BindingLimits::default()).unwrap();
    let graph = analyze(&limited);
    assert!(graph.nodes().is_empty());
    assert_eq!(
        graph.diagnostics(),
        &[CfgDiagnostic {
            address: at(1, 0),
            reason: StopReason::UnsupportedCodeContext
        }]
    );
    let narrow_image = {
        let mut image = self::image(&[(&[0xc3], true)]);
        image.objects[0].metadata.flags &= !0x2000;
        image
    };
    let narrow = binding(&narrow_image, &[0x9000]);
    assert_eq!(
        analyze(&narrow).diagnostics()[0].reason,
        StopReason::UnsupportedCodeContext
    );
}

#[test]
fn trusted_roots_are_bounded_sorted_and_require_a_source_code_view() {
    let image = image(&[(&[0xc3, 0xc3], true), (&[0xc3], false)]);
    let bound = binding(&image, &[0x1000, 0x9000]);
    let graph = StaticCfg::analyze_bound(
        &bound,
        &[at(1, 1), at(0, 0), at(2, 0), at(1, 2), at(1, 1)],
        CfgLimits::default(),
    )
    .unwrap();
    assert_eq!(addresses(&graph), [at(1, 0), at(1, 1)]);
    assert_eq!(
        graph.diagnostics(),
        &[
            CfgDiagnostic {
                address: at(0, 0),
                reason: StopReason::MissingObject
            },
            CfgDiagnostic {
                address: at(1, 2),
                reason: StopReason::OutsideObject
            },
            CfgDiagnostic {
                address: at(2, 0),
                reason: StopReason::NotExecutable
            },
        ]
    );
    assert_eq!(
        StaticCfg::analyze_bound(
            &bound,
            &[at(1, 0)],
            CfgLimits {
                max_worklist: 1,
                ..CfgLimits::default()
            }
        )
        .unwrap_err(),
        CfgError::Limit(CfgResource::Worklist)
    );
}

#[test]
fn partial_and_incompatible_control_fixups_never_manufacture_trusted_calls() {
    for (source, kind) in [
        (1, LeFixupSourceType::Offset32),
        (1, LeFixupSourceType::Selector16),
        (2, LeFixupSourceType::Selector16),
        (3, LeFixupSourceType::Offset32),
        (0, LeFixupSourceType::Offset32),
    ] {
        let mut image = image(&[(&[0xe8, 0, 0, 0, 0, 0xc3, 0xc3], true)]);
        fixup(&mut image, at(1, source), kind, at(1, 6));
        let bound = binding(&image, &[0x1000]);
        let graph = analyze(&bound);
        assert!(graph.nodes().is_empty(), "{source} {kind:?}");
        assert!(graph.edges().is_empty());
        assert_eq!(graph.diagnostics()[0].reason, StopReason::RelocatedEncoding);
    }
}

#[test]
fn opcode_patch_cannot_turn_original_data_into_a_trusted_call() {
    // Offset32 patch emits E8 00 00 00, which would decode as a CALL with byte4.
    let mut image = image(&[(&[0xb8, 0, 0, 0, 0, 0xc3], true)]);
    fixup(
        &mut image,
        at(1, 0),
        LeFixupSourceType::Offset32,
        at(1, 0xe8),
    );
    let bound = binding(&image, &[0]);
    let graph = analyze(&bound);
    assert!(graph.nodes().is_empty());
    assert_eq!(graph.diagnostics()[0].reason, StopReason::RelocatedEncoding);
}

#[test]
fn complete_noncontrol_fields_are_patched_without_seeding_their_values() {
    let mut image = image(&[(&[0xb8, 0, 0, 0, 0, 0xc3, 0xc3], true)]);
    fixup(&mut image, at(1, 1), LeFixupSourceType::Offset32, at(1, 6));
    let bound = binding(&image, &[0x1000]);
    assert_eq!(addresses(&analyze(&bound)), [at(1, 0), at(1, 5)]);
    image.sites[0].kind = LeFixupSourceType::Selector16;
    let bound = binding(&image, &[0x1000]);
    assert_eq!(
        analyze(&bound).diagnostics()[0].reason,
        StopReason::RelocatedEncoding
    );
}

#[test]
fn unsupported_near16_encoding_remains_explicit_even_with_a_binding() {
    let image = image(&[(&[0x66, 0xe8, 0, 0, 0xc3], true)]);
    let bound = binding(&image, &[0x1000]);
    let graph = analyze(&bound);
    assert_eq!(addresses(&graph), [at(1, 0), at(1, 4)]);
    assert!(edge(
        &graph,
        at(1, 0),
        EdgeKind::Call,
        EdgeTarget::Unresolved(UnresolvedReason::BindingDependent16BitTarget)
    ));
}

#[test]
fn bound_overlap_identity_policy_and_order_remain_deterministic() {
    let image = image(&[(&[0xb8, 0x90, 0xc3, 0, 0, 0xc3], true)]);
    let bound = binding(&image, &[0x1000]);
    let first =
        StaticCfg::analyze_bound(&bound, &[at(1, 1), at(1, 2)], CfgLimits::default()).unwrap();
    let second =
        StaticCfg::analyze_bound(&bound, &[at(1, 2), at(1, 1)], CfgLimits::default()).unwrap();
    assert_eq!(first.nodes(), second.nodes());
    assert_eq!(first.edges(), second.edges());
    assert_eq!(first.overlaps(), second.overlaps());
    assert_eq!(first.diagnostics(), second.diagnostics());
    assert_eq!(first.overlaps().len(), 2);
    assert_eq!(
        StaticCfg::analyze_bound(
            &bound,
            &[at(1, 1)],
            CfgLimits {
                max_overlaps: 0,
                ..CfgLimits::default()
            }
        )
        .unwrap_err(),
        CfgError::Limit(CfgResource::Overlaps)
    );
    assert!(matches!(
        StaticCfg::analyze_bound_with_overlap_policy(
            &bound,
            &[at(1, 1)],
            CfgLimits::default(),
            OverlapPolicy::Reject
        ),
        Err(CfgError::OverlappingInstructions { .. })
    ));
}

#[test]
fn bound_limits_count_decode_attempts_edges_worklist_and_data_byte_states() {
    let image = image(&[(&[0x75, 1, 0xc3, 0xc3], true), (&[0xc3; 100], false)]);
    let bound = binding(&image, &[0x1000, 0x9000]);
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
            StaticCfg::analyze_bound(&bound, &[], limits).unwrap_err(),
            CfgError::Limit(resource)
        );
    }
    let bytes_before_data =
        size_of::<ObjectAddress>() + 2 * size_of::<ObjectState>() + 4 * size_of::<u32>();
    assert_eq!(
        StaticCfg::analyze_bound(
            &bound,
            &[],
            CfgLimits {
                max_owned_bytes: bytes_before_data,
                ..CfgLimits::default()
            }
        )
        .unwrap_err(),
        CfgError::OwnedByteLimit(CfgStorage::ByteStates)
    );
    assert_eq!(
        StaticCfg::analyze_bound(
            &bound,
            &[],
            CfgLimits {
                max_owned_bytes: bytes_before_data + 100 * size_of::<u32>(),
                ..CfgLimits::default()
            }
        )
        .unwrap_err(),
        CfgError::OwnedByteLimit(CfgStorage::BindingIndex)
    );
    let invalid = self::image(&[(&[0xb8, 0xc3], true)]);
    let bound = binding(&invalid, &[0x1000]);
    assert_eq!(
        StaticCfg::analyze_bound(
            &bound,
            &[at(1, 1)],
            CfgLimits {
                max_instructions: 1,
                ..CfgLimits::default()
            }
        )
        .unwrap_err(),
        CfgError::Limit(CfgResource::Instructions)
    );
}

#[test]
fn bound_windows_stop_at_holes_and_do_not_cross_object_instruction_boundaries() {
    let mut image = image(&[(&[0xb8, 0, 0, 0, 0, 0xc3], true)]);
    image.objects[0].file_backed_spans[0].byte_len = 2;
    let bound = binding(&image, &[0x1000]);
    let graph = analyze(&bound);
    assert!(graph.nodes().is_empty());
    assert_eq!(
        graph.diagnostics()[0].reason,
        StopReason::TruncatedInstruction
    );
    let image = self::image(&[(&[0xb8], true), (&[0, 0, 0, 0, 0xc3], false)]);
    let bound = binding(&image, &[0x1000, 0x1001]);
    assert_eq!(
        analyze(&bound).diagnostics()[0].reason,
        StopReason::TruncatedInstruction
    );
}
