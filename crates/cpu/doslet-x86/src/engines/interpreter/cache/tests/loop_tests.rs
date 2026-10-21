//! Boundaries required when a block's nonfinal instructions always fall through.
use super::*;

fn pair(code: &[u8]) -> (Cpu, Flat, Cpu, Flat) {
    let mut ram = [0; 0x200];
    ram[..code.len()].copy_from_slice(code);
    let fast = prepared_cpu();
    let mut reference = prepared_cpu();
    reference.set_decode_cache(false);
    (
        fast,
        Flat { ram, generation: 0 },
        reference,
        Flat { ram, generation: 0 },
    )
}

fn same(fast: &Cpu, bus: &Flat, reference: &Cpu, reference_bus: &Flat) {
    assert!(fast.matches(&reference.snapshot()));
    assert_eq!(bus.ram, reference_bus.ram);
    assert_eq!(bus.generation, reference_bus.generation);
}

fn reference_steps(cpu: &mut Cpu, bus: &mut Flat, count: u32) {
    for _ in 0..count {
        cpu.step(bus).unwrap();
    }
}

#[test]
fn repeated_strings_end_blocks_and_preserve_partial_and_deadline_budgets() {
    for count in [0, 1, 5] {
        for backward in [false, true] {
            for (budget, deadline) in [(2, 8), (3, 8), (8, 3), (8, 8)] {
                let (mut fast, mut bus, mut reference, mut reference_bus) =
                    pair(&[0xF3, 0xA4, 0x40, 0xFA]); // REP MOVSB; INC EAX; CLI
                for (cpu, bus) in [(&mut fast, &mut bus), (&mut reference, &mut reference_bus)] {
                    cpu.registers.set32(Gpr32::Ecx, count);
                    cpu.registers
                        .set32(Gpr32::Esi, if backward { 0x104 } else { 0x100 });
                    cpu.registers
                        .set32(Gpr32::Edi, if backward { 0x144 } else { 0x140 });
                    if backward {
                        cpu.flags |= flags::DIRECTION;
                    }
                    bus.ram[0x100..0x105].copy_from_slice(&[1, 2, 3, 4, 5]);
                }
                let expected = budget.min(deadline).min(count.max(1) + 1);
                assert_eq!(
                    fast.interpret_batch(&mut bus, budget, |_| deadline),
                    expected
                );
                reference_steps(&mut reference, &mut reference_bus, expected);
                same(&fast, &bus, &reference, &reference_bus);
                assert_eq!(fast.decode_cache.blocks[0].count, 1);
            }
        }
    }
}

#[test]
fn repeated_strings_preserve_selected_head_and_fallthrough_yields() {
    for kind in 0..3 {
        for head in [0, 2] {
            let (mut fast, mut bus, mut reference, mut reference_bus) =
                pair(&[0xF3, 0xA4, 0x40, 0xFA]);
            for cpu in [&mut fast, &mut reference] {
                cpu.registers.set32(Gpr32::Ecx, 5);
                cpu.registers.set32(Gpr32::Esi, 0x100);
                cpu.registers.set32(Gpr32::Edi, 0x140);
            }
            let completed = match kind {
                0 => fast.interpret_batch_until_block(&mut bus, 16, |_| 16, head),
                1 => fast.interpret_batch_until_entries(&mut bus, 16, |_| 16, &[head]),
                _ => {
                    fast.set_prepared_entries(alloc::vec![head]).unwrap();
                    fast.interpret_batch_until_prepared_entry(&mut bus, 16, |_| 16)
                }
            };
            assert_eq!(completed, if head == 0 { 1 } else { 5 });
            assert_eq!(fast.registers.ip, head);
            reference_steps(&mut reference, &mut reference_bus, completed);
            same(&fast, &bus, &reference, &reference_bus);
        }
    }
}

#[test]
fn final_control_transfers_resume_at_the_actual_successor() {
    for code in [
        &[0xE9, 0x1B, 0, 0, 0][..], // JMP 20h
        &[0xE8, 0x1B, 0, 0, 0],     // CALL 20h
        &[0xFF, 0xE0],              // JMP EAX
        &[0xFF, 0xD0],              // CALL EAX
        &[0xC3],                    // RET
        &[0x74, 0x1E],              // JE 20h
    ] {
        for taken in [false, true] {
            let (mut fast, mut bus, mut reference, mut reference_bus) = pair(code);
            for (cpu, bus) in [(&mut fast, &mut bus), (&mut reference, &mut reference_bus)] {
                cpu.registers.set32(Gpr32::Eax, 0x20);
                cpu.flags = (cpu.flags & !flags::ZERO) | if taken { flags::ZERO } else { 0 };
                bus.ram[code.len()..code.len() + 2].copy_from_slice(&[0x41, 0xFA]);
                bus.ram[0x20..0x22].copy_from_slice(&[0x43, 0xFA]);
                bus.ram[0x1F0..0x1F4].copy_from_slice(&0x20_u32.to_le_bytes());
            }
            assert_eq!(fast.interpret_batch(&mut bus, 8, |_| 8), 2);
            reference_steps(&mut reference, &mut reference_bus, 2);
            same(&fast, &bus, &reference, &reference_bus);
            assert_eq!(fast.decode_cache.blocks[0].count, 1);
        }
    }
}

