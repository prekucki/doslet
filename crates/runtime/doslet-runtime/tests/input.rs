use doslet_config::{HardwareConfig, KeyboardMode};
use doslet_input::PhysicalKey;
use doslet_runtime::{Session, SessionStatus};
use doslet_x86::Gpr16;
fn keyboard_config() -> HardwareConfig {
    HardwareConfig {
        keyboard: KeyboardMode::At,
        ..Default::default()
    }
}
#[test]
fn hardware_key_reaches_bios_only_through_irq_handler() {
    let mut session = Session::new_configured(
        &[
            0x31, 0xC0, 0xCD, 0x16, 0x89, 0xC3, 0xB8, 0, 0x4C, 0xCD, 0x21,
        ],
        keyboard_config(),
    )
    .unwrap();
    session.key_event(PhysicalKey(4), true).unwrap();
    assert_eq!(session.ram_view(0x41A..0x41E).unwrap(), &[0x1E, 0, 0x1E, 0]);
    session.start();
    session.advance(10000);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.registers().get(Gpr16::Bx), 0x1E61);
}
#[test]
fn hardware_alt_o_reaches_bios_and_alt_release_clears_modifier() {
    for alt in [226, 230] {
        let mut session = Session::new_configured(
            &[0x31, 0xC0, 0xCD, 0x16, 0xA3, 0, 2, 0xEB, 0xFE],
            keyboard_config(),
        )
        .unwrap();
        session.key_event(PhysicalKey(alt), true).unwrap();
        session.key_event(PhysicalKey(18), true).unwrap();
        session.start();
        session.advance(10000);
        assert_eq!(session.status(), SessionStatus::Running);
        assert_eq!(session.ram_view(0x10200..0x10202).unwrap(), &[0, 0x18]);
        assert_eq!(session.ram_view(0x417..0x418).unwrap()[0] & 8, 8);
        session.key_event(PhysicalKey(18), false).unwrap();
        session.key_event(PhysicalKey(alt), false).unwrap();
        session.advance(10000);
        assert_eq!(session.ram_view(0x417..0x418).unwrap()[0] & 8, 0);
    }
}
#[test]
fn hardware_shift_and_key_release_update_bda_without_duplicate_bios_injection() {
    let mut session = Session::new_configured(&[0xFB, 0xEB, 0xFD], keyboard_config()).unwrap();
    session.key_event(PhysicalKey(225), true).unwrap();
    session.key_event(PhysicalKey(4), true).unwrap();
    session.start();
    session.advance(1000);
    let ring: [u8; 4] = session.ram_view(0x41A..0x41E).unwrap().try_into().unwrap();
    assert_eq!(ring, [0x1E, 0, 0x20, 0]);
    assert_eq!(session.ram_view(0x41E..0x420).unwrap(), &[b'A', 0x1E]);
    assert_eq!(session.ram_view(0x417..0x418).unwrap()[0] & 2, 2);
    session.key_event(PhysicalKey(225), false).unwrap();
    session.key_event(PhysicalKey(4), false).unwrap();
    session.advance(1000);
    assert_eq!(session.ram_view(0x417..0x418).unwrap()[0] & 2, 0);
    assert_eq!(session.ram_view(0x41A..0x41E).unwrap(), &ring);
}
#[test]
fn guest_replacement_irq1_receives_raw_scan_without_filling_bios_ring() {
    let mut program = vec![0; 0x110];
    let code = [0xBA, 0, 2, 0xB8, 9, 0x25, 0xCD, 0x21, 0xFB, 0xEB, 0xFE];
    program[..code.len()].copy_from_slice(&code);
    let handler = [
        0x50, 0xE4, 0x60, 0xA2, 0, 3, 0xB0, 0x20, 0xE6, 0x20, 0x58, 0xCF,
    ];
    program[0x100..0x100 + handler.len()].copy_from_slice(&handler);
    let mut session = Session::new_configured(&program, keyboard_config()).unwrap();
    session.start();
    session.advance(100);
    session.key_event(PhysicalKey(4), true).unwrap();
    session.advance(1000);
    assert_eq!(session.ram_view(0x10300..0x10301).unwrap(), &[0x1E]);
    assert_eq!(session.ram_view(0x41A..0x41E).unwrap(), &[0x1E, 0, 0x1E, 0]);
}
