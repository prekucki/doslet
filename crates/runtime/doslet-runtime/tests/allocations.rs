#![cfg(not(target_arch = "wasm32"))]
use doslet_runtime::{Session, SessionStatus};

include!("../../../../tests/fixtures/relocated_mz.rs");
include!("../../../../tests/fixtures/protected_graphics.rs");
#[test]
fn decoded_instruction_cache_misses_hits_and_ram_writes_allocate_nothing() {
    let mut program = protected_graphics_com();
    let code = [
        0xB8, 0x10, 0, 0, 0, 0x8E, 0xD8, // Load flat DS.
        0xB8, 1, 0, 0, 0, // MOV EAX,1.
        0xA3, 0, 0, 2, 0,    // MOV [20000h],EAX.
        0x40, // INC EAX.
        0xEB, 0xF8, // Repeat store and increment.
    ];
    program[0x300..0x300 + code.len()].copy_from_slice(&code);
    let mut cached = Session::new(&program).unwrap();
    let mut reference = Session::new(&program).unwrap();
    reference.set_decode_cache(false);
    cached.start();
    reference.start();
    let measured = allocation_counter::measure(|| {
        assert_eq!(cached.advance(100_000), 100_000);
    });
    assert_eq!(reference.advance(100_000), 100_000);
    assert_eq!(cached.registers(), reference.registers());
    assert_eq!(cached.eflags(), reference.eflags());
    assert_eq!(
        cached.ram_view(0x20000..0x20004),
        reference.ram_view(0x20000..0x20004)
    );
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
    #[cfg(feature = "profiling")]
    assert!(cached.decode_cache_stats().hits > 90_000);
}
#[test]
fn confirmed_vga_waits_allocate_nothing_and_keep_logical_time_and_budget() {
    // Set mode 13h and repeatedly finish/start vertical retrace.
    let program = [
        0xB8, 0x13, 0, 0xCD, 0x10, 0xBA, 0xDA, 3, 0xEC, 0x24, 8, 0x75, 0xFB, 0xEC, 0x24, 8, 0x74,
        0xFB, 0xEB, 0xF4,
    ];
    let mut fast = Session::new(&program).unwrap();
    let mut reference = Session::new(&program).unwrap();
    reference.set_vga_wait_acceleration(false);
    fast.start();
    reference.start();
    let measured = allocation_counter::measure(|| {
        assert_eq!(fast.advance(1_000_000), 1_000_000);
    });
    assert_eq!(reference.advance(1_000_000), 1_000_000);
    assert_eq!(fast.status(), SessionStatus::Running);
    assert_eq!(fast.steps(), 1_000_000);
    assert_eq!(fast.emulated_nanos(), 121_212_121);
    assert_eq!(fast.registers(), reference.registers());
    assert_eq!(fast.eflags(), reference.eflags());
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
    #[cfg(feature = "profiling")]
    assert!(fast.execution_stats().vga_wait_steps > 900_000);
}
#[test]
fn interrupt_enabled_hlt_fast_forward_allocates_nothing_and_preserves_budget() {
    let mut session = Session::new(&[0xF4, 0xEB, 0xFD]).unwrap();
    session.start();
    let measured = allocation_counter::measure(|| {
        assert_eq!(session.advance(1_000_000), 1_000_000);
    });
    assert_eq!(session.status(), SessionStatus::Running);
    assert_eq!(session.steps(), 1_000_000);
    assert_eq!(session.emulated_nanos(), 121_212_121);
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}

#[test]
fn executing_a_relocated_mz_including_console_output_allocates_nothing() {
    let mut session = Session::from_mz(&relocated_mz()).unwrap();
    session.start();
    let measured = allocation_counter::measure(|| {
        session.advance(100);
        std::hint::black_box(session.console());
    });
    assert_eq!(session.status(), SessionStatus::Exited(7));
    assert_eq!(session.console(), b"Relocated MZ");
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}

