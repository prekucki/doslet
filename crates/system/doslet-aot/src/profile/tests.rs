use super::*;
use crate::{
    FileBackedSpan, RelocationTarget, Site, SymbolicImage, SymbolicObject,
    binding::{BindingLimits, DosFlatObjectBinding, DosFlatSelector},
    cfg::CfgLimits,
};
use alloc::vec;
use doslet_executable::{LeFixupSourceType, LeHeader, LeLocation, LeObject, LeObjectOffset};
use serde_json::{Value, json};

fn at(offset: u32) -> ObjectAddress {
    ObjectAddress {
        object_number: 1,
        offset,
    }
}
fn image(code: &[u8], bits: DefaultBitness) -> SymbolicImage {
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
                preferred_base: 0x7777_0000,
                flags: 4 | if bits == DefaultBitness::Bits32 {
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
                file_offset: 0,
                byte_len: code.len() as u32,
            }],
        }],
        sites: Vec::new(),
        provenance: Vec::new(),
    }
}
fn binding(image: &SymbolicImage, base: u32) -> DosFlatBindingPlan<'_> {
    let bits = image.objects()[0].default_bitness();
    DosFlatBindingPlan::new(
        image,
        &[DosFlatObjectBinding {
            object_number: 1,
            linear_base: base,
            normal: Some(DosFlatSelector {
                value: 8,
                base: if bits == DefaultBitness::Bits16 {
                    base
                } else {
                    0
                },
                limit: if bits == DefaultBitness::Bits16 {
                    0xffff
                } else {
                    u32::MAX
                },
                default_bitness: bits,
            }),
            alias: None,
        }],
        BindingLimits::default(),
    )
    .unwrap()
}
fn row(pc: u32, bits: u32, code: &[u8], tier: &str) -> Value {
    let mut window = [0u8; 15];
    window[..code.len()].copy_from_slice(code);
    json!({"pc":pc,"bits":bits,"bytes":window,"tier":tier,"reason":"learned"})
}
fn document(nodes: Value) -> Value {
    json!({"version":1,"unknown":"grey","irq_policy":"static-grey","nodes":nodes})
}
fn parse(value: &Value) -> Profile {
    Profile::parse(
        &serde_json::to_vec(value).unwrap(),
        ProfileLimits::default(),
    )
    .unwrap()
}

#[test]
fn verified_relocation_guidance_rebinds_only_the_same_immutable_image() {
    let mut image = image(&[0xb8, 0, 0, 0, 0, 0xc3], DefaultBitness::Bits32);
    image.sites.push(Site {
        source: at(1),
        kind: LeFixupSourceType::Offset32,
        target: RelocationTarget {
            object_number: 1,
            offset: Some(5),
        },
        is_alias: false,
        provenance: 0..0,
    });
    let old = binding(&image, 0x1000);
    let new = binding(&image, 0x8000);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let mut bytes = [0xb8, 0, 0, 0, 0];
    bytes[1..].copy_from_slice(&0x1005u32.to_le_bytes());
    let profile = parse(&document(json!([row(0x1000, 32, &bytes, "fast")])));
    let verified =
        BoundGuidance::bind(&profile, &graph, &old, None, GuidanceLimits::default()).unwrap();
    assert_eq!(verified.report().matched_instructions, 1);
    assert!(!verified.is_rebased());
    let moved = verified.rebind_to(&new, GuidanceLimits::default()).unwrap();
    assert!(moved.is_rebased());
    assert!(moved.matches(&graph, &new));
    assert!(!moved.matches(&graph, &old));
    assert_eq!(moved.instructions(), verified.instructions());
    assert!(moved.instructions()[0].verified_hot());
    // Blind binding in a different layout cannot match historical pointers/PCs.
    let direct =
        BoundGuidance::bind(&profile, &graph, &new, None, GuidanceLimits::default()).unwrap();
    assert_eq!(direct.report().matched_instructions, 0);
    let other = self::image(&[0xb8, 0, 0, 0, 0, 0xc3], DefaultBitness::Bits32);
    let other = binding(&other, 0x8000);
    assert!(matches!(
        verified.rebind_to(&other, GuidanceLimits::default()),
        Err(GuidanceError::DifferentImage)
    ));
    assert!(matches!(
        verified.rebind_to(&new, GuidanceLimits { max_owned_bytes: 0 }),
        Err(GuidanceError::Storage(ProfileError::OwnedByteLimit))
    ));
}

