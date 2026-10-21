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
fn pc24_discovery_compiles_balanced_integer_load_register_arithmetic_loop() {
    // FILD [EAX]; FLD ST0; FMUL ST0,ST1; FADDP ST1,ST0;
    // FISTP [EDI]; DEC ECX; JNZ entry; RET.
    let image = image(
        &[
            0xdb, 0x00, 0xd9, 0xc0, 0xd8, 0xc9, 0xde, 0xc1, 0xdb, 0x1f, 0x49, 0x75, 0xf3, 0xc3,
        ],
        DefaultBitness::Bits32,
    );
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let limits = MemoryProcedureLimits {
        max_work_per_call: Some(128),
        ..Default::default()
    };
    let legacy =
        PreparedMemoryProcedures::from_bound_graph_with_long_fragments(&graph, &binding, limits, 1)
            .unwrap();
    assert!(
        legacy
            .program()
            .functions
            .iter()
            .flat_map(|f| &f.blocks)
            .flat_map(|b| &b.instructions)
            .all(|i| !matches!(i.operation, Operation::X87(_)))
    );
    let prepared =
        PreparedMemoryProcedures::from_bound_graph_with_pc24_fragments(&graph, &binding, limits, 1)
            .unwrap();
    let function = prepared
        .program()
        .functions
        .iter()
        .find(|f| f.entry.offset == 0x10000)
        .unwrap();
    assert_eq!(
        function
            .blocks
            .iter()
            .flat_map(|b| &b.instructions)
            .filter(|i| matches!(i.operation, Operation::X87(_)))
            .count(),
        5
    );
    let compiled = prepared
        .program()
        .compile_with_pc24_locals_and_work_limits(&ProcedureCompileOptions::default(), 128, 4096)
        .unwrap();
    let masks = compiled.x87(function.id).unwrap();
    assert_eq!(masks.read_mask, 0);
    assert_eq!(masks.write_mask, 0xc0);
    assert!(compiled.finite_maximum_work(function.id).is_none());
}

