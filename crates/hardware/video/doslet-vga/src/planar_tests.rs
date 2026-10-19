use super::*;
use scanout::PixelLayout;

fn register(vga: &mut Vga, port: u16, index: u8, value: u8) {
    assert!(vga.write_port(port, index));
    assert!(vga.write_port(port + 1, value));
}

fn mode_x() -> Vga {
    let mut vga = Vga::default();
    vga.set_mode_13h();
    register(&mut vga, 0x3C4, 4, 6); // Chain-4 off, odd/even off.
    register(&mut vga, 0x3D4, 0x14, 0);
    register(&mut vga, 0x3D4, 0x17, 0xE3); // Byte addressing.
    vga
}

#[test]
fn plane_mask_and_read_map_select_independent_256_color_pixels() {
    let mut vga = mode_x();
    for plane in 0..4 {
        register(&mut vga, 0x3C4, 2, 1 << plane);
        assert!(vga.write_memory(80, 10 + plane));
    }
    assert_eq!(&vga.vram_view()[320..324], &[10, 11, 12, 13]);
    for plane in 0..4 {
        register(&mut vga, 0x3CE, 4, plane);
        assert_eq!(vga.read_memory(80), Some(10 + plane));
    }
    register(&mut vga, 0x3C4, 2, 5);
    vga.write_memory(80, 42);
    assert_eq!(&vga.vram_view()[320..324], &[42, 11, 42, 13]);
    register(&mut vga, 0x3C4, 2, 0);
    vga.write_memory(80, 99);
    assert_eq!(&vga.vram_view()[320..324], &[42, 11, 42, 13]);
    assert_eq!(vga.read_memory(APERTURE_LEN), None);
    assert!(!vga.write_memory(APERTURE_LEN, 1));
}

#[test]
fn chain4_and_unchained_access_share_the_same_vram_owner() {
    let mut vga = Vga::default();
    vga.set_mode_13h();
    for offset in 0..8 {
        vga.write_memory(offset, offset as u8 + 1);
    }
    register(&mut vga, 0x3C4, 4, 6);
    register(&mut vga, 0x3CE, 4, 3);
    assert_eq!(vga.read_memory(0), Some(4));
    assert_eq!(vga.read_memory(1), Some(8));
    register(&mut vga, 0x3C4, 2, 2);
    vga.write_memory(1, 99);
    register(&mut vga, 0x3C4, 4, 14);
    assert_eq!(vga.read_memory(5), Some(99));
    // Chain-4 still honors the plane write mask.
    vga.write_memory(4, 111);
    assert_eq!(vga.read_memory(4), Some(5));
}

#[test]
fn latches_copy_all_planes_and_preserve_masked_bits() {
    let mut vga = mode_x();
    for plane in 0..4 {
        register(&mut vga, 0x3C4, 2, 1 << plane);
        vga.write_memory(7, 0xA0 + plane);
    }
    vga.read_memory(7); // Loads all four latches, independently of read map.
    register(&mut vga, 0x3CE, 5, 0x41);
    register(&mut vga, 0x3C4, 2, 15);
    vga.write_memory(8, 0);
    assert_eq!(&vga.vram_view()[32..36], &[0xA0, 0xA1, 0xA2, 0xA3]);
    register(&mut vga, 0x3CE, 5, 0x40);
    register(&mut vga, 0x3CE, 8, 0x0F);
    vga.write_memory(9, 5);
    assert_eq!(&vga.vram_view()[36..40], &[0xA5; 4]);
}

#[test]
fn write_modes_two_three_and_read_comparison_use_plane_bits() {
    let mut vga = mode_x();
    register(&mut vga, 0x3C4, 2, 15);
    register(&mut vga, 0x3CE, 5, 0x42);
    vga.write_memory(0, 5);
    assert_eq!(&vga.vram_view()[..4], &[255, 0, 255, 0]);
    register(&mut vga, 0x3CE, 2, 5);
    register(&mut vga, 0x3CE, 7, 15);
    register(&mut vga, 0x3CE, 5, 0x48);
    assert_eq!(vga.read_memory(0), Some(255));
    register(&mut vga, 0x3CE, 2, 4);
    assert_eq!(vga.read_memory(0), Some(0));
    register(&mut vga, 0x3CE, 5, 0x40);
    vga.write_memory(1, 15);
    vga.read_memory(1);
    register(&mut vga, 0x3CE, 5, 0x43);
    register(&mut vga, 0x3CE, 0, 6);
    register(&mut vga, 0x3CE, 3, 1);
    register(&mut vga, 0x3CE, 8, 0xF0);
    vga.write_memory(2, 0x80);
    assert_eq!(&vga.vram_view()[8..12], &[0x0F, 0x4F, 0x4F, 0x0F]);
}

#[test]
fn page_start_pitch_and_wrapping_drive_borrowed_scanout() {
    let mut vga = mode_x();
    register(&mut vga, 0x3C4, 2, 15);
    vga.write_memory(0xFFFF, 3);
    vga.write_memory(0, 4);
    vga.write_memory(0xFF, 5);
    register(&mut vga, 0x3D4, 0x0C, 0xFF);
    register(&mut vga, 0x3D4, 0x0D, 0xFF);
    register(&mut vga, 0x3D4, 0x13, 0x80); // Pitch = 256 plane bytes.
    let frame = vga.frame().unwrap();
    assert_eq!((frame.width, frame.height), (320, 200));
    assert!(matches!(frame.layout, PixelLayout::Vga256 { .. }));
    assert_eq!(frame.pixels.as_ptr(), vga.vram_view().as_ptr());
    let indices: alloc::vec::Vec<_> = frame.indices().collect();
    assert_eq!(&indices[..8], &[3, 3, 3, 3, 4, 4, 4, 4]);
    assert_eq!(&indices[320..324], &[5; 4]);
    let mut rgba = vec![0; 320 * 200 * 4];
    frame.write_rgba(&mut rgba).unwrap();
    assert_eq!(&rgba[..4], &frame.palette[3]);
    let revision = frame.revision;
    register(&mut vga, 0x3D4, 0x0D, 0);
    assert_ne!(vga.frame().unwrap().revision, revision);
}

#[test]
fn mode_x_240_line_geometry_and_register_only_page_flips_are_visible() {
    let mut vga = mode_x();
    register(&mut vga, 0x3D4, 0x11, 0); // Unlock overflow register.
    register(&mut vga, 0x3D4, 7, 0x3E);
    register(&mut vga, 0x3D4, 0x12, 0xDF); // 480 raster lines / two repeats.
    assert_eq!(vga.frame().unwrap().height, 240);
    let revision = vga.frame().unwrap().revision;
    register(&mut vga, 0x3C4, 2, 1);
    assert_eq!(vga.frame().unwrap().revision, revision);
}

#[cfg(feature = "save-state")]
#[test]
fn portable_state_preserves_latches_for_the_next_planar_copy() {
    use doslet_state::State;
    let mut vga = mode_x();
    register(&mut vga, 0x3C4, 2, 15);
    vga.write_memory(7, 73);
    vga.read_memory(7);
    register(&mut vga, 0x3CE, 5, 0x41);
    let mut restored = Vga::load(vga.save()).unwrap();
    vga.write_memory(8, 0);
    restored.write_memory(8, 0);
    assert_eq!(vga.vram_view(), restored.vram_view());
    assert_eq!(&restored.vram_view()[32..36], &[73; 4]);
}
