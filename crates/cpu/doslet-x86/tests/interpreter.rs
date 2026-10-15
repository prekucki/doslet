use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_x86::{Cpu, Gpr16, Gpr32, StepOutcome, flags};

struct TestBus {
    ram: Box<[u8]>,
    writes: usize,
    ports: [u8; 256],
}
#[test]
fn x87_fst_and_fstp_decode_all_registers_and_preserve_retained_precision() {
    for pop in [false, true] {
        for index in 0..8 {
            let mut bus = TestBus::new(&[0xDD, (if pop { 0xD8 } else { 0xD0 }) + index]);
            let mut cpu = Cpu::default().with_x87();
            let exact = (1_i64 << 60) + 1;
            let fpu = cpu.fpu.as_mut().unwrap();
            fpu.push_integer(exact).unwrap();
            fpu.load_control(0x007F); // Single-precision PC must not round register copies.
            let flags = cpu.flags;
            cpu.step(&mut bus).unwrap();
            assert_eq!(cpu.registers.ip, 2);
            assert_eq!(cpu.flags, flags);
            assert_eq!(bus.writes, 0);
            let fpu = cpu.fpu.as_mut().unwrap();
            assert_eq!((fpu.status() >> 11) & 7, if pop { 0 } else { 7 });
            if pop && index == 0 {
                assert_eq!(fpu.tags(), 0xFFFF); // DD D8 discards precisely ST(0).
            } else {
                if pop {
                    fpu.duplicate(index - 1).unwrap();
                } else {
                    fpu.exchange(index).unwrap();
                }
                assert_eq!(fpu.store_integer(64), Ok(exact as u64));
            }
        }
    }
}
#[test]
fn x87_register_arithmetic_decodes_without_popping_the_source() {
    for (modrm, expected) in [(0xC1, 7.5), (0xC9, 12.5), (0xE1, 2.5), (0xE9, -2.5)] {
        let mut bus = TestBus::new(&[0xD8, modrm]);
        let mut cpu = Cpu::default().with_x87();
        cpu.fpu.as_mut().unwrap().push(2.5).unwrap();
        cpu.fpu.as_mut().unwrap().push(5.0).unwrap();
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.fpu.as_ref().unwrap().store_value(), Ok(expected));
        assert_eq!(cpu.fpu.as_ref().unwrap().st(1), Ok(2.5));
        assert_eq!(cpu.fpu.as_ref().unwrap().status() & 0x3800, 0x3000);
    }
}

#[test]
fn x87_faddp_updates_the_destination_and_pops_only_after_success() {
    let mut bus = TestBus::new(&[0xDE, 0xC1]);
    let mut cpu = Cpu::default().with_x87();
    cpu.fpu.as_mut().unwrap().push_integer(1_i64 << 60).unwrap();
    cpu.fpu.as_mut().unwrap().push_integer(1).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(
        cpu.fpu.as_mut().unwrap().store_integer(64),
        Ok((1_u64 << 60) + 1)
    );
    assert_eq!(cpu.fpu.as_ref().unwrap().status() & 0x3800, 0x3800);
    assert!(cpu.fpu.as_ref().unwrap().st(1).is_err());

    let mut bus = TestBus::new(&[0xDE, 0xC1]);
    let mut cpu = Cpu::default().with_x87();
    let fpu = cpu.fpu.as_mut().unwrap();
    fpu.push(1.0).unwrap();
    fpu.push(1.0 / 3.0).unwrap();
    fpu.load_control(0x005F); // Single precision, unmasked precision exception.
    assert!(cpu.step(&mut bus).is_err());
    assert_eq!(cpu.fpu.as_ref().unwrap().status() & 0x3800, 0x3000);
    assert_eq!(cpu.fpu.as_ref().unwrap().st(1), Ok(1.0));
}

#[test]
fn bit_scans_cover_both_widths_zero_inputs_and_one_hot_positions() {
    for dword in [false, true] {
        for opcode in [0xBC, 0xBD] {
            for bit in 0..if dword { 32 } else { 16 } {
                let mut code = Vec::new();
                if dword {
                    code.push(0x66);
                }
                code.extend_from_slice(&[0x0F, opcode, 0xC2]);
                let mut bus = TestBus::new(&code);
                let mut cpu = Cpu::default();
                cpu.registers.set32(Gpr32::Eax, 0xABCD_FFFF);
                cpu.registers.set32(Gpr32::Edx, 1 << bit);
                cpu.flags |= flags::ZERO;
                cpu.step(&mut bus).unwrap();
                assert_eq!(
                    cpu.registers.get32(Gpr32::Eax),
                    if dword { bit } else { 0xABCD0000 | bit }
                );
                assert_eq!(cpu.flags & flags::ZERO, 0);
                cpu.registers.ip = 0;
                cpu.registers.set32(Gpr32::Edx, 0);
                let before = cpu.registers.get32(Gpr32::Eax);
                cpu.step(&mut bus).unwrap();
                assert_eq!(cpu.registers.get32(Gpr32::Eax), before);
                assert_ne!(cpu.flags & flags::ZERO, 0);
            }
        }
    }
    let mut bus = TestBus::new(&[0x0F, 0xBD, 0x06, 0, 2]);
    bus.write_u16(0x200, 0x8100).unwrap();
    let mut cpu = Cpu::default();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get(Gpr16::Ax), 15);
}

