use super::*;
use crate::{
    FileBackedSpan, RelocationTarget, Site, SymbolicImage, SymbolicObject,
    binding::{BindingLimits, DosFlatObjectBinding, DosFlatSelector},
    cfg::CfgLimits,
};
use alloc::vec;
use doslet_executable::{LeFixupSourceType, LeHeader, LeLocation, LeObject, LeObjectOffset};
use doslet_x86_wasm::static_program::{CompileOptions, Frame, abi};
use wasmi::{Caller, Engine, Linker, Memory, MemoryType, Module, Store};

#[path = "call_tests.rs"]
mod call_tests;
#[path = "extension_tests.rs"]
mod extension_tests;
#[path = "memory_procedure_tests.rs"]
mod memory_procedure_tests;
#[path = "multiply_tests.rs"]
mod multiply_tests;
#[path = "procedure_tests.rs"]
mod procedure_tests;
#[path = "return_tests.rs"]
mod return_tests;

fn at(offset: u32) -> ObjectAddress {
    ObjectAddress {
        object_number: 1,
        offset,
    }
}

#[test]
fn explicit_runtime_entry_stops_before_effects_and_cannot_create_code() {
    let image = image(&[0x40, 0x90, 0xc3], DefaultBitness::Bits32);
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let prepared = PreparedProgram::with_runtime_entries(
        &graph,
        &binding,
        None,
        &[at(0)],
        LoweringLimits::default(),
    )
    .unwrap();
    let (frame, callbacks) = execute(
        &prepared,
        Frame {
            address: address(at(0)),
            remaining_budget: 100,
            ..Frame::default()
        },
    );
    assert_eq!(frame.gpr[0], 0);
    assert_eq!(frame.charged_work, 0);
    assert_eq!(callbacks, [(abi::REASON_FALLBACK, address(at(0)))]);
    for entries in [&[at(0), at(0)][..], &[at(1), at(0)], &[at(30)]] {
        assert!(matches!(
            PreparedProgram::with_runtime_entries(
                &graph,
                &binding,
                None,
                entries,
                LoweringLimits::default()
            ),
            Err(LoweringError::InvalidRuntimeEntries)
        ));
    }
}
fn image(code: &[u8], bitness: DefaultBitness) -> SymbolicImage {
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
                preferred_base: 0x10000,
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
                file_offset: 0,
                byte_len: code.len() as u32,
            }],
        }],
        sites: Vec::new(),
        provenance: Vec::new(),
    }
}
fn binding(image: &SymbolicImage, base: u32) -> DosFlatBindingPlan<'_> {
    let bitness = image.objects()[0].default_bitness();
    DosFlatBindingPlan::new(
        image,
        &[DosFlatObjectBinding {
            object_number: 1,
            linear_base: base,
            normal: Some(DosFlatSelector {
                value: 8,
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
        }],
        BindingLimits::default(),
    )
    .unwrap()
}

fn append_data_code(image: &mut SymbolicImage, code: &[u8]) {
    image.header.object_count = 2;
    image.objects.push(SymbolicObject {
        object_number: 2,
        metadata: LeObject {
            virtual_size: code.len() as u32,
            preferred_base: 0x9876_0000,
            flags: 0x2003,
            first_page_number: 2,
            page_count: 1,
        },
        bytes: code.to_vec(),
        file_backed_spans: vec![FileBackedSpan {
            object_offset: 0,
            file_offset: 4096,
            byte_len: code.len() as u32,
        }],
    });
}

fn two_object_binding(image: &SymbolicImage, data_base: u32) -> DosFlatBindingPlan<'_> {
    let objects: Vec<_> = [0x1000, data_base]
        .into_iter()
        .enumerate()
        .map(|(index, linear_base)| DosFlatObjectBinding {
            object_number: index as u32 + 1,
            linear_base,
            normal: Some(DosFlatSelector {
                value: (index as u16 + 1) * 8,
                base: 0,
                limit: u32::MAX,
                default_bitness: DefaultBitness::Bits32,
            }),
            alias: None,
        })
        .collect();
    DosFlatBindingPlan::new(image, &objects, BindingLimits::default()).unwrap()
}