#[test]
fn accepts_native_export_and_learned_formats_and_preserves_metadata() {
    let mut r = row(0x1234, 32, &[0x90], "fast");
    r["count"] = json!(u64::MAX);
    r["high_count"] = json!(7);
    let mut value = document(json!([r]));
    value["rates"] = json!([null, 8, 16]);
    value["learning"] = json!(false);
    let exported = parse(&value);
    assert!(!exported.learned_format);
    assert_eq!(exported.nodes()[0].tier, Tier::Hot);
    assert_eq!(exported.nodes()[0].count, Some(u64::MAX));
    assert_eq!(exported.nodes()[0].high_count, Some(7));
    assert_eq!(exported.rates, Some([None, Some(8), Some(16)]));
    assert_eq!(exported.learning, Some(false));
    value.as_object_mut().unwrap().remove("version");
    value["learning_version"] = json!(1);
    value["identity"] = json!("exe-sha256:test\nidentity");
    let learned = parse(&value);
    assert!(learned.learned_format);
    assert_eq!(learned.identity(), Some("exe-sha256:test\nidentity"));
}

#[test]
fn rejects_malformed_types_versions_policies_and_fixed_byte_windows() {
    for input in [b"{".as_slice(), b"[]", b"null", b"{}{}"] {
        assert!(Profile::parse(input, ProfileLimits::default()).is_err());
    }
    let base = document(json!([row(0, 32, &[0x90], "fast")]));
    for (field, invalid) in [
        ("version", json!(2)),
        ("version", json!(null)),
        ("unknown", json!("fast")),
        ("irq_policy", json!("dynamic")),
        ("rates", json!([8, 16])),
        ("learning", json!("true")),
        ("identity", json!(false)),
        ("surprise", json!(1)),
    ] {
        let mut value = base.clone();
        value[field] = invalid;
        assert!(
            Profile::parse(
                &serde_json::to_vec(&value).unwrap(),
                ProfileLimits::default()
            )
            .is_err(),
            "{field}"
        );
    }
    for (field, invalid) in [
        ("pc", json!(-1)),
        ("pc", json!(4294967296u64)),
        ("bits", json!(64)),
        ("bytes", json!([144])),
        ("bytes", json!(vec![0; 16])),
        ("tier", json!("turbo")),
        ("reason", json!(1)),
        ("count", json!(1.5)),
        ("high_count", json!(-1)),
        ("surprise", json!(1)),
    ] {
        let mut value = base.clone();
        value["nodes"][0][field] = invalid;
        assert!(
            Profile::parse(
                &serde_json::to_vec(&value).unwrap(),
                ProfileLimits::default()
            )
            .is_err(),
            "{field}"
        );
    }
    let mut value = base.clone();
    value["nodes"][0]["bytes"][14] = json!(256);
    assert!(
        Profile::parse(
            &serde_json::to_vec(&value).unwrap(),
            ProfileLimits::default()
        )
        .is_err()
    );
    value = base.clone();
    value.as_object_mut().unwrap().remove("version");
    assert!(matches!(
        Profile::parse(
            &serde_json::to_vec(&value).unwrap(),
            ProfileLimits::default()
        ),
        Err(ProfileError::Version { .. })
    ));
    value = base;
    value["learning_version"] = json!(1);
    assert!(matches!(
        Profile::parse(
            &serde_json::to_vec(&value).unwrap(),
            ProfileLimits::default()
        ),
        Err(ProfileError::Version { .. })
    ));
    assert!(
        Profile::parse(
            br#"{"version":1,"version":1,"unknown":"grey","irq_policy":"static-grey","nodes":[]}"#,
            ProfileLimits::default()
        )
        .is_err()
    );
}