#[test]
fn fistp_writes_each_integer_width_and_unmasked_invalid_does_not_store_or_pop() {
    for (opcode, modrm, width) in [(0xDF, 0x1E, 2), (0xDB, 0x1E, 4), (0xDF, 0x3E, 8)] {
        let mut bus = TestBus::new(&[opcode, modrm, 0, 2]);
        let mut cpu = Cpu::default().with_x87();
        cpu.fpu.as_mut().unwrap().push(-1.5).unwrap();
        cpu.step(&mut bus).unwrap();
        assert_eq!(
            &bus.ram[0x200..0x200 + width],
            &(-2_i64).to_le_bytes()[..width]
        );
        assert_eq!(cpu.fpu.as_ref().unwrap().tags(), 0xFFFF);
    }
    let mut bus = TestBus::new(&[0xDF, 0x1E, 0, 2]);
    bus.ram[0x200..0x202].fill(0xAA);
    let mut cpu = Cpu::default().with_x87();
    cpu.fpu.as_mut().unwrap().push(32768.0).unwrap();
    cpu.fpu.as_mut().unwrap().load_control(0x037E);
    assert!(matches!(
        cpu.step(&mut bus),
        Err(doslet_x86::CpuError::X87Arithmetic { .. })
    ));
    assert_eq!(&bus.ram[0x200..0x202], &[0xAA; 2]);
    assert_eq!(cpu.fpu.as_ref().unwrap().st0(), Ok(32768.0));
}
impl TestBus {
    fn new(code: &[u8]) -> Self {
        let mut ram = vec![0; 1024 * 1024].into_boxed_slice();
        ram[..code.len()].copy_from_slice(code);
        Self {
            ram,
            writes: 0,
            ports: [0; 256],
        }
    }
}
impl MemoryBus for TestBus {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        self.ram
            .get((address & 0xFFFFF) as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(address))
    }
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        *self
            .ram
            .get_mut((address & 0xFFFFF) as usize)
            .ok_or(BusError::UnmappedMemory(address))? = value;
        self.writes += 1;
        Ok(())
    }
}
impl PortBus for TestBus {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
        self.ports
            .get(usize::from(port))
            .copied()
            .ok_or(BusError::UnmappedPort(port))
    }
    fn write_port_u8(&mut self, port: u16, value: u8) -> Result<(), BusError> {
        *self
            .ports
            .get_mut(usize::from(port))
            .ok_or(BusError::UnmappedPort(port))? = value;
        Ok(())
    }
}