#[test]
fn reading_a_mounted_file_and_sending_midi_allocate_nothing_after_setup() {
    let mut program = vec![0; 0x100];
    let code = [
        0xBA, 0x80, 1, 0xB8, 0, 0x3D, 0xCD, 0x21, 0x89, 0xC3, 0xB9, 5, 0, 0xBA, 0x90, 1, 0xB4,
        0x3F, 0xCD, 0x21, 0xBA, 0x31, 3, 0xB0, 0x3F, 0xEE, 0xBA, 0x30, 3, 0xEC, 0xB0, 0x90, 0xEE,
        0xB0, 60, 0xEE, 0xB0, 100, 0xEE, 0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    program[..code.len()].copy_from_slice(&code);
    program[0x80..0x89].copy_from_slice(b"TEST.BIN\0");
    let mut drive = doslet_dos::files::ReadOnlyDrive::new();
    drive.mount(b"TEST.BIN", Vec::from(&b"hello"[..])).unwrap();
    let mut session = Session::new(&program).unwrap().with_drive(drive);
    session.start();
    let mut midi = [0; 16];
    let measured = allocation_counter::measure(|| {
        session.advance(100);
        assert_eq!(session.drain_midi(&mut midi), 3);
    });
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.ram_view(0x10190..0x10195).unwrap(), b"hello");
    assert_eq!(&midi[..3], &[0x90, 60, 100]);
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}

