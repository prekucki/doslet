use doslet_dos::files::ReadOnlyDrive;
use doslet_runtime::{Session, SessionStatus};

include!("../../../../tests/fixtures/relocated_mz.rs");
include!("../../../../tests/fixtures/midi.rs");

#[test]
fn dos_set_time_returns_status_and_get_time_observes_only_valid_changes() {
    for (cx, dx, status, expected_cx, expected_dx) in [
        (0x0C22_u16, 0x382A_u16, 0_u8, 0x0C22_u16, 0x382A_u16),
        (0x1800, 0, 0xFF, 0, 0),
        (0x003C, 0, 0xFF, 0, 0),
        (0, 0x3C00, 0xFF, 0, 0),
        (0, 100, 0xFF, 0, 0),
    ] {
        let mut code = vec![0xFA, 0xB9]; // CLI; MOV CX,time.
        code.extend_from_slice(&cx.to_le_bytes());
        code.push(0xBA);
        code.extend_from_slice(&dx.to_le_bytes());
        code.extend_from_slice(&[
            0xB8, 0, 0x2D, 0xF9, 0xCD, 0x21, // Set time with CF initially set.
            0xA3, 0, 2, 0x9C, 0x5B, // Store AX; PUSHF / POP BX.
            0xB4, 0x2C, 0xCD, 0x21, // Get time.
            0x89, 0x0E, 2, 2, 0x89, 0x16, 4, 2, // Store CX:DX.
            0xB8, 0, 0x4C, 0xCD, 0x21,
        ]);
        let mut session = Session::new(&code).unwrap();
        session.start();
        session.advance(200);
        assert_eq!(
            session.status(),
            SessionStatus::Exited(0),
            "{:?}",
            session.fault()
        );
        assert_eq!(session.ram_view(0x10200..0x10202).unwrap(), &[status, 0x2D]);
        assert_eq!(
            session.ram_view(0x10202..0x10204).unwrap(),
            &expected_cx.to_le_bytes()
        );
        assert_eq!(
            session.ram_view(0x10204..0x10206).unwrap(),
            &expected_dx.to_le_bytes()
        );
        assert_ne!(session.registers().get(doslet_x86::Gpr16::Bx) & 1, 0);
    }
}

#[test]
fn bios_mode_03h_returns_from_graphics_and_resets_text_state() {
    for mode in [3, 0x83] {
        let program = [
            0xB8, 0x13, 0, 0xCD, 0x10, // Start in graphics mode.
            0xB8, 0, 0xB8, 0x8E, 0xC0, // ES=B800h.
            0x26, 0xC7, 6, 0, 0, 0x58, 0x1E, // First page: 'X', attribute 1Eh.
            0x26, 0xC7, 6, 0xFE, 0x7F, 0x59, 0x2F, // End of eighth page.
            0xB8, 0x40, 0, 0x8E, 0xC0, // ES=BDA.
            0x26, 0xC7, 6, 0x50, 0, 0x12, 0x34, // Stale cursor position.
            0x26, 0xC6, 6, 0x62, 0, 7, // Stale active page.
            0xB8, mode, 0, 0xCD, 0x10, 0xB4, 0x0F, 0xBB, 0x34, 0xFF, 0xCD,
            0x10, // Query mode and page.
            0xA3, 0, 2, // Save AX before DOS exit.
            0xB8, 0, 0x4C, 0xCD, 0x21,
        ];
        let mut session = Session::new(&program).unwrap();
        session.start();
        session.advance(200);
        assert_eq!(session.status(), SessionStatus::Exited(0));
        assert_eq!(session.video_mode(), doslet_vga::VideoMode::Text3);
        assert!(session.frame().is_none()); // Text rendering remains pending.
        assert_eq!(session.ram_view(0x10200..0x10202).unwrap(), &[mode, 80]);
        assert_eq!(session.registers().get(doslet_x86::Gpr16::Bx), 0x34);
        assert_eq!(
            session.ram_view(0x449..0x450).unwrap(),
            &[3, 80, 0, 0, 16, 0, 0]
        );
        assert_eq!(session.ram_view(0x450..0x460).unwrap(), &[0; 16]);
        assert_eq!(session.ram_view(0x460..0x465).unwrap(), &[7, 6, 0, 0xD4, 3]);
        assert_eq!(
            session.ram_view(0x484..0x488).unwrap(),
            &[24, 16, 0, 0x60 | (mode & 0x80)]
        );
        let text = session.ram_view(0xB8000..0xC0000).unwrap();
        if mode == 3 {
            assert!(
                text.as_chunks::<2>()
                    .0
                    .iter()
                    .all(|cell| *cell == [0x20, 7])
            );
        } else {
            assert_eq!(&text[..2], &[0x58, 0x1E]);
            assert_eq!(&text[text.len() - 2..], &[0x59, 0x2F]);
        }
    }
}

