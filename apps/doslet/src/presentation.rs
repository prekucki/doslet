#[cfg(not(target_arch = "wasm32"))]
use doslet_runtime::Session;
use iced::widget::image::Handle;

#[derive(Default)]
pub struct DisplayFrame {
    pub image: Option<Handle>,
    #[cfg(not(target_arch = "wasm32"))]
    revision: Option<(usize, usize, u64)>,
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
