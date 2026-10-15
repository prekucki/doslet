#[cfg(not(target_arch = "wasm32"))]
use doslet_runtime::Session;
use iced::Size;
use iced::widget::image::Handle;

#[derive(Default)]
pub struct DisplayFrame {
    pub image: Option<Handle>,
    #[cfg(not(target_arch = "wasm32"))]
    revision: Option<(usize, usize, u64)>,
}
#[cfg(not(target_arch = "wasm32"))]
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

#[cfg(not(target_arch = "wasm32"))]
pub struct DisplaySizing {
    pub actual_size: bool,
    viewport: Size,
    scale_factor: f32,
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for DisplaySizing {
    fn default() -> Self {
        Self {
            actual_size: false,
            viewport: Size::ZERO,
            scale_factor: 1.0,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub enum SizingMessage {
    ActualSize,
    Fit,
    Viewport(Size),
    ScaleFactor(f32),
    WindowMetrics(iced::window::Id, Size, f32),
}

#[cfg(not(target_arch = "wasm32"))]
impl DisplaySizing {
    pub fn image_size(&self, frame: &DisplayFrame) -> Option<Size> {
        if !self.actual_size {
            return None;
        }
        let pixels = frame.dimensions()?;
        Some(Size::new(
            pixels.width / self.scale_factor,
            pixels.height / self.scale_factor,
        ))
    }

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
        use iced::{Task, window};
        match message {
            SizingMessage::ActualSize => {
                if frame.dimensions().is_some() {
                    self.actual_size = true;
                    return window::latest().and_then(|id| {
                        window::size(id).then(move |size| {
                            window::scale_factor(id)
                                .map(move |scale| SizingMessage::WindowMetrics(id, size, scale))
                        })
                    });
                }
            }
            SizingMessage::Fit => self.actual_size = false,
            SizingMessage::Viewport(size) => self.viewport = size,
            SizingMessage::ScaleFactor(scale) => self.scale_factor = scale,
            SizingMessage::WindowMetrics(id, size, scale) => {
                self.scale_factor = scale;
                // A pending window query must not undo a later click on Fit.
                if self.actual_size
                    && self.viewport.width > 0.0
                    && self.viewport.height > 0.0
                    && let Some(pixels) = frame.dimensions()
                {
                    let target = actual_window_size(size, self.viewport, pixels, scale);
                    return window::maximize(id, false).chain(window::resize(id, target));
                }
            }
        }
        Task::none()
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn actual_window_size(window: Size, viewport: Size, pixels: Size, scale: f32) -> Size {
    // Window/widget dimensions are logical pixels; guest pixels must occupy
    // physical display pixels. Measure the surrounding UI instead of encoding
    // its padding, toolbar, sidebar and variable footer heights here.
    Size::new(
        window.width - viewport.width + pixels.width / scale,
        window.height - viewport.height + pixels.height / scale,
    )
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
    fn actual_size_preserves_raster_pixels_across_dpi_and_video_mode_changes() {
        let text = frame(720, 400);
        let mut sizing = DisplaySizing::default();
        assert_eq!(sizing.image_size(&text), None);
        let _ = sizing.update(SizingMessage::ActualSize, &text);
        assert_eq!(sizing.image_size(&text), Some(Size::new(720.0, 400.0)));
        let _ = sizing.update(SizingMessage::ScaleFactor(1.5), &text);
        let size = sizing.image_size(&text).unwrap();
        assert_eq!(size.width * 1.5, 720.0);
        assert_eq!(size.height * 1.5, 400.0);

        let graphics = frame(640, 480);
        let size = sizing.image_size(&graphics).unwrap();
        assert_eq!(size.width * 1.5, 640.0);
        assert_eq!(size.height * 1.5, 480.0);
        let _ = sizing.update(SizingMessage::Fit, &graphics);
        assert_eq!(sizing.image_size(&graphics), None);
        let fitted = fit_size(Size::new(720.0, 400.0), 4.0 / 3.0);
        assert!((fitted.width - 533.3333).abs() < 0.001);
        assert_eq!(fitted.height, 400.0);
    }

    #[test]
    fn window_resize_keeps_measured_ui_space_and_accounts_for_hidpi() {
        let window = Size::new(1120.0, 760.0);
        let viewport = Size::new(736.0, 450.0);
        let pixels = Size::new(720.0, 400.0);
        assert_eq!(
            actual_window_size(window, viewport, pixels, 1.0),
            Size::new(1104.0, 710.0)
        );
        assert_eq!(
            actual_window_size(window, viewport, pixels, 2.0),
            Size::new(744.0, 510.0)
        );
    }
}
