use super::*;
use doslet_x86_wasm::static_program::{X87Instruction, X87Operation};

fn same_fpu(actual: &Option<MemoryProcedureFpu>, expected: &Option<MemoryProcedureFpu>) {
    assert!(
        match (actual, expected) {
            (Some(actual), Some(expected)) => actual.bitwise_eq(expected),
            (None, None) => true,
            _ => false,
        },
        "coprocessor state differs: actual={actual:?}, expected={expected:?}"
    );
}

const X87_CODE: &[u8] = &[
    0x40, // INC EAX: a scalar prefix survives a refused following FLD.
    0xd9, 0x05, 0x00, 0x20, 0, 0, // FLD real32 [2000h].
    0xd8, 0x05, 0x04, 0x20, 0, 0, // FADD real32 [2004h].
    0xdb, 0x1f, // FISTP integer32 [EDI].
    0xc3,
];

fn mixed_program() -> (Vec<u8>, Binding) {
    let compiled = ProcedureGraphProgram {
        functions: vec![ProcedureGraphFunction {
            id: ProcedureId(9),
            entry: address(ENTRY),
            blocks: vec![ProcedureBlock {
                entry: address(ENTRY),
                instructions: vec![
                    ProcedureInstruction {
                        address: address(ENTRY),
                        operation: Operation::Inc(Register::Eax),
                    },
                    ProcedureInstruction {
                        address: address(ENTRY + 1),
                        operation: Operation::X87(X87Instruction {
                            opcode: 0x105,
                            operation: X87Operation::LoadReal32 {
                                address: operand(None, DATA),
                            },
                        }),
                    },
                    ProcedureInstruction {
                        address: address(ENTRY + 7),
                        operation: Operation::X87(X87Instruction {
                            opcode: 0x005,
                            operation: X87Operation::ArithmeticReal32 {
                                address: operand(None, DATA + 4),
                                operation: 0,
                            },
                        }),
                    },
                    ProcedureInstruction {
                        address: address(ENTRY + 13),
                        operation: Operation::X87(X87Instruction {
                            opcode: 0x31f,
                            operation: X87Operation::StoreInteger32 {
                                address: operand(Some(Register::Edi), 0),
                                pop: true,
                            },
                        }),
                    },
                ],
                terminator: ProcedureTerminator {
                    address: address(ENTRY + 15),
                    kind: ProcedureTerminatorKind::Return {
                        width: Width::Dword,
                        pop_bytes: 0,
                    },
                },
            }],
        }],
    }
    .compile_with_pc24_locals_and_work_limits(&ProcedureCompileOptions::default(), 32, 32)
    .unwrap();
    let mut binding = binding();
    binding.x87 = Some(
        compiled
            .x87(ProcedureId(9))
            .expect("compiler x87 permissions"),
    );
    binding.maximum_instructions = compiled.maximum_work(ProcedureId(9)).unwrap() as u32;
    binding.execution_sites = Some(
        [(0, 1), (1, 6), (7, 6), (13, 2), (15, 1)]
            .into_iter()
            .map(|(offset, length)| ExecutionSite {
                ip: ENTRY + offset,
                length,
            })
            .collect(),
    );
    binding.code[0].expected = X87_CODE.to_vec();
    (compiled.into_wasm(), binding)
}

fn mixed_machine() -> PcMachine {
    let mut machine = machine();
    machine
        .bus
        .debug_ram_mut(ENTRY, X87_CODE.len() as u32)
        .unwrap()
        .copy_from_slice(X87_CODE);
    machine.cpu.registers.set32(Gpr32::Edi, DATA + 8);
    machine.bus.write_u32(DATA, 1.5f32.to_bits()).unwrap();
    machine.bus.write_u32(DATA + 4, 2.25f32.to_bits()).unwrap();
    let fpu = machine.cpu.fpu.as_mut().unwrap();
    fpu.load_control(0x007f);
    fpu.push(7.0).unwrap();
    machine
}

