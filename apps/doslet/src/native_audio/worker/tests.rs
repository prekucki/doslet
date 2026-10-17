use super::*;
use std::time::Instant;

fn config(rate: u32) -> Config {
    Config {
        source: Source::Rust,
        rate,
        nanos: 0,
        replay: Vec::new(),
    }
}
fn wait(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "audio worker did not make progress"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
fn complete(worker: &Worker, nanos: u64) {
    wait(|| {
        worker.check().unwrap();
        worker.shared.completed.load(Ordering::Acquire) == nanos
    });
}

#[test]
fn native_pcm_keeps_hardware_sample_count_and_values() {
    let (output, pcm, midi) = Writer::mock_separate(48000);
    let mut worker = Worker::new(config(48000), output).unwrap();
    let frames: Vec<_> = (0..2205_u64)
        .map(|i| doslet_pcm::SourceFrame {
            nanos: i * 1_000_000_000 / 22050,
            rate: 22050,
            samples: [(i as i16 % 127) * 100, -1000],
        })
        .collect();
    assert!(worker.ready().unwrap());
    worker.receive_source(false, &frames).unwrap();
    worker.render_until(100_000_000).unwrap();
    worker.submit().unwrap();
    complete(&worker, 100_000_000);
    assert_eq!(pcm.available(), 16384 - 2205);
    assert_eq!(midi.available(), 16384 - 4800);
    assert_eq!(
        pcm.consume(2205),
        frames
            .iter()
            .map(|f| f.samples.map(|v| f32::from(v) / 32768.0))
            .collect::<Vec<_>>()
    );
    assert_eq!(worker.stats().received, 2205);
}

#[test]
fn separate_streams_backpressure_pause_and_reset_together() {
    let (output, pcm, midi) = Writer::mock_separate(48000);
    let mut worker = Worker::new(config(48000), output.clone()).unwrap();
    output.set_active(false);
    midi.push(&vec![[0.25; 2]; midi.available()]).unwrap();
    assert_eq!(output.available(), 0);
    assert_eq!(output.refill_nanos(), 0);
    assert!(worker.ready().unwrap());
    worker.render_until(10_000_000).unwrap();
    worker.push_midi(&[0x90, 60, 100]).unwrap();
    worker.submit().unwrap();
    worker.configure(config(48000)).unwrap();
    assert_eq!(pcm.available(), 16384);
    assert_eq!(midi.available(), 16384);
    assert_eq!(worker.stats(), Stats::default());
    output.push_sources(&[[0.2; 2]; 8], &[[0.3; 2]; 8]).unwrap();
    assert_eq!(pcm.consume(8), [[0.0; 2]; 8]);
    assert_eq!(midi.consume(8), [[0.0; 2]; 8]);
    output.set_active(true);
    assert_eq!(pcm.consume(8), [[0.2; 2]; 8]);
    assert_eq!(midi.consume(8), [[0.3; 2]; 8]);
}

#[test]
fn threaded_midi_and_pcm_match_inline_samples_at_both_device_rates() {
    for rate in [44100, 48000] {
        let output = Writer::mock(rate);
        let mut worker = Worker::new(config(rate), output.clone()).unwrap();
        assert_eq!(
            worker.thread.as_ref().unwrap().thread().name(),
            Some("doslet-audio")
        );
        assert_ne!(
            worker.thread.as_ref().unwrap().thread().id(),
            thread::current().id()
        );
        let mut reference = Renderer::new(&config(rate)).unwrap();
        let mut expected = Vec::new();
        for step in 0..40u64 {
            let nanos = (step + 1) * 5_000_000;
            let frames: Vec<_> = (0..240u64)
                .map(|i| {
                    let index = step * 240 + i;
                    doslet_pcm::Frame {
                        nanos: index * 1_000_000_000 / 48000,
                        duration: 20834,
                        samples: [((index % 80) as i16 - 40) * 300, 2000],
                    }
                })
                .collect();
            let midi: &[u8] = match step {
                0 => &[0xC0, 10, 0x90, 60, 100],
                7 => &[0xB0, 7, 20],
                15 => &[0x80, 60, 0, 0x90, 64, 110],
                30 => &[0x80, 64, 0],
                _ => &[],
            };
            reference.mixer.receive(&frames).unwrap();
            loop {
                let count = reference.render(nanos).unwrap();
                if count == 0 {
                    break;
                }
                expected.extend_from_slice(&reference.samples[..count]);
            }
            reference.push_midi(nanos, midi).unwrap();
            wait(|| worker.ready().unwrap());
            worker.receive_pcm(&frames).unwrap();
            worker.render_until(nanos).unwrap();
            worker.push_midi(midi).unwrap();
            worker.submit().unwrap();
        }
        complete(&worker, 200_000_000);
        assert_eq!(worker.stats(), reference.stats());
        assert_eq!(expected.len(), rate as usize / 5);
        assert!(expected.iter().flatten().any(|sample| sample.abs() > 0.1));
        assert_eq!(output.consume(expected.len()), expected);
        assert_eq!(worker.pending_nanos(), 0);
    }
}

#[test]
fn pause_preserves_queued_sound_and_resume_continues_at_the_same_frame() {
    let output = Writer::mock(48000);
    let mut setup = config(48000);
    setup.replay = vec![0x90, 60, 100];
    let mut reference = Renderer::new(&setup).unwrap();
    let mut worker = Worker::new(setup, output.clone()).unwrap();
    output.set_active(false);
    assert!(worker.ready().unwrap());
    worker.render_until(10_000_000).unwrap();
    worker.submit().unwrap();
    complete(&worker, 10_000_000);
    let available = output.available();
    assert_eq!(output.consume(512), vec![[0.0; 2]; 512]);
    assert_eq!(output.available(), available);
    assert_eq!(output.stats(), (0, 0, 0.0));
    assert_eq!(worker.stats().notes, 1);
    output.set_active(true);
    let count = reference.render(10_000_000).unwrap();
    assert_eq!(output.consume(count), reference.samples[..count]);
}

#[test]
fn saturated_queue_yields_and_reset_cancels_old_batches_even_while_paused() {
    let output = Writer::mock(48000);
    let mut worker = Worker::new(config(48000), output.clone()).unwrap();
    output.set_active(false);
    output.push(&vec![[0.5; 2]; output.available()]).unwrap();
    for i in 0..SLOTS {
        assert!(worker.ready().unwrap());
        worker.render_until((i as u64 + 1) * 1_000_000).unwrap();
        worker.push_midi(&[0x90, 60, 100]).unwrap();
        worker.submit().unwrap();
    }
    assert!(!worker.ready().unwrap());
    let mut invalid = config(0);
    invalid.nanos = 123;
    assert!(worker.configure(invalid).is_err());
    assert_eq!(
        output.available(),
        0,
        "failed setup must retain the old queue"
    );
    assert_eq!(worker.pending_nanos(), SLOTS as u64 * 1_000_000);
    worker.check().unwrap();
    let mut replacement = config(48000);
    replacement.nanos = 7_000_000_000; // Output change after a long-running session.
    replacement.replay = vec![0x90, 67, 90];
    worker.configure(replacement).unwrap();
    assert_eq!(worker.stats().notes, 1, "old queued notes must not survive");
    assert_eq!(worker.pending_nanos(), 0);
    assert_eq!(output.available(), 16384);
    assert!(worker.ready().unwrap());
    worker.render_until(7_010_000_000).unwrap();
    worker.submit().unwrap();
    complete(&worker, 7_010_000_000);
    assert_eq!(
        output.available(),
        16384 - 480,
        "replay must not render from time zero"
    );
    // A real guest reset also discards the just-rendered replacement's sound.
    worker.configure(config(48000)).unwrap();
    assert_eq!(worker.stats(), Stats::default());
    assert_eq!(output.available(), 16384);
    output.set_active(true);
    assert_eq!(output.consume(16), vec![[0.0; 2]; 16]);
}

#[test]
fn pending_guest_time_is_bounded_and_failure_can_be_reconfigured() {
    let output = Writer::mock(48000);
    let mut worker = Worker::new(config(48000), output.clone()).unwrap();
    output.push(&vec![[0.1; 2]; output.available()]).unwrap();
    assert!(worker.ready().unwrap());
    worker.render_until(MAX_PENDING_NANOS).unwrap();
    worker.submit().unwrap();
    assert!(
        !worker.ready().unwrap(),
        "pending time limit applies even with free slots"
    );
    worker.configure(config(48000)).unwrap();
    assert!(worker.ready().unwrap());
    worker.receive_pcm(&[doslet_pcm::EMPTY]).unwrap(); // Invalid zero-duration PCM.
    worker.render_until(1_000_000).unwrap();
    worker.submit().unwrap();
    wait(|| worker.check().is_err());
    assert!(worker.check().unwrap_err().contains("invalid PCM frame"));
    assert!(worker.ready().is_err());
    worker.configure(config(48000)).unwrap();
    assert!(worker.ready().unwrap());
    worker.render_until(1_000_000).unwrap();
    worker.submit().unwrap();
    complete(&worker, 1_000_000);
}

#[test]
fn shutdown_does_not_wait_for_a_full_paused_sink() {
    let output = Writer::mock(48000);
    let mut worker = Worker::new(config(48000), output.clone()).unwrap();
    output.set_active(false);
    output.push(&vec![[0.1; 2]; output.available()]).unwrap();
    assert!(worker.ready().unwrap());
    worker.render_until(20_000_000).unwrap();
    worker.submit().unwrap();
    let (done, finished) = mpsc::sync_channel(1);
    let join = thread::spawn(move || {
        drop(worker);
        done.send(()).unwrap();
    });
    finished
        .recv_timeout(Duration::from_secs(5))
        .expect("shutdown waited for playback");
    join.join().unwrap();
}

#[test]
fn external_port_mode_renders_pcm_without_a_second_midi_synth() {
    let output = Writer::mock(48000);
    let mut setup = config(48000);
    setup.source = Source::External;
    let mut worker = Worker::new(setup, output.clone()).unwrap();
    assert!(worker.ready().unwrap());
    worker
        .receive_pcm(&[doslet_pcm::Frame {
            nanos: 0,
            duration: 1_000_000,
            samples: [16384, -16384],
        }])
        .unwrap();
    worker.render_until(1_000_000).unwrap();
    worker.push_midi(&[0x90, 60, 100]).unwrap();
    worker.submit().unwrap();
    complete(&worker, 1_000_000);
    assert_eq!(worker.stats().notes, 0);
    assert_eq!(output.consume(48), vec![[0.5, -0.5]; 48]);
}
