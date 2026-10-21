use super::*;
use crate::wasm::memory_procedures::{
    MemoryProcedureLimits, MemoryProcedureRejection, PreparedMemoryProcedures,
};
use doslet_x86_wasm::static_program as rle_api;
use doslet_x86_wasm::static_program::{
    memory::{self, Descriptor, RamDescriptor, SegmentBounds, StackAddress},
    procedure_memory::{ProcedureCompileOptions, ProcedureId},
};
#[path = "../../../../../tests/fixtures/rle_procedure.rs"]
mod rle_fixture;

#[test]
fn computation_discovery_preserves_whole_procedures_jumping_to_lower_addresses() {
    // CALL thunk; RET; body: INC EAX; RET; NOP; NOP; thunk: JMP body.
    let image = image(
        &[0xe8, 5, 0, 0, 0, 0xc3, 0x40, 0xc3, 0x90, 0x90, 0xeb, 0xfa],
        DefaultBitness::Bits32,
    );
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared = PreparedMemoryProcedures::from_bound_graph_with_computations(
        &graph,
        &binding,
        MemoryProcedureLimits::default(),
    )
    .unwrap();
    assert!(prepared.entries().contains(&at(10)));
}

#[test]
fn backward_branch_to_shared_epilogue_is_not_mistaken_for_a_loop_header() {
    let mut code = [0x90; 45];
    code[..11].copy_from_slice(&[0xe8, 21, 0, 0, 0, 0xe8, 32, 0, 0, 0, 0xc3]);
    code[16..18].copy_from_slice(&[0x40, 0xc3]);
    code[26..30].copy_from_slice(&[0x31, 0xc0, 0xeb, 0xf2]);
    code[42..45].copy_from_slice(&[0x75, 0xee, 0xc3]);
    let image = image(&code, DefaultBitness::Bits32);
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared = PreparedMemoryProcedures::from_bound_graph_with_computations(
        &graph,
        &binding,
        MemoryProcedureLimits::default(),
    )
    .unwrap();
    assert!(prepared.entries().contains(&at(26)));
}

#[test]
fn finite_regions_are_discovered_after_segment_setup_without_entry_hints() {
    // PUSH ES; MOV ECX,3; LODS; TEST AL,1; JZ store; XOR AL,55h;
    // store: STOS; DEC CL; JNZ loop; POP ES; RET.
    let image = image(
        &[
            0x06, 0xb9, 3, 0, 0, 0, 0xac, 0xa8, 1, 0x74, 2, 0x34, 0x55, 0xaa, 0xfe, 0xc9, 0x75,
            0xf4, 0x07, 0xc3,
        ],
        DefaultBitness::Bits32,
    );
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared = PreparedMemoryProcedures::from_bound_graph_with_computations(
        &graph,
        &binding,
        MemoryProcedureLimits::default(),
    )
    .unwrap();
    let index = prepared
        .entries()
        .iter()
        .position(|entry| *entry == at(1))
        .unwrap();
    let function = &prepared.program().functions[index];
    assert!(function.blocks.iter().any(|block| matches!(
        block.terminator.kind,
        doslet_x86_wasm::static_program::procedure_memory::ProcedureTerminatorKind::Exit
    )));
    let compiled = prepared
        .program()
        .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 4096)
        .unwrap();
    assert!(compiled.finite_maximum_work(function.id).is_some());
    assert!(prepared.sources(index).unwrap().contains(&at(6)));
    assert!(!prepared.sources(index).unwrap().contains(&at(0)));
}

#[test]
fn unchanged_external_variable_wait_is_not_a_finite_computational_fragment() {
    // PUSH ES; CMP byte [EAX],0; JNZ CMP; POP ES; RET.
    let image = image(
        &[0x06, 0x80, 0x38, 0, 0x75, 0xfb, 0x07, 0xc3],
        DefaultBitness::Bits32,
    );
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared = PreparedMemoryProcedures::from_bound_graph_with_computations(
        &graph,
        &binding,
        MemoryProcedureLimits::default(),
    )
    .unwrap();
    assert!(!prepared.entries().contains(&at(1)));
}

