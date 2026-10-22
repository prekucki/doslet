use super::*;
use doslet_bus::{MemoryBus, PortBus};
#[allow(dead_code)]
mod fixture {
    include!("../../../../tests/fixtures/opl3.rs");
}

fn write(machine: &mut PcMachine, register: u16, value: u8) {
    let port = 0x388 + (register >> 8) * 2;
    machine.bus.write_port_u8(port, register as u8).unwrap();
    machine.bus.write_port_u8(port + 1, value).unwrap();
}
fn tone(machine: &mut PcMachine) {
    for (register, value) in fixture::OPL_TONE {
        write(machine, register, value);
    }
}
fn full_volume(machine: &mut PcMachine) {
    for register in [0x30, 0x31, 0x34, 0x35] {
        machine.bus.write_port_u8(0x224, register).unwrap();
        machine.bus.write_port_u8(0x225, 0xF8).unwrap();
    }
}

#[test]
fn adlib_and_sb_aliases_share_timer_and_register_latches() {
    let mut m = PcMachine::default();
    m.bus.write_port_u8(0x388, 2).unwrap();
    m.bus.write_port_u8(0x221, 0xFF).unwrap();
    m.bus.write_port_u8(0x228, 4).unwrap();
    m.bus.write_port_u8(0x389, 1).unwrap();
    m.advance_hardware_steps(659).unwrap();
    assert_eq!(m.bus.read_port_u8(0x220), Ok(0));
    m.advance_hardware_steps(1).unwrap(); // 80 us.
    assert_eq!(m.bus.read_port_u8(0x388), Ok(0xC0));
    assert_eq!(m.bus.read_port_u8(0x228), Ok(0xC0));
    let mut pcm = [[0; 2]; 128];
    assert_eq!(m.drain_pcm(&mut pcm), 0);
    let mut disabled = PcMachine::from_config(doslet_config::HardwareConfig {
        sb16: false,
        ..Default::default()
    })
    .unwrap();
    for port in [
        0x220, 0x221, 0x222, 0x223, 0x228, 0x229, 0x388, 0x389, 0x38A, 0x38B,
    ] {
        assert!(disabled.bus.read_port_u8(port).is_err());
        assert!(disabled.bus.write_port_u8(port, 0).is_err());
    }
}

#[test]
#[cfg(feature = "save-state")]
fn observed_and_restored_fm_polls_keep_the_same_bus_and_timer_time() {
    use doslet_state::State;
    let mut ordinary = PcMachine::default();
    let mut observed = PcMachine::default();
    for machine in [&mut ordinary, &mut observed] {
        for (address, byte) in [0xec, 0xeb, 0xfd].into_iter().enumerate() {
            machine.bus.write_u8(address as u32, byte).unwrap();
        }
        machine.cpu.registers.set(doslet_x86::Gpr16::Dx, 0x388);
        write(machine, 2, 0xff);
        write(machine, 4, 0x21);
    }
    for step in 0..200 {
        ordinary.step().unwrap();
        observed
            .step_observed(&mut StepObservation::default())
            .unwrap();
        assert_eq!(ordinary.time(), observed.time());
        assert_eq!(ordinary.cpu.registers.ip, observed.cpu.registers.ip);
        assert_eq!(
            ordinary.cpu.registers.get(doslet_x86::Gpr16::Ax),
            observed.cpu.registers.get(doslet_x86::Gpr16::Ax)
        );
        if step == 50 {
            observed = PcMachine::load(observed.save()).unwrap();
        }
    }
    assert_eq!(
        ordinary.cpu.registers.get(doslet_x86::Gpr16::Ax) as u8,
        0xc0
    );
    assert!(ordinary.time().as_nanos() >= 100_000);
}

#[test]
fn absent_pas_filter_probe_does_not_touch_opl_bank_one_or_hide_other_ports() {
    let mut m = PcMachine::default();
    // Deliberately leave OPL bank one pointing at NEW before the PAS probe.
    m.bus.write_port_u8(0x38A, 5).unwrap();
    m.bus.write_port_u8(0x38B, 1).unwrap();
    for port in [0xB8A, 0xB8B] {
        m.bus.write_port_u8(port, 0).unwrap();
        assert_eq!(m.bus.read_port_u8(port), Ok(0xFF));
    }
    assert!(m.bus.write_port_u8(0xB89, 0).is_err());
    assert!(m.bus.write_port_u8(0xB8C, 0).is_err());
    // Actual OPL timer detection remains available following the PAS write.
    write(&mut m, 2, 0xFF);
    write(&mut m, 4, 1);
    m.advance_hardware_steps(660).unwrap();
    assert_eq!(m.bus.read_port_u8(0x388), Ok(0xC0));
}

