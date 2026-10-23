use super::*;
use doslet_pulse::connect;

fn connect_probe() -> Client {
    let client = connect().unwrap();
    let sink = wait(client.sink_info_by_name(c"doslet-audio-probe-output".into()))
        .expect("create the dedicated null sink before running loopback tests");
    assert_eq!(sink.sample_spec.channels, 2);
    wait(client.source_info_by_name(c"doslet-audio-probe-output.monitor".into()))
        .expect("the dedicated null sink must have its own monitor");
    client
}

fn assert_dormant(writer: &Writer) {
    let queue = writer.queue.lock().unwrap();
    assert!(queue.frames.is_empty());
    assert!(queue.rates.is_empty());
    assert_eq!(queue.len, 0);
    drop(queue);
    let control = writer.pulse.as_ref().unwrap();
    assert!(matches!(
        *control.launch.lock().unwrap(),
        Launch::Pending { .. }
    ));
    assert!(control.thread.get().is_none());
}

#[test]
fn host_clock_transport_serves_small_chunks_without_waiting_for_playback_reserve() {
    let writer = Writer::mock(RATE);
    writer.set_host_clock_reserve(true);
    let mut source = Source {
        queue: writer.queue.clone(),
        generation: 0,
        rate: RATE,
    };
    let mut context = Context::from_waker(std::task::Waker::noop());
    let mut bytes = [0; 48 * 8];
    for _ in 0..3 {
        writer.push(&[[0.25, -0.5]; 48]).unwrap(); // One ms, below the 80 ms reserve.
        assert_eq!(
            Pin::new(&mut source).poll_read(&mut context, &mut bytes),
            Poll::Ready(bytes.len())
        );
        for frame in bytes.as_chunks::<8>().0 {
            assert_eq!(f32::from_le_bytes(frame[..4].try_into().unwrap()), 0.25);
            assert_eq!(f32::from_le_bytes(frame[4..].try_into().unwrap()), -0.5);
        }
        assert_eq!(
            Pin::new(&mut source).poll_read(&mut context, &mut bytes),
            Poll::Pending
        );
    }
    assert_eq!(writer.stats().0, 144);
    assert_eq!(writer.stats().1, 0);
}

#[test]
fn pulse_reserve_is_requested_at_the_server_for_all_cpu_backends() {
    for rate in [11025, 22050, 48000, 96000] {
        let params = stream_params(rate, c"test").unwrap();
        assert_eq!(params.buffer_attr.target_length, rate * 80 / 1000 * 8);
        assert_eq!(
            params.buffer_attr.pre_buffering,
            params.buffer_attr.target_length
        );
        assert!(params.buffer_attr.max_length > params.buffer_attr.target_length);
        assert!(!params.flags.adjust_latency);
    }
}

#[test]
fn unavailable_underrun_reports_preserve_the_last_valid_interval() {
    let mut previous = 0;
    for (bytes, delta, last) in [
        (u64::MAX, 0, 0),
        (80, 10, 10),
        (160, 10, 20),
        (u64::MAX - 8, 0, 20),
        (240, 10, 30),
        (0, 0, 0),
        (80, 10, 10),
    ] {
        assert_eq!(underrun_delta(&mut previous, bytes), delta);
        assert_eq!(previous, last);
    }
}

#[test]
fn unused_system_outputs_survive_controls_without_connecting_or_allocating_samples() {
    let output = Output::new(super::super::Backend::Pulse, super::super::Mixer::System).unwrap();
    let writer = output.writer();
    writer.set_volume(35);
    writer.set_active(false);
    writer.clear().unwrap();
    writer.set_active(true);
    writer.push(&[]).unwrap();
    writer.push_hardware(false, 11025, &[]).unwrap();
    writer.push_hardware(true, 48000, &[]).unwrap();
    writer.check().unwrap();
    assert_eq!(writer.available(), CAPACITY);
    assert_eq!(writer.refill_nanos(), 0);
    assert_dormant(&writer);
    assert_dormant(writer.secondary.as_ref().unwrap());
    assert_dormant(writer.fm.as_ref().unwrap());
}