#[test]
fn rejects_input_node_identity_and_storage_bounds_before_growing() {
    let value = document(json!([row(0, 32, &[0x90], "hot")]));
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(matches!(
        Profile::parse(
            &bytes,
            ProfileLimits {
                max_input_bytes: bytes.len() - 1,
                ..ProfileLimits::default()
            }
        ),
        Err(ProfileError::InputLimit)
    ));
    assert!(matches!(
        Profile::parse(
            &bytes,
            ProfileLimits {
                max_nodes: 0,
                ..ProfileLimits::default()
            }
        ),
        Err(ProfileError::NodeLimit)
    ));
    assert!(matches!(
        Profile::parse(
            &bytes,
            ProfileLimits {
                max_owned_bytes: 0,
                ..ProfileLimits::default()
            }
        ),
        Err(ProfileError::OwnedByteLimit)
    ));
    let empty = serde_json::to_vec(&document(json!([]))).unwrap();
    assert!(
        Profile::parse(
            &empty,
            ProfileLimits {
                max_nodes: 0,
                max_owned_bytes: 0,
                ..ProfileLimits::default()
            }
        )
        .is_ok()
    );
    let mut value = value;
    value["identity"] = json!("abcd");
    assert!(matches!(
        Profile::parse(
            &serde_json::to_vec(&value).unwrap(),
            ProfileLimits {
                max_identity_bytes: 3,
                ..ProfileLimits::default()
            }
        ),
        Err(ProfileError::IdentityLimit)
    ));
    let duplicate = document(json!([
        row(0, 32, &[0x90], "hot"),
        row(0, 32, &[0x90], "hot")
    ]));
    assert!(matches!(
        Profile::parse(
            &serde_json::to_vec(&duplicate).unwrap(),
            ProfileLimits {
                max_nodes: 1,
                ..ProfileLimits::default()
            }
        ),
        Err(ProfileError::NodeLimit)
    ));
}

#[test]
fn binds_linear_pc_and_mode_exactly_without_profile_roots_or_preferred_base_guessing() {
    for (bits, mode) in [(DefaultBitness::Bits16, 16), (DefaultBitness::Bits32, 32)] {
        // RET stops discovery: the valid NOP at offset 1 remains unreachable.
        let image = image(&[0xc3, 0x90], bits);
        let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
        let binding = binding(&image, 0x1234_0000);
        let profile = parse(&document(json!([
            row(0x1234_0000, mode, &[0xc3], "fast"),
            row(0x1234_0001, mode, &[0x90], "fast"),
            row(
                0x1234_0000,
                if mode == 16 { 32 } else { 16 },
                &[0xc3],
                "fast"
            ),
            row(0x7777_0000, mode, &[0xc3], "fast"),
            row(0, mode, &[0xc3], "fast"),
        ])));
        let bound =
            BoundGuidance::bind(&profile, &graph, &binding, None, GuidanceLimits::default())
                .unwrap();
        assert_eq!(graph.nodes().len(), 1);
        assert_eq!(bound.instructions().len(), 1);
        assert!(bound.instructions()[0].verified_hot());
        assert_eq!(bound.report().matched_instructions, 1);
        assert_eq!(bound.report().unmatched_records, 4);
        assert!(
            bound
                .diagnostics()
                .iter()
                .any(|d| d.reason == UnmatchedReason::NotInstructionStart)
        );
        assert!(
            bound
                .diagnostics()
                .iter()
                .any(|d| d.reason == UnmatchedReason::BitnessMismatch)
        );
    }
}

