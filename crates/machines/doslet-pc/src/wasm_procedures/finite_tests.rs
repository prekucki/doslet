use super::*;

const END: u32 = ENTRY + 17;
const OUTPUT: u32 = 0x3000;
const BODY: &[u8] = &[
    0xb9, 3, 0, 0, 0,    // MOV ECX,3
    0xac, // LODSB
    0xa8, 1, // TEST AL,1
    0x74, 2, // JZ store
    0x34, 0x55, // XOR AL,55h
    0xaa, // STOSB
    0xfe, 0xc9, // DEC CL
    0x75, 0xf4, // JNZ LODSB
    0x90, // Interpreter continuation, outside the compiled computation.
];

fn configured() -> (PcMachine, Vec<u8>, Binding) {
    let mut blocks = Vec::new();
    for (offset, next, operation) in [
        (
            0,
            5,
            Operation::Move {
                target: Register::Ecx,
                source: Source::Immediate(3),
            },
        ),
        (
            5,
            6,
            Operation::StringLoad {
                width: Width::Byte,
                address16: false,
                segment: Segment::Ds,
            },
        ),
        (
            6,
            8,
            Operation::Alu {
                kind: Alu::Test,
                target: Register::Al,
                source: Source::Immediate(1),
            },
        ),
        (
            10,
            12,
            Operation::Alu {
                kind: Alu::Xor,
                target: Register::Al,
                source: Source::Immediate(0x55),
            },
        ),
        (
            12,
            13,
            Operation::StringStore {
                width: Width::Byte,
                address16: false,
            },
        ),
        (13, 15, Operation::Dec(Register::Cl)),
    ] {
        blocks.push(ProcedureBlock {
            entry: address(ENTRY + offset),
            instructions: vec![ProcedureInstruction {
                address: address(ENTRY + offset),
                operation,
            }],
            terminator: ProcedureTerminator {
                address: address(ENTRY + next),
                kind: ProcedureTerminatorKind::Fallthrough(address(ENTRY + next)),
            },
        });
    }
    for (offset, condition, taken, not_taken) in [
        (8, Condition::Equal, 12, 10),
        (15, Condition::NotEqual, 5, 17),
    ] {
        blocks.push(ProcedureBlock {
            entry: address(ENTRY + offset),
            instructions: vec![],
            terminator: ProcedureTerminator {
                address: address(ENTRY + offset),
                kind: ProcedureTerminatorKind::Branch {
                    condition,
                    taken: address(ENTRY + taken),
                    not_taken: address(ENTRY + not_taken),
                },
            },
        });
    }
    blocks.push(ProcedureBlock {
        entry: address(END),
        instructions: vec![],
        terminator: ProcedureTerminator {
            address: address(END),
            kind: ProcedureTerminatorKind::Exit,
        },
    });
    let compiled = ProcedureGraphProgram {
        functions: vec![ProcedureGraphFunction {
            id: ProcedureId(9),
            entry: address(ENTRY),
            blocks,
        }],
    }
    .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 4096)
    .unwrap();
    let maximum_instructions = compiled.finite_maximum_work(ProcedureId(9)).unwrap();
    let mut machine = machine();
    machine.cpu.registers.set32(Gpr32::Esi, DATA);
    machine.cpu.registers.set32(Gpr32::Edi, OUTPUT);
    machine
        .bus
        .debug_ram_mut(DATA, 3)
        .unwrap()
        .copy_from_slice(&[1, 2, 3]);
    machine
        .bus
        .debug_ram_mut(ENTRY, BODY.len() as u32)
        .unwrap()
        .copy_from_slice(BODY);
    let binding = Binding {
        x87: None,
        execution_sites: None,
        function_id: 9,
        entry_ip: ENTRY,
        maximum_instructions,
        kernel: None,
        rle_kernel: None,
        finite_computation: Some(FiniteComputation {
            exits: vec![END],
            returns: vec![],
            guards: vec![],
        }),
        kernel_interrupts: KernelInterrupts::Defer,
        code_segment: CodeSegment::FLAT,
        // Instruction starts remain explicit for the optional reference.
        code: [0usize, 5, 6, 8, 10, 12, 13, 15, 17]
            .into_iter()
            .map(|at| {
                let length = match at {
                    0 => 5,
                    6 | 8 | 10 | 13 | 15 => 2,
                    _ => 1,
                };
                CodePart {
                    linear: ENTRY + at as u32,
                    expected: BODY[at..at + length].to_vec(),
                }
            })
            .collect(),
    };
    (machine, compiled.into_wasm(), binding)
}

