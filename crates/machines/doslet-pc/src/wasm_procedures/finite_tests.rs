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