#[test]
fn compiled_mixed_x87_prefix_matches_interpreter_on_complete_and_failed_store() {
    let (wasm, binding) = mixed_program();
    for failed_store in [false, true] {
        let mut fast = mixed_machine();
        let mut reference = mixed_machine();
        if failed_store {
            for machine in [&mut fast, &mut reference] {
                machine.cpu.registers.set32(Gpr32::Edi, ENTRY);
            }
        }
        fast.install_wasm_procedures(
            &wasm,
            vec![Binding {
                x87: binding.x87,
                execution_sites: binding.execution_sites.clone(),
                function_id: binding.function_id,
                entry_ip: binding.entry_ip,
                maximum_instructions: binding.maximum_instructions,
                kernel: None,
                rle_kernel: None,
                finite_computation: None,
                kernel_interrupts: KernelInterrupts::Defer,
                code_segment: binding.code_segment,
                code: vec![CodePart {
                    linear: ENTRY,
                    expected: X87_CODE.to_vec(),
                }],
            }],
        )
        .unwrap();
        let work = if failed_store { 3 } else { 5 };
        assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(work));
        reference_steps(&mut reference, work);
        same_state(&mut fast, &mut reference);
        same_fpu(&fast.cpu.fpu, &reference.cpu.fpu);
        assert_eq!(
            fast.wasm_procedure_stats().unwrap().side_exits,
            u64::from(failed_store)
        );
        if !failed_store {
            assert_eq!(fast.bus.read_u32(DATA + 8).unwrap(), 4);
        }
    }
}

#[test]
fn refused_numeric_domain_commits_only_the_completed_scalar_prefix() {
    let (wasm, binding) = mixed_program();
    let mut fast = mixed_machine();
    let mut reference = mixed_machine();
    for machine in [&mut fast, &mut reference] {
        machine
            .bus
            .write_u32(DATA, f32::INFINITY.to_bits())
            .unwrap();
    }
    fast.install_wasm_procedures(&wasm, vec![binding]).unwrap();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(1));
    reference_steps(&mut reference, 1);
    same_state(&mut fast, &mut reference);
    same_fpu(&fast.cpu.fpu, &reference.cpu.fpu);
    assert_eq!(fast.cpu.registers.ip, ENTRY + 1);
}

#[test]
fn explicit_x87_sites_preserve_tier_counts_and_device_time() {
    let (wasm, binding) = mixed_program();
    let mut fast = mixed_machine();
    let mut reference = mixed_machine();
    for machine in [&mut fast, &mut reference] {
        let labels = binding
            .execution_sites
            .as_ref()
            .unwrap()
            .iter()
            .map(|site| doslet_x86::ExecutionLabel {
                address: site.ip,
                code32: true,
                tier: 1,
                bytes: machine
                    .bus
                    .ram_view(site.ip as usize..site.ip as usize + 15)
                    .unwrap()
                    .try_into()
                    .unwrap(),
            })
            .collect();
        machine
            .set_execution_tiers(Some(labels), [Some(133), Some(8), Some(33)])
            .unwrap();
    }
    fast.install_wasm_procedures(&wasm, vec![binding]).unwrap();
    fast.set_wasm_procedure_tier_accounting(true).unwrap();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(5));
    reference_steps(&mut reference, 5);
    super::tier_tests::same_tiered_state(&mut fast, &mut reference);
    same_fpu(&fast.cpu.fpu, &reference.cpu.fpu);
    assert_eq!(fast.cpu.execution_counts(), [0, 5, 0, 0]);
}

