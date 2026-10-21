use super::*;
use doslet_bus::PortBus;

/// Small, allocation-free description of the current scanout geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoModeLabel {
    mode: doslet_vga::VideoMode,
    dimensions: Option<(usize, usize)>,
    cga_mode: Option<u8>,
}
impl VideoModeLabel {
    pub(super) fn new(
        mode: doslet_vga::VideoMode,
        dimensions: Option<(usize, usize)>,
        cga_mode: Option<u8>,
    ) -> Self {
        Self {
            mode,
            dimensions,
            cga_mode,
        }
    }
}
impl fmt::Display for VideoModeLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some((width, height)) = self.dimensions {
            let adapter = if self.mode == doslet_vga::VideoMode::Vbe101 {
                "SVGA"
            } else {
                "VGA"
            };
            write!(f, "{width}×{height}×8 {adapter}")
        } else if self.cga_mode == Some(6) {
            f.write_str("640×200×1 CGA")
        } else if self.cga_mode.is_some() {
            f.write_str("320×200×2 CGA")
        } else {
            f.write_str("80×25 VGA text")
        }
    }
}

impl Session {
    pub(super) fn palette_service(&mut self, ax: u16) -> Result<(), RuntimeError> {
        let r = self.registers();
        let (bx, cx, dx, es) = (r.get(Gpr16::Bx), r.get(Gpr16::Cx), r.get(Gpr16::Dx), r.es);
        match ax as u8 {
            operation @ (0x00 | 0x01) if operation == 1 || (bx as u8) < 16 => {
                let status = if self.machine.bus.vga.read_port(0x3CC).unwrap_or(1) & 1 != 0 {
                    0x3DA
                } else {
                    0x3BA
                };
                self.machine.bus.read_port_u8(status)?; // Reset attribute-controller flip-flop.
                self.machine
                    .bus
                    .write_port_u8(0x3C0, if operation == 1 { 0x11 } else { bx as u8 })?;
                self.machine
                    .bus
                    .write_port_u8(0x3C0, (bx >> 8) as u8 & 0x3F)?;
                self.machine.bus.write_port_u8(0x3C0, 0x20)?;
            }
            0x10 => {
                if bx > 255 {
                    return Err(RuntimeError::UnsupportedService { vector: 0x10, ax });
                }
                self.machine.bus.vga.set_dac_index(bx as u8);
                for value in [(dx >> 8) as u8, (cx >> 8) as u8, cx as u8] {
                    self.machine.bus.vga.write_dac(value);
                }
            }
            0x12 | 0x17 if bx < 256 && cx <= 256 - bx => {
                if ax as u8 == 0x12 {
                    self.machine.bus.vga.set_dac_index(bx as u8);
                } else {
                    self.machine.bus.vga.set_dac_read_index(bx as u8);
                }
                for offset in 0..cx * 3 {
                    let address = linear_address(es, dx.wrapping_add(offset));
                    if ax as u8 == 0x12 {
                        let value = self.machine.bus.read_u8(address)?;
                        self.machine.bus.vga.write_dac(value);
                    } else {
                        let value = self.machine.bus.vga.read_dac();
                        self.machine.bus.write_u8(address, value)?;
                    }
                }
            }
            0x15 if bx < 256 => {
                self.machine.bus.vga.set_dac_read_index(bx as u8);
                let red = self.machine.bus.vga.read_dac();
                let green = self.machine.bus.vga.read_dac();
                let blue = self.machine.bus.vga.read_dac();
                self.machine.cpu.registers.set_byte(6, red);
                self.machine
                    .cpu
                    .registers
                    .set(Gpr16::Cx, (u16::from(green) << 8) | u16::from(blue));
            }
            _ => return Err(RuntimeError::UnsupportedService { vector: 0x10, ax }),
        }
        Ok(())
    }
    pub(super) fn text_service(&mut self, ax: u16) -> Result<(), RuntimeError> {
        let r = &self.machine.cpu.registers;
        let (bx, cx, dx) = (r.get(Gpr16::Bx), r.get(Gpr16::Cx), r.get(Gpr16::Dx));
        let page = (bx >> 8) as u8;
        // AH=01h takes only CH/CL; BH is not a page parameter. Programs
        // commonly leave unrelated values in BX when hiding the cursor.
        if ax >> 8 == 1 {
            self.machine.bus.write_u16(0x460, cx)?;
            let port = self.machine.bus.vga.input_status_port() - 6;
            self.machine.bus.vga.write_port(port, 0x0A);
            self.machine.bus.vga.write_port(port + 1, (cx >> 8) as u8);
            self.machine.bus.vga.write_port(port, 0x0B);
            self.machine.bus.vga.write_port(port + 1, cx as u8);
            return Ok(());
        }
        if self.video_mode() == doslet_vga::VideoMode::Mode13
            && matches!(ax >> 8, 9 | 10)
            && page == 0
        {
            let cursor = self.machine.bus.read_u16(0x450)?;
            let start = u32::from(cursor >> 8) * 40 + u32::from(cursor & 255);
            for cell in start..(start + u32::from(cx)).min(40 * 25) {
                for row in 0..8 {
                    let bits = doslet_vga::font::FONT_8[usize::from(ax as u8) * 8 + row];
                    for col in 0..8 {
                        let color = if bits & (0x80 >> col) != 0 {
                            bx as u8
                        } else {
                            0
                        };
                        self.machine.bus.write_u8(
                            0xA0000 + (cell / 40 * 8 + row as u32) * 320 + cell % 40 * 8 + col,
                            color,
                        )?;
                    }
                }
            }
            return Ok(());
        }
        let bda_mode = self.machine.bus.read_u8(0x449)? & 0x7F;
        if self.video_mode() == doslet_vga::VideoMode::Text3 && matches!(bda_mode, 4..=6) {
            let cols = if bda_mode == 6 { 80 } else { 40 };
            match ax >> 8 {
                6 | 7 => {
                    let (top, left) = ((cx >> 8).min(24), (cx & 255).min(cols - 1));
                    let (bottom, right) = ((dx >> 8).min(24), (dx & 255).min(cols - 1));
                    if top <= bottom && left <= right {
                        for char_row in top..=bottom {
                            for sub_row in 0..8u16 {
                                let y = char_row * 8 + sub_row;
                                let row_base =
                                    0xB8000 + u32::from(y & 1) * 0x2000 + u32::from(y / 2) * 80;
                                for col in left..=right {
                                    if bda_mode == 6 {
                                        self.machine.bus.write_u8(row_base + u32::from(col), 0)?;
                                    } else {
                                        self.machine
                                            .bus
                                            .write_u16(row_base + u32::from(col) * 2, 0)?;
                                    }
                                }
                            }
                        }
                    }
                    return Ok(());
                }
                9 | 10 => {
                    let cursor = self.machine.bus.read_u16(0x450)?;
                    let cols_u32 = u32::from(cols);
                    let start = u32::from(cursor >> 8) * cols_u32 + u32::from(cursor & 255);
                    let color = u16::from(bx as u8 & if bda_mode == 6 { 1 } else { 3 });
                    let xor = bx as u8 & 0x80 != 0;
                    for cell in start..(start + u32::from(cx)).min(cols_u32 * 25) {
                        self.cga_draw_char(bda_mode, cell, ax as u8, color, xor)?;
                    }
                    return Ok(());
                }
                14 => {
                    self.teletype_cga(ax as u8, bx as u8)?;
                    return Ok(());
                }
                _ => {}
            }
        }
        if self.video_mode() != doslet_vga::VideoMode::Text3 || page >= 8 {
            return Err(RuntimeError::UnsupportedService { vector: 0x10, ax });
        }
        match ax >> 8 {
            5 if (ax as u8) < 8 => {
                self.machine.bus.write_u8(0x462, ax as u8)?;
                self.machine
                    .bus
                    .write_u16(0x44E, u16::from(ax as u8) * 0x1000)?;
            }
            8..=10 => {
                let cursor = self.machine.bus.read_u16(0x450 + u32::from(page) * 2)?;
                let cell = u32::from(cursor >> 8) * 80 + u32::from(cursor & 255);
                let base = 0xB8000 + u32::from(page) * 0x1000;
                if ax >> 8 == 8 {
                    let value = self.machine.bus.read_u16(base + (cell * 2) % 0x1000)?;
                    self.machine.cpu.registers.set(Gpr16::Ax, value);
                } else {
                    for index in 0..u32::from(cx) {
                        let address = base + ((cell + index) * 2) % 0x1000;
                        self.machine.bus.write_u8(address, ax as u8)?;
                        if ax >> 8 == 9 {
                            self.machine.bus.write_u8(address + 1, bx as u8)?;
                        }
                    }
                }
            }
            6 | 7 => {
                let page = self.machine.bus.read_u8(0x462)?;
                let base = 0xB8000 + u32::from(page) * 0x1000;
                let (top, left) = ((cx >> 8).min(24), (cx & 255).min(79));
                let (bottom, right) = ((dx >> 8).min(24), (dx & 255).min(79));
                let lines = u16::from(ax as u8);
                if top <= bottom && left <= right {
                    for n in 0..=bottom - top {
                        let row = if ax >> 8 == 6 { top + n } else { bottom - n };
                        let source = if ax >> 8 == 6 {
                            row.checked_add(lines).filter(|r| *r <= bottom)
                        } else {
                            row.checked_sub(lines).filter(|r| *r >= top)
                        };
                        for col in left..=right {
                            let value = if lines != 0
                                && let Some(source) = source
                            {
                                self.machine
                                    .bus
                                    .read_u16(base + u32::from(source * 80 + col) * 2)?
                            } else {
                                (bx & 0xFF00) | 0x20
                            };
                            self.machine
                                .bus
                                .write_u16(base + u32::from(row * 80 + col) * 2, value)?;
                        }
                    }
                }
            }
            14 => self.teletype(ax as u8)?,
            _ => return Err(RuntimeError::UnsupportedService { vector: 0x10, ax }),
        }
        Ok(())
    }

