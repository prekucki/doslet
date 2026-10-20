use super::*;
use crate::{FileBackedSpan, RelocationTarget, Site, SymbolicObject};
use alloc::{vec, vec::Vec};
use doslet_executable::{LeHeader, LeLocation, LeObject};

fn address(object_number: u32, offset: u32) -> ObjectAddress {
    ObjectAddress {
        object_number,
        offset,
    }
}

// Synthetic SymbolicImage fixtures satisfy the reconstruction layer's source
// width, canonical order and valid object-ID invariants. Parser coverage lives
// in the image/parser tests; these tests isolate explicit layout policy.
fn image(objects: &[(u32, DefaultBitness)]) -> SymbolicImage {
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
            page_count: 0,
            page_size: 4096,
            last_page_bytes: 0,
            object_count: objects.len() as u32,
            entry: None,
            stack: None,
            auto_data_object: None,
            data_pages_offset: 0,
        },
        objects: objects
            .iter()
            .enumerate()
            .map(|(index, &(size, bitness))| SymbolicObject {
                object_number: index as u32 + 1,
                metadata: LeObject {
                    virtual_size: size,
                    preferred_base: 0x7654_0000,
                    flags: 7 | if bitness == DefaultBitness::Bits32 {
                        0x2000
                    } else {
                        0
                    },
                    first_page_number: 0,
                    page_count: 0,
                },
                bytes: vec![0xa5; size as usize],
                file_backed_spans: if size == 0 {
                    vec![]
                } else {
                    vec![FileBackedSpan {
                        object_offset: 0,
                        file_offset: 0,
                        byte_len: size,
                    }]
                },
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
    alias: bool,
) {
    assert!(
        source.offset + u32::from(kind.width())
            <= image
                .object(source.object_number)
                .unwrap()
                .metadata()
                .virtual_size
    );
    image.sites.push(Site {
        source,
        kind,
        target: RelocationTarget {
            object_number: target.object_number,
            offset: if kind == LeFixupSourceType::Selector16 {
                None
            } else {
                Some(target.offset)
            },
        },
        is_alias: alias,
        provenance: 0..0,
    });
    image.sites.sort_unstable_by_key(|site| site.source);
}

fn flat(value: u16) -> DosFlatSelector {
    DosFlatSelector {
        value,
        base: 0,
        limit: u32::MAX,
        default_bitness: DefaultBitness::Bits32,
    }
}

fn narrow(value: u16, base: u32) -> DosFlatSelector {
    DosFlatSelector {
        value,
        base,
        limit: 0xffff,
        default_bitness: DefaultBitness::Bits16,
    }
}

fn binding(object_number: u32, base: u32) -> DosFlatObjectBinding {
    DosFlatObjectBinding {
        object_number,
        linear_base: base,
        normal: Some(flat(0x28)),
        alias: None,
    }
}

fn plan<'a>(
    image: &'a SymbolicImage,
    bindings: &[DosFlatObjectBinding],
) -> Result<DosFlatBindingPlan<'a>, BindingError> {
    DosFlatBindingPlan::new(image, bindings, BindingLimits::default())
}

#[test]
fn address_only_object_encodes_linear_offsets_without_a_normal_selector() {
    let mut image = image(&[(8, DefaultBitness::Bits32), (8, DefaultBitness::Bits16)]);
    fixup(
        &mut image,
        address(1, 0),
        LeFixupSourceType::Offset32,
        address(2, 5),
        false,
    );
    fixup(
        &mut image,
        address(2, 0),
        LeFixupSourceType::Offset32,
        address(1, 7),
        false,
    );
    let target = DosFlatObjectBinding {
        normal: None,
        ..binding(2, 0x9000)
    };
    let plan = plan(&image, &[binding(1, 0x1000), target]).unwrap();
    assert_eq!(plan.object_binding(2).unwrap().normal, None);
    assert_eq!(plan.patches()[0].bytes(), &0x9005u32.to_le_bytes());
    let mut output = [0; 8];
    plan.encode_object(2, &mut output).unwrap();
    assert_eq!(&output[..4], &0x1007u32.to_le_bytes());
}

