//! Actual interpreter entry, shared VGA backing and whole-procedure scheduling.
use super::*;

const COPY: &[u8] = &[
    0x89, 0xcb, 0xd1, 0xeb, 0x74, 0x21, 0x90, 0x90, 0x90, 0x90, 0x66, 0x8b, 0x06, 0x88, 0x07, 0x83,
    0xc6, 2, 0x81, 0xc7, 0xc0, 0, 0, 0, 0x88, 0x67, 0xa0, 0x4b, 0x75, 0xec, 0x83, 0xe1, 1, 0x74, 8,
    0x90, 0x90, 0x90, 0x90, 0x8a, 0x06, 0x88, 0x07, 0xc3,
];

fn program() -> ProcedureGraphProgram {
    let a = |offset| address(ENTRY + offset);
    let alu = |target, value| Operation::Alu {
        kind: Alu::Add,
        target,
        source: Source::Immediate(value),
    };
    let branch = |offset, condition, taken, not_taken| ProcedureTerminator {
        address: a(offset),
        kind: ProcedureTerminatorKind::Branch {
            condition,
            taken: a(taken),
            not_taken: a(not_taken),
        },
    };
    let block = |offset, operations: &[(u32, Operation)], terminator| ProcedureBlock {
        entry: a(offset),
        instructions: operations
            .iter()
            .map(|&(offset, operation)| ProcedureInstruction {
                address: a(offset),
                operation,
            })
            .collect(),
        terminator,
    };
    let fallthrough = |offset| ProcedureTerminator {
        address: a(offset),
        kind: ProcedureTerminatorKind::Fallthrough(a(offset)),
    };
    ProcedureGraphProgram {
        functions: vec![ProcedureGraphFunction {
            id: ProcedureId(9),
            entry: a(0),
            blocks: vec![
                block(
                    0,
                    &[
                        (
                            0,
                            Operation::Move {
                                target: Register::Ebx,
                                source: Source::Register(Register::Ecx),
                            },
                        ),
                        (
                            2,
                            Operation::Shift {
                                target: Register::Ebx,
                                kind: Shift::Shr,
                                count: ShiftCount::One,
                            },
                        ),
                    ],
                    branch(4, Condition::Equal, 39, 6),
                ),
                block(
                    6,
                    &[
                        (6, Operation::Nop),
                        (7, Operation::Nop),
                        (8, Operation::Nop),
                        (9, Operation::Nop),
                    ],
                    fallthrough(10),
                ),
                block(
                    10,
                    &[
                        (
                            10,
                            Operation::LoadMemory {
                                target: Register::Ax,
                                address: operand(Some(Register::Esi), 0),
                            },
                        ),
                        (
                            13,
                            Operation::StoreMemory {
                                width: Width::Byte,
                                source: Source::Register(Register::Al),
                                address: operand(Some(Register::Edi), 0),
                            },
                        ),
                        (15, alu(Register::Esi, 2)),
                        (18, alu(Register::Edi, 192)),
                        (
                            24,
                            Operation::StoreMemory {
                                width: Width::Byte,
                                source: Source::Register(Register::Ah),
                                address: operand(Some(Register::Edi), (-96i32) as u32),
                            },
                        ),
                        (27, Operation::Dec(Register::Ebx)),
                    ],
                    branch(28, Condition::NotEqual, 10, 30),
                ),
                block(
                    30,
                    &[(
                        30,
                        Operation::Alu {
                            kind: Alu::And,
                            target: Register::Ecx,
                            source: Source::Immediate(1),
                        },
                    )],
                    branch(33, Condition::Equal, 43, 35),
                ),
                block(
                    35,
                    &[
                        (35, Operation::Nop),
                        (36, Operation::Nop),
                        (37, Operation::Nop),
                        (38, Operation::Nop),
                    ],
                    fallthrough(39),
                ),
                block(
                    39,
                    &[
                        (
                            39,
                            Operation::LoadMemory {
                                target: Register::Al,
                                address: operand(Some(Register::Esi), 0),
                            },
                        ),
                        (
                            41,
                            Operation::StoreMemory {
                                width: Width::Byte,
                                source: Source::Register(Register::Al),
                                address: operand(Some(Register::Edi), 0),
                            },
                        ),
                    ],
                    fallthrough(43),
                ),
                block(
                    43,
                    &[],
                    ProcedureTerminator {
                        address: a(43),
                        kind: ProcedureTerminatorKind::Return {
                            width: Width::Dword,
                            pop_bytes: 0,
                        },
                    },
                ),
            ],
        }],
    }
}

