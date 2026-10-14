use super::*;
use crate::PcMachine;
use doslet_x86::{Access, Gpr32, SegmentRegister, flags};

fn descriptor(bus: &mut PcBus, selector: u16, base: u32, limit: u32, code: bool, wide: bool) {
    let bytes = [
        limit as u8,
        (limit >> 8) as u8,
        base as u8,
        (base >> 8) as u8,
        (base >> 16) as u8,
        if code { 0x9B } else { 0x93 },
        ((limit >> 16) as u8 & 15) | if wide { 0x40 } else { 0 },
        (base >> 24) as u8,
    ];
    for (i, value) in bytes.into_iter().enumerate() {
        bus.write_u8(0x800 + u32::from(selector) + i as u32, value)
            .unwrap();
    }
}
fn machine(cached: bool) -> PcMachine {
    let mut m = PcMachine::default();
    m.bus.set_a20(true);
    descriptor(&mut m.bus, 8, 0, 0xFFFFF, true, true);
    descriptor(&mut m.bus, 16, 0, 0xFFFFF, false, true);
    m.cpu.control.cr0 |= 1;
    m.cpu.control.gdtr.base = 0x800;
    m.cpu.control.gdtr.limit = 0xFF;
    m.bus.write_u8(0, 0x90).unwrap();
    m.cpu.step(&mut m.bus).unwrap(); // Synchronize PE before loading descriptors.
    for segment in SegmentRegister::ALL {
        m.cpu
            .load_segment(
                &mut m.bus,
                segment,
                if segment == SegmentRegister::Cs {
                    8
                } else {
                    16
                },
            )
            .unwrap();
    }
    m.cpu.flags = 2;
    m.cpu.set_decode_cache(cached);
    m
}

fn code(m: &mut PcMachine, address: u32, bytes: &[u8]) {
    for (i, byte) in bytes.iter().enumerate() {
        m.bus.write_u8(address + i as u32, *byte).unwrap();
    }
    m.cpu.registers.ip = address;
}
fn state(a: &PcMachine, b: &PcMachine) {
    assert_eq!(a.cpu.registers, b.cpu.registers);
    assert_eq!(a.cpu.flags, b.cpu.flags);
    assert_eq!(a.cpu.control, b.cpu.control);
    assert_eq!(a.cpu.is_halted(), b.cpu.is_halted());
    assert_eq!(a.cpu.has_interrupt_shadow(), b.cpu.has_interrupt_shadow());
    assert_eq!(a.time(), b.time());
    assert_eq!(a.bus.pic.read_command(), b.bus.pic.read_command());
    assert_eq!(a.bus.dsp.irq_status(), b.bus.dsp.irq_status());
}
fn memory(a: &PcMachine, b: &PcMachine) {
    assert_eq!(
        a.bus.ram.view(0..crate::memory::RAM_SIZE),
        b.bus.ram.view(0..crate::memory::RAM_SIZE)
    );
    assert_eq!(
        a.bus.vga.frame().map(|f| f.pixels),
        b.bus.vga.frame().map(|f| f.pixels)
    );
}