#[test]
fn normal_selector_fixup_requires_normal_even_when_alias_is_present() {
    let mut image = image(&[(8, DefaultBitness::Bits32), (8, DefaultBitness::Bits16)]);
    fixup(
        &mut image,
        address(1, 0),
        LeFixupSourceType::Selector16,
        address(2, 0),
        false,
    );
    let target = DosFlatObjectBinding {
        normal: None,
        alias: Some(narrow(0x30, 0x9000)),
        ..binding(2, 0x9000)
    };
    assert_eq!(
        plan(&image, &[binding(1, 0x1000), target]).unwrap_err(),
        BindingError::MissingNormal {
            source: address(1, 0),
            target_object: 2,
        }
    );
    image.sites[0].is_alias = true;
    let alias = plan(&image, &[binding(1, 0x1000), target]).unwrap();
    assert_eq!(alias.patches()[0].bytes(), &0x30u16.to_le_bytes());
    // Alias validation remains mandatory independently of the missing normal.
    let invalid = DosFlatObjectBinding {
        alias: Some(narrow(0x30, 0x8000)),
        ..target
    };
    assert_eq!(
        plan(&image, &[binding(1, 0x1000), invalid]).unwrap_err(),
        BindingError::Selector {
            object_number: 2,
            view: SelectorView::Alias,
            problem: SelectorProblem::Base,
        }
    );
}

#[test]
fn encodes_supported_fields_with_explicit_moved_bases_and_preserves_image() {
    let mut image = image(&[(32, DefaultBitness::Bits32), (16, DefaultBitness::Bits32)]);
    fixup(
        &mut image,
        address(1, 0),
        LeFixupSourceType::Offset32,
        address(2, 3),
        false,
    );
    fixup(
        &mut image,
        address(1, 4),
        LeFixupSourceType::SelfRelative32,
        address(2, 7),
        false,
    );
    fixup(
        &mut image,
        address(1, 8),
        LeFixupSourceType::Selector16,
        address(2, 0),
        false,
    );
    fixup(
        &mut image,
        address(2, 0),
        LeFixupSourceType::Offset32,
        address(1, 5),
        false,
    );
    let originals: Vec<_> = image
        .objects()
        .iter()
        .map(|object| object.bytes().to_vec())
        .collect();
    let first = plan(&image, &[binding(2, 0x2000), binding(1, 0x1000)]).unwrap();
    assert!(core::ptr::eq(first.image(), &image));
    assert_eq!(first.object_binding(1), Some(binding(1, 0x1000)));
    assert_eq!(first.patches().len(), 4);
    assert_eq!(first.object_patches(1).unwrap().len(), 3);
    assert_eq!(first.object_patches(2).unwrap().len(), 1);
    assert_eq!(first.patches()[0].bytes(), &0x2003u32.to_le_bytes());
    assert_eq!(first.patches()[1].bytes(), &0x0fffu32.to_le_bytes());
    assert_eq!(first.patches()[2].bytes(), &0x28u16.to_le_bytes());
    let second = plan(&image, &[binding(1, 0x8000), binding(2, 0xb000)]).unwrap();
    assert_eq!(second.patches()[0].bytes(), &0xb003u32.to_le_bytes());
    assert_eq!(second.patches()[1].bytes(), &0x2fffu32.to_le_bytes());
    let mut output = vec![0; 32];
    first.encode_object(1, &mut output).unwrap();
    assert_eq!(&output[..4], &0x2003u32.to_le_bytes());
    assert_eq!(&output[4..8], &0x0fffu32.to_le_bytes());
    assert_eq!(&output[8..10], &0x28u16.to_le_bytes());
    assert_eq!(&output[10..], &[0xa5; 22]);
    for (object, original) in image.objects().iter().zip(originals) {
        assert_eq!(object.bytes(), original);
        assert_eq!(object.metadata().preferred_base, 0x7654_0000);
    }
}

#[test]
fn relative_fields_allow_negative_displacements_and_modular_target_addends() {
    let mut image = image(&[(32, DefaultBitness::Bits32), (16, DefaultBitness::Bits32)]);
    fixup(
        &mut image,
        address(1, 0),
        LeFixupSourceType::SelfRelative32,
        address(2, 0),
        false,
    );
    fixup(
        &mut image,
        address(1, 4),
        LeFixupSourceType::Offset32,
        address(2, u32::MAX),
        false,
    );
    fixup(
        &mut image,
        address(1, 8),
        LeFixupSourceType::SelfRelative32,
        address(2, u32::MAX),
        false,
    );
    fixup(
        &mut image,
        address(1, 12),
        LeFixupSourceType::Offset32,
        address(2, 16),
        false,
    );
    let plan = plan(&image, &[binding(1, 0x2000), binding(2, 0x1000)]).unwrap();
    let values: Vec<_> = plan
        .patches()
        .iter()
        .map(|patch| u32::from_le_bytes(patch.bytes().try_into().unwrap()))
        .collect();
    assert_eq!(values, [0xffff_effc, 0x0fff, 0xffff_eff3, 0x1010]);
}

