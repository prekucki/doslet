use super::*;
use doslet_x86_wasm::static_program::{
    EntryAlias,
    memory::{Descriptor, Effects, RamDescriptor, SegmentBounds, StackAddress},
};

fn descriptor(mode: StackAddress) -> Descriptor {
    Descriptor {
        stack_address: mode,
        ram: RamDescriptor {
            pointer: 4096,
            linear_base: 0x500,
            length: 4,
            generations_pointer: 6000,
            generations_length: 1,
            code_pages_pointer: 7000,
            code_pages_length: 1,
            ..RamDescriptor::default()
        },
        windows: [[SegmentBounds {
            valid: true,
            base: 0,
            first: 0,
            last: u32::MAX,
        }; 6]; 2],
        ..Descriptor::default()
    }
}

fn prepare(graph: &StaticCfg<'_>, binding: &DosFlatBindingPlan<'_>) -> PreparedProgram {
    PreparedProgram::from_bound_cfg(
        graph,
        binding,
        LoweringLimits {
            direct_memory: true,
            ..LoweringLimits::default()
        },
    )
    .unwrap()
}

#[test]
fn original_near_returns_preserve_flags_and_separate_operand_and_stack_widths() {
    for (bytes, width, extra) in [
        (vec![0xc3], Width::Dword, 0),
        (vec![0x66, 0xc3], Width::Word, 0),
        (vec![0xc2, 0xff, 0xff], Width::Dword, 0xffff),
        (vec![0x66, 0xc2, 0xff, 0xff], Width::Word, 0xffff),
        (vec![0x67, 0xc2, 7, 0], Width::Dword, 7),
    ] {
        let image = image(&bytes, DefaultBitness::Bits32);
        let binding = binding(&image, 0x1000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let prepared = prepare(&graph, &binding);
        assert_eq!(prepared.report().near_returns, 1);
        assert_eq!(prepared.program().regions[0].operations, []);
        assert_eq!(
            prepared.program().regions[0].terminator,
            Terminator::Return {
                width,
                pop_bytes: extra,
                target_space: 0,
            }
        );
        for mode in [StackAddress::Bits16, StackAddress::Bits32] {
            let initial_sp: u32 = if mode == StackAddress::Bits16 {
                0xabcd_0500
            } else {
                0x500
            };
            let mut gpr = [0x1234_5678; 8];
            gpr[4] = initial_sp;
            let (frame, calls) = execute_setup(
                &prepared,
                Frame {
                    gpr,
                    eflags: 0xad7,
                    address: address(at(0)),
                    remaining_budget: 1,
                    ..Frame::default()
                },
                CompileOptions {
                    direct_memory: true,
                    ..CompileOptions::default()
                },
                |memory| {
                    descriptor(mode).write(&mut memory[72..]).unwrap();
                    memory[4096..4100].copy_from_slice(&0x9876_4321u32.to_le_bytes());
                },
                |memory| {
                    assert_eq!(&memory[4096..4100], &0x9876_4321u32.to_le_bytes());
                    assert_eq!(Effects::read(&memory[72..]).unwrap().ram_writes, 0);
                    assert_eq!(Effects::read(&memory[72..]).unwrap().device_reads, 0);
                },
            );
            let sp = initial_sp.wrapping_add(width.bits() / 8 + u32::from(extra));
            gpr[4] = if mode == StackAddress::Bits16 {
                (initial_sp & 0xffff_0000) | (sp & 0xffff)
            } else {
                sp
            };
            assert_eq!(frame.gpr, gpr, "{bytes:02x?} {mode:?}");
            assert_eq!(frame.eflags, 0xad7);
            assert_eq!(frame.charged_work, 1);
            assert_eq!(
                frame.address,
                Address {
                    space: 0,
                    offset: 0x9876_4321 & width.mask()
                }
            );
            assert!(calls.is_empty());
        }
    }
}

#[test]
fn relocated_call_and_return_cross_objects_through_existing_linear_entries() {
    let mut image = image(&[0xe8, 0, 0, 0, 0, 0x43, 0xf4], DefaultBitness::Bits32);
    append_data_code(&mut image, &[0x40, 0xc3]);
    image.sites.push(Site {
        source: at(1),
        kind: LeFixupSourceType::SelfRelative32,
        target: RelocationTarget {
            object_number: 2,
            offset: Some(0),
        },
        is_alias: false,
        provenance: 0..0,
    });
    let binding = two_object_binding(&image, 0x3000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared = prepare(&graph, &binding);
    assert_eq!(prepared.report().direct_calls, 1);
    assert_eq!(prepared.report().near_returns, 1);
    let aliases: Vec<_> = prepared
        .program()
        .regions
        .iter()
        .filter(|region| region.completed_instructions() != 0)
        .map(|region| EntryAlias {
            address: Address {
                space: 0,
                offset: binding
                    .object_binding(region.entry.space)
                    .unwrap()
                    .linear_base
                    + region.entry.offset,
            },
            target: region.id,
        })
        .collect();
    for mode in [StackAddress::Bits16, StackAddress::Bits32] {
        let initial_sp = if mode == StackAddress::Bits16 {
            0xabcd_0504
        } else {
            0x504
        };
        let initial = [3, 0, 0, 7, initial_sp, 0, 0, 0];
        let (frame, calls) = execute_setup_with_aliases(
            &prepared,
            Frame {
                gpr: initial,
                eflags: 0x203,
                address: address(at(0)),
                remaining_budget: 4,
                ..Frame::default()
            },
            CompileOptions {
                direct_memory: true,
                ..CompileOptions::default()
            },
            &aliases,
            |memory| {
                descriptor(mode).write(&mut memory[72..]).unwrap();
            },
            |memory| {
                assert_eq!(&memory[4096..4100], &0x1005u32.to_le_bytes());
                assert_eq!(Effects::read(&memory[72..]).unwrap().ram_writes, 1);
                assert_eq!(&memory[6000..6008], &1u64.to_le_bytes());
            },
        );
        let mut expected = initial;
        expected[0] += 1;
        expected[3] += 1;
        assert_eq!(frame.gpr, expected);
        assert_eq!(frame.eflags, 0x203);
        assert_eq!(frame.charged_work, 4);
        assert_eq!(frame.address, address(at(6)));
        assert!(calls.is_empty());
    }
}

#[test]
fn far_returns_and_unbound_or_option_off_near_returns_remain_explicit_fallbacks() {
    for bytes in [vec![0xcb], vec![0xca, 4, 0], vec![0xf3, 0xc3]] {
        let image = image(&bytes, DefaultBitness::Bits32);
        let binding = binding(&image, 0x1000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        assert_eq!(prepare(&graph, &binding).report().near_returns, 0);
    }
    let image = image(&[0xc3], DefaultBitness::Bits32);
    let binding = binding(&image, 0x1000);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    assert_eq!(prepare(&graph, &binding).report().near_returns, 0);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    assert_eq!(
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default())
            .unwrap()
            .report()
            .near_returns,
        0
    );
}