#[test]
fn bios_cursor_positions_are_independent_per_page_and_queries_return_shape() {
    for page in [0, 7] {
        let program = [
            0xB8, 3, 0, 0xCD, 0x10, 0xBB, 0x45, page, 0xBA, 79, 24, 0xB8, 0, 2, 0xCD, 0x10, 0x31,
            0xD2, 0x31, 0xC9, 0xB8, 0x5A, 3, 0xCD, 0x10, 0xA3, 0, 2, // Save unchanged AX.
            0xB8, 0, 0x4C, 0xCD, 0x21,
        ];
        let mut session = Session::new(&program).unwrap();
        session.start();
        session.advance(200);
        assert_eq!(session.status(), SessionStatus::Exited(0));
        assert_eq!(session.registers().get(doslet_x86::Gpr16::Dx), 0x184F);
        assert_eq!(session.registers().get(doslet_x86::Gpr16::Cx), 0x0607);
        assert_eq!(
            session.registers().get(doslet_x86::Gpr16::Bx),
            (u16::from(page) << 8) | 0x45
        );
        assert_eq!(session.ram_view(0x10200..0x10202).unwrap(), &[0x5A, 3]);
        let cursors = session.ram_view(0x450..0x460).unwrap();
        for (index, cursor) in cursors.as_chunks::<2>().0.iter().enumerate() {
            assert_eq!(
                cursor,
                if index == usize::from(page) {
                    &[79, 24]
                } else {
                    &[0, 0]
                }
            );
        }
    }
    let mut invalid = Session::new(&[0xBB, 0, 8, 0xB4, 2, 0xCD, 0x10]).unwrap();
    invalid.start();
    invalid.advance(50);
    assert_eq!(invalid.status(), SessionStatus::Faulted);
}

