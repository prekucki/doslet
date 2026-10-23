#![cfg(all(feature = "v86", not(target_arch = "wasm32")))]
use doslet_runtime::{Session, SessionStatus};

#[test]
fn disabled_gameport_probe_matches_between_v86_and_interpreter() {
    // Trigger an axis conversion, then poll the absent device sixteen times.
    let program = [
        0xba, 0x01, 0x02, // MOV DX,0201h
        0xb9, 0x10, 0x00, // MOV CX,16
        0xbb, 0x00, 0x03, // MOV BX,0300h
        0x30, 0xc0, 0xee, // XOR AL,AL; OUT DX,AL
        0xec, 0x88, 0x07, 0x43, 0xe2, 0xfa, // IN AL,DX; MOV [BX],AL; INC BX; LOOP
        0xb8, 0x00, 0x4c, 0xcd, 0x21, // exit
    ];
    for v86 in [false, true] {
        let mut session = Session::new_configured(
            &program,
            doslet_config::HardwareConfig {
                gameport: false,
                ..Default::default()
            },
        )
        .unwrap();
        if v86 {
            session.enable_v86(&[]).unwrap();
            session.set_v86_host_clock(true).unwrap();
        }
        session.start();
        for _ in 0..128 {
            session.advance(4096);
            if session.status() != SessionStatus::Running {
                break;
            }
            if v86 {
                session
                    .advance_v86_host_time(session.emulated_nanos() + 100_000)
                    .unwrap();
            }
        }
        assert_eq!(
            session.status(),
            SessionStatus::Exited(0),
            "v86={v86}: {:?}",
            session.fault()
        );
        assert_eq!(session.ram_view(0x10300..0x10310).unwrap(), &[0xff; 16]);
    }
}

#[test]
fn extended_keyboard_sequences_match_between_v86_and_interpreter() {
    use doslet_config::{HardwareConfig, KeyboardMode};
    // IRQ1 reads exactly one byte per entry, like a game's keyboard ISR.
    let mut program = vec![0; 0x120];
    let code = [
        0xba, 0, 2, 0xb8, 9, 0x25, 0xcd, 0x21, // install handler at 0200h
        0xbb, 0, 3, // BX = output cursor
        0xb0, 0xfd, 0xe6, 0x21, // unmask only IRQ1
        0xfb, 0xf4, 0xeb, 0xfd, // STI; HLT; loop
    ];
    program[..code.len()].copy_from_slice(&code);
    let handler = [
        0x50, 0xe4, 0x60, 0x88, 0x07, 0x43, 0xb0, 0x20, 0xe6, 0x20, 0x58, 0xcf,
    ];
    program[0x100..0x100 + handler.len()].copy_from_slice(&handler);
    for v86 in [false, true] {
        let mut session = Session::new_configured(
            &program,
            HardwareConfig {
                keyboard: KeyboardMode::At,
                ..Default::default()
            },
        )
        .unwrap();
        if v86 {
            session.enable_v86(&[]).unwrap();
            session.set_v86_host_clock(true).unwrap();
        }
        session.start();
        session.advance(4096);
        let mut expected = Vec::new();
        for (usage, make) in [
            (20, &[0x10][..]),
            (41, &[0x01]),
            (79, &[0xe0, 0x4d]),
            (80, &[0xe0, 0x4b]),
            (81, &[0xe0, 0x50]),
            (82, &[0xe0, 0x48]),
        ] {
            for pressed in [true, false] {
                let mut bytes = make.to_vec();
                if !pressed {
                    *bytes.last_mut().unwrap() |= 0x80;
                }
                expected.extend_from_slice(&bytes);
                session.physical_key(usage, pressed).unwrap();
                for _ in 0..32 {
                    if v86 {
                        session
                            .advance_v86_host_time(session.emulated_nanos() + 100_000)
                            .unwrap();
                    }
                    session.advance(4096);
                    if session.ram_view(0x10300..0x10300 + expected.len()).unwrap() == expected {
                        break;
                    }
                }
                assert_eq!(
                    session.status(),
                    SessionStatus::Running,
                    "{:?}",
                    session.fault()
                );
                assert_eq!(
                    session.ram_view(0x10300..0x10300 + expected.len()).unwrap(),
                    expected,
                    "v86={v86}, usage={usage}, pressed={pressed}"
                );
            }
        }
    }
}