#[test]
fn duplicate_labels_and_actual_instruction_bytes_are_conservative() {
    let image = image(&[0x90, 0x90, 0x90, 0x90, 0xc3], DefaultBitness::Bits32);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let binding = binding(&image, 0x1000);
    let mut trailing = row(0x1000, 32, &[0x90], "hot");
    trailing["bytes"][14] = json!(255); // Outside this one-byte instruction.
    trailing["count"] = json!(12);
    trailing["high_count"] = json!(5);
    let mut counted = row(0x1000, 32, &[0x90], "fast");
    counted["count"] = json!(10);
    let mut locked = row(0x1003, 32, &[0x90], "hot");
    locked["reason"] = json!("code-conflict");
    let profile = parse(&document(json!([
        trailing,
        counted,
        row(0x1001, 32, &[0x90], "hot"),
        row(0x1001, 32, &[0x90], "cold"),
        row(0x1002, 32, &[0x90], "hot"),
        row(0x1002, 32, &[0xcc], "hot"),
        locked
    ])));
    let bound =
        BoundGuidance::bind(&profile, &graph, &binding, None, GuidanceLimits::default()).unwrap();
    let nodes = bound.instructions();
    assert!(nodes[0].verified_hot());
    assert_eq!(
        nodes[0].weight,
        Weight {
            count: Some(12),
            high_count: Some(5)
        }
    );
    for node in &nodes[1..4] {
        assert_eq!(node.tier, Tier::Grey);
        assert!(!node.verified_hot());
    }
    assert_eq!(nodes[1].status, GuidanceStatus::Conflict);
    assert_eq!(nodes[2].status, GuidanceStatus::Conflict);
    assert_eq!(nodes[3].status, GuidanceStatus::ProfileCodeConflict);
    assert_eq!(nodes[4].status, GuidanceStatus::Unmatched);
    assert_eq!(bound.report().duplicate_records, 3);
    assert_eq!(bound.report().conflicting_records, 2);
    assert_eq!(bound.report().byte_mismatches, 1);
    assert_eq!(bound.report().profile_code_conflicts, 1);
}

#[test]
fn relocation_operands_must_equal_explicit_binding_bytes() {
    let mut image = image(&[0xb8, 0, 0, 0, 0, 0xc3], DefaultBitness::Bits32);
    image.sites.push(Site {
        source: at(1),
        kind: LeFixupSourceType::Offset32,
        target: RelocationTarget {
            object_number: 1,
            offset: Some(5),
        },
        is_alias: false,
        provenance: 0..0,
    });
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let binding = binding(&image, 0x7654_0000);
    for (code, expected) in [
        ([0xb8, 5, 0, 0x54, 0x76], GuidanceStatus::Verified),
        ([0xb8, 0, 0, 0, 0], GuidanceStatus::ByteMismatch),
        ([0xb8, 5, 0, 0x55, 0x76], GuidanceStatus::ByteMismatch),
    ] {
        let profile = parse(&document(json!([row(0x7654_0000, 32, &code, "hot")])));
        let bound =
            BoundGuidance::bind(&profile, &graph, &binding, None, GuidanceLimits::default())
                .unwrap();
        assert_eq!(bound.instructions()[0].status, expected);
        assert_eq!(
            bound.instructions()[0].verified_hot(),
            expected == GuidanceStatus::Verified
        );
    }
    assert_eq!(image.objects()[0].bytes()[1..5], [0; 4]);
}

#[test]
fn irq_observations_and_mixed_contexts_cannot_be_promoted_to_hot() {
    let image = image(&[0x90, 0x90, 0x90, 0xc3], DefaultBitness::Bits32);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let binding = binding(&image, 0x1000);
    let mut interrupt = row(0x1001, 32, &[0x90], "hot");
    interrupt["reason"] = json!("interrupt-frame");
    let mut mixed = row(0x1002, 32, &[0x90], "hot");
    mixed["reason"] = json!("interrupt-frame");
    let profile = parse(&document(json!([
        row(0x1000, 32, &[0x90], "irq"),
        interrupt,
        mixed,
        row(0x1002, 32, &[0x90], "hot")
    ])));
    let bound =
        BoundGuidance::bind(&profile, &graph, &binding, None, GuidanceLimits::default()).unwrap();
    for node in &bound.instructions()[..2] {
        assert_eq!(node.tier, Tier::Irq);
        assert_eq!(node.context, Context::IrqObserved);
        assert!(!node.verified_hot());
    }
    assert_eq!(bound.instructions()[2].status, GuidanceStatus::Conflict);
    assert_eq!(bound.instructions()[2].tier, Tier::Grey);
    assert_eq!(bound.instructions()[2].context, Context::IrqObserved);
}