#[test]
fn compiled_x87_calls_allocate_nothing_after_setup() {
    let (wasm, binding) = mixed_program();
    let mut machine = mixed_machine();
    machine
        .install_wasm_procedures(&wasm, vec![binding])
        .unwrap();
    assert_eq!(machine.try_wasm_procedure(32).unwrap(), Some(5));
    let allocation = allocation_counter::measure(|| {
        for _ in 0..16 {
            machine.cpu.registers.ip = ENTRY;
            machine.cpu.registers.set32(Gpr32::Esp, STACK);
            assert_eq!(machine.try_wasm_procedure(32).unwrap(), Some(5));
        }
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
}

#[test]
fn balanced_x87_loop_matches_all_completed_interpreter_iterations() {
    let mut code = vec![0xb9, 3, 0, 0, 0]; // MOV ECX,3.
    code.extend_from_slice(&X87_CODE[1..15]); // FLD/FADD/FISTP.
    code.extend_from_slice(&[0x83, 0xc7, 4, 0x49, 0x75, 0xec, 0xc3]);
    let instruction = |offset, operation| ProcedureInstruction {
        address: address(ENTRY + offset),
        operation,
    };
    let compiled = ProcedureGraphProgram {
        functions: vec![ProcedureGraphFunction {
            id: ProcedureId(9),
            entry: address(ENTRY),
            blocks: vec![
                ProcedureBlock {
                    entry: address(ENTRY),
                    instructions: vec![instruction(
                        0,
                        Operation::Move {
                            target: Register::Ecx,
                            source: Source::Immediate(3),
                        },
                    )],
                    terminator: ProcedureTerminator {
                        address: address(ENTRY + 5),
                        kind: ProcedureTerminatorKind::Fallthrough(address(ENTRY + 5)),
                    },
                },
                ProcedureBlock {
                    entry: address(ENTRY + 5),
                    instructions: vec![
                        instruction(
                            5,
                            Operation::X87(X87Instruction {
                                opcode: 0x105,
                                operation: X87Operation::LoadReal32 {
                                    address: operand(None, DATA),
                                },
                            }),
                        ),
                        instruction(
                            11,
                            Operation::X87(X87Instruction {
                                opcode: 0x005,
                                operation: X87Operation::ArithmeticReal32 {
                                    address: operand(None, DATA + 4),
                                    operation: 0,
                                },
                            }),
                        ),
                        instruction(
                            17,
                            Operation::X87(X87Instruction {
                                opcode: 0x31f,
                                operation: X87Operation::StoreInteger32 {
                                    address: operand(Some(Register::Edi), 0),
                                    pop: true,
                                },
                            }),
                        ),
                        instruction(
                            19,
                            Operation::Alu {
                                kind: Alu::Add,
                                target: Register::Edi,
                                source: Source::Immediate(4),
                            },
                        ),
                        instruction(22, Operation::Dec(Register::Ecx)),
                    ],
                    terminator: ProcedureTerminator {
                        address: address(ENTRY + 23),
                        kind: ProcedureTerminatorKind::Branch {
                            condition: Condition::NotEqual,
                            taken: address(ENTRY + 5),
                            not_taken: address(ENTRY + 25),
                        },
                    },
                },
                ProcedureBlock {
                    entry: address(ENTRY + 25),
                    instructions: vec![],
                    terminator: ProcedureTerminator {
                        address: address(ENTRY + 25),
                        kind: ProcedureTerminatorKind::Return {
                            width: Width::Dword,
                            pop_bytes: 0,
                        },
                    },
                },
            ],
        }],
    }
    .compile_with_pc24_locals_and_work_limits(&ProcedureCompileOptions::default(), 32, 32)
    .unwrap();
    let mut fast = mixed_machine();
    let mut reference = mixed_machine();
    for machine in [&mut fast, &mut reference] {
        machine
            .bus
            .debug_ram_mut(ENTRY, code.len() as u32)
            .unwrap()
            .copy_from_slice(&code);
    }
    let mut binding = binding();
    binding.x87 = compiled.x87(ProcedureId(9));
    binding.maximum_instructions = compiled.maximum_work(ProcedureId(9)).unwrap() as u32;
    binding.execution_sites = Some(
        [
            (0, 5),
            (5, 6),
            (11, 6),
            (17, 2),
            (19, 3),
            (22, 1),
            (23, 2),
            (25, 1),
        ]
        .into_iter()
        .map(|(offset, length)| ExecutionSite {
            ip: ENTRY + offset,
            length,
        })
        .collect(),
    );
    binding.code[0].expected = code;
    fast.install_wasm_procedures(compiled.wasm(), vec![binding])
        .unwrap();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(20));
    reference_steps(&mut reference, 20);
    same_state(&mut fast, &mut reference);
    same_fpu(&fast.cpu.fpu, &reference.cpu.fpu);
    assert_eq!(fast.cpu.registers.ip, RETURN);
    for offset in [8, 12, 16] {
        assert_eq!(fast.bus.read_u32(DATA + offset).unwrap(), 4);
    }
}

fn opted_in_scalar() -> PcMachine {
    let mut machine = machine();
    let fpu = machine.cpu.fpu.as_mut().unwrap();
    fpu.load_control(0x007f);
    fpu.push(1.25).unwrap(); // Entry TOP=7 exercises relative-to-physical masks.
    let mut binding = binding();
    binding.x87 = Some(X87Procedure {
        read_mask: 1,
        write_mask: 0,
    });
    binding.execution_sites = Some(
        [(0, 5), (5, 3), (8, 2), (10, 1)]
            .into_iter()
            .map(|(offset, length)| ExecutionSite {
                ip: ENTRY + offset,
                length,
            })
            .collect(),
    );
    machine
        .install_wasm_procedures(&module(), vec![binding])
        .unwrap();
    machine
}

#[test]
fn opted_in_boundary_rotates_permissions_by_entry_top_and_preserves_unmodified_fpu() {
    let mut fast = opted_in_scalar();
    let before = fast.cpu.fpu.clone();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(4));
    same_fpu(&fast.cpu.fpu, &before);
    assert_eq!(fast.cpu.registers.ip, RETURN);
    let Storage::Wasm(owner) = &fast.bus.ram.storage else {
        panic!("shared owner");
    };
    assert!(owner.pending_pc24_boundary().is_none());
}