#[test]
fn dword_guest_execution_and_bounded_rep_scanout_allocate_nothing() {
    let program = [
        0xB8, 0x13, 0, 0xCD, 0x10, 0xB8, 0, 0xA0, 0x8E, 0xC0, 0x31, 0xFF, 0xB9, 0x80, 0x3E, 0x66,
        0xB8, 1, 1, 1, 1, 0xFC, 0xF3, 0x66, 0xAB, 0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    let mut session = Session::new(&program).unwrap();
    session.start();
    let mut pixels = vec![0; 320 * 200 * 4];
    let measured = allocation_counter::measure(|| {
        session.advance(20000);
        session.frame().unwrap().write_rgba(&mut pixels).unwrap();
    });
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert!(
        session
            .frame()
            .unwrap()
            .pixels
            .iter()
            .all(|byte| *byte == 1)
    );
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}

#[test]
fn protected_code_descriptor_loads_and_dword_vga_output_allocate_nothing() {
    let mut session = Session::new(&protected_graphics_com()).unwrap();
    session.start();
    let mut pixels = vec![0; 320 * 200 * 4];
    let measured = allocation_counter::measure(|| {
        session.advance(20000);
        session.frame().unwrap().write_rgba(&mut pixels).unwrap();
    });
    assert_eq!(session.status(), SessionStatus::Halted);
    assert_eq!(session.registers().cs, 8);
    assert_eq!(session.control_registers().cr0 & 1, 1);
    assert!(
        session
            .frame()
            .unwrap()
            .pixels
            .iter()
            .all(|byte| *byte == 1)
    );
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}

#[test]
fn executing_the_guest_and_borrowing_scanout_allocate_nothing_after_setup() {
    let mut session = Session::demo();
    session.start();
    let mut pixels = vec![0; 320 * 200 * 4];
    let measured = allocation_counter::measure(|| {
        session.advance(100_000);
        let frame = session.frame().unwrap();
        frame.write_rgba(&mut pixels).unwrap();
        std::hint::black_box(frame.pixels);
    });
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}

#[test]
fn banked_vbe_guest_and_full_scanout_conversion_allocate_nothing_after_setup() {
    let program = [
        0xBB, 1, 1, 0xB8, 2, 0x4F, 0xCD, 0x10, 0x31, 0xDB, 0xBA, 4, 0, 0xB8, 5, 0x4F, 0xCD, 0x10,
        0xB8, 0, 0xA0, 0x8E, 0xC0, 0x26, 0xC6, 6, 0, 0, 15, 0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    let mut session = Session::new(&program).unwrap();
    let mut pixels = vec![0; 640 * 480 * 4];
    session.start();
    let measured = allocation_counter::measure(|| {
        session.advance(100);
        session.frame().unwrap().write_rgba(&mut pixels).unwrap();
    });
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.frame().unwrap().pixels[4 * 65536], 15);
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}

#[test]
fn clocked_dma_pcm_and_hardware_irq_execution_allocate_nothing_after_setup() {
    use doslet_bus::{MemoryBus, PortBus};
    let mut machine = doslet_pc::PcMachine::default();
    machine.cpu.flags = 2;
    for (offset, byte) in [0x90, 0xEB, 0xFD].iter().enumerate() {
        machine.bus.write_u8(offset as u32, *byte).unwrap();
    }
    machine.bus.write_u16(0x10000, 0x1234).unwrap();
    machine.bus.write_port_u8(0xD8, 0).unwrap();
    for byte in [0, 0x80] {
        machine.bus.write_port_u8(0xC4, byte).unwrap();
    }
    for byte in [0, 0] {
        machine.bus.write_port_u8(0xC6, byte).unwrap();
    }
    machine.bus.write_port_u8(0x8B, 1).unwrap();
    machine.bus.write_port_u8(0xD6, 0x59).unwrap();
    machine.bus.write_port_u8(0xD4, 1).unwrap();
    // Single-unit signed mono block. The PIC sends IRQ7 to an installed IRET handler.
    machine.bus.write_u16(15 * 4, 0x200).unwrap();
    machine.bus.write_u16(15 * 4 + 2, 0).unwrap();
    machine.bus.write_u8(0x200, 0xCF).unwrap();
    machine.cpu.flags |= doslet_x86::flags::INTERRUPT;
    for byte in [0xD1, 0x41, 3, 0xE8, 0xB0, 0x10, 0, 0] {
        machine.bus.write_port_u8(0x22C, byte).unwrap();
    }
    let mut pcm = [[0; 2]; 4];
    let measured = allocation_counter::measure(|| {
        for _ in 0..10000 {
            machine.step().unwrap();
        }
        assert_eq!(machine.drain_pcm(&mut pcm), 1);
    });
    assert_eq!(pcm[0], [0x1234, 0x1234]);
    // ISR bit is set, so the CPU actually acknowledged the guest IRQ.
    machine.bus.pic.write_command(0x0B);
    assert_eq!(machine.bus.pic.read_command(), 0x80);
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}

#[test]
fn dos_read_write_overlay_and_merged_read_allocate_nothing_after_setup() {
    let mut program = vec![0; 0x120];
    let code = [
        0xBA, 0x80, 1, 0xB8, 2, 0x3D, 0xCD, 0x21, 0x89, 0xC3, 0xB8, 0, 0x42, 0x31, 0xC9, 0xBA, 2,
        0, 0xCD, 0x21, 0xBA, 0x90, 1, 0xB9, 2, 0, 0xB4, 0x40, 0xCD, 0x21, 0xB8, 0, 0x42, 0x31,
        0xC9, 0x31, 0xD2, 0xCD, 0x21, 0xBA, 0xA0, 1, 0xB9, 6, 0, 0xB4, 0x3F, 0xCD, 0x21, 0xBB, 1,
        0, 0xBA, 0xA0, 1, 0xB9, 6, 0, 0xB4, 0x40, 0xCD, 0x21, 0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    program[..code.len()].copy_from_slice(&code);
    program[0x80..0x85].copy_from_slice(b"TEST\0");
    program[0x90..0x92].copy_from_slice(b"XY");
    let mut drive = doslet_dos::files::ReadOnlyDrive::new().with_write_overlay(16);
    drive.mount(b"TEST", Vec::from(&b"abcdef"[..])).unwrap();
    let mut session = Session::new(&program).unwrap().with_drive(drive);
    session.start();
    let measured = allocation_counter::measure(|| {
        session.advance(100);
    });
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.console(), b"abXYef");
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}