#[test]
fn finite_branchy_string_region_matches_original_x86_past_a_soft_budget() {
    let (mut fast, wasm, binding) = configured();
    let (mut reference, _, _) = configured();
    let mut work = 0;
    while reference.cpu.registers.ip != END {
        reference_steps(&mut reference, 1);
        work += 1;
        assert!(work < 256);
    }
    fast.install_wasm_procedures(&wasm, vec![binding]).unwrap();
    assert_eq!(fast.step_with_idle_budget(1).unwrap().1, work);
    same_state(&mut fast, &mut reference);
    assert_eq!(
        fast.bus
            .ram_view(OUTPUT as usize..OUTPUT as usize + 3)
            .unwrap(),
        &[0x54, 2, 0x56]
    );
    let stats = fast.wasm_procedure_stats().unwrap();
    assert_eq!(stats.kernel_calls, 1);
    assert_eq!(stats.side_exits, 1);
    assert_eq!(stats.budget_refusals, 0);
}

#[test]
fn finite_region_reference_stops_before_its_interpreter_continuation() {
    let (mut fast, wasm, binding) = configured();
    let (mut reference, _, reference_binding) = configured();
    fast.install_wasm_procedures(&wasm, vec![binding]).unwrap();
    reference
        .install_interpreted_procedure_boundaries(vec![reference_binding])
        .unwrap();
    let work = fast.step_with_idle_budget(1).unwrap().1;
    assert_eq!(reference.step_with_idle_budget(1).unwrap().1, work);
    same_state(&mut fast, &mut reference);
    assert_eq!(fast.cpu.registers.ip, END);
}

#[test]
fn finite_string_rendering_matches_vga_planes_and_write_modes() {
    for mode in 0..=3 {
        let (mut fast, wasm, binding) = configured();
        let (mut reference, _, _) = configured();
        for machine in [&mut fast, &mut reference] {
            machine.bus.set_video_mode_13h();
            video_register(machine, 0x3c4, 4, 6);
            video_register(machine, 0x3c4, 2, 5);
            video_register(machine, 0x3ce, 5, mode);
            machine.cpu.registers.set32(Gpr32::Edi, 0xa0100);
        }
        let mut work = 0;
        while reference.cpu.registers.ip != END {
            reference_steps(&mut reference, 1);
            work += 1;
            assert!(work < 256);
        }
        fast.install_wasm_procedures(&wasm, vec![binding]).unwrap();
        assert_eq!(fast.step_with_idle_budget(1).unwrap().1, work);
        same_state(&mut fast, &mut reference);
        let stats = fast.wasm_procedure_stats().unwrap();
        assert_eq!(stats.device_writes, 3);
        assert_eq!(stats.side_exits, 1);
    }
}