#[test]
fn unavailable_or_unsupported_x87_admission_declines_without_guest_effects() {
    for refusal in 0..8 {
        let mut machine = opted_in_scalar();
        match refusal {
            0 => machine.cpu.fpu = None,
            1 => machine.cpu.control.cr0 |= 4,
            2 => machine.cpu.control.cr0 |= 8,
            3 => machine.cpu.fpu.as_mut().unwrap().load_control(0x037f),
            4 => machine.cpu.fpu.as_mut().unwrap().load_control(0x027f),
            5 => machine.cpu.fpu.as_mut().unwrap().load_control(0x007e),
            6 => {
                machine.cpu.fpu.as_mut().unwrap().pop().unwrap();
            }
            7 => {
                let fpu = machine.cpu.fpu.as_mut().unwrap();
                fpu.pop().unwrap();
                fpu.push(f64::INFINITY).unwrap();
            }
            _ => unreachable!(),
        }
        let before = machine.cpu.snapshot();
        let control = machine.cpu.control.clone();
        let fpu = machine.cpu.fpu.clone();
        let flags = machine.cpu.flags;
        let data = machine.bus.read_u32(DATA).unwrap();
        assert_eq!(
            machine.try_wasm_procedure(32).unwrap(),
            None,
            "refusal {refusal}"
        );
        assert!(machine.cpu.matches(&before), "refusal {refusal}");
        assert_eq!(machine.cpu.control, control, "refusal {refusal}");
        same_fpu(&machine.cpu.fpu, &fpu);
        assert_eq!(machine.cpu.flags, flags, "refusal {refusal}");
        assert_eq!(
            machine.bus.read_u32(DATA).unwrap(),
            data,
            "refusal {refusal}"
        );
        assert_eq!(machine.wasm_procedure_stats().unwrap().calls, 0);
    }
}

#[test]
fn x87_source_shapes_validate_prefixes_addressing_and_complete_instructions() {
    for (bytes, length, displacement) in [
        (&[0xd9, 0xc1][..], 2, 2..2),
        (&[0xd9, 0x05, 0, 0x20, 0, 0][..], 6, 2..6),
        (&[0x64, 0xd8, 0x44, 0x8d, 0x20][..], 5, 4..5),
        (&[0x67, 0xdb, 0x06, 0, 0x20][..], 5, 3..5),
        (&[0xd8, 0xd1][..], 2, 2..2),
        (&[0xd8, 0x1d, 0, 0x20, 0, 0][..], 6, 2..6),
        (&[0xde, 0xd9][..], 2, 2..2),
        (&[0xdf, 0xe0][..], 2, 2..2),
        (&[0xdd, 0xd8][..], 2, 2..2),
    ] {
        assert_eq!(x87_source_shape(bytes), Some((length, displacement)));
        assert!(x87_source_shape(&bytes[..bytes.len() - 1]).is_none());
    }
    for bytes in [
        &[0xf3, 0xd9, 0xc0][..],
        &[0xd9, 0xd1][..],
        &[0xd8][..],
        &[0xdd, 0xe1][..],
        &[0xdf, 0xe1][..],
        &[0xde, 0xd8][..],
    ] {
        assert!(x87_source_shape(bytes).is_none());
    }
}

