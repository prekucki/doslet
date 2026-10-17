#[cfg(not(target_arch = "wasm32"))]
use doslet_runtime::Session;
use iced::Size;
use iced::widget::image::Handle;
use iced_runtime::image::{self, Allocation, Error};

mod filter;
mod upload;
pub use filter::DisplayFilter;
pub use upload::PreparedFrame;

// Logical pixels keep the UI usable independently of the monitor's DPI.
#[cfg(not(target_arch = "wasm32"))]
pub const DEFAULT_WINDOW_SIZE: Size = Size::new(960.0, 640.0);
#[cfg(not(target_arch = "wasm32"))]
pub const MIN_WINDOW_SIZE: Size = Size::new(400.0, 160.0);

#[derive(Clone)]
pub struct DisplayFrame {
    pub image: Option<Handle>,
    // Snapshots may skip the empty frame at reset. Preserve a stream identity
    // so an upload from the previous session cannot be presented afterward.
    generation: u64,
    #[cfg(not(target_arch = "wasm32"))]
    revision: Option<(usize, usize, u64)>,
}

impl Default for DisplayFrame {
    fn default() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT_GENERATION: AtomicU64 = AtomicU64::new(0);
        Self {
            image: None,
            generation: NEXT_GENERATION.fetch_add(1, Ordering::Relaxed),
            #[cfg(not(target_arch = "wasm32"))]
            revision: None,
        }
    }
}
impl DisplayFrame {
    pub fn dimensions(&self) -> Option<Size> {
        match self.image.as_ref()? {
            Handle::Rgba { width, height, .. } => Some(Size::new(*width as f32, *height as f32)),
            _ => None,
        }
    }
}

/// Fit an image into the available space without changing its selected aspect.
pub fn fit_size(available: Size, aspect_ratio: f32) -> Size {
    if available.width <= available.height * aspect_ratio {
        Size::new(available.width, available.width / aspect_ratio)
    } else {
        Size::new(available.height * aspect_ratio, available.height)
    }
}

/// Use whole physical pixels, even when the host uses fractional DPI scaling.
/// Small viewports retain 1x and scroll instead of discarding guest pixels.
pub fn pixel_perfect_size(available: Size, pixels: Size, scale_factor: f32) -> Size {
    let zoom = (available.width * scale_factor / pixels.width)
        .min(available.height * scale_factor / pixels.height)
        .floor()
        .max(1.0);
    Size::new(
        pixels.width * zoom / scale_factor,
        pixels.height * zoom / scale_factor,
    )
}

pub struct DisplaySizing {
    pub pixel_perfect: bool,
    pub filter: DisplayFilter,
    viewport: Size,
    scale_factor: f32,
    filtered: filter::FilterCache,
    uploads: upload::UploadQueue,
}