#[test]
fn explicit_identity_checks_do_not_replace_code_checks_and_layout_is_not_reused() {
    let image = image(&[0x90, 0xc3], DefaultBitness::Bits32);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let binding = binding(&image, 0x1000);
    let mut value = document(json!([row(0x1000, 32, &[0xcc], "hot")]));
    let no_identity = parse(&value);
    assert!(matches!(
        BoundGuidance::bind(
            &no_identity,
            &graph,
            &binding,
            Some("expected"),
            GuidanceLimits::default()
        ),
        Err(GuidanceError::MissingIdentity)
    ));
    value["identity"] = json!("expected");
    let profile = parse(&value);
    assert!(matches!(
        BoundGuidance::bind(
            &profile,
            &graph,
            &binding,
            Some("different"),
            GuidanceLimits::default()
        ),
        Err(GuidanceError::IdentityMismatch)
    ));
    let bound = BoundGuidance::bind(
        &profile,
        &graph,
        &binding,
        Some("expected"),
        GuidanceLimits::default(),
    )
    .unwrap();
    assert_eq!(bound.identity_check(), IdentityCheck::Matched);
    assert_eq!(bound.instructions()[0].status, GuidanceStatus::ByteMismatch);
    let relocated_layout = self::binding(&image, 0x2000);
    let bound = BoundGuidance::bind(
        &profile,
        &graph,
        &relocated_layout,
        None,
        GuidanceLimits::default(),
    )
    .unwrap();
    assert_eq!(bound.identity_check(), IdentityCheck::Unchecked);
    assert_eq!(bound.report().matched_instructions, 0);
    assert_eq!(bound.instructions()[0].status, GuidanceStatus::Unmatched);
    assert!(matches!(
        BoundGuidance::bind(
            &profile,
            &graph,
            &binding,
            None,
            GuidanceLimits { max_owned_bytes: 0 }
        ),
        Err(GuidanceError::Storage(ProfileError::OwnedByteLimit))
    ));
}

#[test]
fn empty_objects_cannot_hide_nonempty_bound_code_ranges() {
    for empty_base in [0x1000, 0x1001] {
        let mut image = image(&[0x90, 0xc3], DefaultBitness::Bits32);
        image.header.object_count = 2;
        image.objects.push(SymbolicObject {
            object_number: 2,
            metadata: LeObject {
                virtual_size: 0,
                preferred_base: 0,
                flags: 0x2000,
                first_page_number: 0,
                page_count: 0,
            },
            bytes: Vec::new(),
            file_backed_spans: Vec::new(),
        });
        let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
        let bindings = [
            DosFlatObjectBinding {
                object_number: 1,
                linear_base: 0x1000,
                normal: Some(DosFlatSelector {
                    value: 8,
                    base: 0,
                    limit: u32::MAX,
                    default_bitness: DefaultBitness::Bits32,
                }),
                alias: None,
            },
            DosFlatObjectBinding {
                object_number: 2,
                linear_base: empty_base,
                normal: Some(DosFlatSelector {
                    value: 16,
                    base: 0,
                    limit: u32::MAX,
                    default_bitness: DefaultBitness::Bits32,
                }),
                alias: None,
            },
        ];
        let binding = DosFlatBindingPlan::new(&image, &bindings, BindingLimits::default()).unwrap();
        let profile = parse(&document(json!([
            row(0x1000, 32, &[0x90], "hot"),
            row(0x1001, 32, &[0xc3], "hot")
        ])));
        let bound =
            BoundGuidance::bind(&profile, &graph, &binding, None, GuidanceLimits::default())
                .unwrap();
        assert_eq!(bound.report().matched_instructions, 2);
        assert!(bound.instructions().iter().all(|node| node.verified_hot()));
    }
}