fn comparison_branch_program(discard: bool) -> (Vec<u8>, Binding, Vec<u8>) {
    let code = vec![
        0xd8, 0xd1, // FCOM ST1.
        0xdf, 0xe0, // FNSTSW AX.
        0xf6, 0xc4, 1, // TEST AH,1.
        0x74, 7, // JZ greater_or_equal.
        0xbb, 0x11, 0x11, 0x11, 0x11, // MOV EBX,11111111h.
        0xeb, 5, // JMP finish.
        0xbb, 0x22, 0x22, 0x22, 0x22, // MOV EBX,22222222h.
        0xdd, 0xd8, // FSTP ST0: discard the current top.
        0xc3,
    ];
    let instruction = |offset, operation| ProcedureInstruction {
        address: address(ENTRY + offset),
        operation,
    };
    let compiled = ProcedureGraphProgram {
        functions: vec![ProcedureGraphFunction {
            id: ProcedureId(9),
            entry: address(ENTRY),
            blocks: vec![
                ProcedureBlock {
                    entry: address(ENTRY),
                    instructions: vec![
                        instruction(
                            0,
                            Operation::X87(X87Instruction {
                                opcode: 0x0d1,
                                operation: X87Operation::CompareRegister { index: 1, pops: 0 },
                            }),
                        ),
                        instruction(
                            2,
                            Operation::X87(X87Instruction {
                                opcode: 0x7e0,
                                operation: X87Operation::StoreStatusAx,
                            }),
                        ),
                        instruction(
                            4,
                            Operation::Alu {
                                kind: Alu::Test,
                                target: Register::Ah,
                                source: Source::Immediate(1),
                            },
                        ),
                    ],
                    terminator: ProcedureTerminator {
                        address: address(ENTRY + 7),
                        kind: ProcedureTerminatorKind::Branch {
                            condition: Condition::Equal,
                            taken: address(ENTRY + 16),
                            not_taken: address(ENTRY + 9),
                        },
                    },
                },
                ProcedureBlock {
                    entry: address(ENTRY + 9),
                    instructions: vec![instruction(
                        9,
                        Operation::Move {
                            target: Register::Ebx,
                            source: Source::Immediate(0x11111111),
                        },
                    )],
                    terminator: ProcedureTerminator {
                        address: address(ENTRY + 14),
                        kind: ProcedureTerminatorKind::Jump(address(ENTRY + 21)),
                    },
                },
                ProcedureBlock {
                    entry: address(ENTRY + 16),
                    instructions: vec![instruction(
                        16,
                        Operation::Move {
                            target: Register::Ebx,
                            source: Source::Immediate(0x22222222),
                        },
                    )],
                    terminator: ProcedureTerminator {
                        address: address(ENTRY + 21),
                        kind: ProcedureTerminatorKind::Fallthrough(address(ENTRY + 21)),
                    },
                },
                ProcedureBlock {
                    entry: address(ENTRY + 21),
                    instructions: if discard {
                        vec![instruction(
                            21,
                            Operation::X87(X87Instruction {
                                opcode: 0x5d8,
                                operation: X87Operation::StoreRegister {
                                    index: 0,
                                    pop: true,
                                },
                            }),
                        )]
                    } else {
                        vec![]
                    },
                    terminator: ProcedureTerminator {
                        address: address(ENTRY + if discard { 23 } else { 21 }),
                        kind: if discard {
                            ProcedureTerminatorKind::Return {
                                width: Width::Dword,
                                pop_bytes: 0,
                            }
                        } else {
                            ProcedureTerminatorKind::Exit
                        },
                    },
                },
            ],
        }],
    }
    .compile_with_pc24_locals_and_work_limits(&ProcedureCompileOptions::default(), 32, 32)
    .unwrap();
    let mut binding = binding();
    binding.x87 = compiled.x87(ProcedureId(9));
    binding.maximum_instructions = compiled.maximum_work(ProcedureId(9)).unwrap() as u32;
    let mut sites = vec![(0, 2), (2, 2), (4, 3), (7, 2), (9, 5), (14, 2), (16, 5)];
    if discard {
        sites.extend_from_slice(&[(21, 2), (23, 1)]);
    }
    binding.execution_sites = Some(
        sites
            .into_iter()
            .map(|(offset, length)| ExecutionSite {
                ip: ENTRY + offset,
                length,
            })
            .collect(),
    );
    binding.code[0].expected = code.clone();
    (compiled.into_wasm(), binding, code)
}

