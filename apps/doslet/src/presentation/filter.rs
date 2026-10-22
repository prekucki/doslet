//! Presentation-only filters. Source handles share iced's owned RGBA snapshot;
//! no guest RAM or VRAM is copied here. Scratch buffers stay on the UI owner.
use iced::Size;
use iced::widget::image::{FilterMethod, Handle};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DisplayFilter {
    #[default]
    Nearest,
    SharpBilinear,
    Hq2x,
    Xbrz,
    Ravu,
}

impl DisplayFilter {
    pub const ALL: &[Self] = &[
        Self::Nearest,
        Self::SharpBilinear,
        Self::Hq2x,
        Self::Xbrz,
        Self::Ravu,
    ];

    /// Drawn by a GPU shader from the guest-sized frame; the browser checks
    /// that the page did not fall back to Canvas2D.
    #[cfg(target_arch = "wasm32")]
    pub fn gpu(self) -> bool {
        matches!(self, Self::Xbrz | Self::Ravu)
    }

    pub fn sampling(self) -> FilterMethod {
        match self {
            Self::Nearest => FilterMethod::Nearest,
            Self::Xbrz | Self::Ravu => FilterMethod::Nearest,
            Self::SharpBilinear | Self::Hq2x => FilterMethod::Linear,
        }
    }
}

impl From<doslet_config::VideoFilter> for DisplayFilter {
    fn from(value: doslet_config::VideoFilter) -> Self {
        match value {
            doslet_config::VideoFilter::Nearest => Self::Nearest,
            doslet_config::VideoFilter::SharpBilinear => Self::SharpBilinear,
            doslet_config::VideoFilter::Hq2x => Self::Hq2x,
        }
    }
}

impl std::fmt::Display for DisplayFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Nearest => "Nearest",
            Self::SharpBilinear => "Sharp Bilinear",
            Self::Hq2x => "HQ2x",
            Self::Xbrz => "xBRZ (GPU)",
            Self::Ravu => "RAVU-Zoom (GPU)",
        })
    }
}

#[derive(Default)]
pub(super) struct FilterCache {
    source: Option<Handle>,
    filter: DisplayFilter,
    scale: [u32; 2],
    image: Option<Handle>,
    argb: Vec<u32>,
    enlarged: Vec<u32>,
}

impl FilterCache {
    pub fn image(&self) -> Option<&Handle> {
        self.image.as_ref()
    }

    pub fn refresh(
        &mut self,
        source: Option<&Handle>,
        filter: DisplayFilter,
        target: Option<Size>,
    ) {
        let Some(
            source @ Handle::Rgba {
                width,
                height,
                pixels,
                ..
            },
        ) = source
        else {
            self.source = None;
            self.image = None;
            return;
        };
        if !matches!(filter, DisplayFilter::SharpBilinear | DisplayFilter::Hq2x) {
            self.source = None;
            self.image = None;
            return;
        }
        let scale = match filter {
            DisplayFilter::SharpBilinear => {
                let target = target.unwrap_or(Size::new(*width as f32, *height as f32));
                // Nearest prescaling followed by linear sampling is sharp bilinear:
                // pixel interiors stay flat, only their boundaries blend. Limit
                // the intermediate to 4x per axis to bound presentation memory.
                [
                    (target.width / *width as f32).floor().clamp(1.0, 4.0) as u32,
                    (target.height / *height as f32).floor().clamp(1.0, 4.0) as u32,
                ]
            }
            DisplayFilter::Hq2x => [2, 2],
            _ => unreachable!(),
        };
        if self
            .source
            .as_ref()
            .is_some_and(|old| old.id() == source.id())
            && self.filter == filter
            && self.scale == scale
        {
            return;
        }
        self.image = Some(match filter {
            DisplayFilter::SharpBilinear if scale == [1, 1] => source.clone(),
            DisplayFilter::SharpBilinear => Handle::from_rgba(
                width * scale[0],
                height * scale[1],
                nearest_prescale(pixels, *width, *height, scale),
            ),
            DisplayFilter::Hq2x => {
                self.argb.clear();
                self.argb.extend(
                    pixels
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|p| u32::from_be_bytes([p[3], p[0], p[1], p[2]])),
                );
                self.enlarged.resize(self.argb.len() * 4, 0);
                hqx::hq2x(&self.argb, &mut self.enlarged, *width, *height);
                let mut rgba = Vec::with_capacity(self.enlarged.len() * 4);
                for &pixel in &self.enlarged {
                    let [a, r, g, b] = pixel.to_be_bytes();
                    rgba.extend_from_slice(&[r, g, b, a]);
                }
                Handle::from_rgba(width * 2, height * 2, rgba)
            }
            _ => unreachable!(),
        });
        // Cloning a Handle shares its immutable bytes, not the pixel buffer.
        self.source = Some(source.clone());
        self.filter = filter;
        self.scale = scale;
    }
}