#[test]
fn double_shifts_match_bit_serial_results_flags_and_operand_widths() {
    for dword in [false, true] {
        let width = if dword { 32 } else { 16 };
        let mask = if dword { u32::MAX } else { 0xFFFF };
        for opcode in [0xA4, 0xA5, 0xAC, 0xAD] {
            for raw_count in [0u8, 1, 4, 15, 16, 31, 32, 33] {
                let count = raw_count & 31;
                if count > width {
                    continue;
                }
                for (original, source) in [
                    (0u32, 0u32),
                    (u32::MAX, 0),
                    (0x8000_8001, 0x1234_5678),
                    (0x1234_ABCD, 0x8765_4321),
                ] {
                    let mut code = Vec::new();
                    if dword {
                        code.push(0x66);
                    }
                    code.extend_from_slice(&[0x0F, opcode, 0xD0]); // Destination AX/EAX, source DX/EDX.
                    if opcode & 1 == 0 {
                        code.push(raw_count);
                    }
                    let mut bus = TestBus::new(&code);
                    let mut cpu = Cpu::default();
                    cpu.registers.set32(Gpr32::Eax, original);
                    cpu.registers.set32(Gpr32::Edx, source);
                    cpu.registers
                        .set32(Gpr32::Ecx, 0xFACE_1200 | u32::from(raw_count));
                    let previous = 2
                        | flags::CARRY
                        | flags::OVERFLOW
                        | flags::AUXILIARY
                        | flags::DIRECTION
                        | flags::INTERRUPT;
                    cpu.flags = previous;
                    let mut expected = original & mask;
                    let mut feed = source & mask;
                    let mut carry = 1;
                    for _ in 0..count {
                        if opcode & 8 != 0 {
                            carry = expected & 1;
                            expected = (expected >> 1) | ((feed & 1) << (width - 1));
                            feed >>= 1;
                        } else {
                            carry = (expected >> (width - 1)) & 1;
                            expected = ((expected << 1) & mask) | ((feed >> (width - 1)) & 1);
                            feed = (feed << 1) & mask;
                        }
                    }
                    cpu.step(&mut bus).unwrap();
                    assert_eq!(
                        cpu.registers.get32(Gpr32::Eax),
                        (original & !mask) | expected
                    );
                    assert_eq!(cpu.registers.get32(Gpr32::Edx), source);
                    assert_eq!(
                        cpu.registers.get32(Gpr32::Ecx),
                        0xFACE_1200 | u32::from(raw_count)
                    );
                    if count == 0 {
                        assert_eq!(cpu.flags, previous);
                        continue;
                    }
                    assert_eq!(cpu.flags & flags::CARRY, carry);
                    assert_eq!(cpu.flags & flags::ZERO != 0, expected == 0);
                    assert_eq!(
                        cpu.flags & flags::SIGN != 0,
                        expected & (1 << (width - 1)) != 0
                    );
                    assert_eq!(
                        cpu.flags & flags::PARITY != 0,
                        (expected as u8).count_ones().is_multiple_of(2)
                    );
                    let unaffected = flags::AUXILIARY | flags::DIRECTION | flags::INTERRUPT;
                    assert_eq!(cpu.flags & unaffected, previous & unaffected);
                    if count == 1 {
                        assert_eq!(
                            cpu.flags & flags::OVERFLOW != 0,
                            (original ^ expected) & (1 << (width - 1)) != 0
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn double_shifts_use_segment_and_address_overrides_for_memory_destinations() {
    let mut bus = TestBus::new(&[0x26, 0x0F, 0xAC, 0x15, 4]);
    let mut cpu = Cpu::default();
    cpu.registers.es = 0x2000;
    cpu.registers.set(Gpr16::Di, 0x100);
    cpu.registers.set(Gpr16::Dx, 0x000F);
    bus.write_u16(0x20100, 0x1234).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(bus.read_u16(0x20100).unwrap(), 0xF123);
    let mut bus = TestBus::new(&[0x67, 0x66, 0x0F, 0xAC, 0x54, 0xB3, 0x10, 8]);
    let mut cpu = Cpu::default();
    cpu.registers.ds = 0x1000;
    cpu.registers.set32(Gpr32::Ebx, 0x200);
    cpu.registers.set32(Gpr32::Esi, 0x20);
    cpu.registers.set32(Gpr32::Edx, 0xAABB_CCDD);
    bus.ram[0x10290..0x10294].copy_from_slice(&0x1234_5678u32.to_le_bytes());
    cpu.step(&mut bus).unwrap();
    assert_eq!(&bus.ram[0x10290..0x10294], &0xDD12_3456u32.to_le_bytes());
}

#[test]
fn undefined_word_double_shift_counts_stop_without_changing_registers() {
    let mut bus = TestBus::new(&[0x0F, 0xAC, 0xD0, 17]);
    let mut cpu = Cpu::default();
    cpu.registers.set32(Gpr32::Eax, 0x1234_5678);
    let previous = cpu.flags;
    assert!(matches!(
        cpu.step(&mut bus),
        Err(doslet_x86::CpuError::UnsupportedOperand { .. })
    ));
    assert_eq!(cpu.registers.get32(Gpr32::Eax), 0x1234_5678);
    assert_eq!(cpu.flags, previous);
}

#[test]
fn repeated_port_strings_yield_wrap_word_offsets_and_preserve_flags() {
    let mut bus = TestBus::new(&[0xF3, 0x6E]);
    let mut cpu = Cpu::default();
    cpu.registers.ds = 0x1000;
    cpu.registers.set32(Gpr32::Esi, 0xABCD_FFFF);
    cpu.registers.set32(Gpr32::Ecx, 0x1234_0002);
    cpu.registers.set(Gpr16::Dx, 0x10);
    bus.ram[0x1FFFF] = 0x12;
    bus.ram[0x10000] = 0x34;
    let flags = cpu.flags;
    cpu.step(&mut bus).unwrap();
    assert_eq!(bus.ports[0x10], 0x12);
    assert_eq!(cpu.registers.get32(Gpr32::Esi), 0xABCD_0000);
    assert_eq!(cpu.registers.ip, 0);
    cpu.step(&mut bus).unwrap();
    assert_eq!(bus.ports[0x10], 0x34);
    assert_eq!(cpu.registers.get32(Gpr32::Ecx), 0x1234_0000);
    assert_eq!(cpu.registers.ip, 2);
    assert_eq!(cpu.flags, flags);
    cpu.registers.ip = 0;
    cpu.registers.set(Gpr16::Dx, 0xFFFF); // Zero REP count avoids invalid port access.
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.ip, 2);
}

#[test]
fn port_strings_support_word_dword_input_output_and_direction() {
    let mut bus = TestBus::new(&[0x6D, 0x66, 0x6F]);
    let mut cpu = Cpu::default();
    cpu.registers.es = 0x1000;
    cpu.registers.ds = 0x1000;
    cpu.registers.set(Gpr16::Di, 0x100);
    cpu.registers.set(Gpr16::Si, 0x100);
    cpu.registers.set(Gpr16::Dx, 0x10);
    cpu.flags |= flags::DIRECTION;
    bus.ports[0x10] = 0x12;
    bus.ports[0x11] = 0x34;
    bus.ram[0x10102] = 0x56;
    bus.ram[0x10103] = 0x78;
    cpu.step(&mut bus).unwrap();
    assert_eq!(&bus.ram[0x10100..0x10102], &[0x12, 0x34]);
    assert_eq!(cpu.registers.get(Gpr16::Di), 0xFE);
    cpu.step(&mut bus).unwrap();
    assert_eq!(&bus.ports[0x10..0x14], &[0x12, 0x34, 0x56, 0x78]);
    assert_eq!(cpu.registers.get(Gpr16::Si), 0xFC);
}

#[test]
fn decimal_adjust_matches_all_two_digit_bcd_sums_and_differences() {
    let mut bus = TestBus::new(&[0x00, 0xD8, 0x27, 0x28, 0xD8, 0x2F]); // ADD/SUB AL,BL; DAA/DAS.
    let mut cpu = Cpu::default();
    for subtract in [false, true] {
        for left in 0u32..100 {
            for right in 0u32..100 {
                let bcd = |n: u32| ((n / 10) << 4) | (n % 10);
                cpu.registers.ip = if subtract { 3 } else { 0 };
                cpu.registers.set32(Gpr32::Eax, 0x1234_AB00 | bcd(left));
                cpu.registers.set32(Gpr32::Ebx, bcd(right));
                cpu.flags = 2 | flags::INTERRUPT | flags::DIRECTION;
                cpu.step(&mut bus).unwrap();
                let overflow = cpu.flags & flags::OVERFLOW;
                cpu.step(&mut bus).unwrap();
                let result = if subtract {
                    (left + 100 - right) % 100
                } else {
                    (left + right) % 100
                };
                assert_eq!(cpu.registers.get32(Gpr32::Eax), 0x1234_AB00 | bcd(result));
                assert_eq!(
                    cpu.flags & flags::CARRY != 0,
                    if subtract {
                        left < right
                    } else {
                        left + right >= 100
                    }
                );
                assert_eq!(cpu.flags & flags::ZERO != 0, result == 0);
                assert_eq!(cpu.flags & flags::SIGN != 0, bcd(result) & 128 != 0);
                assert_eq!(
                    cpu.flags & flags::PARITY != 0,
                    (bcd(result) as u8).count_ones().is_multiple_of(2)
                );
                assert_eq!(cpu.flags & flags::OVERFLOW, overflow); // Undefined OF is retained.
                assert_eq!(
                    cpu.flags & (flags::INTERRUPT | flags::DIRECTION),
                    flags::INTERRUPT | flags::DIRECTION
                );
            }
        }
    }
}

#[test]
fn xlat_uses_unsigned_al_address_width_and_segment_override_without_changing_flags() {
    for (code, base, expected_address) in [
        (&[0xD7][..], 0xABCD_FFF0, 0xDE),        // BX+AL wraps to 16 bits.
        (&[0x66, 0xD7][..], 0xABCD_FFF0, 0xDE),  // Operand size has no effect.
        (&[0x67, 0xD7][..], 0xFFFF_FFF0, 0xDE),  // 32-bit address wrap.
        (&[0x26, 0xD7][..], 0xABCD_FFF0, 0xDE),  // ES override.
        (&[0x64, 0x67, 0xD7][..], 0x100, 0x1EE), // FS + address override.
    ] {
        let mut bus = TestBus::new(code);
        let mut cpu = Cpu::default();
        cpu.registers.ds = 0x1000;
        cpu.registers.es = 0x2000;
        cpu.registers.fs = 0x3000;
        cpu.registers.set32(Gpr32::Eax, 0x1234_ABEE);
        cpu.registers.set32(Gpr32::Ebx, base);
        cpu.flags |= flags::CARRY | flags::ZERO | flags::DIRECTION | flags::OVERFLOW;
        let before = cpu.flags;
        let segment = match code[0] {
            0x26 => 0x20000,
            0x64 => 0x30000,
            _ => 0x10000,
        };
        bus.ram[segment + expected_address] = 0x57;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.registers.get32(Gpr32::Eax), 0x1234_AB57);
        assert_eq!(cpu.registers.get32(Gpr32::Ebx), base);
        assert_eq!(cpu.flags, before);
        assert_eq!(cpu.registers.ip, code.len() as u32);
        assert_eq!(bus.writes, 0);
    }
}

#[test]
fn word_and_dword_port_accesses_preserve_aliases_width_and_flags() {
    let mut bus = TestBus::new(&[
        0x66, 0xB8, 0xD4, 0xC3, 0xB2, 0xA1, 0x66, 0xE7, 0x10, 0xB8, 0, 0, 0xE5, 0x10, 0xBA, 0x10,
        0, 0xB8, 0x34, 0x12, 0xEF, 0x66, 0xB8, 0, 0, 0, 0, 0x66, 0xED,
    ]);
    let mut cpu = Cpu::default();
    cpu.flags |= flags::CARRY | flags::OVERFLOW;
    let before = cpu.flags;
    for _ in 0..4 {
        cpu.step(&mut bus).unwrap();
    }
    assert_eq!(cpu.registers.get32(Gpr32::Eax), 0xA1B2_C3D4);
    for _ in 0..5 {
        cpu.step(&mut bus).unwrap();
    }
    assert_eq!(cpu.registers.get32(Gpr32::Eax), 0xA1B2_1234);
    assert_eq!(cpu.flags, before);
}

#[test]
fn sti_and_stack_reload_inhibit_one_instruction_and_interrupt_wakes_halt() {
    let mut bus = TestBus::new(&[0xFA, 0xFB, 0x90, 0x8E, 0xD0, 0x90, 0xF4]);
    let mut cpu = Cpu::default();
    cpu.step(&mut bus).unwrap();
    assert!(!cpu.accepts_maskable_interrupt());
    cpu.step(&mut bus).unwrap();
    assert!(!cpu.accepts_maskable_interrupt());
    cpu.step(&mut bus).unwrap();
    assert!(cpu.accepts_maskable_interrupt());
    cpu.step(&mut bus).unwrap();
    assert!(!cpu.accepts_maskable_interrupt());
    cpu.step(&mut bus).unwrap();
    assert!(cpu.accepts_maskable_interrupt());
    cpu.step(&mut bus).unwrap();
    assert!(cpu.is_halted());
    bus.write_u16(0x20, 0x100).unwrap();
    bus.write_u16(0x22, 0).unwrap();
    cpu.interrupt(&mut bus, 8).unwrap();
    assert!(!cpu.is_halted());
    assert_eq!(cpu.registers.ip, 0x100);
    assert_eq!(bus.read_u16(0xFFFA), Ok(7));
}

#[test]
fn two_operand_imul_truncates_and_reports_signed_overflow_at_both_widths() {
    for (dword, lhs, rhs, expected, overflow) in [
        (true, (-3i32) as u32, 7, (-21i32) as u32, false),
        (true, 0x7FFF_FFFF, 2, 0xFFFF_FFFE, true),
        (false, 0xCAFE_FFFD, 7, 0xCAFE_FFEB, false),
        (false, 0xCAFE_7FFF, 2, 0xCAFE_FFFE, true),
    ] {
        let code = if dword {
            &b"\x66\x0F\xAF\xC3"[..]
        } else {
            &b"\x0F\xAF\xC3"[..]
        };
        let mut bus = TestBus::new(code);
        let mut cpu = Cpu::default();
        cpu.registers.set32(Gpr32::Eax, lhs);
        cpu.registers.set32(Gpr32::Ebx, rhs);
        cpu.flags |= flags::ZERO | flags::DIRECTION;
        cpu.step(&mut bus).unwrap();
        assert_eq!(cpu.registers.get32(Gpr32::Eax), expected);
        assert_eq!(
            cpu.flags & (flags::CARRY | flags::OVERFLOW),
            if overflow {
                flags::CARRY | flags::OVERFLOW
            } else {
                0
            }
        );
        assert_eq!(
            cpu.flags & (flags::ZERO | flags::DIRECTION),
            flags::ZERO | flags::DIRECTION
        );
    }
}

#[test]
fn arithmetic_preserves_carry_for_inc_and_sets_signed_overflow() {
    let mut bus = TestBus::new(&[
        0xB8, 0xFF, 0xFF, 0x05, 1, 0, 0x40, 0xB8, 0xFF, 0x7F, 0x05, 1, 0,
    ]);
    let mut cpu = Cpu::default();
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get(Gpr16::Ax), 0);
    assert_ne!(cpu.flags & (flags::CARRY | flags::ZERO), 0);
    cpu.step(&mut bus).unwrap();
    assert_ne!(cpu.flags & flags::CARRY, 0);
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get(Gpr16::Ax), 0x8000);
    assert_ne!(cpu.flags & flags::OVERFLOW, 0);
    assert_eq!(cpu.flags & flags::CARRY, 0);
}

#[test]
fn interrupt_uses_guest_vector_and_iret_restores_state() {
    let mut bus = TestBus::new(&[0xCD, 0x60, 0xF4]);
    bus.ram[0x180..0x184].copy_from_slice(&[0, 2, 0, 0]);
    bus.ram[0x200..0x204].copy_from_slice(&[0xB8, 0x34, 0x12, 0xCF]);
    let mut cpu = Cpu::default();
    cpu.registers.set(Gpr16::Sp, 0x1000);
    let original_flags = cpu.flags;
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.ip, 0x200);
    assert_eq!(cpu.registers.get(Gpr16::Sp), 0xFFA);
    assert_eq!(cpu.flags & flags::INTERRUPT, 0);
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.ip, 2);
    assert_eq!(cpu.flags, original_flags);
    assert_eq!(cpu.registers.get(Gpr16::Sp), 0x1000);
    assert_eq!(cpu.registers.get(Gpr16::Ax), 0x1234);
    assert_eq!(cpu.step(&mut bus).unwrap(), StepOutcome::Halted);
}

#[test]
fn rep_is_bounded_and_resumes_without_a_buffer_copy() {
    let mut bus = TestBus::new(&[0xF3, 0xAA, 0xF4]);
    let mut cpu = Cpu::default();
    cpu.registers.es = 0x100;
    cpu.registers.set(Gpr16::Cx, 3);
    cpu.registers.set_byte(0, 0xAD);
    cpu.step(&mut bus).unwrap();
    assert_eq!(bus.writes, 1);
    assert_eq!(cpu.registers.ip, 0);
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(bus.writes, 3);
    assert_eq!(cpu.registers.ip, 2);
    assert_eq!(&bus.ram[0x1000..0x1003], &[0xAD; 3]);
}

#[test]
fn instruction_fetch_wraps_the_twenty_bit_address_bus() {
    let mut bus = TestBus::new(&[0x34, 0x12]);
    bus.ram[0xFFFFF] = 0xB8;
    let mut cpu = Cpu::default();
    cpu.registers.cs = 0xFFFF;
    cpu.registers.ip = 0xF;
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get(Gpr16::Ax), 0x1234);
}

