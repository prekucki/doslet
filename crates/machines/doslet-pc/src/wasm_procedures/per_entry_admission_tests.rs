//! Entry admission is independent of retained compiled functions and totals.
use super::*;

const ENTRIES: &[u8] = &[0x40, 0x41, 0x42, 0xc3];

fn prepared_entries() -> PcMachine {
    let mut machine = machine();
    machine
        .bus
        .debug_ram_mut(ENTRY, ENTRIES.len() as u32)
        .unwrap()
        .copy_from_slice(ENTRIES);
    machine.bus.write_u8(ENTRY - 1, 0x90).unwrap();
    for register in [Gpr32::Eax, Gpr32::Ecx, Gpr32::Edx] {
        machine.cpu.registers.set32(register, 0);
    }
    let (wasm, bindings) = procedures();
    machine.install_wasm_procedures(&wasm, bindings).unwrap();
    machine
}

fn procedures() -> (Vec<u8>, Vec<Binding>) {
    // Function IDs deliberately differ from entry order and filtered hints.
    let ids = [30, 10, 20];
    let registers = [Register::Eax, Register::Ecx, Register::Edx];
    let mut functions = Vec::new();
    let mut bindings = Vec::new();
    for (index, id) in ids.into_iter().enumerate() {
        functions.push(ProcedureFunction {
            id: ProcedureId(id),
            entry: address(ENTRY + index as u32),
            instructions: registers
                .iter()
                .enumerate()
                .skip(index)
                .map(|(offset, &register)| ProcedureInstruction {
                    address: address(ENTRY + offset as u32),
                    operation: Operation::Inc(register),
                })
                .collect(),
            terminator: ProcedureReturn {
                address: address(ENTRY + 3),
                width: Width::Dword,
                pop_bytes: 0,
            },
        });
        let mut binding = binding();
        binding.function_id = id;
        binding.entry_ip = ENTRY + index as u32;
        binding.maximum_instructions = 4 - index as u32;
        binding.code = vec![CodePart {
            linear: binding.entry_ip,
            expected: ENTRIES[index..].to_vec(),
        }];
        bindings.push(binding);
    }
    (
        ProcedureProgram { functions }
            .compile(&ProcedureCompileOptions::default())
            .unwrap()
            .into_wasm(),
        bindings,
    )
}

fn reset_entry(machine: &mut PcMachine, index: usize) {
    machine.cpu.registers.ip = ENTRY + index as u32;
    machine.cpu.registers.set32(Gpr32::Esp, STACK);
}

#[test]
fn per_entry_admission_maps_filtered_hints_and_removes_interpreter_fences() {
    let mut fast = prepared_entries();
    for index in 0..3 {
        assert_eq!(fast.wasm_procedure_entry_enabled(index), Some(true));
    }
    fast.set_wasm_procedure_entries_enabled(&[false, false, true])
        .unwrap();
    let mut reference = machine();
    reference
        .bus
        .debug_ram_mut(ENTRY, ENTRIES.len() as u32)
        .unwrap()
        .copy_from_slice(ENTRIES);
    reference.bus.write_u8(ENTRY - 1, 0x90).unwrap();
    for register in [Gpr32::Eax, Gpr32::Ecx, Gpr32::Edx] {
        reference.cpu.registers.set32(register, 0);
    }
    fast.cpu.registers.ip = ENTRY - 1;
    reference.cpu.registers.ip = ENTRY - 1;
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 3);
    reference_steps(&mut reference, 3);
    assert_eq!(fast.cpu.registers.ip, ENTRY + 2);
    assert_eq!(fast.cpu.prepared_entry_index(), Some(0));
    same_state(&mut fast, &mut reference);
    let allocation = allocation_counter::measure(|| {
        assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(2));
    });
    assert_eq!((allocation.count_total, allocation.bytes_total), (0, 0));
    reference_steps(&mut reference, 2);
    same_state(&mut fast, &mut reference);
    assert_eq!(fast.wasm_procedure_entry_count(), 3);
    assert_eq!(fast.wasm_procedure_entry_stats(0).unwrap().calls, 0);
    assert_eq!(fast.wasm_procedure_entry_stats(1).unwrap().calls, 0);
    assert_eq!(fast.wasm_procedure_entry_stats(2).unwrap().calls, 1);
    assert_eq!(fast.cpu.registers.get32(Gpr32::Edx), 1);

    // Retain accumulated totals when toggling admission. Filter index zero
    // now names binding one and must execute its complete compiled suffix.
    fast.set_wasm_procedure_entries_enabled(&[false, true, false])
        .unwrap();
    reset_entry(&mut fast, 1);
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(3));
    assert_eq!(fast.wasm_procedure_entry_stats(1).unwrap().calls, 1);
    assert_eq!(fast.wasm_procedure_entry_stats(2).unwrap().calls, 1);
    assert_eq!(fast.cpu.registers.get32(Gpr32::Ecx), 2);
    assert_eq!(fast.cpu.registers.get32(Gpr32::Edx), 2);
}

