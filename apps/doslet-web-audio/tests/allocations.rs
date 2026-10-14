#![cfg(not(target_arch = "wasm32"))]
use doslet_web_audio::AudioEngine;
#[test]
fn worklet_render_reset_and_watermarks_do_not_allocate_after_setup() {
    let mut engine = AudioEngine::new(48000);
    let measured = allocation_counter::measure(|| {
        engine.reset(0.0);
        assert_eq!(engine.receive(1_000_000_000.0, 0), 0);
        for _ in 0..100 {
            assert_eq!(engine.render(137), 0);
        }
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}
