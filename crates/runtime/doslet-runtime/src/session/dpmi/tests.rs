use super::*;

fn prepared(mode: Mode) -> Session {
    let mut s = Session::new(&[0x90]).unwrap();
    s.enable_dpmi(mode).unwrap();
    s.machine.cpu.registers.set(Gpr16::Ax, 1);
    s.machine.cpu.registers.es = 0x8000;
    s.machine.cpu.registers.ss = 0x1000;
    s.machine.cpu.registers.set(Gpr16::Sp, 0xFF00);
    s.machine.bus.write_u16(0x1FF00, 0x100).unwrap();
    s.machine.bus.write_u16(0x1FF02, 0x1000).unwrap();
    s.dpmi_enter().unwrap();
    let code = s.dpmi_alloc_descriptors(1).unwrap();
    s.dpmi_descriptor_at(LDT + u32::from(code & !7), 0, u32::MAX, 0x9A, 0x40)
        .unwrap();
    s.machine
        .cpu
        .load_segment(&mut s.machine.bus, Seg::Cs, code)
        .unwrap();
    for seg in [Seg::Ds, Seg::Es, Seg::Ss] {
        s.machine
            .cpu
            .load_segment(&mut s.machine.bus, seg, DATA)
            .unwrap();
    }
    s.machine.cpu.registers.set32(Gpr32::Esp, 0x70000);
    s.machine.cpu.registers.ip = 0x20000;
    s.machine.cpu.flags = 0x202;
    s.machine.bus.write_u8(0x20000, 0xCD).unwrap();
    s.machine.bus.write_u8(0x20001, 0x31).unwrap();
    s.start();
    s
}

fn call(s: &mut Session, function: u16) {
    s.machine.cpu.registers.ip = 0x20000;
    s.machine.cpu.registers.set(Gpr16::Ax, function);
    let cs = s.registers().cs;
    for _ in 0..100 {
        s.advance(1);
        assert_eq!(s.status(), SessionStatus::Running, "{:?}", s.fault());
        if s.registers().cs == cs && s.registers().ip == 0x20002 {
            return;
        }
    }
    panic!("DPMI did not return: {:?}", s.registers());
}

fn simulated(s: &mut Session, vector: u8, ax: u16) -> [u8; 50] {
    for i in 0..50 {
        s.machine.bus.write_u8(0x30000 + i, 0).unwrap();
    }
    s.machine.bus.write_u16(0x3001C, ax).unwrap();
    s.machine.bus.write_u16(0x30020, 0x202).unwrap();
    s.machine.cpu.registers.set(Gpr16::Bx, u16::from(vector));
    s.machine.cpu.registers.set(Gpr16::Cx, 0);
    s.machine.cpu.registers.set32(Gpr32::Edi, 0x30000);
    call(s, 0x300);
    assert_eq!(s.flags() & 1, 0); // Outer DPMI success, separate from BIOS CF.
    s.ram_view(0x30000..0x30032).unwrap().try_into().unwrap()
}

#[test]
fn direct_and_reflected_calls_return_identical_register_frames() {
    let mut direct = prepared(Mode::Direct);
    let mut reference = prepared(Mode::Reflect);
    for (vector, ax) in [
        (0x10, 0x4F03),
        (0x10, 0x4F05),
        (0x16, 0x0100),
        (0x16, 0x0200),
        (0x33, 3),
        (0x33, 0xB),
        (0x21, 0x3000),
    ] {
        let got = simulated(&mut direct, vector, ax);
        let expected = simulated(&mut reference, vector, ax);
        assert_eq!(got, expected, "INT {vector:02x}, AX={ax:04x}");
        assert_eq!(direct.registers(), reference.registers());
        assert_eq!(direct.eflags(), reference.eflags());
        assert_eq!(direct.steps(), reference.steps());
        assert_eq!(
            direct.execution_tier_stats(),
            reference.execution_tier_stats()
        );
        assert_eq!(direct.emulated_nanos(), reference.emulated_nanos());
        assert_eq!(direct.ram_view(0..0x90000), reference.ram_view(0..0x90000));
    }
    assert_eq!(direct.dpmi_stats().unwrap().direct, 7);
    assert_eq!(direct.dpmi_stats().unwrap().reflected, 0);
    assert_eq!(reference.dpmi_stats().unwrap().reflected, 7);
}

#[test]
fn real_bios_timer_calls_protected_tick_and_allows_chaining_to_default() {
    for mode in [Mode::Reflect, Mode::Direct] {
        for chain in [false, true] {
            let mut s = prepared(mode);
            let mut reference = prepared(mode);
            let code = s.registers().cs;
            let mut handler = alloc::vec![
                0x1E, 0x66, 0xB8, DATA as u8, 0, 0x8E, 0xD8, // Save DS; load flat data.
                0xFF, 0x05, 0, 0x10, 3, 0, // INC DWORD PTR [31000h].
                0xB8, 0xEF, 0xBE, 0xAD, 0xDE, // Clobber EAX on the protected side.
            ];
            if chain {
                handler.extend_from_slice(&[0x9C, 0x9A]); // PUSHFD; CALL FAR old handler.
                handler.extend_from_slice(&(PM_BASE + 0x1C * 8).to_le_bytes());
                handler.extend_from_slice(&CODE.to_le_bytes());
            }
            handler.extend_from_slice(&[0x1F, 0xCF]);
            for (i, byte) in handler.into_iter().enumerate() {
                s.machine.bus.write_u8(0x40000 + i as u32, byte).unwrap();
            }
            s.dpmi_gate(0x1C, code, 0x40000).unwrap();
            for ticks in 1..=2 {
                let expected = simulated(&mut reference, 8, 0x1234);
                let got = simulated(&mut s, 8, 0x1234);
                assert_eq!(got, expected, "mode={mode:?} chain={chain}");
                assert_eq!(s.machine.bus.read_u32(0x31000).unwrap(), ticks);
                assert_eq!(s.dpmi.as_ref().unwrap().depth, 0);
                assert!(s.machine.cpu.protected_mode());
                assert_eq!(s.registers(), reference.registers());
                assert_eq!(s.eflags(), reference.eflags());
            }
        }
    }
}

