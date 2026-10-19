use super::*;
use doslet_x86_wasm::static_program as rle_api;

#[path = "../../../../../tests/fixtures/rle_procedure.rs"]
mod fixture;

const DESTINATION: u32 = 0x6ffe;
const COUNT: u32 = 0x5ffe;
const VALUE: u32 = 0x5004;

fn configured(n: u32, input: &[u8], initial_run: u32) -> (PcMachine, Vec<u8>, Binding) {
    let fixture = fixture::rle_fixture(ProcedureId(9), 0, ENTRY);
    let compiled = ProcedureGraphProgram {
        functions: vec![fixture.function],
    }
    .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 4096)
    .unwrap();
    assert!(compiled.rle_kernel(ProcedureId(9)).is_some());
    let mut machine = machine();
    let sp = STACK + 2; // Exercise unaligned stack and cross-page control writes.
    machine.cpu.registers.set32(Gpr32::Esp, sp);
    machine.cpu.registers.set32(Gpr32::Eax, DATA);
    machine.cpu.registers.set32(Gpr32::Ebx, DESTINATION);
    machine.cpu.registers.set32(Gpr32::Ecx, n);
    machine.cpu.registers.set32(Gpr32::Esi, 0x1234abcd);
    machine.cpu.registers.set32(Gpr32::Edi, 0x87654321);
    machine.cpu.registers.set32(Gpr32::Ebp, 0x13572468);
    for (address, value) in [
        (sp, RETURN),
        (sp + 4, COUNT),
        (sp + 8, VALUE),
        (COUNT, initial_run),
        (VALUE, 0x123456ab),
    ] {
        machine.bus.write_u32(address, value).unwrap();
    }
    machine
        .bus
        .debug_ram_mut(ENTRY, fixture.code.len() as u32)
        .unwrap()
        .copy_from_slice(&fixture.code);
    machine
        .bus
        .debug_ram_mut(DATA, input.len().max(1) as u32)
        .unwrap()[..input.len()]
        .copy_from_slice(input);
    let binding = Binding {
        function_id: 9,
        entry_ip: ENTRY,
        maximum_instructions: u32::MAX,
        kernel: None,
        rle_kernel: Some(RleKernel),
        finite_computation: None,
        kernel_interrupts: KernelInterrupts::Defer,
        code: vec![CodePart {
            linear: ENTRY,
            expected: fixture.code,
        }],
    };
    (machine, compiled.into_wasm(), binding)
}

#[test]
fn whole_rle_matches_interpretation_including_cross_page_generations_and_stack() {
    let runs: Vec<u8> = (0..11).flat_map(|i| [0xff, i as u8]).collect();
    for (n, input, initial) in [
        (640, runs.as_slice(), 0),
        (4, &[0xc0, 99, 1, 0xc2, 7, 3][..], 0),
        (6, &[][..], 12),
        (4, &[1, 2, 3, 4][..], u32::MAX),
        (4, &[0xff, 0x7f][..], 0),
    ] {
        let (mut fast, wasm, binding) = configured(n, input, initial);
        let (mut reference, _, _) = configured(n, input, initial);
        let mut original_work = 0;
        while reference.cpu.registers.ip != RETURN {
            reference_steps(&mut reference, 1);
            original_work += 1;
            assert!(original_work < 20_000);
        }
        fast.install_wasm_procedures(&wasm, vec![binding]).unwrap();
        assert_eq!(fast.step_with_idle_budget(1).unwrap().1, original_work);
        same_state(&mut fast, &mut reference);
        assert_eq!(
            fast.bus.ram.storage.generations(),
            reference.bus.ram.storage.generations()
        );
        assert_eq!(fast.wasm_procedure_stats().unwrap().kernel_calls, 1);
        assert_eq!(fast.wasm_procedure_stats().unwrap().budget_refusals, 0);
    }
}

#[test]
fn rle_reference_and_wasm_share_the_whole_call_hardware_policy() {
    let input: Vec<u8> = (0..11).flat_map(|i| [0xff, i as u8]).collect();
    let (mut fast, wasm, binding) = configured(640, &input, 0);
    let (mut reference, _, reference_binding) = configured(640, &input, 0);
    fast.install_wasm_procedures(&wasm, vec![binding]).unwrap();
    reference
        .install_interpreted_procedure_boundaries(vec![reference_binding])
        .unwrap();
    let work = fast.step_with_idle_budget(1).unwrap().1;
    assert!(work > 4096);
    assert_eq!(reference.step_with_idle_budget(1).unwrap().1, work);
    same_state(&mut fast, &mut reference);
    assert_eq!(
        reference
            .wasm_procedure_stats()
            .unwrap()
            .interpreted_instructions,
        u64::from(work)
    );
}