#[test]
fn external_midi_only_worker_never_starts_any_local_system_output() {
    use crate::native_audio::{Config, Source, Worker};
    let output = Output::new(super::super::Backend::Pulse, super::super::Mixer::System).unwrap();
    let writer = output.writer();
    let mut worker = Worker::new(
        Config {
            source: Source::External,
            rate: RATE,
            nanos: 0,
            replay: Vec::new(),
        },
        writer.clone(),
    )
    .unwrap();
    for nanos in [10_000_000, 1_000_000_000, 2_000_000_000] {
        assert!(worker.ready().unwrap());
        worker.push_midi(&[0xc0, 20, 0x90, 60, 100]).unwrap();
        worker.render_until(nanos).unwrap();
        worker.submit().unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while worker.pending_nanos() != 0 {
            worker.check().unwrap();
            assert!(
                Instant::now() < deadline,
                "external MIDI batch did not complete"
            );
            thread::sleep(Duration::from_millis(1));
        }
    }
    assert_eq!(worker.stats(), Default::default());
    assert_dormant(&writer);
    assert_dormant(writer.secondary.as_ref().unwrap());
    assert_dormant(writer.fm.as_ref().unwrap());
}

#[test]
#[ignore = "requires PULSE_SINK=doslet-audio-probe-output, a dedicated null sink"]
fn native_rate_changes_preserve_segment_duration_and_reset_interrupts_drain() {
    use doslet_pulse::protocol as p;
    assert_eq!(
        std::env::var("PULSE_SINK").unwrap(),
        "doslet-audio-probe-output"
    );
    let client = connect_probe();
    let captured = Arc::new(Mutex::new(Vec::<f32>::with_capacity(48000 * 4)));
    let capture = captured.clone();
    let record = wait(client.create_record_stream(
        p::RecordStreamParams {
            sample_spec: p::SampleSpec {
                format: p::SampleFormat::Float32Le,
                channels: 2,
                sample_rate: RATE,
            },
            channel_map: p::ChannelMap::stereo(),
            source_name: Some(c"doslet-audio-probe-output.monitor".into()),
            buffer_attr: p::stream::BufferAttr {
                fragment_size: 480 * 8,
                ..Default::default()
            },
            ..Default::default()
        },
        move |bytes: &[u8]| {
            let mut values = capture.lock().unwrap();
            for frame in bytes.as_chunks::<8>().0 {
                if values.len() < values.capacity() {
                    values.push(f32::from_le_bytes(frame[..4].try_into().unwrap()));
                }
            }
        },
    ))
    .unwrap();
    let output = deferred_at_rate(c"Doslet native-rate test", 11025);
    output.clear().unwrap();
    assert_dormant(&output.writer());
    output.push_at_rate(11025, &vec![[0.2; 2]; 1103]).unwrap();
    output.push_at_rate(22050, &vec![[-0.3; 2]; 2205]).unwrap();
    output.push_at_rate(32000, &vec![[0.4; 2]; 3200]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while output.queue.lock().unwrap().sample_rate != 32000 || output.available() != CAPACITY {
        output.check().unwrap();
        assert!(
            Instant::now() < deadline,
            "rate transitions did not complete"
        );
        thread::sleep(Duration::from_millis(5));
    }
    thread::sleep(Duration::from_millis(150));
    {
        let samples = captured.lock().unwrap();
        eprintln!(
            "native-rate capture frames={} min={} max={} counts={:?}",
            samples.len(),
            samples.iter().copied().fold(f32::INFINITY, f32::min),
            samples.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            [0.2, -0.3, 0.4].map(|level| samples
                .iter()
                .filter(|v| (**v - level).abs() < 0.02)
                .count())
        );
        for level in [0.2, -0.3, 0.4] {
            let count = samples
                .iter()
                .filter(|v| (**v - level).abs() < 0.02)
                .count();
            assert!(
                (4500..5100).contains(&count),
                "100-ms segment {level} had {count} frames at 48 kHz"
            );
        }
    }
    // Queue an old-format tail and a new format, then pause at the boundary.
    output.push_at_rate(32000, &[[0.7; 2]; 960]).unwrap();
    output.push_at_rate(11025, &[[0.8; 2]; 512]).unwrap();
    output.set_active(false);
    thread::sleep(Duration::from_millis(50));
    let start = Instant::now();
    output.clear().unwrap();
    assert!(start.elapsed() < Duration::from_secs(3));
    thread::sleep(Duration::from_millis(100));
    let boundary = captured.lock().unwrap().len();
    output.push_at_rate(22050, &[[-0.5; 2]; 2205]).unwrap();
    output.set_active(true);
    thread::sleep(Duration::from_millis(250));
    output.check().unwrap();
    output.set_active(false);
    wait(record.cork()).unwrap();
    let samples = captured.lock().unwrap();
    eprintln!(
        "after reset maximum={}",
        samples[boundary..]
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max)
    );
    assert!(samples[boundary..].iter().any(|v| *v < -0.4));
    assert!(
        // Resampling the negative step can ring slightly above zero. The old
        // +0.7/+0.8 blocks must not survive; allow only short filter ringing.
        samples[boundary..].iter().all(|v| *v < 0.15)
            && samples[boundary..].iter().filter(|v| **v > 0.01).count() < 64,
        "old format samples escaped reset"
    );
}

