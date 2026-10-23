#[cfg(not(target_arch = "wasm32"))]
use doslet_runtime::Session;
use iced::Size;
use iced::widget::image::Handle;
use iced_runtime::image::{self, Allocation, Error};

mod crt;
mod filter;
mod upload;
pub use filter::DisplayFilter;
#[cfg(not(target_arch = "wasm32"))]
pub use filter::hardware_gpu;
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

/// Shape of the picture inside the display area when not pixel perfect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScreenShape {
    /// 4:3, as on a CRT.
    #[default]
    Standard,
    /// 16:9 with the whole picture stretched evenly.
    Wide,
    /// 16:9 with the centre near 4:3 proportions and stretched edges.
    Stretch,
}

impl ScreenShape {
    pub fn aspect_ratio(self) -> f32 {
        match self {
            Self::Standard => 4.0 / 3.0,
            Self::Wide | Self::Stretch => 16.0 / 9.0,
        }
    }
}

/// How the game window covers the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FullscreenStyle {
    /// A maximized window without decorations. The compositor never switches
    /// the window into its full screen mode, which avoids compositor bugs on
    /// leaving it (COSMIC crashed there); panels may stay visible.
    Borderless,
    /// The compositor's full screen mode. Leaving it first restores the bars,
    /// then stops presenting frames for a second before the window mode
    /// changes.
    Native,
}

impl Default for FullscreenStyle {
    fn default() -> Self {
        // A browser page has no decorations or maximize state to use.
        if cfg!(target_arch = "wasm32") {
            Self::Native
        } else {
            Self::Borderless
        }
    }
}

/// How often the UI presents a new guest picture. Only presentation follows
/// it; guest timing, input and audio do not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameRate {
    /// The guest's vertical refresh, e.g. about 70 Hz in mode 13h.
    Guest,
    /// At most this many pictures per second.
    Limit(u16),
}

impl Default for FrameRate {
    fn default() -> Self {
        Self::Limit(60)
    }
}

impl FrameRate {
    pub const ALL: &[Self] = &[
        Self::Guest,
        Self::Limit(30),
        Self::Limit(50),
        Self::Limit(60),
        Self::Limit(75),
        Self::Limit(100),
        Self::Limit(120),
        Self::Limit(144),
    ];

    /// Time between presented pictures. Without a known guest refresh the
    /// guest setting falls back to 60 Hz.
    pub fn interval(self, guest_millihertz: u32) -> std::time::Duration {
        let millihertz = match self {
            Self::Guest if guest_millihertz != 0 => guest_millihertz,
            Self::Guest => 60_000,
            Self::Limit(fps) => u32::from(fps) * 1000,
        }
        .clamp(10_000, 240_000);
        std::time::Duration::from_nanos(1_000_000_000_000 / u64::from(millihertz))
    }

    /// Compact form shared with the emulation thread and the browser worker;
    /// 0 is the guest refresh.
    pub fn code(self) -> u16 {
        match self {
            Self::Guest => 0,
            Self::Limit(fps) => fps,
        }
    }

    // The browser worker reads the code in JavaScript.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    pub fn from_code(code: u16) -> Self {
        if code == 0 {
            Self::Guest
        } else {
            Self::Limit(code)
        }
    }
}

impl std::fmt::Display for FrameRate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Guest => f.write_str("Guest refresh"),
            Self::Limit(fps) => write!(f, "{fps} fps"),
        }
    }
}