#[test]
fn rle_value_pointer_uses_ss_while_source_count_and_destination_use_ds() {
    let input = [0xc4, 0x7f];
    let (mut fast, wasm, binding) = configured(4, &input, 0);
    let (mut reference, _, _) = configured(4, &input, 0);
    let data_base = 0x10000;
    for machine in [&mut fast, &mut reference] {
        descriptor(&mut machine.bus, 24, data_base, false);
        machine
            .cpu
            .load_segment(&mut machine.bus, SegmentRegister::Ds, 24)
            .unwrap();
        machine
            .bus
            .debug_ram_mut(data_base + DATA, input.len() as u32)
            .unwrap()
            .copy_from_slice(&input);
        machine.bus.write_u32(data_base + COUNT, 0).unwrap();
        // The EBP-based value operand defaults to SS even with a nonflat DS.
        machine
            .bus
            .write_u32(data_base + VALUE, 0xdeadbeef)
            .unwrap();
    }
    let mut work = 0;
    while reference.cpu.registers.ip != RETURN {
        reference_steps(&mut reference, 1);
        work += 1;
        assert!(work < 20_000);
    }
    fast.install_wasm_procedures(&wasm, vec![binding]).unwrap();
    assert_eq!(fast.step_with_idle_budget(1).unwrap().1, work);
    same_state(&mut fast, &mut reference);
    assert_eq!(
        fast.bus.ram.storage.generations(),
        reference.bus.ram.storage.generations()
    );
    assert_eq!(fast.wasm_procedure_stats().unwrap().kernel_calls, 1);
}

#[test]
fn admitted_whole_rle_calls_allocate_nothing() {
    let input: Vec<u8> = (0..11).flat_map(|i| [0xff, i as u8]).collect();
    let (mut machine, wasm, binding) = configured(640, &input, 0);
    machine
        .install_wasm_procedures(&wasm, vec![binding])
        .unwrap();
    machine.try_wasm_procedure(1).unwrap().unwrap();
    for _ in 0..8 {
        machine.cpu.registers.ip = ENTRY;
        machine.cpu.registers.set32(Gpr32::Esp, STACK + 2);
        machine.cpu.registers.set32(Gpr32::Eax, DATA);
        machine.cpu.registers.set32(Gpr32::Ebx, DESTINATION);
        machine.cpu.registers.set32(Gpr32::Ecx, 640);
        machine.bus.write_u32(COUNT, 0).unwrap();
        let allocation = allocation_counter::measure(|| {
            assert!(machine.try_wasm_procedure(1).unwrap().unwrap() > 4096);
        });
        assert_eq!(allocation.count_total, 0, "{allocation:?}");
        assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
    }
}

#[test]
fn rle_alias_source_code_and_generation_refusals_commit_no_effects() {
    for refusal in 0..3 {
        let (mut machine, wasm, binding) = configured(4, &[0xc4, 7], 0);
        machine
            .install_wasm_procedures(&wasm, vec![binding])
            .unwrap();
        match refusal {
            0 => machine.cpu.registers.set32(Gpr32::Ebx, COUNT),
            1 => machine.cpu.registers.set32(Gpr32::Ebx, ENTRY),
            _ => {
                machine.bus.ram.storage.generations_mut()[DESTINATION as usize / PAGE_BYTES] =
                    u64::MAX - 2
            }
        }
        let cpu = machine.cpu.snapshot();
        let ram = machine.bus.ram.storage.bytes().to_vec();
        let generations = machine.bus.ram.storage.generations().to_vec();
        assert_eq!(machine.try_wasm_procedure(1).unwrap(), None);
        assert!(machine.cpu.matches(&cpu));
        assert_eq!(machine.bus.ram.storage.bytes(), ram.as_slice());
        assert_eq!(
            machine.bus.ram.storage.generations(),
            generations.as_slice()
        );
    }
}
