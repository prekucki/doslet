use doslet_runtime::{Session, SessionStatus};

include!("../../../../tests/fixtures/relocated_mz.rs");

#[test]
fn a_relocated_guest_reads_its_data_and_exits_through_dos() {
    let mut session = Session::from_mz(&relocated_mz()).unwrap();
    session.start();
    assert_eq!(session.advance(3), 3);
    assert_eq!(session.status(), SessionStatus::Running);
    session.pause();
    assert_eq!(session.advance(100), 0);
    session.single_step();
    assert_eq!(session.steps(), 4);
    assert_eq!(session.status(), SessionStatus::Paused);
    session.start();
    session.advance(100);
    assert_eq!(session.status(), SessionStatus::Exited(7));
    assert_eq!(session.console(), b"Relocated MZ");
    assert_eq!(session.steps(), 9);
}

#[test]
fn unsupported_mz_instructions_report_cpu_faults() {
    let mut file = relocated_mz();
    file[32] = 0x0F;
    let mut session = Session::from_mz(&file).unwrap();
    session.start();
    session.advance(100);
    assert_eq!(session.status(), SessionStatus::Faulted);
    assert!(
        session
            .fault()
            .unwrap()
            .to_string()
            .contains("unsupported opcode")
    );
}