#[test]
fn eip_wrap_ends_a_block_before_a_selected_low_entry() {
    for selected_entry in [false, true] {
        let (mut fast, mut bus, mut reference, mut reference_bus) = pair(&[]);
        for (cpu, bus) in [(&mut fast, &mut bus), (&mut reference, &mut reference_bus)] {
            let cs = cpu.registers.segment_cache(SegmentRegister::Cs);
            cpu.registers
                .set_segment(SegmentRegister::Cs, SegmentCache { base: 0x100, ..cs });
            cpu.registers.ip = u32::MAX - 1;
            // CS.base wraps the high EIP into low RAM. INC EBX wraps EIP to 0.
            bus.ram[0xFE..0x102].copy_from_slice(&[0x40, 0x43, 0x41, 0xFA]);
        }
        let count = if selected_entry {
            fast.interpret_batch_until_entries(&mut bus, 8, |_| 8, &[0])
        } else {
            fast.interpret_batch(&mut bus, 8, |_| 8)
        };
        assert_eq!(count, if selected_entry { 2 } else { 3 });
        reference_steps(&mut reference, &mut reference_bus, count);
        same(&fast, &bus, &reference, &reference_bus);
        assert_eq!(fast.decode_cache.blocks[0xFE].count, 2);
    }
}

#[test]
fn structural_code_changes_redecode_before_remaining_or_next_block_instructions() {
    // The first store turns a five-byte MOV into NOP, exposing INC at the next byte.
    let (mut fast, mut bus, mut reference, mut reference_bus) =
        pair(&[0xC6, 0x05, 7, 0, 0, 0, 0x90, 0xB8, 0x40, 0xFA, 0x90, 0x90]);
    assert_eq!(fast.interpret_batch(&mut bus, 3, |_| 3), 3);
    reference_steps(&mut reference, &mut reference_bus, 3);
    same(&fast, &bus, &reference, &reference_bus);
    assert_eq!(fast.registers.ip, 9);
    assert_eq!(fast.registers.get32(Gpr32::Eax), 1);

    for split_budget in [false, true] {
        // The eighth instruction changes an already cached following block.
        let (mut fast, mut bus, mut reference, mut reference_bus) = pair(&[
            0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0xC6, 0x05, 14, 0, 0, 0, 0x90, 0xB8, 0x40,
            0xFA, 0x90, 0x90,
        ]);
        fast.registers.ip = 14;
        reference.registers.ip = 14;
        assert_eq!(fast.interpret_batch(&mut bus, 2, |_| 2), 1);
        reference_steps(&mut reference, &mut reference_bus, 1);
        for cpu in [&mut fast, &mut reference] {
            cpu.registers.ip = 0;
            cpu.registers.set32(Gpr32::Eax, 0);
        }
        if split_budget {
            assert_eq!(fast.interpret_batch(&mut bus, 8, |_| 8), 8);
            assert_eq!(fast.registers.ip, 14);
            assert_eq!(fast.interpret_batch(&mut bus, 2, |_| 2), 2);
        } else {
            assert_eq!(fast.interpret_batch(&mut bus, 10, |_| 10), 10);
        }
        reference_steps(&mut reference, &mut reference_bus, 10);
        same(&fast, &bus, &reference, &reference_bus);
        assert_eq!(fast.registers.ip, 16);
        assert_eq!(fast.registers.get32(Gpr32::Eax), 8);
    }
}

#[test]
fn a_declined_middle_instruction_preserves_the_completed_prefix() {
    let (mut fast, mut bus, mut reference, mut reference_bus) =
        pair(&[0x40, 0x8B, 0x05, 0, 2, 0, 0]); // INC; MOV EAX, [unmapped 200h]
    assert_eq!(fast.interpret_batch(&mut bus, 8, |_| 8), 1);
    reference_steps(&mut reference, &mut reference_bus, 1);
    same(&fast, &bus, &reference, &reference_bus);
    assert_eq!(fast.registers.ip, 1);
    assert_eq!(fast.registers.get32(Gpr32::Eax), 1);
}