/// Enter a real-mode `STI; JMP $` loop at 5000:0000 through 0301h, which
/// never returns.
fn real_mode_loop(s: &mut Session) {
    for (i, b) in [0xFB, 0xEB, 0xFE].into_iter().enumerate() {
        s.machine.bus.write_u8(0x50000 + i as u32, b).unwrap();
    }
    for i in 0..50 {
        s.machine.bus.write_u8(0x30000 + i, 0).unwrap();
    }
    s.machine.bus.write_u16(0x3002C, 0x5000).unwrap();
    s.machine.cpu.registers.ip = 0x20000;
    s.machine.cpu.registers.set(Gpr16::Ax, 0x301);
    s.machine.cpu.registers.set(Gpr16::Bx, 0);
    s.machine.cpu.registers.set(Gpr16::Cx, 0);
    s.machine.cpu.registers.set32(Gpr32::Edi, 0x30000);
    for _ in 0..100 {
        s.advance(1);
        if !s.machine.cpu.protected_mode() && s.registers().cs == 0x5000 {
            return;
        }
    }
    panic!("real-mode procedure not reached: {:?}", s.registers());
}

#[test]
fn real_mode_hardware_interrupts_reach_the_protected_handler() {
    for mode in [Mode::Reflect, Mode::Direct] {
        let mut s = prepared(mode);
        let code = s.registers().cs;
        let handler = [
            0x1E, 0x66, 0xB8, DATA as u8, 0, 0x8E, 0xD8, // Save DS; load flat data.
            0xFF, 0x05, 0, 0x10, 3, 0, // INC DWORD PTR [31000h].
            0xB0, 0x20, 0xE6, 0x20, 0x1F, 0xCF, // EOI; restore DS; IRETD.
        ];
        for (i, byte) in handler.into_iter().enumerate() {
            s.machine.bus.write_u8(0x40000 + i as u32, byte).unwrap();
        }
        s.dpmi_gate(9, code, 0x40000).unwrap();
        real_mode_loop(&mut s);
        let depth = s.dpmi.as_ref().unwrap().depth;
        for presses in 1..=2 {
            s.machine.bus.pic.raise_edge(1);
            s.advance(20);
            assert_eq!(s.status(), SessionStatus::Running, "{:?}", s.fault());
            assert_eq!(
                s.machine.bus.read_u32(0x31000).unwrap(),
                presses,
                "{mode:?}"
            );
            assert!(!s.machine.cpu.protected_mode());
            assert_eq!(s.registers().cs, 0x5000);
            assert_eq!(s.dpmi.as_ref().unwrap().depth, depth);
        }
    }
}

#[test]
fn unhandled_irqs_return_through_the_bios_default_handler() {
    let mut s = prepared(Mode::Direct);
    assert_eq!(
        s.machine.bus.read_u16(0x0D * 4 + 2).unwrap(),
        bios::SHIM_SEGMENT
    );
    for (i, b) in [0xFB, 0xEB, 0xFE].into_iter().enumerate() {
        s.machine.bus.write_u8(0x20010 + i as u32, b).unwrap();
    }
    s.machine.cpu.registers.ip = 0x20010;
    // IRQ 5 in protected mode reflects to its real-mode default handler.
    s.machine.bus.pic.raise_edge(5);
    s.advance(40);
    assert_eq!(s.status(), SessionStatus::Running, "{:?}", s.fault());
    assert!(s.machine.cpu.protected_mode());
    assert_eq!(s.dpmi_stats().unwrap().reflected, 1);
    real_mode_loop(&mut s);
    s.machine.bus.pic.raise_edge(5);
    s.advance(40);
    assert_eq!(s.status(), SessionStatus::Running, "{:?}", s.fault());
    assert_eq!(s.registers().cs, 0x5000);
    assert!(!s.machine.bus.pic.has_interrupt());
}

#[test]
fn installed_guest_handler_and_changed_shim_are_never_bypassed() {
    for altered_shim in [false, true] {
        let mut s = prepared(Mode::Direct);
        let at = if altered_shim {
            bios::SHIM_BASE + 0x10
        } else {
            0x60000
        };
        for (i, b) in [0xB8, 0xAD, 0xDE, 0xCF].into_iter().enumerate() {
            s.machine.bus.write_u8(at + i as u32, b).unwrap();
        }
        if !altered_shim {
            s.machine.bus.write_u16(0x40, 0).unwrap();
            s.machine.bus.write_u16(0x42, 0x6000).unwrap();
        }
        let output = simulated(&mut s, 0x10, 0x4F03);
        assert_eq!(&output[28..30], &[0xAD, 0xDE]);
        assert_eq!(s.dpmi_stats().unwrap().direct, 0);
        assert_eq!(s.dpmi_stats().unwrap().reflected, 1);
    }
}