#[test]
#[ignore = "requires PULSE_SINK=doslet-audio-probe-output, a dedicated two-channel null sink"]
fn system_mixer_loopback_combines_both_sources_and_resets_them() {
    use doslet_pulse::protocol as p;
    assert_eq!(
        std::env::var("PULSE_SINK").unwrap(),
        "doslet-audio-probe-output"
    );
    let client = connect_probe();
    let captured = Arc::new(Mutex::new(Vec::<[f32; 2]>::with_capacity(48000 * 4)));
    let capture = captured.clone();
    let record = wait(client.create_record_stream(
        p::RecordStreamParams {
            sample_spec: p::SampleSpec {
                format: p::SampleFormat::Float32Le,
                channels: 2,
                sample_rate: RATE,
            },
            channel_map: p::ChannelMap::stereo(),
            source_name: Some(c"doslet-audio-probe-output.monitor".into()),
            buffer_attr: p::stream::BufferAttr {
                fragment_size: 480 * 8,
                ..Default::default()
            },
            ..Default::default()
        },
        move |bytes: &[u8]| {
            let mut values = capture.lock().unwrap();
            for frame in bytes.as_chunks::<8>().0 {
                if values.len() < values.capacity() {
                    values.push([
                        f32::from_le_bytes(frame[..4].try_into().unwrap()),
                        f32::from_le_bytes(frame[4..].try_into().unwrap()),
                    ]);
                }
            }
        },
    ))
    .unwrap();
    let output = Output::new(crate::audio::Backend::Cpal, crate::audio::Mixer::System).unwrap();
    assert_eq!(output.mixer(), crate::audio::Mixer::System);
    for _ in 0..10 {
        output
            .push_sources(&[[0.1, 0.05]; 2400], &[[0.2, 0.3]; 2400])
            .unwrap();
        thread::sleep(Duration::from_millis(50));
        output.check().unwrap();
    }
    output.set_active(false);
    thread::sleep(Duration::from_millis(100));
    let available = output.available();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(output.available(), available);
    output.clear().unwrap();
    thread::sleep(Duration::from_millis(100));
    let boundary = captured.lock().unwrap().len();
    output
        .push_sources(&[[-0.1, -0.05]; 2400], &[[-0.2, -0.3]; 2400])
        .unwrap();
    output.set_active(true);
    for _ in 0..10 {
        thread::sleep(Duration::from_millis(50));
        output
            .push_sources(&[[-0.1, -0.05]; 2400], &[[-0.2, -0.3]; 2400])
            .unwrap();
        output.check().unwrap();
    }
    output.set_active(false);
    wait(record.cork()).unwrap();
    let samples = captured.lock().unwrap();
    let close = |sample: &[f32; 2], expected: [f32; 2]| {
        (sample[0] - expected[0]).abs() < 0.01 && (sample[1] - expected[1]).abs() < 0.01
    };
    assert!(
        samples[..boundary]
            .iter()
            .filter(|s| close(s, [0.3, 0.35]))
            .count()
            > 4800,
        "server did not combine both sources for at least 100 ms"
    );
    assert!(
        samples[boundary..]
            .iter()
            .filter(|s| close(s, [-0.3, -0.35]))
            .count()
            > 4800,
        "both reset sources did not resume"
    );
    assert!(
        samples[boundary..].iter().flatten().all(|v| *v < 0.01),
        "old source audio escaped reset"
    );
}

