//! Original instruction labels, exact completed work, and weighted device time.
use super::*;

pub(super) fn labels(
    machine: &PcMachine,
    code: &[u8],
    tier: impl Fn(usize) -> u8,
) -> Vec<doslet_x86::ExecutionLabel> {
    let mut labels = Vec::new();
    let mut cursor = 0;
    while cursor < code.len() {
        let length = doslet_x86::translation::instruction_length(&code[cursor..], true).unwrap();
        let address = ENTRY + cursor as u32;
        labels.push(doslet_x86::ExecutionLabel {
            address,
            code32: true,
            tier: tier(labels.len()),
            bytes: machine
                .bus
                .ram_view(address as usize..address as usize + 15)
                .unwrap()
                .try_into()
                .unwrap(),
        });
        cursor += usize::from(length);
    }
    assert_eq!(cursor, code.len());
    labels
}

pub(super) fn set_tiers(machine: &mut PcMachine, code: &[u8], tier: u8, rates: [Option<u16>; 3]) {
    machine
        .set_execution_tiers(Some(labels(machine, code, |_| tier)), rates)
        .unwrap();
}

pub(super) fn same_tiered_state(fast: &mut PcMachine, reference: &mut PcMachine) {
    same_state(fast, reference);
    assert_eq!(
        fast.cpu.execution_counts(),
        reference.cpu.execution_counts()
    );
    assert_eq!(
        fast.cpu.executed_tier_counts(),
        reference.cpu.executed_tier_counts()
    );
    assert_eq!(
        fast.execution_tier_stats(),
        reference.execution_tier_stats()
    );
}

#[test]
fn uniform_procedures_match_each_original_instruction_and_fractional_clock() {
    for cached in [false, true] {
        for rates in [[Some(133), Some(8), Some(33)], [None, Some(8), Some(133)]] {
            for tier in 0..4 {
                let (mut fast, mut reference) = (installed(), machine());
                fast.cpu.set_decode_cache(cached);
                reference.cpu.set_decode_cache(cached);
                set_tiers(&mut fast, CODE, tier, rates);
                set_tiers(&mut reference, CODE, tier, rates);
                // Native behavior remains conservative until explicitly enabled.
                assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
                fast.set_wasm_procedure_tier_accounting(true).unwrap();
                for _ in 0..7 {
                    for machine in [&mut fast, &mut reference] {
                        machine.cpu.registers.ip = ENTRY;
                        machine.cpu.registers.set32(Gpr32::Esp, STACK);
                    }
                    assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(4));
                    reference_steps(&mut reference, 4);
                    same_tiered_state(&mut fast, &mut reference);
                }
                let mut expected = [0; 4];
                expected[usize::from(tier)] = 28;
                assert_eq!(fast.cpu.execution_counts(), expected);
                assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 7);
            }
        }
    }
}

#[test]
fn mixed_changed_and_replaced_labels_are_reclassified_before_effects() {
    let mut fast = installed();
    let original = labels(&fast, CODE, |_| 0);
    fast.set_execution_tiers(Some(original.clone()), [Some(133), Some(8), Some(33)])
        .unwrap();
    fast.set_wasm_procedure_tier_accounting(true).unwrap();
    let mut mixed = original.clone();
    mixed[1].tier = 1;
    fast.cpu.replace_execution_labels(mixed).unwrap();
    let snapshot = fast.cpu.snapshot();
    let ram = fast.bus.ram.storage.bytes().to_vec();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    assert!(fast.cpu.matches(&snapshot));
    assert_eq!(fast.bus.ram.storage.bytes(), ram);
    assert_eq!(fast.cpu.execution_counts(), [0; 4]);
    let mut stale = original;
    stale[1].bytes[0] ^= 1;
    fast.cpu.replace_execution_labels(stale).unwrap();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    assert_eq!(fast.wasm_procedure_stats().unwrap().tier_refusals, 2);
    // Every instruction missing from the current map is classified GREY.
    fast.cpu.replace_execution_labels(Vec::new()).unwrap();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(4));
    assert_eq!(fast.cpu.execution_counts(), [0, 0, 4, 0]);
}