#[test]
fn compiled_compare_status_test_branch_and_discard_match_original_x86_state() {
    for discard in [false, true] {
        let (wasm, template, code) = comparison_branch_program(discard);
        for (left, right, condition) in [(1.0, 2.0, 0x0100u16), (2.0, 2.0, 0x4000), (3.0, 2.0, 0)] {
            let mut fast = machine();
            let mut reference = machine();
            for machine in [&mut fast, &mut reference] {
                machine
                    .bus
                    .debug_ram_mut(ENTRY, code.len() as u32)
                    .unwrap()
                    .copy_from_slice(&code);
                machine.cpu.registers.set32(Gpr32::Eax, 0xcafe1234);
                machine.cpu.flags = 2 | flags::CARRY | flags::OVERFLOW | flags::DIRECTION;
                let fpu = machine.cpu.fpu.as_mut().unwrap();
                fpu.load_control(0x007f);
                fpu.push(right).unwrap();
                fpu.push(left).unwrap();
                fpu.restore_status(fpu.status() | 0x4700);
                fpu.record_instruction(0x1234, 8, 0x105);
                fpu.record_operand(0x5678, 16);
            }
            let mut live_binding = binding();
            live_binding.x87 = template.x87;
            live_binding.maximum_instructions = template.maximum_instructions;
            live_binding.execution_sites = template.execution_sites.clone();
            live_binding.code[0].expected = code.clone();
            fast.install_wasm_procedures(&wasm, vec![live_binding])
                .unwrap();
            let less = condition == 0x0100;
            let work = if less { 6 } else { 5 } + if discard { 2 } else { 0 };
            assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(work));
            reference_steps(&mut reference, work);
            same_state(&mut fast, &mut reference);
            same_fpu(&fast.cpu.fpu, &reference.cpu.fpu);
            assert_eq!(
                fast.cpu.registers.get32(Gpr32::Eax),
                0xcafe0000 | 0x3000 | u32::from(condition)
            );
            assert_eq!(
                fast.cpu.registers.get32(Gpr32::Ebx),
                if less { 0x11111111 } else { 0x22222222 }
            );
            let fpu = fast.cpu.fpu.as_ref().unwrap();
            assert_eq!(fpu.status() & 0x4700, condition);
            let environment = fpu.save_environment(true, true, false);
            assert_eq!(
                &environment[12..16],
                &(ENTRY + if discard { 21 } else { 0 }).to_le_bytes()
            );
            assert_eq!(
                &environment[18..20],
                &(if discard { 0x5d8u16 } else { 0x0d1 }).to_le_bytes()
            );
            assert_eq!(&environment[20..24], &0x5678u32.to_le_bytes());
        }
    }
}

