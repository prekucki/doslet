use super::*;

// Original linked-list scan: three RAM loads, an internal early exit, and a
// backward edge. The output store is deliberately outside the native trace.
const RAM_SCAN: &[u8] = &[
    0x8B, 0x06, // MOV EAX,[ESI]
    0x39, 0xD0, // CMP EAX,EDX
    0x74, 0x0C, // JE output
    0x8B, 0x5E, 4, // MOV EBX,[ESI+4]
    0x01, 0xDF, // ADD EDI,EBX
    0x8B, 0x76, 8, // MOV ESI,[ESI+8]
    0x85, 0xF6, // TEST ESI,ESI
    0x75, 0xEE, // JNZ start
    0x66, 0x89, 0x3D, 0, 0, 4, 0,    // Word override keeps the output outside the dword trace
    0xFA, // CLI: stop normal batching after the output store.
];

fn ram_scan_registers(m: &PcMachine) -> ([u32; 8], u32, u32) {
    let registers = [
        Gpr32::Eax,
        Gpr32::Ecx,
        Gpr32::Edx,
        Gpr32::Ebx,
        Gpr32::Esp,
        Gpr32::Ebp,
        Gpr32::Esi,
        Gpr32::Edi,
    ]
    .map(|register| m.cpu.registers.get32(register));
    (registers, m.cpu.registers.ip, m.cpu.flags)
}

fn ram_scan_machine(cached: bool) -> PcMachine {
    let mut m = machine(cached);
    clear_shadow(&mut m);
    descriptor(&mut m.bus, 16, 0, 0xFFFFF, false, true);
    // Granularity turns the 20-bit descriptor limit into a full 4-GiB span.
    m.bus.write_u8(0x816, 0xCF).unwrap();
    m.cpu
        .load_segment(&mut m.bus, SegmentRegister::Ds, 16)
        .unwrap();
    for index in 0..32_u32 {
        let address = 0x120000 + index * 12;
        m.bus.write_u32(address, index).unwrap();
        m.bus
            .write_u32(address + 4, index.wrapping_mul(0x1020304))
            .unwrap();
        m.bus
            .write_u32(address + 8, if index == 31 { 0 } else { address + 12 })
            .unwrap();
    }
    code(&mut m, 0x10000, RAM_SCAN);
    m.cpu.registers.set32(Gpr32::Esi, 0x120000);
    m.cpu.registers.set32(Gpr32::Edx, u32::MAX);
    m
}

const RAM_STORE_LOOP: &[u8] = &[
    0x8B, 0x06, // MOV EAX,[ESI]
    0x83, 0xC0, 1, // ADD EAX,1
    0x89, 0x07, // MOV [EDI],EAX
    0x83, 0xC6, 4, // ADD ESI,4
    0x83, 0xC7, 4,    // ADD EDI,4
    0x49, // DEC ECX
    0x75, 0xF0, // JNZ start
    0xFA, // CLI is outside the trace.
];

fn ram_store_machine(cached: bool) -> PcMachine {
    let mut m = ram_scan_machine(cached);
    // Put code inside the native writable RAM span, so self-modifying stores
    // exercise the active-code guard rather than a generic low-RAM rejection.
    m.bus.write_u8(0x80E, 0xCF).unwrap();
    m.cpu
        .load_segment(&mut m.bus, SegmentRegister::Cs, 8)
        .unwrap();
    code(&mut m, 0x160000, RAM_STORE_LOOP);
    m.cpu.registers.set32(Gpr32::Edi, 0x140FFD);
    m.cpu.registers.set32(Gpr32::Ecx, 32);
    m
}

fn call_trace_machine(cached: bool, bytes: &[u8]) -> PcMachine {
    let mut m = ram_store_machine(cached);
    m.cpu
        .load_segment(&mut m.bus, SegmentRegister::Ss, 16)
        .unwrap();
    clear_shadow(&mut m);
    code(&mut m, 0x160000, bytes);
    m.cpu.registers.set32(Gpr32::Esp, 0x150100);
    m
}

#[test]
fn native_ram_trace_uses_configured_capacity_at_last_dword_and_bounds() {
    for memory_mib in [4, 8, 16, 32] {
        let end = u32::from(memory_mib) * 1024 * 1024;
        for crosses_end in [false, true] {
            let mut native = ram_store_machine(true);
            let mut reference = ram_store_machine(false);
            for m in [&mut native, &mut reference] {
                m.bus = PcBus::from_config(doslet_config::HardwareConfig {
                    memory_mib,
                    ..doslet_config::HardwareConfig::default()
                })
                .unwrap();
                m.bus.set_a20(true);
                code(m, 0x160000, RAM_STORE_LOOP);
                m.bus.write_u32(end - 8, 0x87654321).unwrap();
                m.cpu.registers.set32(Gpr32::Esi, end - 8);
                m.cpu
                    .registers
                    .set32(Gpr32::Edi, end - if crosses_end { 2 } else { 4 });
                m.cpu.registers.set32(Gpr32::Ecx, 1);
            }
            warm(&mut native);
            let expected = if crosses_end { 2 } else { 7 };
            assert_eq!(
                native.cpu.dynamic_steps_mut(&mut native.bus, 255),
                expected,
                "memory_mib={memory_mib} crosses_end={crosses_end}"
            );
            for _ in 0..expected {
                reference.cpu.step(&mut reference.bus).unwrap();
            }
            state(&native, &reference);
            assert_eq!(
                native.bus.ram.view(0..end as usize).unwrap(),
                reference.bus.ram.view(0..end as usize).unwrap()
            );
            if !crosses_end {
                assert_eq!(native.bus.read_u32(end - 4).unwrap(), 0x87654322);
            }
            assert_eq!(native.cpu.dynamic_stats().ram_trace_entries, 1);
            if crosses_end {
                continue;
            }

            let load = [0x8B, 0x06, 0x40, 0x66, 0x90];
            for m in [&mut native, &mut reference] {
                code(m, 0x160000, &load);
                m.cpu.registers.set32(Gpr32::Esi, end - 4);
            }
            warm(&mut native);
            assert_eq!(native.cpu.dynamic_steps(&native.bus, 255), 2);
            reference.cpu.step(&mut reference.bus).unwrap();
            reference.cpu.step(&mut reference.bus).unwrap();
            state(&native, &reference);
            assert_eq!(native.cpu.registers.get32(Gpr32::Eax), 0x87654323);
            assert_eq!(
                native.bus.ram.view(0..end as usize).unwrap(),
                reference.bus.ram.view(0..end as usize).unwrap()
            );
        }
    }
}

#[test]
fn native_slices_stop_before_configured_at_typematic_irq_deadline() {
    let mut native = machine(true);
    let mut reference = machine(false);
    for m in [&mut native, &mut reference] {
        clear_shadow(m);
        m.bus = PcBus::from_config(doslet_config::HardwareConfig {
            keyboard: doslet_config::KeyboardMode::At,
            sb16: false,
            ..doslet_config::HardwareConfig::default()
        })
        .unwrap();
        m.bus.set_a20(true);
        descriptor(&mut m.bus, 8, 0, 0xFFFFF, true, true);
        descriptor(&mut m.bus, 16, 0, 0xFFFFF, false, true);
        code(m, 0x10000, &[0x40, 0x41, 0xEB, 0xFC]);
        m.cpu.registers.set32(Gpr32::Esp, 0x50000);
        m.cpu.control.idtr.base = 0x900;
        m.cpu.control.idtr.limit = 0xFF;
        for (index, byte) in [0, 0, 8, 0, 0, 0x8E, 3, 0].into_iter().enumerate() {
            m.bus.write_u8(0x948 + index as u32, byte).unwrap();
        }
        // IN AL,60h; INC EDI; MOV AL,20h; OUT 20h,AL; IRETD.
        for (index, byte) in [0xE4, 0x60, 0x47, 0xB0, 0x20, 0xE6, 0x20, 0xCF]
            .into_iter()
            .enumerate()
        {
            m.bus.write_u8(0x30000 + index as u32, byte).unwrap();
        }
        m.bus.pic.write_data(0xFD); // Only the configured AT IRQ1 is unmasked.
        m.key_event(doslet_input::PhysicalKey(4), true).unwrap();
        assert_eq!(m.bus.pic.acknowledge(), Some(9));
        assert_eq!(m.bus.read_pending_keyboard(), Some(0x1E));
        m.bus.pic.write_command(0x20);
        assert_eq!(m.bus.keyboard.next_deadline(), Some(500_000_000));
        m.advance_hardware_steps(4_124_980).unwrap();
    }
    warm(&mut native);
    native.cpu.flags |= flags::INTERRUPT;
    reference.cpu.flags |= flags::INTERRUPT;
    let deadline = native.bus.keyboard.next_deadline().unwrap();
    let (_, used) = native.step_with_idle_budget(256).unwrap();
    assert!(used > 1 && used < 256);
    assert!(native.time().as_nanos() < deadline);
    for _ in 0..used {
        reference.step().unwrap();
    }
    state(&native, &reference);
    for _ in 0..64 {
        let (_, used) = native.step_with_idle_budget(256).unwrap();
        for _ in 0..used {
            reference.step().unwrap();
        }
        state(&native, &reference);
        assert_eq!(
            native.bus.keyboard.status(),
            reference.bus.keyboard.status()
        );
        assert_eq!(
            native.bus.keyboard.next_deadline(),
            reference.bus.keyboard.next_deadline()
        );
    }
    assert_eq!(native.cpu.registers.get32(Gpr32::Edi), 1);
    assert!(native.cpu.dynamic_stats().executed_steps > 1000);
    memory(&native, &reference);
}