#[test]
fn modrm_addresses_select_ds_or_ss_and_wrap_offsets_before_translation() {
    for mode in 0..3 {
        for rm in 0..8 {
            let mut code = vec![0x8B, (mode << 6) | rm];
            let displacement = match mode {
                0 if rm == 6 => {
                    code.extend([0x34, 0x12]);
                    0x1234
                }
                1 => {
                    code.push(0xFC);
                    0xFFFC
                }
                2 => {
                    code.extend([0xFC, 0xFF]);
                    0xFFFC
                }
                _ => 0,
            };
            let mut bus = TestBus::new(&code);
            let mut cpu = Cpu::default();
            cpu.registers.ds = 0x200;
            cpu.registers.ss = 0x300;
            cpu.registers.set(Gpr16::Bx, 0xFFF0);
            cpu.registers.set(Gpr16::Bp, 0xFFF8);
            cpu.registers.set(Gpr16::Si, 0x20);
            cpu.registers.set(Gpr16::Di, 0x30);
            let base: u16 = match rm {
                0 => 0x10,
                1 => 0x20,
                2 => 0x18,
                3 => 0x28,
                4 => 0x20,
                5 => 0x30,
                6 if mode == 0 => 0,
                6 => 0xFFF8,
                _ => 0xFFF0,
            };
            let segment = if matches!(rm, 2 | 3) || (rm == 6 && mode != 0) {
                0x300
            } else {
                0x200
            };
            let address =
                doslet_x86::real_address(segment, base.wrapping_add(displacement)) as usize;
            bus.ram[address..address + 2].copy_from_slice(&[0xAD, 0xDE]);
            cpu.step(&mut bus).unwrap();
            assert_eq!(cpu.registers.get(Gpr16::Ax), 0xDEAD, "mode={mode}, rm={rm}");
            assert_eq!(cpu.registers.ip as usize, code.len());
        }
    }
}