/// Maps a horizontal position on the stretched display (0..1) to the source
/// image (0..1). The display position is `0.5 + 0.75 d + d^3` for source offset
/// `d` from the centre, so the centre keeps 4:3 proportions (slope 0.75 of a
/// 16:9 display) and the edges are stretched up to 2x that. The same curve is
/// inverted in `xbrz.wgsl`; keep both in step.
pub fn stretch_source(fraction: f32) -> f32 {
    let t = fraction.clamp(0.0, 1.0) - 0.5;
    let m = t.abs();
    let mut d = m;
    for _ in 0..8 {
        d -= (d * (0.75 + d * d) - m) / (0.75 + 3.0 * d * d);
    }
    0.5 + d.copysign(t)
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

/// Counts distinct guest pictures presented per second. A paused or idle
/// guest shows 0, even while the UI itself still redraws.
#[derive(Default)]
struct FrameMeter {
    last: Option<iced_runtime::core::image::Id>,
    start: Option<web_time::Instant>,
    frames: u32,
    fps: Option<f32>,
}

impl FrameMeter {
    fn observe(&mut self, image: Option<&Handle>, now: web_time::Instant) {
        let id = image.map(Handle::id);
        if id.is_some() && id != self.last {
            self.frames += 1;
            self.last = id;
        }
        let start = *self.start.get_or_insert(now);
        let elapsed = now.saturating_duration_since(start);
        if elapsed >= std::time::Duration::from_secs(1) {
            self.fps = Some(self.frames as f32 / elapsed.as_secs_f32());
            self.frames = 0;
            self.start = Some(now);
        }
    }

    /// Frames per second, or 0 once nothing was observed for two seconds.
    fn fps(&self, now: web_time::Instant) -> Option<f32> {
        let stale = self.start.is_some_and(|start| {
            now.saturating_duration_since(start) >= std::time::Duration::from_secs(2)
        });
        if stale { Some(0.0) } else { self.fps }
    }
}

pub struct DisplaySizing {
    pub pixel_perfect: bool,
    pub shape: ScreenShape,
    /// Whether the window is in full screen; hides the toolbar and footer.
    pub fullscreen: bool,
    /// Style for the next full screen request.
    pub fullscreen_style: FullscreenStyle,
    /// Style of the current full screen, so leaving undoes what entering did.
    active_style: FullscreenStyle,
    /// Whether a borderless full screen found the window maximized.
    was_maximized: bool,
    /// Between the exit request and the window settling as windowed.
    leaving: bool,
    /// No frames are presented while a native full screen is about to end.
    frozen: bool,
    /// The chosen filter; [`Self::active_filter`] is the one drawn.
    pub filter: DisplayFilter,
    pub frame_rate: FrameRate,
    /// Alternating scanlines applied before the filter.
    pub crt: bool,
    /// Simple filter drawn instead of a costly one without a hardware GPU.
    pub fallback: DisplayFilter,
    /// Whether GPU filters and the 16:9 smart warp run on a hardware GPU.
    pub gpu: bool,
    frames: FrameMeter,
    viewport: Size,
    scale_factor: f32,
    filtered: filter::FilterCache,
    scanlines: crt::CrtCache,
    uploads: upload::UploadQueue,
    #[cfg(target_arch = "wasm32")]
    web_image: Option<PreparedFrame>,
}

impl Default for DisplaySizing {
    fn default() -> Self {
        Self {
            pixel_perfect: false,
            shape: ScreenShape::default(),
            fullscreen: false,
            fullscreen_style: FullscreenStyle::default(),
            active_style: FullscreenStyle::default(),
            was_maximized: false,
            leaving: false,
            frozen: false,
            filter: DisplayFilter::default(),
            frame_rate: FrameRate::default(),
            crt: false,
            fallback: DisplayFilter::default(),
            gpu: true,
            frames: FrameMeter::default(),
            viewport: Size::ZERO,
            scale_factor: 1.0,
            filtered: filter::FilterCache::default(),
            scanlines: crt::CrtCache::default(),
            uploads: upload::UploadQueue::default(),
            #[cfg(target_arch = "wasm32")]
            web_image: None,
        }
    }
}

#[derive(Clone, Debug)]
pub enum SizingMessage {
    PixelPerfect,
    Fit,
    Shape(ScreenShape),
    /// Request full screen or windowed mode.
    Fullscreen(bool),
    #[cfg(not(target_arch = "wasm32"))]
    FullscreenStyle(FullscreenStyle),
    /// Native exit: stop presenting frames once the bars were drawn again.
    FreezeFrames,
    /// Native exit: leave the compositor's full screen mode.
    ApplyFullscreen(bool),
    /// Borderless entry found the window in this maximized state.
    WasMaximized(bool),
    /// The window had time to settle after leaving full screen.
    FullscreenSettled,
    Filter(DisplayFilter),
    Crt(bool),
    FrameRate(FrameRate),
    Viewport(Size),
    ScaleFactor(f32),
    Uploaded(u64, Result<Allocation, Error>),
}

impl DisplaySizing {
    pub fn image_size(&self, available: Size, pixels: Size) -> Size {
        if self.pixel_perfect {
            pixel_perfect_size(available, pixels, self.scale_factor)
        } else {
            fit_size(available, self.shape.aspect_ratio())
        }
    }

    /// The window is still leaving full screen: the host cursor must stay
    /// visible and nothing may switch the view underneath it.
    pub fn leaving_fullscreen(&self) -> bool {
        self.leaving
    }

    /// New guest pictures shown per second, once a second has been measured.
    pub fn fps(&self) -> Option<f32> {
        self.frames.fps(web_time::Instant::now())
    }

    /// Frames must not be presented: a native full screen is about to end.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn frames_frozen(&self) -> bool {
        self.frozen
    }

    /// True when the picture needs the non-linear horizontal warp.
    pub fn stretched(&self) -> bool {
        // Without a GPU the picture is stretched evenly, as 16:9.
        !self.pixel_perfect && self.shape == ScreenShape::Stretch && self.gpu
    }

    /// Size, screen shape and full screen style from the global settings.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn apply_settings(&mut self, global: &crate::global_settings::GlobalSettings) {
        self.pixel_perfect = global.scaling == "pixel-perfect";
        self.shape = match global.shape.as_str() {
            "16:9" => ScreenShape::Wide,
            "16:9-smart" => ScreenShape::Stretch,
            _ => ScreenShape::Standard,
        };
        self.fullscreen_style = match global.fullscreen.as_str() {
            "native" => FullscreenStyle::Native,
            _ => FullscreenStyle::Borderless,
        };
    }

    /// Takes the filter and its fallback from a machine's output settings.
    pub fn set_output(&mut self, output: &doslet_config::OutputRouting) {
        self.filter = output.video_filter.into();
        self.fallback = output.filter(false).into();
    }

    /// The filter drawn: none in pixel perfect, which only enlarges whole
    /// pixels, and a simple fallback for costly filters without a GPU.
    pub fn active_filter(&self) -> DisplayFilter {
        if self.pixel_perfect {
            DisplayFilter::Nearest
        } else if self.filter.needs_gpu() && !self.gpu {
            self.fallback
        } else {
            self.filter
        }
    }

    pub fn image(&self) -> Option<&PreparedFrame> {
        #[cfg(target_arch = "wasm32")]
        return self.web_image.as_ref();
        #[cfg(not(target_arch = "wasm32"))]
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
        self.frames
            .observe(frame.image.as_ref(), web_time::Instant::now());
        let target = frame.dimensions().map(|pixels| {
            let logical = self.image_size(self.viewport, pixels);
            Size::new(
                logical.width * self.scale_factor,
                logical.height * self.scale_factor,
            )
        });
        let filter = self.active_filter();
        self.scanlines.refresh(frame.image.as_ref(), self.crt);
        let source = self.scanlines.image().or(frame.image.as_ref());
        self.filtered.refresh(source, filter, target);
        #[cfg(target_arch = "wasm32")]
        {
            // WGPU on WASM uploads raster textures when they are first drawn.
            // Waiting for an explicit allocation before drawing deadlocks it.
            self.web_image = frame.dimensions().and_then(|source_size| {
                self.filtered.image().or(source).map(|image| PreparedFrame {
                    image: image.clone(),
                    generation: frame.generation,
                    source_size,
                    filter,
                })
            });
            return iced::Task::none();
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.uploads
                .offer(frame.dimensions().and_then(|source_size| {
                    self.filtered.image().or(source).map(|image| PreparedFrame {
                        image: image.clone(),
                        generation: frame.generation,
                        source_size,
                        filter,
                    })
                }));
            self.start_upload()
        }
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
            #[cfg(not(target_arch = "wasm32"))]
            SizingMessage::FullscreenStyle(style) => {
                // Leaving must undo what entering did; switch styles only
                // in a window.
                if !self.fullscreen && !self.leaving {
                    self.fullscreen_style = style;
                }
            }
            SizingMessage::Fullscreen(true) => {
                if !self.fullscreen {
                    self.fullscreen = true;
                    self.leaving = false;
                    self.frozen = false;
                    self.active_style = self.fullscreen_style;
                    task = match self.active_style {
                        FullscreenStyle::Native => set_fullscreen(true),
                        FullscreenStyle::Borderless => enter_borderless(),
                    };
                }
            }
            SizingMessage::Fullscreen(false) => {
                if self.fullscreen {
                    self.fullscreen = false;
                    match self.active_style {
                        FullscreenStyle::Borderless => {
                            task = leave_borderless(self.was_maximized);
                        }
                        FullscreenStyle::Native => {
                            // Restore the toolbar and footer first, then stop
                            // presenting frames, and only then leave the
                            // compositor's full screen mode. Changing the
                            // layout and the window mode together crashed the
                            // COSMIC compositor on exit.
                            self.leaving = true;
                            task = after(LEAVE_FULLSCREEN_DELAY, SizingMessage::FreezeFrames);
                        }
                    }
                }
            }
            SizingMessage::FreezeFrames => {
                if self.leaving && !self.fullscreen {
                    self.frozen = true;
                    task = after(NATIVE_EXIT_FREEZE, SizingMessage::ApplyFullscreen(false));
                }
            }
            SizingMessage::ApplyFullscreen(on) => {
                // The player may have toggled again during the delay.
                if self.fullscreen == on && (on || self.leaving) {
                    task = set_fullscreen(on);
                    if !on {
                        task = iced::Task::batch([
                            task,
                            after(LEAVE_FULLSCREEN_DELAY, SizingMessage::FullscreenSettled),
                        ]);
                    }
                }
            }
            SizingMessage::FullscreenSettled => {
                if !self.fullscreen {
                    self.leaving = false;
                    self.frozen = false;
                }
            }
            SizingMessage::WasMaximized(maximized) => self.was_maximized = maximized,
            SizingMessage::Shape(shape) => {
                self.uploads.retry();
                self.shape = shape;
            }
            SizingMessage::Filter(filter) => {
                self.uploads.retry();
                self.filter = filter;
            }
            SizingMessage::Crt(enabled) => {
                self.uploads.retry();
                self.crt = enabled;
            }
            SizingMessage::FrameRate(rate) => self.frame_rate = rate,
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

/// Time to present the restored layout before the window leaves full screen,
/// and again for the windowed mode to settle afterwards.
pub const LEAVE_FULLSCREEN_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// How long a native full screen presents no frames before it ends.
pub const NATIVE_EXIT_FREEZE: std::time::Duration = std::time::Duration::from_secs(1);

/// Borderless full screen: drop the decorations and maximize, remembering
/// whether the window was maximized already.
fn enter_borderless() -> iced::Task<SizingMessage> {
    use iced::window;
    window::latest().and_then(|id| {
        window::is_maximized(id).then(move |maximized| {
            iced::Task::batch([
                iced::Task::done(SizingMessage::WasMaximized(maximized)),
                window::toggle_decorations(id),
                window::maximize(id, true),
            ])
        })
    })
}

fn leave_borderless(maximized: bool) -> iced::Task<SizingMessage> {
    use iced::window;
    window::latest().and_then(move |id| {
        iced::Task::batch([
            window::toggle_decorations(id),
            window::maximize(id, maximized),
        ])
    })
}

/// Deliver `message` after `delay` without blocking iced's executor.
#[cfg(not(target_arch = "wasm32"))]
pub fn after<T: Send + 'static>(delay: std::time::Duration, message: T) -> iced::Task<T> {
    let (sender, receiver) = iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        let _ = sender.send(message);
    });
    iced::Task::future(async move { receiver.await.ok() }).and_then(iced::Task::done)
}

