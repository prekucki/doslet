use doslet_midi_routing::{Mode, Routing};
use doslet_mt32_gm::memory::{DISPLAY, Memory, TIMBRES};
fn dt1(address: u32, data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![
        0xF0,
        0x41,
        0x10,
        0x16,
        0x12,
        ((address >> 14) & 127) as u8,
        ((address >> 7) & 127) as u8,
        (address & 127) as u8,
    ];
    bytes.extend_from_slice(data);
    let sum = bytes[5..].iter().fold(0u8, |s, v| s.wrapping_add(*v));
    bytes.extend_from_slice(&[0u8.wrapping_sub(sum) & 127, 0xF7]);
    bytes
}
fn push(router: &mut Routing, bytes: &[u8], output: Mode) -> Vec<Vec<u8>> {
    let mut messages = Vec::new();
    for &byte in bytes {
        router
            .push(byte, output, &mut |m| messages.push(m.to_vec()))
            .unwrap();
    }
    messages
}

fn feed_gm(router: &mut Routing, synth: &mut doslet_midi_synth::Synth, bytes: &[u8]) {
    for message in push(router, bytes, Mode::Gm) {
        for byte in message {
            synth.push_byte(byte).unwrap();
        }
    }
}

#[test]
fn translated_reset_restores_a_channel_muted_by_expression() {
    let mut router = Routing::new(Mode::Mt32);
    let mut synth = doslet_midi_synth::Synth::new(48000).unwrap();
    feed_gm(
        &mut router,
        &mut synth,
        &[0xB1, 11, 0, 0xC1, 0, 0x91, 60, 100],
    );
    let mut samples = [[0.0; 2]; 1024];
    synth.render(&mut samples);
    assert!(samples.iter().flatten().all(|&sample| sample == 0.0));
    feed_gm(&mut router, &mut synth, &dt1(0x7F << 14, &[0]));
    feed_gm(&mut router, &mut synth, &[0x91, 64, 100]);
    synth.render(&mut samples);
    assert!(
        samples.iter().flatten().any(|sample| sample.abs() > 0.0001),
        "MT-32 reset must make subsequent GM notes audible again"
    );
}

#[test]
fn translated_reset_stops_notes_already_released_under_sustain() {
    let mut router = Routing::new(Mode::Mt32);
    let mut synth = doslet_midi_synth::Synth::new(48000).unwrap();
    feed_gm(
        &mut router,
        &mut synth,
        &[0xC1, 48, 0xB1, 64, 127, 0x91, 60, 100, 0x81, 60, 0],
    );
    let mut samples = [[0.0; 2]; 1024];
    synth.render(&mut samples);
    assert!(samples.iter().flatten().any(|sample| sample.abs() > 0.0001));
    feed_gm(&mut router, &mut synth, &dt1(0x7F << 14, &[0]));
    synth.render(&mut samples);
    assert!(
        samples.iter().flatten().all(|&sample| sample == 0.0),
        "MT-32 reset must also silence notes retained by the GM sustain pedal"
    );
}

#[test]
fn activity_counts_guest_notes_with_running_status_but_not_replay_or_note_off() {
    let mut router = Routing::new(Mode::Gm);
    push(&mut router, &[0x90, 60], Mode::Gm);
    assert_eq!(router.input_note_events(), 0);
    push(
        &mut router,
        &[100, 62, 0xF8, 90, 60, 0, 0x80, 62, 64],
        Mode::Gm,
    );
    assert_eq!(router.input_note_events(), 2);
    router.replay(Mode::Gm, &mut |_| {});
    router.set_mode(Mode::Mt32);
    router.replay(Mode::Mt32, &mut |_| {});
    assert_eq!(router.input_note_events(), 2);
    push(&mut router, &[0x91, 65, 100], Mode::Gm);
    assert_eq!(router.input_note_events(), 3);
}

