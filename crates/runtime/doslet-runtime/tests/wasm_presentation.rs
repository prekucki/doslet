#![cfg(all(feature = "wasm-procedures", not(target_arch = "wasm32")))]

use doslet_runtime::{Session, SessionStatus};

mod fixture {
    include!("../../../../tests/fixtures/wasm_presentation.rs");
}

#[test]
fn presentation_borrows_migrated_vram_and_reports_text_vga_and_vbe_modes() {
    for (setup, dimensions, label) in [
        (
            &[0xb8, 0x13, 0, 0xcd, 0x10][..], // INT 10h: mode 13h.
            (320, 200),
            "320×200×8 VGA",
        ),
        (
            &[0xbb, 1, 1, 0xb8, 2, 0x4f, 0xcd, 0x10][..], // VBE mode 101h.
            (640, 480),
            "640×480×8 SVGA",
        ),
    ] {
        let mut program = setup.to_vec();
        program.extend_from_slice(&[
            0xb8, 0, 0xa0, 0x8e, 0xc0, // ES=A000h.
            0x26, 0xc6, 6, 0, 0, 7, // First video pixel = 7.
            0xb8, 0, 0x4c, 0xcd, 0x21, // Exit.
        ]);
        let mut session = Session::new_configured(
            &program,
            doslet_config::HardwareConfig {
                memory_mib: 4,
                ..Default::default()
            },
        )
        .unwrap();
        fixture::install(&mut session);

        // Native startup requests both the text image and its label before execution.
        assert_eq!(session.video_mode_label().to_string(), "80×25 VGA text");
        assert!(session.frame().is_none());
        let text = session.presentation_frame();
        assert_eq!((text.width(), text.height()), (720, 400));
        let mut rgba = vec![0; text.width() * text.height() * 4];
        text.write_rgba(&mut rgba).unwrap();
        assert_eq!((session.steps(), session.emulated_nanos()), (0, 0));

        session.start();
        session.advance(100);
        assert_eq!(session.status(), SessionStatus::Exited(0));
        let before = (
            session.steps(),
            session.emulated_nanos(),
            session.debug_pc(),
        );
        assert_eq!(session.video_mode_label().to_string(), label);
        let indexed = session.frame().unwrap();
        assert_eq!((indexed.width, indexed.height), dimensions);
        assert_eq!(indexed.pixels.as_ptr(), session.vram_view().as_ptr());
        assert_eq!(indexed.pixels[0], 7);
        let frame = session.presentation_frame();
        assert_eq!((frame.width(), frame.height()), dimensions);
        rgba.resize(frame.width() * frame.height() * 4, 0);
        frame.write_rgba(&mut rgba).unwrap();
        assert_eq!(&rgba[..4], &indexed.palette[7]);
        assert_eq!(
            (
                session.steps(),
                session.emulated_nanos(),
                session.debug_pc()
            ),
            before
        );
    }
}