    pub(super) fn teletype(&mut self, byte: u8) -> Result<(), RuntimeError> {
        let page = self.machine.bus.read_u8(0x462)?;
        let cursor_address = 0x450 + u32::from(page) * 2;
        let cursor = self.machine.bus.read_u16(cursor_address)?;
        let (mut row, mut col) = (cursor >> 8, cursor & 255);
        match byte {
            7 => {}
            8 => col = col.saturating_sub(1),
            13 => col = 0,
            10 => row += 1,
            _ => {
                let address = 0xB8000 + u32::from(page) * 0x1000 + u32::from(row * 80 + col) * 2;
                self.machine.bus.write_u8(address, byte)?;
                col += 1;
                if col >= 80 {
                    col = 0;
                    row += 1;
                }
            }
        }
        if row >= 25 {
            let base = 0xB8000 + u32::from(page) * 0x1000;
            for cell in 0..24 * 80 {
                let value = self.machine.bus.read_u16(base + (cell + 80) * 2)?;
                self.machine.bus.write_u16(base + cell * 2, value)?;
            }
            for cell in 24 * 80..25 * 80 {
                self.machine.bus.write_u16(base + cell * 2, 0x0720)?;
            }
            row = 24;
        }
        self.machine
            .bus
            .write_u16(cursor_address, (row << 8) | col)?;
        Ok(())
    }