#[test]
fn gm_passes_through_but_roland_upgrades_before_a_gm_sink_sees_sysex() {
    let mut router = Routing::new(Mode::Gm);
    assert_eq!(
        push(&mut router, &[0x90, 60, 90], Mode::Gm),
        vec![vec![0x90, 60, 90]]
    );
    let messages = push(&mut router, &dt1(DISPLAY, b"Civilization"), Mode::Gm);
    assert_eq!(router.mode(), Mode::Mt32);
    assert!(router.upgraded());
    assert!(messages.iter().all(|m| m[0] != 0xF0));
    assert_eq!(&router.translator().display()[..12], b"Civilization");
    let notes = push(&mut router, &[0x91, 60, 90], Mode::Gm);
    assert!(notes.contains(&vec![0x90, 60, 90]));
}
#[test]
fn mt32_raw_sink_keeps_sysex_and_switching_replays_parameters_and_active_notes() {
    let mut router = Routing::new(Mode::Mt32);
    let message = dt1(DISPLAY, b"LCD");
    assert_eq!(
        push(&mut router, &message, Mode::Mt32),
        vec![message.clone()]
    );
    push(&mut router, &[0x91, 60, 90], Mode::Mt32);
    let mut gm = Vec::new();
    router.replay(Mode::Gm, &mut |m| gm.push(m.to_vec()));
    assert!(gm.iter().all(|m| m[0] != 0xF0));
    assert!(gm.contains(&vec![0x90, 60, 90]));
    assert!(gm.contains(&vec![0xB0, 6, 12]));
    let mut raw = Vec::new();
    router.replay(Mode::Mt32, &mut |m| raw.push(m.to_vec()));
    assert!(raw.contains(&message));
    assert!(raw.contains(&vec![0x91, 60, 90]));
    // An incomplete wire message stays in the shared decoder during switching.
    push(&mut router, &[0x81, 60], Mode::Gm);
    router.replay(Mode::Mt32, &mut |_| {});
    assert!(push(&mut router, &[0], Mode::Gm).contains(&vec![0x80, 60, 0]));
}
#[test]
fn written_memory_replay_exceeds_old_capacity_and_is_bounded_and_checksum_valid() {
    let mut memory = Memory::new(&doslet_mt32_gm::tables::POWER_ON_PROGRAMS);
    let mut packets = Vec::new();
    memory.replay(&mut |m| packets.push(m.to_vec()));
    assert!(packets.is_empty());
    let data: Vec<u8> = (0..16384).map(|i| (i % 128) as u8).collect();
    memory.write(TIMBRES, &data);
    memory.replay(&mut |m| packets.push(m.to_vec()));
    assert!(packets.iter().map(Vec::len).sum::<usize>() > 16384);
    let mut restored = Memory::new(&doslet_mt32_gm::tables::POWER_ON_PROGRAMS);
    for packet in packets {
        assert!(packet.len() <= 266);
        assert_eq!(
            packet[5..packet.len() - 1]
                .iter()
                .fold(0u8, |s, v| s.wrapping_add(*v))
                & 127,
            0
        );
        let address = doslet_mt32_gm::memory::memaddr(packet[5], packet[6], packet[7]);
        restored.write(address, &packet[8..packet.len() - 2]);
    }
    assert_eq!(memory.timbres, restored.timbres);
}
#[test]
fn translation_and_replay_allocate_nothing_after_setup() {
    let mut router = Routing::new(Mode::Mt32);
    let message = dt1(DISPLAY, b"No allocation");
    let allocations = allocation_counter::measure(|| {
        for &byte in &message {
            router.push(byte, Mode::Gm, &mut |_| {}).unwrap();
        }
        router.replay(Mode::Gm, &mut |_| {});
        router.replay(Mode::Mt32, &mut |_| {});
    });
    assert_eq!(allocations.count_total, 0);
}
#[test]
fn reset_clears_notes_and_written_regions_in_raw_replay() {
    let mut router = Routing::new(Mode::Mt32);
    push(&mut router, &dt1(DISPLAY, b"before reset"), Mode::Mt32);
    push(&mut router, &[0x91, 60, 90], Mode::Mt32);
    push(&mut router, &dt1(0x7f << 14, &[1]), Mode::Mt32);
    let mut replay = Vec::new();
    router.replay(Mode::Mt32, &mut |m| replay.push(m.to_vec()));
    assert!(!replay.iter().any(|m| m[0] & 0xF0 == 0x90));
    assert_eq!(replay.iter().filter(|m| m[0] == 0xF0).count(), 1);
}