#[test]
fn per_entry_admission_rejects_bad_selections_atomically_and_can_reenable_all() {
    let mut fast = prepared_entries();
    fast.set_wasm_procedure_entries_enabled(&[false, true, false])
        .unwrap();
    reset_entry(&mut fast, 1);
    assert_eq!(fast.cpu.prepared_entry_index(), Some(0));
    for selection in [&[][..], &[true], &[true, true, true, true]] {
        assert!(fast.set_wasm_procedure_entries_enabled(selection).is_err());
        assert_eq!(fast.cpu.prepared_entry_index(), Some(0));
        assert_eq!(
            (0..3)
                .map(|index| fast.wasm_procedure_entry_enabled(index).unwrap())
                .collect::<Vec<_>>(),
            [false, true, false]
        );
    }
    assert_eq!(fast.wasm_procedure_entry_enabled(3), None);
    assert!(fast.set_wasm_procedure_entry_enabled(3, true).is_err());
    fast.set_wasm_procedure_entries_enabled(&[false; 3])
        .unwrap();
    assert!(!fast.cpu.has_prepared_entries());
    for index in 0..3 {
        reset_entry(&mut fast, index);
        assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    }
    fast.set_wasm_procedures_suspended(true).unwrap();
    fast.set_wasm_procedures_suspended(false).unwrap();
    assert!(!fast.cpu.has_prepared_entries());
    fast.set_wasm_procedure_entries_enabled(&[true; 3]).unwrap();
    reset_entry(&mut fast, 0);
    assert_eq!(fast.try_wasm_procedure(1).unwrap(), None); // Existing budget proof.
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(4));
    assert_eq!(fast.wasm_procedure_stats().unwrap().budget_refusals, 1);
    fast.set_wasm_procedure_entry_enabled(0, false).unwrap();
    reset_entry(&mut fast, 0);
    assert_eq!(fast.cpu.prepared_entry_index(), None);
    fast.set_wasm_procedure_entry_enabled(0, true).unwrap();
    assert_eq!(fast.cpu.prepared_entry_index(), Some(0));
    fast.bus.debug_ram_mut(ENTRY, 1).unwrap()[0] ^= 1;
    let before = fast.cpu.snapshot();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None); // Existing source proof.
    assert!(fast.cpu.matches(&before));
}

#[test]
fn per_entry_admission_respects_suspension_and_rebase_and_replacement_defaults() {
    let mut fast = prepared_entries();
    fast.set_wasm_procedures_suspended(true).unwrap();
    fast.set_wasm_procedure_entries_enabled(&[false, true, false])
        .unwrap();
    assert!(!fast.cpu.has_prepared_entries());
    reset_entry(&mut fast, 1);
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    fast.set_wasm_procedures_suspended(false).unwrap();
    assert_eq!(fast.cpu.prepared_entry_index(), Some(0));
    assert!(fast.rebase_wasm_procedures(u32::MAX).is_err());
    assert_eq!(fast.wasm_procedure_entry_enabled(1), Some(true));
    const BASE: u32 = 0x100000;
    fast.bus
        .debug_ram_mut(BASE + ENTRY, ENTRIES.len() as u32)
        .unwrap()
        .copy_from_slice(ENTRIES);
    fast.bus.write_u8(BASE + RETURN, 0x90).unwrap();
    fast.rebase_wasm_procedures(BASE).unwrap();
    descriptor(&mut fast.bus, 24, BASE, true);
    fast.cpu
        .load_segment(&mut fast.bus, SegmentRegister::Cs, 24)
        .unwrap();
    reset_entry(&mut fast, 1);
    assert_eq!(fast.cpu.prepared_entry_index(), Some(0));
    assert_eq!(fast.wasm_procedure_entry_enabled(0), Some(false));
    assert_eq!(fast.wasm_procedure_entry_enabled(2), Some(false));
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(3));
    assert_eq!(fast.wasm_procedure_entry_stats(1).unwrap().calls, 1);

    struct NoCache;
    impl CompiledCache for NoCache {
        fn load(&mut self, _: &[u8], _: u64) -> Option<Vec<u8>> {
            None
        }
        fn store(&mut self, _: &[u8], _: u64, _: &[u8]) {}
    }
    let (wasm, mut bindings) = procedures();
    for binding in &mut bindings {
        binding.code_segment.base = BASE;
        for part in &mut binding.code {
            part.linear += BASE;
        }
    }
    fast.replace_wasm_procedures_cached(&wasm, bindings, &mut NoCache)
        .unwrap();
    for index in 0..3 {
        assert_eq!(fast.wasm_procedure_entry_enabled(index), Some(true));
        assert_eq!(fast.wasm_procedure_entry_stats(index).unwrap().calls, 0);
    }
}

#[cfg(feature = "wasm-procedure-diagnostics")]
#[test]
fn per_entry_admission_combines_diagnostic_and_suspension_policies() {
    let mut fast = prepared_entries();
    fast.set_wasm_procedure_entries_enabled(&[false, true, false])
        .unwrap();
    fast.set_wasm_procedure_diagnostic_mode(DiagnosticMode::SharedWithoutEntries)
        .unwrap();
    assert!(!fast.cpu.has_prepared_entries());
    fast.set_wasm_procedures_suspended(true).unwrap();
    fast.set_wasm_procedure_diagnostic_mode(DiagnosticMode::Normal)
        .unwrap();
    assert!(!fast.cpu.has_prepared_entries());
    fast.set_wasm_procedure_entries_enabled(&[true, false, false])
        .unwrap();
    assert!(!fast.cpu.has_prepared_entries());
    fast.set_wasm_procedures_suspended(false).unwrap();
    reset_entry(&mut fast, 0);
    assert_eq!(fast.cpu.prepared_entry_index(), Some(0));
    reset_entry(&mut fast, 1);
    assert_eq!(fast.cpu.prepared_entry_index(), None);
    fast.set_wasm_procedure_diagnostic_mode(DiagnosticMode::SharedPcBus)
        .unwrap();
    fast.set_wasm_procedures_suspended(true).unwrap();
    fast.set_wasm_procedures_suspended(false).unwrap();
    assert!(!fast.cpu.has_prepared_entries());
}