    fn cga_draw_char(
        &mut self,
        mode: u8,
        cell: u32,
        ch: u8,
        color: u16,
        xor: bool,
    ) -> Result<(), RuntimeError> {
        let cols = if mode == 6 { 80 } else { 40 };
        let cell_row = cell / cols;
        let cell_col = cell % cols;
        for row in 0..8u32 {
            let bits = doslet_vga::font::FONT_8[usize::from(ch) * 8 + row as usize];
            let y = cell_row * 8 + row;
            if mode == 6 {
                let addr = 0xB8000 + (y & 1) * 0x2000 + (y / 2) * 80 + cell_col;
                let byte = if color != 0 { bits } else { 0 };
                if xor {
                    let old = self.machine.bus.read_u8(addr)?;
                    self.machine.bus.write_u8(addr, old ^ byte)?;
                } else {
                    self.machine.bus.write_u8(addr, byte)?;
                }
            } else {
                let mut packed = 0u16;
                for col in 0..8u32 {
                    let pixel = if bits & (0x80 >> col) != 0 { color } else { 0 };
                    packed = (packed << 2) | pixel;
                }
                let addr = 0xB8000 + (y & 1) * 0x2000 + (y / 2) * 80 + cell_col * 2;
                let bytes = packed.to_be_bytes();
                if xor {
                    let old0 = self.machine.bus.read_u8(addr)?;
                    let old1 = self.machine.bus.read_u8(addr + 1)?;
                    self.machine.bus.write_u8(addr, old0 ^ bytes[0])?;
                    self.machine.bus.write_u8(addr + 1, old1 ^ bytes[1])?;
                } else {
                    self.machine.bus.write_u8(addr, bytes[0])?;
                    self.machine.bus.write_u8(addr + 1, bytes[1])?;
                }
            }
        }
        Ok(())
    }

