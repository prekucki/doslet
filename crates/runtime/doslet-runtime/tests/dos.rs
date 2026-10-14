use doslet_dos::files::ReadOnlyDrive;
use doslet_runtime::{Session, SessionStatus};

include!("../../../../tests/fixtures/relocated_mz.rs");

#[test]
fn bios_timer_invokes_replaced_int_1c_hook_and_restores_ax_after_eoi() {
    let mut program = vec![0; 0x210];
    let code = [
        0xBA, 0, 3, 0xB8, 0x1C, 0x25, 0xCD, 0x21, 0xB8, 0xFE, 0xCA, 0xCD, 8, 0x89, 0xC3, 0xB8, 0,
        0x4C, 0xCD, 0x21,
    ];
    program[..code.len()].copy_from_slice(&code);
    program[0x200..0x207].copy_from_slice(&[0xFF, 6, 0, 2, 0x31, 0xC0, 0xCF]);
    let mut session = Session::new(&program).unwrap();
    session.start();
    session.advance(100);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.ram_view(0x10200..0x10202).unwrap(), &[1, 0]);
    assert_eq!(session.registers().get(doslet_x86::Gpr16::Bx), 0xCAFE);
    assert_eq!(session.ram_view(0x46C..0x470).unwrap(), &[1, 0, 0, 0]);
}

#[test]
fn vbe_bios_exposes_real_mode_metadata_and_banked_guest_pixels() {
    let program = [
        0xBF, 0, 2, 0xB8, 0, 0x4F, 0xCD, 0x10, // Controller at ES:200h.
        0xBF, 0, 3, 0xB9, 1, 1, 0xB8, 1, 0x4F, 0xCD, 0x10, // Mode info.
        0xBB, 1, 1, 0xB8, 2, 0x4F, 0xCD, 0x10, // Set mode 101h.
        0xB8, 0, 0xA0, 0x8E, 0xC0, 0x26, 0xC6, 6, 0, 0, 7, // Bank 0 pixel.
        0x31, 0xDB, 0xBA, 1, 0, 0xB8, 5, 0x4F, 0xCD, 0x10, // Set bank 1.
        0x26, 0xC6, 6, 0, 0, 9, // Distinct bank 1 pixel.
        0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    let mut session = Session::new(&program).unwrap();
    session.start();
    session.advance(100);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    let controller = session.ram_view(0x10200..0x10300).unwrap();
    assert_eq!(&controller[..6], b"VESA\x02\x01");
    assert_eq!(&controller[18..20], &[8, 0]);
    let info = session.ram_view(0x10300..0x10400).unwrap();
    assert_eq!(&info[4..10], &[64, 0, 64, 0, 0, 0xA0]);
    assert_eq!(&info[16..22], &[0x80, 2, 0x80, 2, 0xE0, 1]);
    assert_eq!(info[25], 8);
    let frame = session.frame().unwrap();
    assert_eq!((frame.width, frame.height), (640, 480));
    assert_eq!(frame.pixels[0], 7);
    assert_eq!(frame.pixels[65536], 9);
}

#[test]
fn dos_directory_change_query_and_relative_open_share_the_same_state() {
    let mut program = vec![0; 0x100];
    let code = [
        0xB2, 2, 0xB4, 0x0E, 0xCD, 0x21, // Select C:.
        0xBA, 0x80, 1, 0xB4, 0x3B, 0xCD, 0x21, // CHDIR DIR.
        0xB2, 0, 0xBE, 0xA0, 1, 0xB4, 0x47, 0xCD, 0x21, // Query default drive.
        0xBA, 0x90, 1, 0xB8, 0, 0x3D, 0xCD, 0x21, // Open relative file.
        0x89, 0xC3, 0xBA, 0xB0, 1, 0xB9, 1, 0, 0xB4, 0x3F, 0xCD, 0x21, 0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    program[..code.len()].copy_from_slice(&code);
    program[0x80..0x84].copy_from_slice(b"DIR\0");
    program[0x90..0x99].copy_from_slice(b"TEST.DAT\0");
    let mut drive = ReadOnlyDrive::new();
    drive.mount(b"DIR/TEST.DAT", Vec::from(&b"X"[..])).unwrap();
    let mut session = Session::new(&program).unwrap().with_drive(drive);
    session.start();
    session.advance(4);
    assert_eq!(session.registers().get(doslet_x86::Gpr16::Ax), 0x0E03);
    session.advance(100);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.ram_view(0x101A0..0x101A4).unwrap(), b"DIR\0");
    assert_eq!(session.ram_view(0x101B0..0x101B1).unwrap(), b"X");
}

#[test]
fn a_guest_opens_reads_and_writes_a_mounted_file_to_stdout() {
    let mut program = vec![0; 0x100];
    let code = [
        0xBA, 0x80, 1, 0xB8, 0, 0x3D, 0xCD, 0x21, 0x89, 0xC3, 0xB9, 5, 0, 0xBA, 0x90, 1, 0xB4,
        0x3F, 0xCD, 0x21, 0xB4, 0x3E, 0xCD, 0x21, 0xBB, 1, 0, 0xB9, 5, 0, 0xB4, 0x40, 0xCD, 0x21,
        0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    program[..code.len()].copy_from_slice(&code);
    program[0x80..0x89].copy_from_slice(b"TEST.BIN\0");
    let mut drive = ReadOnlyDrive::new();
    drive
        .mount(b"TEST.BIN", Vec::from(&b"hello world"[..]))
        .unwrap();
    let mut session = Session::new(&program).unwrap().with_drive(drive);
    session.start();
    session.advance(100);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.console(), b"hello");
}

#[test]
fn memory_service_results_survive_iret_and_mcb_sizes_are_visible() {
    let program = [
        0xBC, 0xFE, 0x0F, // Move stack into the retained process block.
        0xBB, 0, 1, 0xB4, 0x4A, 0xCD, 0x21, 0xBB, 0x10, 0, 0xB4, 0x48, 0xCD, 0x21, 0x72,
        0x0E, // JC -> error exit.
        0x8E, 0xC0, 0xB4, 0x49, 0xCD, 0x21, 0x72, 6, // JC -> error exit.
        0xB8, 0, 0x4C, 0xCD, 0x21, 0x90, 0xB8, 1, 0x4C, 0xCD, 0x21,
    ];
    let mut session = Session::new(&program).unwrap();
    session.start();
    session.advance(100);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.ram_view(0x10002..0x10004).unwrap(), &[0, 0x11]);
    assert_eq!(session.ram_view(0x0FFF3..0x0FFF5).unwrap(), &[0, 1]);
}