/// The browser has no compositor issue to wait for; deliver immediately.
#[cfg(target_arch = "wasm32")]
pub fn after<T: Send + 'static>(_: std::time::Duration, message: T) -> iced::Task<T> {
    iced::Task::done(message)
}

/// Ask the host for full screen or windowed mode, but only when the real
/// window mode differs, so a stale flag never repeats a mode request.
pub fn set_fullscreen<T: Send + 'static>(on: bool) -> iced::Task<T> {
    use iced::window::{self, Mode};
    window::latest().and_then(move |id| {
        window::mode(id).then(move |current| {
            if (current == Mode::Fullscreen) == on {
                iced::Task::none()
            } else {
                window::set_mode(id, if on { Mode::Fullscreen } else { Mode::Windowed })
            }
        })
    })
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
    fn wide_shapes_use_sixteen_by_nine_and_stretch_warps_only_there() {
        let frame = frame(640, 480);
        let pixels = frame.dimensions().unwrap();
        let available = Size::new(1600.0, 900.0);
        let mut sizing = DisplaySizing::default();
        assert!(!sizing.stretched());
        let _ = sizing.update(SizingMessage::Shape(ScreenShape::Wide), &frame);
        assert_eq!(sizing.image_size(available, pixels), available);
        assert!(!sizing.stretched());
        let _ = sizing.update(SizingMessage::Shape(ScreenShape::Stretch), &frame);
        assert_eq!(sizing.image_size(available, pixels), available);
        assert!(sizing.stretched());
        // Pixel perfect suspends the shape without forgetting it.
        let _ = sizing.update(SizingMessage::PixelPerfect, &frame);
        assert!(!sizing.stretched());
        let _ = sizing.update(SizingMessage::Fit, &frame);
        assert_eq!(sizing.shape, ScreenShape::Stretch);
        assert!(sizing.stretched());
    }

    #[test]
    fn pixel_perfect_draws_no_filter_and_gpu_filters_fall_back_without_a_gpu() {
        let frame = frame(320, 200);
        let mut sizing = DisplaySizing::default();
        let output: doslet_config::OutputRouting = doslet_config::OutputRouting {
            video_filter: doslet_config::VideoFilter::Ravu,
            ..Default::default()
        };
        sizing.set_output(&output);
        assert_eq!(sizing.active_filter(), DisplayFilter::Ravu);
        let _ = sizing.update(SizingMessage::PixelPerfect, &frame);
        assert_eq!(sizing.active_filter(), DisplayFilter::Nearest);
        let _ = sizing.update(SizingMessage::Fit, &frame);
        sizing.gpu = false;
        assert_eq!(sizing.active_filter(), DisplayFilter::SharpBilinear);
        sizing.set_output(&doslet_config::OutputRouting {
            video_filter_fallback: Some(doslet_config::VideoFilter::Nearest),
            ..output
        });
        assert_eq!(sizing.active_filter(), DisplayFilter::Nearest);
        // HQ2x is too costly as well; simple filters never change, and 16:9
        // smart becomes an even stretch.
        let _ = sizing.update(SizingMessage::Filter(DisplayFilter::Hq2x), &frame);
        assert_eq!(sizing.active_filter(), DisplayFilter::Nearest);
        let _ = sizing.update(SizingMessage::Filter(DisplayFilter::SharpBilinear), &frame);
        assert_eq!(sizing.active_filter(), DisplayFilter::SharpBilinear);
        let _ = sizing.update(SizingMessage::Shape(ScreenShape::Stretch), &frame);
        assert!(!sizing.stretched());
    }

    #[test]
    fn native_exit_restores_the_bars_then_freezes_frames_then_leaves() {
        let frame = frame(320, 200);
        let mut sizing = DisplaySizing::default();
        let _ = sizing.update(
            SizingMessage::FullscreenStyle(FullscreenStyle::Native),
            &frame,
        );
        let _ = sizing.update(SizingMessage::Fullscreen(true), &frame);
        assert!(sizing.fullscreen && !sizing.leaving_fullscreen());
        // A style change during full screen would break the exit; it waits.
        let _ = sizing.update(
            SizingMessage::FullscreenStyle(FullscreenStyle::Borderless),
            &frame,
        );
        assert_eq!(sizing.fullscreen_style, FullscreenStyle::Native);
        // The bars return at once while frames keep flowing.
        let _ = sizing.update(SizingMessage::Fullscreen(false), &frame);
        assert!(!sizing.fullscreen && sizing.leaving_fullscreen());
        assert!(!sizing.frames_frozen());
        let _ = sizing.update(SizingMessage::FreezeFrames, &frame);
        assert!(sizing.frames_frozen());
        let _ = sizing.update(SizingMessage::ApplyFullscreen(false), &frame);
        assert!(sizing.frames_frozen() && sizing.leaving_fullscreen());
        let _ = sizing.update(SizingMessage::FullscreenSettled, &frame);
        assert!(!sizing.frames_frozen() && !sizing.leaving_fullscreen());
        // Re-entering during the exit cancels the rest of it.
        let _ = sizing.update(SizingMessage::Fullscreen(false), &frame);
        let _ = sizing.update(SizingMessage::Fullscreen(true), &frame);
        let _ = sizing.update(SizingMessage::Fullscreen(false), &frame);
        let _ = sizing.update(SizingMessage::Fullscreen(true), &frame);
        let _ = sizing.update(SizingMessage::FreezeFrames, &frame);
        assert!(sizing.fullscreen && !sizing.frames_frozen());
    }

    #[test]
    fn borderless_full_screen_never_freezes_or_waits() {
        let frame = frame(320, 200);
        let mut sizing = DisplaySizing::default();
        let _ = sizing.update(
            SizingMessage::FullscreenStyle(FullscreenStyle::Borderless),
            &frame,
        );
        let _ = sizing.update(SizingMessage::Fullscreen(true), &frame);
        let _ = sizing.update(SizingMessage::WasMaximized(true), &frame);
        assert!(sizing.fullscreen);
        let _ = sizing.update(SizingMessage::Fullscreen(false), &frame);
        assert!(!sizing.fullscreen && !sizing.leaving_fullscreen());
        let _ = sizing.update(SizingMessage::FreezeFrames, &frame);
        assert!(!sizing.frames_frozen());
    }

    #[test]
    fn frame_rate_intervals_follow_the_guest_or_the_limit() {
        use std::time::Duration;
        assert_eq!(
            FrameRate::default().interval(70_086),
            Duration::from_nanos(16_666_666)
        );
        assert_eq!(
            FrameRate::Limit(30).interval(0),
            Duration::from_nanos(33_333_333)
        );
        assert_eq!(
            FrameRate::Guest.interval(70_086),
            Duration::from_nanos(14_268_184)
        );
        // An unknown guest refresh behaves like 60 Hz; extremes are bounded.
        assert_eq!(
            FrameRate::Guest.interval(0),
            FrameRate::Limit(60).interval(0)
        );
        assert_eq!(
            FrameRate::Limit(1000).interval(0),
            FrameRate::Limit(240).interval(0)
        );
        for rate in FrameRate::ALL {
            assert_eq!(FrameRate::from_code(rate.code()), *rate);
        }
    }

    #[test]
    fn frame_meter_counts_new_pictures_and_drops_to_zero_when_idle() {
        let start = web_time::Instant::now();
        let at = |ms| start + std::time::Duration::from_millis(ms);
        let mut meter = FrameMeter::default();
        let first = Handle::from_rgba(1, 1, vec![0; 4]);
        meter.observe(Some(&first), at(0));
        assert_eq!(meter.fps(at(0)), None);
        // The same handle again is not a new picture.
        for ms in [100, 200, 300] {
            meter.observe(Some(&first), at(ms));
        }
        for ms in (400..=1000).step_by(100) {
            meter.observe(Some(&Handle::from_rgba(1, 1, vec![0; 4])), at(ms));
        }
        // 1 + 7 new pictures in the first second.
        assert_eq!(meter.fps(at(1000)), Some(8.0));
        assert_eq!(meter.fps(at(3100)), Some(0.0));
    }

    #[test]
    fn stretch_curve_is_monotonic_symmetric_and_keeps_the_centre_near_4_3() {
        assert_eq!(stretch_source(0.0), 0.0);
        assert_eq!(stretch_source(1.0), 1.0);
        assert_eq!(stretch_source(0.5), 0.5);
        let mut previous = 0.0;
        for step in 1..=100 {
            let x = step as f32 / 100.0;
            let u = stretch_source(x);
            assert!(u > previous);
            assert!((u + stretch_source(1.0 - x) - 1.0).abs() < 1e-5);
            previous = u;
        }
        // Near the centre one display unit covers 1/0.75 source units.
        let slope = (stretch_source(0.501) - stretch_source(0.499)) / 0.002;
        assert!((slope - 1.0 / 0.75).abs() < 0.01, "{slope}");
        // Forward map: 0.5 + 0.75 d + d^3.
        let u = stretch_source(0.8);
        let d = u - 0.5;
        assert!((0.5 + 0.75 * d + d * d * d - 0.8).abs() < 1e-5);
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