#[test]
fn segment_override_and_lea_keep_effective_offsets_distinct() {
    let mut bus = TestBus::new(&[0x3E, 0x8B, 0x46, 0xFE, 0x8D, 0x4E, 0xFE]);
    let mut cpu = Cpu::default();
    cpu.registers.ds = 0x200;
    cpu.registers.ss = 0x300;
    cpu.registers.set(Gpr16::Bp, 0x100);
    bus.write_u16(0x20FE, 0x1234).unwrap();
    bus.write_u16(0x30FE, 0x5678).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get(Gpr16::Ax), 0x1234);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get(Gpr16::Cx), 0xFE);
}

#[test]
fn byte_arithmetic_signed_conditions_and_carry_input_are_architectural() {
    let mut bus = TestBus::new(&[
        0xB0, 0x7F, 0x04, 1, 0x7C, 2, 0xB3, 9, 0xB0, 0xFF, 0xF9, 0x14, 0, 0x1C, 0xFF,
    ]);
    let mut cpu = Cpu::default();
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get_byte(0), 0x80);
    assert_eq!(
        cpu.flags & (flags::SIGN | flags::OVERFLOW | flags::AUXILIARY),
        flags::SIGN | flags::OVERFLOW | flags::AUXILIARY
    );
    cpu.step(&mut bus).unwrap(); // JL must not branch: SF and OF are both set.
    assert_eq!(cpu.registers.ip, 6);
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get_byte(0), 0);
    assert_ne!(cpu.flags & flags::CARRY, 0);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get_byte(0), 0);
    assert_ne!(cpu.flags & flags::CARRY, 0);
}

#[test]
fn rep_movsw_and_repne_scasb_yield_and_stop_on_the_right_condition() {
    let mut bus = TestBus::new(&[0xF3, 0xA5, 0xF2, 0xAE]);
    let mut cpu = Cpu::default();
    cpu.registers.ds = 0x200;
    cpu.registers.es = 0x300;
    cpu.registers.set(Gpr16::Si, 4);
    cpu.registers.set(Gpr16::Di, 8);
    cpu.registers.set(Gpr16::Cx, 2);
    cpu.flags |= flags::DIRECTION;
    bus.ram[0x2002..0x2006].copy_from_slice(&[1, 2, 3, 4]);
    cpu.step(&mut bus).unwrap();
    assert_eq!(bus.writes, 2);
    assert_eq!(cpu.registers.ip, 0);
    cpu.step(&mut bus).unwrap();
    assert_eq!(bus.writes, 4);
    assert_eq!(&bus.ram[0x3006..0x300A], &[1, 2, 3, 4]);
    cpu.flags &= !flags::DIRECTION;
    cpu.registers.set_byte(0, 2);
    cpu.registers.set(Gpr16::Di, 6);
    cpu.registers.set(Gpr16::Cx, 5);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.ip, 2);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.ip, 4);
    assert_eq!(cpu.registers.get(Gpr16::Cx), 3);
    assert_ne!(cpu.flags & flags::ZERO, 0);
}

#[test]
fn shifts_preserve_zero_count_and_produce_carry_and_sign() {
    let mut bus = TestBus::new(&[0xC0, 0xE0, 0, 0xD0, 0xD0, 0xC0, 0xF8, 4]);
    let mut cpu = Cpu::default();
    cpu.registers.set_byte(0, 0x80);
    let original_flags = cpu.flags;
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.flags, original_flags);
    assert_eq!(cpu.registers.get_byte(0), 0x80);
    cpu.step(&mut bus).unwrap(); // RCL AL,1.
    assert_eq!(cpu.registers.get_byte(0), 0);
    assert_ne!(cpu.flags & flags::CARRY, 0);
    assert_ne!(cpu.flags & flags::OVERFLOW, 0);
    cpu.registers.set_byte(0, 0xF0);
    cpu.step(&mut bus).unwrap(); // SAR AL,4.
    assert_eq!(cpu.registers.get_byte(0), 0xFF);
    assert_ne!(cpu.flags & flags::SIGN, 0);
}

