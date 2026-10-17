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
}

#[test]
fn invalid_real_call_and_unknown_function_report_failure() {
    let mut s = prepared(Mode::Direct);
    s.machine.cpu.registers.set(Gpr16::Cx, 1);
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

#[test]
fn guest_callback_returns_through_nested_real_and_protected_frames() {
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
fn labelled_execution_retains_reflected_service_timing() {
    let mut s = prepared(Mode::Direct);
    s.set_execution_tiers(
        Some(alloc::vec::Vec::new()),
        [Some(100), Some(50), Some(25)],
    )
    .unwrap();
    simulated(&mut s, 0x16, 0x0100);
    assert_eq!(s.dpmi_stats().unwrap().direct, 0);
    assert_eq!(s.dpmi_stats().unwrap().reflected, 1);
}

#[cfg(feature = "save-state")]
#[test]
fn portable_and_working_states_reject_an_unserialized_dpmi_host() {
    let s = prepared(Mode::Direct);
    assert_eq!(
        s.save_state().err().unwrap().0,
        "save states do not yet support the experimental DPMI host"
    );
    assert_eq!(
        s.capture_working_state().err().unwrap().0,
        "save states do not yet support the experimental DPMI host"
    );
    let original = Session::new(&[0x90]).unwrap();
    let restored = Session::load_state(&original.save_state().unwrap()).unwrap();
    assert!(restored.dpmi_stats().is_none());
}