#[test]
fn v86_host_clock_accepts_keyboard_input_while_cpu_waits_for_irq() {
    use doslet_config::{HardwareConfig, KeyboardMode};
    // Install IRQ1, mask IRQ0, then wait in HLT. No timer may rescue input
    // that is accidentally left in the owner's pending event queue.
    let mut program = vec![0; 0x110];
    let code = [
        0xba, 0, 2, 0xb8, 9, 0x25, 0xcd, 0x21, 0xb0, 0xfd, 0xe6, 0x21, 0xfb, 0xf4, 0xeb, 0xfd,
    ];
    program[..code.len()].copy_from_slice(&code);
    let handler = [
        0x50, 0xe4, 0x60, 0xa2, 0, 3, 0xb0, 0x20, 0xe6, 0x20, 0x58, 0xcf,
    ];
    program[0x100..0x100 + handler.len()].copy_from_slice(&handler);
    let mut session = Session::new_configured(
        &program,
        HardwareConfig {
            keyboard: KeyboardMode::At,
            ..Default::default()
        },
    )
    .unwrap();
    session.enable_v86(&[]).unwrap();
    session.set_v86_host_clock(true).unwrap();
    session.start();
    for _ in 0..32 {
        session.advance(4096);
        if session.debug_cpu().is_halted() {
            break;
        }
    }
    assert!(session.debug_cpu().is_halted());
    for (pressed, scan) in [(true, 0x1e), (false, 0x9e)] {
        session.physical_key(4, pressed).unwrap();
        // The host advances devices while the CPU sleeps, without executing it.
        for _ in 0..4 {
            session
                .advance_v86_host_time(session.emulated_nanos() + 1_000_000)
                .unwrap();
            if session.debug_irq_pending() {
                break;
            }
        }
        assert!(
            session.debug_irq_pending(),
            "queued key must reach IRQ1 without CPU execution"
        );
        for _ in 0..32 {
            session.advance(4096);
            if session.debug_cpu().is_halted() {
                break;
            }
        }
        assert_eq!(
            session.status(),
            SessionStatus::Running,
            "{:?}",
            session.fault()
        );
        assert_eq!(session.ram_view(0x10300..0x10301).unwrap(), &[scan]);
        assert!(session.debug_cpu().is_halted());
    }
}

#[test]
fn v86_disables_incompatible_policies_even_when_a_profile_reapplies_them() {
    let mut session = Session::new(&[0xeb, 0xfe]).unwrap();
    session.set_adaptive_cpu(true);
    session.enable_v86(&[]).unwrap();
    assert!(!session.idle_observation().enabled);
    assert!(!session.synchronization_analysis().enabled);
    session.set_idle_loops(true);
    session.set_dynamic_core(true);
    session.set_synchronization_analysis(true);
    session.set_adaptive_cpu(true);
    session.set_adaptive_idle(true);
    session.set_idle_observation(true);
    assert!(!session.dynamic_core_enabled());
    assert!(!session.adaptive_idle_enabled());
    assert!(!session.idle_observation().enabled);
    assert!(!session.synchronization_analysis().enabled);
    assert!(!session.synchronization_analysis().controller_enabled);
    session.start();
    session.advance(100_000);
    assert_eq!(
        session.status(),
        SessionStatus::Running,
        "{:?}",
        session.fault()
    );
}