#[test]
fn a_kept_uniform_tier_follows_relabelling_and_source_changes() {
    let mut fast = installed();
    set_tiers(&mut fast, CODE, 0, [Some(133), Some(8), Some(33)]);
    fast.set_wasm_procedure_tier_accounting(true).unwrap();
    let call = |fast: &mut PcMachine| {
        fast.cpu.registers.ip = ENTRY;
        fast.cpu.registers.set32(Gpr32::Esp, STACK);
        fast.try_wasm_procedure(32).unwrap()
    };
    assert_eq!(call(&mut fast), Some(4));
    assert_eq!(call(&mut fast), Some(4));
    assert_eq!(fast.cpu.execution_counts(), [8, 0, 0, 0]);
    let relabelled = labels(&fast, CODE, |_| 1);
    fast.cpu.replace_execution_labels(relabelled).unwrap();
    assert_eq!(call(&mut fast), Some(4));
    assert_eq!(fast.cpu.execution_counts(), [8, 4, 0, 0]);
    // A changed source byte refuses the call; restored bytes keep the labels.
    let original = fast
        .bus
        .ram_view(ENTRY as usize..ENTRY as usize + 1)
        .unwrap()[0];
    fast.bus.debug_ram_mut(ENTRY, 1).unwrap()[0] = original ^ 0xff;
    assert_eq!(call(&mut fast), None);
    fast.bus.debug_ram_mut(ENTRY, 1).unwrap()[0] = original;
    assert_eq!(call(&mut fast), Some(4));
    assert_eq!(fast.cpu.execution_counts(), [8, 8, 0, 0]);
}

#[test]
fn tiered_side_exit_charges_only_its_exact_completed_prefix() {
    for tier in 0..4 {
        let (mut fast, mut reference) = (installed(), machine());
        for machine in [&mut fast, &mut reference] {
            machine.cpu.registers.set32(Gpr32::Ebx, ENTRY); // Protected source store must exit.
            set_tiers(machine, CODE, tier, [Some(133), Some(8), Some(33)]);
        }
        fast.set_wasm_procedure_tier_accounting(true).unwrap();
        assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(2));
        reference_steps(&mut reference, 2);
        same_tiered_state(&mut fast, &mut reference);
        assert_eq!(fast.cpu.registers.ip, ENTRY + 8);
        assert_eq!(fast.wasm_procedure_stats().unwrap().side_exits, 1);
        let mut counts = [0; 4];
        counts[usize::from(tier)] = 2;
        assert_eq!(fast.cpu.execution_counts(), counts);
    }
}

#[test]
fn suspended_procedures_leave_their_entries_to_the_interpreter() {
    let mut fast = installed();
    fast.set_wasm_procedures_suspended(true).unwrap();
    assert!(!fast.wasm_procedures_active());
    assert!(!fast.cpu.has_prepared_entries());
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    fast.set_wasm_procedures_suspended(false).unwrap();
    assert!(fast.wasm_procedures_active());
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(4));
}

#[test]
fn first_uniform_tier_call_keeps_execution_allocation_free() {
    let mut fast = installed();
    set_tiers(&mut fast, CODE, 0, [Some(133), Some(8), Some(33)]);
    fast.set_wasm_procedure_tier_accounting(true).unwrap();
    let measured =
        allocation_counter::measure(|| assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(4)));
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}

fn x87_exit_fixture(explicit: bool) -> (Vec<u8>, Binding, Vec<u8>) {
    const PREFIX: u32 = 32;
    let boundary = RETURN;
    let mut code = vec![0x40; PREFIX as usize];
    code.extend_from_slice(&[0x89, 0x03, 0xe9]); // MOV [EBX],EAX; JMP boundary.
    code.extend_from_slice(&(boundary - (ENTRY + PREFIX + 7)).to_le_bytes());
    let mut instructions: Vec<_> = (0..PREFIX)
        .map(|offset| ProcedureInstruction {
            address: address(ENTRY + offset),
            operation: Operation::Inc(Register::Eax),
        })
        .collect();
    instructions.push(ProcedureInstruction {
        address: address(ENTRY + PREFIX),
        operation: Operation::StoreMemory {
            address: operand(Some(Register::Ebx), 0),
            width: Width::Dword,
            source: Source::Register(Register::Eax),
        },
    });
    let function = ProcedureGraphFunction {
        id: ProcedureId(9),
        entry: address(ENTRY),
        blocks: vec![
            ProcedureBlock {
                entry: address(ENTRY),
                instructions,
                terminator: ProcedureTerminator {
                    address: address(ENTRY + PREFIX + 2),
                    kind: ProcedureTerminatorKind::Jump(address(boundary)),
                },
            },
            ProcedureBlock {
                entry: address(boundary),
                instructions: vec![],
                terminator: ProcedureTerminator {
                    address: address(boundary),
                    kind: ProcedureTerminatorKind::Exit,
                },
            },
        ],
    };
    let wasm = ProcedureGraphProgram {
        functions: vec![function],
    }
    .compile(&ProcedureCompileOptions::default())
    .unwrap()
    .into_wasm();
    let mut sites: Vec<_> = (0..PREFIX)
        .map(|offset| ExecutionSite {
            ip: ENTRY + offset,
            length: 1,
        })
        .collect();
    sites.extend([
        ExecutionSite {
            ip: ENTRY + PREFIX,
            length: 2,
        },
        ExecutionSite {
            ip: ENTRY + PREFIX + 2,
            length: 5,
        },
    ]);
    let binding = Binding {
        x87: None,
        function_id: 9,
        entry_ip: ENTRY,
        maximum_instructions: 34,
        execution_sites: explicit.then_some(sites),
        kernel: None,
        rle_kernel: None,
        finite_computation: None,
        kernel_interrupts: KernelInterrupts::Defer,
        code_segment: CodeSegment::FLAT,
        code: vec![
            CodePart {
                linear: ENTRY,
                expected: code.clone(),
            },
            CodePart {
                linear: boundary,
                expected: vec![0xd9, 0xe8],
            },
        ],
    };
    (wasm, binding, code)
}