#[test]
fn decoded_forms_match_reference_with_dynamic_registers_prefixes_and_faults() {
    let mut cached = machine(true);
    let mut reference = machine(false);
    let mut seed = 0x12345678_u32;
    let opcodes = [
        0, 1, 2, 3, 4, 5, 8, 9, 0x10, 0x11, 0x18, 0x19, 0x20, 0x21, 0x28, 0x29, 0x30, 0x31, 0x38,
        0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x40, 0x48, 0x50, 0x58, 0x68, 0x6A, 0x70, 0x74, 0x7F, 0x80,
        0x81, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x8B, 0x8D, 0x90, 0x97, 0xA0, 0xA1,
        0xA2, 0xA3, 0xB0, 0xB7, 0xB8, 0xBF, 0xC2, 0xC3, 0xC6, 0xC7, 0xE8, 0xE9, 0xEB, 0xEC, 0xEE,
        0xF7, 0x0F, 0xF3,
    ];
    let prefixes: &[&[u8]] = &[&[], &[0x67], &[0x64], &[0x66], &[0xF2], &[0x67, 0x3E]];
    for opcode in opcodes {
        for prefix in prefixes {
            for modrm in [0_u8, 4, 5, 0x44, 0x84, 0xC0, 0xDB, 0xE8, 0xF8] {
                let mut bytes = [0; 15];
                for byte in &mut bytes {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    *byte = seed as u8;
                }
                bytes[..prefix.len()].copy_from_slice(prefix);
                bytes[prefix.len()] = opcode;
                bytes[prefix.len() + 1] = modrm;
                code(&mut cached, 0x10000, &bytes);
                code(&mut reference, 0x10000, &bytes);
                for round in 0..2 {
                    for m in [&mut cached, &mut reference] {
                        m.cpu.registers.ip = 0x10000;
                        for (i, register) in [
                            Gpr32::Eax,
                            Gpr32::Ecx,
                            Gpr32::Edx,
                            Gpr32::Ebx,
                            Gpr32::Esp,
                            Gpr32::Ebp,
                            Gpr32::Esi,
                            Gpr32::Edi,
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            m.cpu
                                .registers
                                .set32(register, 0x20000 + i as u32 * 64 + round * 4);
                        }
                        m.cpu.flags = 2
                            | (seed & (flags::CARRY | flags::ZERO | flags::SIGN | flags::OVERFLOW));
                    }
                    let a = cached.step();
                    let b = reference.step();
                    assert_eq!(
                        a, b,
                        "opcode={opcode:#x}, modrm={modrm:#x}, prefix={prefix:?}, round={round}"
                    );
                    state(&cached, &reference);
                }
            }
        }
        memory(&cached, &reference);
    }
    #[cfg(feature = "profiling")]
    assert!(cached.cpu.decode_cache_stats().hits > 1000);
}

#[test]
fn code_writes_cross_page_wide_writes_aliases_and_bus_replacement_invalidate() {
    let mut m = machine(true);
    let address = 0x10FFD;
    code(&mut m, address, &[0xB8, 1, 0, 0, 0]);
    m.step().unwrap();
    m.cpu.registers.ip = address;
    m.step().unwrap();
    // Immediate spans two 4-KiB pages; mutate its second page.
    m.bus.write_u16(address + 3, 0xCAFE).unwrap();
    m.cpu.registers.ip = address;
    m.step().unwrap();
    assert_eq!(m.cpu.registers.get32(Gpr32::Eax), 0xCAFE0001);
    m.bus.write_u32(address + 1, 7).unwrap();
    m.cpu.registers.ip = address;
    m.step().unwrap();
    assert_eq!(m.cpu.registers.get32(Gpr32::Eax), 7);
    m.bus.set_a20(false);
    m.bus.write_u8(address | (1 << 20), 0xBB).unwrap();
    m.cpu.registers.ip = address;
    m.step().unwrap();
    assert_eq!(m.cpu.registers.get32(Gpr32::Ebx), 7);
    // Replacement ownership must reject entries even with matching generations.
    let mut replacement = machine(false);
    code(&mut replacement, address, &[0xB8, 42, 0, 0, 0]);
    m.cpu.registers.ip = address;
    m.cpu.step(&mut replacement.bus).unwrap();
    assert_eq!(m.cpu.registers.get32(Gpr32::Eax), 42);
    #[cfg(feature = "profiling")]
    {
        let stats = m.cpu.decode_cache_stats();
        assert!(stats.hits >= 1);
        assert_eq!(stats.write_invalidations, 2);
        assert_eq!(stats.mapping_invalidations, 1);
        assert_eq!(stats.owner_invalidations, 1);
        assert_eq!(stats.invalidations, 4);
    }
}