#[test]
fn native_call_loop_keeps_byte_aliases_lea_stack_and_returns_equivalent() {
    let mut bytes = alloc::vec![
        0x8A, 0x06, // MOV AL,[ESI]
        0x88, 0xC4, // MOV AH,AL
        0x38, 0x26, // CMP BYTE [ESI],AH
        0x8D, 0x5E, 4,    // LEA EBX,[ESI+4]
        0x50, // PUSH EAX
        0x87, 0x44, 0x24, 4, // XCHG [ESP+4],EAX
        0xE8, 0x0D, 0, 0, 0,    // CALL callee at offset 32
        0x5A, // POP EDX
        0x47, // INC EDI
        0xEB, 0xE9, // JMP start
    ];
    bytes.resize(32, 0x90);
    bytes.extend_from_slice(&[
        0x84, 0x26, // TEST BYTE [ESI],AH
        0x88, 0x66, 1, // MOV [ESI+1],AH
        0x40, 0xC3, // INC EAX; RET
    ]);
    for budget in [1, 2, 3, 7, 31, 255] {
        let mut native = call_trace_machine(true, &bytes);
        let mut reference = call_trace_machine(false, &bytes);
        for m in [&mut native, &mut reference] {
            m.bus.write_u32(0x120000, 0xA5B6_C780).unwrap();
        }
        warm(&mut native);
        for _ in 0..32 {
            let (_, used) = native.step_with_idle_budget(budget).unwrap();
            assert!(used > 0 && used <= budget);
            for _ in 0..used {
                reference.step().unwrap();
            }
            state(&native, &reference);
            assert_eq!(
                native.bus.ram.view(0x1500E0..0x150110),
                reference.bus.ram.view(0x1500E0..0x150110)
            );
            assert_eq!(
                native.bus.read_u32(0x120000),
                reference.bus.read_u32(0x120000)
            );
        }
        if budget == 255 {
            let stats = native.cpu.dynamic_stats();
            assert!(stats.ram_trace_steps > 1000);
            assert!(stats.ram_trace_steps > stats.ram_trace_entries * 16);
        }
        memory(&native, &reference);
    }
}

#[test]
fn native_memory_exchange_uses_the_old_esp_address_and_preserves_flags() {
    let bytes = [0x8B, 0x06, 0x87, 0x24, 0x24, 0x66, 0x90]; // MOV; XCHG [ESP],ESP; boundary.
    let mut native = call_trace_machine(true, &bytes);
    let mut reference = call_trace_machine(false, &bytes);
    for m in [&mut native, &mut reference] {
        m.bus.write_u32(0x150100, 0x140FFD).unwrap();
        m.cpu.flags = 2 | flags::DIRECTION | 0x8D5;
    }
    warm(&mut native);
    assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 255), 2);
    reference.cpu.step(&mut reference.bus).unwrap();
    reference.cpu.step(&mut reference.bus).unwrap();
    state(&native, &reference);
    memory(&native, &reference);
    assert_eq!(native.cpu.registers.get32(Gpr32::Esp), 0x140FFD);
    assert_eq!(native.bus.read_u32(0x150100).unwrap(), 0x150100);
    assert_eq!(native.cpu.flags, 2 | flags::DIRECTION | 0x8D5);
}

#[test]
fn native_memory_exchange_guards_active_code_before_changing_either_operand() {
    let bytes = [0x8B, 0x06, 0x87, 0x07, 0x66, 0x90];
    let mut native = call_trace_machine(true, &bytes);
    let mut reference = call_trace_machine(false, &bytes);
    for m in [&mut native, &mut reference] {
        m.cpu.registers.set32(Gpr32::Edi, 0x160800);
        m.bus.write_u32(0x160800, 0x12345678).unwrap();
    }
    warm(&mut native);
    assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 255), 1);
    reference.cpu.step(&mut reference.bus).unwrap();
    state(&native, &reference);
    memory(&native, &reference);
    assert_eq!(native.bus.read_u32(0x160800).unwrap(), 0x12345678);
    assert_eq!(native.cpu.registers.ip, 0x160002);
    native.cpu.step(&mut native.bus).unwrap();
    reference.cpu.step(&mut reference.bus).unwrap();
    state(&native, &reference);
    memory(&native, &reference);
    assert_eq!(native.cpu.registers.get32(Gpr32::Eax), 0x12345678);
}

#[test]
fn native_memory_exchange_invalidates_other_prepared_code() {
    let bytes = [0x8B, 0x0E, 0x87, 0x07, 0x66, 0x90]; // MOV ECX,[ESI]; XCHG [EDI],EAX.
    let mut native = call_trace_machine(true, &bytes);
    let mut reference = call_trace_machine(false, &bytes);
    for m in [&mut native, &mut reference] {
        code(m, 0x180000, &[0x40, 0x41, 0x66, 0x90]);
    }
    warm(&mut native);
    for m in [&mut native, &mut reference] {
        code(m, 0x160000, &bytes);
        m.cpu.registers.set32(Gpr32::Eax, 0x90664148);
        m.cpu.registers.set32(Gpr32::Edi, 0x180000);
    }
    warm(&mut native);
    assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 255), 2);
    reference.cpu.step(&mut reference.bus).unwrap();
    reference.cpu.step(&mut reference.bus).unwrap();
    state(&native, &reference);
    memory(&native, &reference);
    assert_eq!(native.cpu.registers.get32(Gpr32::Eax), 0x90664140);
    native.cpu.registers.ip = 0x180000;
    reference.cpu.registers.ip = 0x180000;
    let before = native.cpu.dynamic_stats();
    assert_eq!(native.cpu.dynamic_steps(&native.bus, 255), 0);
    assert!(native.cpu.dynamic_stats().invalidations > before.invalidations);
    native.cpu.step(&mut native.bus).unwrap();
    reference.cpu.step(&mut reference.bus).unwrap();
    state(&native, &reference);
}

#[test]
fn native_byte_memory_cmp_and_test_preserve_flags_and_high_register_aliases() {
    for opcode in [0x38, 0x84] {
        let bytes = [0x8A, 0x66, 1, opcode, 0x26, 0x66, 0x90]; // MOV AH,[ESI+1]; CMP/TEST [ESI],AH; word NOP boundary.
        let mut native = call_trace_machine(true, &bytes);
        let mut reference = call_trace_machine(false, &bytes);
        warm(&mut native);
        for left in [0, 1, 0x7F, 0x80, 0xFF] {
            for right in [0, 1, 0x7F, 0x80, 0xFF] {
                for m in [&mut native, &mut reference] {
                    m.bus.write_u32(0x120000, left | (right << 8)).unwrap();
                    m.cpu.registers.ip = 0x160000;
                    m.cpu.registers.set32(Gpr32::Eax, 0xAABB_CCDD);
                    m.cpu.flags = 2 | flags::DIRECTION | flags::CARRY;
                }
                assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 2), 2);
                reference.cpu.step(&mut reference.bus).unwrap();
                reference.cpu.step(&mut reference.bus).unwrap();
                state(&native, &reference);
                assert_eq!(
                    native.cpu.registers.get32(Gpr32::Eax),
                    0xAABB_00DD | (right << 8)
                );
            }
        }
        memory(&native, &reference);
    }
}

#[test]
fn native_dword_memory_left_cmp_and_test_match_edge_flags() {
    for opcode in [0x39, 0x85] {
        let bytes = [0x8B, 0x4E, 4, opcode, 0x1E, 0x66, 0x90];
        let mut native = call_trace_machine(true, &bytes);
        let mut reference = call_trace_machine(false, &bytes);
        warm(&mut native);
        for left in [0, 1, 0x7FFF_FFFF, 0x8000_0000, u32::MAX] {
            for right in [0, 1, 0x7FFF_FFFF, 0x8000_0000, u32::MAX] {
                for m in [&mut native, &mut reference] {
                    m.bus.write_u32(0x120000, left).unwrap();
                    m.cpu.registers.ip = 0x160000;
                    m.cpu.registers.set32(Gpr32::Ebx, right);
                    m.cpu.flags = 2 | flags::DIRECTION | 0x8D5;
                }
                assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 2), 2);
                reference.cpu.step(&mut reference.bus).unwrap();
                reference.cpu.step(&mut reference.bus).unwrap();
                state(&native, &reference);
            }
        }
        memory(&native, &reference);
    }
}

#[test]
fn native_push_esp_pop_esp_and_return_cleanup_match_original_interpreter() {
    for bytes in [
        &[0x8B, 0x06, 0x54, 0x5C, 0x66, 0x90][..], // PUSH ESP captures the old value; POP ESP wins over increment.
        &[0x8B, 0x06, 0xC2, 8, 0][..], // RET 8 pops the target and discards two arguments.
    ] {
        let mut native = call_trace_machine(true, bytes);
        let mut reference = call_trace_machine(false, bytes);
        for m in [&mut native, &mut reference] {
            m.bus.write_u32(0x150100, 0x160080).unwrap();
        }
        warm(&mut native);
        let expected = if bytes[2] == 0x54 { 3 } else { 2 };
        assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 32), expected);
        for _ in 0..expected {
            reference.cpu.step(&mut reference.bus).unwrap();
        }
        state(&native, &reference);
        memory(&native, &reference);
        assert_eq!(
            native.cpu.registers.get32(Gpr32::Esp),
            if expected == 3 { 0x150100 } else { 0x15010C }
        );
    }
}

#[test]
fn native_near_calls_exit_to_external_targets_with_correct_return_frames() {
    for tail in [
        &[0xFF, 0xD3][..],          // CALL EBX
        &[0xFF, 0x16][..],          // CALL DWORD [ESI]
        &[0xE8, 0x79, 0, 0, 0][..], // CALL relative to offset 128
    ] {
        let mut bytes = alloc::vec![0x8B, 0x06];
        bytes.extend_from_slice(tail);
        bytes.extend_from_slice(&[0x66, 0x90]);
        let mut native = call_trace_machine(true, &bytes);
        let mut reference = call_trace_machine(false, &bytes);
        for m in [&mut native, &mut reference] {
            m.cpu.registers.set32(Gpr32::Ebx, 0x160080);
            m.bus.write_u32(0x120000, 0x160080).unwrap();
            m.bus.write_u8(0x160080, 0xFA).unwrap(); // Explicit unsupported boundary at callee entry.
        }
        warm(&mut native);
        assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 255), 2);
        for _ in 0..2 {
            reference.cpu.step(&mut reference.bus).unwrap();
        }
        state(&native, &reference);
        memory(&native, &reference);
        assert_eq!(native.cpu.registers.ip, 0x160080);
        assert_eq!(native.cpu.registers.get32(Gpr32::Esp), 0x1500FC);
        assert_eq!(
            native.bus.read_u32(0x1500FC).unwrap(),
            0x160002 + tail.len() as u32
        );
    }
}

