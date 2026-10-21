use super::*;
use crate::{FileBackedSpan, SymbolicImage, SymbolicObject, cfg::CfgLimits};
use alloc::{vec, vec::Vec};
use doslet_executable::{LeHeader, LeLocation, LeObject, LeObjectOffset};

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
                preferred_base: 0x10000,
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

fn offsets(plan: &RegionPlan<'_, '_>) -> Vec<Vec<u32>> {
    (0..plan.regions().len())
        .map(|region| {
            plan.instructions(region)
                .unwrap()
                .map(|node| node.address.offset)
                .collect()
        })
        .collect()
}

fn assert_partition(plan: &RegionPlan<'_, '_>) {
    let mut all: Vec<_> = (0..plan.regions().len())
        .flat_map(|region| plan.node_indices(region).unwrap().iter().copied())
        .collect();
    all.sort_unstable();
    assert_eq!(all, (0..plan.graph().nodes().len()).collect::<Vec<_>>());
    for (index, region) in plan.regions().iter().enumerate() {
        assert_eq!(plan.region_index(region.entry), Some(index));
        assert_eq!(
            region.instruction_count(),
            plan.node_indices(index).unwrap().len()
        );
    }
    assert!(plan.instructions(plan.regions().len()).is_none());
}

#[test]
fn straight_line_runs_split_at_bound_and_explicit_entries() {
    let image = image(&[0x90, 0x40, 0x43, 0x90, 0x48, 0xc3]);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let limits = RegionLimits {
        max_instructions_per_region: 2,
        ..RegionLimits::default()
    };
    let plan = RegionPlan::build(&graph, &[], limits).unwrap();
    assert_eq!(offsets(&plan), [vec![0, 1], vec![2, 3], vec![4, 5]]);
    assert_partition(&plan);
    let plan =
        RegionPlan::build(&graph, &[address(3), address(3)], RegionLimits::default()).unwrap();
    assert_eq!(offsets(&plan), [vec![0, 1, 2], vec![3, 4, 5]]);
    assert_partition(&plan);
}

#[test]
fn branches_and_shared_tail_preserve_every_control_entry() {
    // JZ 5; INC EAX; JMP 6; INC EBX; NOP; RET
    let image = image(&[0x74, 3, 0x40, 0xeb, 1, 0x43, 0x90, 0xc3]);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let plan = RegionPlan::build(&graph, &[], RegionLimits::default()).unwrap();
    assert_eq!(offsets(&plan), [vec![0], vec![2, 3], vec![5], vec![6, 7]]);
    assert_partition(&plan);
}

#[test]
fn calls_runtime_services_and_cycles_end_regions() {
    // CALL 9; INT 21; JMP 0; INC EAX; RET
    let image = image(&[0xe8, 4, 0, 0, 0, 0xcd, 0x21, 0xeb, 0xf7, 0x40, 0xc3]);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let plan = RegionPlan::build(&graph, &[], RegionLimits::default()).unwrap();
    assert_eq!(offsets(&plan), [vec![0], vec![5], vec![7], vec![9, 10]]);
    assert_partition(&plan);
}

#[test]
fn overlapping_instruction_streams_group_by_node_identity() {
    // JZ alternative MOV; TEST AX,0xcf89; RET. MOV's bytes are TEST's immediate.
    let image = image(&[0x74, 2, 0x66, 0xa9, 0x89, 0xcf, 0xc3]);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    assert_eq!(graph.overlaps().len(), 1);
    let plan = RegionPlan::build(&graph, &[], RegionLimits::default()).unwrap();
    assert_eq!(offsets(&plan), [vec![0], vec![2], vec![4], vec![6]]);
    assert_partition(&plan);
}

#[test]
fn invalid_destinations_and_stopped_entries_do_not_create_regions() {
    // JZ a truncated MOV; RET
    let image = image(&[0x74, 1, 0xc3, 0xb8]);
    let graph = StaticCfg::analyze(&image, &[address(100)], CfgLimits::default()).unwrap();
    let plan = RegionPlan::build(&graph, &[], RegionLimits::default()).unwrap();
    assert_eq!(offsets(&plan), [vec![0], vec![2]]);
    assert_partition(&plan);
    assert_eq!(
        RegionPlan::build(&graph, &[address(3)], RegionLimits::default()).unwrap_err(),
        RegionError::UnknownBoundary(address(3))
    );
}

#[test]
fn preparation_limits_fail_without_publishing_a_partial_partition() {
    let image = image(&[0x90, 0x90, 0xc3]);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let defaults = RegionLimits::default();
    for (limits, error) in [
        (
            RegionLimits {
                max_instructions_per_region: 0,
                ..defaults
            },
            RegionError::ZeroRegionLength,
        ),
        (
            RegionLimits {
                max_instructions: 2,
                ..defaults
            },
            RegionError::InstructionLimit,
        ),
        (
            RegionLimits {
                max_regions: 0,
                ..defaults
            },
            RegionError::RegionLimit,
        ),
        (
            RegionLimits {
                max_regions: 1,
                max_instructions_per_region: 1,
                ..defaults
            },
            RegionError::RegionLimit,
        ),
        (
            RegionLimits {
                max_owned_bytes: 0,
                ..defaults
            },
            RegionError::OwnedByteLimit,
        ),
    ] {
        assert_eq!(RegionPlan::build(&graph, &[], limits).unwrap_err(), error);
    }
    assert_eq!(
        RegionPlan::build(
            &graph,
            &[address(1)],
            RegionLimits {
                max_boundaries: 0,
                ..defaults
            }
        )
        .unwrap_err(),
        RegionError::BoundaryLimit
    );
    let plan = RegionPlan::build(&graph, &[address(1)], defaults).unwrap();
    let repeat = RegionPlan::build(&graph, &[address(1)], defaults).unwrap();
    assert_eq!(plan.regions(), repeat.regions());
    assert_eq!(plan.node_indices, repeat.node_indices);
}

#[test]
fn empty_graph_has_no_regions_even_with_zero_capacity() {
    let image = image(&[0xb8]);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    assert!(graph.nodes().is_empty());
    let plan = RegionPlan::build(
        &graph,
        &[],
        RegionLimits {
            max_regions: 0,
            max_instructions: 0,
            max_boundaries: 0,
            max_owned_bytes: 0,
            max_instructions_per_region: 1,
        },
    )
    .unwrap();
    assert!(plan.regions().is_empty());
}
