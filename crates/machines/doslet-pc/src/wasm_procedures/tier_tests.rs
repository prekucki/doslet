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