#[test]
fn cs_reload_code_limit_mmio_and_partial_ip_faults_keep_reference_behavior() {
    let mut cached = machine(true);
    let mut reference = machine(false);
    for bytes in [
        &b"\x81\x05\x00\x00\x00\x01\x01\x00\x00\x00"[..],
        &b"\xC7\x05\x00\x00\x00\x01\x01\x00\x00\x00"[..],
        &b"\xE8\x00\x00\x00\x40"[..],
        &b"\xE8\x00\x00\x00\x00"[..],
    ] {
        for m in [&mut cached, &mut reference] {
            code(m, 0x10000, bytes);
            m.cpu.registers.set32(Gpr32::Esp, 0);
        }
        assert_eq!(cached.step(), reference.step());
        state(&cached, &reference);
        memory(&cached, &reference);
    }
    for m in [&mut cached, &mut reference] {
        code(m, 0x10FFF, &[0xB8, 1, 0, 0, 0]);
        m.step().unwrap();
        descriptor(&mut m.bus, 8, 0, 0x10FFF, true, true);
        m.cpu
            .load_segment(&mut m.bus, SegmentRegister::Cs, 8)
            .unwrap();
        m.cpu.registers.ip = 0x10FFF;
    }
    let a = cached.step();
    let b = reference.step();
    assert_eq!(a, b);
    state(&cached, &reference);
    assert_eq!(cached.cpu.registers.ip, 0x11000);
    assert!(matches!(
        a,
        Err(crate::MachineError::Cpu(
            doslet_x86::CpuError::SegmentAccess {
                access: Access::Execute,
                ..
            }
        ))
    ));
    for m in [&mut cached, &mut reference] {
        descriptor(&mut m.bus, 8, 0, 0xFFFFF, true, true);
        m.cpu
            .load_segment(&mut m.bus, SegmentRegister::Cs, 8)
            .unwrap();
        m.bus.vga.set_mode_13h();
        code(m, APERTURE_START, &[0xB8, 9, 0, 0, 0]);
    }
    assert_eq!(cached.step(), reference.step());
    state(&cached, &reference);
    memory(&cached, &reference);
}

#[test]
fn cached_addressing_uses_reloaded_data_segments_and_changed_a20_mapping() {
    let mut m = machine(true);
    code(&mut m, 0x10000, &[0xA1, 0x10, 0, 0, 0]);
    m.bus.write_u32(0x10, 3).unwrap();
    m.bus.write_u32(0x40010, 7).unwrap();
    m.step().unwrap();
    descriptor(&mut m.bus, 16, 0x40000, 0xFFFFF, false, true);
    m.cpu
        .load_segment(&mut m.bus, SegmentRegister::Ds, 16)
        .unwrap();
    m.cpu.registers.ip = 0x10000;
    m.step().unwrap();
    assert_eq!(m.cpu.registers.get32(Gpr32::Eax), 7);
    // Identical CS:IP uses different physical code with the A20 line changed.
    code(&mut m, 0x110000, &[0xB8, 42, 0, 0, 0]);
    code(&mut m, 0x10000, &[0xB8, 99, 0, 0, 0]);
    descriptor(&mut m.bus, 8, 0x100000, 0xFFFFF, true, true);
    m.cpu
        .load_segment(&mut m.bus, SegmentRegister::Cs, 8)
        .unwrap();
    m.cpu.registers.ip = 0x10000;
    m.step().unwrap();
    assert_eq!(m.cpu.registers.get32(Gpr32::Eax), 42);
    m.bus.set_a20(false);
    m.cpu.registers.ip = 0x10000;
    m.step().unwrap();
    assert_eq!(m.cpu.registers.get32(Gpr32::Eax), 99);
    m.bus.set_a20(true);
    m.cpu.registers.ip = 0x10000;
    m.step().unwrap();
    assert_eq!(m.cpu.registers.get32(Gpr32::Eax), 42);
}