#[test]
fn memory_abi_preserves_live_blocks_and_counts_failed_allocations() {
    let mut s = prepared(Mode::Direct);
    s.machine.cpu.registers.set(Gpr16::Bx, 0);
    s.machine.cpu.registers.set(Gpr16::Cx, 4097);
    call(&mut s, 0x501);
    assert_eq!(s.flags() & 1, 0);
    let handle = (s.registers().get(Gpr16::Si), s.registers().get(Gpr16::Di));
    assert_eq!(s.dpmi_stats().unwrap().memory.committed_bytes, 8192);
    let usage = s.memory_usage();
    assert!(usage.dpmi_entered && usage.protected_mode);
    assert_eq!(usage.dpmi_requested, 4097);
    assert_eq!(usage.dpmi_committed, 8192);
    assert_eq!(usage.dpmi_blocks, 1);
    assert_eq!(usage.dpmi_peak, 8192);
    s.machine.cpu.registers.set(Gpr16::Bx, 0xFFFF);
    s.machine.cpu.registers.set(Gpr16::Cx, 0xFFFF);
    call(&mut s, 0x503);
    assert_eq!(s.flags() & 1, 1);
    assert_eq!(s.dpmi_stats().unwrap().memory.requested_bytes, 4097);
    s.machine.cpu.registers.set(Gpr16::Si, handle.0);
    s.machine.cpu.registers.set(Gpr16::Di, handle.1);
    call(&mut s, 0x502);
    assert_eq!(s.flags() & 1, 0);
    assert_eq!(s.dpmi_stats().unwrap().memory.committed_bytes, 0);
    assert_eq!(s.dpmi_stats().unwrap().memory.peak_requested_bytes, 4097);
    let freed = s.memory_usage();
    assert_eq!(freed.dpmi_committed, 0);
    assert_eq!(freed.dpmi_blocks, 0);
    assert_eq!(freed.dpmi_peak, 8192);
}

#[test]
fn real_far_call_receives_copied_stack_words() {
    let mut s = prepared(Mode::Direct);
    // MOV BP,SP; MOV AX,[BP+4]; MOV DX,[BP+6]; RETF at 6000:0000.
    for (i, byte) in [0x89, 0xE5, 0x8B, 0x46, 0x04, 0x8B, 0x56, 0x06, 0xCB]
        .into_iter()
        .enumerate()
    {
        s.machine.bus.write_u8(0x60000 + i as u32, byte).unwrap();
    }
    for i in 0..50 {
        s.machine.bus.write_u8(0x30000 + i, 0).unwrap();
    }
    s.machine.bus.write_u16(0x3002A, 0).unwrap(); // IP
    s.machine.bus.write_u16(0x3002C, 0x6000).unwrap(); // CS
    s.machine.bus.write_u16(0x30020, 0x202).unwrap();
    let esp = s.registers().get32(Gpr32::Esp) - 4;
    s.machine.cpu.registers.set32(Gpr32::Esp, esp);
    s.machine.bus.write_u16(esp, 0x1111).unwrap();
    s.machine.bus.write_u16(esp + 2, 0x2222).unwrap();
    s.machine.cpu.registers.set(Gpr16::Cx, 2);
    s.machine.cpu.registers.set32(Gpr32::Edi, 0x30000);
    call(&mut s, 0x301);
    assert_eq!(s.flags() & 1, 0);
    assert_eq!(s.machine.bus.read_u16(0x3001C).unwrap(), 0x1111);
    assert_eq!(s.machine.bus.read_u16(0x30014).unwrap(), 0x2222);
    assert_eq!(s.registers().get32(Gpr32::Esp), esp);
    // More words than a host stack slice holds are refused.
    s.machine.cpu.registers.set(Gpr16::Cx, MAX_STACK_WORDS + 1);
    call(&mut s, 0x301);
    assert_eq!(s.flags() & 1, 1);
}

#[test]
fn resize_moves_a_block_with_its_contents_when_it_cannot_grow() {
    let mut s = prepared(Mode::Direct);
    let allocate = |s: &mut Session, bytes: u32| {
        s.machine.cpu.registers.set(Gpr16::Bx, (bytes >> 16) as u16);
        s.machine.cpu.registers.set(Gpr16::Cx, bytes as u16);
        call(s, 0x501);
        assert_eq!(s.flags() & 1, 0);
        let r = s.registers();
        (
            (u32::from(r.get(Gpr16::Bx)) << 16) | u32::from(r.get(Gpr16::Cx)),
            (r.get(Gpr16::Si), r.get(Gpr16::Di)),
        )
    };
    let (first, handle) = allocate(&mut s, 4096);
    let (second, _) = allocate(&mut s, 4096);
    assert_eq!(second, first + 4096);
    for i in 0..4096 {
        s.machine.bus.write_u8(first + i, i as u8).unwrap();
    }
    s.machine.bus.write_u8(second, 0xAA).unwrap();
    s.machine.cpu.registers.set(Gpr16::Bx, 0);
    s.machine.cpu.registers.set(Gpr16::Cx, 16384);
    s.machine.cpu.registers.set(Gpr16::Si, handle.0);
    s.machine.cpu.registers.set(Gpr16::Di, handle.1);
    call(&mut s, 0x503);
    assert_eq!(s.flags() & 1, 0);
    let r = s.registers();
    let moved = (u32::from(r.get(Gpr16::Bx)) << 16) | u32::from(r.get(Gpr16::Cx));
    assert_eq!((r.get(Gpr16::Si), r.get(Gpr16::Di)), handle);
    assert_eq!(moved, second + 4096);
    for i in 0..4096 {
        assert_eq!(s.machine.bus.read_u8(moved + i).unwrap(), i as u8);
    }
    assert_eq!(s.machine.bus.read_u8(second).unwrap(), 0xAA);
    assert_eq!(s.dpmi_stats().unwrap().memory.committed_bytes, 16384 + 4096);
}