#[test]
fn paused_and_obsolete_sources_cannot_consume_current_generation() {
    let writer = Writer::mock(RATE);
    let mut source = Source {
        rate: RATE,
        queue: writer.queue.clone(),
        generation: 0,
    };
    let mut bytes = [0_u8; 16];
    let mut context = Context::from_waker(std::task::Waker::noop());
    writer.push(&[[0.25, -0.5]; 2]).unwrap();
    writer.set_active(false);
    assert_eq!(
        Pin::new(&mut source).poll_read(&mut context, &mut bytes),
        Poll::Pending
    );
    assert_eq!(writer.available(), CAPACITY - 2);
    writer.set_active(true);
    assert_eq!(
        Pin::new(&mut source).poll_read(&mut context, &mut bytes),
        Poll::Ready(16)
    );
    assert_eq!(f32::from_le_bytes(bytes[..4].try_into().unwrap()), 0.25);
    assert_eq!(f32::from_le_bytes(bytes[4..8].try_into().unwrap()), -0.5);

    writer.push(&[[0.75; 2]; 10]).unwrap();
    writer.clear().unwrap();
    writer.push(&[[-0.25; 2]; 2]).unwrap();
    assert_eq!(
        Pin::new(&mut source).poll_read(&mut context, &mut bytes),
        Poll::Pending
    );
    assert_eq!(writer.available(), CAPACITY - 2);
    let mut current = Source {
        rate: RATE,
        queue: writer.queue.clone(),
        generation: 1,
    };
    assert_eq!(
        Pin::new(&mut current).poll_read(&mut context, &mut bytes),
        Poll::Ready(16)
    );
    assert_eq!(f32::from_le_bytes(bytes[..4].try_into().unwrap()), -0.25);
}

#[test]
fn source_waits_for_samples_without_signalling_eof() {
    let writer = Writer::mock(RATE);
    let mut source = Source {
        rate: RATE,
        queue: writer.queue.clone(),
        generation: 0,
    };
    let mut context = Context::from_waker(std::task::Waker::noop());
    let mut bytes = [0; 8];
    assert_eq!(
        Pin::new(&mut source).poll_read(&mut context, &mut bytes),
        Poll::Pending
    );
    writer.push(&[[0.5; 2]]).unwrap();
    assert_eq!(
        Pin::new(&mut source).poll_read(&mut context, &mut bytes),
        Poll::Ready(8)
    );
    assert_eq!(
        Pin::new(&mut source).poll_read(&mut context, &mut bytes),
        Poll::Pending
    );
}

