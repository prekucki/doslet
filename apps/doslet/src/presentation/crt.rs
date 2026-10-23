//! Optional scanlines over immutable presentation snapshots, cached while paused.
use iced::widget::image::Handle;

#[derive(Default)]
pub(super) struct CrtCache {
    source: Option<Handle>,
    image: Option<Handle>,
}

impl CrtCache {
    pub fn image(&self) -> Option<&Handle> {
        self.image.as_ref()
    }

    pub fn refresh(&mut self, source: Option<&Handle>, enabled: bool) {
        let Some(
            source @ Handle::Rgba {
                width,
                height,
                pixels,
                ..
            },
        ) = source.filter(|_| enabled)
        else {
            self.source = None;
            self.image = None;
            return;
        };
        if self
            .source
            .as_ref()
            .is_some_and(|old| old.id() == source.id())
        {
            return;
        }
        let mut output = pixels.to_vec();
        if *width > 0 {
            for (index, pixel) in output.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                let dark = (index / *width as usize) % 2 == 1;
                for channel in &mut pixel[..3] {
                    *channel = if dark {
                        ((*channel as u16 * 207) / 255) as u8
                    } else {
                        channel.saturating_add(3)
                    };
                }
            }
        }
        self.image = Some(Handle::from_rgba(*width, *height, output));
        self.source = Some(source.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scanlines_cache_paused_frames_and_clear_when_disabled_or_reset() {
        let source = Handle::from_rgba(1, 2, [100, 120, 140, 77].repeat(2));
        let mut cache = CrtCache::default();
        cache.refresh(Some(&source), true);
        let first = cache.image().unwrap().id();
        let Handle::Rgba { pixels, .. } = cache.image().unwrap() else {
            panic!("RGBA expected")
        };
        assert_eq!((pixels[3], pixels[7]), (77, 77));
        assert!(pixels[0] > pixels[4]);
        cache.refresh(Some(&source), true);
        assert_eq!(cache.image().unwrap().id(), first);
        cache.refresh(Some(&source), false);
        assert!(cache.image().is_none());
        cache.refresh(Some(&source), true);
        cache.refresh(None, true);
        assert!(cache.image().is_none());
    }
}