#[test]
fn unavailable_legacy_video_modes_still_fault_explicitly() {
    let mut session = Session::new(&[0xB8, 4, 0, 0xCD, 0x10]).unwrap();
    session.start();
    session.advance(20);
    assert_eq!(session.status(), SessionStatus::Faulted);
}

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
fn mz_environment_describes_configured_sound_blaster_and_keeps_program_trailer() {
    for (sb16, mpu401, irq, dma8, dma16, expected) in [
        (true, true, 7, 1, 5, Some("BLASTER=A220 I7 D1 H5 T6 P330")),
        (true, false, 5, 3, 6, Some("BLASTER=A220 I5 D3 H6 T6")),
        (false, true, 7, 1, 5, None),
    ] {
        let config = doslet_config::HardwareConfig {
            sb16,
            mpu401,
            sb_irq: irq,
            sb_dma8: dma8,
            sb_dma16: dma16,
            ..Default::default()
        };
        let session =
            Session::from_mz_stub_named_configured(&relocated_mz(), b"C:\\SYNTH.EXE", config)
                .unwrap();
        let ram = session.ram_view(0x0f000..0x0f200).unwrap();
        let end = ram.windows(2).position(|v| v == [0, 0]).unwrap();
        let vars: Vec<_> = ram[..end].split(|b| *b == 0).collect();
        assert_eq!(vars[0], b"PATH=C:\\");
        assert_eq!(vars.get(1).copied(), expected.map(str::as_bytes));
        assert_eq!(&ram[end + 2..end + 4], &[1, 0]);
        assert_eq!(&ram[end + 4..end + 17], b"C:\\SYNTH.EXE\0");
        assert_eq!(session.audio_use(), Default::default());
    }
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
    let mut program = vec![
        0xBA, 0x31, 3, 0xB0, 0xFF, 0xEE, // Reset MPU.
        0xBA, 0x30, 3, 0xEC, // Consume reset acknowledgement.
        0xBA, 0x31, 3, 0xB0, 0x3F, 0xEE, // Enter UART mode.
        0xBA, 0x30, 3, 0xEC, // Consume UART acknowledgement.
    ];
    for byte in [0x90, 60, 100] {
        append_midi_byte(&mut program, byte);
    }
    program.extend_from_slice(&[0xB8, 0, 0x4C, 0xCD, 0x21]);
    let mut session = Session::new(&program).unwrap();
    session.start();
    session.advance(10000);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    let mut midi = [0; 16];
    assert_eq!(session.drain_midi(&mut midi), 3);
    assert_eq!(&midi[..3], &[0x90, 60, 100]);
    assert_eq!(session.drain_midi(&mut midi), 0);
}

#[test]
fn guest_uart_busy_survives_frequent_drains_at_different_cpu_rates() {
    let mut program = vec![0xBA, 0x31, 3, 0xB0, 0x3F, 0xEE];
    for byte in 0..64 {
        append_midi_byte(&mut program, byte);
    }
    program.extend_from_slice(&[0xB8, 0, 0x4C, 0xCD, 0x21]);
    for mhz in [1, 33] {
        let mut session = Session::new(&program).unwrap();
        session.set_cpu_mhz(mhz).unwrap();
        session.start();
        let mut received = Vec::new();
        let mut bytes = [0; 64];
        for _ in 0..10000 {
            session.advance(64);
            let count = session.drain_midi(&mut bytes);
            received.extend_from_slice(&bytes[..count]);
            if session.status() != SessionStatus::Running {
                break;
            }
        }
        assert_eq!(session.status(), SessionStatus::Exited(0));
        assert_eq!(received, (0..64).collect::<Vec<u8>>());
        assert!(session.emulated_nanos() >= 63 * 320_000);
        assert!(session.emulated_nanos() < 25_000_000);
    }
}

#[test]
fn launch_setup_exposes_psp_arguments_and_working_directory_to_dos_guest() {
    let program = [
        0x31, 0xC9, 0x8A, 0x0E, 0x80, 0, // CX = PSP command tail length.
        0xBB, 1, 0, 0xBA, 0x81, 0, 0xB4, 0x40, 0xCD, 0x21, 0xBE, 0, 3, 0x31, 0xD2, 0xB4, 0x47,
        0xCD, 0x21, // Query current directory.
        0xBB, 1, 0, 0xB9, 3, 0, 0xBA, 0, 3, 0xB4, 0x40, 0xCD, 0x21, 0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    let mut drive = doslet_dos::files::ReadOnlyDrive::new();
    drive.mount(b"SUB/FILE.DAT", Vec::new()).unwrap();
    let mut session = Session::new(&program).unwrap().with_drive(drive);
    assert!(session.configure_launch(&[b'x'; 127], b"C:\\SUB").is_err());
    session.configure_launch(b" /test", b"C:\\SUB").unwrap();
    session.start();
    assert!(session.configure_launch(b"new", b"C:\\").is_err());
    session.advance(100);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.console(), b" /testSUB");
}