#[test]
fn native_stack_operations_reject_code_aliases_and_changed_stack_segments() {
    let bytes = [0x8B, 0x06, 0x50, 0x66, 0x90];
    let mut native = call_trace_machine(true, &bytes);
    let mut reference = call_trace_machine(false, &bytes);
    warm(&mut native);
    for m in [&mut native, &mut reference] {
        m.cpu.registers.set32(Gpr32::Esp, 0x160804);
    }
    assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 255), 1);
    reference.cpu.step(&mut reference.bus).unwrap();
    state(&native, &reference);
    memory(&native, &reference);
    assert_eq!(native.cpu.registers.get32(Gpr32::Esp), 0x160804);

    // A usable small stack remains valid for the interpreter, but falls outside
    // the flat, full-width assumptions of the native stack implementation.
    descriptor(&mut native.bus, 24, 0, 0xFFFFF, false, false);
    native
        .cpu
        .load_segment(&mut native.bus, SegmentRegister::Ss, 24)
        .unwrap();
    clear_shadow(&mut native);
    native.cpu.registers.ip = 0x160000;
    let before = ram_scan_registers(&native);
    assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 255), 0);
    assert_eq!(ram_scan_registers(&native), before);
}

#[test]
fn native_calls_reject_a_limited_cs_without_guest_side_effects() {
    let bytes = [0x8B, 0x06, 0xFF, 0xD3, 0x66, 0x90];
    let mut native = call_trace_machine(true, &bytes);
    let mut reference = call_trace_machine(false, &bytes);
    for m in [&mut native, &mut reference] {
        descriptor(&mut m.bus, 8, 0, 0x160, true, true);
        m.bus.write_u8(0x80E, 0xC0).unwrap(); // Effective limit 160FFFh.
        m.cpu
            .load_segment(&mut m.bus, SegmentRegister::Cs, 8)
            .unwrap();
        m.cpu.registers.set32(Gpr32::Ebx, 0x161000);
    }
    warm(&mut native);
    assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 255), 0);
    state(&native, &reference);
    memory(&native, &reference);
    assert_eq!(native.cpu.registers.get32(Gpr32::Esp), 0x150100);
    assert_eq!(native.cpu.registers.ip, 0x160000);
}

#[test]
fn cached_indirect_call_faults_preserve_original_push_and_fetch_order() {
    for indirect in [false, true] {
        let bytes: &[u8] = if indirect {
            &[0xFF, 0xD0]
        } else {
            &[0xE8, 0xFB, 0x0F, 0, 0]
        };
        let mut cached = call_trace_machine(true, bytes);
        let mut reference = call_trace_machine(false, bytes);
        for m in [&mut cached, &mut reference] {
            descriptor(&mut m.bus, 8, 0, 0x160, true, true);
            m.bus.write_u8(0x80E, 0xC0).unwrap();
            m.cpu
                .load_segment(&mut m.bus, SegmentRegister::Cs, 8)
                .unwrap();
            m.cpu.registers.set32(Gpr32::Eax, 0x161000);
        }
        assert_eq!(
            cached.cpu.step(&mut cached.bus).unwrap_err(),
            reference.cpu.step(&mut reference.bus).unwrap_err()
        );
        state(&cached, &reference);
        memory(&cached, &reference);
        assert_eq!(
            cached.cpu.registers.get32(Gpr32::Esp),
            if indirect { 0x1500FC } else { 0x150100 }
        );
    }
    // CALL ESP captures its operand before pushing the return address.
    let mut cached = call_trace_machine(true, &[0xFF, 0xD4]);
    let mut reference = call_trace_machine(false, &[0xFF, 0xD4]);
    cached.cpu.step(&mut cached.bus).unwrap();
    reference.cpu.step(&mut reference.bus).unwrap();
    state(&cached, &reference);
    memory(&cached, &reference);
    assert_eq!(cached.cpu.registers.ip, 0x150100);
    assert_eq!(cached.cpu.registers.get32(Gpr32::Esp), 0x1500FC);
}

#[test]
fn native_low_ram_globals_stay_inside_the_trace_loop() {
    let bytes = [
        0x8B, 0x06, // MOV EAX,[ESI]
        0x39, 0x46, 8, // CMP [ESI+8],EAX
        0x85, 0x46, 8,    // TEST [ESI+8],EAX
        0x47, // INC EDI
        0xEB, 0xF5, // JMP start
    ];
    let mut native = call_trace_machine(true, &bytes);
    let mut reference = call_trace_machine(false, &bytes);
    for m in [&mut native, &mut reference] {
        m.cpu.registers.set32(Gpr32::Esi, 0x142F0);
        m.bus.write_u32(0x142F0, 0x8000_0001).unwrap();
        m.bus.write_u32(0x142F8, 0xFFFF_FFFF).unwrap();
    }
    warm(&mut native);
    assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 255), 255);
    for _ in 0..255 {
        reference.cpu.step(&mut reference.bus).unwrap();
    }
    state(&native, &reference);
    memory(&native, &reference);
    let stats = native.cpu.dynamic_stats();
    assert_eq!(stats.ram_trace_steps, 255);
    assert_eq!(stats.ram_trace_entries, 1);
}

#[test]
fn native_byte_and_dword_accesses_respect_both_vga_exclusion_edges() {
    for byte in [false, true] {
        for address in [0x9FFFC, 0x9FFFD, 0x9FFFF, 0xA0000, 0xAFFFF, 0xB0000] {
            for store in [false, true] {
                let opcode = match (byte, store) {
                    (false, false) => 0x8B,
                    (false, true) => 0x89,
                    (true, false) => 0x8A,
                    (true, true) => 0x88,
                };
                let bytes = [opcode, 0x06, 0x47, 0x66, 0x90];
                let mut native = call_trace_machine(true, &bytes);
                let mut reference = call_trace_machine(false, &bytes);
                for m in [&mut native, &mut reference] {
                    m.cpu.registers.set32(Gpr32::Esi, address);
                    m.cpu.registers.set32(Gpr32::Eax, 0x1234_ABCD);
                }
                warm(&mut native);
                let allowed = address >= 0xB0000 || address + if byte { 1 } else { 4 } <= 0xA0000;
                let expected = if allowed { 2 } else { 0 };
                assert_eq!(
                    native.cpu.dynamic_steps_mut(&mut native.bus, 255),
                    expected,
                    "byte={byte} store={store} address={address:x}"
                );
                for _ in 0..expected {
                    reference.cpu.step(&mut reference.bus).unwrap();
                }
                state(&native, &reference);
                memory(&native, &reference);
            }
        }
    }
}

#[test]
fn native_bda_tick_reads_fall_back_and_preserve_bus_observation() {
    let bytes = [0x8B, 0x06, 0x47, 0x66, 0x90];
    let mut native = call_trace_machine(true, &bytes);
    let mut reference = call_trace_machine(false, &bytes);
    for m in [&mut native, &mut reference] {
        m.cpu.registers.set32(Gpr32::Esi, 0x46C);
    }
    warm(&mut native);
    #[cfg(feature = "profiling")]
    let before = native.bus.stats.bda_tick_reads;
    assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 255), 0);
    #[cfg(feature = "profiling")]
    assert_eq!(native.bus.stats.bda_tick_reads, before);
    native.step_with_idle_budget(2).unwrap();
    reference.step().unwrap();
    reference.step().unwrap();
    state(&native, &reference);
    memory(&native, &reference);
    #[cfg(feature = "profiling")]
    {
        assert_eq!(native.bus.stats.bda_tick_reads, before + 4);
        assert_eq!(
            native.bus.stats.bda_tick_reads,
            reference.bus.stats.bda_tick_reads
        );
    }
}

#[test]
fn dynamic_exit_profiling_preserves_trace_results_and_counts_zero_step_exits() {
    use doslet_x86::DynamicExitReason;

    for (case, expected_reason, budget) in [
        (0, DynamicExitReason::Budget, 5),
        (1, DynamicExitReason::UnsupportedInstruction, 256),
        (2, DynamicExitReason::LoadGuard, 256),
        (3, DynamicExitReason::ActiveCode, 256),
        (4, DynamicExitReason::GraphLimit, 256),
    ] {
        let mut measured = if case == 3 {
            ram_store_machine(true)
        } else {
            ram_scan_machine(true)
        };
        let mut normal = if case == 3 {
            ram_store_machine(true)
        } else {
            ram_scan_machine(true)
        };
        measured.cpu.set_dynamic_exit_profiling(true).unwrap();
        for m in [&mut measured, &mut normal] {
            match case {
                1 => m.cpu.registers.set32(Gpr32::Edx, 0),
                2 => m.cpu.registers.set32(Gpr32::Esi, 0xA0000),
                3 => m.cpu.registers.set32(Gpr32::Edi, 0x160800),
                4 => {
                    let mut bytes = alloc::vec![0x8B, 0x06];
                    bytes.extend_from_slice(&[0x40; 20]);
                    bytes.push(0xFA);
                    code(m, 0x10000, &bytes);
                }
                _ => {}
            }
            warm(m);
        }
        assert_eq!(
            measured
                .cpu
                .dynamic_exit_profile()
                .iter()
                .map(|record| record.count)
                .sum::<u64>(),
            0
        );
        let entry_ip = measured.cpu.registers.ip;
        let used = measured.cpu.dynamic_steps_mut(&mut measured.bus, budget);
        assert_eq!(used, normal.cpu.dynamic_steps_mut(&mut normal.bus, budget));
        state(&measured, &normal);
        memory(&measured, &normal);
        let records = measured.cpu.dynamic_exit_profile();
        // Exactly one compiled trace was invoked, even for a first-load guard
        // that returns zero steps and leaves all architectural state unchanged.
        assert_eq!(
            records.iter().map(|record| record.count).sum::<u64>(),
            1,
            "case {case}"
        );
        let record = records.iter().find(|record| record.count != 0).unwrap();
        assert_eq!(record.reason, expected_reason, "case {case}");
        assert_eq!(record.entry_ip, entry_ip);
        assert_eq!(record.exit_ip, measured.cpu.registers.ip);
        assert_eq!(record.cs, measured.cpu.registers.cs);
        assert_eq!(record.steps, u64::from(used));
        assert_eq!(measured.cpu.dynamic_exit_profile_dropped(), 0);
        assert!(
            normal
                .cpu
                .dynamic_exit_profile()
                .iter()
                .all(|record| record.count == 0)
        );
        if case == 2 {
            assert_eq!(used, 0);
        }
    }
}