#[test]
fn bound_cross_object_branches_execute_relocated_data_code_and_reject_other_bindings() {
    for (source, field) in [
        (vec![0xe9, 0, 0, 0, 0, 0x43, 0xf4], 1),
        (vec![0x0f, 0x85, 0, 0, 0, 0, 0x43, 0xf4], 2),
    ] {
        let mut image = image(&source, DefaultBitness::Bits32);
        append_data_code(&mut image, &[0x40, 0xf4]);
        image.sites.push(Site {
            source: at(field),
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
        let prepared =
            PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
        let (frame, calls) = execute(
            &prepared,
            Frame {
                address: address(at(0)),
                remaining_budget: 2,
                ..Frame::default()
            },
        );
        assert_eq!(frame.gpr[0], 1);
        assert_eq!(frame.gpr[3], 0);
        assert_eq!(
            frame.address,
            Address {
                space: 2,
                offset: 1
            }
        );
        assert_eq!(frame.charged_work, 2);
        assert!(calls.is_empty());
        assert_eq!(prepared.report().external_stubs, 0);
        if field == 2 {
            let (frame, calls) = execute(
                &prepared,
                Frame {
                    address: address(at(0)),
                    eflags: 0x42,
                    remaining_budget: 2,
                    ..Frame::default()
                },
            );
            assert_eq!(frame.gpr[0], 0);
            assert_eq!(frame.gpr[3], 1);
            assert_eq!(frame.address, address(at(7)));
            assert!(calls.is_empty());
        }
        let other = two_object_binding(&image, 0x4000);
        assert_eq!(
            PreparedProgram::from_bound_cfg(&graph, &other, LoweringLimits::default()).unwrap_err(),
            LoweringError::DifferentBinding
        );
    }
}

#[test]
fn bound_sequential_memory_access_at_object_boundary_stays_isolated() {
    use doslet_x86_wasm::static_program::memory::{Descriptor, RamDescriptor, SegmentBounds};
    let mut image = image(&[0xa1, 0x00, 0x05, 0, 0], DefaultBitness::Bits32);
    append_data_code(&mut image, &[0x43, 0xf4]);
    let binding = two_object_binding(&image, 0x1005);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared = PreparedProgram::from_bound_cfg(
        &graph,
        &binding,
        LoweringLimits {
            direct_memory: true,
            ..LoweringLimits::default()
        },
    )
    .unwrap();
    let (frame, calls) = execute_setup(
        &prepared,
        Frame {
            address: address(at(0)),
            remaining_budget: 2,
            ..Frame::default()
        },
        CompileOptions {
            direct_memory: true,
            ..CompileOptions::default()
        },
        |bytes| {
            bytes[4096..4100].copy_from_slice(&0x1234_5678u32.to_le_bytes());
            Descriptor {
                ram: RamDescriptor {
                    pointer: 4096,
                    linear_base: 0x500,
                    length: 4,
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
            .write(&mut bytes[72..])
            .unwrap();
        },
        |_| {},
    );
    assert_eq!(frame.gpr[0], 0x1234_5678);
    assert_eq!(frame.gpr[3], 1);
    assert_eq!(
        frame.address,
        Address {
            space: 2,
            offset: 1
        }
    );
    assert_eq!(frame.charged_work, 2);
    assert!(calls.is_empty());
}

#[test]
fn bound_fallthrough_to_a_stopped_decode_exits_at_the_actual_object() {
    let mut image = image(&[0x90], DefaultBitness::Bits32);
    append_data_code(&mut image, &[0x0f]);
    let binding = two_object_binding(&image, 0x1001);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    assert_eq!(graph.nodes().len(), 1);
    let prepared =
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
    let (frame, calls) = execute(
        &prepared,
        Frame {
            address: address(at(0)),
            remaining_budget: 10,
            ..Frame::default()
        },
    );
    let next = Address {
        space: 2,
        offset: 0,
    };
    assert_eq!(frame.charged_work, 1);
    assert_eq!(frame.address, next);
    assert_eq!(calls, [(abi::REASON_FALLBACK, next)]);
}

#[test]
fn bound_cross_object_fallthrough_keeps_each_region_source_in_one_object() {
    let mut image = image(&[0x40], DefaultBitness::Bits32);
    append_data_code(&mut image, &[0x43, 0xf4]);
    let binding = two_object_binding(&image, 0x1001);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared =
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
    for (region, sources) in prepared.program().regions.iter().zip(prepared.sources()) {
        assert!(
            sources
                .instructions()
                .iter()
                .all(|source| source.object_number == region.entry.space)
        );
    }
    let (frame, calls) = execute(
        &prepared,
        Frame {
            address: address(at(0)),
            remaining_budget: 1,
            ..Frame::default()
        },
    );
    assert_eq!(frame.charged_work, 1);
    assert_eq!(
        frame.address,
        Address {
            space: 2,
            offset: 0
        }
    );
    assert_eq!(frame.gpr[0], 1);
    assert_eq!(frame.gpr[3], 0);
    assert!(calls.is_empty());
}

#[test]
fn original_shift_encodings_lower_to_counted_wasm_including_aliases_and_cl() {
    let initial = [0x1234_5678, 1, 0, 0, 0, 1, 0, 0];
    for (bytes, target, expected) in [
        (vec![0xc1, 0xe8, 0x10], 0, 0x1234),
        (vec![0xc1, 0xe5, 0x17], 5, 0x0080_0000),
        (vec![0x0f, 0xa4, 0xe8, 9], 0, 0x1234_5678u32 << 9),
        (vec![0xd1, 0xf0], 0, 0x2468_acf0), // SAL /6
        (vec![0xd3, 0xd1], 1, 3),           // RCL ECX,CL; old CL=1, CF=1
        (vec![0xd2, 0xcc], 0, 0x1234_2b78), // ROR AH,CL; preserve aliases
        (vec![0x66, 0x0f, 0xac, 0xc0, 0x20], 0, initial[0]),
    ] {
        let mut code = bytes.clone();
        code.push(0xf4);
        let image = image(&code, DefaultBitness::Bits32);
        let binding = binding(&image, 0x1000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let prepared =
            PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
        assert_eq!(prepared.report().register_instructions, 1, "{bytes:02x?}");
        let flags = 0x203;
        let (frame, calls) = execute(
            &prepared,
            Frame {
                gpr: initial,
                eflags: flags,
                address: address(at(0)),
                remaining_budget: 1,
                ..Frame::default()
            },
        );
        let mut expected_gpr = initial;
        expected_gpr[target] = expected;
        assert_eq!(frame.gpr, expected_gpr, "{bytes:02x?}");
        assert_eq!(frame.charged_work, 1);
        assert_eq!(frame.address, address(at(bytes.len() as u32)));
        assert!(calls.is_empty());
        if bytes.last() == Some(&0x20) {
            assert_eq!(frame.eflags, flags);
        }
    }
}

#[test]
fn unsupported_word_double_shift_counts_and_memory_shifts_fall_back_before_effects() {
    for bytes in [
        vec![0x66, 0x0f, 0xa5, 0xc8],     // SHLD AX,CX,CL
        vec![0x66, 0x0f, 0xa4, 0xc8, 17], // undefined word count
        vec![0xc1, 0x28, 0x10],           // SHR DWORD [EAX],16
    ] {
        let image = image(&bytes, DefaultBitness::Bits32);
        let binding = binding(&image, 0x1000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let prepared =
            PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
        let initial = Frame {
            gpr: [0x1234_5678; 8],
            address: address(at(0)),
            remaining_budget: 10,
            ..Frame::default()
        };
        let (frame, calls) = execute(&prepared, initial);
        assert_eq!(frame.gpr, initial.gpr);
        assert_eq!(frame.eflags, initial.eflags);
        assert_eq!(frame.charged_work, 0);
        assert_eq!(calls, [(abi::REASON_FALLBACK, initial.address)]);
    }
}

#[test]
fn reachable_code_writes_isolate_the_mutable_instruction_without_relaxing_its_source() {
    // MOV [linear immediate field],EBX; NOP; MOV ECX,12345678h; NOP; HLT.
    let image = image(
        &[
            0x89, 0x1d, 0x08, 0x10, 0, 0, 0x90, 0xb9, 0x78, 0x56, 0x34, 0x12, 0x90, 0xf4,
        ],
        DefaultBitness::Bits32,
    );
    let original = image.objects()[0].bytes().to_vec();
    let binding = binding(&image, 0x1000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared = PreparedProgram::from_bound_cfg(
        &graph,
        &binding,
        LoweringLimits {
            direct_memory: true,
            isolate_mutable_sources: true,
            ..LoweringLimits::default()
        },
    )
    .unwrap();
    assert_eq!(prepared.report().mutable_source_instructions, 1);
    for offset in [6, 7, 12] {
        let source = prepared
            .program()
            .regions
            .iter()
            .zip(prepared.sources())
            .find(|(region, _)| region.entry == address(at(offset)))
            .unwrap()
            .1;
        assert_eq!(source.instructions(), &[at(offset)]);
    }
    let mutable = prepared
        .program()
        .regions
        .iter()
        .find(|region| region.entry == address(at(7)))
        .unwrap();
    assert_eq!(
        mutable.operations,
        [Operation::Move {
            target: Register::Ecx,
            source: Source::Immediate(0x1234_5678)
        }]
    );
    assert_eq!(image.objects()[0].bytes(), original);
}

#[test]
fn profile_byte_conflicts_only_partition_existing_source_and_never_supply_operands() {
    use crate::profile::{BoundGuidance, GuidanceLimits, Profile, ProfileLimits};
    use serde_json::json;
    let image = image(
        &[0x90, 0xb9, 0x78, 0x56, 0x34, 0x12, 0x90, 0xf4],
        DefaultBitness::Bits32,
    );
    let binding = binding(&image, 0x1000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let observed = [0xb9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    let bytes = serde_json::to_vec(
        &json!({"version":1,"unknown":"grey","irq_policy":"static-grey","nodes":[
            {"pc":0x1001,"bits":32,"bytes":observed,"tier":"grey","count":10,"high_count":0},
            {"pc":0x1003,"bits":32,"bytes":observed,"tier":"grey","count":10,"high_count":0}
        ]}),
    )
    .unwrap();
    let profile = Profile::parse(&bytes, ProfileLimits::default()).unwrap();
    let guidance =
        BoundGuidance::bind(&profile, &graph, &binding, None, GuidanceLimits::default()).unwrap();
    assert_eq!(guidance.report().byte_mismatches, 1);
    let prepared = PreparedProgram::from_guided_cfg(
        &graph,
        &binding,
        &guidance,
        false,
        LoweringLimits {
            isolate_mutable_sources: true,
            ..LoweringLimits::default()
        },
    )
    .unwrap();
    assert_eq!(prepared.report().mutable_source_instructions, 1);
    for offset in [0, 1, 6] {
        let source = prepared
            .program()
            .regions
            .iter()
            .zip(prepared.sources())
            .find(|(region, _)| region.entry == address(at(offset)))
            .unwrap()
            .1;
        assert_eq!(source.instructions(), &[at(offset)]);
    }
    assert!(
        !prepared
            .program()
            .regions
            .iter()
            .any(|region| region.entry == address(at(3)))
    );
    let (frame, _) = execute(
        &prepared,
        Frame {
            address: address(at(0)),
            remaining_budget: 10,
            ..Frame::default()
        },
    );
    assert_eq!(frame.gpr[1], 0x1234_5678);
    assert_eq!(frame.charged_work, 3);
}

#[test]
fn original_memory_alu_encodings_preserve_operand_order_carry_and_aliases() {
    use doslet_x86_wasm::static_program::memory::{
        Descriptor, Effects, RamDescriptor, SegmentBounds,
    };
    for (bytes, expected_eax, expected_flags) in [
        (vec![0x03, 0x05, 0, 5, 0, 0], 5, 0x206), // ADD
        (vec![0x13, 0x05, 0, 5, 0, 0], 6, 0x206), // ADC with old CF
        (vec![0x2b, 0x05, 0, 5, 0, 0], 1, 0x202), // SUB
        (vec![0x1b, 0x05, 0, 5, 0, 0], 0, 0x246), // SBB with old CF
        (vec![0x3b, 0x05, 0, 5, 0, 0], 3, 0x202), // CMP reg,mem
        (vec![0x3b, 0x03], 3, 0x202),             // CMP reg,[EBX]
        (vec![0x39, 0x05, 0, 5, 0, 0], 3, 0x297), // CMP mem,reg
        (
            vec![0xf7, 0x05, 0, 5, 0, 0, 0xff, 0xff, 0xff, 0xff],
            3,
            0x202,
        ), // TEST
        (vec![0x83, 0x3d, 0, 5, 0, 0, 0xff], 3, 0x217), // sign-extended CMP
        (vec![0x02, 0x25, 0, 5, 0, 0], 0x203, 0x202), // ADD AH,[mem]
        (vec![0x66, 0x1b, 0x05, 0, 5, 0, 0], 0, 0x246), // SBB AX,[mem]
    ] {
        let mut code = bytes.clone();
        code.push(0xf4);
        let image = image(&code, DefaultBitness::Bits32);
        let binding = binding(&image, 0x1000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let prepared = PreparedProgram::from_bound_cfg(
            &graph,
            &binding,
            LoweringLimits {
                direct_memory: true,
                ..LoweringLimits::default()
            },
        )
        .unwrap();
        assert_eq!(prepared.report().memory_alu, 1, "{bytes:02x?}");
        let initial = [3, 0, 0, 0x500, 0, 0, 0, 0];
        let (frame, calls) = execute_setup(
            &prepared,
            Frame {
                gpr: initial,
                eflags: 0x203,
                address: address(at(0)),
                remaining_budget: 1,
                ..Frame::default()
            },
            CompileOptions {
                direct_memory: true,
                ..CompileOptions::default()
            },
            |memory| {
                memory[4096..4100].copy_from_slice(&2u32.to_le_bytes());
                Descriptor {
                    ram: RamDescriptor {
                        pointer: 4096,
                        linear_base: 0x500,
                        length: 4,
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
                .write(&mut memory[72..])
                .unwrap();
            },
            |memory| {
                assert_eq!(&memory[4096..4100], &2u32.to_le_bytes());
                let effects = Effects::read(&memory[72..]).unwrap();
                assert_eq!(effects.ram_writes, 0);
                assert_eq!(effects.device_reads, 0);
            },
        );
        let mut expected = initial;
        expected[0] = expected_eax;
        assert_eq!(frame.gpr, expected, "{bytes:02x?}");
        assert_eq!(frame.eflags, expected_flags, "{bytes:02x?}");
        assert_eq!(frame.charged_work, 1);
        assert_eq!(frame.address, address(at(bytes.len() as u32)));
        assert!(calls.is_empty());
    }
}

#[test]
fn original_ram_update_encodings_store_results_and_preserve_unary_carry_rules() {
    use doslet_x86_wasm::static_program::memory::{
        Descriptor, Effects, RamDescriptor, SegmentBounds,
    };
    for (bytes, expected_memory, expected_flags) in [
        (vec![0x01, 0x05, 0, 5, 0, 0], 5u32, 0x206),
        (vec![0x11, 0x05, 0, 5, 0, 0], 6, 0x206),
        (vec![0xff, 0x05, 0, 5, 0, 0], 3, 0x207),
        (vec![0xff, 0x0d, 0, 5, 0, 0], 1, 0x203),
        (vec![0xf7, 0x1d, 0, 5, 0, 0], 0xffff_fffe, 0x293),
        (vec![0xf7, 0x15, 0, 5, 0, 0], 0xffff_fffd, 0x203),
        (vec![0x83, 0x05, 0, 5, 0, 0, 0xff], 1, 0x213),
        (vec![0xfe, 0x0d, 0, 5, 0, 0], 1, 0x203),
        (vec![0x66, 0xff, 0x05, 0, 5, 0, 0], 3, 0x207),
    ] {
        let mut code = bytes.clone();
        code.push(0xf4);
        let image = image(&code, DefaultBitness::Bits32);
        let binding = binding(&image, 0x1000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let prepared = PreparedProgram::from_bound_cfg(
            &graph,
            &binding,
            LoweringLimits {
                direct_memory: true,
                ..LoweringLimits::default()
            },
        )
        .unwrap();
        assert_eq!(prepared.report().memory_updates, 1, "{bytes:02x?}");
        let initial = [3, 0, 0, 0x500, 0, 0, 0, 0];
        let (frame, calls) = execute_setup(
            &prepared,
            Frame {
                gpr: initial,
                eflags: 0x203,
                address: address(at(0)),
                remaining_budget: 1,
                ..Frame::default()
            },
            CompileOptions {
                direct_memory: true,
                ..CompileOptions::default()
            },
            |memory| {
                memory[4096..4100].copy_from_slice(&2u32.to_le_bytes());
                Descriptor {
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
                .write(&mut memory[72..])
                .unwrap();
            },
            |memory| {
                assert_eq!(
                    &memory[4096..4100],
                    &expected_memory.to_le_bytes(),
                    "{bytes:02x?}"
                );
                assert_eq!(&memory[6000..6008], &1u64.to_le_bytes());
                assert_eq!(Effects::read(&memory[72..]).unwrap().ram_writes, 1);
            },
        );
        assert_eq!(frame.gpr, initial, "{bytes:02x?}");
        assert_eq!(frame.eflags, expected_flags, "{bytes:02x?}");
        assert_eq!(frame.charged_work, 1);
        assert_eq!(frame.address, address(at(bytes.len() as u32)));
        assert!(calls.is_empty());
    }
}

#[test]
fn stack_decoding_uses_operand_width_and_keeps_unimplemented_forms_explicit() {
    for (bitness, bytes, expected) in [
        (
            32,
            vec![0x50],
            Operation::Push {
                width: Width::Dword,
                source: Source::Register(Register::Eax),
            },
        ),
        (
            32,
            vec![0x66, 0x50],
            Operation::Push {
                width: Width::Word,
                source: Source::Register(Register::Ax),
            },
        ),
        (
            16,
            vec![0x50],
            Operation::Push {
                width: Width::Word,
                source: Source::Register(Register::Ax),
            },
        ),
        (
            16,
            vec![0x66, 0x50],
            Operation::Push {
                width: Width::Dword,
                source: Source::Register(Register::Eax),
            },
        ),
        (
            32,
            vec![0x67, 0x54],
            Operation::Push {
                width: Width::Dword,
                source: Source::Register(Register::Esp),
            },
        ),
        (
            32,
            vec![0x6a, 0x80],
            Operation::Push {
                width: Width::Dword,
                source: Source::Immediate(0xffff_ff80),
            },
        ),
        (
            32,
            vec![0x66, 0x6a, 0x80],
            Operation::Push {
                width: Width::Word,
                source: Source::Immediate(0xff80),
            },
        ),
        (
            16,
            vec![0x6a, 0xff],
            Operation::Push {
                width: Width::Word,
                source: Source::Immediate(0xffff),
            },
        ),
        (
            32,
            vec![0x68, 0x78, 0x56, 0x34, 0x12],
            Operation::Push {
                width: Width::Dword,
                source: Source::Immediate(0x1234_5678),
            },
        ),
        (
            16,
            vec![0x68, 0x78, 0x56],
            Operation::Push {
                width: Width::Word,
                source: Source::Immediate(0x5678),
            },
        ),
        (
            32,
            vec![0x58],
            Operation::Pop {
                target: Register::Eax,
            },
        ),
        (
            16,
            vec![0x58],
            Operation::Pop {
                target: Register::Ax,
            },
        ),
        (
            32,
            vec![0x66, 0x58],
            Operation::Pop {
                target: Register::Ax,
            },
        ),
        (
            32,
            vec![0x8f, 0xc4],
            Operation::Pop {
                target: Register::Esp,
            },
        ),
    ] {
        let instruction = Decoder::new(bitness, &bytes, DecoderOptions::NONE).decode();
        assert_eq!(
            stack_operation(&instruction),
            Some(expected),
            "{bitness}: {bytes:02x?}"
        );
    }
    for bytes in [
        vec![0xff, 0x30], // PUSH [EAX]
        vec![0x8f, 0x00], // POP [EAX]
        vec![0x06],       // PUSH ES
        vec![0x0f, 0xa0], // PUSH FS
        vec![0x9c],       // PUSHFD
        vec![0x60],       // PUSHAD
        vec![0x61],       // POPAD
    ] {
        let instruction = Decoder::new(32, &bytes, DecoderOptions::NONE).decode();
        assert_eq!(stack_operation(&instruction), None, "{bytes:02x?}");
    }
}

#[test]
fn original_lea_encodings_compute_offsets_without_a_memory_descriptor() {
    for (bits, bytes, target, value) in [
        (
            DefaultBitness::Bits32,
            vec![0x8d, 0x44, 0x88, 0xf0],
            0,
            0x8765_4331,
        ),
        (
            DefaultBitness::Bits32,
            vec![0x66, 0x8d, 0x54, 0x88, 0xf0],
            2,
            0x1111_4331,
        ),
        (DefaultBitness::Bits32, vec![0x67, 0x8d, 0x07], 0, 5),
        (DefaultBitness::Bits32, vec![0x67, 0x8d, 0x00], 0, 0x15),
        (
            DefaultBitness::Bits32,
            vec![0x64, 0x8d, 0x00],
            0,
            0x8765_4321,
        ),
        (
            DefaultBitness::Bits32,
            vec![0x8d, 0x15, 0xf0, 0xff, 0xff, 0xff],
            2,
            0xffff_fff0,
        ),
        (
            DefaultBitness::Bits32,
            vec![0x66, 0x67, 0x8d, 0x16, 0xff, 0xff],
            2,
            0x1111_ffff,
        ),
        (
            DefaultBitness::Bits32,
            vec![0x8d, 0x04, 0x8d, 0xf0, 0xff, 0xff, 0xff],
            0,
            0x10,
        ),
        (
            DefaultBitness::Bits32,
            vec![0x67, 0x8d, 0x40, 0xff],
            0,
            0x14,
        ),
        (DefaultBitness::Bits16, vec![0x8d, 0x00], 0, 0x8765_0015),
        (DefaultBitness::Bits16, vec![0x66, 0x8d, 0x00], 0, 0x15),
        (
            DefaultBitness::Bits16,
            vec![0x67, 0x8d, 0x44, 0x88, 0xf0],
            0,
            0x8765_4331,
        ),
        (
            DefaultBitness::Bits16,
            vec![0x66, 0x67, 0x8d, 0x54, 0x88, 0xf0],
            2,
            0x8765_4331,
        ),
    ] {
        let mut code = bytes.clone();
        code.push(0xf4);
        let image = image(&code, bits);
        let binding = binding(&image, 0x1000);
        let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
        let prepared =
            PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
        assert_eq!(prepared.report().register_instructions, 1);
        let mut expected = [
            0x8765_4321,
            8,
            0x1111_2222,
            0x1234_0005,
            0,
            0,
            0xffff_0010,
            0,
        ];
        let (frame, calls) = execute(
            &prepared,
            Frame {
                gpr: expected,
                eflags: 0x8d7,
                address: address(at(0)),
                remaining_budget: 1,
                ..Frame::default()
            },
        );
        expected[target] = value;
        assert_eq!(frame.gpr, expected, "{bits:?} {bytes:02x?}");
        assert_eq!(frame.eflags, 0x8d7);
        assert_eq!(frame.charged_work, 1);
        assert_eq!(frame.address, address(at(bytes.len() as u32)));
        assert!(calls.is_empty());
    }
}

#[test]
fn original_push_pop_encodings_use_live_stack_mode_and_preserve_flags() {
    use doslet_x86_wasm::static_program::memory::{
        Descriptor, Effects, RamDescriptor, SegmentBounds, StackAddress,
    };
    for stack_address in [StackAddress::Bits16, StackAddress::Bits32] {
        let initial_sp = if stack_address == StackAddress::Bits16 {
            0xabcd_0504
        } else {
            0x504
        };
        for (bytes, register_index, result, final_sp) in [
            (vec![0x50, 0x5b], 3, 0x1234_5678, initial_sp),
            (vec![0x66, 0x50, 0x66, 0x5b], 3, 0x9876_5678, initial_sp),
            (vec![0x6a, 0x80, 0x58], 0, 0xffff_ff80, initial_sp),
            (vec![0x67, 0x54, 0x5a], 2, initial_sp, initial_sp),
            (vec![0x50, 0x5c], 4, 0x1234_5678, 0x1234_5678),
        ] {
            let mut code = bytes.clone();
            code.push(0xf4);
            let image = image(&code, DefaultBitness::Bits32);
            let binding = binding(&image, 0x1000);
            let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
            let prepared = PreparedProgram::from_bound_cfg(
                &graph,
                &binding,
                LoweringLimits {
                    direct_memory: true,
                    ..LoweringLimits::default()
                },
            )
            .unwrap();
            assert_eq!(prepared.report().stack_operations, 2, "{bytes:02x?}");
            let mut expected = [0x1234_5678, 0, 0, 0x9876_abcd, initial_sp, 0, 0, 0];
            let (frame, calls) = execute_setup(
                &prepared,
                Frame {
                    gpr: expected,
                    eflags: 0xad7,
                    address: address(at(0)),
                    remaining_budget: 2,
                    ..Frame::default()
                },
                CompileOptions {
                    direct_memory: true,
                    ..CompileOptions::default()
                },
                |memory| {
                    Descriptor {
                        stack_address,
                        ram: RamDescriptor {
                            pointer: 4096,
                            linear_base: 0x500,
                            length: 16,
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
                    .write(&mut memory[72..])
                    .unwrap();
                },
                |memory| {
                    assert_eq!(&memory[6000..6008], &1u64.to_le_bytes());
                    assert_eq!(Effects::read(&memory[72..]).unwrap().ram_writes, 1);
                    assert_eq!(Effects::read(&memory[72..]).unwrap().device_reads, 0);
                },
            );
            expected[4] = final_sp;
            expected[register_index] = result;
            assert_eq!(frame.gpr, expected, "{stack_address:?} {bytes:02x?}");
            assert_eq!(frame.eflags, 0xad7);
            assert_eq!(frame.charged_work, 2);
            assert_eq!(frame.address, address(at(bytes.len() as u32)));
            assert!(calls.is_empty());
        }
    }
}

fn execute(prepared: &PreparedProgram, frame: Frame) -> (Frame, Vec<(u32, Address)>) {
    execute_setup(prepared, frame, CompileOptions::default(), |_| {}, |_| {})
}

fn execute_setup(
    prepared: &PreparedProgram,
    frame: Frame,
    options: CompileOptions,
    setup: impl FnOnce(&mut [u8]),
    inspect: impl FnOnce(&[u8]),
) -> (Frame, Vec<(u32, Address)>) {
    execute_setup_with_aliases(prepared, frame, options, &[], setup, inspect)
}

fn execute_setup_with_aliases(
    prepared: &PreparedProgram,
    frame: Frame,
    options: CompileOptions,
    aliases: &[doslet_x86_wasm::static_program::EntryAlias],
    setup: impl FnOnce(&mut [u8]),
    inspect: impl FnOnce(&[u8]),
) -> (Frame, Vec<(u32, Address)>) {
    let bytes = prepared
        .program()
        .compile_with_aliases(&options, aliases)
        .unwrap();
    let engine = Engine::default();
    let module = Module::new(&engine, bytes.wasm()).unwrap();
    let mut store = Store::new(&engine, Vec::<(u32, Address)>::new());
    let memory = Memory::new(&mut store, MemoryType::new(1, None)).unwrap();
    let mut linker = Linker::new(&engine);
    linker
        .define(abi::IMPORT_MODULE, abi::MEMORY_IMPORT, memory)
        .unwrap();
    linker
        .func_wrap(
            abi::IMPORT_MODULE,
            abi::RUNTIME_IMPORT,
            |mut caller: Caller<'_, Vec<(u32, Address)>>, pointer: i32, reason: i32| -> i32 {
                let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                let frame = Frame::read(&memory.data(&caller)[pointer as usize..]).unwrap();
                caller.data_mut().push((reason as u32, frame.address));
                abi::EXIT as i32
            },
        )
        .unwrap();
    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    frame.write(memory.data_mut(&mut store)).unwrap();
    setup(memory.data_mut(&mut store));
    let run = instance
        .get_typed_func::<i32, i32>(&store, abi::RUN_EXPORT)
        .unwrap();
    run.call(&mut store, 0).unwrap();
    let output = Frame::read(memory.data(&store)).unwrap();
    inspect(memory.data(&store));
    (output, store.data().clone())
}

#[test]
fn original_mov_memory_bytes_lower_to_direct_loads_and_stores_with_address_overrides() {
    use doslet_x86_wasm::static_program::memory::{
        Descriptor, OFFSET, RamDescriptor, SegmentBounds,
    };
    let code = [
        0xbe, 0, 0x11, 0, 0, // mov esi,1100h
        0x8b, 0x06, // mov eax,[esi]
        0x8a, 0x66, 1, // mov ah,[esi+1]
        0x66, 0x8b, 0x56, 2, // mov dx,[esi+2]
        0x67, 0x8a, 0x40, 8, // mov al,[bx+si+8]
        0x64, 0x8b, 0x0e, // mov ecx,fs:[esi]
        0x66, 0x89, 0x46, 4, // mov [esi+4],ax
        0xc6, 0x46, 6, 0x7a, // mov byte [esi+6],7ah
        0x89, 0x56, 7, // mov [esi+7],edx
        0xc3,
    ];
    let image = image(&code, DefaultBitness::Bits32);
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let prepared = PreparedProgram::from_bound_cfg(
        &graph,
        &binding,
        LoweringLimits {
            direct_memory: true,
            ..LoweringLimits::default()
        },
    )
    .unwrap();
    assert_eq!(prepared.report().memory_loads, 5);
    assert_eq!(prepared.report().memory_stores, 3);
    let frame = Frame {
        address: address(at(0)),
        remaining_budget: 100,
        gpr: [0, 0, 0xdeadbeef, 0, 0, 0, 0, 0],
        ..Frame::default()
    };
    let (frame, callbacks) = execute_setup(
        &prepared,
        frame,
        CompileOptions {
            direct_memory: true,
            ..CompileOptions::default()
        },
        |bytes| {
            let mut descriptor = Descriptor {
                ram: RamDescriptor {
                    pointer: 0x1000,
                    linear_base: 0x500,
                    length: 0x2000,
                    generations_pointer: 0x4000,
                    generations_length: 32,
                    code_pages_pointer: 0x5000,
                    code_pages_length: 32,
                    ..RamDescriptor::default()
                },
                windows: [[SegmentBounds {
                    valid: true,
                    base: 0,
                    first: 0,
                    last: u32::MAX,
                }; 6]; 2],
                ..Descriptor::default()
            };
            descriptor.windows[0][4].base = 0x100;
            descriptor.write(&mut bytes[OFFSET as usize..]).unwrap();
            bytes[0x1c00..0x1c0c].copy_from_slice(&[
                0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc,
            ]);
            bytes[0x1d00..0x1d04].copy_from_slice(&[0xa1, 0xb2, 0xc3, 0xd4]);
        },
        |bytes| {
            assert_eq!(
                &bytes[0x1c00..0x1c0c],
                &[
                    0x11, 0x22, 0x33, 0x44, 0x99, 0x22, 0x7a, 0x33, 0x44, 0xad, 0xde, 0xcc
                ]
            );
            assert_eq!(&bytes[0x4008..0x4010], &3u64.to_le_bytes());
        },
    );
    assert_eq!(frame.gpr[0], 0x44332299);
    assert_eq!(frame.gpr[1], 0xd4c3b2a1);
    assert_eq!(frame.gpr[2], 0xdead4433);
    assert_eq!(frame.charged_work, 9);
    assert_eq!(
        callbacks,
        [(abi::REASON_FALLBACK, address(at(code.len() as u32 - 1)))]
    );
    assert_eq!(image.objects()[0].bytes(), &code);
}

#[test]
fn real_decode_lowering_executes_byte_word_and_dword_registers() {
    // MOV EAX,89abcdef; MOV AH,12; MOV AX,fffe; ADD AX,3; INC AL; RET
    let image = image(
        &[
            0xb8, 0xef, 0xcd, 0xab, 0x89, 0xb4, 0x12, 0x66, 0xb8, 0xfe, 0xff, 0x66, 0x83, 0xc0, 3,
            0xfe, 0xc0, 0xc3,
        ],
        DefaultBitness::Bits32,
    );
    let binding = binding(&image, 0x5432_0000);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let prepared =
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
    let (frame, callbacks) = execute(
        &prepared,
        Frame {
            address: address(at(0)),
            remaining_budget: 100,
            ..Frame::default()
        },
    );
    assert_eq!(frame.gpr[0], 0x89ab_0002);
    assert_eq!(frame.charged_work, 5);
    assert_eq!(frame.address, address(at(17)));
    assert_eq!(callbacks, [(abi::REASON_FALLBACK, address(at(17)))]);
    assert_eq!(prepared.report().fallback_instructions, 1);
}

#[test]
fn patched_immediate_comes_from_binding_not_original_bytes_or_profile() {
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
    let binding = binding(&image, 0x7654_0000);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let prepared =
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
    assert_eq!(image.objects()[0].bytes()[1..5], [0; 4]);
    let (frame, _) = execute(
        &prepared,
        Frame {
            address: address(at(0)),
            remaining_budget: 100,
            ..Frame::default()
        },
    );
    assert_eq!(frame.gpr[0], 0x7654_0005);
    assert_eq!(frame.charged_work, 1);
}

#[test]
fn unsupported_memory_instruction_stops_before_effects() {
    // INC EBX; MOV EAX,[EBX]; INC ECX; RET
    let image = image(&[0x43, 0x8b, 3, 0x41, 0xc3], DefaultBitness::Bits32);
    let binding = binding(&image, 0x1000);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let prepared =
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
    let (frame, _) = execute(
        &prepared,
        Frame {
            address: address(at(0)),
            remaining_budget: 100,
            ..Frame::default()
        },
    );
    assert_eq!(frame.gpr, [0, 0, 0, 1, 0, 0, 0, 0]);
    assert_eq!(frame.address, address(at(1)));
    assert_eq!(frame.charged_work, 1);
    assert!(
        prepared
            .program()
            .regions
            .iter()
            .any(|region| region.entry == address(at(3)))
    );
}

#[test]
fn direct_loop_stays_generated_and_soft_budget_finishes_region() {
    // INC AL; CMP AL,5; JNE 0; RET
    let image = image(
        &[0xfe, 0xc0, 0x3c, 5, 0x75, 0xfa, 0xc3],
        DefaultBitness::Bits32,
    );
    let binding = binding(&image, 0x1234_0000);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let prepared =
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
    let (frame, callbacks) = execute(
        &prepared,
        Frame {
            address: address(at(0)),
            remaining_budget: 4,
            ..Frame::default()
        },
    );
    assert_eq!(frame.gpr[0], 2);
    assert_eq!(frame.charged_work, 6);
    assert_eq!(frame.remaining_budget, 0);
    assert!(callbacks.is_empty());
}

#[test]
fn code16_binding_resolves_targets_without_inventing_new_code() {
    // JMP 4; unvisited bytes; INC AX; RET (entry explicitly trusted at4).
    let image = image(&[0xeb, 2, 0, 0, 0x40, 0xc3], DefaultBitness::Bits16);
    let binding = binding(&image, 0x4321_0000);
    let graph = StaticCfg::analyze(&image, &[at(4)], CfgLimits::default()).unwrap();
    let prepared =
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
    let (frame, _) = execute(
        &prepared,
        Frame {
            address: address(at(0)),
            remaining_budget: 100,
            gpr: [0x1234_ffff, 0, 0, 0, 0, 0, 0, 0],
            ..Frame::default()
        },
    );
    assert_eq!(frame.gpr[0], 0x1234_0000);
    assert_eq!(frame.charged_work, 2);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let prepared =
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
    assert_eq!(prepared.report().external_stubs, 1);
    assert_eq!(prepared.report().register_instructions, 0);
    let (frame, _) = execute(
        &prepared,
        Frame {
            address: address(at(0)),
            remaining_budget: 100,
            ..Frame::default()
        },
    );
    assert_eq!(frame.address, address(at(4)));
    assert_eq!(frame.charged_work, 1);
}

#[test]
fn bound_target_splits_a_previously_sequential_discovered_stream() {
    // code16 near jump unresolved in unbound CFG; trusted entry4 reaches5.
    let image = image(&[0xeb, 3, 0, 0, 0x40, 0x41, 0xc3], DefaultBitness::Bits16);
    let binding = binding(&image, 0x2000);
    let graph = StaticCfg::analyze(&image, &[at(4)], CfgLimits::default()).unwrap();
    let prepared =
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
    assert!(
        prepared
            .program()
            .regions
            .iter()
            .any(|region| region.entry == address(at(5)))
    );
    let (frame, _) = execute(
        &prepared,
        Frame {
            address: address(at(0)),
            remaining_budget: 100,
            ..Frame::default()
        },
    );
    assert_eq!(frame.gpr[0], 0);
    assert_eq!(frame.gpr[1], 1);
}

#[test]
fn mismatched_images_and_resource_limits_are_explicit() {
    let image = image(&[0x90, 0xc3], DefaultBitness::Bits32);
    let other = self::image(image.objects()[0].bytes(), DefaultBitness::Bits32);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let other_binding = binding(&other, 0x1000);
    assert_eq!(
        PreparedProgram::from_bound_cfg(&graph, &other_binding, LoweringLimits::default())
            .unwrap_err(),
        LoweringError::DifferentImage
    );
    let binding = binding(&image, 0x1000);
    assert_eq!(
        PreparedProgram::from_bound_cfg(
            &graph,
            &binding,
            LoweringLimits {
                max_owned_bytes: 0,
                ..LoweringLimits::default()
            }
        )
        .unwrap_err(),
        LoweringError::OwnedByteLimit
    );
}

#[test]
fn watcom_overlap_executes_each_instruction_identity() {
    // JZ MOV; DEC EDI; TEST AX,cf89; RET. MOV EDI,ECX starts
    // inside TEST's immediate, as in the Watcom memchr intrinsic.
    let image = image(
        &[0x74, 3, 0x4f, 0x66, 0xa9, 0x89, 0xcf, 0xc3],
        DefaultBitness::Bits32,
    );
    let binding = binding(&image, 0x1000);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    assert_eq!(graph.overlaps().len(), 1);
    let prepared =
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
    for (flags, expected, work) in [(2, 0x122, 3), (0x42, 0, 2)] {
        let (frame, _) = execute(
            &prepared,
            Frame {
                address: address(at(0)),
                remaining_budget: 100,
                gpr: [0xff, 0, 0, 0, 0, 0, 0, 0x123],
                eflags: flags,
                ..Frame::default()
            },
        );
        assert_eq!(frame.gpr[7], expected);
        assert_eq!(frame.charged_work, work);
        assert_eq!(frame.address, address(at(7)));
    }
}

#[test]
fn bound_relocated_jump_splits_an_existing_stream_and_uses_patched_target() {
    let mut image = image(
        &[0xe9, 0, 0, 0, 0, 0x40, 0x90, 0x43, 0xc3],
        DefaultBitness::Bits32,
    );
    image.sites.push(Site {
        source: at(1),
        kind: LeFixupSourceType::SelfRelative32,
        target: RelocationTarget {
            object_number: 1,
            offset: Some(7),
        },
        is_alias: false,
        provenance: 0..0,
    });
    let binding = binding(&image, 0x7654_0000);
    let graph = StaticCfg::analyze(&image, &[at(5)], CfgLimits::default()).unwrap();
    let prepared =
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
    let (frame, _) = execute(
        &prepared,
        Frame {
            address: address(at(0)),
            remaining_budget: 100,
            ..Frame::default()
        },
    );
    assert_eq!(frame.gpr[0], 0);
    assert_eq!(frame.gpr[3], 1);
    assert_eq!(frame.charged_work, 2);
    assert_eq!(prepared.report().external_stubs, 0);
}

#[test]
fn code16_sequential_ip_wrap_preserves_word_alias_and_returns_actual_next_ip() {
    let mut code = vec![0x90; 65536];
    code[0] = 0xf4;
    code[65535] = 0x40;
    let image = image(&code, DefaultBitness::Bits16);
    let binding = binding(&image, 0x2000);
    let graph = StaticCfg::analyze(&image, &[at(65535)], CfgLimits::default()).unwrap();
    let prepared =
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
    let (frame, callbacks) = execute(
        &prepared,
        Frame {
            address: address(at(65535)),
            remaining_budget: 1,
            gpr: [0x1234_ffff, 0, 0, 0, 0, 0, 0, 0],
            ..Frame::default()
        },
    );
    assert_eq!(frame.gpr[0], 0x1234_0000);
    assert_eq!(frame.address, address(at(0)));
    assert_eq!(frame.charged_work, 1);
    assert!(callbacks.is_empty());
}

#[test]
fn guidance_splits_tier_transitions_preserves_metadata_and_requires_hot_opt_in() {
    use crate::profile::{BoundGuidance, Context, GuidanceLimits, Profile, ProfileLimits, Tier};
    use serde_json::json;

    let image = image(
        &[0x40, 0x43, 0x41, 0x42, 0x46, 0xc3],
        DefaultBitness::Bits32,
    );
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let binding = binding(&image, 0x1000);
    let mut rows = Vec::new();
    for (index, (&byte, tier)) in image.objects()[0]
        .bytes()
        .iter()
        .zip(["fast", "slow", "grey", "irq", "hot", "grey"])
        .enumerate()
    {
        let mut bytes = [0; 15];
        bytes[0] = byte;
        rows.push(
            json!({"pc":0x1000 + index, "bits":32,"bytes":bytes,"tier":tier,
            "count":100 + index,"high_count":2 + index}),
        );
    }
    let bytes = serde_json::to_vec(
        &json!({"version":1,"unknown":"grey","irq_policy":"static-grey","nodes":rows}),
    )
    .unwrap();
    let profile = Profile::parse(&bytes, ProfileLimits::default()).unwrap();
    let guidance =
        BoundGuidance::bind(&profile, &graph, &binding, None, GuidanceLimits::default()).unwrap();
    for uncounted_hot in [false, true] {
        let prepared = PreparedProgram::from_guided_cfg(
            &graph,
            &binding,
            &guidance,
            uncounted_hot,
            LoweringLimits::default(),
        )
        .unwrap();
        assert_eq!(prepared.program().regions.len(), 6);
        assert_eq!(prepared.guidance(), guidance.instructions());
        assert_eq!(prepared.guidance()[1].tier, Tier::Cold);
        assert_eq!(prepared.guidance()[2].tier, Tier::Grey);
        assert_eq!(prepared.guidance()[3].tier, Tier::Irq);
        assert_eq!(prepared.guidance()[3].context, Context::IrqObserved);
        assert_eq!(prepared.guidance()[4].weight.count, Some(104));
        for region in &prepared.program().regions {
            assert_eq!(
                region.accounting,
                if uncounted_hot && matches!(region.entry.offset, 0 | 4) {
                    Accounting::UncountedHot
                } else {
                    Accounting::Counted
                }
            );
        }
        let (frame, _) = execute(
            &prepared,
            Frame {
                address: address(at(0)),
                remaining_budget: 100,
                ..Frame::default()
            },
        );
        assert_eq!(frame.gpr, [1, 1, 1, 1, 0, 0, 1, 0]);
        assert_eq!(frame.charged_work, if uncounted_hot { 3 } else { 5 });
        assert_eq!(frame.address, address(at(5)));
    }
    let original =
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
    assert!(
        original
            .program()
            .regions
            .iter()
            .all(|region| region.accounting == Accounting::Counted)
    );
    assert!(
        original
            .guidance()
            .iter()
            .all(|instruction| instruction.tier == Tier::Grey)
    );
    // Equal declarations do not allow evidence verified for another plan or
    // CFG instance to be silently reused.
    let other_binding = self::binding(&image, 0x1000);
    assert_eq!(
        PreparedProgram::from_guided_cfg(
            &graph,
            &other_binding,
            &guidance,
            true,
            LoweringLimits::default()
        )
        .unwrap_err(),
        LoweringError::DifferentGuidance
    );
    let other_graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    assert_eq!(
        PreparedProgram::from_guided_cfg(
            &other_graph,
            &binding,
            &guidance,
            true,
            LoweringLimits::default()
        )
        .unwrap_err(),
        LoweringError::DifferentGuidance
    );
}

#[test]
fn conflicting_hot_labels_and_interrupt_observations_remain_counted() {
    use crate::profile::{BoundGuidance, GuidanceLimits, Profile, ProfileLimits, Tier};
    use serde_json::json;
    let image = image(&[0x40, 0x43, 0x41, 0x42, 0xc3], DefaultBitness::Bits32);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let binding = binding(&image, 0x1000);
    let mut rows = Vec::new();
    for (offset, byte, tier, reason) in [
        (0, 0x40, "hot", "learned"),
        (0, 0x40, "cold", "learned"),
        (1, 0x43, "hot", "interrupt-frame"),
        (2, 0x41, "hot", "code-conflict"),
        (3, 0x40, "hot", "learned"),
    ]
    // Wrong actual byte at offset 3.
    {
        let mut bytes = [0; 15];
        bytes[0] = byte;
        rows.push(
            json!({"pc":0x1000 + offset,"bits":32,"bytes":bytes,"tier":tier,"reason":reason}),
        );
    }
    let profile = Profile::parse(
        &serde_json::to_vec(
            &json!({"version":1,"unknown":"grey","irq_policy":"static-grey","nodes":rows}),
        )
        .unwrap(),
        ProfileLimits::default(),
    )
    .unwrap();
    let guidance =
        BoundGuidance::bind(&profile, &graph, &binding, None, GuidanceLimits::default()).unwrap();
    let prepared = PreparedProgram::from_guided_cfg(
        &graph,
        &binding,
        &guidance,
        true,
        LoweringLimits::default(),
    )
    .unwrap();
    assert_eq!(prepared.guidance()[1].tier, Tier::Irq);
    assert!(
        prepared
            .program()
            .regions
            .iter()
            .all(|region| region.accounting == Accounting::Counted)
    );
    let (frame, _) = execute(
        &prepared,
        Frame {
            address: address(at(0)),
            remaining_budget: 100,
            ..Frame::default()
        },
    );
    assert_eq!(frame.gpr, [1, 1, 1, 1, 0, 0, 0, 0]);
    assert_eq!(frame.charged_work, 4);
}