#[test]
#[ignore = "requires PULSE_SINK=doslet-audio-probe-output, a dedicated two-channel null sink"]
fn pulse_loopback_pause_and_reset_discard_old_server_audio() {
    use doslet_pulse::protocol as p;
    let sink = std::env::var("PULSE_SINK").expect("PULSE_SINK must select the test sink");
    assert_eq!(sink, "doslet-audio-probe-output");
    let client = connect_probe();
    let info =
        wait(client.sink_info_by_name(std::ffi::CString::new(sink.clone()).unwrap())).unwrap();
    assert_eq!(info.sample_spec.channels, 2);
    // PipeWire also marks module-null-sink as HARDWARE. The exact dedicated
    // fixture name above, not that compatibility flag, selects this test sink.
    let recorded = Arc::new(Mutex::new(Vec::<f32>::with_capacity(RATE as usize * 3)));
    let capture = recorded.clone();
    let record = wait(client.create_record_stream(
        p::RecordStreamParams {
            sample_spec: p::SampleSpec {
                format: p::SampleFormat::Float32Le,
                channels: 2,
                sample_rate: RATE,
            },
            channel_map: p::ChannelMap::stereo(),
            source_name: Some(std::ffi::CString::new(format!("{sink}.monitor")).unwrap()),
            buffer_attr: p::stream::BufferAttr {
                fragment_size: 480 * 8,
                ..Default::default()
            },
            ..Default::default()
        },
        move |bytes: &[u8]| {
            let mut values = capture.lock().unwrap();
            for frame in bytes.as_chunks::<8>().0 {
                if values.len() < values.capacity() {
                    values.push(f32::from_le_bytes(frame[..4].try_into().unwrap()));
                }
            }
        },
    ))
    .unwrap();
    let existing_clients: Vec<_> = wait(client.list_clients())
        .unwrap()
        .into_iter()
        .map(|client| client.index)
        .collect();
    let output = open().unwrap();
    for _ in 0..5 {
        output.push(&[[0.25; 2]; 2400]).unwrap();
        thread::sleep(Duration::from_millis(50));
        output.check().unwrap();
    }
    output.set_active(false);
    thread::sleep(Duration::from_millis(100));
    let available = output.available();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(output.available(), available);
    output.clear().unwrap();
    thread::sleep(Duration::from_millis(100));
    let boundary = recorded.lock().unwrap().len();
    output.push(&[[-0.25; 2]; 2400]).unwrap();
    output.set_active(true);
    for _ in 0..5 {
        thread::sleep(Duration::from_millis(50));
        output.push(&[[-0.25; 2]; 2400]).unwrap();
        output.check().unwrap();
    }
    output.set_active(false);
    wait(record.cork()).unwrap();
    let samples = recorded.lock().unwrap();
    assert!(
        samples[..boundary].iter().any(|s| *s > 0.2),
        "positive signal missing"
    );
    assert!(
        samples[boundary..].iter().any(|s| *s < -0.2),
        "new generation missing"
    );
    assert!(
        samples[boundary..].iter().all(|s| *s < 0.01),
        "old audio escaped reset"
    );
    drop(samples);

    // Disconnect only this test process's newly opened playback client. Never
    // restart the user's server or disturb other applications. Successful
    // reconnections must not reset the lifetime retry budget.
    let pid = std::ffi::CString::new(std::process::id().to_string()).unwrap();
    let mut killed = Vec::new();
    for attempt in 0..4 {
        let deadline = Instant::now() + Duration::from_secs(5);
        let index = loop {
            let clients = wait(client.list_clients()).unwrap();
            if let Some(current) = clients.iter().find(|candidate| {
                !existing_clients.contains(&candidate.index)
                    && !killed.contains(&candidate.index)
                    && candidate.props.get(p::Prop::ApplicationProcessId)
                        == Some(pid.as_bytes_with_nul())
            }) {
                break current.index;
            }
            assert!(Instant::now() < deadline, "reconnection {attempt} missing");
            thread::sleep(Duration::from_millis(20));
        };
        // Let setup complete before injecting the next disconnect.
        thread::sleep(Duration::from_millis(150));
        output.check().unwrap();
        wait(client.kill_client(index)).unwrap();
        killed.push(index);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Err(error) = output.check() {
            assert!(error.contains("exhausted after 3 attempts"), "{error}");
            break;
        }
        assert!(Instant::now() < deadline, "retry budget was not exhausted");
        thread::sleep(Duration::from_millis(20));
    }
}
