#[derive(Clone, Copy)]
pub enum PixelLayout {
    Linear,
    /// Four adjacent bytes hold the planes at one 16-bit VGA address.
    Vga256 {
        start: usize,
        pitch: usize,
    },
}

/// Read-only view. It cannot outlive or overlap mutable access to the VGA device.
#[derive(Clone, Copy)]
pub struct IndexedFrame<'a> {
    pub width: usize,
    pub height: usize,
    /// Borrowed backing storage; use `indices` for visible pixels with any layout.
    pub pixels: &'a [u8],
    pub palette: &'a [[u8; 4]; 256],
    pub revision: u64,
    pub pixel_mask: u8,
    pub layout: PixelLayout,
}
impl IndexedFrame<'_> {
    pub fn indices(&self) -> impl Iterator<Item = u8> + '_ {
        (0..self.width * self.height).map(|index| {
            let offset = match self.layout {
                PixelLayout::Linear => index,
                PixelLayout::Vga256 { start, pitch } => {
                    let (y, x) = (index / self.width, index % self.width);
                    ((start + y * pitch + x / 4) & 0xFFFF) * 4 + x % 4
                }
            };
            self.pixels[offset] & self.pixel_mask
        })
    }
    pub fn write_rgba(&self, output: &mut [u8]) -> Result<(), FrameSizeError> {
        if output.len() != self.width * self.height * 4 {
            return Err(FrameSizeError);
        }
        if matches!(self.layout, PixelLayout::Linear) {
            for (pixel, rgba) in self.pixels.iter().zip(output.as_chunks_mut::<4>().0) {
                *rgba = self.palette[usize::from(*pixel & self.pixel_mask)];
            }
        } else {
            for (pixel, rgba) in self.indices().zip(output.as_chunks_mut::<4>().0) {
                *rgba = self.palette[usize::from(pixel)];
            }
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameSizeError;

/// Borrowed text cells; glyph expansion writes directly into host storage.
pub struct TextFrame<'a> {
    pub cells: &'a [u8],
    pub palette: &'a [[u8; 4]; 256],
    pub revision: u64,
    pub cursor: u16,
    pub cursor_shape: u16,
    pub blink: bool,
}
pub enum PresentationFrame<'a> {
    Indexed(IndexedFrame<'a>),
    Text(TextFrame<'a>),
    Cursor {
        frame: IndexedFrame<'a>,
        x: i32,
        y: i32,
        masks: &'a [u16; 32],
    },
}
impl PresentationFrame<'_> {
    pub fn width(&self) -> usize {
        match self {
            Self::Indexed(f) | Self::Cursor { frame: f, .. } => f.width,
            Self::Text(_) => 720,
        }
    }
    pub fn height(&self) -> usize {
        match self {
            Self::Indexed(f) | Self::Cursor { frame: f, .. } => f.height,
            Self::Text(_) => 400,
        }
    }
    pub fn revision(&self) -> u64 {
        match self {
            Self::Indexed(f) | Self::Cursor { frame: f, .. } => f.revision,
            Self::Text(f) => f.revision,
        }
    }
    pub fn write_rgba(&self, output: &mut [u8]) -> Result<(), FrameSizeError> {
        if output.len() != self.width() * self.height() * 4 {
            return Err(FrameSizeError);
        }
        match self {
            Self::Indexed(f) => f.write_rgba(output),
            Self::Cursor {
                frame: f,
                x,
                y,
                masks,
            } => {
                f.write_rgba(output)?;
                for row in 0..16 {
                    for column in 0..16 {
                        let (px, py) = (*x + column, *y + row);
                        if px < 0 || py < 0 || px >= f.width as i32 || py >= f.height as i32 {
                            continue;
                        }
                        let keep = masks[row as usize] & (0x8000 >> column) != 0;
                        let invert = masks[row as usize + 16] & (0x8000 >> column) != 0;
                        let index = (py as usize * f.width + px as usize) * 4;
                        for channel in &mut output[index..index + 3] {
                            if !keep {
                                *channel = 0;
                            }
                            if invert {
                                *channel = !*channel;
                            }
                        }
                    }
                }
                Ok(())
            }
            Self::Text(f) => {
                for (index, &[ch, attribute]) in
                    f.cells.as_chunks::<2>().0.iter().take(2000).enumerate()
                {
                    let glyph =
                        &crate::font::FONT_16[usize::from(ch) * 16..usize::from(ch) * 16 + 16];
                    let fg = usize::from(attribute & 15);
                    let bg = usize::from((attribute >> 4) & 7);
                    for (y, bits) in glyph.iter().enumerate() {
                        let cursor = index == usize::from(f.cursor)
                            && f.cursor_shape & 0x2000 == 0
                            && f.blink
                            && (usize::from((f.cursor_shape >> 8) & 31)
                                ..=usize::from(f.cursor_shape & 31))
                                .contains(&y);
                        for x in 0..9 {
                            let ink = if x < 8 {
                                bits & (0x80 >> x) != 0
                            } else {
                                (0xC0..=0xDF).contains(&ch) && bits & 1 != 0
                            };
                            let color = if cursor || ink && (attribute & 0x80 == 0 || f.blink) {
                                fg
                            } else {
                                bg
                            };
                            let pixel = ((index / 80 * 16 + y) * 720 + index % 80 * 9 + x) * 4;
                            output[pixel..pixel + 4].copy_from_slice(&f.palette[color]);
                        }
                    }
                }
                Ok(())
            }
        }
    }
}
