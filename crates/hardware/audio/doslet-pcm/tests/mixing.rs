use doslet_pcm::{Error, Frame, Mixer};

#[test]
fn resampling_is_independent_of_packet_and_callback_partitioning() {
    for source_rate in [8000_u32, 11025, 22050, 44100, 48000] {
        for output_rate in [44100, 48000] {
            let frames: Vec<_> = (0..1000_u64)
                .map(|i| Frame {
                    nanos: i * 1_000_000_000 / u64::from(source_rate),
                    duration: 1_000_000_000_u32.div_ceil(source_rate),
                    samples: [if i & 1 == 0 { 16000 } else { -16000 }, 8000],
                })
                .collect();
            let mut whole = Mixer::new(output_rate).unwrap();
            whole.receive(&frames).unwrap();
            let mut expected = vec![[0.1; 2]; 2048];
            whole.mix(&mut expected);
            let mut streamed = Mixer::new(output_rate).unwrap();
            let mut actual = vec![[0.1; 2]; 2048];
            let mut used = 0;
            for batch in frames.chunks(17) {
                streamed.receive(batch).unwrap();
                let end = ((batch.last().unwrap().nanos as u128 * u128::from(output_rate))
                    / 1_000_000_000) as usize;
                let end = end.min(actual.len());
                for output in actual[used..end].chunks_mut(13) {
                    streamed.mix(output);
                }
                used = end;
                if used == actual.len() {
                    break;
                }
            }
            for output in actual[used..].chunks_mut(137) {
                streamed.mix(output);
            }
            assert_eq!(
                actual, expected,
                "source={source_rate}, output={output_rate}"
            );
        }
    }
}

#[test]
fn stereo_mixing_clips_and_stopped_playback_does_not_hold_a_dc_sample() {
    let mut mixer = Mixer::new(48000).unwrap();
    mixer
        .receive(&[Frame {
            nanos: 0,
            duration: 1_000_000,
            samples: [i16::MAX, i16::MIN],
        }])
        .unwrap();
    let mut output = [[0.5, -0.5]; 96];
    mixer.mix(&mut output);
    assert_eq!(&output[..48], &[[1.0, -1.0]; 48]);
    assert_eq!(&output[48..], &[[0.5, -0.5]; 48]);
    assert_eq!(mixer.nonzero(), 1);
}

#[test]
fn validation_and_overflow_are_atomic_reset_clears_old_audio_without_allocating() {
    let mut mixer = Mixer::new(48000).unwrap();
    let frames = [Frame {
        nanos: 0,
        duration: 1000,
        samples: [1; 2],
    }; 16384];
    mixer.receive(&frames).unwrap();
    assert_eq!(mixer.receive(&frames[..1]), Err(Error::Full));
    let measured = allocation_counter::measure(|| {
        mixer.reset(44100, 1_000_000).unwrap();
        let mut output = [[0.0; 2]; 128];
        mixer.mix(&mut output);
        assert_eq!(output, [[0.0; 2]; 128]);
        assert_eq!(
            mixer.receive(&[Frame {
                duration: 0,
                ..frames[0]
            }]),
            Err(Error::InvalidFrame)
        );
        assert_eq!(mixer.received(), 0);
        mixer
            .receive(&[Frame {
                nanos: 10_000_000,
                ..frames[0]
            }])
            .unwrap();
        assert_eq!(
            mixer.receive(&[
                Frame {
                    nanos: 11_000_000,
                    ..frames[0]
                },
                frames[0]
            ]),
            Err(Error::TimeReversed)
        );
        assert_eq!(mixer.received(), 1);
    });
    assert_eq!(measured.count_total, 0);
}
