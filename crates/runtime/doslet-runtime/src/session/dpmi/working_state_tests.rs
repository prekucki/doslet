use super::*;

fn assert_same_guest(a: &Session, b: &Session) {
    assert!(a.machine.cpu.matches(&b.machine.cpu.snapshot()));
    assert!(a.save_wire_media(false) == b.save_wire_media(false));
    assert_eq!(a.dpmi_stats(), b.dpmi_stats());
    assert!(a.dpmi_blocks().eq(b.dpmi_blocks()));
    assert_eq!(a.dpmi.as_ref().unwrap().mode, b.dpmi.as_ref().unwrap().mode);
}

#[test]
fn allocations_and_descriptors_are_independent_across_replays() {
    let mut live = prepared(Mode::Direct);
    live.machine.cpu.registers.set(Gpr16::Bx, 0);
    live.machine.cpu.registers.set(Gpr16::Cx, 4097);
    call(&mut live, 0x501);
    let block = *live.dpmi_blocks().next().unwrap();
    live.machine
        .bus
        .write_u32(block.address, 0x12345678)
        .unwrap();
    let snapshot = live.capture_working_state().unwrap();
    let mut fork = snapshot.restore().unwrap();
    assert_same_guest(&live, &fork);

    // Free a captured handle and reuse its pages only in the profiling copy.
    call(&mut fork, 0x502);
    fork.machine.cpu.registers.set(Gpr16::Bx, 0);
    fork.machine.cpu.registers.set(Gpr16::Cx, 8192);
    call(&mut fork, 0x501);
    let next = *fork.dpmi_blocks().next().unwrap();
    assert_eq!(next.address, block.address);
    assert_ne!(next.handle, block.handle);
    fork.machine
        .bus
        .write_u32(next.address, 0xDEADBEEF)
        .unwrap();
    let selector = fork.dpmi_alloc_descriptors(1).unwrap();
    fork.dpmi_descriptor_at(LDT + u32::from(selector & !7), 0x60000, 0xFFFF, 0x92, 0)
        .unwrap();
    assert!(!live.dpmi.as_ref().unwrap().used[usize::from(selector >> 3)]);
    assert_eq!(
        live.machine.bus.read_u32(block.address).unwrap(),
        0x12345678
    );
    assert_eq!(*live.dpmi_blocks().next().unwrap(), block);

    // The same snapshot can be replayed repeatedly without inheriting changes.
    let mut second = snapshot.restore().unwrap();
    assert_same_guest(&live, &second);
    assert_eq!(second.dpmi_alloc_descriptors(1).unwrap(), selector);
    call(&mut second, 0x502);
    assert_eq!(second.flags() & 1, 0);
    assert_eq!(second.dpmi_blocks().count(), 0);
    assert_eq!(live.dpmi_blocks().count(), 1);
}

#[test]
fn pending_direct_and_reflected_returns_resume_exactly() {
    for mode in [Mode::Direct, Mode::Reflect] {
        let mut live = prepared(mode);
        live.machine.bus.write_u16(0x3001C, 0x0100).unwrap();
        live.machine.bus.write_u16(0x30020, 0x202).unwrap();
        live.machine.cpu.registers.set(Gpr16::Ax, 0x300);
        live.machine.cpu.registers.set(Gpr16::Bx, 0x16);
        live.machine.cpu.registers.set(Gpr16::Cx, 0);
        live.machine.cpu.registers.set32(Gpr32::Edi, 0x30000);
        assert_eq!(live.advance(2), 2);
        let host = live.dpmi.as_ref().unwrap();
        assert_eq!(host.direct_return_pending, mode == Mode::Direct);
        assert_eq!(host.depth, usize::from(mode == Mode::Reflect));
        let mut fork = live.capture_working_state().unwrap().restore().unwrap();
        assert_same_guest(&live, &fork);
        assert_eq!(live.advance(1), 1);
        assert_eq!(fork.advance(1), 1);
        assert_same_guest(&live, &fork);
        assert_eq!(fork.registers().ip, 0x20002);
        assert_eq!(fork.dpmi.as_ref().unwrap().depth, 0);
        assert!(!fork.dpmi.as_ref().unwrap().direct_return_pending);
    }
}

#[test]
fn registered_callbacks_and_nested_frames_survive_working_capture() {
    let mut live = callback_session();
    live.machine.cpu.registers.ip = 0x20000;
    live.machine.cpu.registers.set(Gpr16::Ax, 0x302);
    let caller = live.registers().cs;
    let mut nested = false;
    for _ in 0..32 {
        // Capture each instruction boundary, including the nested protected
        // callback and both real/protected return trampolines.
        let mut fork = live.capture_working_state().unwrap().restore().unwrap();
        assert_eq!(live.advance(1), 1);
        assert_eq!(fork.advance(1), 1);
        assert_eq!(live.status(), SessionStatus::Running);
        assert_same_guest(&live, &fork);
        nested |= live.dpmi.as_ref().unwrap().depth == 2;
        if live.registers().cs == caller && live.registers().ip == 0x20002 {
            assert!(nested);
            assert_eq!(live.machine.bus.read_u32(0x3001C).unwrap(), 0x12345678);
            assert_eq!(live.dpmi.as_ref().unwrap().depth, 0);
            return;
        }
    }
    panic!("DPMI callback did not return");
}