fn nearest_prescale(pixels: &[u8], width: u32, height: u32, scale: [u32; 2]) -> Vec<u8> {
    let row_bytes = width as usize * scale[0] as usize * 4;
    let mut output = vec![0; row_bytes * height as usize * scale[1] as usize];
    for (source, rows) in pixels
        .chunks_exact(width as usize * 4)
        .zip(output.chunks_exact_mut(row_bytes * scale[1] as usize))
    {
        let (first, rest) = rows.split_at_mut(row_bytes);
        for (pixel, copies) in source
            .as_chunks::<4>()
            .0
            .iter()
            .zip(first.chunks_exact_mut(4 * scale[0] as usize))
        {
            for copy in copies.as_chunks_mut::<4>().0 {
                copy.copy_from_slice(pixel);
            }
        }
        for row in rest.chunks_exact_mut(row_bytes) {
            row.copy_from_slice(first);
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(image: &Handle) -> (u32, u32, &[u8]) {
        let Handle::Rgba {
            width,
            height,
            pixels,
            ..
        } = image
        else {
            panic!("RGBA image expected")
        };
        (*width, *height, pixels)
    }

    #[test]
    fn sharp_bilinear_prescales_axes_independently_without_changing_colors() {
        let red = [255, 0, 0, 255];
        let blue = [0, 0, 255, 255];
        let source = Handle::from_rgba(2, 1, [red, blue].concat());
        let mut cache = FilterCache::default();
        cache.refresh(
            Some(&source),
            DisplayFilter::SharpBilinear,
            Some(Size::new(5.0, 3.5)),
        );
        let (width, height, pixels) = rgba(cache.image().unwrap());
        assert_eq!((width, height), (4, 3));
        for row in pixels.as_chunks::<16>().0 {
            assert_eq!(row.as_slice(), [red, red, blue, blue].concat());
        }
    }

    #[test]
    fn switching_to_xbrz_discards_cpu_enlargement() {
        let source = Handle::from_rgba(2, 2, vec![255; 16]);
        let mut cache = FilterCache::default();
        cache.refresh(Some(&source), DisplayFilter::Hq2x, None);
        assert_eq!(rgba(cache.image().unwrap()).0, 4);
        cache.refresh(
            Some(&source),
            DisplayFilter::Xbrz,
            Some(Size::new(100.0, 100.0)),
        );
        // The viewport must receive the original guest-sized image for its shader.
        assert!(cache.image().is_none());
        cache.refresh(Some(&source), DisplayFilter::Hq2x, None);
        assert_eq!(rgba(cache.image().unwrap()).0, 4);
    }

    #[test]
    fn hq2x_preserves_rgba_channels_and_smooths_a_diagonal() {
        let color = [23, 145, 219, 255];
        let source = Handle::from_rgba(2, 2, color.repeat(4));
        let mut cache = FilterCache::default();
        cache.refresh(Some(&source), DisplayFilter::Hq2x, None);
        let (width, height, pixels) = rgba(cache.image().unwrap());
        assert_eq!((width, height), (4, 4));
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| *pixel == color)
        );

        let black = [0, 0, 0, 255];
        let white = [255, 255, 255, 255];
        let diagonal = [
            white, white, black, white, black, black, black, black, black,
        ]
        .concat();
        let source = Handle::from_rgba(3, 3, diagonal.clone());
        cache.refresh(Some(&source), DisplayFilter::Hq2x, None);
        let (_, _, pixels) = rgba(cache.image().unwrap());
        assert_ne!(pixels, nearest_prescale(&diagonal, 3, 3, [2, 2]));
        assert!(
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[3] == 255)
        );
    }

    #[test]
    fn cache_refreshes_on_frame_filter_and_zoom_changes_but_reuses_paused_frames() {
        let source = Handle::from_rgba(2, 2, vec![255; 16]);
        let mut cache = FilterCache::default();
        let target = Some(Size::new(5.0, 5.0));
        cache.refresh(Some(&source), DisplayFilter::SharpBilinear, target);
        let first = cache.image().unwrap().id();
        cache.refresh(Some(&source), DisplayFilter::SharpBilinear, target);
        assert_eq!(cache.image().unwrap().id(), first);
        cache.refresh(
            Some(&source),
            DisplayFilter::SharpBilinear,
            Some(Size::new(7.0, 7.0)),
        );
        assert_ne!(cache.image().unwrap().id(), first);
        assert_eq!(rgba(cache.image().unwrap()).0, 6);
        cache.refresh(Some(&source), DisplayFilter::Hq2x, target);
        assert_eq!(rgba(cache.image().unwrap()).0, 4);
        let next = Handle::from_rgba(1, 1, vec![0, 0, 0, 255]);
        cache.refresh(Some(&next), DisplayFilter::Hq2x, target);
        assert_eq!(rgba(cache.image().unwrap()).0, 2);
        cache.refresh(Some(&next), DisplayFilter::Nearest, target);
        assert!(cache.image().is_none());
        cache.refresh(Some(&source), DisplayFilter::Hq2x, target);
        cache.refresh(None, DisplayFilter::Hq2x, None);
        assert!(cache.image().is_none());
    }
}
