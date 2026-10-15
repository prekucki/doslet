use doslet_runtime::{Session, SessionStatus};
use doslet_x86::Gpr16;

#[test]
fn multiplex_yield_returns_to_caller_and_ends_only_the_current_batch() {
    let program = [
        0xF9, // STC: IRET must restore caller flags.
        0xBB, 0x34, 0x12, // MOV BX,1234h.
        0xB8, 0x80, 0x16, 0xCD, 0x2F, // Release timeslice.
        0xA3, 0, 2, // Save result after frontend resumes us.
        0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    let mut session = Session::new(&program).unwrap();
    session.start();
    let completed = session.advance(1000);
    assert!(completed > 0 && completed < 1000);
    assert!(session.timeslice_yielded());
    assert_eq!(session.status(), SessionStatus::Running);
    assert_eq!(session.registers().ip, 0x109);
    assert_eq!(session.registers().get(Gpr16::Ax), 0x1600);
    assert_eq!(session.registers().get(Gpr16::Bx), 0x1234);
    assert_ne!(session.flags() & 1, 0);
    assert_eq!(session.ram_view(0x10200..0x10202).unwrap(), &[0, 0]);

    // Splitting the same execution into single steps must advance exactly the
    // same amount of guest time; yielding neither sleeps nor fast-forwards.
    let mut stepped = Session::new(&program).unwrap();
    stepped.start();
    for _ in 0..completed {
        assert_eq!(stepped.advance(1), 1);
    }
    assert!(stepped.timeslice_yielded());
    assert_eq!(stepped.emulated_nanos(), session.emulated_nanos());

    session.advance(1000);
    assert!(!session.timeslice_yielded());
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.ram_view(0x10200..0x10202).unwrap(), &[0, 0x16]);
}

#[test]
fn repeated_yields_make_progress_without_pausing_the_session() {
    let program = [
        0xB9, 3, 0, // MOV CX,3.
        0xB8, 0x80, 0x16, 0xCD, 0x2F, // Release timeslice.
        0xE2, 0xF9, // LOOP to MOV AX.
        0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    let mut session = Session::new(&program).unwrap();
    session.start();
    for remaining in [3, 2, 1] {
        let before = session.emulated_nanos();
        assert!(session.advance(1000) > 0);
        assert!(session.timeslice_yielded());
        assert_eq!(session.registers().get(Gpr16::Cx), remaining);
        assert_eq!(session.status(), SessionStatus::Running);
        assert!(session.emulated_nanos() > before);
    }
    session.advance(1000);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert!(!session.timeslice_yielded());
}

#[test]
fn guest_multiplex_handler_keeps_ownership_of_the_interrupt() {
    let program = [
        0xBA, 0x15, 1, // DX=0115h, guest handler.
        0xB8, 0x2F, 0x25, 0xCD, 0x21, // Install INT 2Fh vector.
        0xB8, 0x80, 0x16, 0xCD, 0x2F, 0xA3, 0, 2, 0xB8, 0, 0x4C, 0xCD, 0x21, 0xB0, 0x42,
        0xCF, // Handler: MOV AL,42h; IRET.
    ];
    let mut session = Session::new(&program).unwrap();
    session.start();
    session.advance(1000);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert!(!session.timeslice_yielded());
    assert_eq!(session.ram_view(0x10200..0x10202).unwrap(), &[0x42, 0x16]);
}