#[test]
fn pc24_lowering_preserves_fop_reversed_arithmetic_and_memory_address() {
    use doslet_x86_wasm::static_program::X87Operation as X;
    for (code, expected) in [
        (
            [0xd8, 0xe1],
            X::ArithmeticRegister {
                index: 1,
                operation: 4,
                target_top: true,
                pop: false,
            },
        ),
        (
            [0xdc, 0xe1],
            X::ArithmeticRegister {
                index: 1,
                operation: 5,
                target_top: false,
                pop: false,
            },
        ),
        (
            [0xde, 0xe9],
            X::ArithmeticRegister {
                index: 1,
                operation: 4,
                target_top: false,
                pop: true,
            },
        ),
        (
            [0xde, 0xf1],
            X::ArithmeticRegister {
                index: 1,
                operation: 7,
                target_top: false,
                pop: true,
            },
        ),
        ([0xd8, 0xd7], X::CompareRegister { index: 7, pops: 0 }),
        ([0xd8, 0xd9], X::CompareRegister { index: 1, pops: 1 }),
        ([0xde, 0xd9], X::CompareRegister { index: 1, pops: 2 }),
        ([0xdf, 0xe0], X::StoreStatusAx),
        (
            [0xdd, 0xd3],
            X::StoreRegister {
                index: 3,
                pop: false,
            },
        ),
        (
            [0xdd, 0xd8],
            X::StoreRegister {
                index: 0,
                pop: true,
            },
        ),
        (
            [0xdd, 0xdb],
            X::StoreRegister {
                index: 3,
                pop: true,
            },
        ),
    ] {
        let image = image(&[code[0], code[1], 0xc3], DefaultBitness::Bits32);
        let binding = binding(&image, 0x10000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let node = &graph.nodes()[0];
        let (decoded, offsets, ip) = decode_node_with_offsets(node, &graph, &binding).unwrap();
        let result = super::super::x87::lower(
            &decoded,
            &offsets,
            ip,
            node,
            &graph,
            &binding,
            &Default::default(),
        )
        .unwrap();
        assert_eq!(
            result,
            Operation::X87(doslet_x86_wasm::static_program::X87Instruction {
                opcode: (u16::from(code[0] & 7) << 8) | u16::from(code[1]),
                operation: expected,
            })
        );
    }
    let image = image(
        &[0x36, 0xdb, 0x44, 0x8d, 0xf0, 0xc3],
        DefaultBitness::Bits32,
    );
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let node = &graph.nodes()[0];
    let (decoded, offsets, ip) = decode_node_with_offsets(node, &graph, &binding).unwrap();
    let Operation::X87(instruction) = super::super::x87::lower(
        &decoded,
        &offsets,
        ip,
        node,
        &graph,
        &binding,
        &Default::default(),
    )
    .unwrap() else {
        panic!("x87 lowering")
    };
    let X::LoadInteger32 { address } = instruction.operation else {
        panic!("FILD32 lowering")
    };
    assert_eq!(address.segment, memory::Segment::Ss);
    assert_eq!(address.base, Some(Register::Ebp));
    assert_eq!(address.index, Some(Register::Ecx));
    assert_eq!(address.scale, 4);
    assert_eq!(address.displacement, (-16i32) as u32);
}

#[test]
fn pc24_compare_lowering_preserves_memory_address_and_rejects_other_forms() {
    use doslet_x86_wasm::static_program::X87Operation as X;
    for (code, expected_pop) in [
        (vec![0x36, 0xd8, 0x54, 0x8d, 0xf0, 0xc3], Some(false)),
        (vec![0x36, 0xd8, 0x5c, 0x8d, 0xf0, 0xc3], Some(true)),
        // Ordered real64 and unordered comparisons remain interpreter work.
        (vec![0xdc, 0x10, 0xc3], None),
        (vec![0xdd, 0xe1, 0xc3], None),
        (vec![0xda, 0xe9, 0xc3], None),
        // FNSTSW memory and FNSTCW memory have distinct effects.
        (vec![0xdd, 0x38, 0xc3], None),
        (vec![0xd9, 0x38, 0xc3], None),
    ] {
        let image = image(&code, DefaultBitness::Bits32);
        let binding = binding(&image, 0x10000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let node = &graph.nodes()[0];
        let (decoded, offsets, ip) = decode_node_with_offsets(node, &graph, &binding).unwrap();
        let result = super::super::x87::lower(
            &decoded,
            &offsets,
            ip,
            node,
            &graph,
            &binding,
            &Default::default(),
        );
        if let Some(expected_pop) = expected_pop {
            let Some(Operation::X87(instruction)) = result else {
                panic!("missing compare: {code:x?}")
            };
            let X::CompareReal32 { address, pop } = instruction.operation else {
                panic!("unexpected compare: {instruction:?}")
            };
            assert_eq!(pop, expected_pop);
            assert_eq!(address.segment, memory::Segment::Ss);
            assert_eq!(address.base, Some(Register::Ebp));
            assert_eq!(address.index, Some(Register::Ecx));
            assert_eq!(address.scale, 4);
            assert_eq!(address.displacement, (-16i32) as u32);
            assert_eq!(instruction.opcode, u16::from(code[2]));
        } else {
            assert!(result.is_none(), "unexpectedly lowered {code:x?}");
        }
    }
}

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
fn bounded_fragments_require_an_explicit_work_limit_and_keep_legacy_admission() {
    // PUSH ES; loop: CMP byte [EAX],0; JZ done; SUB byte [EAX],1;
    // MOV [EDI],ECX; JMP loop; done: POP ES; RET.
    let image = image(
        &[
            0x06, 0x80, 0x38, 0, 0x74, 7, 0x80, 0x28, 1, 0x89, 0x0f, 0xeb, 0xf4, 0x07, 0xc3,
        ],
        DefaultBitness::Bits32,
    );
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    assert!(matches!(
        PreparedMemoryProcedures::from_bound_graph_with_bounded_fragments(
            &graph,
            &binding,
            MemoryProcedureLimits::default(),
        ),
        Err(crate::wasm::memory_procedures::MemoryProcedureError::InputLimit)
    ));
    let limits = MemoryProcedureLimits {
        max_work_per_call: Some(24),
        ..Default::default()
    };
    let legacy =
        PreparedMemoryProcedures::from_bound_graph_with_computations(&graph, &binding, limits)
            .unwrap();
    assert!(!legacy.entries().contains(&at(1)));
    let prepared =
        PreparedMemoryProcedures::from_bound_graph_with_bounded_fragments(&graph, &binding, limits)
            .unwrap();
    assert!(
        PreparedMemoryProcedures::from_bound_graph_with_long_fragments(
            &graph, &binding, limits, 32,
        )
        .unwrap()
        .entries()
        .contains(&at(1))
    );
    let index = prepared
        .entries()
        .iter()
        .position(|&entry| entry == at(1))
        .unwrap();
    let function = &prepared.program().functions[index];
    assert!(function.finite_maximum_work().is_err());
    assert!(function.blocks.iter().any(|block| matches!(
        block.terminator.kind,
        rle_api::procedure_memory::ProcedureTerminatorKind::Exit
    )));
    let compiled = prepared
        .program()
        .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 24)
        .unwrap();
    assert_eq!(compiled.maximum_work(function.id), Some(24));
    assert_eq!(compiled.finite_maximum_work(function.id), None);
    assert_eq!(compiled.semantic_kernel(function.id), None);
    assert_eq!(compiled.rle_kernel(function.id), None);

    let engine = Engine::default();
    let module = Module::new(&engine, compiled.wasm()).unwrap();
    let mut store = Store::new(&engine, ());
    let memory = Memory::new(&mut store, MemoryType::new(2, Some(2))).unwrap();
    let mut linker = Linker::new(&engine);
    linker
        .define(abi::IMPORT_MODULE, abi::MEMORY_IMPORT, memory)
        .unwrap();
    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    let callable = instance
        .get_typed_func::<u32, u32>(&store, compiled.export_name(function.id).unwrap())
        .unwrap();
    for initial in 0u8..=3 {
        for grant in 0..=27u32 {
            for protected_page in [None, Some(4usize), Some(5)] {
                let bytes = memory.data_mut(&mut store);
                bytes.fill(0);
                let frame = Frame {
                    gpr: [0x4000, 0x12345678, 0, 0, 0x7000, 0, 0, 0x5000],
                    eflags: 0x203,
                    address: Address {
                        space: 0,
                        offset: 0x10001,
                    },
                    ..Default::default()
                };
                frame.write(bytes).unwrap();
                Descriptor {
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
                    inline_grant: grant,
                    ..Default::default()
                }
                .write(&mut bytes[memory::OFFSET as usize..])
                .unwrap();
                bytes[0x14000] = initial;
                if let Some(page) = protected_page {
                    bytes[0x280 + page] = 1;
                }

                // Independent instruction-boundary model for this tiny loop.
                // Values stay in 0..=3, so these are the complete CMP/SUB flags
                // (IF is preserved; CF/OF/AF clear; parity only for zero/three).
                let mut ip = 1;
                let mut counter = initial;
                let mut flags = frame.eflags;
                let mut work = 0u64;
                let mut updates = 0u64;
                let mut stores = 0u64;
                while work < u64::from(grant.min(24)) && ip != 13 {
                    match ip {
                        1 => {
                            flags = match counter {
                                0 => 0x246,
                                3 => 0x206,
                                _ => 0x202,
                            };
                            ip = 4;
                        }
                        4 => ip = if counter == 0 { 13 } else { 6 },
                        6 => {
                            if protected_page == Some(4) {
                                break;
                            }
                            counter -= 1;
                            flags = if counter == 0 { 0x246 } else { 0x202 };
                            updates += 1;
                            ip = 9;
                        }
                        9 => {
                            if protected_page == Some(5) {
                                break;
                            }
                            stores += 1;
                            ip = 11;
                        }
                        11 => ip = 1,
                        _ => unreachable!(),
                    }
                    work += 1;
                }
                assert_eq!(
                    callable.call(&mut store, 0).unwrap(),
                    rle_api::procedure_memory::SIDE_EXIT
                );
                let bytes = memory.data(&store);
                let actual = Frame::read(bytes).unwrap();
                assert_eq!(actual.gpr, frame.gpr);
                assert_eq!(actual.eflags, flags);
                assert_eq!(actual.address.offset, 0x10000 + ip);
                assert_eq!(actual.charged_work, work);
                assert_eq!(bytes[0x14000], counter);
                let output = if stores == 0 { 0u32 } else { frame.gpr[1] };
                assert_eq!(&bytes[0x15000..0x15004], &output.to_le_bytes());
                for (page, expected) in [(4, updates), (5, stores)] {
                    let start = 0x200 + page * 8;
                    assert_eq!(
                        u64::from_le_bytes(bytes[start..start + 8].try_into().unwrap()),
                        expected
                    );
                }
                let start = (memory::OFFSET + memory::RAM_WRITES) as usize;
                assert_eq!(
                    u64::from_le_bytes(bytes[start..start + 8].try_into().unwrap()),
                    updates + stores
                );
            }
        }
    }
}

#[test]
fn bounded_fragments_reject_tiny_prefixes_internal_diamonds_and_multiple_cycles() {
    for code in [
        &[0x40, 0x07, 0xc3][..], // Acyclic INC; unsupported POP ES.
        &[0x40, 0x75, 0xfd, 0x48, 0x75, 0xfd, 0x07, 0xc3], // Two loops.
        // loop: TEST EAX,EAX; JZ merge; INC EBX; merge: DEC ECX; JNZ loop;
        // unsupported POP ES; RET. The loop contains an internal diamond.
        &[0x85, 0xc0, 0x74, 1, 0x43, 0x49, 0x75, 0xf8, 0x07, 0xc3],
    ] {
        let image = image(code, DefaultBitness::Bits32);
        let binding = binding(&image, 0x10000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let prepared = PreparedMemoryProcedures::from_bound_graph_with_bounded_fragments(
            &graph,
            &binding,
            MemoryProcedureLimits {
                max_work_per_call: Some(24),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!prepared.entries().contains(&at(0)));
        assert!(
            prepared
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.entry == at(0)
                    && diagnostic.reason == MemoryProcedureRejection::UnsupportedLoop)
        );
    }
}

fn long_fragment_code(count: usize, terminal: &[u8]) -> Vec<u8> {
    let mut code = Vec::new();
    for _ in 1..count {
        code.extend_from_slice(&[0x89, 0xc8]);
    } // MOV EAX,ECX.
    code.extend_from_slice(&[0x89, 0x07]); // MOV [EDI],EAX.
    code.extend_from_slice(terminal);
    code.push(0xc3);
    code
}

#[test]
fn long_fragments_validate_threshold_and_preserve_legacy_admission() {
    for terminal in [&[0xd9, 0xe8][..], &[0xe8, 0, 0, 0, 0]] {
        // FLD1 / CALL next.
        for count in [31, 32] {
            let code = long_fragment_code(count, terminal);
            let image = image(&code, DefaultBitness::Bits32);
            let binding = binding(&image, 0x10000);
            let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
            let limits = MemoryProcedureLimits {
                max_work_per_call: Some(1),
                ..Default::default()
            };
            for minimum in [0, 4097, u32::MAX] {
                assert!(matches!(
                    PreparedMemoryProcedures::from_bound_graph_with_long_fragments(
                        &graph, &binding, limits, minimum,
                    ),
                    Err(crate::wasm::memory_procedures::MemoryProcedureError::InputLimit)
                ));
            }
            assert!(
                PreparedMemoryProcedures::from_bound_graph_with_long_fragments(
                    &graph,
                    &binding,
                    MemoryProcedureLimits::default(),
                    32,
                )
                .is_err()
            );
            for legacy in [
                PreparedMemoryProcedures::from_bound_graph(&graph, &binding, limits).unwrap(),
                PreparedMemoryProcedures::from_bound_graph_with_computations(
                    &graph, &binding, limits,
                )
                .unwrap(),
                PreparedMemoryProcedures::from_bound_graph_with_bounded_fragments(
                    &graph, &binding, limits,
                )
                .unwrap(),
            ] {
                assert!(!legacy.entries().contains(&at(0)));
            }
            let prepared = PreparedMemoryProcedures::from_bound_graph_with_long_fragments(
                &graph, &binding, limits, 32,
            )
            .unwrap();
            let index = prepared.entries().iter().position(|entry| *entry == at(0));
            assert_eq!(index.is_some(), count == 32);
            if let Some(index) = index {
                let function = &prepared.program().functions[index];
                let compiled = prepared
                    .program()
                    .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 1)
                    .unwrap();
                // Acyclic reservations use the complete longest path, not the
                // cyclic quota. No finite/atomic computation is selected.
                assert_eq!(compiled.maximum_work(function.id), Some(32));
                assert_eq!(compiled.finite_maximum_work(function.id), None);
                assert_eq!(compiled.semantic_kernel(function.id), None);
                assert_eq!(compiled.rle_kernel(function.id), None);
            }
        }
    }
}

#[test]
fn long_fragments_dag_paths_and_store_refusals_exit_before_x87_or_call() {
    for terminal in [&[0xd9, 0xe8][..], &[0xe8, 0, 0, 0, 0]] {
        // TEST ECX,ECX; JZ exit; 29 MOV EAX,ECX; MOV [EDI],EAX;
        // exit: FLD1 / CALL next; RET. Short path costs 2, longest costs 32.
        let mut code = vec![0x85, 0xc9, 0x74, 60];
        code.extend_from_slice(&long_fragment_code(30, terminal));
        let image = image(&code, DefaultBitness::Bits32);
        let binding = binding(&image, 0x10000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let prepared = PreparedMemoryProcedures::from_bound_graph_with_long_fragments(
            &graph,
            &binding,
            MemoryProcedureLimits {
                max_work_per_call: Some(1),
                ..Default::default()
            },
            32,
        )
        .unwrap();
        let index = prepared
            .entries()
            .iter()
            .position(|entry| *entry == at(0))
            .unwrap();
        let function = &prepared.program().functions[index];
        let compiled = prepared
            .program()
            .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 1)
            .unwrap();
        assert_eq!(compiled.maximum_work(function.id), Some(32));
        assert_eq!(compiled.finite_maximum_work(function.id), None);
        let engine = Engine::default();
        let module = Module::new(&engine, compiled.wasm()).unwrap();
        let mut store = Store::new(&engine, ());
        let memory = Memory::new(&mut store, MemoryType::new(2, Some(2))).unwrap();
        let mut linker = Linker::new(&engine);
        linker
            .define(abi::IMPORT_MODULE, abi::MEMORY_IMPORT, memory)
            .unwrap();
        let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
        let callable = instance
            .get_typed_func::<u32, u32>(&store, compiled.export_name(function.id).unwrap())
            .unwrap();
        for input in [0, 1] {
            for protected in [false, true] {
                let bytes = memory.data_mut(&mut store);
                bytes.fill(0);
                let frame = Frame {
                    gpr: [0xdeadbeef, input, 0, 0, 0x7000, 0, 0, 0x5000],
                    eflags: 0x202,
                    address: Address {
                        space: 0,
                        offset: 0x10000,
                    },
                    remaining_budget: 7,
                    charged_work: 11,
                    ..Default::default()
                };
                frame.write(bytes).unwrap();
                Descriptor {
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
                    inline_grant: 32, // The owner reserves the full acyclic maximum.
                    ..Default::default()
                }
                .write(&mut bytes[memory::OFFSET as usize..])
                .unwrap();
                bytes[0x17000..0x17004].copy_from_slice(&0x23456u32.to_le_bytes());
                if protected {
                    bytes[0x285] = 1;
                }
                assert_eq!(
                    callable.call(&mut store, 0).unwrap(),
                    rle_api::procedure_memory::SIDE_EXIT
                );
                let bytes = memory.data(&store);
                let actual = Frame::read(bytes).unwrap();
                let refused = input != 0 && protected;
                let work = if input == 0 {
                    2
                } else if refused {
                    31
                } else {
                    32
                };
                let mut expected = frame;
                if input != 0 {
                    expected.gpr[0] = input;
                }
                expected.eflags = if input == 0 { 0x246 } else { 0x202 };
                expected.charged_work += work;
                expected.address.offset += if refused { 62 } else { 64 };
                assert_eq!(actual, expected); // Includes unchanged ESP and frame budget.
                assert_eq!(&bytes[0x17000..0x17004], &0x23456u32.to_le_bytes());
                let written = u32::from(input != 0 && !protected);
                assert_eq!(&bytes[0x15000..0x15004], &written.to_le_bytes());
                assert_eq!(
                    u64::from_le_bytes(bytes[0x228..0x230].try_into().unwrap()),
                    u64::from(written)
                );
                let start = (memory::OFFSET + memory::RAM_WRITES) as usize;
                assert_eq!(
                    u64::from_le_bytes(bytes[start..start + 8].try_into().unwrap()),
                    u64::from(written)
                );
            }
        }
    }
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