#[test]
fn host_clock_cpu_work_does_not_advance_devices_and_time_wakes_irq_halt() {
    let mut session = Session::new(&[0xfb, 0xf4, 0xeb, 0xfd, 0x90, 0x90, 0x43, 0xcf]).unwrap();
    assert!(session.set_v86_host_clock(true).is_err());
    session.enable_v86(&[]).unwrap();
    session.set_v86_host_clock(true).unwrap();
    // IRQ0 -> guest INC BX; IRET. Keep timer/keyboard BIOS work out of this test.
    assert!(session.debug_write_memory(8 * 4, &[6, 1, 0, 0x10]));
    session.start();
    let initial = session.emulated_nanos();
    assert!(session.advance(65_536) < 65_536);
    assert_eq!(session.status(), SessionStatus::Running);
    assert!(session.timeslice_yielded());
    assert_eq!(session.emulated_nanos(), initial);
    let before = session.debug_irq_entries();
    for ms in 1..=60 {
        session
            .advance_v86_host_time(initial + ms * 1_000_000)
            .unwrap();
        session.advance(65_536);
        assert_eq!(
            session.status(),
            SessionStatus::Running,
            "{:?}",
            session.fault()
        );
    }
    assert_eq!(session.emulated_nanos(), initial + 60_000_000);
    assert!(session.debug_irq_entries() > before);
    assert_ne!(session.debug_cpu().registers.get(doslet_x86::Gpr16::Bx), 0);
    session.pause();
    assert_eq!(
        session
            .advance_v86_host_time(initial + 100_000_000)
            .unwrap(),
        0
    );
    session.start();
    assert_eq!(session.advance_v86_host_time(0).unwrap(), 0);
}

#[test]
fn host_clock_rep_copy_returns_to_owner_before_consuming_the_instruction_grant() {
    // ES=A000h; DI=0; CX=32000; AX=1234h; REP STOSW; endless loop.
    let program = [
        0xb8, 0, 0xa0, 0x8e, 0xc0, 0x31, 0xff, 0xb9, 0, 0x7d, 0xb8, 0x34, 0x12, 0xf3, 0xab, 0xeb,
        0xfe,
    ];
    let mut session = Session::new(&program).unwrap();
    session.enable_v86(&[]).unwrap();
    session.set_v86_host_clock(true).unwrap();
    session.start();
    let initial = session.emulated_nanos();
    let completed = session.advance(65_536);
    assert_eq!(
        session.status(),
        SessionStatus::Running,
        "{:?}",
        session.fault()
    );
    assert!(completed <= 4096, "unbounded host-clock batch: {completed}");
    assert_eq!(session.emulated_nanos(), initial);
    assert!(session.debug_cpu().registers.get(doslet_x86::Gpr16::Cx) >= 27_000);
    assert!(session.advance_v86_host_time(initial + 1_000_000).unwrap() > 0);
}

#[test]
fn host_clock_many_segment_boundaries_return_to_owner_before_the_instruction_limit() {
    // MOV AX,1000h; MOV ES,AX; JMP back to MOV ES,AX.
    let mut session = Session::new(&[0xb8, 0, 0x10, 0x8e, 0xc0, 0xeb, 0xfc]).unwrap();
    session.enable_v86(&[]).unwrap();
    session.set_v86_host_clock(true).unwrap();
    session.start();
    let completed = session.advance(65_536);
    assert_eq!(
        session.status(),
        SessionStatus::Running,
        "{:?}",
        session.fault()
    );
    assert!(session.timeslice_yielded());
    assert!(completed < 1024, "unbounded core transitions: {completed}");
}

#[test]
fn host_clock_fm_samples_follow_elapsed_time_not_cpu_throughput() {
    #[allow(dead_code)]
    mod fixture {
        include!("../../../../tests/fixtures/opl3.rs");
    }
    let program = fixture::opl3_tone_program();
    let loop_ip = 0x100 + program.len() as u32 - 2;
    let mut session = Session::new(&program).unwrap();
    session.enable_v86(&[]).unwrap();
    session.set_v86_host_clock(true).unwrap();
    session.capture_audio_sources(true);
    session.start();
    for _ in 0..128 {
        session.advance(65_536);
        if session.registers().ip == loop_ip {
            break;
        }
    }
    assert_eq!(session.registers().ip, loop_ip);
    let initial = session.emulated_nanos();
    let mut frames = [doslet_pcm::EMPTY; 1024];
    assert_eq!(session.drain_pcm_timed(&mut frames), 0);
    let mut samples = Vec::new();
    let mut sources = Vec::new();
    let mut source_frames = [doslet_pcm::SourceFrame::default(); 1024];
    for ms in 1..=20 {
        // Widely different CPU throughput must not stretch the audio clock.
        session.advance(if ms % 2 == 0 { 65_536 } else { 1 });
        session
            .advance_v86_host_time(initial + ms * 1_000_000)
            .unwrap();
        let count = session.drain_pcm_timed(&mut frames);
        samples.extend_from_slice(&frames[..count]);
        let count = session.drain_audio_source(true, &mut source_frames);
        sources.extend_from_slice(&source_frames[..count]);
    }
    assert_eq!(session.emulated_nanos(), initial + 20_000_000);
    // The mixed queue omits silence; the source clock includes every sample.
    assert_eq!(sources.len(), 960); // 20 ms at 48 kHz.
    assert!(samples.iter().any(|f| f.samples != [0, 0]));
    for pair in sources.windows(2) {
        assert_eq!(pair[0].rate, 48_000);
        let delta = pair[1].nanos - pair[0].nanos;
        assert!((20_833..=20_834).contains(&delta), "{pair:?}");
    }
}