#[test]
fn signed_division_and_divide_overflow_do_not_modify_destination_on_error() {
    let mut bus = TestBus::new(&[0x99, 0xF7, 0xFB, 0xF7, 0xF3]);
    let mut cpu = Cpu::default();
    cpu.registers.set(Gpr16::Ax, (-7i16) as u16);
    cpu.registers.set(Gpr16::Bx, 3);
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get(Gpr16::Ax), (-2i16) as u16);
    assert_eq!(cpu.registers.get(Gpr16::Dx), 0xFFFF);
    cpu.registers.set(Gpr16::Ax, 0);
    cpu.registers.set(Gpr16::Dx, 3);
    assert!(matches!(
        cpu.step(&mut bus),
        Err(doslet_x86::CpuError::DivideError { .. })
    ));
    assert_eq!(cpu.registers.get(Gpr16::Ax), 0);
    assert_eq!(cpu.registers.get(Gpr16::Dx), 3);
}

#[test]
fn indirect_far_calls_push_a_return_frame_and_retf_restores_it() {
    let mut bus = TestBus::new(&[0xFF, 0x1E, 0, 2, 0xF4]);
    bus.write_u16(0x200, 0x10).unwrap();
    bus.write_u16(0x202, 0x100).unwrap();
    bus.ram[0x1010..0x1014].copy_from_slice(&[0xB8, 0xAD, 0xDE, 0xCB]);
    let mut cpu = Cpu::default();
    cpu.registers.set(Gpr16::Sp, 0x800);
    cpu.step(&mut bus).unwrap();
    assert_eq!((cpu.registers.cs, cpu.registers.ip), (0x100, 0x10));
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!((cpu.registers.cs, cpu.registers.ip), (0, 4));
    assert_eq!(cpu.registers.get(Gpr16::Sp), 0x800);
    assert_eq!(cpu.registers.get(Gpr16::Ax), 0xDEAD);
}

#[test]
fn register_aliases_preserve_high_bits_and_operand_override_changes_width() {
    let mut bus = TestBus::new(&[
        0x66, 0xB8, 0x44, 0x33, 0x22, 0x11, 0xB0, 0xAA, 0xB4, 0xBB, 0xB8, 0x34, 0x12,
    ]);
    let mut cpu = Cpu::default();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Eax), 0x11223344);
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Eax), 0x1122BBAA);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Eax), 0x11221234);
}

#[test]
fn dword_alu_sets_flags_from_all_32_bits_and_propagates_carry() {
    let mut bus = TestBus::new(&[
        0x66, 0xB8, 0xFF, 0xFF, 0xFF, 0x7F, 0x66, 0x05, 1, 0, 0, 0, 0x66, 0xB8, 0xFF, 0xFF, 0xFF,
        0xFF, 0xF9, 0x66, 0x15, 0, 0, 0, 0,
    ]);
    let mut cpu = Cpu::default();
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Eax), 0x80000000);
    assert_eq!(
        cpu.flags & (flags::SIGN | flags::OVERFLOW),
        flags::SIGN | flags::OVERFLOW
    );
    assert_eq!(cpu.flags & flags::ZERO, 0);
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Eax), 0);
    assert_eq!(
        cpu.flags & (flags::CARRY | flags::ZERO),
        flags::CARRY | flags::ZERO
    );
}

#[test]
fn dword_push_pop_preserve_esp_high_half_in_a_word_stack_segment() {
    let mut bus = TestBus::new(&[0x66, 0x50, 0x66, 0x5B]);
    let mut cpu = Cpu::default();
    cpu.registers.set32(Gpr32::Eax, 0xDEADBEEF);
    cpu.registers.set32(Gpr32::Esp, 0x12340800);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Esp), 0x123407FC);
    assert_eq!(&bus.ram[0x7FC..0x800], &0xDEADBEEFu32.to_le_bytes());
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get32(Gpr32::Ebx), 0xDEADBEEF);
    assert_eq!(cpu.registers.get32(Gpr32::Esp), 0x12340800);
}

#[test]
fn pushad_popad_skip_the_saved_stack_pointer_slot() {
    let mut bus = TestBus::new(&[0x66, 0x60, 0x66, 0x61]);
    let mut cpu = Cpu::default();
    let registers = [
        Gpr32::Eax,
        Gpr32::Ecx,
        Gpr32::Edx,
        Gpr32::Ebx,
        Gpr32::Esp,
        Gpr32::Ebp,
        Gpr32::Esi,
        Gpr32::Edi,
    ];
    for (index, register) in registers.iter().enumerate() {
        cpu.registers.set32(*register, 0xDEAD0000 + index as u32);
    }
    cpu.registers.set32(Gpr32::Esp, 0xABCD0800);
    let expected = registers.map(|r| cpu.registers.get32(r));
    cpu.step(&mut bus).unwrap();
    assert_eq!(&bus.ram[0x7EC..0x7F0], &0xABCD0800u32.to_le_bytes());
    for register in registers {
        if register != Gpr32::Esp {
            cpu.registers.set32(register, 0);
        }
    }
    cpu.step(&mut bus).unwrap();
    assert_eq!(registers.map(|r| cpu.registers.get32(r)), expected);
}

#[test]
fn popfd_cpu_detection_cannot_enable_486_or_cpuid_flags_on_a_386() {
    let mut bus = TestBus::new(&[
        0x66, 0x9C, 0x66, 0x58, 0x66, 0x0D, 0, 0, 0x24, 0, 0x66, 0x50, 0x66, 0x9D, 0x66, 0x9C,
        0x66, 0x59,
    ]);
    let mut cpu = Cpu::default();
    cpu.registers.set(Gpr16::Sp, 0x800);
    for _ in 0..7 {
        cpu.step(&mut bus).unwrap();
    }
    assert_eq!(
        cpu.registers.get32(Gpr32::Ecx) & (flags::ALIGNMENT | flags::ID),
        0
    );
    assert_eq!(cpu.flags & 2, 2);
    assert_eq!(cpu.registers.get(Gpr16::Sp), 0x800);
}

