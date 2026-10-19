use super::*;

mod fixture {
    include!("../../../tests/fixtures/wasm_presentation.rs");
}

#[test]
fn native_snapshots_render_and_label_a_session_after_wasm_memory_migration() {
    let mut app = Doslet::new();
    app.session = Session::new(&[
        0xb8, 0x13, 0, 0xcd, 0x10, // INT 10h: mode 13h.
        0xb8, 0, 0xa0, 0x8e, 0xc0, // ES=A000h.
        0x26, 0xc6, 6, 0, 0, 7, // First video pixel = 7.
        0xb8, 0, 0x4c, 0xcd, 0x21, // Exit.
    ])
    .unwrap()
    .into();
    fixture::install(&mut app.session);

    let initial = app.snapshot(false);
    assert_eq!(initial.video_mode_label, "80×25 VGA text");
    assert!(initial.display.image.is_some());
    assert_eq!(initial.session.status(), SessionStatus::Ready);
    assert_eq!(initial.session.steps(), 0);

    app.session.start();
    app.session.advance(100);
    let before = (app.session.steps(), app.session.emulated_nanos());
    let graphics = app.snapshot(false);
    assert_eq!(graphics.video_mode_label, "320×200×8 VGA");
    assert!(graphics.display.image.is_some());
    assert_eq!(graphics.session.status(), SessionStatus::Exited(0));
    assert_eq!(graphics.session.steps(), before.0);
    assert_eq!(app.session.frame().unwrap().pixels[0], 7);
    assert_eq!((app.session.steps(), app.session.emulated_nanos()), before);
}
