use doslet_runtime::{Session, SessionStatus};
use doslet_x86::Gpr16;
include!("../../../../tests/fixtures/iso9660.rs");

#[test]
fn dos_disk_space_reports_mounted_cd_as_full_without_write_space() {
    let program = [0xBA, 4, 0, 0xB8, 3, 0x36, 0xCD, 0x21, 0xEB, 0xFE];
    let image = test_iso();
    let sectors = image.len() / 512;
    let mut session = Session::new(&program)
        .unwrap()
        .with_cdrom_image(b'D', image)
        .unwrap();
    session.start();
    session.advance(4);
    assert_eq!(session.registers().get(Gpr16::Ax), 1);
    assert_eq!(session.registers().get(Gpr16::Bx), 0);
    assert_eq!(session.registers().get(Gpr16::Cx), 512);
    assert_eq!(usize::from(session.registers().get(Gpr16::Dx)), sectors);
}

#[test]
fn mscdex_reports_the_mounted_drive_and_bounds_sector_reads() {
    let program = [
        0xB8, 0, 0x15, 0xCD, 0x2F, 0xB9, 3, 0, 0xB8, 0x0B, 0x15, 0xCD, 0x2F, 0xB9, 3, 0, 0xBB, 0,
        4, 0xBA, 1, 0, 0x31, 0xF6, 0xBF, 23, 0, 0xB8, 8, 0x15, 0xCD, 0x2F, 0xBF, 25, 0, 0xB8, 8,
        0x15, 0xCD, 0x2F, 0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    let mut session = Session::new(&program)
        .unwrap()
        .with_cdrom_image(b'D', test_iso())
        .unwrap();
    session.start();
    session.advance(3);
    assert_eq!(session.registers().get(Gpr16::Ax), 0x15FF);
    assert_eq!(session.registers().get(Gpr16::Bx), 1);
    assert_eq!(session.registers().get(Gpr16::Cx), 3);
    session.advance(4);
    assert_eq!(session.registers().get(Gpr16::Ax), 0x5AD8);
    assert_eq!(session.registers().get(Gpr16::Bx), 0xADAD);
    session.advance(8);
    assert_eq!(session.registers().get(Gpr16::Ax), 0);
    assert_eq!(session.flags() & 1, 0);
    assert_eq!(session.ram_view(0x10400..0x10404).unwrap(), b"data");
    assert_eq!(session.disk_read_bytes(), 2048);
    session.advance(4);
    assert_eq!(session.registers().get(Gpr16::Ax), 15);
    assert_eq!(session.flags() & 1, 1);
    assert_eq!(session.disk_read_bytes(), 2048);
    assert_eq!(session.ram_view(0x10400..0x10404).unwrap(), b"data");
    session.advance(3);
    assert_eq!(session.status(), SessionStatus::Exited(0));
}

#[test]
fn cdrom_dos_handle_reports_its_drive_and_refuses_writes() {
    let mut program = vec![0; 0x130];
    let code = [
        0xBA, 0, 2, 0xB8, 0, 0x3D, 0xCD, 0x21, 0x89, 0xC3, 0xB8, 0, 0x44, 0xCD, 0x21, 0xBA, 0, 3,
        0xB9, 1, 0, 0xB4, 0x40, 0xCD, 0x21, 0xB4, 0x3E, 0xCD, 0x21,
    ];
    program[..code.len()].copy_from_slice(&code);
    let name = b"D:\\HEROES\\SOUND\\HEROES01.62S\0";
    program[0x100..0x100 + name.len()].copy_from_slice(name);
    let mut session = Session::new(&program)
        .unwrap()
        .with_cdrom_image(b'D', test_iso())
        .unwrap();
    session.start();
    session.advance(8);
    assert_eq!(session.registers().get(Gpr16::Dx), 3);
    assert_eq!(session.flags() & 1, 0);
    session.advance(5);
    assert_eq!(session.registers().get(Gpr16::Ax), 5);
    assert_eq!(session.flags() & 1, 1);
    session.advance(3);
    assert_eq!(session.flags() & 1, 0);
}
