//! Borrowed indexed scanout for the linear mode-13h bootstrap.
//! Mode 13h and banked VBE 101h with selected clocked raster status.
//! Planar memory, programmable raster timing and text rendering are pending.
#![no_std]
extern crate alloc;

mod palette;
mod registers;
pub mod scanout;
use alloc::{boxed::Box, vec};
pub use scanout::IndexedFrame;

pub const APERTURE_START: u32 = 0xA0000;
pub const APERTURE_LEN: usize = 65536;
pub const VRAM_LEN: usize = 512 * 1024;
pub const WIDTH: usize = 320;
pub const HEIGHT: usize = 200;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoMode {
    Text3,
    Mode13,
    Vbe101,
}

pub struct Vga {
    memory: Box<[u8]>,
    palette: [[u8; 4]; 256],
    dac_cursor: usize,
    registers: registers::Registers,
    mode: VideoMode,
    bank: u16,
    raster_ticks: u32,
    pending_nanos: u64,
    pixel_fraction: u64,
    revision: u64,
}
impl Default for Vga {
    fn default() -> Self {
        Self {
            memory: vec![0; VRAM_LEN].into_boxed_slice(),
            palette: palette::default_palette(),
            dac_cursor: 0,
            registers: registers::Registers::default(),
            mode: VideoMode::Text3,
            bank: 0,
            raster_ticks: 0,
            pending_nanos: 0,
            pixel_fraction: 0,
            revision: 0,
        }
    }
}
impl Vga {
    /// Borrow physical VRAM for diagnostics, including banks outside scanout.
    /// CPU memory transactions still use the mapped aperture operations.
    pub fn vram_view(&self) -> &[u8] {
        &self.memory
    }
    pub fn set_mode_13h(&mut self) {
        self.flush_raster_time();
        self.memory.fill(0);
        self.palette = palette::default_palette();
        self.dac_cursor = 0;
        self.registers.mode_13h();
        self.mode = VideoMode::Mode13;
        self.bank = 0;
        self.raster_ticks = 0;
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn read_memory(&self, offset: usize) -> Option<u8> {
        if offset >= APERTURE_LEN {
            return None;
        }
        self.memory
            .get(usize::from(self.bank) * APERTURE_LEN + offset)
            .copied()
    }
    pub fn write_memory(&mut self, offset: usize, value: u8) -> bool {
        if offset >= APERTURE_LEN {
            return false;
        }
        let Some(target) = self
            .memory
            .get_mut(usize::from(self.bank) * APERTURE_LEN + offset)
        else {
            return false;
        };
        if *target != value {
            *target = value;
            self.revision = self.revision.wrapping_add(1);
        }
        true
    }
    pub fn mode(&self) -> VideoMode {
        self.mode
    }
    pub fn set_vbe_101h(&mut self, preserve: bool) {
        self.flush_raster_time();
        if !preserve {
            self.memory.fill(0);
        }
        self.bank = 0;
        self.mode = VideoMode::Vbe101;
        self.raster_ticks = 0;
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn bank(&self) -> u16 {
        self.bank
    }
    pub fn set_bank(&mut self, bank: u16) -> bool {
        if self.mode != VideoMode::Vbe101 || usize::from(bank) >= VRAM_LEN / APERTURE_LEN {
            return false;
        }
        self.bank = bank;
        true
    }
    pub fn set_dac_index(&mut self, index: u8) {
        self.dac_cursor = usize::from(index) * 3;
    }
    pub fn read_port(&self, port: u16) -> Option<u8> {
        if port == self.input_status_port() {
            let (total, visible, lines, sync_start, sync_end) = self.raster_geometry();
            let phase = self.raster_phase();
            let x = phase % total;
            let y = phase / total;
            return Some(
                u8::from(x >= visible || y >= lines)
                    | if (sync_start..sync_end).contains(&y) {
                        8
                    } else {
                        0
                    },
            );
        }
        self.registers.read(port)
    }
    /// Active input-status decode without resolving raster time.
    #[inline]
    pub fn input_status_port(&self) -> u16 {
        if self.registers.misc & 1 != 0 {
            0x3DA
        } else {
            0x3BA
        }
    }
    pub fn pixel_clock_hz(&self) -> u32 {
        if self.mode == VideoMode::Text3 {
            28_322_000
        } else {
            25_175_000
        }
    }
    /// Selected input-status port and exact guest-nanosecond deadline of the
    /// next vertical-retrace bit transition. This is a query without read side
    /// effects; a future attribute-controller flip-flop model must revisit wait
    /// batching equivalence before adding status-read side effects.
    pub fn retrace_deadline(&self, port: u16) -> Option<u64> {
        if port != self.input_status_port() {
            return None;
        }
        let (line, _, _, start, end) = self.raster_geometry();
        let phase = self.raster_phase();
        let start = line * start;
        let end = line * end;
        let pixels = if phase < start {
            start - phase
        } else if phase < end {
            end - phase
        } else {
            self.raster_total() - phase + start
        };
        let fraction = (self.pending_nanos * u64::from(self.pixel_clock_hz())
            + self.pixel_fraction)
            % 1_000_000_000;
        Some(
            (u64::from(pixels) * 1_000_000_000 - fraction)
                .div_ceil(u64::from(self.pixel_clock_hz())),
        )
    }
    fn raster_geometry(&self) -> (u32, u32, u32, u32, u32) {
        match self.mode {
            VideoMode::Text3 => (900, 720, 400, 412, 414),
            VideoMode::Mode13 => (800, 640, 400, 412, 414),
            VideoMode::Vbe101 => (800, 640, 480, 490, 492),
        }
    }
    pub fn advance_pixel_ticks(&mut self, ticks: u32) {
        self.flush_raster_time();
        self.advance_raster_ticks(ticks);
    }
    /// Accumulate guest time, preserving fractional pixels across mode changes.
    /// Each u32 input keeps clock products bounded; reads evaluate exact phase.
    pub fn advance_nanos(&mut self, nanos: u32) {
        self.pending_nanos += u64::from(nanos);
        if self.pending_nanos >= 1_000_000 {
            self.flush_raster_time();
        }
    }
    fn raster_total(&self) -> u32 {
        let (line, _, _, _, _) = self.raster_geometry();
        line * if self.mode == VideoMode::Vbe101 {
            525
        } else {
            449
        }
    }
    fn raster_phase(&self) -> u32 {
        let ticks = (self.pending_nanos * u64::from(self.pixel_clock_hz()) + self.pixel_fraction)
            / 1_000_000_000;
        ((u64::from(self.raster_ticks) + ticks) % u64::from(self.raster_total())) as u32
    }
    fn flush_raster_time(&mut self) {
        if self.pending_nanos == 0 {
            return;
        }
        let numerator = self.pending_nanos * u64::from(self.pixel_clock_hz()) + self.pixel_fraction;
        self.pixel_fraction = numerator % 1_000_000_000;
        self.pending_nanos = 0;
        self.advance_raster_ticks((numerator / 1_000_000_000) as u32);
    }
    fn advance_raster_ticks(&mut self, ticks: u32) {
        let (line, _, _, _, _) = self.raster_geometry();
        let rows = if self.mode == VideoMode::Vbe101 {
            525
        } else {
            449
        };
        self.raster_ticks =
            ((u64::from(self.raster_ticks) + u64::from(ticks)) % u64::from(line * rows)) as u32;
    }
    pub fn write_port(&mut self, port: u16, value: u8) -> bool {
        self.registers.write(port, value)
    }
    pub fn write_dac(&mut self, value: u8) {
        let value = u16::from(value & 63);
        self.palette[self.dac_cursor / 3][self.dac_cursor % 3] = ((value * 255 + 31) / 63) as u8;
        self.dac_cursor = (self.dac_cursor + 1) % (256 * 3);
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn frame(&self) -> Option<IndexedFrame<'_>> {
        let (width, height) = match self.mode {
            VideoMode::Text3 => return None,
            VideoMode::Mode13 => (WIDTH, HEIGHT),
            VideoMode::Vbe101 => (640, 480),
        };
        Some(IndexedFrame {
            width,
            height,
            pixels: &self.memory[..width * height],
            palette: &self.palette,
            revision: self.revision,
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn retrace_deadline_is_exact_with_fractional_pixels_and_mode_changes() {
        for mode in 0..3 {
            for initial in [0, 121, 122, 1000, 999999, 16_700_001] {
                let mut vga = super::Vga::default();
                if mode == 1 {
                    vga.set_mode_13h();
                }
                if mode == 2 {
                    vga.set_vbe_101h(false);
                }
                vga.advance_nanos(initial);
                for _ in 0..4 {
                    let before = vga.read_port(0x3DA).unwrap() & 8;
                    let deadline = vga.retrace_deadline(0x3DA).unwrap();
                    assert!(deadline > 0);
                    vga.advance_nanos((deadline - 1) as u32);
                    assert_eq!(vga.read_port(0x3DA).unwrap() & 8, before);
                    assert_eq!(vga.retrace_deadline(0x3DA), Some(1));
                    vga.advance_nanos(1);
                    assert_ne!(vga.read_port(0x3DA).unwrap() & 8, before);
                }
                assert_eq!(vga.retrace_deadline(0x3BA), None);
                vga.write_port(0x3C2, 0);
                assert_eq!(vga.retrace_deadline(0x3DA), None);
                assert!(vga.retrace_deadline(0x3BA).is_some());
            }
        }
    }
    #[test]
    fn lazy_raster_time_matches_eager_pixels_across_modes_reads_and_large_inputs() {
        let mut lazy = super::Vga::default();
        let mut reference = super::Vga::default();
        let mut fraction = 0u64;
        for i in 0..30000 {
            if i % 4000 == 1000 {
                lazy.set_mode_13h();
                reference.set_mode_13h();
            }
            if i % 4000 == 3000 {
                lazy.set_vbe_101h(false);
                reference.set_vbe_101h(false);
            }
            let nanos = [121, 122, 1000, 999999, 1000000, u32::MAX][i % 6];
            fraction += u64::from(nanos) * u64::from(reference.pixel_clock_hz());
            reference.advance_pixel_ticks((fraction / 1_000_000_000) as u32);
            fraction %= 1_000_000_000;
            lazy.advance_nanos(nanos);
            assert_eq!(lazy.read_port(0x3DA), reference.read_port(0x3DA));
            assert_eq!(lazy.raster_phase(), reference.raster_ticks);
        }
    }
    #[test]
    fn small_and_large_raster_advances_match_modulo_reference() {
        let mut vga = super::Vga::default();
        for mode in 0..3 {
            if mode == 1 {
                vga.set_mode_13h();
            }
            if mode == 2 {
                vga.set_vbe_101h(false);
            }
            let total = if mode == 0 {
                900 * 449
            } else if mode == 1 {
                800 * 449
            } else {
                800 * 525
            };
            let mut phase = 0u64;
            for i in 0..10000 {
                let ticks = [3, 4, 1000, total - 1, total, u32::MAX][i % 6];
                phase = (phase + u64::from(ticks)) % u64::from(total);
                vga.advance_pixel_ticks(ticks);
                assert_eq!(u64::from(vga.raster_ticks), phase);
            }
        }
    }
    use super::*;
    #[test]
    fn input_status_retrace_and_blanking_follow_clocked_raster_phase() {
        let mut vga = Vga::default();
        vga.set_vbe_101h(false);
        assert_eq!(vga.read_port(0x3DA), Some(0));
        vga.advance_pixel_ticks(640);
        assert_eq!(vga.read_port(0x3DA), Some(1));
        vga.advance_pixel_ticks(800 * 490 - 640);
        assert_eq!(vga.read_port(0x3DA), Some(9));
        vga.advance_pixel_ticks(800 * 2);
        assert_eq!(vga.read_port(0x3DA), Some(1));
        vga.advance_pixel_ticks(800 * 33);
        assert_eq!(vga.read_port(0x3DA), Some(0));
        assert_eq!(vga.read_port(0x3BA), None);
    }
    #[test]
    fn banked_vbe_memory_scanout_and_preservation_share_one_vram_owner() {
        let mut vga = Vga::default();
        assert!(!vga.set_bank(1));
        vga.set_vbe_101h(false);
        assert!(vga.set_bank(1));
        vga.write_memory(0, 42);
        assert_eq!(vga.read_memory(0), Some(42));
        assert!(vga.set_bank(0));
        assert_eq!(vga.read_memory(0), Some(0));
        let frame = vga.frame().unwrap();
        assert_eq!((frame.width, frame.height), (640, 480));
        assert_eq!(frame.pixels[APERTURE_LEN], 42);
        assert_eq!(frame.pixels.as_ptr(), vga.memory.as_ptr());
        assert!(!vga.set_bank(8));
        vga.set_vbe_101h(true);
        assert_eq!(vga.frame().unwrap().pixels[APERTURE_LEN], 42);
        vga.set_vbe_101h(false);
        assert_eq!(vga.frame().unwrap().pixels[APERTURE_LEN], 0);
        vga.set_mode_13h();
        assert_eq!(vga.frame().unwrap().pixels.len(), WIDTH * HEIGHT);
        assert_eq!(vga.bank(), 0);
    }
    #[test]
    fn frame_borrows_video_memory_and_conversion_uses_caller_storage() {
        let mut vga = Vga::default();
        vga.set_mode_13h();
        vga.write_memory(0, 1);
        let frame = vga.frame().unwrap();
        assert_eq!(frame.pixels.as_ptr(), vga.memory.as_ptr());
        let mut rgba = vec![0; WIDTH * HEIGHT * 4];
        frame.write_rgba(&mut rgba).unwrap();
        assert_eq!(&rgba[..4], &frame.palette[1]);
        assert!(frame.write_rgba(&mut rgba[..4]).is_err());
    }
    #[test]
    fn dac_wraps_and_expands_six_bit_components() {
        let mut vga = Vga::default();
        vga.set_mode_13h();
        vga.set_dac_index(255);
        vga.write_dac(63);
        vga.write_dac(0);
        vga.write_dac(31);
        vga.write_dac(63);
        let frame = vga.frame().unwrap();
        assert_eq!(frame.palette[255], [255, 0, 125, 255]);
        assert_eq!(frame.palette[0][0], 255);
    }
}