fn x87_exit_machine(code: &[u8]) -> PcMachine {
    let mut machine = machine();
    machine.set_cpu_mhz(1000).unwrap();
    machine
        .bus
        .debug_ram_mut(ENTRY, code.len() as u32)
        .unwrap()
        .copy_from_slice(code);
    machine
        .bus
        .debug_ram_mut(RETURN, 2)
        .unwrap()
        .copy_from_slice(&[0xd9, 0xe8]);
    let mut labels = labels(&machine, code, |_| 0);
    labels.push(doslet_x86::ExecutionLabel {
        address: RETURN,
        code32: true,
        tier: 1,
        bytes: machine
            .bus
            .ram_view(RETURN as usize..RETURN as usize + 15)
            .unwrap()
            .try_into()
            .unwrap(),
    });
    machine
        .set_execution_tiers(Some(labels), [Some(266), Some(8), Some(33)])
        .unwrap();
    machine
}

#[test]
fn execution_sites_admit_hot_scalar_prefix_before_cold_unexecuted_x87() {
    let (wasm, binding, code) = x87_exit_fixture(true);
    let (mut fast, mut reference) = (x87_exit_machine(&code), x87_exit_machine(&code));
    fast.install_wasm_procedures(&wasm, vec![binding]).unwrap();
    fast.set_wasm_procedure_tier_accounting(true).unwrap();
    let measured =
        allocation_counter::measure(|| assert_eq!(fast.try_wasm_procedure(128).unwrap(), Some(34)));
    assert_eq!(measured.count_total, 0);
    reference_steps(&mut reference, 34);
    same_tiered_state(&mut fast, &mut reference);
    assert_eq!(fast.cpu.registers.ip, RETURN);
    assert_eq!(fast.cpu.execution_counts(), [34, 0, 0, 0]);
    // The x87 boundary is not costed, but every immutable byte is still checked.
    fast.cpu.registers.ip = ENTRY;
    fast.bus.debug_ram_mut(RETURN + 1, 1).unwrap()[0] = 0xee;
    let before = fast.cpu.snapshot();
    assert_eq!(fast.try_wasm_procedure(128).unwrap(), None);
    assert!(fast.cpu.matches(&before));
    assert_eq!(fast.wasm_procedure_stats().unwrap().source_refusals, 1);
}

#[test]
fn execution_sites_keep_unexecuted_boundary_pages_protected_and_legacy_conservative() {
    let (wasm, binding, code) = x87_exit_fixture(true);
    let (mut fast, mut reference) = (x87_exit_machine(&code), x87_exit_machine(&code));
    for machine in [&mut fast, &mut reference] {
        machine.cpu.registers.set32(Gpr32::Ebx, RETURN);
    }
    fast.install_wasm_procedures(&wasm, vec![binding]).unwrap();
    fast.set_wasm_procedure_tier_accounting(true).unwrap();
    assert_eq!(fast.try_wasm_procedure(128).unwrap(), Some(32)); // Store to protected Exit page declines.
    reference_steps(&mut reference, 32);
    same_tiered_state(&mut fast, &mut reference);
    assert_eq!(fast.bus.debug_ram(RETURN, 2).unwrap(), [0xd9, 0xe8]);
    let (wasm, binding, code) = x87_exit_fixture(false);
    let mut legacy = x87_exit_machine(&code);
    legacy
        .install_wasm_procedures(&wasm, vec![binding])
        .unwrap();
    let before = legacy.cpu.snapshot();
    assert!(legacy.rebase_wasm_procedures(0x100000).is_err());
    assert!(legacy.cpu.matches(&before));
    assert_eq!(
        legacy.wasm_procedure_code_segment(),
        Some(CodeSegment::FLAT)
    );
    legacy.set_wasm_procedure_tier_accounting(true).unwrap();
    assert_eq!(legacy.try_wasm_procedure(128).unwrap(), None);
    assert_eq!(legacy.wasm_procedure_stats().unwrap().tier_refusals, 1);
}