fn setup(count: u32) -> (PcMachine, PcMachine) {
    let compiled = program()
        .compile_with_copy_kernels(&ProcedureCompileOptions::default(), 4096)
        .unwrap();
    let kernel = compiled
        .semantic_kernel(ProcedureId(9))
        .expect("recognized copy");
    let mut fast = machine();
    let mut reference = machine();
    for machine in [&mut fast, &mut reference] {
        machine
            .bus
            .debug_ram_mut(ENTRY, COPY.len() as u32)
            .unwrap()
            .copy_from_slice(COPY);
        machine.bus.set_video_mode_13h();
        video_register(machine, 0x3c4, 4, 6);
        video_register(machine, 0x3c4, 2, 5);
        machine.cpu.registers.set32(Gpr32::Ecx, count);
        machine.cpu.registers.set32(Gpr32::Esi, DATA);
        machine.cpu.registers.set32(Gpr32::Edi, 0xa0000);
        machine.cpu.registers.set32(Gpr32::Eax, 0xabcd_5678);
        machine.cpu.flags = 2 | flags::AUXILIARY | flags::CARRY | flags::OVERFLOW;
        for offset in 0..200 {
            machine
                .bus
                .write_u8(DATA + offset, (offset as u8).wrapping_mul(17))
                .unwrap();
        }
    }
    fast.install_wasm_procedures(
        compiled.wasm(),
        vec![Binding {
            function_id: 9,
            entry_ip: ENTRY,
            maximum_instructions: 4096,
            kernel: Some(kernel),
            rle_kernel: None,
            finite_computation: None,
            kernel_interrupts: KernelInterrupts::Defer,
            code: vec![CodePart {
                linear: ENTRY,
                expected: COPY.to_vec(),
            }],
        }],
    )
    .unwrap();
    (fast, reference)
}

#[test]
fn one_kernel_call_matches_interpreter_memory_and_abi_for_complete_procedure() {
    for count in [1, 2, 3, 199, 200] {
        let (mut fast, mut reference) = setup(count);
        let cost = fast.wasm_procedures.as_ref().unwrap().bindings[0]
            .binding
            .kernel
            .unwrap()
            .actual_work(count)
            .unwrap();
        assert_eq!(fast.step_with_idle_budget(cost).unwrap().1, cost);
        reference_steps(&mut reference, cost);
        same_state(&mut fast, &mut reference);
        assert_eq!(fast.cpu.registers.ip, RETURN);
        let stats = fast.wasm_procedure_stats().unwrap();
        assert_eq!(stats.kernel_calls, 1);
        assert_eq!(stats.kernel_instructions, u64::from(cost));
        assert_eq!(stats.side_exits, 0);
        assert_eq!(stats.device_writes, u64::from(count));
    }
}

#[test]
fn whole_kernel_finishes_past_soft_budget_and_requires_current_source_bytes() {
    let (mut fast, _) = setup(200);
    assert_eq!(fast.try_wasm_procedure(1).unwrap(), Some(710));
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 1);
    assert_eq!(fast.wasm_procedure_stats().unwrap().budget_refusals, 0);
    let (mut fast, _) = setup(200);
    fast.bus.write_u8(ENTRY, 0x90).unwrap();
    assert_eq!(fast.try_wasm_procedure(710).unwrap(), None);
    assert_eq!(fast.wasm_procedure_stats().unwrap().source_refusals, 1);
    assert!(fast.bus.vram_view().iter().all(|&byte| byte == 0));
}

#[test]
fn zero_count_declines_before_entering_or_changing_memory() {
    let (mut fast, _) = setup(0);
    assert_eq!(fast.try_wasm_procedure(4096).unwrap(), None);
    assert_eq!(fast.cpu.registers.ip, ENTRY);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 0);
    assert!(fast.bus.vram_view().iter().all(|&byte| byte == 0));
}