#[test]
fn exec_overlay_runs_a_preloaded_mz_without_a_new_process() {
    let mut program = vec![0; 0x100];
    let code = [
        0xBA, 0x90, 1, 0xBB, 0x80, 1, 0xB8, 3, 0x4B, 0xCD, 0x21, 0xEA, 0, 0, 0, 0x11,
    ];
    program[..code.len()].copy_from_slice(&code);
    program[0x80..0x84].copy_from_slice(&[0, 0x11, 0, 0x11]);
    program[0x90..0x9A].copy_from_slice(b"CHILD.EXE\0");
    let mut drive = ReadOnlyDrive::new();
    drive
        .mount(b"CHILD.EXE", Vec::from(relocated_mz()))
        .unwrap();
    let mut session = Session::new(&program).unwrap().with_drive(drive);
    session.start();
    session.advance(100);
    assert_eq!(session.loaded_overlays(), 1);
    assert_eq!(session.status(), SessionStatus::Exited(7));
    assert_eq!(session.console(), b"Relocated MZ");
}

#[test]
fn executing_an_extended_files_dos_stub_does_not_execute_its_le_image() {
    let original = relocated_mz();
    let mut file = [0; 240];
    file[..28].copy_from_slice(&original[..28]);
    file[2..4].copy_from_slice(&208u16.to_le_bytes());
    file[8..10].copy_from_slice(&5u16.to_le_bytes());
    file[24..26].copy_from_slice(&64u16.to_le_bytes());
    file[60..64].copy_from_slice(&208u32.to_le_bytes());
    file[64..68].copy_from_slice(&original[28..32]);
    file[80..208].copy_from_slice(&original[32..]);
    file[208..210].copy_from_slice(b"LE");
    assert!(Session::from_mz(&file).is_err());
    let mut session = Session::from_mz_stub_named(&file, b"C:\\SYNTH.EXE").unwrap();
    session.start();
    session.advance(100);
    assert_eq!(session.status(), SessionStatus::Exited(7));
    assert_eq!(session.console(), b"Relocated MZ");
    assert_eq!(session.ram_view(0x1002C..0x1002E).unwrap(), &[0, 0x0F]);
}