fn finish(session: &mut Session) {
    session.start();
    for _ in 0..2000 {
        session.advance(4096);
        if session.status() != SessionStatus::Running {
            break;
        }
    }
    assert_eq!(
        session.status(),
        SessionStatus::Exited(0),
        "{:?}",
        session.fault()
    );
}

#[test]
fn v86_high_dos_function_does_not_block_program_exit() {
    let mut session = Session::new(&[
        0xB8, 0x01, 0xD0, // MOV AX,D001h
        0xCD, 0x21, // INT 21h
        0x3C, 0x00, // CMP AL,0
        0x75, 0x0E, // JNE failure exit
        0xB8, 0x00, 0xD0, // MOV AX,D000h
        0xCD, 0x21, // INT 21h
        0x3C, 0x00, // CMP AL,0
        0x75, 0x05, // JNE failure exit
        0xB8, 0x00, 0x4C, // MOV AX,4C00h
        0xCD, 0x21, // INT 21h
        0xB8, 0x01, 0x4C, // MOV AX,4C01h
        0xCD, 0x21, // INT 21h
    ])
    .unwrap();
    session.enable_v86(&[]).unwrap();
    finish(&mut session);
}

#[test]
fn host_clock_midi_polling_yields_before_a_driver_timeout() {
    let mut code = vec![0xba, 0x31, 3, 0xb0, 0x3f, 0xee];
    for byte in [0x90, 60, 100] {
        code.extend_from_slice(&[
            0xb9, 100, 0, // CX=100: a bounded real driver wait.
            0xba, 0x31, 3, // DX=331h.
            0xec, 0xa8, 0x40, 0x74, 7, // IN; TEST busy; JZ ready.
            0xe2, 0xf9, // LOOP poll.
            0xb8, 1, 0x4c, 0xcd, 0x21, // Timeout: exit 1.
            0x4a, 0xb0, byte, 0xee, // Ready: transmit on 330h.
        ]);
    }
    code.extend_from_slice(&[0xb8, 0, 0x4c, 0xcd, 0x21]);
    let mut session = Session::new(&code).unwrap();
    session.enable_v86(&[]).unwrap();
    session.set_v86_host_clock(true).unwrap();
    session.start();
    let mut accepted = Vec::new();
    let mut bytes = [0; 8];
    for _ in 0..1000 {
        session.advance(65_536);
        let count = session.drain_midi(&mut bytes);
        for &byte in &bytes[..count] {
            accepted.push((session.emulated_nanos(), byte));
        }
        if session.status() != SessionStatus::Running {
            break;
        }
        let target = session.emulated_nanos() + 10_000;
        session.advance_v86_host_time(target).unwrap();
    }
    assert_eq!(
        session.status(),
        SessionStatus::Exited(0),
        "{:?}; {:?}",
        session.fault(),
        session.v86_last_error()
    );
    assert_eq!(
        accepted.iter().map(|(_, b)| *b).collect::<Vec<_>>(),
        [0x90, 60, 100]
    );
    assert!(accepted.windows(2).all(|w| w[1].0 - w[0].0 >= 320_000));
}

#[test]
fn v86_dos_services_and_rust_vga_match_the_demo() {
    let mut reference = Session::demo();
    let mut foreign = Session::demo();
    foreign.enable_v86(&[]).unwrap();
    assert!(foreign.v86_enabled());
    finish(&mut reference);
    finish(&mut foreign);
    assert_eq!(
        foreign.frame().unwrap().pixels,
        reference.frame().unwrap().pixels
    );
}