#[test]
fn one_past_and_large_targets_are_not_dereferenced_or_range_proven() {
    let mut image = image(&[(16, DefaultBitness::Bits32), (0, DefaultBitness::Bits32)]);
    fixup(
        &mut image,
        address(1, 0),
        LeFixupSourceType::Offset32,
        address(1, 16),
        false,
    );
    fixup(
        &mut image,
        address(1, 4),
        LeFixupSourceType::Offset32,
        address(2, 0xffff_fff0),
        false,
    );
    let plan = plan(&image, &[binding(1, 0xffff_ffdf), binding(2, 0x100)]).unwrap();
    assert_eq!(plan.patches()[0].bytes(), &0xffff_ffefu32.to_le_bytes());
    assert_eq!(plan.patches()[1].bytes(), &0xf0u32.to_le_bytes());
    assert!(plan.object_patches(2).unwrap().is_empty());
    plan.encode_object(2, &mut []).unwrap();
}

#[test]
fn code16_normal_view_has_zero_offset_origin_and_explicit_selector() {
    let mut image = image(&[(16, DefaultBitness::Bits16)]);
    fixup(
        &mut image,
        address(1, 0),
        LeFixupSourceType::Selector16,
        address(1, 0),
        false,
    );
    fixup(
        &mut image,
        address(1, 4),
        LeFixupSourceType::Offset32,
        address(1, 5),
        false,
    );
    let binding = DosFlatObjectBinding {
        normal: Some(narrow(0x30, 0x9000)),
        ..binding(1, 0x9000)
    };
    let plan = plan(&image, &[binding]).unwrap();
    assert_eq!(plan.patches()[0].bytes(), &0x30u16.to_le_bytes());
    assert_eq!(plan.patches()[1].bytes(), &0x9005u32.to_le_bytes());
    assert_eq!(plan.object_binding(1).unwrap().normal.unwrap().base, 0x9000);
}

#[test]
fn code16_self_relative_is_explicitly_unsupported() {
    let mut image = image(&[(16, DefaultBitness::Bits16)]);
    fixup(
        &mut image,
        address(1, 0),
        LeFixupSourceType::SelfRelative32,
        address(1, 0),
        false,
    );
    let binding = DosFlatObjectBinding {
        normal: Some(narrow(0x30, 0x9000)),
        ..binding(1, 0x9000)
    };
    assert_eq!(
        plan(&image, &[binding]).unwrap_err(),
        BindingError::UnsupportedCode16SelfRelative {
            source: address(1, 0)
        }
    );
}

#[test]
fn alias_flag_uses_only_explicit_object_relative_alias() {
    let mut image = image(&[(16, DefaultBitness::Bits32)]);
    fixup(
        &mut image,
        address(1, 0),
        LeFixupSourceType::Selector16,
        address(1, 0),
        true,
    );
    fixup(
        &mut image,
        address(1, 2),
        LeFixupSourceType::Selector16,
        address(1, 0),
        false,
    );
    assert_eq!(
        plan(&image, &[binding(1, 0x1000)]).unwrap_err(),
        BindingError::MissingAlias {
            source: address(1, 0),
            target_object: 1
        }
    );
    let binding = DosFlatObjectBinding {
        alias: Some(narrow(0x33, 0x1000)),
        ..binding(1, 0x1000)
    };
    let plan = plan(&image, &[binding]).unwrap();
    assert_eq!(plan.patches()[0].bytes(), &0x33u16.to_le_bytes());
    assert_eq!(plan.patches()[1].bytes(), &0x28u16.to_le_bytes());
}

