use doslet_runtime::{Session, SessionStatus};
use doslet_x86::Gpr16;

#[test]
fn blocking_bios_input_yields_runs_timer_and_returns_the_key() {
    let mut session = Session::new(&[
        0x31, 0xC0, 0xCD, 0x16, 0x89, 0xC3, 0xB8, 0, 0x4C, 0xCD, 0x21,
    ])
    .unwrap();
    session.start();
    assert_eq!(session.advance(1_000_000), 1_000_000);
    assert!(session.waiting_for_key());
    assert_eq!(session.status(), SessionStatus::Running);
    assert_ne!(session.ram_view(0x46C..0x470).unwrap(), &[0; 4]);
    session.push_key(0x1E, b'a').unwrap();
    session.advance(1_000_000);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.registers().get(Gpr16::Bx), 0x1E61);
}

#[test]
fn bios_poll_preserves_the_key_and_returns_saved_zero_flag() {
    let program = [
        0xB4, 1, 0xCD, 0x16, 0x74, 0x0B, 0x89, 0xC3, 0x31, 0xC0, 0xCD, 0x16, 0x89, 0xC2, 0xB8, 0,
        0x4C, 0xCD, 0x21,
    ];
    let mut session = Session::new(&program).unwrap();
    session.push_key(0x48, 0).unwrap();
    session.start();
    session.advance(100);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.registers().get(Gpr16::Bx), 0x4800);
    assert_eq!(session.registers().get(Gpr16::Dx), 0x4800);
}

#[test]
fn dos_extended_keys_return_zero_then_scan_code_and_queue_is_finite() {
    let mut session = Session::new(&[
        0xB4, 8, 0xCD, 0x21, 0x88, 0xC3, 0xB4, 8, 0xCD, 0x21, 0x88, 0xC7, 0xB8, 0, 0x4C, 0xCD, 0x21,
    ])
    .unwrap();
    for _ in 0..15 {
        assert!(session.push_key(0x48, 0).unwrap());
    }
    assert!(!session.push_key(0x50, 0).unwrap());
    session.start();
    session.advance(100);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.registers().get(Gpr16::Bx), 0x4800);
}