#[test]
fn native_ram_stores_match_each_partial_budget_and_cross_page_dwords() {
    for budget in [1, 2, 3, 4, 6, 7, 8, 15, 16, 31, 63, 255, 256] {
        let mut native = ram_store_machine(true);
        let mut reference = ram_store_machine(false);
        warm(&mut native);
        while native.cpu.registers.ip != 0x160010 {
            let (_, used) = native.step_with_idle_budget(budget).unwrap();
            assert!(used > 0 && used <= budget);
            for _ in 0..used {
                reference.step().unwrap();
            }
            state(&native, &reference);
            assert_eq!(
                native.bus.ram.view(0x140FF0..0x141090),
                reference.bus.ram.view(0x140FF0..0x141090)
            );
        }
        assert_eq!(native.bus.read_u32(0x140FFD).unwrap(), 1);
        assert_eq!(native.cpu.registers.get32(Gpr32::Ecx), 0);
        if budget == 256 {
            let stats = native.cpu.dynamic_stats();
            assert!(stats.ram_trace_steps > 16);
            assert_eq!(stats.ram_trace_entries, 1);
            assert_eq!(stats.ram_trace_steps, 32 * 7);
        }
        memory(&native, &reference);
    }
}

#[test]
fn native_ram_store_to_active_code_exits_before_the_store() {
    let mut native = ram_store_machine(true);
    let mut reference = ram_store_machine(false);
    for m in [&mut native, &mut reference] {
        m.bus.write_u32(0x120000, 0x9090908F).unwrap();
        m.cpu.registers.set32(Gpr32::Edi, 0x160007);
    }
    warm(&mut native);
    let before = native.cpu.dynamic_stats();
    assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 256), 2);
    for _ in 0..2 {
        reference.cpu.step(&mut reference.bus).unwrap();
    }
    state(&native, &reference);
    assert_eq!(native.bus.read_u32(0x160007).unwrap(), 0x8304C683);
    native.cpu.step(&mut native.bus).unwrap();
    reference.cpu.step(&mut reference.bus).unwrap();
    state(&native, &reference);
    memory(&native, &reference);
    assert_eq!(native.bus.read_u32(0x160007).unwrap(), 0x90909090);
    let after = native.cpu.dynamic_stats();
    assert_eq!(after.ram_trace_steps - before.ram_trace_steps, 2);
    assert_eq!(
        after.batched_interpreter_steps,
        before.batched_interpreter_steps
    );
    native.cpu.registers.ip = 0x160000;
    assert_eq!(native.cpu.dynamic_steps(&native.bus, 256), 0);
}

#[test]
fn native_ram_store_protects_neighbors_on_the_active_code_page() {
    let mut native = ram_store_machine(true);
    let mut reference = ram_store_machine(false);
    for m in [&mut native, &mut reference] {
        m.cpu.registers.set32(Gpr32::Edi, 0x160800);
    }
    warm(&mut native);
    // This address is outside the trace bytes but shares their tracked page.
    assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 256), 2);
    for _ in 0..2 {
        reference.cpu.step(&mut reference.bus).unwrap();
    }
    state(&native, &reference);
    assert_eq!(native.bus.read_u32(0x160800).unwrap(), 0);
    native.cpu.step(&mut native.bus).unwrap();
    reference.cpu.step(&mut reference.bus).unwrap();
    state(&native, &reference);
    memory(&native, &reference);
    assert_eq!(native.bus.read_u32(0x160800).unwrap(), 1);
    native.cpu.registers.ip = 0x160000;
    let before = native.cpu.dynamic_stats();
    assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 2), 2);
    let after = native.cpu.dynamic_stats();
    assert!(after.invalidations > before.invalidations);
    assert_eq!(after.compiled_blocks, before.compiled_blocks);
}

#[test]
fn native_ram_store_invalidates_another_prepared_block() {
    let mut native = ram_store_machine(true);
    let mut reference = ram_store_machine(false);
    for m in [&mut native, &mut reference] {
        code(m, 0x180000, &[0x40, 0x41, 0x66, 0x90]);
    }
    warm(&mut native);
    for m in [&mut native, &mut reference] {
        code(m, 0x160000, RAM_STORE_LOOP);
        m.bus.write_u32(0x120000, 0x00EB4147).unwrap(); // +1 -> DEC EAX; INC ECX; JMP.
        m.cpu.registers.set32(Gpr32::Edi, 0x180000);
        m.cpu.registers.set32(Gpr32::Ecx, 1);
    }
    warm(&mut native);
    assert_eq!(native.step_with_idle_budget(7).unwrap().1, 7);
    for _ in 0..7 {
        reference.step().unwrap();
    }
    state(&native, &reference);
    memory(&native, &reference);
    native.cpu.registers.ip = 0x180000;
    reference.cpu.registers.ip = 0x180000;
    let before = native.cpu.dynamic_stats();
    assert_eq!(native.cpu.dynamic_steps(&native.bus, 32), 0);
    assert!(native.cpu.dynamic_stats().invalidations > before.invalidations);
    native.cpu.step(&mut native.bus).unwrap();
    reference.cpu.step(&mut reference.bus).unwrap();
    state(&native, &reference);
}

#[test]
fn native_ram_stores_guard_mmio_bounds_and_preserve_completed_prefixes() {
    for destination in [0xA0000, 0xFFFF_FFFE, 0x1000000, 0xFFFFFE] {
        let mut native = ram_store_machine(true);
        let mut reference = ram_store_machine(false);
        warm(&mut native);
        for m in [&mut native, &mut reference] {
            m.cpu.registers.set32(Gpr32::Edi, destination);
        }
        // The load and ADD are valid. The rejected store has no memory/device
        // effects and its scheduling step must remain unconsumed.
        assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 256), 2);
        for _ in 0..2 {
            reference.cpu.step(&mut reference.bus).unwrap();
        }
        state(&native, &reference);
        assert_eq!(native.cpu.registers.ip, 0x160005);
        memory(&native, &reference);
    }
}

#[test]
fn native_ram_stores_require_flat_writable_segments_and_a20() {
    let mut native = ram_store_machine(true);
    warm(&mut native);
    for (selector, base, access, granularity) in [
        (16, 0, 0x91, 0xCF),
        (16, 0x1000, 0x93, 0xCF),
        (16, 0, 0x93, 0x4F),
        (24, 0, 0x91, 0xCF),
    ] {
        descriptor(&mut native.bus, 16, 0, 0xFFFFF, false, true);
        native.bus.write_u8(0x816, 0xCF).unwrap();
        native
            .cpu
            .load_segment(&mut native.bus, SegmentRegister::Ds, 16)
            .unwrap();
        // Populate the positive validation cache before changing its inputs.
        assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 2), 2);
        native.cpu.registers.ip = 0x160000;
        native.cpu.registers.set32(Gpr32::Eax, 0);
        native.cpu.flags = 2;
        descriptor(&mut native.bus, selector, base, 0xFFFFF, false, true);
        native
            .bus
            .write_u8(0x805 + u32::from(selector), access)
            .unwrap();
        native
            .bus
            .write_u8(0x806 + u32::from(selector), granularity)
            .unwrap();
        native
            .cpu
            .load_segment(&mut native.bus, SegmentRegister::Ds, selector)
            .unwrap();
        let before = ram_scan_registers(&native);
        assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 256), 0);
        assert_eq!(ram_scan_registers(&native), before);
        assert_eq!(native.bus.read_u32(0x140FFD).unwrap(), 0);
    }
    descriptor(&mut native.bus, 16, 0, 0xFFFFF, false, true);
    native.bus.write_u8(0x816, 0xCF).unwrap();
    native
        .cpu
        .load_segment(&mut native.bus, SegmentRegister::Ds, 16)
        .unwrap();
    native.bus.set_a20(false);
    let before = ram_scan_registers(&native);
    assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 256), 0);
    assert_eq!(ram_scan_registers(&native), before);
    native.bus.set_a20(true);
    assert_eq!(native.cpu.dynamic_steps_mut(&mut native.bus, 256), 32 * 7);
    assert_eq!(native.bus.read_u32(0x140FFD).unwrap(), 1);
}

#[test]
fn native_ram_scan_matches_reference_at_partial_budgets_and_internal_exits() {
    let mut native = ram_scan_machine(true);
    let mut reference = ram_scan_machine(false);
    warm(&mut native);
    for needle in [0, 1, 17, 31, u32::MAX] {
        for budget in [1, 2, 3, 7, 8, 9, 15, 16, 17, 31, 63, 255, 256] {
            for m in [&mut native, &mut reference] {
                m.cpu.registers.ip = 0x10000;
                m.cpu.registers.set32(Gpr32::Esi, 0x120000);
                m.cpu.registers.set32(Gpr32::Edi, 0);
                m.cpu.registers.set32(Gpr32::Edx, needle);
                m.cpu.flags = 2 | flags::DIRECTION | flags::CARRY;
            }
            let used = native.cpu.dynamic_steps(&native.bus, budget);
            assert!(used <= budget);
            for _ in 0..used {
                reference.cpu.step(&mut reference.bus).unwrap();
            }
            state(&native, &reference);
            // Finish through the output store using irregular machine batches.
            while native.cpu.registers.ip < 0x10019 {
                let (_, completed) = native.step_with_idle_budget(budget).unwrap();
                for _ in 0..completed {
                    reference.step().unwrap();
                }
                state(&native, &reference);
            }
            assert_eq!(
                native.bus.read_u32(0x40000),
                reference.bus.read_u32(0x40000)
            );
        }
    }
    let stats = native.cpu.dynamic_stats();
    assert!(
        stats.executed_steps > 1000,
        "RAM trace was never used: {stats:?}"
    );
    // Measure one entry separately from the small-budget/early-exit cases.
    for m in [&mut native, &mut reference] {
        m.cpu.registers.ip = 0x10000;
        m.cpu.registers.set32(Gpr32::Esi, 0x120000);
        m.cpu.registers.set32(Gpr32::Edi, 0);
        m.cpu.registers.set32(Gpr32::Edx, u32::MAX);
        m.cpu.flags = 2;
    }
    let before = native.cpu.dynamic_stats();
    let used = native.cpu.dynamic_steps(&native.bus, 256);
    assert!(used > 16 && used <= 256);
    assert_eq!(
        native.cpu.dynamic_stats().executed_blocks - before.executed_blocks,
        1
    );
    for _ in 0..used {
        reference.cpu.step(&mut reference.bus).unwrap();
    }
    state(&native, &reference);
    memory(&native, &reference);
}