#[test]
fn specific_descriptors_and_paging_hints() {
    let mut s = prepared(Mode::Direct);
    s.machine.cpu.registers.set(Gpr16::Bx, 0x0C);
    call(&mut s, 0x0D);
    assert_eq!(s.flags() & 1, 0);
    call(&mut s, 0x0D);
    assert_eq!((s.flags() & 1, s.registers().get(Gpr16::Ax)), (1, 0x8011));
    s.machine.cpu.registers.set(Gpr16::Bx, 0x84);
    call(&mut s, 0x0D);
    assert_eq!((s.flags() & 1, s.registers().get(Gpr16::Ax)), (1, 0x8022));
    // Ordinary allocation leaves the reserved entries to 000Dh.
    s.machine.cpu.registers.set(Gpr16::Cx, 1);
    call(&mut s, 0x0000);
    assert!(s.registers().get(Gpr16::Ax) >> 3 >= SPECIFIC as u16);
    for function in [0x0602, 0x0603, 0x0702, 0x0703] {
        call(&mut s, function);
        assert_eq!(s.flags() & 1, 0, "{function:04x}");
    }
}

#[test]
fn invalid_real_call_and_unknown_function_report_failure() {
    let mut s = prepared(Mode::Direct);
    s.machine.cpu.registers.set(Gpr16::Cx, MAX_STACK_WORDS + 1);
    s.machine.cpu.registers.set32(Gpr32::Edi, 0x30000);
    call(&mut s, 0x300);
    assert_eq!(s.flags() & 1, 1);
    assert_eq!(s.registers().get(Gpr16::Ax), 0x8021);
    call(&mut s, 0xFFFF);
    assert_eq!(s.flags() & 1, 1);
    assert_eq!(s.registers().get(Gpr16::Ax), 0x8001);
}

#[test]
fn direct_iret_procedure_preserves_custom_stack_and_guest_time() {
    let mut direct = prepared(Mode::Direct);
    let mut reference = prepared(Mode::Reflect);
    for flags in [0, 1, 0x202, 0x246] {
        for s in [&mut direct, &mut reference] {
            let mut rm = RealRegisters::read(&mut s.machine.bus, 0x30000).unwrap();
            rm.general = [0; 8];
            rm.general[0] = 0x0100; // Keyboard status changes ZF in saved flags.
            rm.general[4] = 0x200;
            rm.segments = [0; 6];
            rm.segments[Seg::Cs as usize] = bios::SHIM_SEGMENT;
            rm.segments[Seg::Ss as usize] = 0x6000;
            rm.ip = 0x16;
            rm.flags = flags;
            rm.write(&mut s.machine.bus, 0x30000).unwrap();
            s.machine.cpu.registers.set(Gpr16::Cx, 0);
            s.machine.cpu.registers.set32(Gpr32::Edi, 0x30000);
            call(s, 0x302);
        }
        assert_eq!(direct.registers(), reference.registers());
        assert_eq!(direct.eflags(), reference.eflags());
        assert_eq!(direct.steps(), reference.steps());
        assert_eq!(direct.emulated_nanos(), reference.emulated_nanos());
        assert_eq!(direct.ram_view(0..0x90000), reference.ram_view(0..0x90000));
    }
}

#[test]
fn callback_maps_real_stack_and_accepts_replacement_return_structure() {
    let mut s = prepared(Mode::Direct);
    let code = s.registers().cs;
    s.dpmi.as_mut().unwrap().callbacks[0] = Some(Callback {
        code,
        ip: 0x22000,
        data: DATA,
        offset: 0x30000,
        structure: 0x30000,
    });
    s.machine.cpu.set_protected_mode(false);
    for (seg, val) in [(Seg::Cs, 0x5000), (Seg::Ss, 0x6000)] {
        s.machine
            .cpu
            .load_segment(&mut s.machine.bus, seg, val)
            .unwrap();
    }
    s.machine.cpu.registers.set32(Gpr32::Esp, 0x200);
    s.machine.cpu.registers.ip = 0x1234;
    s.dpmi_callback(0).unwrap();
    assert_eq!(
        s.dpmi_pointer(Seg::Ds, s.registers().get32(Gpr32::Esi), 4, Access::Read)
            .unwrap(),
        0x60200
    );
    let mut rm = RealRegisters::read(&mut s.machine.bus, 0x30000).unwrap();
    rm.general[0] = 0xDEADBEEF;
    rm.ip = 0x4321;
    rm.write(&mut s.machine.bus, 0x30100).unwrap();
    s.machine.cpu.registers.set32(Gpr32::Edi, 0x30100);
    s.dpmi_callback_return().unwrap();
    assert!(!s.machine.cpu.protected_mode());
    assert_eq!(s.registers().get32(Gpr32::Eax), 0xDEADBEEF);
    assert_eq!(s.registers().ip, 0x4321);
    assert_eq!(s.registers().segment_cache(Seg::Ss).base, 0x60000);
}