fn guarded_countdown(value: u32) -> (PcMachine, Vec<u8>, Binding, u32) {
    let a = |offset| address(ENTRY + offset);
    let function = ProcedureGraphFunction {
        id: ProcedureId(9),
        entry: a(0),
        blocks: vec![
            ProcedureBlock {
                entry: a(0),
                instructions: vec![ProcedureInstruction {
                    address: a(0),
                    operation: Operation::StringStore {
                        width: Width::Byte,
                        address16: false,
                    },
                }],
                terminator: ProcedureTerminator {
                    address: a(1),
                    kind: ProcedureTerminatorKind::Fallthrough(a(1)),
                },
            },
            ProcedureBlock {
                entry: a(1),
                instructions: vec![ProcedureInstruction {
                    address: a(1),
                    operation: Operation::Dec(Register::Ecx),
                }],
                terminator: ProcedureTerminator {
                    address: a(2),
                    kind: ProcedureTerminatorKind::Fallthrough(a(2)),
                },
            },
            ProcedureBlock {
                entry: a(2),
                instructions: vec![],
                terminator: ProcedureTerminator {
                    address: a(2),
                    kind: ProcedureTerminatorKind::Branch {
                        condition: Condition::NotEqual,
                        taken: a(0),
                        not_taken: a(4),
                    },
                },
            },
            ProcedureBlock {
                entry: a(4),
                instructions: vec![],
                terminator: ProcedureTerminator {
                    address: a(4),
                    kind: ProcedureTerminatorKind::Exit,
                },
            },
        ],
    };
    let compiled = ProcedureGraphProgram {
        functions: vec![function],
    }
    .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 4096)
    .unwrap();
    let guard = compiled.finite_loop_guards(ProcedureId(9)).unwrap()[0];
    let mut machine = machine();
    machine.cpu.registers.set32(Gpr32::Ecx, value);
    machine.cpu.registers.set32(Gpr32::Edi, OUTPUT);
    machine
        .bus
        .debug_ram_mut(ENTRY, 5)
        .unwrap()
        .copy_from_slice(&[0xaa, 0x49, 0x75, 0xfc, 0x90]);
    let binding = Binding {
        x87: None,
        execution_sites: None,
        function_id: 9,
        entry_ip: ENTRY,
        maximum_instructions: compiled.finite_maximum_work(ProcedureId(9)).unwrap(),
        kernel: None,
        rle_kernel: None,
        finite_computation: Some(FiniteComputation {
            exits: vec![ENTRY + 4],
            returns: vec![],
            guards: vec![FiniteLoopGuard {
                entry_ip: guard.entry.offset,
                counter: guard.counter.index() as u8,
                decrement: guard.decrement,
                maximum_trips: guard.maximum_trips,
            }],
        }),
        kernel_interrupts: KernelInterrupts::Defer,
        code_segment: CodeSegment::FLAT,
        code: vec![
            CodePart {
                linear: ENTRY,
                expected: vec![0xaa],
            },
            CodePart {
                linear: ENTRY + 1,
                expected: vec![0x49],
            },
            CodePart {
                linear: ENTRY + 2,
                expected: vec![0x75, 0xfc],
            },
            CodePart {
                linear: ENTRY + 4,
                expected: vec![0x90],
            },
        ],
    };
    (machine, compiled.into_wasm(), binding, guard.maximum_trips)
}

#[test]
fn interpreted_reference_honors_live_dword_entry_guard_without_executing_a_wrap() {
    let (_, _, _, cap) = guarded_countdown(1);
    for value in [0, 3, cap + 1] {
        let (mut fast, wasm, binding, _) = guarded_countdown(value);
        let (mut reference, _, reference_binding, _) = guarded_countdown(value);
        fast.install_wasm_procedures(&wasm, vec![binding]).unwrap();
        reference
            .install_interpreted_procedure_boundaries(vec![reference_binding])
            .unwrap();
        let expected = if value == 3 { 9 } else { 0 };
        assert_eq!(fast.try_wasm_procedure(1).unwrap(), Some(expected));
        assert_eq!(reference.try_wasm_procedure(1).unwrap(), Some(expected));
        same_state(&mut fast, &mut reference);
        assert_eq!(
            fast.cpu.registers.ip,
            if value == 3 { ENTRY + 4 } else { ENTRY }
        );
    }
}

#[test]
fn virtual_register_displacement_and_immediate_reflect_live_code_patches_without_source_refusal() {
    for explicit in [false, true] {
        virtual_register_slot_case(explicit);
    }
}