#[test]
fn optional_irq_admission_checks_only_the_whole_procedure_boundary() {
    let (mut fast, _) = setup(200);
    fast.cpu.flags |= flags::INTERRUPT;
    fast.bus.pit.write_control(0x34).unwrap();
    fast.bus.pit.write_counter(0, 8).unwrap();
    fast.bus.pit.write_counter(0, 0).unwrap();
    fast.wasm_procedures.as_mut().unwrap().bindings[0]
        .binding
        .kernel_interrupts = KernelInterrupts::BeforeNextIrq;
    assert_eq!(fast.try_wasm_procedure(710).unwrap(), None);
    assert_eq!(fast.cpu.registers.ip, ENTRY);
    assert_eq!(fast.wasm_procedure_stats().unwrap().kernel_irq_refusals, 1);
    assert!(fast.bus.vram_view().iter().all(|&byte| byte == 0));
    fast.wasm_procedures.as_mut().unwrap().bindings[0]
        .binding
        .kernel_interrupts = KernelInterrupts::Defer;
    assert_eq!(fast.try_wasm_procedure(710).unwrap(), Some(710));
    assert_eq!(fast.cpu.registers.ip, RETURN);
    assert!(fast.bus.pic.has_interrupt());
    assert_eq!(fast.wasm_procedure_stats().unwrap().kernel_calls, 1);
}

