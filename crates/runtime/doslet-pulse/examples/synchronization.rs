//! Explicit monitor-only test. Requires a dedicated four-channel null sink.
//! cargo run -p doslet-pulse --example synchronization -- SINK SECONDS
#[cfg(target_os = "linux")]
fn main() -> Result<(), String> {
    use doslet_pulse::{connect, playback_params, protocol as p, wait};
    use pulseaudio::AsPlaybackSource;
    use std::{
        ffi::CString,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };
    let args: Vec<_> = std::env::args().collect();
    let sink_name = args
        .get(1)
        .ok_or("Specify a dedicated four-channel null sink")?;
    if !sink_name.starts_with("doslet-audio-probe") {
        return Err("Only a dedicated doslet-audio-probe sink may be used".into());
    }
    let seconds: u64 = args
        .get(2)
        .map_or(Ok(10), |s| s.parse())
        .map_err(|_| "Invalid duration")?;
    if !(2..=600).contains(&seconds) {
        return Err("Duration must be 2..600 seconds".into());
    }
    let client = connect()?;
    let info = wait(client.server_info())?;
    println!(
        "server={:?} version={:?}",
        info.server_name, info.server_version
    );
    let sink = wait(
        client.sink_info_by_name(CString::new(sink_name.as_str()).map_err(|e| e.to_string())?),
    )?;
    if sink.sample_spec.channels != 4 {
        return Err("Probe requires a four-channel null sink".into());
    }
    let positions = [
        p::ChannelPosition::FrontLeft,
        p::ChannelPosition::FrontRight,
        p::ChannelPosition::RearLeft,
        p::ChannelPosition::RearRight,
    ];
    let captured = Arc::new(Mutex::new(Vec::<[f32; 4]>::with_capacity(
        (seconds as usize + 3) * 48000,
    )));
    let capture = captured.clone();
    let record = wait(client.create_record_stream(
        p::RecordStreamParams {
            sample_spec: p::SampleSpec {
                format: p::SampleFormat::Float32Le,
                channels: 4,
                sample_rate: 48000,
            },
            channel_map: p::ChannelMap::new(positions),
            source_name: Some(CString::new(format!("{sink_name}.monitor")).unwrap()),
            buffer_attr: p::stream::BufferAttr {
                fragment_size: 480 * 16,
                ..Default::default()
            },
            ..Default::default()
        },
        move |bytes: &[u8]| {
            let mut frames = capture.lock().unwrap();
            for frame in bytes.as_chunks::<16>().0 {
                if frames.len() == frames.capacity() {
                    break;
                }
                frames.push(std::array::from_fn(|i| {
                    f32::from_le_bytes(frame[i * 4..i * 4 + 4].try_into().unwrap())
                }));
            }
        },
    ))?;
    let mut streams = Vec::new();
    for (index, rate) in [11025_u32, 22050, 32000, 48000].into_iter().enumerate() {
        let mut params = playback_params(rate, 1, c"Doslet synchronization probe")?;
        params.channel_map = p::ChannelMap::new([positions[index]]);
        params.sink_name = Some(CString::new(sink_name.as_str()).unwrap());
        params.flags.no_remix_channels = true;
        params.sync_id = 1;
        let mut cursor = 0_u64;
        let source = move |data: &mut [u8]| {
            for bytes in data.as_chunks_mut::<4>().0 {
                // A 5-ms pulse each second. The first is at 0.5 s.
                let phase = cursor % u64::from(rate);
                let sample: f32 =
                    if (u64::from(rate / 2)..u64::from(rate / 2 + rate / 200)).contains(&phase) {
                        0.5
                    } else {
                        0.0
                    };
                bytes.copy_from_slice(&sample.to_le_bytes());
                cursor += 1;
            }
            data.len() / 4 * 4
        };
        streams.push(wait(
            client.create_playback_stream(params, source.as_playback_source()),
        )?);
    }
    // Explicitly uncork each stream: compatibility servers need not implement
    // PulseAudio's implicit sync-group control operations.
    wait(futures::future::try_join_all(
        streams.iter().map(|s| s.uncork()),
    ))?;
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut next_pause = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        if args.get(3).is_some_and(|arg| arg == "controls") && Instant::now() >= next_pause {
            wait(futures::future::try_join_all(
                streams.iter().map(|s| s.cork()),
            ))?;
            std::thread::sleep(Duration::from_millis(100));
            wait(futures::future::try_join_all(
                streams.iter().map(|s| s.uncork()),
            ))?;
            next_pause += Duration::from_secs(2);
        }
    }
    for stream in &streams {
        wait(stream.cork())?;
    }
    wait(record.cork())?;
    let frames = captured.lock().unwrap();
    let mut edges: [Vec<usize>; 4] = std::array::from_fn(|_| Vec::new());
    for (i, frame) in frames.iter().enumerate().skip(1) {
        for ch in 0..4 {
            if frame[ch] > 0.25 && frames[i - 1][ch] <= 0.25 {
                edges[ch].push(i);
            }
        }
    }
    let count = edges.iter().map(Vec::len).min().unwrap();
    println!("marker_counts={:?}", edges.each_ref().map(Vec::len));
    if count < 2 {
        return Err(format!(
            "Insufficient captured markers: {:?}",
            edges.each_ref().map(Vec::len)
        ));
    }
    let mut worst = 0;
    let mut drift = 0;
    for ch in 1..4 {
        let first = edges[ch][0] as i64 - edges[0][0] as i64;
        let last = edges[ch][count - 1] as i64 - edges[0][count - 1] as i64;
        worst = worst.max(first.abs()).max(last.abs());
        drift = drift.max((last - first).abs());
        println!(
            "channel={ch} markers={count} first_offset_frames={first} last_offset_frames={last}"
        );
    }
    println!(
        "captured_frames={} worst_ordinal_offset_us={} ordinal_offset_change_us={}",
        frames.len(),
        worst * 1_000_000 / 48000,
        drift * 1_000_000 / 48000
    );
    if worst > 48 || drift > 48 || edges.iter().any(|channel| channel.len() != count) {
        return Err("Marker correspondence exceeds 1 ms or markers were lost; system mixing gate FAILED. Ordinal offsets cannot distinguish phase drift from missing markers.".into());
    }
    println!(
        "marker correspondence PASSED for this run; reset/rate changes, underruns and other servers require separate acceptance"
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This probe requires Linux");
}