#[test]
fn simultaneous_fm_and_dma_are_summed_on_one_ordered_pcm_timeline() {
    let mut fm = PcMachine::default();
    let mut mixed = PcMachine::default();
    mixed.capture_audio_sources(true);
    // Existing DMA fixture emits one signed mono sample (1234h) at 1 kHz.
    for m in [&mut fm, &mut mixed] {
        full_volume(m);
        tone(m);
    }
    mixed.bus.write_u16(0x10000, 0x1234).unwrap();
    for (port, value) in [
        (0xD8, 0),
        (0xC4, 0),
        (0xC4, 0x80),
        (0xC6, 0),
        (0xC6, 0),
        (0x8B, 1),
        (0xD6, 0x59),
        (0xD4, 1),
    ] {
        mixed.bus.write_port_u8(port, value).unwrap();
    }
    for value in [0x41, 3, 0xE8, 0xB0, 0x10, 0, 0] {
        mixed.bus.write_port_u8(0x22C, value).unwrap();
    }
    let mut a = [doslet_pcm::EMPTY; 256];
    let mut b = a;
    for _ in 0..33000 {
        fm.advance_hardware_steps(1).unwrap();
        mixed.advance_hardware_steps(1).unwrap();
    }
    let count = fm.drain_pcm_timed(&mut a);
    let other = mixed.drain_pcm_timed(&mut b);
    assert!(count > 100);
    assert_eq!(count, other);
    let mut digital_samples = 0;
    for (a, b) in a[..count].iter().zip(&b[..count]) {
        assert_eq!(a.nanos, b.nanos);
        assert_eq!(a.duration, b.duration);
        if (1_000_000..2_000_000).contains(&a.nanos) {
            digital_samples += 1;
            assert_eq!(b.samples, a.samples.map(|s| s.saturating_add(0x1234)));
        } else {
            assert_eq!(a.samples, b.samples);
        }
    }
    assert_eq!(digital_samples, 48);
    assert!(b[..count].windows(2).all(|p| p[0].nanos < p[1].nanos));
    let mut sources = [doslet_pcm::SourceFrame::default(); 256];
    let pcm_count = mixed.drain_audio_source(false, &mut sources);
    assert_eq!(pcm_count, 1);
    assert_eq!(sources[0].rate, 1000);
    assert_eq!(sources[0].samples, [0x1234; 2]);
    let fm_count = mixed.drain_audio_source(true, &mut sources);
    assert!(fm_count >= count);
    for frame in &sources[..fm_count] {
        assert_eq!(frame.rate, doslet_opl3::SAMPLE_RATE);
        if let Some(expected) = a[..count].iter().find(|f| f.nanos == frame.nanos) {
            assert_eq!(
                frame.samples, expected.samples,
                "FM tap must not contain DMA PCM"
            );
        } else {
            assert_eq!(frame.samples, [0; 2]);
        }
    }
}

#[test]
fn external_device_slices_preserve_fm_samples_and_timestamps() {
    let mut single = PcMachine::default();
    let mut external = PcMachine::default();
    for m in [&mut single, &mut external] {
        tone(m);
    }
    for _ in 0..30 {
        for _ in 0..8250 {
            single.advance_hardware_steps(1).unwrap();
        }
        assert_eq!(
            external.debug_advance_device_time(1_000_000).unwrap(),
            1_000_000
        );
    }
    let mut a = [doslet_pcm::EMPTY; 2048];
    let mut b = a;
    let count = single.drain_pcm_timed(&mut a);
    assert_eq!(count, external.drain_pcm_timed(&mut b));
    assert_eq!(a, b);
    assert!(count > 1000);
    assert!(a[..count].iter().any(|frame| frame.samples != [0; 2]));
}

#[test]
fn fm_idle_batches_match_single_steps_and_queue_overflow_is_explicit() {
    let mut single = PcMachine::default();
    let mut batched = PcMachine::default();
    for m in [&mut single, &mut batched] {
        tone(m);
    }
    for _ in 0..30 {
        for _ in 0..8250 {
            single.advance_hardware_steps(1).unwrap();
        }
        batched.advance_hardware_steps(8250).unwrap();
    }
    let mut a = [doslet_pcm::EMPTY; 2048];
    let mut b = a;
    let count = single.drain_pcm_timed(&mut a);
    assert_eq!(count, batched.drain_pcm_timed(&mut b));
    assert_eq!(a, b);
    assert!(count > 1000);
    let mut result = Ok(());
    for _ in 0..100 {
        result = batched.advance_hardware_steps(8250);
        if result.is_err() {
            break;
        }
    }
    assert_eq!(result, Err(MachineError::AudioQueueFull));
}

#[test]
fn adlib_and_sb_modes_distinguish_opl2_from_opl3_and_route_8bit_dma() {
    let mut adlib = PcMachine::from_config(
        doslet_config::HardwareConfig::default().with_sound_card(doslet_config::SoundCard::Adlib),
    )
    .unwrap();
    assert_eq!(adlib.audio_status().irq, None);
    assert_eq!(adlib.audio_status().dma8, None);
    assert_eq!(adlib.audio_status().dma16, None);
    assert_eq!(adlib.bus.read_port_u8(0x388), Ok(0x06));
    assert_eq!(adlib.bus.read_port_u8(0x38A), Ok(0xFF));
    assert_eq!(adlib.bus.read_port_u8(0x226), Ok(0xFF));
    write(&mut adlib, 2, 0xFF);
    write(&mut adlib, 4, 0x21);
    adlib.advance_hardware_steps(660).unwrap();
    assert_eq!(adlib.bus.read_port_u8(0x388), Ok(0xC6));

    let mut sb = PcMachine::from_config(
        doslet_config::HardwareConfig::default().with_sound_card(doslet_config::SoundCard::Sb),
    )
    .unwrap();
    assert_eq!(sb.audio_status().irq, Some(7));
    assert_eq!(sb.audio_status().dma8, Some(1));
    assert_eq!(sb.audio_status().dma16, None);
    assert_eq!(sb.bus.read_port_u8(0x220), Ok(0x06));
    assert_eq!(sb.bus.read_port_u8(0x224), Ok(0xFF));
    sb.bus.write_port_u8(0x22C, 0xE1).unwrap();
    assert_eq!(sb.bus.read_port_u8(0x22A), Ok(2));
    assert_eq!(sb.bus.read_port_u8(0x22A), Ok(1));
}
