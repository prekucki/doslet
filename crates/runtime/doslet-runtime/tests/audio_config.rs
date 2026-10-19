use doslet_runtime::{Session, SessionStatus};

include!("../../../../tests/fixtures/relocated_mz.rs");

#[test]
fn fm_detection_waits_for_the_isa_bus_at_high_cpu_rates() {
    let program = [
        0xba, 0x88, 3, // DX=388h.
        0xb0, 2, 0xee, 0x42, 0xb0, 0xff, 0xee, 0x4a, // Timer 1 preset FFh.
        0xb0, 4, 0xee, 0x42, 0xb0, 0x21, 0xee, 0x4a, // Start unmasked timer 1.
        0xb9, 100, 0, 0xec, 0xe2, 0xfd, // ROTT-style delay: 100 port reads.
        0xec, 0xa2, 0, 2, // Read final status to DS:0200h.
        0xb8, 0, 0x4c, 0xcd, 0x21,
    ];
    for mhz in [33, 1000] {
        for budget in [1, 10_000] {
            let mut session = Session::new_configured(
                &program,
                doslet_config::HardwareConfig {
                    cpu_mhz: mhz,
                    ..Default::default()
                },
            )
            .unwrap();
            session.start();
            for _ in 0..1000 {
                session.advance(budget);
                if session.status() != SessionStatus::Running {
                    break;
                }
            }
            assert_eq!(session.status(), SessionStatus::Exited(0));
            assert_eq!(
                session.ram_view(0x10200..0x10201).unwrap(),
                &[0xc0],
                "{mhz} MHz, budget {budget}"
            );
            assert!(!session.audio_status().opl_started);
        }
    }
}

#[test]
fn live_audio_configuration_preserves_execution_and_rechecks_guest_use() {
    // Probe the MPU status port, then loop.
    let mut s = Session::new_configured(
        &[0xba, 0x31, 3, 0xec, 0xeb, 0xfe],
        doslet_config::HardwareConfig {
            sb16: false,
            ..Default::default()
        },
    )
    .unwrap();
    let before = (s.epoch(), s.steps(), s.emulated_nanos(), s.debug_pc());
    let mut audio = s.hardware_config().audio();
    audio.mpu401 = false;
    s.configure_unused_audio(audio).unwrap();
    audio.mpu401 = true;
    s.configure_unused_audio(audio).unwrap();
    assert_eq!(
        (s.epoch(), s.steps(), s.emulated_nanos(), s.debug_pc()),
        before
    );
    assert_eq!(s.status(), SessionStatus::Ready);
    s.start();
    s.advance(20);
    assert!(s.audio_use().mpu401);
    let before = (s.epoch(), s.steps(), s.emulated_nanos(), s.debug_pc());
    audio.mpu401 = false;
    assert!(s.configure_unused_audio(audio).is_err());
    assert!(s.hardware_config().mpu401);
    assert_eq!(
        (s.epoch(), s.steps(), s.emulated_nanos(), s.debug_pc()),
        before
    );
    assert_eq!(s.status(), SessionStatus::Running);
}

#[test]
fn blaster_changes_are_rejected_atomically_even_before_audio_io() {
    for sb16 in [false, true] {
        let mut s = Session::from_mz_stub_named_configured(
            &relocated_mz(),
            b"C:\\SYNTH.EXE",
            doslet_config::HardwareConfig {
                sb16,
                ..Default::default()
            },
        )
        .unwrap();
        let old = s.hardware_config();
        let environment = s.ram_view(0x0f000..0x0f100).unwrap().to_vec();
        let before = (s.epoch(), s.steps(), s.emulated_nanos(), s.debug_pc());
        let mut changes = Vec::new();
        let mut audio = old.audio();
        audio.sb16 = !sb16;
        changes.push(audio);
        if sb16 {
            let mut audio = old.audio();
            audio.irq = 5;
            changes.push(audio);
            let mut audio = old.audio();
            audio.dma8 = 3;
            changes.push(audio);
            let mut audio = old.audio();
            audio.dma16 = 6;
            changes.push(audio);
            let mut audio = old.audio();
            audio.mpu401 = false;
            changes.push(audio);
        }
        for audio in changes {
            assert!(matches!(
                s.configure_unused_audio(audio),
                Err(doslet_runtime::AudioConfigError::RestartRequired(_))
            ));
            assert_eq!(s.hardware_config(), old);
            assert_eq!(s.ram_view(0x0f000..0x0f100).unwrap(), environment);
            assert_eq!(
                (s.epoch(), s.steps(), s.emulated_nanos(), s.debug_pc()),
                before
            );
        }
        s.configure_unused_audio(old.audio()).unwrap();
    }
}