#[test]
fn an_instruction_crossing_the_a20_boundary_falls_back_without_losing_bytes() {
    let mut m = machine(true);
    code(&mut m, 0xFFFFD, &[0xB8, 1, 2, 3, 4]);
    descriptor(&mut m.bus, 8, 0xFFFFD, 0xFFFFF, true, true);
    m.cpu
        .load_segment(&mut m.bus, SegmentRegister::Cs, 8)
        .unwrap();
    m.cpu.registers.ip = 0;
    m.step().unwrap();
    assert_eq!(m.cpu.registers.get32(Gpr32::Eax), 0x04030201);
    m.bus.write_u16(0, 0xCAFE).unwrap();
    m.bus.set_a20(false);
    m.cpu.registers.ip = 0;
    m.step().unwrap();
    assert_eq!(m.cpu.registers.get32(Gpr32::Eax), 0xCAFE0201);
    assert_eq!(m.cpu.registers.ip, 5);
}

#[cfg(feature = "profiling")]
#[test]
fn profiling_distinguishes_context_discards_collisions_and_failed_probes() {
    let mut m = machine(true);
    for address in [0x10000, 0x10201] {
        code(&mut m, address, &[0x90]);
        m.step().unwrap();
    }
    let stats = m.cpu.decode_cache_stats();
    assert_eq!(stats.collisions, 1);
    assert_eq!(stats.evictions, 1);
    assert_eq!(stats.invalidations, 0);
    descriptor(&mut m.bus, 24, 0, 0xFFFFF, true, true);
    m.cpu
        .load_segment(&mut m.bus, SegmentRegister::Cs, 24)
        .unwrap();
    m.cpu.registers.ip = 0x10201;
    m.step().unwrap();
    let stats = m.cpu.decode_cache_stats();
    assert_eq!(stats.context_switches, 1);
    assert_eq!(stats.context_invalidations, 1);
    assert_eq!(stats.invalidations, 1);
    code(&mut m, 0x20000, &[0x66, 0xB8, 1, 0]);
    for _ in 0..2 {
        m.cpu.registers.ip = 0x20000;
        m.step().unwrap();
    }
    let stats = m.cpu.decode_cache_stats();
    assert_eq!(stats.failed_decodes, 2);
    assert_eq!(stats.fallback_hits, 0);
}

fn cs_fault(result: Result<doslet_x86::StepOutcome, crate::MachineError>) -> bool {
    matches!(
        result,
        Err(crate::MachineError::Cpu(
            doslet_x86::CpuError::SegmentAccess {
                segment: SegmentRegister::Cs,
                access: Access::Execute,
                ..
            }
        ))
    )
}

#[test]
fn host_cr0_writes_resynchronize_hidden_segments_in_both_directions() {
    let mut cached = machine(true);
    let mut reference = machine(false);
    for m in [&mut cached, &mut reference] {
        code(m, 0x10000, &[0xB8, 1, 0, 0, 0]);
        m.step().unwrap();
        // Leaving PE through the public field: the selector-8 cache no longer
        // matches CS, so real-mode CS 1000h executes the bytes as 16-bit code.
        m.cpu.control.cr0 &= !1;
        m.cpu.registers.cs = 0x1000;
        m.cpu.registers.ip = 0;
        m.cpu.registers.set32(Gpr32::Eax, 0);
    }
    assert_eq!(cached.step(), reference.step());
    state(&cached, &reference);
    assert_eq!(cached.cpu.registers.ip, 3);
    assert_eq!(cached.cpu.registers.get32(Gpr32::Eax), 1);
    for m in [&mut cached, &mut reference] {
        // Entering PE through the field materializes CS 1000h's real-mode cache.
        m.cpu.control.cr0 |= 1;
        m.cpu.registers.ip = 0;
    }
    assert_eq!(cached.step(), reference.step());
    state(&cached, &reference);
    assert_eq!(cached.cpu.registers.ip, 3);
    for m in [&mut cached, &mut reference] {
        // The materialized cache belongs to selector 1000h, not a later write.
        m.cpu.registers.cs = 0x2000;
        m.cpu.registers.ip = 0;
    }
    let result = cached.step();
    assert_eq!(result, reference.step());
    state(&cached, &reference);
    assert!(cs_fault(result));
}