#[test]
fn whole_kernel_does_not_allocate_from_first_entry() {
    let (mut fast, _) = setup(200);
    let measured = allocation_counter::measure(|| {
        assert_eq!(fast.step_with_idle_budget(710).unwrap().1, 710);
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}

#[test]
fn uniform_tier_semantic_copy_matches_original_labels_work_and_weighted_time() {
    for rates in [[Some(133), Some(8), Some(33)], [None, Some(8), Some(133)]] {
        for tier in 0..4 {
            for count in [1, 3, 200] {
                let (mut fast, mut reference) = setup(count);
                for machine in [&mut fast, &mut reference] {
                    super::tier_tests::set_tiers(machine, COPY, tier, rates);
                }
                fast.set_wasm_procedure_tier_accounting(true).unwrap();
                let cost = fast.wasm_procedures.as_ref().unwrap().bindings[0]
                    .binding
                    .kernel
                    .unwrap()
                    .actual_work(count)
                    .unwrap();
                assert_eq!(fast.step_with_idle_budget(1).unwrap().1, cost);
                reference_steps(&mut reference, cost);
                super::tier_tests::same_tiered_state(&mut fast, &mut reference);
                let mut counts = [0; 4];
                counts[usize::from(tier)] = u64::from(cost);
                assert_eq!(fast.cpu.execution_counts(), counts);
                assert_eq!(fast.wasm_procedure_stats().unwrap().kernel_calls, 1);
            }
        }
    }
}

#[test]
fn tiered_before_irq_admission_uses_the_selected_weight_and_fraction() {
    for (tier, admitted) in [(0, true), (1, false)] {
        let (mut fast, _) = setup(200);
        super::tier_tests::set_tiers(&mut fast, COPY, tier, [None, Some(8), Some(133)]);
        fast.set_wasm_procedure_tier_accounting(true).unwrap();
        fast.cpu.flags |= flags::INTERRUPT;
        fast.bus.pit.write_control(0x34).unwrap();
        fast.bus.pit.write_counter(0, 8).unwrap();
        fast.bus.pit.write_counter(0, 0).unwrap();
        fast.wasm_procedures.as_mut().unwrap().bindings[0]
            .binding
            .kernel_interrupts = KernelInterrupts::BeforeNextIrq;
        assert_eq!(fast.try_wasm_procedure(1).unwrap().is_some(), admitted);
        assert_eq!(
            fast.wasm_procedure_stats().unwrap().kernel_irq_refusals,
            u64::from(!admitted)
        );
        if !admitted {
            assert_eq!(fast.cpu.execution_counts(), [0; 4]);
            assert_eq!(fast.cpu.registers.ip, ENTRY);
        }
    }
}

#[test]
fn mixed_copy_labels_match_exact_original_sites_and_time_for_every_path() {
    for cache in [false, true] {
        for rates in [[Some(133), Some(8), Some(33)], [None, Some(8), Some(133)]] {
            for count in [1, 2, 3, 199, 200] {
                let (mut fast, mut reference) = setup(count);
                assert!(fast.wasm_procedures.as_ref().unwrap().bindings[0].canonical_copy);
                for machine in [&mut fast, &mut reference] {
                    machine.cpu.set_decode_cache(cache);
                    let labels = super::tier_tests::labels(machine, COPY, |site| (site % 4) as u8);
                    machine.set_execution_tiers(Some(labels), rates).unwrap();
                }
                fast.set_wasm_procedure_tier_accounting(true).unwrap();
                let proof = fast.wasm_procedures.as_ref().unwrap().bindings[0]
                    .tier_proof(&fast.cpu, &fast.bus);
                assert!(matches!(proof, Some(TierProof::Complete(_))));
                let cost = fast.wasm_procedures.as_ref().unwrap().bindings[0]
                    .binding
                    .kernel
                    .unwrap()
                    .actual_work(count)
                    .unwrap();
                assert_eq!(fast.step_with_idle_budget(1).unwrap().1, cost);
                reference_steps(&mut reference, cost);
                super::tier_tests::same_tiered_state(&mut fast, &mut reference);
                assert_eq!(fast.wasm_procedure_stats().unwrap().kernel_calls, 1);
                assert!(
                    fast.cpu
                        .execution_counts()
                        .iter()
                        .filter(|&&n| n != 0)
                        .count()
                        > 1
                );
            }
        }
    }
}

#[test]
fn mixed_copy_preflight_refusal_does_not_charge_the_unexecuted_tier_proof() {
    let (mut fast, _) = setup(200);
    let labels = super::tier_tests::labels(&fast, COPY, |site| (site % 4) as u8);
    fast.set_execution_tiers(Some(labels), [Some(133), Some(8), Some(33)])
        .unwrap();
    fast.set_wasm_procedure_tier_accounting(true).unwrap();
    // Semantic copy supports planar VGA only; ordinary RAM forces a zero prefix.
    fast.cpu.registers.set32(Gpr32::Edi, 0x5000);
    let before = fast.cpu.snapshot();
    let time = fast.time();
    assert_eq!(fast.try_wasm_procedure(1).unwrap(), Some(0));
    assert!(fast.cpu.matches(&before));
    assert_eq!(fast.cpu.execution_counts(), [0; 4]);
    assert_eq!(fast.time(), time);
    assert_eq!(
        fast.wasm_procedure_stats().unwrap().translated_instructions,
        0
    );
}

#[test]
fn copy_site_attribution_declines_a_noncanonical_source_cfg() {
    let (mut fast, _) = setup(3);
    let labels = super::tier_tests::labels(&fast, COPY, |site| (site % 4) as u8);
    fast.set_execution_tiers(Some(labels), [Some(133), Some(8), Some(33)])
        .unwrap();
    fast.set_wasm_procedure_tier_accounting(true).unwrap();
    let binding = &mut fast.wasm_procedures.as_mut().unwrap().bindings[0];
    let kernel = binding.binding.kernel.unwrap();
    let instructions = binding.instructions.as_mut().unwrap();
    assert!(canonical_copy_sources(kernel, ENTRY, instructions));
    let original = instructions[2].flow;
    instructions[2].flow = doslet_x86::translation::NearFlow::Next;
    binding.canonical_copy = canonical_copy_sources(kernel, ENTRY, instructions);
    assert!(!binding.canonical_copy);
    assert_eq!(fast.try_wasm_procedure(1).unwrap(), None);
    assert_eq!(fast.cpu.execution_counts(), [0; 4]);
    let binding = &mut fast.wasm_procedures.as_mut().unwrap().bindings[0];
    let instructions = binding.instructions.as_mut().unwrap();
    instructions[2].flow = original;
    instructions[4].address += 1;
    assert!(!canonical_copy_sources(kernel, ENTRY, instructions));
    instructions[4].address -= 1;
    let last = instructions.len() - 1;
    instructions[last].flow = doslet_x86::translation::NearFlow::Return { pop: 4, width: 4 };
    assert!(!canonical_copy_sources(kernel, ENTRY, instructions));
}

fn with_interpreted_boundaries(count: u32) -> (PcMachine, PcMachine) {
    let (fast, mut reference) = setup(count);
    let kernel = fast.wasm_procedures.as_ref().unwrap().bindings[0]
        .binding
        .kernel;
    let ram = reference.bus.ram.storage.bytes().as_ptr();
    let vram = reference.bus.vram_view().as_ptr();
    reference
        .install_interpreted_procedure_boundaries(vec![Binding {
            function_id: 9,
            entry_ip: ENTRY,
            maximum_instructions: 4096,
            kernel,
            rle_kernel: None,
            finite_computation: None,
            kernel_interrupts: KernelInterrupts::Defer,
            code: vec![CodePart {
                linear: ENTRY,
                expected: COPY.to_vec(),
            }],
        }])
        .unwrap();
    assert_eq!(reference.bus.ram.storage.bytes().as_ptr(), ram);
    assert_eq!(reference.bus.vram_view().as_ptr(), vram);
    (fast, reference)
}

#[test]
fn interpreted_boundaries_match_aot_and_retain_the_native_memory_owner() {
    for count in [1, 2, 3, 199, 200] {
        let (mut fast, mut reference) = with_interpreted_boundaries(count);
        let cost = fast.wasm_procedures.as_ref().unwrap().bindings[0]
            .binding
            .kernel
            .unwrap()
            .actual_work(count)
            .unwrap();
        assert_eq!(fast.step_with_idle_budget(1).unwrap().1, cost);
        assert_eq!(reference.step_with_idle_budget(1).unwrap().1, cost);
        same_state(&mut fast, &mut reference);
        let stats = reference.wasm_procedure_stats().unwrap();
        assert_eq!(stats.calls, 1);
        assert_eq!(stats.interpreted_calls, 1);
        assert_eq!(stats.interpreted_instructions, u64::from(cost));
        assert_eq!(stats.translated_instructions, 0);
        assert_eq!(stats.kernel_instructions, u64::from(cost));
        assert_eq!(stats.device_writes, 0);
    }
}

#[test]
fn both_backends_deliver_a_timer_irq_after_the_complete_return() {
    let (mut fast, mut reference) = with_interpreted_boundaries(200);
    for machine in [&mut fast, &mut reference] {
        machine.cpu.control.idtr.base = 0x900;
        machine.cpu.control.idtr.limit = 0xff;
        // PIC IRQ0 vector 8 -> 0008:00030000, same-privilege interrupt gate.
        for (i, byte) in [0, 0, 8, 0, 0, 0x8e, 3, 0].into_iter().enumerate() {
            machine.bus.write_u8(0x940 + i as u32, byte).unwrap();
        }
        machine.bus.write_u8(0x30000, 0xcf).unwrap(); // IRETD
        machine.cpu.flags |= flags::INTERRUPT;
        machine.bus.pit.write_control(0x34).unwrap();
        machine.bus.pit.write_counter(0, 8).unwrap();
        machine.bus.pit.write_counter(0, 0).unwrap();
        assert_eq!(machine.step_with_idle_budget(710).unwrap().1, 710);
        assert_eq!(machine.cpu.registers.ip, RETURN);
        assert_eq!(machine.cpu.registers.get32(Gpr32::Esp), STACK + 4);
        assert_eq!(machine.interrupt_entries, 0);
        assert!(machine.bus.pic.has_interrupt());
        machine.step_with_idle_budget(1).unwrap();
        assert_eq!(machine.cpu.registers.ip, 0x30000);
        assert_eq!(machine.interrupt_entries, 1);
    }
    same_state(&mut fast, &mut reference);
}

#[test]
fn interpreted_boundaries_require_source_and_supported_memory() {
    let (_, mut machine) = with_interpreted_boundaries(200);
    machine.bus.write_u8(ENTRY, 0x90).unwrap();
    assert_eq!(machine.try_wasm_procedure(710).unwrap(), None);
    assert_eq!(machine.wasm_procedure_stats().unwrap().source_refusals, 1);
    machine.bus.write_u8(ENTRY, COPY[0]).unwrap();
    for (source, destination, count) in [
        (DATA, 0xa0000, 0),
        (0x400, 0xa0000, 200),
        (0x9ffff, 0xa0000, 200),
        (u32::MAX, 0xa0000, 200),
        (DATA, DATA, 200),
        (DATA, 0xaffff, 200),
    ] {
        machine.cpu.registers.set32(Gpr32::Esi, source);
        machine.cpu.registers.set32(Gpr32::Edi, destination);
        machine.cpu.registers.set32(Gpr32::Ecx, count);
        assert_eq!(machine.try_wasm_procedure(710).unwrap(), None);
        assert_eq!(machine.cpu.registers.ip, ENTRY);
    }
    assert_eq!(machine.wasm_procedure_stats().unwrap().calls, 0);
    assert!(machine.bus.vram_view().iter().all(|&byte| byte == 0));
}

#[test]
fn interpreted_whole_procedure_does_not_allocate_from_first_entry() {
    let (_, mut machine) = with_interpreted_boundaries(200);
    let measured = allocation_counter::measure(|| {
        assert_eq!(machine.step_with_idle_budget(710).unwrap().1, 710);
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}

#[test]
fn interpreted_install_rejects_an_unbounded_binding() {
    let mut machine = machine();
    assert!(
        machine
            .install_interpreted_procedure_boundaries(vec![binding()])
            .is_err()
    );
    assert!(machine.wasm_procedure_stats().is_none());
}

#[test]
fn interpreted_boundaries_decline_unsupported_vga_and_transient_cpu_states() {
    let (_, mut machine) = with_interpreted_boundaries(200);
    for (port, index, unsupported, original) in [
        (0x3c4, 4, 14, 6),    // Chain four
        (0x3ce, 5, 1, 0),     // Write mode
        (0x3ce, 1, 1, 0),     // Set/reset enable
        (0x3ce, 3, 1, 0),     // Rotation
        (0x3ce, 8, 127, 255), // Bit mask
    ] {
        video_register(&mut machine, port, index, unsupported);
        assert_eq!(machine.try_wasm_procedure(710).unwrap(), None);
        video_register(&mut machine, port, index, original);
    }
    let original = machine.cpu.flags;
    for flag in [flags::TRAP, flags::RESUME, flags::VIRTUAL] {
        machine.cpu.flags = original | flag;
        assert_eq!(machine.try_wasm_procedure(710).unwrap(), None);
    }
    machine.cpu.flags = original;
    assert_eq!(machine.wasm_procedure_stats().unwrap().calls, 0);
    machine
        .cpu
        .load_segment(&mut machine.bus, SegmentRegister::Ss, 16)
        .unwrap();
    assert!(machine.cpu.has_interrupt_shadow());
    assert_eq!(machine.try_wasm_procedure(710).unwrap(), None);
    assert!(machine.bus.vram_view().iter().all(|&byte| byte == 0));
}

#[test]
fn malformed_interpreted_contract_is_fatal_and_cannot_replay() {
    let (_, mut machine) = with_interpreted_boundaries(200);
    machine.wasm_procedures.as_mut().unwrap().bindings[0]
        .binding
        .kernel
        .as_mut()
        .unwrap()
        .prefix_nops += 1;
    let error = MachineError::WasmProcedure(ExecutionError::InvalidResult(
        MemoryProcedureResultError::InstructionPointer,
    ));
    let time = machine.time();
    assert_eq!(machine.step_with_idle_budget(711), Err(error));
    assert!(machine.bus.vram_view().iter().any(|&byte| byte != 0));
    assert_eq!(machine.time(), time);
    let accesses = machine.bus.device_accesses;
    let cpu = machine.cpu.snapshot();
    assert_eq!(machine.step_with_idle_budget(711), Err(error));
    assert_eq!(machine.bus.device_accesses, accesses);
    assert!(machine.cpu.matches(&cpu));
}