#[test]
fn v86_ports_reach_rust_mpu401() {
    let mut session = Session::new(&[
        0xba, 0x31, 0x03, // DX=331h
        0xb0, 0x3f, 0xee, // MPU UART command
        0x4a, 0xb0, 0x90, 0xee, // DX=330h, MIDI status
        0xb8, 0, 0x4c, 0xcd, 0x21,
    ])
    .unwrap();
    session.enable_v86(&[]).unwrap();
    finish(&mut session);
    let mut bytes = [0; 8];
    assert_eq!(session.drain_midi(&mut bytes), 1);
    assert_eq!(bytes[0], 0x90);
}

#[test]
fn v86_session_executes_jit_without_reimporting_context_each_batch() {
    let mut session = Session::new(&[
        0xbb, 3, 0, // three passes exceed the upstream hotness threshold
        0xb9, 0x60, 0xea, // CX=60000
        0x40, 0xe2, 0xfd, // INC AX; LOOP
        0x4b, 0x75, 0xf7, // DEC BX; JNZ outer
        0xb8, 0, 0x4c, 0xcd, 0x21,
    ])
    .unwrap();
    session.enable_v86(&[]).unwrap();
    session.start();
    session.advance(250_000);
    // Preparation is asynchronous: synchronize the job here so this test does
    // not depend on how the OS schedules its compiler thread.
    session.wait_for_v86_compilation().unwrap();
    finish(&mut session);
    let stats = session.v86_statistics().unwrap();
    assert!(
        stats.compiled_modules > 0 && stats.jit_entries > 0,
        "{stats:?}"
    );
    assert!(
        stats.context_imports < 5,
        "CPU context was rebound during ordinary batches: {stats:?}"
    );
}

#[test]
fn v86_preserves_guest_interrupt_vectors_and_borrows_text_ram() {
    let mut session = Session::new(&[
        0xcd, 0x60, // guest-installed handler, not a DOS host service
        0xb8, 0, 0x4c, 0xcd, 0x21, 0xbb, 0x34, 0x12, 0xcf, // handler: MOV BX,1234h; IRET
    ])
    .unwrap();
    session.enable_v86(&[]).unwrap();
    assert!(session.debug_write_memory(0x60 * 4, &[7, 1, 0, 0x10]));
    finish(&mut session);
    assert_eq!(
        session.debug_cpu().registers.get(doslet_x86::Gpr16::Bx),
        0x1234
    );
    assert_eq!(session.v86_statistics().unwrap().software_interrupts, 2);

    let mut text = Session::new(&[
        0xb8, 0, 0xb8, 0x8e, 0xc0, // ES=B800h
        0x26, 0xc7, 0x06, 0, 0, b'X', 7, // MOV ES:[0],0748h
        0xb8, 0, 0x4c, 0xcd, 0x21,
    ])
    .unwrap();
    text.enable_v86(&[]).unwrap();
    finish(&mut text);
    assert_eq!(text.ram_view(0xb8000..0xb8002), Some(&[b'X', 7][..]));
}

#[test]
fn v86_dpmi_host_returns_to_an_ldt_code_selector() {
    let mut session = Session::new(&[
        0xb8, 0x87, 0x16, 0xcd, 0x2f, // query DPMI entry ES:DI
        0x89, 0x3e, 0, 2, // save far pointer at DS:200h
        0x8c, 0xc0, 0xa3, 2, 2, 0xb8, 0, 0x80, 0x8e, 0xc0, // ES=8000h private host memory
        0xb8, 1, 0, 0xff, 0x1e, 0, 2, // 32-bit DPMI client; far call
        0xb8, 0, 4, 0xcd, 0x31, // INT 31h get host version; IRETD to LDT
        0xb8, 0, 0x4c, 0xcd, 0x21,
    ])
    .unwrap();
    session
        .enable_dpmi(doslet_runtime::session::dpmi::Mode::Direct)
        .unwrap();
    session.enable_v86(&[]).unwrap();
    finish(&mut session);
    let stats = session.dpmi_stats().unwrap();
    assert!(stats.entered && stats.calls > 0);
}
