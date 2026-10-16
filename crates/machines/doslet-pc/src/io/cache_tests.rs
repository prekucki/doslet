use super::*;
use crate::PcMachine;
use alloc::vec::Vec;
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

#[cfg(all(
    feature = "dynamic-core",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[path = "dynamic_tests.rs"]
mod dynamic_tests;

#[test]
fn decoded_forms_match_reference_with_dynamic_registers_prefixes_and_faults() {
    for mode in 0..3 {
        let mut cached = machine(true);
        let mut reference = machine(false);
        for m in [&mut cached, &mut reference] {
            if mode == 1 {
                descriptor(&mut m.bus, 8, 0, 0xFFFFF, true, false);
                m.cpu
                    .load_segment(&mut m.bus, SegmentRegister::Cs, 8)
                    .unwrap();
            }
            if mode == 2 {
                m.cpu.control.cr0 &= !1;
                code(m, 0x1000, &[0x90]);
                m.cpu.step(&mut m.bus).unwrap();
                for segment in SegmentRegister::ALL {
                    m.cpu.load_segment(&mut m.bus, segment, 0).unwrap();
                }
            }
        }
        let mut seed = 0x12345678_u32;
        let opcodes = [
            0, 1, 2, 3, 4, 5, 8, 9, 0x10, 0x11, 0x18, 0x19, 0x20, 0x21, 0x28, 0x29, 0x30, 0x31,
            0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x40, 0x48, 0x50, 0x58, 0x68, 0x6A, 0x70, 0x74,
            0x7F, 0x80, 0x81, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8A, 0x8B, 0x8D, 0x90,
            0x97, 0xA0, 0xA1, 0xA2, 0xA3, 0xB0, 0xB7, 0xB8, 0xBF, 0xC2, 0xC3, 0xC6, 0xC7, 0xE8,
            0xE9, 0xEB, 0xEC, 0xEE, 0xF7, 0x0F, 0xF3, 0xC0, 0xC1, 0xD0, 0xD1, 0xD2, 0xD3, 0xFE,
            0xFF,
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
                    code(&mut cached, 0x1000, &bytes);
                    code(&mut reference, 0x1000, &bytes);
                    for round in 0..2 {
                        for m in [&mut cached, &mut reference] {
                            m.cpu.registers.ip = 0x1000;
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
                                | (seed
                                    & (flags::CARRY | flags::ZERO | flags::SIGN | flags::OVERFLOW));
                        }
                        let a = cached.step();
                        let b = reference.step();
                        assert_eq!(
                            a, b,
                            "opcode={opcode:#x}, modrm={modrm:#x}, prefix={prefix:?}, round={round}"
                        );
                        assert!(
                            cached.cpu.snapshot() == reference.cpu.snapshot(),
                            "mode={mode} opcode={opcode:x} modrm={modrm:x} prefix={prefix:?}"
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
    for address in [0x10000, 0x11008] {
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
    m.cpu.registers.ip = 0x11008;
    m.step().unwrap();
    let stats = m.cpu.decode_cache_stats();
    assert_eq!(stats.context_switches, 1);
    assert_eq!(stats.context_invalidations, 0);
    assert_eq!(stats.invalidations, 0);
    code(&mut m, 0x20000, &[0x66, 0xB8, 1, 0]);
    for _ in 0..2 {
        m.cpu.registers.ip = 0x20000;
        m.step().unwrap();
    }
    let stats = m.cpu.decode_cache_stats();
    assert_eq!(stats.failed_decodes, 0);
    assert_eq!(stats.fallback_hits, 0);
    code(&mut m, 0x20010, &[0xFA]); // CLI deliberately stays outside the cache.
    for _ in 0..2 {
        m.cpu.registers.ip = 0x20010;
        m.step().unwrap();
    }
    let stats = m.cpu.decode_cache_stats();
    assert_eq!(stats.failed_decodes, 1);
    assert_eq!(stats.fallback_hits, 1);
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

type Stamp = (doslet_bus::CodeOwnerId, u64, [u64; 2]);
/// The former per-hit stamp, recomputed from the current bus state.
fn stamp(bus: &PcBus, address: u32, length: u8) -> Option<Stamp> {
    let range = bus.wide_ram_range(address, u32::from(length))?;
    bus.ram.stamp_reference(range, bus.a20.generation)
}

#[test]
fn code_checks_match_the_former_stamp_under_writes_a20_and_replacement() {
    let mut bus = PcBus::default();
    let spans: [(u32, u8); 8] = [
        (0x0FFE, 4),    // crosses pages 0 and 1
        (0x10FFC, 15),  // crosses a page boundary
        (0x0FFF0, 15),  // physical alias of the next span with A20 disabled
        (0x10FFF0, 15), // above 1 MiB
        (0x0FFFFD, 5),  // crosses the A20 boundary
        (0x9FFFE, 4),   // touches the VGA aperture: never checked
        (0xFFFFF8, 8),  // last RAM bytes
        (0x20000, 1),
    ];
    let mut checks = [None; 8];
    let mut decoded = [None; 8];
    let (mut valid, mut stale) = (0, 0);
    let mut seed = 0x2468_ACE1_u32;
    for round in 0..4000 {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        let (base, _) = spans[seed as usize % spans.len()];
        let target = base.wrapping_add((seed >> 8) % 24).wrapping_sub(4);
        // Device-memory writes may be rejected; only RAM effects matter here.
        match (seed >> 16) % 8 {
            0 => drop(bus.write_u8(target, seed as u8)),
            1 => drop(bus.write_u16(target, seed as u16)),
            2 => drop(bus.write_u32(target, seed)),
            3 => {
                let start = target as usize;
                if let Some(view) = bus.ram.view_mut(start..start + 3) {
                    view.fill(seed as u8);
                }
            }
            4 => bus.set_a20(!bus.a20_enabled()),
            _ => {
                let span = (seed >> 20) as usize % spans.len();
                let (address, length) = spans[span];
                checks[span] = bus.code_check(address, length);
                decoded[span] = stamp(&bus, address, length);
                assert_eq!(checks[span].is_some(), decoded[span].is_some());
            }
        }
        for (span, &(address, length)) in spans.iter().enumerate() {
            assert_eq!(
                bus.code_check(address, length).is_some(),
                stamp(&bus, address, length).is_some()
            );
            if let Some(check) = &checks[span] {
                let expected = stamp(&bus, address, length) == decoded[span];
                assert_eq!(
                    bus.code_valid(check),
                    expected,
                    "round {round}, span {span}"
                );
                valid += u32::from(expected);
                stale += u32::from(!expected);
            }
        }
    }
    assert!(valid > 1000 && stale > 1000, "{valid}/{stale}");
    let replacement = PcBus::default();
    for check in checks.iter().flatten() {
        assert!(!replacement.code_valid(check));
    }
}

#[test]
fn buses_with_identical_histories_reject_each_others_checks() {
    let mut a = PcBus::default();
    let mut b = PcBus::default();
    for bus in [&mut a, &mut b] {
        bus.set_a20(true);
        bus.write_u32(0x10FFE, 0x1234_5678).unwrap();
    }
    let check = a.code_check(0x10FFE, 4).unwrap();
    let other = b.code_check(0x10FFE, 4).unwrap();
    // Only the owner identity distinguishes the two spans.
    assert_eq!(
        (check.pages, check.generations, check.space & 0xFFFF_FFFF),
        (other.pages, other.generations, other.space & 0xFFFF_FFFF)
    );
    assert!(a.code_valid(&check) && b.code_valid(&other));
    assert!(!b.code_valid(&check) && !a.code_valid(&other));
}

#[test]
fn registers_moved_from_another_machine_resolve_their_own_cs_context() {
    let mut a = machine(true);
    let mut b = machine(true);
    for m in [&mut a, &mut b] {
        code(m, 0x10000, &[0xB8, 1, 0, 0, 0]);
        code(m, 0x11000, &[0xB8, 2, 0, 0, 0]);
    }
    // Same selector, PE mode and CS version count; only B's descriptor base differs.
    descriptor(&mut b.bus, 8, 0x1000, 0xFFFFF, true, true);
    for m in [&mut a, &mut b] {
        m.cpu
            .load_segment(&mut m.bus, SegmentRegister::Cs, 8)
            .unwrap();
        m.cpu.registers.ip = 0x10000;
    }
    a.step().unwrap();
    assert_eq!(a.cpu.registers.get32(Gpr32::Eax), 1);
    core::mem::swap(&mut a.cpu.registers, &mut b.cpu.registers);
    a.cpu.registers.ip = 0x10000;
    a.step().unwrap();
    assert_eq!(a.cpu.registers.get32(Gpr32::Eax), 2);
}

#[test]
fn word_byte_shifts_and_double_shifts_match_reference_for_all_counts() {
    let mut cached = machine(true);
    let mut reference = machine(false);
    for word in [false, true] {
        for opcode in [0xC0, 0xC1, 0xD2, 0xD3, 0xA4, 0xA5, 0xAC, 0xAD] {
            for group in 0..8 {
                for count in 0..=33 {
                    for carry in [0, flags::CARRY] {
                        let mut bytes = Vec::new();
                        if word {
                            bytes.push(0x66);
                        }
                        if opcode < 0xC0 {
                            bytes.push(0x0F);
                        }
                        bytes.extend_from_slice(&[opcode, 0xC0 | (group << 3), count]);
                        for m in [&mut cached, &mut reference] {
                            code(m, 0x1000, &bytes);
                            m.cpu.registers.set32(Gpr32::Eax, 0x81FE_8041);
                            m.cpu
                                .registers
                                .set32(Gpr32::Ecx, 0xA55A_0000 | u32::from(count));
                            m.cpu.flags = 2 | flags::OVERFLOW | flags::AUXILIARY | carry;
                        }
                        let a = cached.cpu.step(&mut cached.bus);
                        let b = reference.cpu.step(&mut reference.bus);
                        assert_eq!(
                            a, b,
                            "word={word} opcode={opcode:x} group={group} count={count}"
                        );
                        assert!(
                            cached.cpu.snapshot() == reference.cpu.snapshot(),
                            "word={word} opcode={opcode:x} group={group} count={count} carry={carry}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn cached_batches_cover_real_and_protected_word_code_and_size_overrides() {
    for mode in 0..3 {
        for override_operand in [false, true] {
            let mut cached = machine(true);
            let mut reference = machine(false);
            for m in [&mut cached, &mut reference] {
                if mode == 1 {
                    descriptor(&mut m.bus, 8, 0, 0xFFFF, true, false);
                    m.cpu
                        .load_segment(&mut m.bus, SegmentRegister::Cs, 8)
                        .unwrap();
                } else if mode == 2 {
                    m.cpu.control.cr0 &= !1;
                    code(m, 0x1000, &[0x90]);
                    m.cpu.step(&mut m.bus).unwrap();
                    for segment in SegmentRegister::ALL {
                        m.cpu.load_segment(&mut m.bus, segment, 0).unwrap();
                    }
                }
                code(m, 0x1000, &[0x90, 0x90]);
                m.cpu.step(&mut m.bus).unwrap();
                m.cpu.step(&mut m.bus).unwrap();
                m.cpu.registers.set32(Gpr32::Eax, 0x1234_5678);
                m.cpu.registers.set32(Gpr32::Ebx, 0x8000_0001);
                m.cpu.registers.set32(Gpr32::Esp, 0x5000);
                m.cpu.registers.set32(Gpr32::Esi, 0x1000);
                m.cpu.flags = 0x803;
            }
            let mut bytes = Vec::new();
            for instruction in [
                &[0x50][..],
                &[0x5A],
                &[0x40],
                &[0x49],
                &[0xC1, 0xE3, 3],
                &[0xD1, 0xD8],
                &[0x0F, 0xAC, 0xD0, 5],
                &[0x83, 0xC0, 3],
                &[0xFE, 0xCC],
            ] {
                if override_operand {
                    bytes.push(0x66);
                }
                bytes.extend_from_slice(instruction);
            }
            // INC/DEC memory and a memory shift use the code's address width.
            for opcode in [0xFF, 0xFE, 0xD1] {
                if override_operand {
                    bytes.push(0x66);
                }
                bytes.push(opcode);
                bytes.push(
                    if mode == 0 { 0x05 } else { 0x06 } | if opcode == 0xD1 { 0x20 } else { 0 },
                );
                bytes.extend_from_slice(if mode == 0 {
                    &[0, 0x20, 0, 0][..]
                } else {
                    &[0, 0x20][..]
                });
            }
            if override_operand {
                bytes.push(0x66);
            }
            bytes.extend_from_slice(&[0xFF, 0xE6]); // JMP SI/ESI back to the loop.
            for m in [&mut cached, &mut reference] {
                code(m, 0x1000, &bytes);
            }
            for budget in [2, 3, 7, 17, 255, 256] {
                let count = if cfg!(all(
                    feature = "dynamic-core",
                    any(target_arch = "x86_64", target_arch = "aarch64")
                )) && budget % 2 == 0
                {
                    cached
                        .cpu
                        .dynamic_batch(&mut cached.bus, budget, |_| budget)
                } else {
                    cached
                        .cpu
                        .interpret_batch(&mut cached.bus, budget, |_| budget)
                };
                assert_eq!(
                    count, budget,
                    "mode={mode}, override={override_operand}, ip={:x}",
                    cached.cpu.registers.ip
                );
                for _ in 0..count {
                    reference.cpu.step(&mut reference.bus).unwrap();
                }
                state(&cached, &reference);
                memory(&cached, &reference);
            }
        }
    }
}

#[test]
fn mode_switch_at_same_linear_address_and_real_ip_wrap_do_not_reuse_wrong_decode() {
    let mut cached = machine(true);
    let mut reference = machine(false);
    for m in [&mut cached, &mut reference] {
        code(m, 0x1000, &[0xB8, 0x34, 0x12, 0x78, 0x56]);
    }
    for wide in [true, false, true, false] {
        for m in [&mut cached, &mut reference] {
            descriptor(&mut m.bus, 8, 0, 0xFFFFF, true, wide);
            m.cpu
                .load_segment(&mut m.bus, SegmentRegister::Cs, 8)
                .unwrap();
            m.cpu.registers.ip = 0x1000;
        }
        cached.cpu.step(&mut cached.bus).unwrap();
        reference.cpu.step(&mut reference.bus).unwrap();
        state(&cached, &reference);
        assert_eq!(cached.cpu.registers.ip, if wide { 0x1005 } else { 0x1003 });
    }
    for m in [&mut cached, &mut reference] {
        m.cpu.control.cr0 &= !1;
        code(m, 0x1000, &[0x90]);
        m.cpu.step(&mut m.bus).unwrap();
        m.cpu.registers.cs = 0;
        m.bus.write_u16(0xFFFF, 0x34B8).unwrap();
        m.bus.write_u16(0, 0x1234).unwrap();
        m.cpu.registers.ip = 0xFFFF;
    }
    cached.cpu.step(&mut cached.bus).unwrap();
    reference.cpu.step(&mut reference.bus).unwrap();
    state(&cached, &reference);
    assert_eq!(cached.cpu.registers.ip, 2);
}

/// Every prepared binary form runs through a real batch, cached single step and
/// byte-fetching reference. SIB/16-bit addressing and high byte aliases vary at
/// execution time; a terminal CLI makes the completed-step count unambiguous.
#[test]
fn prepared_binary_forms_match_both_references() {
    let mut batch = machine(true);
    let mut cached = machine(true);
    let mut reference = machine(false);
    for m in [&mut batch, &mut cached, &mut reference] {
        code(m, 0x1000, &[0x90, 0x90]);
        m.cpu.step(&mut m.bus).unwrap();
        m.cpu.step(&mut m.bus).unwrap();
    }
    for width in [1, 2, 4] {
        for address32 in [false, true] {
            for operation in 0..10 {
                for form in 0..5 {
                    // x86 has no TEST reg,mem direction bit; its mem,reg form
                    // exercises the same commutative operation and flags.
                    if operation == 9 && form == 2 {
                        continue;
                    }
                    let mut bytes = Vec::new();
                    if width == 2 {
                        bytes.push(0x66);
                    }
                    if !address32 {
                        bytes.push(0x67);
                    }
                    let wide = u8::from(width != 1);
                    let immediate = form == 1 || form == 4;
                    let memory = form >= 2;
                    let opcode = if operation == 0 {
                        if immediate {
                            0xC6 + wide
                        } else {
                            0x88 + wide + if form == 2 { 2 } else { 0 }
                        }
                    } else if operation == 9 {
                        if immediate { 0xF6 + wide } else { 0x84 + wide }
                    } else if immediate {
                        0x80 + wide
                    } else {
                        (operation - 1) * 8 + wide + if form == 2 { 2 } else { 0 }
                    };
                    bytes.push(opcode);
                    let group = if immediate && (1..9).contains(&operation) {
                        operation - 1
                    } else {
                        0
                    };
                    if memory {
                        bytes.push((group << 3) | if address32 { 0x44 } else { 0x40 });
                        if address32 {
                            bytes.extend_from_slice(&[0x73, 0xD0]);
                        } else {
                            bytes.push(0xF0);
                        }
                    } else {
                        bytes.push(0xC4 | (group << 3));
                    } // AH or SP/ESP target
                    if immediate {
                        bytes.extend_from_slice(&0x81FE_80FF_u32.to_le_bytes()[..width as usize]);
                    }
                    bytes.push(0xFA);
                    for carry in [0, flags::CARRY] {
                        for m in [&mut batch, &mut cached, &mut reference] {
                            code(m, 0x1000, &bytes);
                            for (reg, value) in [
                                (Gpr32::Eax, 0x807F_80FF),
                                (Gpr32::Ebx, 0x3FF0),
                                (Gpr32::Esi, 0x20),
                                (Gpr32::Esp, 0x5000),
                            ] {
                                m.cpu.registers.set32(reg, value);
                            }
                            m.cpu.flags = 2 | flags::OVERFLOW | flags::AUXILIARY | carry;
                            m.bus.write_u32(0x4000, 0xFE01_7F80).unwrap();
                        }
                        let count = batch.cpu.interpret_batch(&mut batch.bus, 2, |_| 2);
                        assert_eq!(
                            count, 1,
                            "width={width} address32={address32} operation={operation} form={form} bytes={bytes:x?}"
                        );
                        cached.cpu.step(&mut cached.bus).unwrap();
                        reference.cpu.step(&mut reference.bus).unwrap();
                        state(&batch, &reference);
                        state(&batch, &cached);
                        assert_eq!(
                            batch.bus.ram.view(0x4000..0x4004),
                            reference.bus.ram.view(0x4000..0x4004)
                        );
                        assert_eq!(
                            batch.bus.ram.view(0x4000..0x4004),
                            cached.bus.ram.view(0x4000..0x4004)
                        );
                    }
                }
            }
        }
    }
    memory(&batch, &reference);
    memory(&batch, &cached);
}

#[test]
fn prepared_shifts_validate_before_effects_and_match_all_counts() {
    let mut batch = machine(true);
    let mut cached = machine(true);
    let mut reference = machine(false);
    for m in [&mut batch, &mut cached, &mut reference] {
        code(m, 0x1000, &[0x90, 0x90]);
        m.cpu.step(&mut m.bus).unwrap();
        m.cpu.step(&mut m.bus).unwrap();
    }
    for width in [1, 2, 4] {
        for address32 in [false, true] {
            for operation in 0..8 {
                for mem in [false, true] {
                    for cl in [false, true] {
                        for count in 0..=33_u8 {
                            let mut bytes = Vec::new();
                            if width == 2 {
                                bytes.push(0x66);
                            }
                            if !address32 {
                                bytes.push(0x67);
                            }
                            bytes.push(if cl { 0xD2 } else { 0xC0 } + u8::from(width != 1));
                            bytes.push(
                                (operation << 3)
                                    | if mem {
                                        if address32 { 5 } else { 6 }
                                    } else {
                                        0xC4
                                    },
                            );
                            if mem {
                                bytes.extend_from_slice(
                                    &0x4000_u32.to_le_bytes()[..if address32 { 4 } else { 2 }],
                                );
                            }
                            if !cl {
                                bytes.push(count);
                            }
                            bytes.push(0xFA);
                            for m in [&mut batch, &mut cached, &mut reference] {
                                code(m, 0x1000, &bytes);
                                m.cpu.registers.set32(Gpr32::Eax, 0x81FE_8041);
                                m.cpu.registers.set32(Gpr32::Esp, 0x81FE_8041);
                                m.cpu.registers.set32(Gpr32::Ecx, u32::from(count));
                                m.cpu.flags = 2 | flags::CARRY | flags::OVERFLOW | flags::AUXILIARY;
                                m.bus.write_u32(0x4000, 0x81FE_8041).unwrap();
                            }
                            assert_eq!(batch.cpu.interpret_batch(&mut batch.bus, 2, |_| 2), 1);
                            cached.cpu.step(&mut cached.bus).unwrap();
                            reference.cpu.step(&mut reference.bus).unwrap();
                            state(&batch, &reference);
                            state(&batch, &cached);
                            assert_eq!(
                                batch.bus.ram.view(0x4000..0x4004),
                                reference.bus.ram.view(0x4000..0x4004)
                            );
                            assert_eq!(
                                batch.bus.ram.view(0x4000..0x4004),
                                cached.bus.ram.view(0x4000..0x4004)
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn prepared_control_stack_lea_inc_and_extensions_match_both_references() {
    let mut batch = machine(true);
    let mut cached = machine(true);
    let mut reference = machine(false);
    for m in [&mut batch, &mut cached, &mut reference] {
        code(m, 0x1000, &[0x90, 0x90]);
        m.cpu.step(&mut m.bus).unwrap();
        m.cpu.step(&mut m.bus).unwrap();
    }
    for word in [false, true] {
        for address32 in [false, true] {
            let mut cases: Vec<Vec<u8>> = Vec::new();
            for register in 0..8 {
                for opcode in [0x40, 0x48, 0x50, 0x58] {
                    cases.push(alloc::vec![opcode + register]);
                }
            }
            for decrement in [false, true] {
                for byte in [false, true] {
                    let mut bytes = alloc::vec![
                        if byte { 0xFE } else { 0xFF },
                        if address32 { 5 } else { 6 } | (u8::from(decrement) << 3)
                    ];
                    bytes.extend_from_slice(
                        &0x4000_u32.to_le_bytes()[..if address32 { 4 } else { 2 }],
                    );
                    cases.push(bytes);
                }
            }
            let mut lea = alloc::vec![0x8D, if address32 { 0x44 } else { 0x40 }];
            if address32 {
                lea.extend_from_slice(&[0x73, 0xD0]);
            } else {
                lea.push(0xF0);
            }
            cases.push(lea);
            for opcode in [0xB6, 0xB7, 0xBE, 0xBF] {
                cases.push(alloc::vec![0x0F, opcode, 0xC4]); // AH or SP source
                let mut bytes = alloc::vec![0x0F, opcode, if address32 { 5 } else { 6 }];
                bytes.extend_from_slice(&0x4000_u32.to_le_bytes()[..if address32 { 4 } else { 2 }]);
                cases.push(bytes);
            }
            for opcode in [0xE8, 0xE9] {
                let mut bytes = alloc::vec![opcode];
                bytes.extend_from_slice(&0x20_u32.to_le_bytes()[..if word { 2 } else { 4 }]);
                cases.push(bytes);
            }
            cases.push(alloc::vec![0xEB, 0x20]);
            cases.push(alloc::vec![0xC3]);
            cases.push(alloc::vec![0xC2, 0x10, 0]);
            for cc in 0..16 {
                cases.push(alloc::vec![0x70 + cc, 0x20]);
                let mut bytes = alloc::vec![0x0F, 0x80 + cc];
                bytes.extend_from_slice(&0x20_u32.to_le_bytes()[..if word { 2 } else { 4 }]);
                cases.push(bytes);
            }
            for instruction in cases {
                // The decoder intentionally leaves MOVZX/SX with a word
                // destination to the reference interpreter.
                if word && instruction[0] == 0x0F && instruction[1] >= 0xB6 {
                    continue;
                }
                let mut bytes = Vec::new();
                if word {
                    bytes.push(0x66);
                }
                if !address32 {
                    bytes.push(0x67);
                }
                bytes.extend_from_slice(&instruction);
                bytes.resize(0x40, 0xFA); // Both branch successors stop at CLI.
                for initial_flags in [
                    2,
                    2 | flags::CARRY | flags::ZERO | flags::SIGN | flags::PARITY | flags::OVERFLOW,
                ] {
                    for m in [&mut batch, &mut cached, &mut reference] {
                        code(m, 0x1000, &bytes);
                        m.cpu.registers.set32(Gpr32::Eax, 0xFF80_7FFF);
                        m.cpu.registers.set32(Gpr32::Ebx, 0x3FF0);
                        m.cpu.registers.set32(Gpr32::Esi, 0x20);
                        m.cpu.registers.set32(Gpr32::Esp, 0x5000);
                        m.cpu.flags = initial_flags;
                        m.bus.write_u32(0x4000, 0x81FE_80FF).unwrap();
                        m.bus.write_u32(0x5000, 0x1020).unwrap();
                    }
                    assert_eq!(
                        batch.cpu.interpret_batch(&mut batch.bus, 2, |_| 2),
                        1,
                        "word={word} bytes={bytes:x?}"
                    );
                    cached.cpu.step(&mut cached.bus).unwrap();
                    reference.cpu.step(&mut reference.bus).unwrap();
                    state(&batch, &reference);
                    state(&batch, &cached);
                    for range in [0x4000..0x4004, 0x4FFC..0x5004] {
                        assert_eq!(
                            batch.bus.ram.view(range.clone()),
                            reference.bus.ram.view(range.clone())
                        );
                        assert_eq!(
                            batch.bus.ram.view(range.clone()),
                            cached.bus.ram.view(range)
                        );
                    }
                }
            }
        }
    }
    memory(&batch, &reference);
    memory(&batch, &cached);
}

#[test]
fn prepared_guards_have_no_effect_and_self_modification_redecodes() {
    let mut batch = machine(true);
    let mut reference = machine(false);
    for m in [&mut batch, &mut reference] {
        code(m, 0x1000, &[0x90, 0x90]);
        m.cpu.step(&mut m.bus).unwrap();
        m.cpu.step(&mut m.bus).unwrap();
    }
    // Both ends of VGA, an unmapped MMIO address, and a DS-limit crossing.
    for address in [0x9FFFF_u32, 0xA0000, 0xAFFFF, 0x1000000, 0xFFFFF] {
        for prefix in [
            &[0x8B, 0x05][..],
            &[0x89, 0x05],
            &[0x81, 0x05],
            &[0xFF, 0x05],
            &[0xC1, 0x25],
            &[0x0F, 0xBF, 0x05],
        ] {
            let mut bytes = prefix.to_vec();
            bytes.extend_from_slice(&address.to_le_bytes());
            bytes.extend_from_slice(&[1, 0, 0, 0, 0xFA]);
            for m in [&mut batch, &mut reference] {
                code(m, 0x1000, &bytes);
                m.cpu.registers.set32(Gpr32::Eax, 0x1234_5678);
                m.cpu.flags = 0x803;
            }
            let before = batch.cpu.snapshot();
            assert_eq!(batch.cpu.interpret_batch(&mut batch.bus, 2, |_| 2), 0);
            assert!(batch.cpu.snapshot() == before);
            memory(&batch, &reference);
            assert_eq!(
                batch.cpu.step(&mut batch.bus),
                reference.cpu.step(&mut reference.bus)
            );
            state(&batch, &reference);
            memory(&batch, &reference);
        }
    }
    // An invalid CALL target must not push a return address or change SP.
    for m in [&mut batch, &mut reference] {
        code(m, 0x1000, &[0xE8, 0, 0, 0x10, 0]);
        m.cpu.registers.set32(Gpr32::Esp, 0x5000);
    }
    let before = batch.cpu.snapshot();
    assert_eq!(batch.cpu.interpret_batch(&mut batch.bus, 2, |_| 2), 0);
    assert!(batch.cpu.snapshot() == before);
    memory(&batch, &reference);
    // Rewrite the next decoded instruction on the active page.
    for m in [&mut batch, &mut reference] {
        code(
            m,
            0x1000,
            &[0xC6, 0x05, 7, 0x10, 0, 0, 0x48, 0x40, 0x41, 0xFA],
        );
        m.cpu.registers.set32(Gpr32::Eax, 10);
        m.cpu.registers.set32(Gpr32::Ecx, 20);
    }
    assert_eq!(batch.cpu.interpret_batch(&mut batch.bus, 256, |_| 256), 3);
    for _ in 0..3 {
        reference.cpu.step(&mut reference.bus).unwrap();
    }
    assert_eq!(batch.cpu.registers.get32(Gpr32::Eax), 9);
    state(&batch, &reference);
    memory(&batch, &reference);
}

#[test]
fn prepared_effective_addresses_branch_targets_and_stacks_wrap_at_16_bits() {
    let mut batch = machine(true);
    let mut reference = machine(false);
    for m in [&mut batch, &mut reference] {
        code(m, 0x1000, &[0x90, 0x90]);
        m.cpu.step(&mut m.bus).unwrap();
        m.cpu.step(&mut m.bus).unwrap();
        // BX + SI + disp8 wraps to 4000h and ignores both high halves.
        m.cpu.registers.set32(Gpr32::Ebx, 0xABCD_FFF0);
        m.cpu.registers.set32(Gpr32::Esi, 0x9876_4010);
        m.bus.write_u32(0x4000, 0x1234_5678).unwrap();
        code(m, 0x1000, &[0x67, 0x8B, 0x40, 0, 0xFA]);
    }
    assert_eq!(batch.cpu.interpret_batch(&mut batch.bus, 2, |_| 2), 1);
    reference.cpu.step(&mut reference.bus).unwrap();
    state(&batch, &reference);
    assert_eq!(batch.cpu.registers.get32(Gpr32::Eax), 0x1234_5678);
    for m in [&mut batch, &mut reference] {
        descriptor(&mut m.bus, 8, 0, 0xFFFF, true, false);
        descriptor(&mut m.bus, 16, 0, 0xFFFF, false, false);
        m.cpu
            .load_segment(&mut m.bus, SegmentRegister::Cs, 8)
            .unwrap();
        m.cpu
            .load_segment(&mut m.bus, SegmentRegister::Ss, 16)
            .unwrap();
        code(m, 0x1000, &[0x90, 0x90]);
        m.cpu.step(&mut m.bus).unwrap();
        m.cpu.step(&mut m.bus).unwrap();
        code(m, 0x12, &[0xFA]);
        code(m, 0xFFF0, &[0xEB, 0x20]);
    }
    assert_eq!(batch.cpu.interpret_batch(&mut batch.bus, 2, |_| 2), 1);
    reference.cpu.step(&mut reference.bus).unwrap();
    state(&batch, &reference);
    assert_eq!(batch.cpu.registers.ip, 0x12);
    for sp in [0xABCD_0000, 0xABCD_0001] {
        for m in [&mut batch, &mut reference] {
            code(m, 0x1000, &[0x54, 0xFA]); // PUSH SP, whose old value must be read.
            m.cpu.registers.set32(Gpr32::Esp, sp);
        }
        let before = batch.cpu.snapshot();
        let count = batch.cpu.interpret_batch(&mut batch.bus, 2, |_| 2);
        if sp & 1 == 0 {
            assert_eq!(count, 1);
            reference.cpu.step(&mut reference.bus).unwrap();
        } else {
            assert_eq!(count, 0); // FFFFh + word exceeds the SS limit.
            assert!(batch.cpu.snapshot() == before);
            assert_eq!(
                batch.cpu.step(&mut batch.bus),
                reference.cpu.step(&mut reference.bus)
            );
        }
        state(&batch, &reference);
        memory(&batch, &reference);
    }
}