#[test]
fn rep_movsd_yields_after_one_dword_and_keeps_word_address_counters() {
    let mut bus = TestBus::new(&[0x66, 0xF3, 0xA5]);
    let mut cpu = Cpu::default();
    cpu.registers.ds = 0x200;
    cpu.registers.es = 0x300;
    cpu.registers.set(Gpr16::Cx, 2);
    bus.ram[0x2000..0x2008].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    cpu.step(&mut bus).unwrap();
    assert_eq!(bus.writes, 4);
    assert_eq!(cpu.registers.ip, 0);
    assert_eq!(cpu.registers.get(Gpr16::Cx), 1);
    assert_eq!(cpu.registers.get(Gpr16::Si), 4);
    cpu.step(&mut bus).unwrap();
    assert_eq!(bus.writes, 8);
    assert_eq!(&bus.ram[0x3000..0x3008], &[1, 2, 3, 4, 5, 6, 7, 8]);
    assert_eq!(cpu.registers.ip, 3);
}

#[test]
fn fs_gs_segment_moves_and_overrides_address_their_own_data() {
    let mut bus = TestBus::new(&[0x8E, 0xE0, 0x64, 0xA1, 0, 0, 0x8E, 0xE8, 0x65, 0xA1, 0, 0]);
    let mut cpu = Cpu::default();
    cpu.registers.set(Gpr16::Ax, 0x200);
    bus.write_u16(0x2000, 0x300).unwrap();
    bus.write_u16(0x3000, 0xBEEF).unwrap();
    for _ in 0..4 {
        cpu.step(&mut bus).unwrap();
    }
    assert_eq!((cpu.registers.fs, cpu.registers.gs), (0x200, 0x300));
    assert_eq!(cpu.registers.get(Gpr16::Ax), 0xBEEF);
}

#[test]
fn descriptor_tables_keep_full_dword_base_and_word_lgdt_masks_to_24_bits() {
    let mut bus = TestBus::new(&[
        0x66, 0x0F, 0x01, 0x16, 0, 2, 0x0F, 0x01, 0x06, 8, 2, 0x0F, 0x01, 0x16, 0, 2,
    ]);
    bus.ram[0x200..0x206].copy_from_slice(&[0x17, 0, 0x78, 0x56, 0x34, 0x12]);
    let mut cpu = Cpu::default();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.control.gdtr.base, 0x12345678);
    cpu.step(&mut bus).unwrap();
    assert_eq!(&bus.ram[0x208..0x20E], &[0x17, 0, 0x78, 0x56, 0x34, 0x12]);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.control.gdtr.base, 0x345678);
}

#[test]
fn pe_transition_keeps_the_cached_real_mode_code_base_until_reload() {
    let mut bus = TestBus::new(&[
        0x0F, 0x20, 0xC0, 0x66, 0x0D, 1, 0, 0, 0, 0x0F, 0x22, 0xC0, 0x90,
    ]);
    let mut cpu = Cpu::default();
    assert_eq!(cpu.control.cr0 & 5, 4); // x87 unavailable, real mode.
    for _ in 0..3 {
        cpu.step(&mut bus).unwrap();
    }
    assert_eq!(cpu.control.cr0 & 1, 1);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.ip, 13);
    assert_eq!(
        cpu.registers
            .segment_cache(doslet_x86::SegmentRegister::Cs)
            .base,
        0
    );
}

#[test]
fn enter_nested_frame_and_leave_restore_the_original_stack() {
    let mut bus = TestBus::new(&[0xC8, 8, 0, 2, 0xC9]);
    let mut cpu = Cpu::default();
    cpu.registers.set(Gpr16::Sp, 0x800);
    cpu.registers.set(Gpr16::Bp, 0x700);
    bus.write_u16(0x6FE, 0x3456).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get(Gpr16::Bp), 0x7FE);
    assert_eq!(cpu.registers.get(Gpr16::Sp), 0x7F2);
    assert_eq!(bus.read_u16(0x7FC).unwrap(), 0x3456);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get(Gpr16::Bp), 0x700);
    assert_eq!(cpu.registers.get(Gpr16::Sp), 0x800);
}

#[test]
fn x87_control_initialization_and_zero_storage_use_real_state() {
    let mut bus = TestBus::new(&[
        0xD9, 0x2E, 0, 2, // FLDCW [200h].
        0xD9, 0x3E, 2, 2, // FNSTCW [202h].
        0xDB, 0xE3, 0xDF, 0xE0, 0xD9, 0xEE, // FNINIT, FNSTSW AX, FLDZ.
        0xDD, 0x1E, 0x10, 2, // FSTP qword [210h].
    ]);
    bus.write_u16(0x200, 0x027F).unwrap();
    bus.ram[0x210..0x218].fill(0xAA);
    let mut cpu = Cpu::default().with_x87();
    cpu.step(&mut bus).unwrap();
    cpu.step(&mut bus).unwrap();
    assert_eq!(bus.read_u16(0x202).unwrap(), 0x027F);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.fpu.as_ref().unwrap().control(), 0x037F);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.get(Gpr16::Ax), 0);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.fpu.as_ref().unwrap().status() & 0x3800, 0x3800);
    cpu.step(&mut bus).unwrap();
    assert_eq!(&bus.ram[0x210..0x218], &[0; 8]);
    assert_eq!(cpu.fpu.as_ref().unwrap().tags(), 0xFFFF);
}

#[test]
fn fild_signed_integer_widths_and_address_override_preserve_cpu_state() {
    for (opcode, modrm, width, value) in [
        (0xDF, 0x06, 2, -32768_i64),
        (0xDB, 0x06, 4, i64::from(i32::MIN)),
        (0xDF, 0x2E, 8, i64::MIN),
        (0xDF, 0x2E, 8, 123456789),
    ] {
        let mut bus = TestBus::new(&[opcode, modrm, 0, 2]);
        bus.ram[0x200..0x200 + width].copy_from_slice(&value.to_le_bytes()[..width]);
        let mut cpu = Cpu::default().with_x87();
        let flags = cpu.flags;
        cpu.step(&mut bus).unwrap();
        assert_eq!(
            cpu.fpu.as_ref().unwrap().store_value().unwrap(),
            value as f64
        );
        assert_eq!(cpu.flags, flags);
        assert_eq!(cpu.fpu.as_ref().unwrap().status() & 0x3A00, 0x3800);
    }
    // Actual SETUP encoding DF 2C 24: FILD qword [ESP] in 32-bit addressing.
    let mut bus = TestBus::new(&[0x67, 0xDF, 0x2C, 0x24]);
    bus.ram[0x200..0x208].copy_from_slice(&(-42_i64).to_le_bytes());
    let mut cpu = Cpu::default().with_x87();
    cpu.registers.set32(doslet_x86::Gpr32::Esp, 0x200);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.fpu.as_ref().unwrap().store_value().unwrap(), -42.0);
    assert_eq!(cpu.registers.get32(doslet_x86::Gpr32::Esp), 0x200);
}