#[test]
fn fragmented_guest_memory_snapshot_fits_the_declared_transport_capacity() {
    let mut memory = Memory::new(&doslet_mt32_gm::tables::POWER_ON_PROGRAMS);
    for i in (0..16384).step_by(2) {
        memory.write(TIMBRES + i, &[42]);
    }
    let mut bytes = 0;
    memory.replay(&mut |m| bytes += m.len());
    assert!(bytes > 65536);
    assert!(
        bytes + doslet_midi_synth::wire::REPLAY_CAPACITY < doslet_midi_routing::SNAPSHOT_CAPACITY
    );
}

#[test]
fn channel_monitor_follows_sustain_expression_and_receiver_dialect() {
    let mut router = Routing::new(Mode::Mt32);
    push(
        &mut router,
        &[
            0xC1, 48, 0xB1, 7, 127, 11, 0, 64, 127, 0x91, 60, 100, 0x81, 60, 0,
        ],
        Mode::Mt32,
    );
    let raw = router.channels(Mode::Mt32);
    assert!(!raw[0].enabled);
    assert_eq!(raw[1].state.program, 48);
    assert_eq!(&raw[1].instrument[..10], b"Str Sect 1");
    assert_eq!(raw[1].state.volume, 127);
    assert_eq!(raw[1].state.expression, 0);
    assert_eq!(raw[1].state.notes, 1);
    assert_eq!(raw[1].state.velocity, 100);
    assert!(raw[1].state.sustain);
    let gm = router.channels(Mode::Gm);
    assert_eq!(gm[0].state.notes, 1);
    assert_eq!(gm[0].state.expression, 0);
    for channel in raw {
        let mut bytes = [0; doslet_midi_routing::Channel::BYTES];
        channel.write(&mut bytes);
        assert_eq!(doslet_midi_routing::Channel::read(&bytes), Some(channel));
    }
    push(&mut router, &[0xB1, 64, 0], Mode::Mt32);
    assert_eq!(router.channels(Mode::Mt32)[1].state.notes, 0);
    push(&mut router, Mode::Mt32.reset_message(), Mode::Mt32);
    assert_eq!(router.channels(Mode::Mt32)[1].state.expression, 127);
}

#[test]
fn channel_monitor_tracks_sysex_assignments_and_master_volume_without_allocating() {
    use doslet_mt32_gm::memory::{CHANNEL_ASSIGN, MASTER_VOLUME, SYSTEM};
    let mut router = Routing::new(Mode::Mt32);
    push(
        &mut router,
        &dt1(SYSTEM + CHANNEL_ASSIGN as u32, &[15]),
        Mode::Mt32,
    );
    push(
        &mut router,
        &dt1(SYSTEM + MASTER_VOLUME as u32, &[0]),
        Mode::Mt32,
    );
    push(&mut router, &[0x9F, 60, 100], Mode::Mt32);
    let count = allocation_counter::measure(|| {
        let channels = router.channels(Mode::Mt32);
        assert!(!channels[1].enabled);
        assert!(channels[15].enabled);
        assert_eq!(channels[15].state.notes, 1);
        assert_eq!(channels[15].state.volume, 0);
    });
    assert_eq!(count.count_total, 0);
}

#[test]
fn gm_reset_does_not_replay_stale_muted_channels_after_an_output_switch() {
    let mut router = Routing::new(Mode::Gm);
    push(
        &mut router,
        &[
            0xC0, 48, 0xB0, 7, 0, 11, 0, 64, 127, 0x90, 60, 100, 0x80, 60, 0,
        ],
        Mode::Gm,
    );
    assert_eq!(router.channels(Mode::Gm)[0].state.expression, 0);
    push(&mut router, Mode::Gm.reset_message(), Mode::Gm);
    let mut replay = Vec::new();
    router.replay(Mode::Gm, &mut |m| replay.push(m.to_vec()));
    assert!(replay.contains(&vec![0xB0, 7, 100]));
    assert!(replay.contains(&vec![0xB0, 11, 127]));
    assert!(replay.contains(&vec![0xC0, 0]));
    assert!(!replay.iter().any(|m| m[0] & 0xF0 == 0x90));
    assert_eq!(router.channels(Mode::Gm)[0].state.notes, 0);

    let mut mt32 = Routing::new(Mode::Mt32);
    push(&mut mt32, &[0x91, 60, 100], Mode::Mt32);
    push(&mut mt32, Mode::Gm.reset_message(), Mode::Mt32);
    assert_eq!(mt32.channels(Mode::Mt32)[1].state.notes, 1);
}
