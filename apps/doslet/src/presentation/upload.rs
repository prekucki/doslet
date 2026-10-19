//! Keep the displayed allocation alive until its replacement is ready.
//! One upload and one replaceable latest frame bound work when rendering lags.
use super::DisplayFilter;
use iced::Size;
use iced::widget::image::Handle;
use iced_runtime::image::Allocation;

#[cfg(all(test, not(target_arch = "wasm32")))]
mod gpu_tests;

#[derive(Clone)]
pub struct PreparedFrame {
    pub image: Handle,
    pub(super) generation: u64,
    pub source_size: Size,
    pub filter: DisplayFilter,
}

impl PreparedFrame {
    fn same_image(&self, other: &Self) -> bool {
        self.image.id() == other.image.id() && self.same_configuration(other)
    }

    fn same_configuration(&self, other: &Self) -> bool {
        self.generation == other.generation
            && self.source_size == other.source_size
            && self.filter == other.filter
    }
}

struct Pending {
    id: u64,
    epoch: u64,
    frame: PreparedFrame,
}

// The generic guard lets tests exercise delayed completions without fabricating
// a GPU allocation. Production always holds iced's real allocation guard.
pub(super) struct UploadQueue<A = Allocation> {
    wanted: Option<PreparedFrame>,
    pending: Option<Pending>,
    visible: Option<(PreparedFrame, A)>,
    epoch: u64,
    next_id: u64,
    error: Option<String>,
}

impl<A> Default for UploadQueue<A> {
    fn default() -> Self {
        Self {
            wanted: None,
            pending: None,
            visible: None,
            epoch: 0,
            next_id: 0,
            error: None,
        }
    }
}