#[test]
fn native_ram_scan_rejects_changed_segments_a20_and_unmapped_loads() {
    let mut native = ram_scan_machine(true);
    warm(&mut native);
    for (base, limit, granularity) in [(0x100, 0xFFFFF, 0xCF), (0, 0xFFFFF, 0x4F)] {
        descriptor(&mut native.bus, 16, base, limit, false, true);
        native.bus.write_u8(0x816, granularity).unwrap();
        native
            .cpu
            .load_segment(&mut native.bus, SegmentRegister::Ds, 16)
            .unwrap();
        native.cpu.registers.ip = 0x10000;
        let before = ram_scan_registers(&native);
        assert_eq!(native.cpu.dynamic_steps(&native.bus, 256), 0);
        assert_eq!(ram_scan_registers(&native), before);
    }
    descriptor(&mut native.bus, 16, 0, 0xFFFFF, false, true);
    native.bus.write_u8(0x816, 0xCF).unwrap();
    native
        .cpu
        .load_segment(&mut native.bus, SegmentRegister::Ds, 16)
        .unwrap();
    for address in [0xA0000, 0xFFFF_FFFE, 0x1000000] {
        native.cpu.registers.ip = 0x10000;
        native.cpu.registers.set32(Gpr32::Esi, address);
        let before = ram_scan_registers(&native);
        assert_eq!(native.cpu.dynamic_steps(&native.bus, 256), 0);
        assert_eq!(ram_scan_registers(&native), before);
    }
    native.cpu.registers.set32(Gpr32::Esi, 0x120000);
    native.bus.set_a20(false);
    assert_eq!(native.cpu.dynamic_steps(&native.bus, 256), 0);
    native.bus.set_a20(true);
    assert!(native.cpu.dynamic_steps(&native.bus, 256) > 16);
}

#[test]
fn native_ram_scan_rechecks_code_and_bus_ownership() {
    let mut native = ram_scan_machine(true);
    warm(&mut native);
    let mut replacement = ram_scan_machine(false);
    replacement.bus.write_u8(0x10000, 0x90).unwrap();
    let before = ram_scan_registers(&native);
    assert_eq!(native.cpu.dynamic_steps(&replacement.bus, 256), 0);
    assert_eq!(ram_scan_registers(&native), before);
    native.bus.write_u8(0x10000, 0x90).unwrap();
    assert_eq!(native.cpu.dynamic_steps(&native.bus, 256), 0);
    assert_eq!(ram_scan_registers(&native), before);
}

#[test]
fn short_observed_profile_does_not_pin_a_larger_current_ram_trace() {
    let mut bytes = alloc::vec![0x90, 0x90];
    bytes.extend_from_slice(RAM_SCAN);
    let mut native = ram_scan_machine(true);
    let mut reference = ram_scan_machine(false);
    code(&mut native, 0x10000, &bytes);
    code(&mut reference, 0x10000, &bytes);

    // An older observation contains only the two NOPs. It is valid DSJ1 code,
    // but must not override discovery of the current complete RAM trace.
    let mut short_profile = alloc::vec::Vec::from(&b"DSJ1\x01\x00"[..]);
    short_profile.extend_from_slice(&0x10000_u32.to_le_bytes());
    short_profile.extend_from_slice(&[2, 0x90, 0x90]);
    native.cpu.load_dynamic_profile(&short_profile).unwrap();
    assert_eq!(native.cpu.prepare_dynamic(8), 1);
    for _ in 0..16 {
        native.cpu.registers.ip = 0x10000;
        assert_eq!(native.cpu.dynamic_steps(&native.bus, 256), 0);
    }
    assert_eq!(native.cpu.prepare_dynamic(8), 1);
    let before = native.cpu.dynamic_stats();
    let used = native.cpu.dynamic_steps(&native.bus, 256);
    assert!(used > 16 && used <= 256);
    assert_eq!(
        native.cpu.dynamic_stats().executed_blocks - before.executed_blocks,
        1
    );
    for _ in 0..used {
        reference.cpu.step(&mut reference.bus).unwrap();
    }
    state(&native, &reference);
    memory(&native, &reference);

    // A complete current observation should still prepare the same trace for
    // immediate use, without another discovery/warmup period.
    let mut profile = alloc::vec![0; doslet_x86::DYNAMIC_PROFILE_MAX_BYTES];
    let length = native.cpu.write_dynamic_profile(&mut profile);
    assert!(length > short_profile.len());
    let mut next = ram_scan_machine(true);
    let mut next_reference = ram_scan_machine(false);
    code(&mut next, 0x10000, &bytes);
    code(&mut next_reference, 0x10000, &bytes);
    next.cpu.load_dynamic_profile(&profile[..length]).unwrap();
    assert_eq!(next.cpu.prepare_dynamic(8), 1);
    let used = next.cpu.dynamic_steps(&next.bus, 256);
    assert!(used > 16 && used <= 256);
    assert_eq!(next.cpu.dynamic_stats().prepared_hits, 1);
    for _ in 0..used {
        next_reference.cpu.step(&mut next_reference.bus).unwrap();
    }
    state(&next, &next_reference);
    memory(&next, &next_reference);
}

#[test]
fn native_ram_scan_exits_before_a_later_unmapped_load() {
    let mut native = ram_scan_machine(true);
    let mut reference = ram_scan_machine(false);
    warm(&mut native);
    for m in [&mut native, &mut reference] {
        // The first iteration is valid. Its pointer chase then reaches MMIO,
        // so checking only the first iteration's addresses is insufficient.
        m.bus.write_u32(0x120008, 0xA0000).unwrap();
    }
    let used = native.cpu.dynamic_steps(&native.bus, 256);
    assert_eq!(used, 8);
    for _ in 0..used {
        reference.cpu.step(&mut reference.bus).unwrap();
    }
    state(&native, &reference);
    assert_eq!(native.cpu.registers.get32(Gpr32::Esi), 0xA0000);
    assert_eq!(native.cpu.registers.ip, 0x10000);
    memory(&native, &reference);
}

#[test]
fn native_ram_scan_preserves_irq_delivery_and_timestamped_dma() {
    let mut native = ram_scan_machine(true);
    let mut reference = ram_scan_machine(false);
    for m in [&mut native, &mut reference] {
        // Make the original list cyclic so hardware, rather than list length,
        // bounds execution. Neither the JIT nor its caller can infer the links.
        m.bus.write_u32(0x120000 + 31 * 12 + 8, 0x120000).unwrap();
        m.cpu.registers.set32(Gpr32::Esp, 0x50000);
        m.cpu.control.idtr.base = 0x900;
        m.cpu.control.idtr.limit = 0xFF;
        for (index, byte) in [0, 0, 8, 0, 0, 0x8E, 3, 0].into_iter().enumerate() {
            m.bus.write_u8(0x940 + index as u32, byte).unwrap();
        }
        // INC ECX; MOV AL,20h; OUT 20h,AL; IRETD.
        for (index, byte) in [0x41, 0xB0, 0x20, 0xE6, 0x20, 0xCF].into_iter().enumerate() {
            m.bus.write_u8(0x30000 + index as u32, byte).unwrap();
        }
        m.bus.write_port_u8(0x21, 0xFE).unwrap(); // Only IRQ0 has a test gate.
        m.bus.pit.write_control(0x34).unwrap();
        m.bus.pit.write_counter(0, 37).unwrap();
        m.bus.pit.write_counter(0, 0).unwrap();
        m.bus.write_u8(0x80000, 0xC3).unwrap();
        m.bus.write_port_u8(0x0C, 0).unwrap();
        for value in [0, 0] {
            m.bus.write_port_u8(2, value).unwrap();
        }
        for value in [0, 0] {
            m.bus.write_port_u8(3, value).unwrap();
        }
        m.bus.write_port_u8(0x83, 8).unwrap();
        m.bus.write_port_u8(0x0B, 0x59).unwrap();
        m.bus.write_port_u8(0x0A, 1).unwrap();
        for value in [0xD1, 0x41, 0xFF, 0xFF, 0xC6, 0, 0, 0] {
            m.bus.write_port_u8(0x22C, value).unwrap();
        }
        m.cpu.flags = 2 | flags::INTERRUPT;
    }
    warm(&mut native);
    let mut audio_a = [doslet_pcm::Frame {
        nanos: 0,
        duration: 0,
        samples: [0; 2],
    }; 4];
    let mut audio_b = audio_a;
    let mut frames = 0;
    for budget in [1, 2, 7, 15, 31, 257, 4096].into_iter().cycle().take(100) {
        let mut remaining = budget;
        while remaining != 0 {
            let (_, used) = native.step_with_idle_budget(remaining).unwrap();
            assert!(used > 0 && used <= remaining);
            remaining -= used;
            for _ in 0..used {
                reference.step().unwrap();
            }
            state(&native, &reference);
            let count = native.drain_pcm_timed(&mut audio_a);
            assert_eq!(count, reference.drain_pcm_timed(&mut audio_b));
            assert_eq!(&audio_a[..count], &audio_b[..count]);
            frames += count;
        }
    }
    assert!(frames > 100);
    assert!(native.cpu.registers.get32(Gpr32::Ecx) > 0);
    let stats = native.cpu.dynamic_stats();
    assert!(stats.executed_steps > stats.executed_blocks * 16);
    memory(&native, &reference);
}

#[test]
fn native_loops_preserve_every_condition_flags_and_budget_remainder() {
    for near in [false, true] {
        for condition in 0..16 {
            let mut bytes = alloc::vec![0x90, 0x90];
            if near {
                bytes.extend_from_slice(&[0x0F, 0x80 | condition, 0xF8, 0xFF, 0xFF, 0xFF]);
            } else {
                bytes.extend_from_slice(&[0x70 | condition, 0xFC]);
            }
            let mut native = machine(true);
            let mut reference = machine(false);
            for m in [&mut native, &mut reference] {
                clear_shadow(m);
                code(m, 0x10000, &bytes);
            }
            warm(&mut native);
            for bits in 0..64 {
                let arithmetic = [
                    flags::CARRY,
                    flags::PARITY,
                    flags::AUXILIARY,
                    flags::ZERO,
                    flags::SIGN,
                    flags::OVERFLOW,
                ]
                .into_iter()
                .enumerate()
                .fold(0, |value, (index, flag)| {
                    value | if bits & (1 << index) == 0 { 0 } else { flag }
                });
                for budget in [0, 1, 2, 3, 4, 5, 6, 7, 31, 256] {
                    for m in [&mut native, &mut reference] {
                        m.cpu.registers.ip = 0x10000;
                        m.cpu.flags = 2 | flags::INTERRUPT | flags::DIRECTION | 0x3000 | arithmetic;
                    }
                    // Ask the original interpreter whether the branch is taken;
                    // the test does not duplicate the JIT's condition formulas.
                    let mut expected = 0;
                    if budget >= 3 {
                        for _ in 0..3 {
                            reference.cpu.step(&mut reference.bus).unwrap();
                        }
                        expected = if reference.cpu.registers.ip == 0x10000 {
                            budget / 3 * 3
                        } else {
                            3
                        };
                        for _ in 3..expected {
                            reference.cpu.step(&mut reference.bus).unwrap();
                        }
                    }
                    assert_eq!(
                        native.cpu.dynamic_steps(&native.bus, budget),
                        expected,
                        "near={near} condition={condition} flags={arithmetic:x} budget={budget}"
                    );
                    state(&native, &reference);
                }
            }
        }
    }
}