#[test]
fn x87_integer_calibration_memory_arithmetic_and_rounded_store() {
    let mut bus = TestBus::new(&[
        0xDF, 0x2E, 0, 2, // FILD m64int [200h] = 3.
        0xD8, 0x3E, 8, 2, // FDIVR m32real [208h] = 1 / 3.
        0xD8, 0x0E, 12, 2, // FMUL m32real [20Ch] = * 2.
        0xD9, 0x1E, 16, 2, // FSTP m32real [210h] (inexact).
        0xD9, 0x06, 16, 2, // FLD m32real [210h].
        0xDC, 0x06, 24, 2, // FADD m64real [218h] = + 1.
    ]);
    bus.ram[0x200..0x208].copy_from_slice(&3_i64.to_le_bytes());
    bus.ram[0x208..0x20C].copy_from_slice(&1_f32.to_le_bytes());
    bus.ram[0x20C..0x210].copy_from_slice(&2_f32.to_le_bytes());
    bus.ram[0x218..0x220].copy_from_slice(&1_f64.to_le_bytes());
    let mut cpu = Cpu::default().with_x87();
    for _ in 0..4 {
        cpu.step(&mut bus).unwrap();
    }
    let stored = f32::from_bits(bus.read_u32(0x210).unwrap());
    assert_eq!(stored.to_bits(), 0x3F2AAAAB);
    assert_eq!(cpu.fpu.as_ref().unwrap().tags(), 0xFFFF);
    assert_ne!(cpu.fpu.as_ref().unwrap().status() & 0x20, 0);
    for _ in 0..2 {
        cpu.step(&mut bus).unwrap();
    }
    assert_eq!(
        cpu.fpu.as_ref().unwrap().store_value(),
        Ok(f64::from(stored) + 1.0)
    );
}

#[test]
fn an_absent_or_task_switched_x87_fails_instead_of_silently_initializing() {
    let mut bus = TestBus::new(&[0xDB, 0xE3]);
    let mut cpu = Cpu::default();
    assert!(matches!(
        cpu.step(&mut bus),
        Err(doslet_x86::CpuError::X87Unavailable { .. })
    ));
    let mut cpu = Cpu::default().with_x87();
    cpu.control.cr0 |= 8;
    assert!(matches!(
        cpu.step(&mut bus),
        Err(doslet_x86::CpuError::X87Unavailable { .. })
    ));
}

#[test]
fn x87_exchange_decodes_each_stack_register_without_changing_top() {
    for index in 0..8 {
        let mut bus = TestBus::new(&[0xD9, 0xC8 + index]);
        let mut cpu = Cpu::default().with_x87();
        let fpu = cpu.fpu.as_mut().unwrap();
        for value in 0..8 {
            fpu.push(f64::from(value)).unwrap();
        }
        let status = fpu.status();
        cpu.step(&mut bus).unwrap();
        let fpu = cpu.fpu.as_ref().unwrap();
        assert_eq!(fpu.st0(), Ok(f64::from(7 - index)));
        assert_eq!(fpu.st(index), Ok(7.0));
        assert_eq!(fpu.status(), status);
    }
}

#[test]
fn x87_infinity_detection_uses_division_comparison_and_status_flags() {
    let mut bus = TestBus::new(&[
        0xDB, 0xE3, // FNINIT.
        0xD9, 0xE8, 0xD9, 0xEE, // FLD1, FLDZ.
        0xDE, 0xF9, // FDIVP ST(1),ST(0): +infinity, ZE set.
        0xD9, 0xC0, 0xD9, 0xE0, // Duplicate, negate.
        0xDE, 0xD9, 0xDF, 0xE0, // FCOMPP, FNSTSW AX.
        0x9E, // SAHF: C0 becomes CF, C3 becomes ZF.
    ]);
    let mut cpu = Cpu::default().with_x87();
    for _ in 0..9 {
        cpu.step(&mut bus).unwrap();
    }
    assert_eq!(cpu.registers.get(Gpr16::Ax), 0x104);
    assert_eq!(
        cpu.flags & (flags::CARRY | flags::ZERO | flags::PARITY),
        flags::CARRY
    );
    assert_eq!(cpu.fpu.as_ref().unwrap().tags(), 0xFFFF);
}

#[test]
fn dword_multiply_divide_and_overflow_use_the_full_accumulator_pair() {
    for (opcode, eax, edx, ebx, expected) in [
        (0xE3, 0x8000_0000, 0, 2, Some((0, 1))), // MUL.
        (
            0xEB,
            (-7i32) as u32,
            0,
            3,
            Some(((-21i32) as u32, u32::MAX)),
        ), // IMUL.
        (0xF3, 7, 1, 3, Some((0x5555_5557, 2))), // DIV of 2^32+7.
        (
            0xFB,
            (-22i32) as u32,
            u32::MAX,
            3,
            Some(((-7i32) as u32, u32::MAX)),
        ),
        (0xF3, 0, 1, 1, None), // Unsigned quotient overflow.
        (0xFB, 0x8000_0000, u32::MAX, u32::MAX, None), // Signed quotient overflow.
        (0xFB, 0, 0x8000_0000, u32::MAX, None), // i64::MIN / -1.
        (0xF3, 1, 0, 0, None),
    ] {
        let mut bus = TestBus::new(&[0x66, 0xF7, opcode]);
        let mut cpu = Cpu::default();
        cpu.registers.set32(Gpr32::Eax, eax);
        cpu.registers.set32(Gpr32::Edx, edx);
        cpu.registers.set32(Gpr32::Ebx, ebx);
        let result = cpu.step(&mut bus);
        if let Some((quotient, remainder)) = expected {
            result.unwrap();
            assert_eq!(
                (
                    cpu.registers.get32(Gpr32::Eax),
                    cpu.registers.get32(Gpr32::Edx)
                ),
                (quotient, remainder)
            );
        } else {
            assert!(matches!(
                result,
                Err(doslet_x86::CpuError::DivideError { .. })
            ));
            assert_eq!(
                (
                    cpu.registers.get32(Gpr32::Eax),
                    cpu.registers.get32(Gpr32::Edx)
                ),
                (eax, edx)
            );
        }
    }
}
