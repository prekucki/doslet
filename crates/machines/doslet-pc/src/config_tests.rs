use crate::*;
use doslet_bus::{MemoryBus, PortBus};
use doslet_config::{HardwareConfig, KeyboardMode};
use doslet_input::PhysicalKey;

#[test]
fn mixer_errors_identify_the_selected_register_and_wave_blaster_mask_is_routed() {
    let mut machine = PcMachine::default();
    machine.bus.write_port_u8(0x224, 0x83).unwrap();
    let original = machine.bus.read_port_u8(0x225).unwrap();
    machine.bus.write_port_u8(0x225, 0x0b).unwrap();
    assert_eq!(machine.bus.read_port_u8(0x225), Ok(0x0b));
    assert_eq!(machine.audio_status().irq_enabled_sources, 3);
    machine.bus.write_port_u8(0x225, original).unwrap();
    machine.bus.write_port_u8(0x224, 0x7f).unwrap();
    let error = doslet_bus::BusError::UnsupportedPortRegister {
        port: 0x225,
        index: 0x7f,
    };
    assert_eq!(machine.bus.read_port_u8(0x225), Err(error));
    assert_eq!(machine.bus.write_port_u8(0x225, 0), Err(error));
}

#[test]
fn live_keyboard_mode_keeps_controller_configuration_and_pic_masks() {
    let mut m = PcMachine::default();
    m.bus.write_port_u8(0x21, 0x55).unwrap();
    m.bus.write_port_u8(0x64, 0x60).unwrap();
    m.bus.write_port_u8(0x60, 0x41).unwrap();
    m.set_keyboard_mode(KeyboardMode::At).unwrap();
    m.key_event(PhysicalKey(4), true).unwrap();
    assert_eq!(m.bus.read_pending_keyboard(), Some(0x1e));
    let time = m.time();
    m.set_keyboard_mode(KeyboardMode::Bios).unwrap();
    assert_eq!(m.bus.read_pending_keyboard(), Some(0x9e));
    assert_eq!(m.bus.read_port_u8(0x21), Ok(0x55));
    assert_eq!(m.time(), time);
    m.set_keyboard_mode(KeyboardMode::At).unwrap();
    m.bus.write_port_u8(0x64, 0x20).unwrap();
    assert_eq!(m.bus.read_pending_keyboard(), Some(0x41));
    m.key_event(PhysicalKey(4), true).unwrap();
    assert_eq!(m.bus.read_pending_keyboard(), Some(0x1e));
}

#[test]
fn native_ram_views_follow_each_configured_capacity_and_page_table() {
    for memory_mib in [4, 8, 16, 32] {
        let mut m = PcMachine::from_config(HardwareConfig {
            memory_mib,
            ..HardwareConfig::default()
        })
        .unwrap();
        assert!(m.bus.native_ram().is_none());
        assert!(m.bus.native_ram_mut().is_none());
        m.bus.set_a20(true);
        let end = usize::from(memory_mib) * 1024 * 1024;
        m.bus.write_u32(end as u32 - 4, 0xA1B2C3D4).unwrap();
        {
            let view = m
                .bus
                .native_ram()
                .expect("configured RAM must support native reads");
            assert_eq!(view.linear_base, 0x500);
            assert_eq!(view.bytes.len(), end - 0x500);
            assert_eq!(
                &view.bytes[view.bytes.len() - 4..],
                &0xA1B2C3D4_u32.to_le_bytes()
            );
            assert_eq!(view.excluded_start, 0xA0000);
            assert_eq!(view.excluded_len, 0x10000);
        }
        {
            let view = m
                .bus
                .native_ram_mut()
                .expect("configured RAM must support native writes");
            assert_eq!(view.linear_base, 0x500);
            assert_eq!(view.bytes.len(), end - 0x500);
            assert_eq!(view.page_generations.len(), end / 4096);
            assert_eq!(view.excluded_start, 0xA0000);
            assert_eq!(view.excluded_len, 0x10000);
        }
        m.bus.set_a20(false);
        assert!(m.bus.native_ram().is_none());
        assert!(m.bus.native_ram_mut().is_none());
    }
}