#[test]
fn malformed_execution_sites_refuse_before_installation() {
    for case in 0..7 {
        let (wasm, mut binding, code) = x87_exit_fixture(true);
        let sites = binding.execution_sites.as_mut().unwrap();
        match case {
            0 => {
                sites.remove(0);
            }
            1 => sites.clear(),
            2 => sites[0].length = 0,
            3 => sites[0].length = 2, // INC is one byte and overlaps next site.
            4 => sites[1].ip = sites[0].ip,
            5 => sites.last_mut().unwrap().length = 4, // Truncated JMP length.
            _ => {
                sites.push(ExecutionSite {
                    ip: RETURN,
                    length: 2,
                });
            } // x87 is not an executed scalar site.
        }
        let mut machine = x87_exit_machine(&code);
        assert!(
            machine
                .install_wasm_procedures(&wasm, vec![binding])
                .is_err(),
            "case{case}"
        );
        assert!(!machine.wasm_procedures_active());
    }
}

#[test]
fn execution_site_holes_must_be_real_complete_four_byte_operand_slots() {
    let mut binding = binding();
    binding.entry_ip = ENTRY;
    binding.execution_sites = Some(vec![ExecutionSite {
        ip: ENTRY,
        length: 5,
    }]);
    binding.code = vec![CodePart {
        linear: ENTRY,
        expected: vec![0xb8],
    }]; // MOV EAX,imm32.
    let decoded = source_instructions(&binding).unwrap();
    assert_eq!((decoded[0].length, decoded[0].prefix_length), (5, 1));
    binding.execution_sites.as_mut().unwrap()[0].length = 4;
    assert!(source_instructions(&binding).is_none());
    // A four-byte gap covering part of ModR/M/opcode is not an operand slot.
    binding.execution_sites.as_mut().unwrap()[0].length = 6;
    binding.code = vec![
        CodePart {
            linear: ENTRY,
            expected: vec![0x8b],
        },
        CodePart {
            linear: ENTRY + 5,
            expected: vec![0],
        },
    ];
    assert!(source_instructions(&binding).is_none());
    // Two adjacent disp32/imm32 holes are reconstructed without any live RAM read.
    binding.execution_sites.as_mut().unwrap()[0].length = 10;
    binding.code = vec![CodePart {
        linear: ENTRY,
        expected: vec![0xc7, 0x05],
    }];
    let decoded = source_instructions(&binding).unwrap();
    assert_eq!((decoded[0].length, decoded[0].prefix_length), (10, 2));
}

#[test]
fn execution_sites_preserve_finite_terminal_and_copy_contract_invariants() {
    let (wasm, mut binding, code) = x87_exit_fixture(true);
    binding.finite_computation = Some(FiniteComputation {
        exits: vec![ENTRY + 1],
        returns: vec![],
        guards: vec![],
    });
    let mut machine = x87_exit_machine(&code);
    assert!(
        machine
            .install_wasm_procedures(&wasm, vec![binding])
            .is_err()
    );
    let (wasm, mut binding, code) = x87_exit_fixture(true);
    binding.finite_computation = Some(FiniteComputation {
        exits: vec![],
        returns: vec![RETURN],
        guards: vec![],
    });
    let mut machine = x87_exit_machine(&code);
    assert!(
        machine
            .install_wasm_procedures(&wasm, vec![binding])
            .is_err()
    );
    let (wasm, mut binding, code) = x87_exit_fixture(true);
    binding.kernel = Some(CopyKernel {
        stride: 96,
        max_work_per_call: binding.maximum_instructions,
        prefix_nops: 0,
        tail_nops: 0,
    });
    let mut machine = x87_exit_machine(&code);
    assert!(
        machine
            .install_wasm_procedures(&wasm, vec![binding])
            .is_err()
    );
}