#[test]
fn compiled_comparison_pop_forms_and_register_stores_match_retained_state() {
    for (bytes, operation) in [
        (
            vec![0xd8, 0x15, 0, 0x20, 0, 0],
            X87Operation::CompareReal32 {
                address: operand(None, DATA),
                pop: false,
            },
        ),
        (
            vec![0xd8, 0x1d, 0, 0x20, 0, 0],
            X87Operation::CompareReal32 {
                address: operand(None, DATA),
                pop: true,
            },
        ),
        (
            vec![0xd8, 0xd9],
            X87Operation::CompareRegister { index: 1, pops: 1 },
        ),
        (
            vec![0xde, 0xd9],
            X87Operation::CompareRegister { index: 1, pops: 2 },
        ),
        (
            vec![0xdd, 0xd1],
            X87Operation::StoreRegister {
                index: 1,
                pop: false,
            },
        ),
        (
            vec![0xdd, 0xd9],
            X87Operation::StoreRegister {
                index: 1,
                pop: true,
            },
        ),
    ] {
        let length = bytes.len() as u32;
        let opcode = (u16::from(bytes[0] & 7) << 8) | u16::from(bytes[1]);
        let mut code = bytes;
        code.extend_from_slice(&[0xdf, 0xe0, 0xc3]);
        let compiled = ProcedureGraphProgram {
            functions: vec![ProcedureGraphFunction {
                id: ProcedureId(9),
                entry: address(ENTRY),
                blocks: vec![ProcedureBlock {
                    entry: address(ENTRY),
                    instructions: vec![
                        ProcedureInstruction {
                            address: address(ENTRY),
                            operation: Operation::X87(X87Instruction { opcode, operation }),
                        },
                        ProcedureInstruction {
                            address: address(ENTRY + length),
                            operation: Operation::X87(X87Instruction {
                                opcode: 0x7e0,
                                operation: X87Operation::StoreStatusAx,
                            }),
                        },
                    ],
                    terminator: ProcedureTerminator {
                        address: address(ENTRY + length + 2),
                        kind: ProcedureTerminatorKind::Return {
                            width: Width::Dword,
                            pop_bytes: 0,
                        },
                    },
                }],
            }],
        }
        .compile_with_pc24_locals_and_work_limits(&ProcedureCompileOptions::default(), 32, 32)
        .unwrap();
        let mut binding = binding();
        binding.x87 = compiled.x87(ProcedureId(9));
        binding.maximum_instructions = compiled.maximum_work(ProcedureId(9)).unwrap() as u32;
        binding.execution_sites = Some(
            [(0, length as u8), (length, 2), (length + 2, 1)]
                .into_iter()
                .map(|(offset, length)| ExecutionSite {
                    ip: ENTRY + offset,
                    length,
                })
                .collect(),
        );
        binding.code[0].expected = code.clone();
        let mut fast = machine();
        let mut reference = machine();
        for machine in [&mut fast, &mut reference] {
            machine
                .bus
                .debug_ram_mut(ENTRY, code.len() as u32)
                .unwrap()
                .copy_from_slice(&code);
            machine.bus.write_u32(DATA, 2.25f32.to_bits()).unwrap();
            machine.cpu.registers.set32(Gpr32::Eax, 0xdeadbeef);
            let fpu = machine.cpu.fpu.as_mut().unwrap();
            fpu.load_control(0x007f);
            fpu.push(2.25).unwrap();
            fpu.push(-0.0).unwrap();
            fpu.record_instruction(0x1234, 8, 0x105);
            fpu.record_operand(0x5678, 16);
        }
        fast.install_wasm_procedures(compiled.wasm(), vec![binding])
            .unwrap();
        assert_eq!(
            fast.try_wasm_procedure(32).unwrap(),
            Some(3),
            "FOP {opcode:03x}"
        );
        reference_steps(&mut reference, 3);
        same_state(&mut fast, &mut reference);
        same_fpu(&fast.cpu.fpu, &reference.cpu.fpu);
        assert_eq!(fast.cpu.registers.get32(Gpr32::Eax) >> 16, 0xdead);
        let environment = fast
            .cpu
            .fpu
            .as_ref()
            .unwrap()
            .save_environment(true, true, false);
        assert_eq!(&environment[12..16], &ENTRY.to_le_bytes());
        assert_eq!(&environment[18..20], &opcode.to_le_bytes());
        assert_eq!(
            &environment[20..24],
            &(if length == 6 { DATA } else { 0x5678 }).to_le_bytes()
        );
    }
}