#[test]
fn guest_uart_port_output_reaches_the_sessions_midi_queue() {
    let program = [
        0xBA, 0x31, 3, 0xB0, 0xFF, 0xEE, // Reset MPU.
        0xBA, 0x30, 3, 0xEC, // Consume reset acknowledgement.
        0xBA, 0x31, 3, 0xB0, 0x3F, 0xEE, // Enter UART mode.
        0xBA, 0x30, 3, 0xEC, 0xB0, 0x90, 0xEE, 0xB0, 60, 0xEE, 0xB0, 100, 0xEE, 0xB8, 0, 0x4C,
        0xCD, 0x21,
    ];
    let mut session = Session::new(&program).unwrap();
    session.start();
    session.advance(100);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    let mut midi = [0; 16];
    assert_eq!(session.drain_midi(&mut midi), 3);
    assert_eq!(&midi[..3], &[0x90, 60, 100]);
    assert_eq!(session.drain_midi(&mut midi), 0);
}

#[test]
fn absent_extender_services_return_their_probe_status_without_running_null_vectors() {
    let program = [
        0xB8, 0x87, 0x16, 0xCD, 0x2F, 0xB8, 0, 0x43, 0xCD, 0x2F, 0xB8, 0, 0xDE, 0xCD, 0x67, 0xB8,
        0xDE, 0xBF, 0xCD, 0x15, 0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    let mut session = Session::new(&program).unwrap();
    session.start();
    for expected in [1, 0x4300, 0x8400, 0x86DE] {
        session.advance(3);
        assert_eq!(session.registers().get(doslet_x86::Gpr16::Ax), expected);
        assert_eq!(session.status(), SessionStatus::Running);
    }
    assert_eq!(session.flags() & 1, 1);
    session.advance(3);
    assert_eq!(session.status(), SessionStatus::Exited(0));
}

#[test]
fn bios_a20_and_extended_memory_match_the_actual_pc_bus() {
    let program = [
        0xB8, 1, 0x24, 0xCD, 0x15, 0xB4, 0x88, 0xCD, 0x15, 0xB8, 2, 0x24, 0xCD, 0x15,
    ];
    let mut session = Session::new(&program).unwrap();
    session.start();
    session.advance(3);
    assert!(session.a20_enabled());
    assert_eq!(session.flags() & 1, 0);
    session.advance(3);
    assert_eq!(session.registers().get(doslet_x86::Gpr16::Ax), 15360);
    session.advance(3);
    assert_eq!(session.registers().get(doslet_x86::Gpr16::Ax), 1);
}

#[test]
fn country_record_callback_is_executable_guest_code() {
    let program = [
        0xBA, 0x80, 1, 0xB8, 0, 0x38, 0xCD, 0x21, 0xB0, b'q', 0xFF, 0x1E, 0x92, 1, 0xB4, 0x4C,
        0xCD, 0x21,
    ];
    let mut session = Session::new(&program).unwrap();
    session.start();
    session.advance(100);
    assert_eq!(session.status(), SessionStatus::Exited(b'Q'));
    assert_eq!(session.ram_view(0x10182..0x10183).unwrap(), b"$");
}

#[test]
fn irq0_vector_is_initialized_and_updates_the_bios_tick_count() {
    let mut session = Session::new(&[0xCD, 8, 0xB8, 0, 0x4C, 0xCD, 0x21]).unwrap();
    assert_ne!(session.ram_view(0x20..0x24).unwrap(), &[0; 4]);
    session.start();
    session.advance(100);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.ram_view(0x46C..0x470).unwrap(), &[1, 0, 0, 0]);
}