#[test]
fn mscdex_installation_probe_reports_no_cdrom_drives() {
    let program = [
        0xBB, 0xFF, 0xFF, 0xB9, 0xFF, 0xFF, 0xB8, 0, 0x15, 0xCD, 0x2F,
    ];
    let mut session = Session::new(&program).unwrap();
    session.start();
    session.advance(5);
    assert_eq!(session.status(), SessionStatus::Running);
    assert_eq!(session.registers().get(doslet_x86::Gpr16::Ax), 0x1500);
    assert_eq!(session.registers().get(doslet_x86::Gpr16::Bx), 0);
    assert_eq!(session.registers().get(doslet_x86::Gpr16::Cx), 0);
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

#[test]
fn cyberman_installation_probe_returns_unsupported_without_touching_its_buffer() {
    let program = [
        0xBA, 0x40, 0x02, // DX=0240h, caller-owned static device data.
        0xB8, 0xC1, 0x53, 0xCD, 0x33, 0xA3, 0x00, 0x02, // Save status AX.
        0x89, 0x16, 0x02, 0x02, // Save preserved DX.
        0xB8, 0x00, 0x4C, 0xCD, 0x21,
    ];
    let mut session = Session::new(&program).unwrap();
    session.start();
    session.advance(100);
    assert_eq!(session.fault(), None);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(
        session.ram_view(0x10200..0x10204).unwrap(),
        &[0, 0, 0x40, 2]
    );
    assert_eq!(session.ram_view(0x10240..0x1024A).unwrap(), &[0; 10]);
}

#[test]
fn find_first_next_use_the_dta_and_return_dos_error_flags() {
    let mut program = vec![
        0xBA, 0, 4, 0xB4, 0x1A, 0xCD, 0x21, // DTA=1000:0400.
        0x31, 0xC9, 0xBA, 0, 3, 0xB8, 0xFC, 0x4E, 0xF9, 0xCD, 0x21, 0x9C, 0x5B, 0x89, 0x1E, 0,
        5, // Save flags after first search.
        0xBE, 0, 4, 0xBF, 0, 6, 0xB9, 43, 0, 0xF3, 0xA4, // Preserve first complete DTA.
        0xB8, 0xFC, 0x4F, 0xCD, 0x21, 0x9C, 0x5B, 0x89, 0x1E, 2, 5, 0xBE, 0, 4, 0xBF, 0, 7, 0xB9,
        43, 0, 0xF3, 0xA4, 0xB8, 0xFC, 0x4F, 0xCD, 0x21, 0xA3, 4, 5, 0x9C, 0x5B, 0x89, 0x1E, 6, 5,
        0xBA, 16, 3, 0xB8, 0xFC, 0x4E, 0xCD, 0x21, 0xA3, 8, 5, 0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    program.resize(0x200, 0);
    program.extend_from_slice(b"*.SAV\0");
    program.resize(0x210, 0);
    program.extend_from_slice(b"MISSING.*\0");
    let mut drive = ReadOnlyDrive::new();
    drive.mount(b"ONE.SAV", vec![42; 3]).unwrap();
    drive.mount(b"TWO.SAV", vec![42; 4]).unwrap();
    let mut session = Session::new(&program).unwrap().with_drive(drive);
    session.start();
    session.advance(1000);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.fault(), None);
    let flags = session.ram_view(0x10500..0x1050A).unwrap();
    assert_eq!(flags[0] & 1, 0);
    assert_eq!(flags[2] & 1, 0);
    assert_eq!(&flags[4..6], &18u16.to_le_bytes());
    assert_eq!(flags[6] & 1, 1);
    assert_eq!(&flags[8..10], &2u16.to_le_bytes());
    for (base, name, length) in [
        (0x10600, b"ONE.SAV\0".as_slice(), 3u32),
        (0x10700, b"TWO.SAV\0", 4),
    ] {
        let result = session.ram_view(base..base + 43).unwrap();
        assert_eq!(result[21], 0x20);
        assert_eq!(&result[22..24], &0x6000u16.to_le_bytes());
        assert_eq!(&result[26..30], &length.to_le_bytes());
        assert_eq!(&result[30..38], name);
    }
}