#[test]
fn mov_cr0_transitions_in_both_directions_match_reference_on_repeated_passes() {
    let mut cached = machine(true);
    let mut reference = machine(false);
    // 10100h: leave PE; JMP FAR 1000:00000200 (linear 10200h).
    let leave = [
        0x0F, 0x20, 0xC0, 0x24, 0xFE, 0x0F, 0x22, 0xC0, 0xEA, 0x00, 0x02, 0, 0, 0x00, 0x10,
    ];
    // 10200h: enter PE; JMP FAR 0008:00010300 to flat 32-bit code.
    let enter = [
        0x0F, 0x20, 0xC0, 0x0C, 0x01, 0x0F, 0x22, 0xC0, 0x66, 0xEA, 0x00, 0x03, 0x01, 0, 0x08, 0,
    ];
    // 10300h: MOV EAX,1; JMP 10100h.
    let body = [0xB8, 1, 0, 0, 0, 0xE9, 0xF6, 0xFD, 0xFF, 0xFF];
    for m in [&mut cached, &mut reference] {
        code(m, 0x10200, &enter);
        code(m, 0x10300, &body);
        code(m, 0x10100, &leave);
    }
    for pass in 0..3 {
        for step in 0..10 {
            let result = cached.step();
            assert_eq!(result, reference.step(), "pass {pass}, step {step}");
            result.unwrap();
            state(&cached, &reference);
            if step == 3 {
                assert_eq!(cached.cpu.registers.cs, 0x1000);
                assert!(!cached.cpu.registers.code32());
            }
        }
        assert_eq!(cached.cpu.registers.ip, 0x10100);
        assert!(cached.cpu.registers.code32());
        assert_eq!(cached.cpu.registers.get32(Gpr32::Eax) & 1, 1);
    }
    #[cfg(feature = "profiling")]
    {
        // PE changes switch CS contexts; decoded forms still use the cache.
        let stats = cached.cpu.decode_cache_stats();
        assert!(stats.context_switches >= 5);
        assert!(stats.misses > stats.failed_decodes);
    }
}

#[test]
fn direct_cs_writes_and_wrapping_instruction_ends_keep_reference_behavior() {
    let mut cached = machine(true);
    let mut reference = machine(false);
    for m in [&mut cached, &mut reference] {
        code(m, 0x10000, &[0xB8, 1, 0, 0, 0]);
        m.step().unwrap();
        // The public selector no longer matches the hidden CS cache.
        m.cpu.registers.cs = 16;
        m.cpu.registers.ip = 0x10000;
    }
    let result = cached.step();
    assert_eq!(result, reference.step());
    state(&cached, &reference);
    assert!(cs_fault(result));
    for m in [&mut cached, &mut reference] {
        m.cpu.registers.cs = 8;
        m.cpu.registers.ip = 0x10000;
        m.cpu.registers.set32(Gpr32::Eax, 0);
    }
    assert_eq!(cached.step(), reference.step());
    state(&cached, &reference);
    assert_eq!(cached.cpu.registers.get32(Gpr32::Eax), 1);
    for m in [&mut cached, &mut reference] {
        // A 4-GiB code segment based at 10002h maps EIP FFFFFFFEh to 10000h.
        // The cached MOV EAX,imm32 overflows IP + length - 1, so it falls
        // back to byte fetching; the window/address unit test pins the check.
        let descriptor = [0xFF, 0xFF, 0x02, 0x00, 0x01, 0x9B, 0xCF, 0x00];
        for (i, value) in descriptor.into_iter().enumerate() {
            m.bus.write_u8(0x800 + 24 + i as u32, value).unwrap();
        }
        m.cpu
            .load_segment(&mut m.bus, SegmentRegister::Cs, 24)
            .unwrap();
        m.cpu.registers.ip = 0xFFFF_FFFE;
    }
    assert_eq!(cached.step(), reference.step());
    state(&cached, &reference);
    memory(&cached, &reference);
}
