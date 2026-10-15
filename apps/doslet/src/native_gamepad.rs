//! Native controller snapshots carry stable IDs and connection ordering.
use gilrs::{Axis, Button, EventType, GilrsBuilder};
mod wakeup;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub fn uuid_string(uuid: [u8; 16]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(32);
    for byte in uuid {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
#[derive(Clone, Copy, Debug)]
pub struct PadState {
    pub id: usize,
    pub uuid: [u8; 16],
    pub generation: u64,
    /// First sequence of four ordered axis/button events reserved by this snapshot.
    pub sequence: u64,
    /// A first/recovery snapshot must be accepted and then released before execution.
    pub resync: bool,
    pub ambiguous: bool,
    /// Browser-standard ordering, positive Y points down.
    pub axes: [i16; 8],
    pub buttons: u32,
}
#[derive(Debug)]
pub enum GamepadUpdate {
    Connected {
        id: usize,
        name: String,
        uuid: String,
        generation: u64,
    },
    Disconnected {
        id: usize,
        uuid: String,
        generation: u64,
        sequence: u64,
    },
    State(PadState),
    Error(String),
    ReleaseAll,
}
struct Connection {
    uuid: [u8; 16],
    generation: u64,
    sequence: u64,
    resync: bool,
}
pub struct NativeGamepads {
    receiver: Receiver<GamepadUpdate>,
    overflow: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    wakeup: wakeup::Signal,
}
fn send(
    sender: &SyncSender<GamepadUpdate>,
    overflow: &AtomicBool,
    wakeup: &wakeup::Signal,
    event: GamepadUpdate,
) -> Result<bool, ()> {
    match sender.try_send(event) {
        Ok(()) => {
            wakeup.notify();
            Ok(true)
        }
        Err(TrySendError::Full(_)) => {
            overflow.store(true, Ordering::Release);
            wakeup.notify();
            Ok(false)
        }
        Err(TrySendError::Disconnected(_)) => Err(()),
    }
}
impl NativeGamepads {
    #[cfg(test)]
    pub fn test_pair() -> (Self, impl Fn(GamepadUpdate)) {
        let (sender, receiver) = mpsc::sync_channel(256);
        let overflow = Arc::new(AtomicBool::new(false));
        let wakeup = wakeup::Signal::default();
        let pads = Self {
            receiver,
            overflow: overflow.clone(),
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
            wakeup: wakeup.clone(),
        };
        (pads, move |update| {
            assert!(send(&sender, &overflow, &wakeup, update).unwrap());
        })
    }
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::sync_channel(256);
        let overflow = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let worker_overflow = Arc::clone(&overflow);
        let worker_stop = Arc::clone(&stop);
        let wakeup = wakeup::Signal::default();
        let worker_wakeup = wakeup.clone();
        let worker = thread::spawn(move || {
            let mut gilrs = match GilrsBuilder::new()
                .with_default_filters(false)
                .with_force_feedback(false)
                .build()
            {
                Ok(gilrs) => gilrs,
                Err(error) => {
                    let _ = send(
                        &sender,
                        &worker_overflow,
                        &worker_wakeup,
                        GamepadUpdate::Error(error.to_string()),
                    );
                    return;
                }
            };
            let mut connections = BTreeMap::new();
            let mut next_generation = 1u64;
            for (id, pad) in gilrs.gamepads() {
                let id: usize = id.into();
                let uuid = pad.uuid();
                let generation = next_generation;
                next_generation += 1;
                connections.insert(
                    id,
                    Connection {
                        uuid,
                        generation,
                        sequence: 0,
                        resync: true,
                    },
                );
                if send(
                    &sender,
                    &worker_overflow,
                    &worker_wakeup,
                    GamepadUpdate::Connected {
                        id,
                        name: pad.name().into(),
                        uuid: uuid_string(uuid),
                        generation,
                    },
                )
                .is_err()
                {
                    return;
                }
            }
            while !worker_stop.load(Ordering::Acquire) {
                let Some(event) = gilrs.next_event_blocking(Some(Duration::from_millis(16))) else {
                    continue;
                };
                let id: usize = event.id.into();
                if worker_overflow.load(Ordering::Acquire) {
                    for connection in connections.values_mut() {
                        connection.resync = true;
                    }
                }
                let update = match event.event {
                    EventType::Connected => {
                        let Some(generation) = next_generation.checked_add(1) else {
                            let _ = send(
                                &sender,
                                &worker_overflow,
                                &worker_wakeup,
                                GamepadUpdate::Error(
                                    "Controller connection generation exhausted".into(),
                                ),
                            );
                            return;
                        };
                        next_generation = generation;
                        let pad = gilrs.gamepad(event.id);
                        let uuid = pad.uuid();
                        connections.insert(
                            id,
                            Connection {
                                uuid,
                                generation,
                                sequence: 0,
                                resync: true,
                            },
                        );
                        Some(GamepadUpdate::Connected {
                            id,
                            name: pad.name().into(),
                            uuid: uuid_string(uuid),
                            generation,
                        })
                    }
                    EventType::Disconnected => {
                        connections
                            .remove(&id)
                            .map(|connection| GamepadUpdate::Disconnected {
                                id,
                                uuid: uuid_string(connection.uuid),
                                generation: connection.generation,
                                sequence: connection.sequence,
                            })
                    }
                    EventType::ButtonPressed(..)
                    | EventType::ButtonReleased(..)
                    | EventType::ButtonChanged(..)
                    | EventType::AxisChanged(..) => {
                        let Some(connection) = connections.get(&id) else {
                            continue;
                        };
                        let uuid = connection.uuid;
                        let ambiguous = uuid == [0; 16]
                            || connections
                                .values()
                                .filter(|connection| connection.uuid == uuid)
                                .count()
                                != 1;
                        let connection = connections.get_mut(&id).unwrap();
                        let sequence = connection.sequence;
                        let Some(next) = sequence.checked_add(4) else {
                            let _ = send(
                                &sender,
                                &worker_overflow,
                                &worker_wakeup,
                                GamepadUpdate::Error("Controller event sequence exhausted".into()),
                            );
                            return;
                        };
                        connection.sequence = next;
                        let pad = gilrs.gamepad(event.id);
                        let mut axes = [0; 8];
                        for (slot, axis) in [
                            Axis::LeftStickX,
                            Axis::LeftStickY,
                            Axis::RightStickX,
                            Axis::RightStickY,
                            Axis::LeftZ,
                            Axis::RightZ,
                            Axis::DPadX,
                            Axis::DPadY,
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            let value = pad.value(axis).clamp(-1.0, 1.0);
                            let sign = if matches!(slot, 1 | 3 | 7) { -1.0 } else { 1.0 };
                            axes[slot] = (value * sign * 32767.0).round() as i16;
                        }
                        let mut buttons = 0;
                        for (bit, button) in [
                            Button::South,
                            Button::East,
                            Button::West,
                            Button::North,
                            Button::LeftTrigger,
                            Button::RightTrigger,
                            Button::LeftTrigger2,
                            Button::RightTrigger2,
                            Button::Select,
                            Button::Start,
                            Button::LeftThumb,
                            Button::RightThumb,
                            Button::DPadUp,
                            Button::DPadDown,
                            Button::DPadLeft,
                            Button::DPadRight,
                            Button::Mode,
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            if pad.is_pressed(button) {
                                buttons |= 1 << bit;
                            }
                        }
                        Some(GamepadUpdate::State(PadState {
                            id,
                            uuid,
                            generation: connection.generation,
                            sequence,
                            resync: connection.resync,
                            ambiguous,
                            axes,
                            buttons,
                        }))
                    }
                    _ => None,
                };
                let Some(update) = update else {
                    continue;
                };
                let was_state = matches!(update, GamepadUpdate::State(_));
                match send(&sender, &worker_overflow, &worker_wakeup, update) {
                    Ok(true) => {
                        if was_state
                            && !worker_overflow.load(Ordering::Acquire)
                            && let Some(connection) = connections.get_mut(&id)
                        {
                            connection.resync = false;
                        }
                    }
                    Ok(false) => {
                        for connection in connections.values_mut() {
                            connection.resync = true;
                        }
                    }
                    Err(()) => break,
                }
            }
        });
        Self {
            receiver,
            overflow,
            stop,
            worker: Some(worker),
            wakeup,
        }
    }
    pub fn subscription(&self) -> iced::Subscription<()> {
        iced::Subscription::run_with(self.wakeup.clone(), wakeup::Signal::stream)
    }
    /// Clear before draining: a producer racing with the drain can wake us again.
    pub fn acknowledge(&self) {
        self.wakeup.clear();
    }
    pub fn poll(&mut self) -> Option<GamepadUpdate> {
        if self.overflow.load(Ordering::Acquire) {
            // Bounded discard plus a reserved latch. The worker keeps resync set
            // while this flag is raised, so a discarded recovery snapshot cannot
            // re-enable held controls after input loss.
            for _ in 0..256 {
                if self.receiver.try_recv().is_err() {
                    break;
                }
            }
            self.overflow.store(false, Ordering::Release);
            return Some(GamepadUpdate::ReleaseAll);
        }
        self.receiver.try_recv().ok()
    }
}
impl Drop for NativeGamepads {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_identity_format_is_portable() {
        assert_eq!(uuid_string([0xAB; 16]), "abababababababababababababababab");
    }
    #[test]
    fn overflow_release_is_reserved_and_bounded() {
        use iced::futures::Stream;
        use std::{
            pin::Pin,
            task::{Context, Poll},
        };
        let (sender, receiver) = mpsc::sync_channel(256);
        let overflow = Arc::new(AtomicBool::new(false));
        let wakeup = wakeup::Signal::default();
        let mut notifications = wakeup.stream();
        let waker = iced::futures::task::noop_waker();
        let mut cx = Context::from_waker(&waker);
        for _ in 0..256 {
            assert!(send(&sender, &overflow, &wakeup, GamepadUpdate::ReleaseAll).unwrap());
        }
        assert_eq!(
            Pin::new(&mut notifications).poll_next(&mut cx),
            Poll::Ready(Some(()))
        );
        assert!(Pin::new(&mut notifications).poll_next(&mut cx).is_pending());
        // Even a dropped event must wake an idle UI so it releases held input.
        assert!(!send(&sender, &overflow, &wakeup, GamepadUpdate::ReleaseAll).unwrap());
        assert_eq!(
            Pin::new(&mut notifications).poll_next(&mut cx),
            Poll::Ready(Some(()))
        );
        let mut pads = NativeGamepads {
            receiver,
            overflow: overflow.clone(),
            stop: Arc::new(AtomicBool::new(false)),
            worker: None,
            wakeup,
        };
        pads.acknowledge();
        assert!(matches!(pads.poll(), Some(GamepadUpdate::ReleaseAll)));
        assert!(!overflow.load(Ordering::Acquire));
        assert!(pads.poll().is_none());
        assert!(Pin::new(&mut notifications).poll_next(&mut cx).is_pending());
    }
}
