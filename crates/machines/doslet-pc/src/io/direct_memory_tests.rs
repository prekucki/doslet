use super::*;

fn configured(kind: u8) -> PcBus {
    let mut bus = PcBus::default();
    bus.set_a20(true);
    if kind == 0 {
        bus.vga.set_vbe_101h(false);
        assert!(bus.vga.set_bank(7));
    } else {
        bus.vga.set_mode_13h();
    }
    for offset in 0..32 {
        assert!(bus.vga.write_memory(offset, (offset * 7 + 3) as u8));
    }
    if kind >= 2 {
        for (port, index, value) in [(0x3c4, 4, 6), (0x3ce, 4, 2)] {
            assert!(bus.vga.write_port(port, index));
            assert!(bus.vga.write_port(port + 1, value));
        }
    }
    if kind == 3 {
        for (index, value) in [(2, 3), (5, 0x48), (7, 5)] {
            assert!(bus.vga.write_port(0x3ce, index));
            assert!(bus.vga.write_port(0x3cf, value));
        }
    }
    bus
}

fn direct_byte(view: &doslet_vga::MemoryReadView<'_>, offset: usize, latches: &mut [u8; 4]) -> u8 {
    let Some(planar) = view.planar else {
        return view.bytes[view.bank_offset as usize + offset];
    };
    let group = if planar.chain4 {
        offset & !3
    } else {
        offset * 4
    };
    latches.copy_from_slice(&view.bytes[group..group + 4]);
    if let Some((compare, care)) = planar.compare {
        let mut value = 255;
        for (plane, &latch) in latches.iter().enumerate() {
            if care & (1 << plane) != 0 {
                value &= latch ^ if compare & (1 << plane) != 0 { 0 } else { 255 };
            }
        }
        value
    } else {
        latches[if planar.chain4 {
            offset & 3
        } else {
            planar.read_plane as usize
        }]
    }
}

#[test]
fn exported_video_controls_match_bus_values_latches_and_saved_access_counts() {
    for kind in 0..4 {
        let mut direct = configured(kind);
        let mut reference = configured(kind);
        for offset in [0usize, 1, 3, 7] {
            let backing = direct.vga.vram_view().as_ptr();
            let (value, latches) = {
                let view = direct.direct_memory_view().unwrap();
                let video = view.video.unwrap();
                assert_eq!(
                    video.bytes.as_ptr(),
                    backing,
                    "borrow the actual video backing"
                );
                assert_ne!(video.bytes.as_ptr(), reference.vga.vram_view().as_ptr());
                let mut latches = video.latches;
                let bytes =
                    core::array::from_fn(|index| direct_byte(&video, offset + index, &mut latches));
                (u32::from_le_bytes(bytes), latches)
            };
            direct.complete_direct_memory(4, latches, 0);
            assert_eq!(
                value,
                reference.read_u32(APERTURE_START + offset as u32).unwrap()
            );
            assert_eq!(direct.device_accesses, reference.device_accesses);
            assert_eq!(
                direct.vga.direct_read_view().unwrap().latches,
                reference.vga.direct_read_view().unwrap().latches
            );
            #[cfg(feature = "save-state")]
            {
                use doslet_state::State;
                assert!(
                    direct.save() == reference.save(),
                    "saved bus differs for read mode {kind}"
                );
            }
        }
    }
}

#[test]
fn foreign_ram_view_is_zero_copy_and_retains_generation_and_write_accounting() {
    let mut direct = PcBus::default();
    let mut reference = PcBus::default();
    direct.set_a20(true);
    reference.set_a20(true);
    let old = direct.code_check(0x1fff, 2).unwrap();
    let pointer = direct.ram_view(0x500..0x501).unwrap().as_ptr();
    {
        let view = direct.direct_memory_view().unwrap();
        assert_eq!(view.ram.bytes.as_ptr(), pointer);
        let index = 0x1fff - view.ram.linear_base as usize;
        view.ram.bytes[index..index + 2].copy_from_slice(&0x1234u16.to_le_bytes());
        view.ram.page_generations[1] += 1;
        view.ram.page_generations[2] += 1;
    }
    direct.complete_direct_memory(0, [0; 4], 2);
    reference.write_u16(0x1fff, 0x1234).unwrap();
    assert!(!direct.code_valid(&old));
    assert_eq!(direct.ram.writes(), reference.ram.writes());
    assert_eq!(
        direct.code_check(0x1fff, 2).unwrap().generations,
        reference.code_check(0x1fff, 2).unwrap().generations
    );
    assert_eq!(
        direct.ram_view(0x1fff..0x2001),
        reference.ram_view(0x1fff..0x2001)
    );
}

#[test]
fn foreign_views_decline_nonidentity_mapping_and_active_recording() {
    let mut bus = PcBus::default();
    assert!(bus.direct_memory_view().is_none());
    bus.set_a20(true);
    bus.begin_ram_write_log();
    assert!(bus.direct_memory_view().is_none());
    assert!(bus.end_ram_write_log());
    assert!(bus.direct_memory_view().is_some());
}