#[test]
fn computation_pass_recognizes_full_rle_from_decoded_source_without_relaxing_other_cycles() {
    let fixture = rle_fixture::rle_fixture(ProcedureId(9), 0, 0);
    assert!(fixture.function.recognize_rle_kernel().is_some());
    let image = image(&fixture.code, DefaultBitness::Bits32);
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared = PreparedMemoryProcedures::from_bound_graph_with_computations(
        &graph,
        &binding,
        MemoryProcedureLimits::default(),
    )
    .unwrap();
    let index = prepared
        .entries()
        .iter()
        .position(|entry| *entry == at(0))
        .unwrap();
    let id = prepared.program().functions[index].id;
    let compiled = prepared
        .program()
        .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 4096)
        .unwrap();
    assert!(compiled.rle_kernel(id).is_some());
    assert_eq!(compiled.maximum_work(id), Some(u64::from(u32::MAX)));

    // Two separate cyclic regions remain outside the ordinary linear-loop
    // compiler: enabling computations must not admit an arbitrary graph.
    let image = super::image(
        &[0x40, 0x75, 0xfd, 0x48, 0x75, 0xfd, 0xc3],
        DefaultBitness::Bits32,
    );
    let binding = super::binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let rejected = PreparedMemoryProcedures::from_bound_graph_with_computations(
        &graph,
        &binding,
        MemoryProcedureLimits::default(),
    )
    .unwrap();
    assert!(!rejected.entries().contains(&at(0)));
    assert!(
        rejected
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.entry == at(0)
                && diagnostic.reason == MemoryProcedureRejection::UnsupportedLoop)
    );
}

#[test]
fn bound_original_memory_diamond_executes_both_paths_with_real_stack_return() {
    // PUSH EBP; MOV EBP,ESP; MOV EAX,[ESI]; TEST AL,1; JZ even;
    // XOR EAX,EDX; JMP merge; even: NEG EAX; merge: MOV [EDI],EAX; POP EBP; RET.
    let code = [
        0x55, 0x89, 0xe5, 0x8b, 0x06, 0xa8, 1, 0x74, 4, 0x31, 0xd0, 0xeb, 2, 0xf7, 0xd8, 0x89,
        0x07, 0x5d, 0xc3,
    ];
    let image = image(&code, DefaultBitness::Bits32);
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared = PreparedMemoryProcedures::from_bound_graph(
        &graph,
        &binding,
        MemoryProcedureLimits::default(),
    )
    .unwrap();
    assert!(prepared.diagnostics().is_empty());
    assert_eq!(prepared.entries(), &[at(0)]);
    assert_eq!(prepared.sources(0).unwrap().len(), 11);
    let compiled = prepared
        .program()
        .compile(&ProcedureCompileOptions::default())
        .unwrap();
    assert_eq!(compiled.maximum_work(ProcedureId(0)), Some(10));
    let engine = Engine::default();
    let module = Module::new(&engine, compiled.wasm()).unwrap();
    assert_eq!(module.imports().count(), 1);
    let mut store = Store::new(&engine, ());
    let memory = Memory::new(&mut store, MemoryType::new(2, Some(2))).unwrap();
    let mut linker = Linker::new(&engine);
    linker
        .define(abi::IMPORT_MODULE, abi::MEMORY_IMPORT, memory)
        .unwrap();
    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    let function = instance
        .get_typed_func::<u32, u32>(&store, compiled.export_name(ProcedureId(0)).unwrap())
        .unwrap();
    let descriptor = Descriptor {
        ram: RamDescriptor {
            pointer: 0x10500,
            linear_base: 0x500,
            length: 0xfb00,
            generations_pointer: 0x200,
            generations_length: 16,
            code_pages_pointer: 0x280,
            code_pages_length: 16,
            ..Default::default()
        },
        stack_address: StackAddress::Bits32,
        windows: [[SegmentBounds {
            valid: true,
            base: 0,
            first: 0,
            last: u32::MAX,
        }; 6]; 2],
        ..Default::default()
    };
    for value in [0_u32, 1, 2, 3, 0xffff_ffff, 0x8000_0000] {
        let bytes = memory.data_mut(&mut store);
        bytes.fill(0);
        let frame = Frame {
            gpr: [0, 0, 0x12345678, 0, 0x7000, 0x99887766, 0x4000, 0x5000],
            address: Address {
                space: 0,
                offset: 0x10000,
            },
            ..Default::default()
        };
        frame.write(bytes).unwrap();
        descriptor
            .write(&mut bytes[memory::OFFSET as usize..])
            .unwrap();
        bytes[0x14000..0x14004].copy_from_slice(&value.to_le_bytes());
        bytes[0x17000..0x17004].copy_from_slice(&0x23456_u32.to_le_bytes());
        assert_eq!(function.call(&mut store, 0).unwrap(), 0);
        let bytes = memory.data(&store);
        let actual = Frame::read(bytes).unwrap();
        let expected = if value & 1 == 0 {
            value.wrapping_neg()
        } else {
            value ^ 0x12345678
        };
        assert_eq!(actual.gpr[0], expected);
        assert_eq!(actual.gpr[4], 0x7004);
        assert_eq!(actual.gpr[5], 0x99887766);
        assert_eq!(
            actual.address,
            Address {
                space: 0,
                offset: 0x23456
            }
        );
        assert_eq!(actual.charged_work, if value & 1 == 0 { 9 } else { 10 });
        assert_eq!(&bytes[0x15000..0x15004], &expected.to_le_bytes());
    }
}