#[test]
fn validates_bitness_base_coverage_and_64k_window_for_all_supplied_views() {
    for (bitness, view) in [
        (DefaultBitness::Bits32, SelectorView::Normal),
        (DefaultBitness::Bits16, SelectorView::Normal),
        (DefaultBitness::Bits32, SelectorView::Alias),
    ] {
        let image = image(&[(16, bitness)]);
        let valid = if view == SelectorView::Normal && bitness == DefaultBitness::Bits32 {
            flat(0x30)
        } else {
            narrow(0x30, 0x1000)
        };
        for problem in [
            SelectorProblem::Null,
            SelectorProblem::Bitness,
            SelectorProblem::Base,
            SelectorProblem::Coverage,
        ] {
            let mut invalid = valid;
            match problem {
                SelectorProblem::Null => invalid.value = 3,
                SelectorProblem::Bitness => {
                    invalid.default_bitness = if valid.default_bitness == DefaultBitness::Bits32 {
                        DefaultBitness::Bits16
                    } else {
                        DefaultBitness::Bits32
                    }
                }
                SelectorProblem::Base => invalid.base ^= 0x1000,
                SelectorProblem::Coverage => invalid.limit = 14,
                SelectorProblem::Window16 => unreachable!(),
            }
            let mut supplied = binding(1, 0x1000);
            if view == SelectorView::Alias {
                supplied.alias = Some(invalid);
            } else {
                supplied.normal = Some(invalid);
            }
            assert_eq!(
                plan(&image, &[supplied]).unwrap_err(),
                BindingError::Selector {
                    object_number: 1,
                    view,
                    problem
                }
            );
        }
        if valid.default_bitness == DefaultBitness::Bits16 {
            let mut invalid = valid;
            invalid.limit = 0x1_0000;
            let mut supplied = binding(1, 0x1000);
            if view == SelectorView::Alias {
                supplied.alias = Some(invalid);
            } else {
                supplied.normal = Some(invalid);
            }
            assert_eq!(
                plan(&image, &[supplied]).unwrap_err(),
                BindingError::Selector {
                    object_number: 1,
                    view,
                    problem: SelectorProblem::Window16
                }
            );
        }
    }
    for bitness in [DefaultBitness::Bits16, DefaultBitness::Bits32] {
        let image = image(&[(0x1_0001, bitness)]);
        let mut supplied = binding(1, 0x1000);
        let view = if bitness == DefaultBitness::Bits16 {
            supplied.normal = Some(narrow(0x30, 0x1000));
            SelectorView::Normal
        } else {
            supplied.alias = Some(narrow(0x30, 0x1000));
            SelectorView::Alias
        };
        assert_eq!(
            plan(&image, &[supplied]).unwrap_err(),
            BindingError::Selector {
                object_number: 1,
                view,
                problem: SelectorProblem::Window16
            }
        );
    }
    let image = image(&[(0x1_0000, DefaultBitness::Bits16)]);
    assert!(
        plan(
            &image,
            &[DosFlatObjectBinding {
                normal: Some(narrow(0x30, 0x1000)),
                ..binding(1, 0x1000)
            }]
        )
        .is_ok()
    );
}

#[test]
fn selector_coverage_accepts_inclusive_last_byte_and_shared_flat_views() {
    let image = image(&[(16, DefaultBitness::Bits32), (16, DefaultBitness::Bits32)]);
    let mut bindings = [binding(1, 0x1000), binding(2, 0x2000)];
    for binding in &mut bindings {
        binding.normal.as_mut().unwrap().limit = 0x200f;
    }
    bindings[1].normal.as_mut().unwrap().value |= 3; // Same descriptor, a different RPL.
    assert!(plan(&image, &bindings).is_ok());
    bindings[1].normal.as_mut().unwrap().limit += 1;
    assert_eq!(
        plan(&image, &bindings).unwrap_err(),
        BindingError::InconsistentSelector {
            identity: 0x28,
            first_object: 1,
            second_object: 2
        }
    );
    bindings[1].normal.as_mut().unwrap().value = 0x2c; // TI distinguishes a different descriptor.
    assert!(plan(&image, &bindings).is_ok());
}

#[test]
fn selector_identity_consistency_includes_alias_views() {
    let image = image(&[(16, DefaultBitness::Bits32), (16, DefaultBitness::Bits32)]);
    let bindings = [
        DosFlatObjectBinding {
            alias: Some(narrow(0x31, 0x1000)),
            ..binding(1, 0x1000)
        },
        DosFlatObjectBinding {
            alias: Some(narrow(0x33, 0x2000)),
            ..binding(2, 0x2000)
        },
    ];
    assert_eq!(
        plan(&image, &bindings).unwrap_err(),
        BindingError::InconsistentSelector {
            identity: 0x30,
            first_object: 1,
            second_object: 2
        }
    );
    let binding = DosFlatObjectBinding {
        alias: Some(narrow(0x2b, 0x1000)),
        ..binding(1, 0x1000)
    };
    let single_image = self::image(&[(16, DefaultBitness::Bits32)]);
    assert_eq!(
        plan(&single_image, &[binding]).unwrap_err(),
        BindingError::InconsistentSelector {
            identity: 0x28,
            first_object: 1,
            second_object: 1
        }
    );
}