#[test]
fn every_ram_capacity_matches_cmos_and_last_wide_transaction() {
    for memory_mib in [4, 8, 16, 32] {
        let mut m = PcMachine::from_config(HardwareConfig {
            memory_mib,
            x87: false,
            ..HardwareConfig::default()
        })
        .unwrap();
        assert_eq!(m.bus.ram_size(), memory_mib as usize * 1024 * 1024);
        m.bus.set_a20(true);
        let end = m.bus.ram_size() as u32;
        m.bus.write_u32(end - 4, 0x12345678).unwrap();
        assert_eq!(m.bus.read_u32(end - 4), Ok(0x12345678));
        assert!(m.bus.write_u32(end - 2, 0xAABBCCDD).is_err());
        assert_eq!(m.bus.read_u16(end - 2), Ok(0xCCDD));
        m.bus.cmos.select(0x17);
        let low = m.bus.cmos.read();
        m.bus.cmos.select(0x18);
        let high = m.bus.cmos.read();
        assert_eq!(
            u16::from_le_bytes([low, high]),
            u16::from(memory_mib - 1) * 1024
        );
        m.bus.cmos.select(0x14);
        assert_eq!(m.bus.cmos.read() & 2, 0);
    }
}
#[test]
fn disabled_cards_lose_ports_and_scheduling_and_config_is_validated() {
    let config = HardwareConfig {
        sb16: false,
        mpu401: false,
        gameport: false,
        sb_irq: 255,
        sb_dma8: 255,
        sb_dma16: 255,
        ..HardwareConfig::default()
    };
    let mut m = PcMachine::from_config(config).unwrap();
    for port in [
        0x224, 0x225, 0x226, 0x22A, 0x22C, 0x22E, 0x22F, 0x330, 0x331, 0x201,
    ] {
        assert!(m.bus.read_port_u8(port).is_err());
        assert!(m.bus.write_port_u8(port, 0).is_err());
    }
    assert_eq!(m.audio_status().irq, None);
    assert_eq!(m.audio_status().dma8, None);
    assert!(matches!(
        PcMachine::from_config(HardwareConfig {
            memory_mib: 12,
            ..config
        }),
        Err(MachineError::Hardware(_))
    ));
    assert!(matches!(
        PcMachine::from_config(HardwareConfig {
            sb_irq: 2,
            ..HardwareConfig::default()
        }),
        Err(MachineError::Hardware(_))
    ));
}
#[test]
fn configured_sound_grants_are_immutable_under_unsupported_guest_requests() {
    let mut m = PcMachine::from_config(HardwareConfig {
        sb_irq: 5,
        sb_dma8: 3,
        sb_dma16: 6,
        ..HardwareConfig::default()
    })
    .unwrap();
    assert_eq!(m.audio_status().irq, Some(5));
    assert_eq!(m.audio_status().dma8, Some(3));
    assert_eq!(m.audio_status().dma16, Some(6));
    m.bus.write_port_u8(0x224, 0x80).unwrap();
    assert_eq!(m.bus.read_port_u8(0x225), Ok(2));
    for value in [1, 4, 8, 0, 6] {
        assert!(m.bus.write_port_u8(0x225, value).is_err());
        assert_eq!(m.bus.read_port_u8(0x225), Ok(2));
    }
    m.bus.write_port_u8(0x224, 0x81).unwrap();
    assert_eq!(m.bus.read_port_u8(0x225), Ok(0x48));
    assert!(m.bus.write_port_u8(0x225, 0x22).is_err());
    assert_eq!(m.bus.read_port_u8(0x225), Ok(0x48));
}
#[test]
fn at_irq_edges_only_rearm_when_output_advances_and_bios_mode_is_exclusive() {
    let mut m = PcMachine::from_config(HardwareConfig {
        keyboard: KeyboardMode::At,
        ..HardwareConfig::default()
    })
    .unwrap();
    m.key_event(PhysicalKey(4), true).unwrap();
    assert_eq!(m.bus.pic.acknowledge(), Some(9));
    for _ in 0..3 {
        m.bus.update_keyboard_irq();
    }
    assert_eq!(m.bus.pic.read_command(), 0);
    assert_eq!(m.bus.read_pending_keyboard(), Some(0x1E));
    m.bus.pic.write_command(0x20);
    assert_eq!(m.bus.pic.acknowledge(), None);
    m.key_event(PhysicalKey(79), true).unwrap();
    assert_eq!(m.bus.pic.acknowledge(), Some(9));
    assert_eq!(m.bus.read_pending_keyboard(), Some(0xE0));
    m.bus.pic.write_command(0x20);
    assert_eq!(m.bus.pic.acknowledge(), Some(9));
    assert_eq!(m.bus.read_pending_keyboard(), Some(0x4D));
    m.bus.pic.write_command(0x20);
    m.release_keys().unwrap();
    assert_eq!(m.bus.read_pending_keyboard(), Some(0x9E));
    assert_eq!(m.bus.read_pending_keyboard(), Some(0xE0));
    assert_eq!(m.bus.read_pending_keyboard(), Some(0xCD));
    let mut bios = PcMachine::default();
    bios.key_event(PhysicalKey(4), true).unwrap();
    assert_eq!(bios.bus.read_pending_keyboard(), None);
    assert_eq!(bios.bus.pic.acknowledge(), None);
}
#[test]
fn gameport_deadlines_are_independent_of_number_of_host_reads() {
    let mut m = PcMachine::from_config(HardwareConfig {
        gameport: true,
        ..HardwareConfig::default()
    })
    .unwrap();
    m.set_joystick([-32767, 32767], [true, false]);
    m.bus.write_port_u8(0x201, 0).unwrap();
    for _ in 0..1000 {
        assert_eq!(m.bus.read_port_u8(0x201), Ok(0xEF));
    }
    m.advance_hardware_steps(199).unwrap();
    assert_eq!(m.bus.read_port_u8(0x201), Ok(0xEE));
    m.advance_hardware_steps(8250).unwrap();
    m.advance_hardware_steps(8250).unwrap();
    assert_eq!(m.bus.read_port_u8(0x201), Ok(0xEC));
}