#[test]
fn descriptor_exhaustion_and_zero_dos_allocation_are_guest_errors() {
    let mut s = prepared(Mode::Direct);
    s.machine.cpu.registers.set(Gpr16::Bx, 0);
    call(&mut s, 0x100);
    assert_eq!(s.flags() & 1, 1);
    s.dpmi.as_mut().unwrap().used.fill(true);
    for function in [0x0000, 0x0002, 0x0100] {
        s.machine.cpu.registers.set(Gpr16::Bx, 16);
        s.machine.cpu.registers.set(Gpr16::Cx, 1);
        call(&mut s, function);
        assert_eq!(s.flags() & 1, 1);
        assert_eq!(s.registers().get(Gpr16::Ax), 0x8011);
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn host_services_allocate_nothing_after_setup() {
    let mut s = prepared(Mode::Direct);
    let measured = allocation_counter::measure(|| {
        for _ in 0..100 {
            simulated(&mut s, 0x16, 0x0100);
            s.machine.cpu.registers.set(Gpr16::Bx, 0);
            s.machine.cpu.registers.set(Gpr16::Cx, 4096);
            call(&mut s, 0x501);
            assert!(s.memory_usage().dpmi_committed > 0);
            call(&mut s, 0x502);
        }
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}

#[test]
fn restored_context_replaces_hidden_caches_even_with_same_selector() {
    let mut s = prepared(Mode::Direct);
    let original = s.machine.cpu.snapshot();
    let control = s.machine.cpu.control.clone();
    let context = s.machine.cpu.context();
    let selector = s.registers().cs;
    s.dpmi_descriptor_at(LDT + u32::from(selector & !7), 0x40000, 0xFFFF, 0x9A, 0)
        .unwrap();
    s.machine
        .cpu
        .load_segment(&mut s.machine.bus, Seg::Cs, selector)
        .unwrap();
    s.machine.cpu.registers.set32(Gpr32::Eax, 123);
    s.machine.cpu.set_protected_mode(false);
    s.machine.cpu.control.idtr.base = 0;
    s.machine.cpu.restore_context(&context);
    assert!(s.machine.cpu.matches(&original));
    assert_eq!(s.machine.cpu.control, control);
    assert_eq!(s.registers().segment_cache(Seg::Cs).base, 0);
    assert!(s.registers().code32());
}

#[test]
fn debugger_uses_reflection_and_restores_direct_dispatch_on_detach() {
    let mut s = prepared(Mode::Direct);
    let settings = s.debug_disable_accelerations();
    simulated(&mut s, 0x16, 0x0100);
    assert_eq!(s.dpmi_stats().unwrap().direct, 0);
    assert_eq!(s.dpmi_stats().unwrap().reflected, 1);
    s.debug_restore_accelerations(settings);
    simulated(&mut s, 0x16, 0x0100);
    assert_eq!(s.dpmi_stats().unwrap().direct, 1);
}

fn callback_session() -> Session {
    let mut s = prepared(Mode::Direct);
    let code = s.registers().cs;
    s.machine
        .cpu
        .load_segment(&mut s.machine.bus, Seg::Ds, code)
        .unwrap();
    s.machine.cpu.registers.set32(Gpr32::Esi, 0x22000);
    s.machine.cpu.registers.set32(Gpr32::Edi, 0x30100);
    call(&mut s, 0x303);
    let (segment, offset) = (s.registers().get(Gpr16::Cx), s.registers().get(Gpr16::Dx));
    s.machine
        .cpu
        .load_segment(&mut s.machine.bus, Seg::Ds, DATA)
        .unwrap();
    // Callback uses DS:ESI to pop the real far return address into ES:EDI,
    // supplies EAX, and IRETD returns to the host's protected trampoline.
    for (i, byte) in [
        0x8B, 0x06, 0x26, 0x89, 0x47, 42, 0x26, 0x66, 0x83, 0x47, 46, 4, 0x26, 0xC7, 0x47, 28,
        0x78, 0x56, 0x34, 0x12, 0xCF,
    ]
    .into_iter()
    .enumerate()
    {
        s.machine.bus.write_u8(0x22000 + i as u32, byte).unwrap();
    }
    for (i, byte) in [
        0x9A,
        offset as u8,
        (offset >> 8) as u8,
        segment as u8,
        (segment >> 8) as u8,
        0xCF,
    ]
    .into_iter()
    .enumerate()
    {
        s.machine.bus.write_u8(0x60000 + i as u32, byte).unwrap();
    }
    let mut rm = RealRegisters::read(&mut s.machine.bus, 0x30000).unwrap();
    rm.segments[Seg::Cs as usize] = 0x6000;
    rm.segments[Seg::Ss as usize] = 0x7000;
    rm.general[4] = 0x200;
    rm.flags = 0x202;
    rm.write(&mut s.machine.bus, 0x30000).unwrap();
    s.machine.cpu.registers.set(Gpr16::Cx, 0);
    s.machine.cpu.registers.set32(Gpr32::Edi, 0x30000);
    s
}

#[test]
fn guest_callback_returns_through_nested_real_and_protected_frames() {
    let mut s = callback_session();
    call(&mut s, 0x302);
    assert_eq!(s.machine.bus.read_u32(0x3001C).unwrap(), 0x12345678);
    assert_eq!(s.dpmi.as_ref().unwrap().depth, 0);
    assert_eq!(s.registers().get32(Gpr32::Esp), 0x70000);
}

#[test]
fn observed_call_uses_real_instruction_without_changing_configured_mode() {
    let mut s = prepared(Mode::Direct);
    s.machine.bus.write_u16(0x3001C, 0x0100).unwrap();
    s.machine.bus.write_u16(0x30020, 0x202).unwrap();
    s.machine.cpu.registers.set(Gpr16::Ax, 0x300);
    s.machine.cpu.registers.set(Gpr16::Bx, 0x16);
    s.machine.cpu.registers.set(Gpr16::Cx, 0);
    s.machine.cpu.registers.set32(Gpr32::Edi, 0x30000);
    let mut observation = doslet_pc::StepObservation::default();
    s.execute_step_observed(&mut observation).unwrap(); // INT 31h.
    s.execute_step_observed(&mut observation).unwrap(); // Reflected real IRET.
    assert_eq!(s.dpmi_stats().unwrap().direct, 0);
    assert_eq!(s.dpmi_stats().unwrap().reflected, 1);
    assert_eq!(s.dpmi.as_ref().unwrap().mode, Mode::Direct);
    assert_eq!(s.registers().cs, bios::SHIM_SEGMENT);
    assert_eq!(s.registers().ip, RM_RETURN);
    assert!(observation.service_writes_unobserved);
}

#[test]
fn direct_services_charge_the_dispatch_label_with_weighted_timing() {
    for tier in 0..4 {
        let mut direct = prepared(Mode::Direct);
        let mut reference = prepared(Mode::Reflect);
        for s in [&mut direct, &mut reference] {
            // Equivalent labels for the protected dispatcher and real IRET
            // allow comparison at different HOT/COLD/GREY/IRQ rates, including MAX.
            let labels = [
                (bios::SHIM_BASE + PM_BASE + 0x31 * 8, true),
                (bios::SHIM_BASE + 0x16, false),
            ]
            .map(|(address, code32)| doslet_x86::ExecutionLabel {
                address,
                code32,
                bytes: s
                    .ram_view(address as usize..address as usize + 15)
                    .unwrap()
                    .try_into()
                    .unwrap(),
                tier,
            })
            .to_vec();
            s.set_execution_tiers(Some(labels), [None, Some(8), Some(133)])
                .unwrap();
        }
        for _ in 0..8 {
            assert_eq!(
                simulated(&mut direct, 0x16, 0x0100),
                simulated(&mut reference, 0x16, 0x0100)
            );
            assert_eq!(direct.registers(), reference.registers());
            assert_eq!(direct.eflags(), reference.eflags());
            assert_eq!(direct.steps(), reference.steps());
            assert_eq!(direct.emulated_nanos(), reference.emulated_nanos());
            assert_eq!(
                direct.execution_tier_stats(),
                reference.execution_tier_stats()
            );
            assert_eq!(direct.ram_view(0..0x90000), reference.ram_view(0..0x90000));
        }
        let mut expected = [0, 0, 8, 0]; // One guest INT 31h per call.
        expected[usize::from(tier)] += 16; // Synthetic real IRET and protected IRET.
        assert_eq!(direct.executed_tier_counts(), expected);
        assert_eq!(direct.dpmi_stats().unwrap().direct, 8);
        assert_eq!(direct.dpmi_stats().unwrap().reflected, 0);
        assert_eq!(reference.dpmi_stats().unwrap().reflected, 8);
    }
}

/// A 32-bit client registers handlers, then runs `client` at 0x20000. The
/// #DE handler counts in [0x30000] and skips the 2-byte DIV. The #NP/#GP
/// handler stores the error code in [0x30004] and skips a 2-byte MOV Sreg.
fn faulting_client(vectors: &[(u8, u32)], client: &[u8]) -> Session {
    let mut s = prepared(Mode::Direct);
    let code = s.registers().cs;
    for &(vector, handler) in vectors {
        s.machine.cpu.registers.set(Gpr16::Bx, u16::from(vector));
        s.machine.cpu.registers.set(Gpr16::Cx, code);
        s.machine.cpu.registers.set32(Gpr32::Edx, handler);
        call(&mut s, 0x203);
        assert_eq!(s.flags() & 1, 0);
    }
    let divide = [0xFF, 0x05, 0, 0, 3, 0, 0x83, 0x44, 0x24, 0x0C, 2, 0xCB];
    let selector = [
        0x50, 0x8B, 0x44, 0x24, 0x0C, 0xA3, 4, 0, 3, 0, 0x83, 0x44, 0x24, 0x10, 2, 0x58, 0xCB,
    ];
    for (at, code) in [
        (0x21000, &divide[..]),
        (0x21100, &selector[..]),
        (0x20000, client),
    ] {
        for (i, &byte) in code.iter().enumerate() {
            s.machine.bus.write_u8(at + i as u32, byte).unwrap();
        }
    }
    s.machine.cpu.registers.ip = 0x20000;
    s
}

/// MOV ECX,0; DIV ECX; NOP; JMP $.
const DIVIDE: [u8; 10] = [0xB9, 0, 0, 0, 0, 0xF7, 0xF1, 0x90, 0xEB, 0xFE];

fn run_until_loop(s: &mut Session) {
    for _ in 0..200 {
        if s.status() != SessionStatus::Running || s.registers().ip == 0x20008 {
            return;
        }
        s.advance(1);
    }
}

#[test]
fn divide_error_reaches_the_registered_handler_and_resumes_the_client() {
    let mut s = faulting_client(&[(0, 0x21000)], &DIVIDE);
    s.machine.cpu.registers.set32(Gpr32::Eax, 0x1234);
    s.machine.cpu.registers.set32(Gpr32::Edx, 7);
    let (cs, ss, esp) = (
        s.registers().cs,
        s.registers().ss,
        s.registers().get32(Gpr32::Esp),
    );
    run_until_loop(&mut s);
    assert_eq!(s.status(), SessionStatus::Running, "{:?}", s.fault());
    assert_eq!(s.machine.bus.read_u32(0x30000).unwrap(), 1);
    assert_eq!(s.registers().ip, 0x20008);
    assert_eq!((s.registers().cs, s.registers().ss), (cs, ss));
    assert_eq!(s.registers().get32(Gpr32::Esp), esp);
    // The faulting DIV changed nothing before the handler ran.
    assert_eq!(s.registers().get32(Gpr32::Eax), 0x1234);
    assert_eq!(s.registers().get32(Gpr32::Edx), 7);
    assert_eq!(s.dpmi.as_ref().unwrap().depth, 0);
}

#[test]
fn selector_faults_carry_their_error_code() {
    // MOV AX,1234h; MOV DS,AX; JMP $ (the handler skips to the JMP).
    let client = [0x66, 0xB8, 0x34, 0x12, 0x8E, 0xD8, 0x90, 0x90, 0xEB, 0xFE];
    let mut s = faulting_client(&[(11, 0x21100), (13, 0x21100)], &client);
    run_until_loop(&mut s);
    assert_eq!(s.status(), SessionStatus::Running, "{:?}", s.fault());
    assert_eq!(s.machine.bus.read_u32(0x30004).unwrap(), 0x1234);
    assert_eq!(s.registers().ip, 0x20008);
}

#[test]
fn a_faulting_pop_leaves_the_stack_pointer_unchanged() {
    // PUSH 1234h; POP DS; JMP $ (the handler skips 2 bytes from POP DS).
    let client = [0x68, 0x34, 0x12, 0, 0, 0x1F, 0x90, 0xEB, 0xFE];
    let mut s = faulting_client(&[(11, 0x21100), (13, 0x21100)], &client);
    let esp = s.registers().get32(Gpr32::Esp);
    for _ in 0..200 {
        if s.status() != SessionStatus::Running || s.registers().ip == 0x20007 {
            break;
        }
        s.advance(1);
    }
    assert_eq!(s.status(), SessionStatus::Running, "{:?}", s.fault());
    assert_eq!(s.registers().ip, 0x20007);
    assert_eq!(s.machine.bus.read_u32(0x30004).unwrap(), 0x1234);
    // The pushed selector is still on the stack, as before the POP.
    assert_eq!(s.registers().get32(Gpr32::Esp), esp - 4);
}

#[test]
fn faults_without_a_handler_or_inside_one_still_stop_the_session() {
    let mut s = faulting_client(&[], &DIVIDE);
    run_until_loop(&mut s);
    assert_eq!(s.status(), SessionStatus::Faulted);
    assert!(!s.machine.cpu.precise_faults_enabled());
    // A handler that faults itself is not entered again.
    let mut s = faulting_client(&[(0, 0x20005)], &DIVIDE);
    run_until_loop(&mut s);
    assert_eq!(s.status(), SessionStatus::Faulted);
}

#[cfg(feature = "save-state")]
#[test]
fn portable_states_keep_a_running_exception_handler() {
    let mut original = faulting_client(&[(0, 0x21000)], &DIVIDE);
    while original.dpmi.as_ref().unwrap().depth == 0 {
        original.advance(1);
        assert_eq!(original.status(), SessionStatus::Running);
    }
    let bytes = original.save_state().unwrap();
    let mut restored = Session::load_state(&bytes).unwrap();
    assert!(restored.machine.cpu.precise_faults_enabled());
    for session in [&mut original, &mut restored] {
        run_until_loop(session);
        assert_eq!(session.registers().ip, 0x20008);
        assert_eq!(session.machine.bus.read_u32(0x30000).unwrap(), 1);
    }
    assert_eq!(
        original.save_state().unwrap(),
        restored.save_state().unwrap()
    );
}

#[cfg(feature = "save-state")]
#[test]
fn portable_states_keep_the_host_inside_nested_frames() {
    let mut original = callback_session();
    // A live allocation, then a real-mode call whose code calls back into
    // protected mode. Save while both host frames are active.
    original.machine.cpu.registers.set(Gpr16::Bx, 0);
    original.machine.cpu.registers.set(Gpr16::Cx, 4097);
    call(&mut original, 0x501);
    original.machine.cpu.registers.set(Gpr16::Cx, 0);
    original.machine.cpu.registers.set32(Gpr32::Edi, 0x30000);
    original.machine.cpu.registers.ip = 0x20000;
    original.machine.cpu.registers.set(Gpr16::Ax, 0x302);
    let cs = original.registers().cs;
    while original.dpmi.as_ref().unwrap().depth < 2 {
        original.advance(1);
        assert_eq!(original.status(), SessionStatus::Running);
    }
    let bytes = original.save_state().unwrap();
    let mut restored = Session::load_state(&bytes).unwrap();
    assert_eq!(restored.save_state().unwrap(), bytes);
    assert_eq!(restored.dpmi_mode(), Some(Mode::Direct));
    assert_eq!(restored.dpmi.as_ref().unwrap().depth, 2);
    for session in [&mut original, &mut restored] {
        for _ in 0..100 {
            if session.registers().cs == cs && session.registers().ip == 0x20002 {
                break;
            }
            session.advance(1);
            assert_eq!(session.status(), SessionStatus::Running);
        }
        assert_eq!(session.machine.bus.read_u32(0x3001C).unwrap(), 0x12345678);
        assert_eq!(session.dpmi.as_ref().unwrap().depth, 0);
        assert_eq!(session.dpmi_stats().unwrap().memory.committed_bytes, 8192);
    }
    assert_eq!(
        original.save_state().unwrap(),
        restored.save_state().unwrap()
    );
    // States without a host still restore without one.
    let plain = Session::new(&[0x90]).unwrap();
    let restored = Session::load_state(&plain.save_state().unwrap()).unwrap();
    assert!(restored.dpmi_stats().is_none());
}

#[cfg(feature = "save-state")]
#[test]
fn host_records_are_validated_before_restoring() {
    use doslet_state::State;
    let host = prepared(Mode::Reflect).dpmi.unwrap();
    let wire = host.save();
    assert!(Host::load(wire.clone()).is_ok());
    let mut invalid = wire.clone();
    invalid.mode = 3;
    assert!(Host::load(invalid).is_err());
    let mut invalid = wire.clone();
    invalid.used.pop();
    assert!(Host::load(invalid).is_err());
    let mut invalid = wire.clone();
    invalid.depth = 17;
    assert!(Host::load(invalid).is_err());
    let mut invalid = wire;
    invalid.memory_start = invalid.memory_end + 1;
    assert!(Host::load(invalid).is_err());
}

#[cfg(feature = "save-state")]
#[path = "working_state_tests.rs"]
mod working_states;

#[test]
fn mouse_callback_is_delivered_while_dpmi_client_runs_protected_code() {
    for mode in [Mode::Direct, Mode::Reflect] {
        let mut s = prepared(mode);
        // Real callback records event/position through CS and returns far.
        for (i, byte) in [
            0x2E, 0xA3, 0x00, 0x01, 0x2E, 0x89, 0x0E, 0x02, 0x01, 0x2E, 0x89, 0x16, 0x04, 0x01,
            0xCB,
        ]
        .into_iter()
        .enumerate()
        {
            s.machine.bus.write_u8(0x60000 + i as u32, byte).unwrap();
        }
        s.mouse.callback = (7, 0x6000, 0);
        s.machine.bus.write_u16(0xF0409, 0).unwrap();
        s.machine.bus.write_u16(0xF040B, 0x6000).unwrap();
        s.machine.bus.write_u8(0x20000, 0xEB).unwrap();
        s.machine.bus.write_u8(0x20001, 0xFE).unwrap();
        let before = RealRegisters::from_cpu(&s.machine.cpu);
        let ip = s.registers().ip;
        let flags = s.eflags();
        s.pointer_move(42, 34);
        s.pointer_button(0, true);
        for _ in 0..200 {
            s.advance(1);
            assert_eq!(s.fault(), None);
            if s.machine.bus.read_u16(0x60100).unwrap() == 3
                && !s.mouse.callback_active
                && s.registers().cs == before.segments[Seg::Cs as usize]
                && s.registers().ip == ip
            {
                break;
            }
        }
        assert_eq!(s.machine.bus.read_u16(0x60100).unwrap(), 3);
        assert_eq!(s.machine.bus.read_u16(0x60102).unwrap(), 42);
        assert_eq!(s.machine.bus.read_u16(0x60104).unwrap(), 34);
        assert_eq!(
            RealRegisters::from_cpu(&s.machine.cpu).general,
            before.general
        );
        assert_eq!(
            RealRegisters::from_cpu(&s.machine.cpu).segments,
            before.segments
        );
        assert_eq!(s.eflags(), flags);
        assert!(!s.mouse.callback_active);
    }
}

#[test]
fn mouse_irq_invokes_registered_protected_callback_and_returns_to_client() {
    let mut s = callback_session();
    s.mouse.callback = (7, bios::SHIM_SEGMENT, CB_BASE as u16);
    s.machine.bus.write_u16(0xF0409, CB_BASE as u16).unwrap();
    s.machine
        .bus
        .write_u16(0xF040B, bios::SHIM_SEGMENT)
        .unwrap();
    s.machine.bus.write_u8(0x20000, 0xEB).unwrap();
    s.machine.bus.write_u8(0x20001, 0xFE).unwrap();
    s.machine.cpu.registers.ip = 0x20000;
    let before = RealRegisters::from_cpu(&s.machine.cpu);
    let flags = s.eflags();
    // IF=0 defers delivery without consuming the pending movement.
    s.machine.cpu.flags &= !0x200;
    s.pointer_move(42, 34);
    s.advance(20);
    assert!(!s.mouse.callback_active);
    assert_ne!(s.mouse.pending, 0);
    s.machine.cpu.flags = flags;
    for _ in 0..200 {
        s.advance(1);
        assert_eq!(s.fault(), None);
        if s.mouse.pending == 0
            && !s.mouse.callback_active
            && s.registers().cs == before.segments[Seg::Cs as usize]
            && s.registers().ip == 0x20000
        {
            break;
        }
    }
    assert_eq!(s.machine.bus.read_u32(0x3011C).unwrap(), 0x12345678);
    assert_eq!(s.machine.bus.read_u16(0x30118).unwrap(), 42);
    assert_eq!(s.machine.bus.read_u16(0x30114).unwrap(), 34);
    assert_eq!(
        RealRegisters::from_cpu(&s.machine.cpu).general,
        before.general
    );
    assert_eq!(
        RealRegisters::from_cpu(&s.machine.cpu).segments,
        before.segments
    );
    assert_eq!(s.eflags(), flags);
    assert_eq!(s.dpmi.as_ref().unwrap().depth, 0);
    assert!(!s.mouse.callback_active);
}