impl Default for DisplaySizing {
    fn default() -> Self {
        Self {
            pixel_perfect: false,
            filter: DisplayFilter::default(),
            viewport: Size::ZERO,
            scale_factor: 1.0,
            filtered: filter::FilterCache::default(),
            uploads: upload::UploadQueue::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum SizingMessage {
    PixelPerfect,
    Fit,
    Filter(DisplayFilter),
    Viewport(Size),
    ScaleFactor(f32),
    Uploaded(u64, Result<Allocation, Error>),
}

impl DisplaySizing {
    pub fn image_size(&self, available: Size, pixels: Size) -> Size {
        if self.pixel_perfect {
            pixel_perfect_size(available, pixels, self.scale_factor)
        } else {
            fit_size(available, 4.0 / 3.0)
        }
    }

    pub fn image(&self) -> Option<&PreparedFrame> {
        self.uploads.frame()
    }

    pub fn error(&self) -> Option<&str> {
        self.uploads.error()
    }

    pub fn refresh(&mut self, frame: &DisplayFrame) -> iced::Task<SizingMessage> {
        #[cfg(target_arch = "wasm32")]
        {
            self.scale_factor =
                web_sys::window().map_or(1.0, |window| window.device_pixel_ratio()) as f32;
        }
        let target = frame.dimensions().map(|pixels| {
            let logical = self.image_size(self.viewport, pixels);
            Size::new(
                logical.width * self.scale_factor,
                logical.height * self.scale_factor,
            )
        });
        self.filtered
            .refresh(frame.image.as_ref(), self.filter, target);
        self.uploads
            .offer(frame.dimensions().and_then(|source_size| {
                self.filtered
                    .image()
                    .or(frame.image.as_ref())
                    .map(|image| PreparedFrame {
                        image: image.clone(),
                        generation: frame.generation,
                        source_size,
                        filter: self.filter,
                    })
            }));
        self.start_upload()
    }

    fn start_upload(&mut self) -> iced::Task<SizingMessage> {
        match self.uploads.start() {
            Some((id, handle)) => {
                image::allocate(handle).map(move |result| SizingMessage::Uploaded(id, result))
            }
            None => iced::Task::none(),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn subscription() -> iced::Subscription<SizingMessage> {
        iced::window::events().filter_map(|(_, event)| match event {
            iced::window::Event::Rescaled(scale) => Some(SizingMessage::ScaleFactor(scale)),
            _ => None,
        })
    }

    pub fn update(
        &mut self,
        message: SizingMessage,
        frame: &DisplayFrame,
    ) -> iced::Task<SizingMessage> {
        let mut task = iced::Task::none();
        match message {
            SizingMessage::PixelPerfect => {
                self.uploads.retry();
                if frame.dimensions().is_some() {
                    self.pixel_perfect = true;
                    task = query_scale_factor();
                }
            }
            SizingMessage::Fit => {
                self.uploads.retry();
                self.pixel_perfect = false;
            }
            SizingMessage::Filter(filter) => {
                self.uploads.retry();
                self.filter = filter;
            }
            SizingMessage::Viewport(size) => {
                self.viewport = size;
                task = query_scale_factor();
            }
            SizingMessage::ScaleFactor(scale) => {
                if scale.is_finite() && scale > 0.0 {
                    self.scale_factor = scale;
                }
            }
            SizingMessage::Uploaded(id, result) => {
                let previous_error = self.uploads.error().map(str::to_owned);
                self.uploads
                    .finish(id, result.map_err(|error| error.to_string()));
                if self.uploads.error() != previous_error.as_deref()
                    && let Some(error) = self.uploads.error()
                {
                    #[cfg(not(target_arch = "wasm32"))]
                    eprintln!("{error}");
                    #[cfg(target_arch = "wasm32")]
                    log::error!("{error}");
                }
                return self.start_upload();
            }
        }
        iced::Task::batch([task, self.refresh(frame)])
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn query_scale_factor() -> iced::Task<SizingMessage> {
    use iced::window;
    window::latest().and_then(|id| window::scale_factor(id).map(SizingMessage::ScaleFactor))
}

#[cfg(target_arch = "wasm32")]
fn query_scale_factor() -> iced::Task<SizingMessage> {
    iced::Task::done(SizingMessage::ScaleFactor(
        web_sys::window().map_or(1.0, |window| window.device_pixel_ratio()) as f32,
    ))
}

#[cfg(not(target_arch = "wasm32"))]
impl DisplayFrame {
    pub fn refresh(&mut self, session: &Session) {
        let frame = session.presentation_frame();
        let revision = (frame.width(), frame.height(), frame.revision());
        if self.revision == Some(revision) {
            return;
        }
        // iced's image handle owns its pixels. This allocation/conversion belongs to
        // presentation, never to CPU execution or borrowed device scanout.
        let mut pixels = vec![0; frame.width() * frame.height() * 4];
        frame
            .write_rgba(&mut pixels)
            .expect("matching framebuffer size");
        self.image = Some(Handle::from_rgba(
            frame.width() as u32,
            frame.height() as u32,
            pixels,
        ));
        self.revision = Some(revision);
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32) -> DisplayFrame {
        DisplayFrame {
            image: Some(Handle::from_rgba(
                width,
                height,
                vec![0; width as usize * height as usize * 4],
            )),
            ..Default::default()
        }
    }

    #[test]
    fn integer_zoom_adapts_to_viewport_dpi_and_video_mode() {
        let text = frame(720, 400);
        let mut sizing = DisplaySizing::default();
        let available = Size::new(960.0, 640.0);
        let _ = sizing.update(SizingMessage::PixelPerfect, &text);
        assert_eq!(
            sizing.image_size(available, text.dimensions().unwrap()),
            Size::new(720.0, 400.0)
        );
        let _ = sizing.update(SizingMessage::ScaleFactor(1.5), &text);
        let size = sizing.image_size(available, text.dimensions().unwrap());
        assert_eq!(size.width * 1.5, 1440.0);
        assert_eq!(size.height * 1.5, 800.0);

        let graphics = frame(640, 480);
        let size = sizing.image_size(available, graphics.dimensions().unwrap());
        assert_eq!(size.width * 1.5, 1280.0);
        assert_eq!(size.height * 1.5, 960.0);
        let _ = sizing.update(SizingMessage::Fit, &graphics);
        // A late DPI response must not reselect integer scaling or resize a window.
        let _ = sizing.update(SizingMessage::ScaleFactor(2.0), &graphics);
        assert!(!sizing.pixel_perfect);
        assert_eq!(
            sizing.image_size(available, graphics.dimensions().unwrap()),
            fit_size(available, 4.0 / 3.0)
        );
        let fitted = fit_size(Size::new(720.0, 400.0), 4.0 / 3.0);
        assert!((fitted.width - 533.3333).abs() < 0.001);
        assert_eq!(fitted.height, 400.0);
    }

    #[test]
    fn mode_13h_keeps_square_pixels_and_small_windows_keep_one_times() {
        let pixels = Size::new(320.0, 200.0);
        assert_eq!(
            pixel_perfect_size(Size::new(1000.0, 700.0), pixels, 1.0),
            Size::new(960.0, 600.0)
        );
        assert_eq!(
            pixel_perfect_size(Size::new(640.0, 480.0), pixels, 1.0),
            Size::new(640.0, 400.0)
        );
        assert_eq!(
            pixel_perfect_size(Size::new(200.0, 100.0), pixels, 1.0),
            pixels
        );
    }

    #[test]
    fn fractional_dpi_uses_the_largest_whole_physical_zoom() {
        for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
            for pixels in [
                Size::new(320.0, 200.0),
                Size::new(640.0, 480.0),
                Size::new(720.0, 400.0),
            ] {
                let available = Size::new(1001.0, 751.0);
                let size = pixel_perfect_size(available, pixels, scale);
                let zoom = size.width * scale / pixels.width;
                assert!((zoom - zoom.round()).abs() < 0.0001);
                assert!((size.height * scale / pixels.height - zoom).abs() < 0.0001);
                assert!(size.width <= available.width && size.height <= available.height);
                assert!(
                    pixels.width * (zoom + 1.0) > available.width * scale
                        || pixels.height * (zoom + 1.0) > available.height * scale
                );
            }
        }
    }
}
