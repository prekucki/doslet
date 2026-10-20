use super::*;
use crate::SessionStatus;

fn session(program: &[u8]) -> Session {
    let mut session = Session::new(program).unwrap();
    session.set_dynamic_core(false);
    session.set_decode_cache(false);
    session.set_idle_loops(false);
    session.set_vga_wait_acceleration(false);
    session.set_synchronization_analysis(false);
    session.set_adaptive_idle(false);
    session.set_idle_observation(false);
    session.start();
    session
}

fn equivalent(actual: &Session, reference: &Session) {
    assert!(
        actual
            .machine
            .cpu
            .matches(&reference.machine.cpu.snapshot())
    );
    assert_eq!(actual.steps(), reference.steps());
    assert_eq!(actual.emulated_nanos(), reference.emulated_nanos());
    assert_eq!(
        actual.hardware_config().cpu_mhz,
        reference.hardware_config().cpu_mhz
    );
    assert_eq!(
        actual.status(),
        reference.status(),
        "actual fault={:?}; reference fault={:?}",
        actual.fault(),
        reference.fault()
    );
    assert!(
        actual.fault().is_none(),
        "guest fault: {:?}",
        actual.fault()
    );
    assert_eq!(
        actual.machine.bus.ram_view(0..0xa0000),
        reference.machine.bus.ram_view(0..0xa0000)
    );
    assert_eq!(actual.vram_view(), reference.vram_view());
    #[cfg(feature = "save-state")]
    {
        let actual = actual.save_state().unwrap();
        let reference = reference.save_state().unwrap();
        assert!(
            actual == reference,
            "saved states differ: lengths {} / {}, first difference {:?}",
            actual.len(),
            reference.len(),
            actual.iter().zip(&reference).position(|(a, b)| a != b)
        );
    }
}

fn reference(original: &Session, program: &[u8]) -> Session {
    #[cfg(feature = "save-state")]
    {
        let _ = program;
        Session::load_state(&original.save_state().unwrap()).unwrap()
    }
    #[cfg(not(feature = "save-state"))]
    {
        let _ = original;
        session(program)
    }
}

#[test]
fn suggested_wait_proves_same_value_stores_and_preserves_timer_wakeup() {
    // Poll the BIOS timer. The local holds the same value on each waiting
    // pass; IRQ0 eventually changes the watched word and the loop leaves.
    let program = [
        0xb8, 0x40, 0, // mov ax,40h
        0x8e, 0xd8, // mov ds,ax
        0xa1, 0x6c, 0, // mov ax,[6ch]
        0xa3, 0x82, 0, // mov [82h],ax (oldtime)
        0xa1, 0x6c, 0, // wait: mov ax,[6ch]
        0xa3, 0x80, 0, // mov [80h],ax (local)
        0xa1, 0x80, 0, // mov ax,[80h]
        0x3b, 0x06, 0x82, 0, // cmp ax,[82h]
        0x74, 0xf1, // je wait
        0x42, // inc dx (event observed)
        0xf4, 0xeb, 0xfd, // hlt; jmp hlt
    ];
    let mut hinted = session(&program);
    let mut reference = reference(&hinted, &program);
    for session in [&mut hinted, &mut reference] {
        session.set_cpu_mhz(1).unwrap();
        // Establish the recurring CPU flags/local value before the hint.
        assert_eq!(session.advance(9), 9);
    }
    hinted.idle.next_attempt = u64::MAX;
    for _ in 0..3 {
        let actual = hinted.debug_advance_wait_candidate(65_536, 10);
        assert_eq!(reference.advance(65_536), actual);
        equivalent(&hinted, &reference);
    }
    assert!(hinted.idle.stats.proofs != 0);
    assert!(hinted.idle.stats.skipped_steps != 0);
    assert_ne!(hinted.registers().get(doslet_x86::Gpr16::Dx), 0);
    assert!(!hinted.idle.enabled && hinted.idle.proof.is_none() && hinted.idle.proven.is_none());
    assert_eq!(
        hinted.idle.next_attempt,
        u64::MAX,
        "normal discovery state preserved"
    );
}

#[test]
fn suggested_work_loop_cannot_skip_changed_ram_and_remains_bounded() {
    let program = [0xff, 0x06, 0, 2, 0xeb, 0xfa]; // inc word [200h]; jmp
    let mut hinted = session(&program);
    let mut reference = reference(&hinted, &program);
    assert_eq!(hinted.debug_advance_wait_candidate(u32::MAX, 4), 65_536);
    assert_eq!(reference.advance(65_536), 65_536);
    equivalent(&hinted, &reference);
    assert_eq!(hinted.idle.stats.skipped_steps, 0);
    assert!(hinted.idle.stats.failures != 0);
    assert_eq!(hinted.status(), SessionStatus::Running);
    let before = hinted.steps();
    assert_eq!(hinted.debug_advance_wait_candidate(0, 0), 0);
    assert_eq!(hinted.steps(), before);
}
