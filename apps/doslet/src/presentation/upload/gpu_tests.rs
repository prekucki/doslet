//! Opt-in renderer regression: no window or guest game is required.
use super::*;
use crate::presentation::filter::FilterCache;
use iced::{Color, Font, Pixels, Rectangle};
use iced_runtime::core::{Renderer as _, image, image::Renderer as _, renderer::Headless};
use std::{
    sync::mpsc,
    time::{Duration, Instant},
};

fn render(renderer: &mut iced::Renderer, frame: Option<&PreparedFrame>) -> Vec<u8> {
    let bounds = Rectangle::with_size(Size::new(64.0, 48.0));
    renderer.reset(bounds);
    if let Some(frame) = frame {
        renderer.draw_image(
            image::Image::new(frame.image.clone()).filter_method(frame.filter.sampling()),
            bounds,
            bounds,
        );
    }
    Headless::screenshot(renderer, Size::new(64, 48), 1.0, Color::BLACK)
}

fn assert_color(pixels: &[u8], expected: [u8; 4]) {
    for (x, y) in [(8, 8), (32, 24), (56, 40)] {
        let offset = (y * 64 + x) * 4;
        assert_eq!(
            &pixels[offset..offset + 4],
            expected,
            "blank or stale image at ({x}, {y})"
        );
    }
}

#[test]
#[ignore = "requires a working wgpu adapter; renders HQ2x frames offscreen"]
fn large_hq2x_frames_remain_visible_during_gpu_upload() {
    let mut renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
        Font::DEFAULT,
        Pixels(16.0),
        Some("wgpu"),
    ))
    .expect("a wgpu adapter is required for this opt-in test");
    let mut queue = UploadQueue::default();
    let mut filter = FilterCache::default();
    let mut previous = None;
    let mut screenshots = 0;
    for color in [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]]
        .into_iter()
        .cycle()
        .take(9)
    {
        let source = Handle::from_rgba(640, 480, color.repeat(640 * 480));
        filter.refresh(Some(&source), DisplayFilter::Hq2x, None);
        let image = filter.image().unwrap().clone();
        assert!(matches!(&image, Handle::Rgba { pixels, .. } if pixels.len() > 2 * 1024 * 1024));
        queue.offer(Some(PreparedFrame {
            image,
            generation: 0,
            source_size: Size::new(640.0, 480.0),
            filter: DisplayFilter::Hq2x,
        }));
        let (id, handle) = queue.start().unwrap();
        let (sender, receiver) = mpsc::channel();
        renderer.allocate_image(&handle, move |allocation| {
            sender.send(allocation).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            // This also lets iced collect completed background uploads. The UI
            // must still draw the old image until it processes the completion.
            let pixels = render(&mut renderer, queue.frame());
            screenshots += 1;
            if let Some(color) = previous {
                assert_color(&pixels, color);
            }
            if let Ok(allocation) = receiver.try_recv() {
                queue.finish(id, Ok(allocation.expect("upload failed")));
                break;
            }
            assert!(Instant::now() < deadline, "GPU upload did not complete");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_color(&render(&mut renderer, queue.frame()), color);
        screenshots += 1;
        previous = Some(color);
    }
    println!("Checked {screenshots} rendered images across 9 HQ2x uploads");
}
