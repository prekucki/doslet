//! Borrowed indexed scanout for mode 13h, unchained 256-color VGA and VBE 101h.
//! Text scanout is composed by the runtime. General EGA/16-color scanout and
//! programmable raster timing remain pending.
#![no_std]
extern crate alloc;

pub mod font;
mod palette;
#[cfg(test)]
mod planar_tests;
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

/// Immutable controls and borrowed physical backing for an exclusive foreign
/// execution slice. Reads must reproduce latch/compare behavior and commit the
/// final four latch bytes before any ordinary device operation resumes.
pub struct MemoryReadView<'a> {
    pub bytes: &'a [u8],
    pub bank_offset: u32,
    pub planar: Option<PlanarRead>,
    pub latches: [u8; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanarRead {
    pub chain4: bool,
    pub read_plane: u8,
    /// (compare, care), present only in read mode 1.
    pub compare: Option<(u8, u8)>,
}

/// Small register snapshot for an exclusive foreign memory-write slice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryWriteControls {
    pub plane_mask: u8,
    pub mode: u8,
    pub set_reset: u8,
    pub enable_set_reset: u8,
    pub rotate: u8,
    pub bit_mask: u8,
}

pub struct Vga {
    memory: Box<[u8]>,
    latches: [u8; 4],
    palette: [[u8; 4]; 256],
    dac_cursor: usize,
    dac_read_cursor: usize,
    dac_reading: bool,
    dac_mask: u8,
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
            latches: [0; 4],
            palette: palette::default_palette(),
            dac_cursor: 0,
            dac_read_cursor: 0,
            dac_reading: false,
            dac_mask: 255,
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
    /// No copy or remapping of VRAM. The caller must not retain this borrow
    /// across foreign execution; only an explicitly bounded pointer descriptor
    /// may be exported by the application while this owner remains exclusive.
    pub fn direct_read_view(&self) -> Option<MemoryReadView<'_>> {
        self.direct_read_view_with_memory(&self.memory)
    }

    /// The machine supplies the sole physical backing after setup migration.
    pub fn direct_read_view_with_memory<'a>(&self, memory: &'a [u8]) -> Option<MemoryReadView<'a>> {
        assert_eq!(memory.len(), VRAM_LEN, "physical VGA backing length");
        if !self.memory_available(0) {
            return None;
        }
        Some(MemoryReadView {
            bytes: memory,
            bank_offset: u32::from(self.bank) * APERTURE_LEN as u32,
            planar: (self.mode == VideoMode::Mode13).then(|| {
                let (_, _, _, _, compare) = self.registers.graphics_pipeline();
                PlanarRead {
                    chain4: self.registers.chain4(),
                    read_plane: self.registers.read_plane() as u8,
                    compare: self.registers.read_compare().map(|care| (compare, care)),
                }
            }),
            latches: self.latches,
        })
    }

    /// Commit staged direct-read effects before runtime/device code resumes.
    /// Linear modes leave the latch registers unchanged, as `read_memory` does.
    pub fn complete_direct_reads(&mut self, latches: [u8; 4]) {
        if self.mode == VideoMode::Mode13 {
            self.latches = latches;
        }
    }

    pub fn direct_write_controls(&self) -> MemoryWriteControls {
        let (set_reset, enable_set_reset, rotate, bit_mask, _) = self.registers.graphics_pipeline();
        MemoryWriteControls {
            plane_mask: self.registers.plane_mask(),
            mode: self.registers.write_mode(),
            set_reset,
            enable_set_reset,
            rotate,
            bit_mask,
        }
    }

    /// Generated writes count physical bytes whose value actually changed,
    /// exactly as the ordinary write pipeline increments the scanout revision.
    pub fn complete_direct_writes(&mut self, changes: u64) {
        self.revision = self.revision.wrapping_add(changes);
    }

    /// Setup-only migration into the machine's new sole memory owner. Once
    /// transferred, memory operations must use the explicit backing APIs.
    pub fn transfer_memory_into(&mut self, memory: &mut [u8]) {
        assert_eq!(memory.len(), VRAM_LEN, "physical VGA backing length");
        assert_eq!(
            self.memory.len(),
            VRAM_LEN,
            "VGA backing already transferred"
        );
        memory.copy_from_slice(&self.memory);
        self.memory = Box::default();
    }

    pub fn memory_configuration(&self) -> (u8, u8, u8, u16, u8, u8) {
        self.registers.memory_config()
    }
    /// Borrow physical VRAM for diagnostics, including banks outside scanout.
    /// CPU memory transactions still use the mapped aperture operations.
    pub fn vram_view(&self) -> &[u8] {
        assert_eq!(
            self.memory.len(),
            VRAM_LEN,
            "VGA backing transferred to machine"
        );
        &self.memory
    }
    pub fn palette(&self) -> &[[u8; 4]; 256] {
        &self.palette
    }
    /// Restore the selected 80x25 text configuration. Text scanout is pending;
    /// the machine/runtime owns the current B800h text-memory backing.
    pub fn set_mode_3h(&mut self) {
        self.flush_raster_time();
        self.palette = palette::default_palette();
        self.dac_cursor = 0;
        self.dac_read_cursor = 0;
        self.dac_reading = false;
        self.dac_mask = 255;
        self.registers = registers::Registers::default();
        self.mode = VideoMode::Text3;
        self.bank = 0;
        self.raster_ticks = 0;
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn set_mode_13h(&mut self) {
        assert_eq!(
            self.memory.len(),
            VRAM_LEN,
            "VGA backing transferred to machine"
        );
        self.memory.fill(0);
        self.configure_mode_13h();
    }
    pub fn set_mode_13h_with_memory(&mut self, memory: &mut [u8]) {
        assert_eq!(memory.len(), VRAM_LEN, "physical VGA backing length");
        memory.fill(0);
        self.configure_mode_13h();
    }
    fn configure_mode_13h(&mut self) {
        self.flush_raster_time();
        self.palette = palette::default_palette();
        self.dac_cursor = 0;
        self.dac_read_cursor = 0;
        self.dac_reading = false;
        self.dac_mask = 255;
        self.registers.mode_13h();
        self.mode = VideoMode::Mode13;
        self.bank = 0;
        self.raster_ticks = 0;
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn read_memory(&mut self, offset: usize) -> Option<u8> {
        assert_eq!(
            self.memory.len(),
            VRAM_LEN,
            "VGA backing transferred to machine"
        );
        Self::read_memory_impl(
            &self.memory,
            offset,
            &self.registers,
            self.mode,
            self.bank,
            &mut self.latches,
        )
    }
    pub fn read_memory_with_memory(&mut self, memory: &[u8], offset: usize) -> Option<u8> {
        assert_eq!(memory.len(), VRAM_LEN, "physical VGA backing length");
        Self::read_memory_impl(
            memory,
            offset,
            &self.registers,
            self.mode,
            self.bank,
            &mut self.latches,
        )
    }
    fn read_memory_impl(
        memory: &[u8],
        offset: usize,
        registers: &registers::Registers,
        mode: VideoMode,
        bank: u16,
        latches: &mut [u8; 4],
    ) -> Option<u8> {
        if offset >= APERTURE_LEN {
            return None;
        }
        if mode == VideoMode::Mode13 {
            let group = if registers.chain4() {
                offset & !3
            } else {
                offset * 4
            };
            latches.copy_from_slice(&memory[group..group + 4]);
            if let Some(care) = registers.read_compare() {
                let (_, _, _, _, compare) = registers.graphics_pipeline();
                let mut result = 0xFF;
                for (plane, latch) in latches.iter().enumerate() {
                    if care & (1 << plane) != 0 {
                        result &= latch ^ if compare & (1 << plane) != 0 { 0 } else { 0xFF };
                    }
                }
                return Some(result);
            }
            let plane = if registers.chain4() {
                offset & 3
            } else {
                registers.read_plane()
            };
            return Some(latches[plane]);
        }
        memory
            .get(usize::from(bank) * APERTURE_LEN + offset)
            .copied()
    }
    /// Whether `read_memory` and `write_memory` succeed for this aperture
    /// offset under the current bank. Only mode and bank changes, which are
    /// port or BIOS operations, change the answer.
    pub fn memory_available(&self, offset: usize) -> bool {
        offset < APERTURE_LEN && usize::from(self.bank) * APERTURE_LEN + offset < VRAM_LEN
    }
    pub fn write_memory(&mut self, offset: usize, value: u8) -> bool {
        assert_eq!(
            self.memory.len(),
            VRAM_LEN,
            "VGA backing transferred to machine"
        );
        Self::write_memory_impl(
            &mut self.memory,
            offset,
            value,
            &self.registers,
            self.mode,
            self.bank,
            &self.latches,
            &mut self.revision,
        )
    }
    pub fn write_memory_with_memory(
        &mut self,
        memory: &mut [u8],
        offset: usize,
        value: u8,
    ) -> bool {
        assert_eq!(memory.len(), VRAM_LEN, "physical VGA backing length");
        Self::write_memory_impl(
            memory,
            offset,
            value,
            &self.registers,
            self.mode,
            self.bank,
            &self.latches,
            &mut self.revision,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn write_memory_impl(
        memory: &mut [u8],
        offset: usize,
        value: u8,
        registers: &registers::Registers,
        mode: VideoMode,
        bank: u16,
        latches: &[u8; 4],
        revision: &mut u64,
    ) -> bool {
        if offset >= APERTURE_LEN {
            return false;
        }
        if mode == VideoMode::Mode13 {
            let chained = registers.chain4();
            let group = if chained { offset & !3 } else { offset * 4 };
            let mask = registers.plane_mask() & if chained { 1 << (offset & 3) } else { 15 };
            let (set, enable, rotate, bit_mask, _) = registers.graphics_pipeline();
            let rotated = value.rotate_right(u32::from(rotate & 7));
            for (plane, &latch) in latches.iter().enumerate() {
                if mask & (1 << plane) == 0 {
                    continue;
                }
                let (source, bits) = match registers.write_mode() {
                    1 => (latch, 0xFF),
                    2 => (if value & (1 << plane) != 0 { 0xFF } else { 0 }, bit_mask),
                    3 => (
                        if set & (1 << plane) != 0 { 0xFF } else { 0 },
                        bit_mask & rotated,
                    ),
                    _ => (
                        if enable & (1 << plane) != 0 {
                            if set & (1 << plane) != 0 { 0xFF } else { 0 }
                        } else {
                            rotated
                        },
                        bit_mask,
                    ),
                };
                let source = if registers.write_mode() == 1 {
                    source
                } else {
                    match (rotate >> 3) & 3 {
                        1 => source & latch,
                        2 => source | latch,
                        3 => source ^ latch,
                        _ => source,
                    }
                };
                let result = (source & bits) | (latch & !bits);
                if memory[group + plane] != result {
                    memory[group + plane] = result;
                    *revision = revision.wrapping_add(1);
                }
            }
            return true;
        }
        let Some(target) = memory.get_mut(usize::from(bank) * APERTURE_LEN + offset) else {
            return false;
        };
        if *target != value {
            *target = value;
            *revision = revision.wrapping_add(1);
        }
        true
    }
    pub fn mode(&self) -> VideoMode {
        self.mode
    }
    pub fn is_unchained_256(&self) -> bool {
        self.mode == VideoMode::Mode13 && !self.registers.chain4()
    }
    pub fn set_vbe_101h(&mut self, preserve: bool) {
        assert_eq!(
            self.memory.len(),
            VRAM_LEN,
            "VGA backing transferred to machine"
        );
        if !preserve {
            self.memory.fill(0);
        }
        self.configure_vbe_101h();
    }
    pub fn set_vbe_101h_with_memory(&mut self, memory: &mut [u8], preserve: bool) {
        assert_eq!(memory.len(), VRAM_LEN, "physical VGA backing length");
        if !preserve {
            memory.fill(0);
        }
        self.configure_vbe_101h();
    }
    fn configure_vbe_101h(&mut self) {
        self.flush_raster_time();
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
        self.dac_reading = false;
    }
    pub fn set_dac_read_index(&mut self, index: u8) {
        self.dac_read_cursor = usize::from(index) * 3;
        self.dac_reading = true;
    }
    pub fn read_dac(&mut self) -> u8 {
        let value = self.palette[self.dac_read_cursor / 3][self.dac_read_cursor % 3];
        self.dac_read_cursor = (self.dac_read_cursor + 1) % 768;
        ((u16::from(value) * 63 + 127) / 255) as u8
    }
    pub fn read_port(&self, port: u16) -> Option<u8> {
        match port {
            0x3C6 => return Some(self.dac_mask),
            0x3C7 => return Some(if self.dac_reading { 0 } else { 3 }),
            0x3C8 => return Some((self.dac_cursor / 3) as u8),
            _ => {}
        }
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
    pub fn read_status(&mut self, port: u16) -> Option<u8> {
        let value = self.read_port(port)?;
        if port == self.input_status_port() {
            self.registers.reset_attribute_latch();
        }
        Some(value)
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
    /// effects. Actual bus status reads reset the attribute index/data latch;
    /// repeated reads in recognized wait loops have the same idempotent reset.
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
        if port == 0x3C6 {
            self.dac_mask = value;
            self.revision = self.revision.wrapping_add(1);
            return true;
        }
        let previous = self.registers.layout_256();
        if self.registers.write(port, value) {
            if self.registers.layout_256() != previous {
                self.revision = self.revision.wrapping_add(1);
            }
            true
        } else {
            false
        }
    }
    pub fn write_dac(&mut self, value: u8) {
        let value = u16::from(value & 63);
        self.palette[self.dac_cursor / 3][self.dac_cursor % 3] = ((value * 255 + 31) / 63) as u8;
        self.dac_cursor = (self.dac_cursor + 1) % (256 * 3);
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn frame(&self) -> Option<IndexedFrame<'_>> {
        self.frame_with_memory(&self.memory)
    }
    pub fn frame_with_memory<'a>(&'a self, memory: &'a [u8]) -> Option<IndexedFrame<'a>> {
        assert_eq!(memory.len(), VRAM_LEN, "physical VGA backing length");
        let mut layout = scanout::PixelLayout::Linear;
        let (width, height) = match self.mode {
            VideoMode::Text3 => return None,
            VideoMode::Mode13 => {
                let (width, height, start, pitch) = self.registers.layout_256();
                if start != 0 || pitch * 4 != width || width * height > 256 * 1024 {
                    layout = scanout::PixelLayout::Vga256 { start, pitch };
                }
                (width, height)
            }
            VideoMode::Vbe101 => (640, 480),
        };
        Some(IndexedFrame {
            width,
            height,
            pixels: &memory[..if matches!(layout, scanout::PixelLayout::Linear) {
                width * height
            } else {
                256 * 1024
            }],
            palette: &self.palette,
            revision: self.revision,
            pixel_mask: self.dac_mask,
            layout,
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn status_read_resets_attribute_latch_and_dac_round_trips_all_components() {
        let mut vga = super::Vga::default();
        vga.write_port(0x3C0, 0x10);
        vga.read_status(0x3DA);
        vga.write_port(0x3C0, 0x11);
        vga.write_port(0x3C0, 0x34);
        assert_eq!(vga.read_port(0x3C1), Some(0x34));
        for value in 0..64 {
            vga.set_dac_index(255);
            for component in [value, 0, 63] {
                vga.write_dac(component);
            }
            vga.set_dac_read_index(255);
            assert_eq!(vga.read_port(0x3C7), Some(0));
            assert_eq!(vga.read_dac(), value);
            assert_eq!(vga.read_dac(), 0);
            assert_eq!(vga.read_dac(), 63);
        }
        vga.set_mode_13h();
        vga.write_memory(0, 0xF1);
        vga.write_port(0x3C6, 15);
        let mut output = alloc::vec![0;super::WIDTH*super::HEIGHT*4];
        vga.frame().unwrap().write_rgba(&mut output).unwrap();
        assert_eq!(&output[..4], &vga.palette[1]);
    }
    #[test]
    fn text_mode_restores_registers_bank_and_timing_without_replacing_vram() {
        let mut vga = super::Vga::default();
        let owner = vga.vram_view().as_ptr();
        vga.set_vbe_101h(false);
        assert!(vga.set_bank(3));
        vga.write_memory(0, 42);
        vga.advance_nanos(999999);
        vga.write_port(0x3C2, 0);
        let revision = vga.revision;
        vga.set_mode_3h();
        assert_eq!(vga.mode(), super::VideoMode::Text3);
        assert_eq!(vga.bank(), 0);
        assert_eq!(vga.pixel_clock_hz(), 28_322_000);
        assert_eq!(vga.input_status_port(), 0x3DA);
        assert_eq!(vga.read_port(0x3CC), Some(0x67));
        assert_eq!(vga.raster_ticks, 0);
        assert_eq!(vga.pending_nanos, 0);
        assert!(vga.revision > revision);
        assert!(vga.frame().is_none());
        assert_eq!(vga.vram_view().as_ptr(), owner);
        // Text backing is separate for now; the switch does not erase graphics VRAM.
        assert_eq!(vga.vram_view()[3 * super::APERTURE_LEN], 42);
        vga.set_mode_13h();
        assert!(vga.frame().is_some());
    }

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

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