#[test]
fn memory_procedure_pass_rejects_cycles_unknown_calls_and_unbound_images() {
    for (code, expected) in [
        (
            &[0x8b, 0x06, 0x85, 0xc0, 0x74, 0xfa, 0xc3][..],
            MemoryProcedureRejection::Cycle,
        ),
        (
            &[0xe8, 0, 0, 0, 0, 0xc3][..],
            MemoryProcedureRejection::Call,
        ),
        (
            &[0xff, 0xe0][..],
            MemoryProcedureRejection::UnsupportedInstruction,
        ),
    ] {
        let image = image(code, DefaultBitness::Bits32);
        let binding = binding(&image, 0x10000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let prepared = PreparedMemoryProcedures::from_bound_graph(
            &graph,
            &binding,
            MemoryProcedureLimits::default(),
        )
        .unwrap();
        assert!(
            prepared
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.entry == at(0) && diagnostic.reason == expected)
        );
        let unbound = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
        assert!(
            PreparedMemoryProcedures::from_bound_graph(
                &unbound,
                &binding,
                MemoryProcedureLimits::default()
            )
            .is_err()
        );
    }
}

#[test]
fn memory_procedure_pass_checks_storage_scan_and_graph_limits() {
    let image = image(&[0x40, 0xc3], DefaultBitness::Bits32);
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    for limits in [
        MemoryProcedureLimits {
            max_owned_bytes: 0,
            ..Default::default()
        },
        MemoryProcedureLimits {
            max_scan_steps: 0,
            ..Default::default()
        },
        MemoryProcedureLimits {
            max_input_edges: 0,
            ..Default::default()
        },
        MemoryProcedureLimits {
            max_procedures: 0,
            ..Default::default()
        },
        MemoryProcedureLimits {
            max_nodes_per_procedure: 257,
            ..Default::default()
        },
        MemoryProcedureLimits {
            max_total_nodes: 1,
            ..Default::default()
        },
        MemoryProcedureLimits {
            max_work_per_call: Some(0),
            ..Default::default()
        },
        MemoryProcedureLimits {
            max_work_per_call: Some(4097),
            ..Default::default()
        },
    ] {
        assert!(PreparedMemoryProcedures::from_bound_graph(&graph, &binding, limits).is_err());
    }
    let prepared = PreparedMemoryProcedures::from_bound_graph(
        &graph,
        &binding,
        MemoryProcedureLimits {
            max_nodes_per_procedure: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(prepared.program().functions.is_empty());
    assert_eq!(
        prepared.diagnostics()[0].reason,
        MemoryProcedureRejection::InstructionBound
    );
}

#[test]
fn bounded_counted_copy_plans_resume_root_and_executes_conditional_entry_and_exit() {
    // MOV EBX,ECX; SHR EBX,1; JZ single;
    // loop: MOV AX,[ESI]; MOV [EDI],AL; ADD ESI,2; ADD EDI,192;
    // MOV [EDI-96],AH; DEC EBX; JNZ loop;
    // AND ECX,1; JZ done; single: MOV AL,[ESI]; MOV [EDI],AL; done: RET.
    let code = [
        0x89, 0xcb, 0xd1, 0xeb, 0x74, 0x19, 0x66, 0x8b, 0x06, 0x88, 0x07, 0x83, 0xc6, 2, 0x81,
        0xc7, 0xc0, 0, 0, 0, 0x88, 0x67, 0xa0, 0x4b, 0x75, 0xec, 0x83, 0xe1, 1, 0x74, 4, 0x8a,
        0x06, 0x88, 0x07, 0xc3,
    ];
    let image = image(&code, DefaultBitness::Bits32);
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared = PreparedMemoryProcedures::from_bound_graph(
        &graph,
        &binding,
        MemoryProcedureLimits {
            max_work_per_call: Some(64),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(prepared.diagnostics().is_empty());
    assert_eq!(prepared.entries(), &[at(0), at(6)]);
    assert_eq!(prepared.max_work_per_call(), Some(64));
    assert_eq!(prepared.sources(0).unwrap().len(), 15);
    assert_eq!(prepared.sources(1).unwrap().len(), 12);
    assert!(
        prepared
            .program()
            .compile(&ProcedureCompileOptions::default())
            .is_err()
    );
    let compiled = prepared
        .program()
        .compile_with_work_limit(&ProcedureCompileOptions::default(), 64)
        .unwrap();
    assert_eq!(compiled.maximum_work(ProcedureId(0)), Some(64));
    assert_eq!(compiled.maximum_work(ProcedureId(1)), Some(64));
    let engine = Engine::default();
    let module = Module::new(&engine, compiled.wasm()).unwrap();
    assert_eq!(module.imports().count(), 1);
    let mut store = Store::new(&engine, ());
    let memory = Memory::new(&mut store, MemoryType::new(2, Some(2))).unwrap();
    let mut linker = Linker::new(&engine);
    linker
        .define(abi::IMPORT_MODULE, abi::MEMORY_IMPORT, memory)
        .unwrap();
    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    let function = instance
        .get_typed_func::<u32, u32>(&store, compiled.export_name(ProcedureId(0)).unwrap())
        .unwrap();
    let descriptor = Descriptor {
        ram: RamDescriptor {
            pointer: 0x10500,
            linear_base: 0x500,
            length: 0xfb00,
            generations_pointer: 0x200,
            generations_length: 16,
            code_pages_pointer: 0x280,
            code_pages_length: 16,
            ..Default::default()
        },
        stack_address: StackAddress::Bits32,
        windows: [[SegmentBounds {
            valid: true,
            base: 0,
            first: 0,
            last: u32::MAX,
        }; 6]; 2],
        inline_grant: 64,
        ..Default::default()
    };
    for pixels in 0..=7 {
        let bytes = memory.data_mut(&mut store);
        bytes.fill(0);
        Frame {
            gpr: [0x12345678, pixels, 0, 0, 0x7000, 0, 0x4000, 0x5000],
            address: Address {
                space: 0,
                offset: 0x10000,
            },
            ..Default::default()
        }
        .write(bytes)
        .unwrap();
        descriptor
            .write(&mut bytes[memory::OFFSET as usize..])
            .unwrap();
        bytes[0x14000..0x14008].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        bytes[0x17000..0x17004].copy_from_slice(&0x23456_u32.to_le_bytes());
        assert_eq!(function.call(&mut store, 0).unwrap(), 0);
        let bytes = memory.data(&store);
        let actual = Frame::read(bytes).unwrap();
        let pairs = pixels / 2;
        assert_eq!(actual.gpr[3], 0);
        assert_eq!(actual.gpr[6], 0x4000 + pairs * 2);
        assert_eq!(actual.gpr[7], 0x5000 + pairs * 192);
        assert_eq!(actual.gpr[4], 0x7004);
        assert_eq!(actual.address.offset, 0x23456);
        assert_eq!(
            actual.charged_work,
            if pairs == 0 {
                6
            } else {
                3 + u64::from(pairs) * 7 + 2 + u64::from(pixels & 1) * 2 + 1
            }
        );
        for pair in 0..pairs as usize {
            assert_eq!(bytes[0x15000 + pair * 192], (pair * 2 + 1) as u8);
            assert_eq!(bytes[0x15000 + pair * 192 + 96], (pair * 2 + 2) as u8);
        }
        if pairs == 0 || pixels & 1 != 0 {
            assert_eq!(bytes[0x15000 + pairs as usize * 192], (pairs * 2 + 1) as u8);
        }
    }
    assert!(
        PreparedMemoryProcedures::from_bound_graph(
            &graph,
            &binding,
            MemoryProcedureLimits {
                max_work_per_call: Some(64),
                max_root_occurrences: 1,
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn bounded_planner_declines_complex_candidate_without_poisoning_other_roots() {
    // JZ +6; +2: JMP +2; +6: JMP +6. The entry reaches two independent
    // loops, while each backward target is itself a supported bounded root.
    let image = image(
        &[0x74, 4, 0xeb, 0xfe, 0xcc, 0xcc, 0xeb, 0xfe],
        DefaultBitness::Bits32,
    );
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared = PreparedMemoryProcedures::from_bound_graph(
        &graph,
        &binding,
        MemoryProcedureLimits {
            max_work_per_call: Some(32),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(prepared.entries(), &[at(2), at(6)]);
    assert_eq!(
        prepared.diagnostics(),
        &[crate::wasm::memory_procedures::MemoryProcedureDiagnostic {
            entry: at(0),
            instruction: at(0),
            reason: MemoryProcedureRejection::UnsupportedLoop,
        }]
    );
    let compiled = prepared
        .program()
        .compile_with_work_limit(&ProcedureCompileOptions::default(), 32)
        .unwrap();
    assert_eq!(compiled.procedure_ids().len(), 2);
    Module::new(&Engine::default(), compiled.wasm()).unwrap();
}

#[test]
fn self_modifying_column_prologue_splits_into_virtual_register_loop_root() {
    // 00: A3 07 00 01 00       MOV [0x10007], EAX      (patches disp32 of MOV AL,[EBX+disp32])
    // 05: 8A 83 00 20 00 00    MOV AL, [EBX + 0x2000]  (loop header at +5, disp32 at +7..+11)
    // 0B: 81 C7 04 00 00 00    ADD EDI, 4              (imm32 at +13..+17)
    // 11: 88 07                MOV [EDI], AL
    // 13: 49                   DEC ECX
    // 14: 75 EF                JNZ +5
    // 16: C3                   RET
    // 17: 89 1D 0D 00 01 00    MOV [0x1000D], EBX      (patches imm32 of ADD EDI,imm32)
    // 1D: C3                   RET
    let code = [
        0xa3, 0x07, 0x00, 0x01, 0x00, // 0..5
        0x8a, 0x83, 0x00, 0x20, 0x00, 0x00, // 5..11
        0x81, 0xc7, 0x04, 0x00, 0x00, 0x00, // 11..17
        0x88, 0x07, // 17..19
        0x49, // 19..20
        0x75, 0xef, // 20..22
        0xc3, // 22..23
        0x89, 0x1d, 0x0d, 0x00, 0x01, 0x00, // 23..29
        0xc3, // 29..30
    ];
    let image = image(&code, DefaultBitness::Bits32);
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[at(0), at(23)], CfgLimits::default()).unwrap();
    let prepared = PreparedMemoryProcedures::from_bound_graph_with_computations(
        &graph,
        &binding,
        MemoryProcedureLimits {
            max_work_per_call: Some(64),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(prepared.entries().contains(&at(5)));
    let func = prepared
        .program()
        .functions
        .iter()
        .find(|f| f.entry.offset == 0x10005)
        .unwrap();
    let has_vreg_disp = func
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .any(|inst| {
            matches!(
                inst.operation,
                Operation::LoadMemory {
                    address,
                    ..
                } if address.scale & memory::VIRTUAL_DISPLACEMENT != 0
                    && address.displacement == 0x10007
            )
        });
    let has_vreg_imm = func
        .blocks
        .iter()
        .flat_map(|b| &b.instructions)
        .any(|inst| {
            matches!(
                inst.operation,
                Operation::Alu {
                    source: Source::VirtualRegister(0x1000d),
                    ..
                }
            )
        });
    assert!(has_vreg_disp);
    assert!(has_vreg_imm);
    let compiled = prepared
        .program()
        .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 64)
        .unwrap();
    Module::new(&Engine::default(), compiled.wasm()).unwrap();
}