impl<A> UploadQueue<A> {
    pub fn frame(&self) -> Option<&PreparedFrame> {
        self.visible.as_ref().map(|(frame, _)| frame)
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn retry(&mut self) {
        self.error = None;
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn offer(&mut self, frame: Option<PreparedFrame>) {
        let configuration_changed = match (&self.wanted, &frame) {
            (Some(old), Some(new)) => !old.same_configuration(new),
            (None, None) => false,
            _ => true,
        };
        if configuration_changed {
            self.epoch = self.epoch.wrapping_add(1);
            self.error = None;
        }
        if frame.is_none() {
            self.visible = None;
        }
        self.wanted = frame;
    }

    pub fn start(&mut self) -> Option<(u64, Handle)> {
        if self.pending.is_some() || self.error.is_some() {
            return None;
        }
        let wanted = self.wanted.as_ref()?;
        if self.frame().is_some_and(|ready| ready.same_image(wanted)) {
            return None;
        }
        self.next_id = self.next_id.wrapping_add(1);
        let request = (self.next_id, wanted.image.clone());
        self.pending = Some(Pending {
            id: self.next_id,
            epoch: self.epoch,
            frame: wanted.clone(),
        });
        Some(request)
    }

    pub fn finish(&mut self, id: u64, allocation: Result<A, String>) {
        if self.pending.as_ref().is_none_or(|pending| pending.id != id) {
            return;
        }
        let pending = self.pending.take().expect("matched pending upload");
        if pending.epoch != self.epoch {
            return;
        }
        match allocation {
            // Present completed frames even if a newer frame is waiting. Dropping
            // every such completion would starve display updates on a slow GPU.
            Ok(allocation) => self.visible = Some((pending.frame, allocation)),
            Err(error) => self.error = Some(format!("Display upload failed: {error}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::Cell, rc::Rc};

    fn frame() -> PreparedFrame {
        PreparedFrame {
            image: Handle::from_rgba(2, 2, vec![255; 16]),
            generation: 0,
            source_size: Size::new(1.0, 1.0),
            filter: DisplayFilter::Hq2x,
        }
    }

    struct Guard(Rc<Cell<usize>>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    #[test]
    fn retains_visible_frame_and_allocation_until_replacement_is_ready() {
        let dropped = Rc::new(Cell::new(0));
        let mut queue = UploadQueue::default();
        let first = frame();
        queue.offer(Some(first.clone()));
        let (id, _) = queue.start().unwrap();
        assert!(queue.frame().is_none());
        queue.finish(id, Ok(Guard(dropped.clone())));

        let next = frame();
        queue.offer(Some(next.clone()));
        let (id, _) = queue.start().unwrap();
        for _ in 0..10 {
            assert_eq!(queue.frame().unwrap().image.id(), first.image.id());
            assert!(queue.start().is_none());
            assert_eq!(dropped.get(), 0);
        }
        queue.finish(id, Ok(Guard(dropped.clone())));
        assert_eq!(queue.frame().unwrap().image.id(), next.image.id());
        assert_eq!(dropped.get(), 1);
        drop(queue);
        assert_eq!(dropped.get(), 2);
    }

    #[test]
    fn coalesces_frames_during_upload_without_starving_presentation() {
        let mut queue = UploadQueue::<()>::default();
        let first = frame();
        queue.offer(Some(first.clone()));
        let (id, _) = queue.start().unwrap();
        for _ in 0..100 {
            queue.offer(Some(frame()));
            assert!(queue.start().is_none());
        }
        let latest = frame();
        queue.offer(Some(latest.clone()));
        queue.finish(id, Ok(()));
        assert_eq!(queue.frame().unwrap().image.id(), first.image.id());
        let (next_id, image) = queue.start().unwrap();
        assert_eq!(image.id(), latest.image.id());
        queue.finish(next_id, Ok(()));
        assert_eq!(queue.frame().unwrap().image.id(), latest.image.id());
        assert!(queue.start().is_none());
    }

    #[test]
    fn filter_change_rejects_old_completion_but_keeps_previous_visible_frame() {
        let mut queue = UploadQueue::<()>::default();
        let first = frame();
        queue.offer(Some(first.clone()));
        let (id, _) = queue.start().unwrap();
        queue.finish(id, Ok(()));
        queue.offer(Some(frame()));
        let (old_id, _) = queue.start().unwrap();
        let mut nearest = frame();
        nearest.filter = DisplayFilter::Nearest;
        queue.offer(Some(nearest.clone()));
        queue.finish(old_id, Ok(()));
        assert_eq!(queue.frame().unwrap().image.id(), first.image.id());
        let (id, _) = queue.start().unwrap();
        queue.finish(old_id, Err("late completion".into()));
        assert!(queue.error().is_none());
        queue.finish(id, Ok(()));
        assert_eq!(queue.frame().unwrap().filter, DisplayFilter::Nearest);
    }

    #[test]
    fn reset_invalidates_inflight_frame_even_when_new_game_has_same_video_mode() {
        let mut queue = UploadQueue::<()>::default();
        queue.offer(Some(frame()));
        let (old_id, _) = queue.start().unwrap();
        queue.offer(None);
        queue.offer(Some(frame()));
        queue.finish(old_id, Ok(()));
        assert!(queue.frame().is_none());
        assert!(queue.start().is_some());
    }

    #[test]
    fn reset_invalidates_inflight_frame_when_snapshot_skips_empty_display() {
        let mut queue = UploadQueue::<()>::default();
        queue.offer(Some(frame()));
        let (old_id, _) = queue.start().unwrap();
        let mut restarted = frame();
        restarted.generation = 1;
        queue.offer(Some(restarted.clone()));
        queue.finish(old_id, Ok(()));
        assert!(queue.frame().is_none());
        let (id, _) = queue.start().unwrap();
        queue.finish(id, Ok(()));
        assert_eq!(queue.frame().unwrap().image.id(), restarted.image.id());
    }

    #[test]
    fn upload_failure_keeps_last_frame_and_requires_explicit_retry() {
        let mut queue = UploadQueue::<()>::default();
        let first = frame();
        queue.offer(Some(first.clone()));
        let (id, _) = queue.start().unwrap();
        queue.finish(id, Ok(()));
        queue.offer(Some(frame()));
        let (id, _) = queue.start().unwrap();
        queue.finish(id, Err("out of memory".into()));
        for _ in 0..10 {
            queue.offer(Some(frame()));
            assert!(queue.start().is_none());
            assert_eq!(queue.frame().unwrap().image.id(), first.image.id());
        }
        assert!(queue.error().unwrap().contains("out of memory"));
        queue.retry();
        assert!(queue.start().is_some());
    }
}