#[test]
fn native_finite_loops_carry_adc_and_sbb_between_iterations_and_exit() {
    for operation in [0x11, 0x19] {
        for near in [false, true] {
            // ADC/SBB EAX,EBX; DEC ECX; JNZ start. DEC must preserve the carry
            // that the next iteration consumes, including on a budget exit.
            let mut bytes = alloc::vec![operation, 0xD8, 0x49];
            if near {
                bytes.extend_from_slice(&[0x0F, 0x85, 0xF7, 0xFF, 0xFF, 0xFF]);
            } else {
                bytes.extend_from_slice(&[0x75, 0xFB]);
            }
            let mut native = machine(true);
            let mut reference = machine(false);
            for m in [&mut native, &mut reference] {
                clear_shadow(m);
                code(m, 0x10000, &bytes);
            }
            warm(&mut native);
            for iterations in [1, 2, 3, 17] {
                for initial in [0, 1, 0x7FFF_FFFF, 0x8000_0000, u32::MAX] {
                    for addend in [0, 1, 0x8000_0000, u32::MAX] {
                        for carry in [0, flags::CARRY] {
                            for budget in [0, 1, 2, 3, 4, 5, 6, 7, 50, 51, 52, 256] {
                                for m in [&mut native, &mut reference] {
                                    m.cpu.registers.ip = 0x10000;
                                    m.cpu.registers.set32(Gpr32::Eax, initial);
                                    m.cpu.registers.set32(Gpr32::Ebx, addend);
                                    m.cpu.registers.set32(Gpr32::Ecx, iterations);
                                    m.cpu.flags = 2 | flags::DIRECTION | carry;
                                }
                                let expected = iterations.min(budget / 3) * 3;
                                assert_eq!(native.cpu.dynamic_steps(&native.bus, budget), expected);
                                for _ in 0..expected {
                                    reference.cpu.step(&mut reference.bus).unwrap();
                                }
                                state(&native, &reference);
                                assert_eq!(
                                    native.cpu.registers.ip,
                                    if expected == iterations * 3 {
                                        0x10000 + bytes.len() as u32
                                    } else {
                                        0x10000
                                    }
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn native_unconditional_loops_repeat_in_one_entry_with_bounded_work() {
    for near in [false, true] {
        // ADC EAX,EBX; INC ECX; JMP start. INC preserves carry across iterations.
        let mut bytes = alloc::vec![0x11, 0xD8, 0x41];
        if near {
            bytes.extend_from_slice(&[0xE9, 0xF8, 0xFF, 0xFF, 0xFF]);
        } else {
            bytes.extend_from_slice(&[0xEB, 0xFB]);
        }
        let mut native = machine(true);
        let mut reference = machine(false);
        for m in [&mut native, &mut reference] {
            clear_shadow(m);
            code(m, 0x10000, &bytes);
        }
        warm(&mut native);
        for (budget, deadline) in [0, 1, 2, 3, 4, 5, 6, 7, 255, 256, 257, 1003]
            .into_iter()
            .map(|budget| (budget, budget))
            .chain([(1003, 0), (1003, 1), (1003, 2), (1003, 256), (4, 1003)])
        {
            for m in [&mut native, &mut reference] {
                m.cpu.registers.ip = 0x10000;
                m.cpu.registers.set32(Gpr32::Eax, u32::MAX);
                m.cpu.registers.set32(Gpr32::Ebx, 1);
                m.cpu.registers.set32(Gpr32::Ecx, 0x7FFF_FFFE);
                m.cpu.flags = 2 | flags::DIRECTION | flags::CARRY;
            }
            let before = native.cpu.dynamic_stats();
            let expected = budget.min(deadline) / 3 * 3;
            assert_eq!(
                native
                    .cpu
                    .dynamic_steps_with_limit(&native.bus, budget, || deadline),
                expected
            );
            for _ in 0..expected {
                reference.cpu.step(&mut reference.bus).unwrap();
            }
            state(&native, &reference);
            let after = native.cpu.dynamic_stats();
            assert_eq!(
                after.executed_steps - before.executed_steps,
                u64::from(expected)
            );
            assert_eq!(
                after.executed_blocks - before.executed_blocks,
                u64::from(expected != 0),
                "all iterations must execute in one native entry"
            );
        }
    }
}

#[test]
fn native_two_instruction_loop_resumes_until_the_condition_becomes_false() {
    let mut native = machine(true);
    let mut reference = machine(false);
    for m in [&mut native, &mut reference] {
        clear_shadow(m);
        code(m, 0x10000, &[0x49, 0x75, 0xFD]); // DEC ECX; JNZ start.
        m.cpu.registers.set32(Gpr32::Ecx, 17);
        m.cpu.flags = 2 | flags::DIRECTION | flags::CARRY;
    }
    warm(&mut native);
    let mut completed = 0;
    for budget in [0, 1, 2, 3, 4, 5, 7, 31] {
        let expected = (34 - completed).min(budget / 2 * 2);
        assert_eq!(native.cpu.dynamic_steps(&native.bus, budget), expected);
        for _ in 0..expected {
            reference.cpu.step(&mut reference.bus).unwrap();
        }
        completed += expected;
        state(&native, &reference);
    }
    assert_eq!(completed, 34);
    assert_eq!(native.cpu.registers.get32(Gpr32::Ecx), 0);
    assert_eq!(native.cpu.registers.ip, 0x10003);
    assert_ne!(native.cpu.flags & flags::CARRY, 0);
}

#[test]
fn native_loops_leave_operand_size_overridden_branches_to_the_interpreter() {
    for bytes in [
        &[0x90, 0x90, 0x66, 0x75, 0xFB][..],
        &[0x90, 0x90, 0x66, 0x0F, 0x85, 0xF9, 0xFF][..],
        &[0x90, 0x90, 0x66, 0xE9, 0xFA, 0xFF][..],
    ] {
        let mut native = machine(true);
        let mut reference = machine(false);
        for m in [&mut native, &mut reference] {
            clear_shadow(m);
            code(m, 0x10000, bytes);
        }
        warm(&mut native);
        assert_eq!(native.cpu.dynamic_steps(&native.bus, 256), 2);
        for _ in 0..2 {
            reference.cpu.step(&mut reference.bus).unwrap();
        }
        state(&native, &reference);
        assert_eq!(native.cpu.registers.ip, 0x10002);
        assert_eq!(native.cpu.dynamic_steps(&native.bus, 256), 0);
        state(&native, &reference);
        assert_eq!(
            native.cpu.step(&mut native.bus),
            reference.cpu.step(&mut reference.bus)
        );
        state(&native, &reference);
    }
}

#[test]
fn native_flag_liveness_preserves_carry_chains_and_last_writer() {
    for (bytes, count) in [
        (
            &[
                0x01, 0xC8, 0x11, 0xD8, 0x40, 0x19, 0xD0, 0x49, 0x11, 0xC8, 0x66, 0x90,
            ][..],
            6,
        ),
        (&[0x01, 0xC8, 0x90, 0x41, 0x66, 0x90][..], 3),
        (
            &[0x31, 0xC0, 0x19, 0xDB, 0x85, 0xC9, 0x11, 0xD2, 0x66, 0x90][..],
            4,
        ),
    ] {
        let mut native = machine(true);
        let mut reference = machine(false);
        code(&mut native, 0x10000, bytes);
        code(&mut reference, 0x10000, bytes);
        warm(&mut native);
        let mut seed = 0x12345678_u32;
        for round in 0..1024 {
            for register in 0..8 {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let value = if round < 8 {
                    [0, u32::MAX, 0x7fffffff, 0x80000000][(round + register) % 4]
                } else {
                    seed
                };
                for m in [&mut native, &mut reference] {
                    let reg = [
                        Gpr32::Eax,
                        Gpr32::Ecx,
                        Gpr32::Edx,
                        Gpr32::Ebx,
                        Gpr32::Esp,
                        Gpr32::Ebp,
                        Gpr32::Esi,
                        Gpr32::Edi,
                    ][register];
                    m.cpu.registers.set32(reg, value);
                    m.cpu.registers.ip = 0x10000;
                    m.cpu.flags = 2 | flags::DIRECTION | (seed & 0x8d5);
                }
            }
            assert_eq!(native.cpu.dynamic_steps(&native.bus, 32), count);
            for _ in 0..count {
                reference.cpu.step(&mut reference.bus).unwrap();
            }
            assert_eq!(native.cpu.registers, reference.cpu.registers);
            assert_eq!(native.cpu.flags, reference.cpu.flags);
        }
    }
}

fn clear_shadow(m: &mut PcMachine) {
    code(m, 0x90000, &[0x90]);
    m.cpu.step(&mut m.bus).unwrap();
}

#[test]
fn batches_stop_before_if_changes_ports_mmio_and_faults() {
    for tail in [
        &[0xFA][..],                // CLI
        &[0xFB][..],                // STI
        &[0x9D][..],                // POPFD
        &[0xEC][..],                // IN AL,DX
        &[0xEE][..],                // OUT DX,AL
        &[0xA1, 0, 0, 0x0A, 0][..], // VGA aperture
        &[0xA1, 0, 0, 0x10, 0][..], // Outside the DS limit
        &[0x8E, 0xD0][..],          // MOV SS,AX
        &[0xF3, 0xA6][..],          // REPE CMPSB stays uncached
    ] {
        let mut native = machine(true);
        let mut reference = machine(false);
        for m in [&mut native, &mut reference] {
            clear_shadow(m);
            code(m, 0x10001, tail);
            code(m, 0x10000, &[0x40]);
        }
        assert_eq!(native.step_with_idle_budget(256).unwrap().1, 1);
        reference.step().unwrap();
        state(&native, &reference);
        assert_eq!(native.cpu.registers.ip, 0x10001);
        if tail == [0xA1, 0, 0, 0x10, 0] {
            assert_eq!(
                native.step_with_idle_budget(256).unwrap_err(),
                reference.step().unwrap_err()
            );
            state(&native, &reference);
        }
    }
}

#[test]
fn ram_batches_repeat_strings_one_iteration_per_step() {
    let mut native = machine(true);
    let mut reference = machine(false);
    for m in [&mut native, &mut reference] {
        clear_shadow(m);
        // REP MOVSD copies four dwords from 20000h to 21000h, then CLI ends the slice.
        code(m, 0x10000, &[0xF3, 0xA5, 0xFA]);
        for offset in 0..16 {
            m.bus.write_u8(0x20000 + offset, offset as u8 + 1).unwrap();
        }
        m.cpu.registers.set32(Gpr32::Esi, 0x20000);
        m.cpu.registers.set32(Gpr32::Edi, 0x21000);
        m.cpu.registers.set32(Gpr32::Ecx, 4);
        m.cpu.flags = 2;
    }
    assert_eq!(native.step_with_idle_budget(256).unwrap().1, 4);
    for _ in 0..4 {
        reference.step().unwrap();
    }
    assert_eq!(native.cpu.registers.get32(Gpr32::Ecx), 0);
    assert_eq!(native.cpu.registers.ip, 0x10002);
    state(&native, &reference);
    memory(&native, &reference);
}

#[test]
fn ram_batches_revalidate_self_modified_code_and_account_once() {
    let mut native = machine(true);
    let mut reference = machine(false);
    for m in [&mut native, &mut reference] {
        clear_shadow(m);
        code(m, 0x10007, &[0x40, 0x41, 0xFA]);
    }
    warm(&mut native);
    for m in [&mut native, &mut reference] {
        // MOV BYTE [10007h],48h changes the prepared INC EAX into DEC EAX.
        code(m, 0x10000, &[0xC6, 0x05, 7, 0, 1, 0, 0x48]);
        m.cpu.registers.set32(Gpr32::Eax, 10);
        m.cpu.registers.set32(Gpr32::Ecx, 20);
        m.cpu.flags = 2;
    }
    assert_eq!(native.step_with_idle_budget(256).unwrap().1, 3);
    for _ in 0..3 {
        reference.step().unwrap();
    }
    assert_eq!(native.cpu.registers.get32(Gpr32::Eax), 9);
    assert_eq!(native.cpu.registers.get32(Gpr32::Ecx), 21);
    assert_eq!(native.cpu.dynamic_stats().execution_batches, 1);
    state(&native, &reference);
    memory(&native, &reference);
    #[cfg(feature = "profiling")]
    assert_eq!(native.execution_stats(), reference.execution_stats());
}

#[test]
fn ram_batches_stop_at_dma_sample_deadlines_and_preserve_timestamped_audio() {
    let mut native = machine(true);
    let mut reference = machine(false);
    for m in [&mut native, &mut reference] {
        clear_shadow(m);
        // Modify DMA sample data while looping, so delayed hardware would read
        // the wrong value even if final registers and RAM happened to match.
        code(m, 0x10000, &[0x40, 0xA3, 0, 0, 8, 0, 0xEB, 0xF8]);
        m.bus.write_port_u8(0x0C, 0).unwrap();
        for value in [0, 0] {
            m.bus.write_port_u8(2, value).unwrap();
        }
        for value in [0, 0] {
            m.bus.write_port_u8(3, value).unwrap();
        }
        m.bus.write_port_u8(0x83, 8).unwrap();
        m.bus.write_port_u8(0x0B, 0x59).unwrap(); // DMA1, auto-init, memory -> device.
        m.bus.write_port_u8(0x0A, 1).unwrap();
        for value in [0xD1, 0x41, 0xAC, 0x44, 0xC6, 0, 0, 0] {
            m.bus.write_port_u8(0x22C, value).unwrap();
        }
    }
    let mut audio_a = [doslet_pcm::Frame {
        nanos: 0,
        duration: 0,
        samples: [0; 2],
    }; 128];
    let mut audio_b = audio_a;
    let mut frames = 0;
    for budget in [1, 7, 31, 257, 4096].into_iter().cycle().take(100) {
        let mut remaining = budget;
        while remaining != 0 {
            remaining -= native.step_with_idle_budget(remaining).unwrap().1;
        }
        for _ in 0..budget {
            reference.step().unwrap();
        }
        state(&native, &reference);
        let count = native.drain_pcm_timed(&mut audio_a);
        assert_eq!(count, reference.drain_pcm_timed(&mut audio_b));
        assert_eq!(&audio_a[..count], &audio_b[..count]);
        frames += count;
    }
    assert!(frames > 100);
    assert!(native.cpu.dynamic_stats().batched_interpreter_steps > 10000);
    memory(&native, &reference);
}

#[test]
fn native_loops_yield_at_dma_sample_deadlines() {
    let mut native = machine(true);
    let mut reference = machine(false);
    for m in [&mut native, &mut reference] {
        clear_shadow(m);
        // Three scheduling steps per iteration, all executed inside native code.
        code(m, 0x10000, &[0x40, 0x41, 0xEB, 0xFC]);
        m.bus.write_u8(0x80000, 0xC3).unwrap();
        m.bus.write_port_u8(0x0C, 0).unwrap();
        for value in [0, 0] {
            m.bus.write_port_u8(2, value).unwrap();
        }
        for value in [0, 0] {
            m.bus.write_port_u8(3, value).unwrap();
        }
        m.bus.write_port_u8(0x83, 8).unwrap();
        m.bus.write_port_u8(0x0B, 0x59).unwrap();
        m.bus.write_port_u8(0x0A, 1).unwrap();
        // 65535 Hz makes crossing two sample deadlines possible in 256 steps.
        for value in [0xD1, 0x41, 0xFF, 0xFF, 0xC6, 0, 0, 0] {
            m.bus.write_port_u8(0x22C, value).unwrap();
        }
    }
    warm(&mut native);
    let mut audio_a = [doslet_pcm::Frame {
        nanos: 0,
        duration: 0,
        samples: [0; 2],
    }; 4];
    let mut audio_b = audio_a;
    let mut frames = 0;
    for budget in [1, 2, 3, 4, 7, 31, 257, 4096].into_iter().cycle().take(100) {
        let mut remaining = budget;
        while remaining != 0 {
            let (_, used) = native.step_with_idle_budget(remaining).unwrap();
            assert!(used > 0 && used <= remaining);
            remaining -= used;
            for _ in 0..used {
                reference.step().unwrap();
            }
            state(&native, &reference);
            let count = native.drain_pcm_timed(&mut audio_a);
            assert_eq!(count, reference.drain_pcm_timed(&mut audio_b));
            assert_eq!(&audio_a[..count], &audio_b[..count]);
            frames += count;
        }
    }
    assert!(frames > 100);
    let stats = native.cpu.dynamic_stats();
    assert!(stats.executed_steps > stats.executed_blocks * 16);
    memory(&native, &reference);
    #[cfg(feature = "profiling")]
    assert_eq!(native.execution_stats(), reference.execution_stats());
}

fn warm(m: &mut PcMachine) {
    let ip = m.cpu.registers.ip;
    if m.cpu.has_interrupt_shadow() {
        m.cpu.step(&mut m.bus).unwrap();
        m.cpu.registers.ip = ip;
    }
    for _ in 0..16 {
        m.cpu.dynamic_steps(&m.bus, 32);
    }
    assert!(
        m.cpu.prepare_dynamic(8) > 0,
        "error={:?} stats={:?} ip={:x} cs={:?} bytes={:?}",
        m.cpu.dynamic_error(),
        m.cpu.dynamic_stats(),
        m.cpu.registers.ip,
        m.cpu.registers.segment_cache(SegmentRegister::Cs),
        m.bus.code_bytes(m.cpu.registers.ip, 32)
    );
}

#[test]
fn native_code_survives_execution_cache_collisions_and_rebinding_at_other_addresses() {
    let mut m = machine(true);
    let bytes = [0x40, 0x41, 0x66, 0x90];
    code(&mut m, 0x10000, &bytes);
    warm(&mut m);
    let compiled = m.cpu.dynamic_stats().compiled_blocks;
    for address in [0x10401, 0x10000, 0x10401, 0x10000] {
        code(&mut m, address, &bytes);
        assert_eq!(m.cpu.dynamic_steps(&m.bus, 32), 2);
    }
    assert_eq!(m.cpu.dynamic_stats().compiled_blocks, compiled);
    assert_eq!(m.cpu.prepare_dynamic(8), 0);
    assert!(m.cpu.dynamic_stats().prepared_hits >= 4);
}

#[test]
fn observed_profile_prepares_first_entry_but_never_trusts_changed_ram() {
    let mut first = machine(true);
    let bytes = [0x40, 0x41, 0x42, 0x43, 0x66, 0x90];
    code(&mut first, 0x10000, &bytes);
    warm(&mut first);
    let mut profile = alloc::vec![0; doslet_x86::DYNAMIC_PROFILE_MAX_BYTES];
    let length = first.cpu.write_dynamic_profile(&mut profile);
    assert!(length > 6);
    let mut next = machine(true);
    code(&mut next, 0x10000, &bytes);
    next.cpu.step(&mut next.bus).unwrap();
    next.cpu.registers.ip = 0x10000;
    next.cpu.load_dynamic_profile(&profile[..length]).unwrap();
    assert_eq!(next.cpu.prepare_dynamic(8), 1);
    assert_eq!(next.cpu.dynamic_steps(&next.bus, 32), 4);
    assert_eq!(next.cpu.dynamic_stats().prepared_hits, 1);
    next.cpu.registers.ip = 0x10000;
    next.bus.write_u8(0x10000, 0x48).unwrap();
    assert_eq!(next.cpu.dynamic_steps(&next.bus, 32), 0);
    for end in 0..length {
        assert!(next.cpu.load_dynamic_profile(&profile[..end]).is_err());
    }
    profile[6 + 5] = 0xCD; // INT cannot become a native operation.
    assert!(next.cpu.load_dynamic_profile(&profile[..length]).is_err());
}

#[test]
fn native_alu_flags_match_reference_at_overflow_carry_and_random_inputs() {
    let mut native = machine(true);
    let mut reference = machine(false);
    let mut seed = 0x1234_5678_u32;
    for operation in 0..8 {
        // ALU EAX,ECX; NOP; word override ends the native block.
        let bytes = [operation * 8 + 1, 0xC8, 0x90, 0x66, 0x90];
        code(&mut native, 0x10000, &bytes);
        code(&mut reference, 0x10000, &bytes);
        warm(&mut native);
        for round in 0..2048 {
            let edge = [0, 1, 0x7FFF_FFFF, 0x8000_0000, u32::MAX, 15, 16, 255];
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let left = if round < 128 {
                edge[(round / 16) % 8]
            } else {
                seed
            };
            let right = if round < 128 {
                edge[(round / 2) % 8]
            } else {
                seed.rotate_left(13)
            };
            for m in [&mut native, &mut reference] {
                m.cpu.registers.ip = 0x10000;
                m.cpu.registers.set32(Gpr32::Eax, left);
                m.cpu.registers.set32(Gpr32::Ecx, right);
                m.cpu.flags = 2 | flags::DIRECTION | if round % 2 == 0 { 0 } else { 0x8D5 };
            }
            assert_eq!(native.cpu.dynamic_steps(&native.bus, 32), 2);
            reference.cpu.step(&mut reference.bus).unwrap();
            reference.cpu.step(&mut reference.bus).unwrap();
            assert_eq!(
                native.cpu.registers, reference.cpu.registers,
                "op {operation} round {round}"
            );
            assert_eq!(
                native.cpu.flags, reference.cpu.flags,
                "op {operation} round {round}"
            );
        }
    }
    assert_eq!(native.cpu.dynamic_stats().executed_steps, 8 * 2048 * 2);
}

#[test]
fn native_moves_inc_dec_exchange_neg_not_and_test_match_reference() {
    let bytes = [
        0xB8, 0, 0, 0, 0x80, // MOV EAX,80000000h
        0x89, 0xC1, // MOV ECX,EAX
        0x40, 0x49, // INC EAX; DEC ECX
        0x91, // XCHG EAX,ECX
        0xF7, 0xD8, // NEG EAX
        0xF7, 0xD1, // NOT ECX
        0x85, 0xC8, // TEST EAX,ECX
        0x66, 0x90,
    ];
    let mut native = machine(true);
    let mut reference = machine(false);
    code(&mut native, 0x10000, &bytes);
    code(&mut reference, 0x10000, &bytes);
    warm(&mut native);
    assert_eq!(native.cpu.dynamic_steps(&native.bus, 32), 8);
    for _ in 0..8 {
        reference.cpu.step(&mut reference.bus).unwrap();
    }
    assert_eq!(native.cpu.registers, reference.cpu.registers);
    assert_eq!(native.cpu.flags, reference.cpu.flags);
}

#[test]
fn native_code_mutation_cross_page_a20_and_replacement_bus_are_validated() {
    let address = 0x10FFD;
    let mut m = machine(true);
    code(&mut m, address, &[0xB8, 1, 0, 0, 0, 0x40, 0x66, 0x90]);
    warm(&mut m);
    assert_eq!(m.cpu.dynamic_steps(&m.bus, 32), 2);
    assert_eq!(m.cpu.registers.get32(Gpr32::Eax), 2);
    m.cpu.registers.ip = address;
    m.bus.write_u16(address + 3, 0xCAFE).unwrap();
    assert_eq!(m.cpu.dynamic_steps(&m.bus, 32), 0);
    warm(&mut m);
    assert_eq!(m.cpu.dynamic_steps(&m.bus, 32), 2);
    assert_eq!(m.cpu.registers.get32(Gpr32::Eax), 0xCAFE0002);
    m.cpu.registers.ip = address;
    // Unrelated write on a code page can rebind unchanged prepared bytes.
    m.bus.write_u8(address - 1, 7).unwrap();
    assert_eq!(m.cpu.dynamic_steps(&m.bus, 32), 2);
    m.cpu.registers.ip = address;
    m.bus.set_a20(false);
    m.bus.write_u32((address + 1) | (1 << 20), 7).unwrap();
    assert_eq!(m.cpu.dynamic_steps(&m.bus, 32), 0);
    warm(&mut m);
    assert_eq!(m.cpu.dynamic_steps(&m.bus, 32), 2);
    assert_eq!(m.cpu.registers.get32(Gpr32::Eax), 8);
    let mut replacement = machine(false);
    code(
        &mut replacement,
        address,
        &[0xB8, 42, 0, 0, 0, 0x40, 0x66, 0x90],
    );
    m.cpu.registers.ip = address;
    assert_eq!(m.cpu.dynamic_steps(&replacement.bus, 32), 0);
    assert_eq!(m.cpu.registers.get32(Gpr32::Eax), 8);
    assert!(m.cpu.dynamic_stats().invalidations >= 4);
}

#[test]
fn native_budget_debug_paging_shadow_and_cs_limit_fall_back_without_side_effects() {
    let mut m = machine(true);
    code(&mut m, 0x10000, &[0x40, 0x41, 0x66, 0x90]);
    warm(&mut m);
    for budget in [0, 1] {
        assert_eq!(m.cpu.dynamic_steps(&m.bus, budget), 0);
    }
    for flag in [flags::TRAP, flags::RESUME, flags::VIRTUAL] {
        m.cpu.flags |= flag;
        assert_eq!(m.cpu.dynamic_steps(&m.bus, 32), 0);
        m.cpu.flags &= !flag;
    }
    m.cpu.control.cr0 |= 0x8000_0000;
    assert_eq!(m.cpu.dynamic_steps(&m.bus, 32), 0);
    m.cpu.control.cr0 &= !0x8000_0000;
    m.cpu
        .load_segment(&mut m.bus, SegmentRegister::Ss, 16)
        .unwrap();
    assert_eq!(m.cpu.dynamic_steps(&m.bus, 32), 0);
    m.cpu.step(&mut m.bus).unwrap();
    m.cpu.registers.ip = 0x10000;
    descriptor(&mut m.bus, 8, 0, 0x10000, true, true);
    m.cpu
        .load_segment(&mut m.bus, SegmentRegister::Cs, 8)
        .unwrap();
    let before = m.cpu.registers.get32(Gpr32::Eax);
    assert_eq!(m.cpu.dynamic_steps(&m.bus, 32), 0);
    assert_eq!(m.cpu.registers.get32(Gpr32::Eax), before);
    assert_eq!(m.cpu.registers.ip, 0x10000);
}

#[test]
fn native_batches_preserve_pit_irq_delivery_and_varied_step_budgets() {
    let mut native = machine(true);
    let mut reference = machine(false);
    for m in [&mut native, &mut reference] {
        code(m, 0x10000, &[0x40, 0x41, 0x42, 0x43, 0xEB, 0xFA]);
        m.cpu.step(&mut m.bus).unwrap(); // Clear SS interrupt shadow.
        m.cpu.registers.ip = 0x10000;
        m.cpu.registers.set32(Gpr32::Esp, 0x50000);
        m.cpu.control.idtr.base = 0x900;
        m.cpu.control.idtr.limit = 0xFF;
        // IRQ0 -> 0008:00030000, same-privilege 32-bit interrupt gate.
        for (i, byte) in [0, 0, 8, 0, 0, 0x8E, 3, 0].into_iter().enumerate() {
            m.bus.write_u8(0x940 + i as u32, byte).unwrap();
        }
        // INC EDI; MOV AL,20h; OUT 20h,AL; IRETD.
        for (i, byte) in [0x47, 0xB0, 0x20, 0xE6, 0x20, 0xCF].into_iter().enumerate() {
            m.bus.write_u8(0x30000 + i as u32, byte).unwrap();
        }
        m.bus.pit.write_control(0x34).unwrap();
        m.bus.pit.write_counter(0, 37).unwrap();
        m.bus.pit.write_counter(0, 0).unwrap();
        m.cpu.flags = 2 | flags::INTERRUPT;
    }
    warm(&mut native);
    for round in 0..200 {
        let budget = [1, 2, 7, 31, 257, 1009][round % 6];
        let mut remaining = budget;
        while remaining != 0 {
            let (_, used) = native.step_with_idle_budget(remaining).unwrap();
            assert!(used > 0 && used <= remaining);
            remaining -= used;
        }
        for _ in 0..budget {
            reference.step().unwrap();
        }
        state(&native, &reference);
    }
    assert!(native.cpu.dynamic_stats().executed_steps > 1000);
    assert!(
        native.cpu.dynamic_stats().executed_steps > native.cpu.dynamic_stats().executed_blocks * 5,
        "the IRQ test must exercise multiple loop iterations per native entry"
    );
    assert!(native.cpu.registers.get32(Gpr32::Edi) > 0);
    memory(&native, &reference);
}

#[test]
fn delayed_compilation_keeps_executing_and_publication_rechecks_guest_code() {
    let mut native = machine(true);
    let mut reference = machine(false);
    for m in [&mut native, &mut reference] {
        clear_shadow(m);
        code(m, 0x10000, &[0x40, 0x43, 0xEB, 0xFC]);
    }
    for _ in 0..16 {
        assert_eq!(native.cpu.dynamic_steps(&native.bus, 32), 0);
    }
    let job = native.cpu.take_dynamic_compilation(2).unwrap();
    // A deliberately delayed host job cannot prevent guest progress.
    assert_eq!(native.cpu.dynamic_batch(&mut native.bus, 256, |_| 256), 256);
    for _ in 0..256 {
        reference.cpu.step(&mut reference.bus).unwrap();
    }
    state(&native, &reference);
    // Replace INC with DEC while the old block is being compiled.
    for m in [&mut native, &mut reference] {
        m.bus.write_u8(0x10000, 0x48).unwrap();
        m.cpu.registers.ip = 0x10000;
    }
    assert_eq!(native.cpu.finish_dynamic_compilation(job.compile()), 1);
    assert_eq!(native.cpu.dynamic_batch(&mut native.bus, 256, |_| 256), 256);
    for _ in 0..256 {
        reference.cpu.step(&mut reference.bus).unwrap();
    }
    state(&native, &reference);
    assert_eq!(native.cpu.dynamic_stats().executed_steps, 0);
}
