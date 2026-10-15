//! Coalesced host notifications. The event queue and the machine keep their owners.
use iced::futures::{Stream, task::AtomicWaker};
use std::{
    hash::{Hash, Hasher},
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};

#[derive(Default)]
struct Inner {
    pending: AtomicBool,
    waker: AtomicWaker,
}

#[derive(Clone, Default)]
pub(super) struct Signal(Arc<Inner>);

impl Hash for Signal {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Clones identify the same subscription across view updates.
        std::ptr::hash(Arc::as_ptr(&self.0), state);
    }
}

impl Signal {
    pub(super) fn notify(&self) {
        self.0.pending.store(true, Ordering::Release);
        self.0.waker.wake();
    }

    pub(super) fn clear(&self) {
        self.0.pending.store(false, Ordering::Release);
    }

    pub(super) fn stream(&self) -> Wakeups {
        Wakeups(self.clone())
    }
}

// The idle subscription is the sole listener. Running execution drains the
// queue on its existing timer, with this subscription removed.
pub(super) struct Wakeups(Signal);

impl Stream for Wakeups {
    type Item = ();

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<()>> {
        let signal = &self.0.0;
        // Register before checking to retain events arriving during registration.
        signal.waker.register(cx.waker());
        if signal.pending.swap(false, Ordering::AcqRel) {
            signal.waker.take();
            Poll::Ready(Some(()))
        } else {
            Poll::Pending
        }
    }
}

// Do not unregister on drop: cancellation of an old subscription may finish
// after its replacement has registered. The next poll/notification replaces
// or consumes the stored waker; dropping Signal releases it as well.

#[cfg(test)]
mod tests {
    use super::*;
    use iced::futures::task::{ArcWake, waker};
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct CountWakes(AtomicUsize);
    impl ArcWake for CountWakes {
        fn wake_by_ref(this: &Arc<Self>) {
            this.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn idle_listener_waits_and_worker_events_are_coalesced() {
        let signal = Signal::default();
        let wakes = Arc::new(CountWakes::default());
        let waker = waker(wakes.clone());
        let mut cx = Context::from_waker(&waker);
        let mut stream = signal.stream();
        for _ in 0..10 {
            assert!(Pin::new(&mut stream).poll_next(&mut cx).is_pending());
        }
        assert_eq!(wakes.0.load(Ordering::Relaxed), 0);
        for _ in 0..256 {
            signal.notify();
        }
        assert!(wakes.0.load(Ordering::Relaxed) > 0);
        assert_eq!(
            Pin::new(&mut stream).poll_next(&mut cx),
            Poll::Ready(Some(()))
        );
        assert!(Pin::new(&mut stream).poll_next(&mut cx).is_pending());
    }

    #[test]
    fn events_before_subscription_and_across_pause_transitions_are_retained() {
        let signal = Signal::default();
        let waker = iced::futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        signal.notify();
        let mut stream = signal.stream();
        assert_eq!(
            Pin::new(&mut stream).poll_next(&mut cx),
            Poll::Ready(Some(()))
        );
        drop(stream); // Running: no idle listener.
        signal.notify();
        signal.clear(); // The execution tick drains the already queued event.
        let mut stream = signal.stream(); // Paused again.
        assert!(Pin::new(&mut stream).poll_next(&mut cx).is_pending());
        signal.notify();
        assert_eq!(
            Pin::new(&mut stream).poll_next(&mut cx),
            Poll::Ready(Some(()))
        );
    }

    #[test]
    fn delayed_cancellation_does_not_unregister_the_next_idle_listener() {
        let signal = Signal::default();
        let old = signal.stream();
        let wakes = Arc::new(CountWakes::default());
        let waker = waker(wakes.clone());
        let mut cx = Context::from_waker(&waker);
        let mut current = signal.stream();
        assert!(Pin::new(&mut current).poll_next(&mut cx).is_pending());
        drop(old);
        signal.notify();
        assert_eq!(wakes.0.load(Ordering::Relaxed), 1);
        assert_eq!(
            Pin::new(&mut current).poll_next(&mut cx),
            Poll::Ready(Some(()))
        );
    }
}