fn virtual_register_slot_case(explicit: bool) {
    // 00: 8A 83 [disp32 @ ENTRY+2]  MOV AL, [EBX + disp32]
    // 06: 81 C7 [imm32  @ ENTRY+8]  ADD EDI, imm32
    // 0C: 88 07                     MOV [EDI], AL
    // 0E: 49                        DEC ECX
    // 0F: 75 EF                     JNZ 00
    // 11: C3                        RET
    let code_bytes: [u8; 18] = [
        0x8a, 0x83, 0x00, 0x20, 0x00, 0x00, // MOV AL, [EBX + 0x2000]
        0x81, 0xc7, 0x04, 0x00, 0x00, 0x00, // ADD EDI, 4
        0x88, 0x07, // MOV [EDI], AL
        0x49, // DEC ECX
        0x75, 0xef, // JNZ ENTRY
        0xc3, // RET
    ];
    let a = |offset| address(ENTRY + offset);
    let function = ProcedureGraphFunction {
        id: ProcedureId(11),
        entry: a(0),
        blocks: vec![
            ProcedureBlock {
                entry: a(0),
                instructions: vec![
                    ProcedureInstruction {
                        address: a(0),
                        operation: Operation::LoadMemory {
                            target: Register::Al,
                            address: MemoryAddress {
                                base: Some(Register::Ebx),
                                index: None,
                                scale: 1
                                    | doslet_x86_wasm::static_program::memory::VIRTUAL_DISPLACEMENT,
                                displacement: ENTRY + 2,
                                address16: false,
                                segment: Segment::Ds,
                            },
                        },
                    },
                    ProcedureInstruction {
                        address: a(6),
                        operation: Operation::Alu {
                            kind: Alu::Add,
                            target: Register::Edi,
                            source: Source::VirtualRegister(ENTRY + 8),
                        },
                    },
                    ProcedureInstruction {
                        address: a(12),
                        operation: Operation::StoreMemory {
                            address: operand(Some(Register::Edi), 0),
                            width: Width::Byte,
                            source: Source::Register(Register::Al),
                        },
                    },
                    ProcedureInstruction {
                        address: a(14),
                        operation: Operation::Dec(Register::Ecx),
                    },
                ],
                terminator: ProcedureTerminator {
                    address: a(15),
                    kind: ProcedureTerminatorKind::Branch {
                        condition: Condition::NotEqual,
                        taken: a(0),
                        not_taken: a(17),
                    },
                },
            },
            ProcedureBlock {
                entry: a(17),
                instructions: vec![],
                terminator: ProcedureTerminator {
                    address: a(17),
                    kind: ProcedureTerminatorKind::Return {
                        width: Width::Dword,
                        pop_bytes: 0,
                    },
                },
            },
        ],
    };
    let compiled = ProcedureGraphProgram {
        functions: vec![function],
    }
    .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 4096)
    .unwrap();
    let guard = compiled.finite_loop_guards(ProcedureId(11)).unwrap()[0];
    let binding = Binding {
        x87: None,
        execution_sites: explicit.then(|| {
            [(0, 6), (6, 6), (12, 2), (14, 1), (15, 2), (17, 1)]
                .into_iter()
                .map(|(offset, length)| ExecutionSite {
                    ip: ENTRY + offset,
                    length,
                })
                .collect()
        }),
        function_id: 11,
        code_segment: CodeSegment::FLAT,
        entry_ip: ENTRY,
        maximum_instructions: compiled.finite_maximum_work(ProcedureId(11)).unwrap(),
        kernel: None,
        rle_kernel: None,
        finite_computation: Some(FiniteComputation {
            exits: vec![],
            returns: vec![ENTRY + 17],
            guards: vec![FiniteLoopGuard {
                entry_ip: guard.entry.offset,
                counter: guard.counter.index() as u8,
                decrement: guard.decrement,
                maximum_trips: guard.maximum_trips,
            }],
        }),
        kernel_interrupts: KernelInterrupts::Defer,
        // Split around virtual-register slots [ENTRY+2..ENTRY+6] and [ENTRY+8..ENTRY+12].
        code: vec![
            CodePart {
                linear: ENTRY,
                expected: vec![0x8a, 0x83],
            },
            CodePart {
                linear: ENTRY + 6,
                expected: vec![0x81, 0xc7],
            },
            CodePart {
                linear: ENTRY + 12,
                expected: vec![0x88, 0x07],
            },
            CodePart {
                linear: ENTRY + 14,
                expected: vec![0x49],
            },
            CodePart {
                linear: ENTRY + 15,
                expected: vec![0x75, 0xef],
            },
            CodePart {
                linear: ENTRY + 17,
                expected: vec![0xc3],
            },
        ],
    };
    let wasm = compiled.into_wasm();
    let setup = |disp: u32, stride: u32| {
        let mut m = machine();
        m.bus
            .debug_ram_mut(ENTRY, code_bytes.len() as u32)
            .unwrap()
            .copy_from_slice(&code_bytes);
        m.bus.write_u32(ENTRY + 2, disp).unwrap();
        m.bus.write_u32(ENTRY + 8, stride).unwrap();
        m.bus
            .debug_ram_mut(DATA, 8)
            .unwrap()
            .copy_from_slice(&[0x11, 0x22, 0x33, 0x44, 0xaa, 0xbb, 0xcc, 0xdd]);
        m.cpu.registers.set32(Gpr32::Ebx, 1);
        m.cpu.registers.set32(Gpr32::Edi, OUTPUT);
        m.cpu.registers.set32(Gpr32::Ecx, 2);
        m.cpu.registers.set32(Gpr32::Esp, 0x7000);
        m.bus.write_u32(0x7000, RETURN).unwrap();
        m
    };

    let mut fast = setup(DATA, 3);
    let mut reference = setup(DATA, 3);
    fast.install_wasm_procedures(&wasm, vec![binding]).unwrap();
    // Slot addresses in this module are absolute. A declined move must leave
    // the old entry and source proofs usable for the ordinary call below.
    assert!(fast.rebase_wasm_procedures(0x10_0000).is_err());
    assert_eq!(fast.wasm_procedure_code_segment(), Some(CodeSegment::FLAT));
    same_state(&mut fast, &mut reference);

    // First call with disp = DATA, stride = 3
    let work1 = fast.try_wasm_procedure(1).unwrap().unwrap();
    assert!(work1 > 0);
    while reference.cpu.registers.ip != RETURN {
        reference_steps(&mut reference, 1);
    }
    same_state(&mut fast, &mut reference);

    // Patch virtual registers in-place via bus writes (as self-modifying guest code does)
    for m in [&mut fast, &mut reference] {
        m.cpu.registers.ip = ENTRY;
        m.cpu.registers.set32(Gpr32::Ebx, 2);
        m.cpu.registers.set32(Gpr32::Edi, OUTPUT + 0x20);
        m.cpu.registers.set32(Gpr32::Ecx, 2);
        m.cpu.registers.set32(Gpr32::Esp, 0x7000);
        m.bus.write_u32(0x7000, RETURN).unwrap();
        m.bus.write_u32(ENTRY + 2, DATA + 4).unwrap();
        m.bus.write_u32(ENTRY + 8, 5).unwrap();
    }

    let work2 = fast.try_wasm_procedure(1).unwrap().unwrap();
    assert_eq!(work2, work1);
    while reference.cpu.registers.ip != RETURN {
        reference_steps(&mut reference, 1);
    }
    same_state(&mut fast, &mut reference);

    let stats = fast.wasm_procedure_stats().unwrap();
    assert_eq!(stats.calls, 2);
    assert_eq!(stats.source_refusals, 0);
}