#[test]
fn rejects_missing_duplicate_and_invalid_object_numbers() {
    let image = image(&[(16, DefaultBitness::Bits32), (16, DefaultBitness::Bits32)]);
    for (bindings, expected) in [
        (vec![], BindingError::MissingObject(1)),
        (vec![binding(2, 0x2000)], BindingError::MissingObject(1)),
        (vec![binding(1, 0x1000)], BindingError::MissingObject(2)),
        (
            vec![binding(1, 0x1000), binding(1, 0x2000)],
            BindingError::DuplicateObject(1),
        ),
        (vec![binding(0, 0x1000)], BindingError::UnknownObject(0)),
        (vec![binding(3, 0x1000)], BindingError::UnknownObject(3)),
        (
            vec![binding(u32::MAX, 0x1000)],
            BindingError::UnknownObject(u32::MAX),
        ),
    ] {
        assert_eq!(plan(&image, &bindings).unwrap_err(), expected);
    }
}

#[test]
fn rejects_loaded_extent_overflow_and_overlaps_but_accepts_adjacency_and_empty_ranges() {
    let image = image(&[
        (16, DefaultBitness::Bits32),
        (16, DefaultBitness::Bits32),
        (0, DefaultBitness::Bits32),
    ]);
    assert_eq!(
        plan(
            &image,
            &[binding(1, u32::MAX - 14), binding(2, 0x1000), binding(3, 0)]
        )
        .unwrap_err(),
        BindingError::ObjectExtentOverflow(1)
    );
    for (first, second) in [(0x1000, 0x1000), (0x1000, 0x100f), (0x100f, 0x1000)] {
        assert!(matches!(
            plan(
                &image,
                &[binding(1, first), binding(2, second), binding(3, first)]
            ),
            Err(BindingError::OverlappingObjects { .. })
        ));
    }
    assert!(
        plan(
            &image,
            &[binding(1, 0x1010), binding(2, 0x1000), binding(3, 0x1008)]
        )
        .is_ok()
    );
}

#[test]
fn highest_linear_byte_and_adjacent_extents_fit_without_wrapping() {
    let single_byte = image(&[(1, DefaultBitness::Bits32)]);
    assert!(plan(&single_byte, &[binding(1, u32::MAX)]).is_ok());

    let mut image = image(&[(16, DefaultBitness::Bits32); 2]);
    fixup(
        &mut image,
        address(1, 12),
        LeFixupSourceType::SelfRelative32,
        address(1, 0),
        false,
    );
    let bindings = [binding(1, u32::MAX - 15), binding(2, u32::MAX - 31)];
    let prepared = plan(&image, &bindings).unwrap();
    // The source field occupies the final four bytes; its following address
    // wraps to zero only in the intentional modular relocation formula.
    let mut output = [0; 16];
    prepared.encode_object(1, &mut output).unwrap();
    assert_eq!(&output[12..], &0xffff_fff0u32.to_le_bytes());

    let mut overlapping = bindings;
    overlapping[1].linear_base += 1;
    assert!(matches!(
        plan(&image, &overlapping),
        Err(BindingError::OverlappingObjects { .. })
    ));
    let mut wrapping = bindings;
    wrapping[0].linear_base += 1;
    assert_eq!(
        plan(&image, &wrapping).unwrap_err(),
        BindingError::ObjectExtentOverflow(1)
    );
}

#[test]
fn invalid_output_or_object_leaves_destination_unchanged() {
    let mut image = image(&[(16, DefaultBitness::Bits32)]);
    fixup(
        &mut image,
        address(1, 0),
        LeFixupSourceType::Offset32,
        address(1, 4),
        false,
    );
    let plan = plan(&image, &[binding(1, 0x1000)]).unwrap();
    for length in [0, 15, 17, 32] {
        let mut output = vec![0x5a; length];
        assert_eq!(
            plan.encode_object(1, &mut output),
            Err(BindingError::OutputLength {
                object_number: 1,
                expected: 16,
                actual: length
            })
        );
        assert_eq!(output, vec![0x5a; length]);
    }
    for number in [0, 2, u32::MAX] {
        let mut output = [0x5a; 16];
        assert_eq!(
            plan.encode_object(number, &mut output),
            Err(BindingError::UnknownObject(number))
        );
        assert_eq!(output, [0x5a; 16]);
        assert!(plan.object_patches(number).is_none());
        assert!(plan.object_binding(number).is_none());
    }
}