    pub(super) fn teletype_cga(&mut self, byte: u8, attr: u8) -> Result<(), RuntimeError> {
        let mode = self.machine.bus.read_u8(0x449)? & 0x7F;
        let cols = if mode == 6 { 80 } else { 40 };
        let cursor = self.machine.bus.read_u16(0x450)?;
        let (mut row, mut col) = (cursor >> 8, cursor & 255);
        match byte {
            7 => {}
            8 => col = col.saturating_sub(1),
            13 => col = 0,
            10 => row += 1,
            _ => {
                let cell = u32::from(row) * u32::from(cols) + u32::from(col);
                let mask = if mode == 6 { 1 } else { 3 };
                let color = u16::from(if attr & mask != 0 { attr & mask } else { mask });
                self.cga_draw_char(mode, cell, byte, color, attr & 0x80 != 0)?;
                col += 1;
                if col >= cols {
                    col = 0;
                    row += 1;
                }
            }
        }
        if row >= 25 {
            for y in 0..192u32 {
                let src_y = y + 8;
                let dst = 0xB8000 + (y & 1) * 0x2000 + (y / 2) * 80;
                let src = 0xB8000 + (src_y & 1) * 0x2000 + (src_y / 2) * 80;
                for offset in (0..80u32).step_by(2) {
                    let word = self.machine.bus.read_u16(src + offset)?;
                    self.machine.bus.write_u16(dst + offset, word)?;
                }
            }
            for y in 192..200u32 {
                let dst = 0xB8000 + (y & 1) * 0x2000 + (y / 2) * 80;
                for offset in (0..80u32).step_by(2) {
                    self.machine.bus.write_u16(dst + offset, 0)?;
                }
            }
            row = 24;
        }
        self.machine.bus.write_u16(0x450, (row << 8) | col)?;
        Ok(())
    }
}

#[cfg(test)]
mod mode_label_tests {
    use super::*;
    use alloc::string::ToString;
    fn register(s: &mut Session, port: u16, index: u8, value: u8) {
        s.machine.bus.write_port_u8(port, index).unwrap();
        s.machine.bus.write_port_u8(port + 1, value).unwrap();
    }
    #[test]
    fn labels_follow_crtc_width_height_and_scanline_repeats_without_changing_state() {
        let mut s = Session::new(&[0xeb, 0xfe]).unwrap();
        assert_eq!(s.video_mode_label().to_string(), "80×25 VGA text");
        s.machine.bus.vga.set_mode_13h();
        assert_eq!(s.video_mode_label().to_string(), "320×200×8 VGA");
        register(&mut s, 0x3c4, 4, 6); // Unchain planes.
        assert_eq!(s.video_mode_label().to_string(), "320×200×8 VGA");
        register(&mut s, 0x3d4, 0x11, 0); // Unlock CRTC overflow.
        register(&mut s, 0x3d4, 7, 0x3e);
        register(&mut s, 0x3d4, 0x12, 0xdf);
        assert_eq!(s.video_mode_label().to_string(), "320×240×8 VGA");
        register(&mut s, 0x3d4, 1, 89);
        register(&mut s, 0x3d4, 9, 0);
        let before = (
            s.steps(),
            s.emulated_nanos(),
            s.machine.bus.vga.frame().unwrap().revision,
        );
        assert_eq!(s.video_mode_label().to_string(), "360×480×8 VGA");
        assert_eq!(
            (
                s.steps(),
                s.emulated_nanos(),
                s.machine.bus.vga.frame().unwrap().revision
            ),
            before
        );
        s.machine.bus.vga.set_vbe_101h(false);
        assert_eq!(s.video_mode_label().to_string(), "640×480×8 SVGA");
    }
}