#[test]
fn unsupported_kind_after_a_supported_patch_never_publishes_partial_plan() {
    for kind in [
        LeFixupSourceType::Byte,
        LeFixupSourceType::Offset16,
        LeFixupSourceType::Pointer1616,
        LeFixupSourceType::Pointer1632,
    ] {
        let mut image = image(&[(16, DefaultBitness::Bits32)]);
        fixup(
            &mut image,
            address(1, 0),
            LeFixupSourceType::Offset32,
            address(1, 0),
            false,
        );
        fixup(&mut image, address(1, 8), kind, address(1, u32::MAX), false);
        assert_eq!(
            plan(&image, &[binding(1, 0x1000)]).unwrap_err(),
            BindingError::UnsupportedKind {
                source: address(1, 8),
                kind
            }
        );
        assert_eq!(image.object(1).unwrap().bytes(), &[0xa5; 16]);
    }
}

#[test]
fn deterministic_order_and_object_patch_ranges_include_empty_objects() {
    let mut image = image(&[(16, DefaultBitness::Bits32); 4]);
    fixup(
        &mut image,
        address(2, 4),
        LeFixupSourceType::Offset32,
        address(1, 0),
        false,
    );
    fixup(
        &mut image,
        address(2, 0),
        LeFixupSourceType::Offset32,
        address(1, 1),
        false,
    );
    fixup(
        &mut image,
        address(4, 0),
        LeFixupSourceType::Offset32,
        address(1, 2),
        false,
    );
    let bindings = [
        binding(4, 0x4000),
        binding(2, 0x2000),
        binding(3, 0x3000),
        binding(1, 0x1000),
    ];
    let first = plan(&image, &bindings).unwrap();
    let second = plan(&image, &bindings.into_iter().rev().collect::<Vec<_>>()).unwrap();
    assert_eq!(first.patches(), second.patches());
    assert_eq!(
        first
            .patches()
            .iter()
            .map(|patch| patch.source)
            .collect::<Vec<_>>(),
        [address(2, 0), address(2, 4), address(4, 0)]
    );
    assert_eq!(
        (1..=4)
            .map(|number| first.object_patches(number).unwrap().len())
            .collect::<Vec<_>>(),
        [0, 2, 0, 1]
    );
}

#[test]
fn patch_count_and_every_owned_storage_reservation_are_bounded() {
    let mut image = image(&[(16, DefaultBitness::Bits32)]);
    fixup(
        &mut image,
        address(1, 0),
        LeFixupSourceType::Offset32,
        address(1, 0),
        false,
    );
    let bindings = [binding(1, 0x1000)];
    assert_eq!(
        DosFlatBindingPlan::new(
            &image,
            &bindings,
            BindingLimits {
                max_patches: 0,
                ..BindingLimits::default()
            }
        )
        .unwrap_err(),
        BindingError::PatchLimit
    );
    let objects = size_of::<BoundObject>();
    let ranges = objects + size_of::<LoadedRange>();
    let selectors = ranges + size_of::<SelectorRecord>();
    for (bytes, storage) in [
        (0, BindingStorage::Objects),
        (objects, BindingStorage::RangeIndex),
        (ranges, BindingStorage::SelectorIndex),
        (selectors, BindingStorage::Patches),
    ] {
        assert_eq!(
            DosFlatBindingPlan::new(
                &image,
                &bindings,
                BindingLimits {
                    max_owned_bytes: bytes,
                    ..BindingLimits::default()
                }
            )
            .unwrap_err(),
            BindingError::OwnedByteLimit(storage)
        );
    }
    assert!(
        DosFlatBindingPlan::new(
            &image,
            &bindings,
            BindingLimits {
                max_owned_bytes: selectors + size_of::<RelocationPatch>(),
                ..BindingLimits::default()
            }
        )
        .is_ok()
    );
    let mut budget = Budget(usize::MAX);
    let mut values: Vec<RelocationPatch> = Vec::new();
    assert_eq!(
        budget.reserve(&mut values, usize::MAX, BindingStorage::Patches),
        Err(BindingError::OwnedByteLimit(BindingStorage::Patches))
    );
    assert_eq!(values.capacity(), 0);
}

#[test]
fn empty_image_accepts_no_bindings_with_zero_limits() {
    let image = image(&[]);
    let plan = DosFlatBindingPlan::new(
        &image,
        &[],
        BindingLimits {
            max_owned_bytes: 0,
            max_patches: 0,
        },
    )
    .unwrap();
    assert!(plan.patches().is_empty());
    assert!(plan.object_binding(1).is_none());
}
