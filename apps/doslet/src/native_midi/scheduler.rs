//! Bounded host event queue. Only MIDI events cross this boundary, never a Session.
use midir::MidiOutputConnection;
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const LOOKAHEAD: Duration = Duration::from_millis(80);
const CAPACITY: usize = 8192;
const LATE_TOLERANCE: Duration = Duration::from_millis(2);

trait Sink: Send + 'static {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String>;
}
impl Sink for MidiOutputConnection {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        MidiOutputConnection::send(self, bytes).map_err(|e| e.to_string())
    }
}

enum Message {
    Short { bytes: [u8; 3], len: u8 },
    // SysEx is exceptional host traffic, bounded by the wire decoder to 1024 bytes.
    Long(Box<[u8]>),
}
impl Message {
    fn new(bytes: &[u8]) -> Self {
        if bytes.len() <= 3 {
            let mut short = [0; 3];
            short[..bytes.len()].copy_from_slice(bytes);
            Self::Short {
                bytes: short,
                len: bytes.len() as u8,
            }
        } else {
            Self::Long(bytes.into())
        }
    }
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Short { bytes, len } => &bytes[..usize::from(*len)],
            Self::Long(bytes) => bytes,
        }
    }
}
struct Event {
    nanos: u64,
    message: Message,
}
struct State {
    queue: VecDeque<Event>,
    capacity: usize,
    clock: Option<(u64, Instant)>,
    last_nanos: Option<u64>,
    active: bool,
    silence: bool,
    reset: Option<Message>,
    stop: bool,
    /// Generation of the statistics: only reset() starts a new one, so a
    /// send that completes after a pause still counts.
    epoch: u64,
    error: Option<String>,
    sent: u64,
    late: u64,
    max_late: Duration,
}
enum Action {
    Send {
        message: Message,
        deadline: Instant,
        epoch: u64,
    },
    Silence,
    Reset(Message),
    Stop,
    Wait(Option<Duration>),
}
impl State {
    fn new(capacity: usize) -> Self {
        Self {
            queue: VecDeque::with_capacity(capacity),
            capacity,
            clock: None,
            last_nanos: None,
            active: false,
            silence: false,
            reset: None,
            stop: false,
            epoch: 0,
            error: None,
            sent: 0,
            late: 0,
            max_late: Duration::ZERO,
        }
    }
    fn cancel(&mut self) {
        self.queue.clear();
        self.clock = None;
        self.last_nanos = None;
        self.silence = true;
    }
    fn push(&mut self, nanos: u64, bytes: &[u8]) -> Result<(), String> {
        if let Some(error) = &self.error {
            return Err(error.clone());
        }
        if !self.active {
            return Ok(());
        }
        if self.last_nanos.is_some_and(|last| nanos < last) {
            return Err("MIDI time moved backwards; reset the output".into());
        }
        if self.queue.len() == self.capacity {
            return Err("Host MIDI queue full".into());
        }
        if bytes.is_empty() || bytes.len() > 1024 {
            return Err("Invalid host MIDI message length".into());
        }
        self.queue.push_back(Event {
            nanos,
            message: Message::new(bytes),
        });
        self.last_nanos = Some(nanos);
        Ok(())
    }
    fn next(&mut self, now: Instant) -> Action {
        if self.silence {
            self.silence = false;
            return Action::Silence;
        }
        if self.stop {
            return Action::Stop;
        }
        if let Some(message) = self.reset.take() {
            return Action::Reset(message);
        }
        if self.active
            && self.error.is_none()
            && let Some((guest, host)) = self.clock
            && let Some(event) = self.queue.front()
        {
            let Some(deadline) =
                host.checked_add(Duration::from_nanos(event.nanos.saturating_sub(guest)))
            else {
                self.error = Some("Host MIDI deadline overflow".into());
                self.cancel();
                return self.next(now);
            };
            if now < deadline {
                return Action::Wait(Some(deadline.duration_since(now)));
            }
            let event = self.queue.pop_front().unwrap();
            return Action::Send {
                message: event.message,
                deadline,
                epoch: self.epoch,
            };
        }
        Action::Wait(None)
    }
}
struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}
pub struct Output {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}
impl Output {
    #[cfg(test)]
    pub(super) fn mock() -> (Self, std::sync::mpsc::Receiver<(Instant, Vec<u8>)>) {
        let (tx, rx) = std::sync::mpsc::channel();
        struct Mock(std::sync::mpsc::Sender<(Instant, Vec<u8>)>);
        impl Sink for Mock {
            fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
                self.0
                    .send((Instant::now(), bytes.to_vec()))
                    .map_err(|e| e.to_string())
            }
        }
        (Self::with_sink(Mock(tx)).unwrap(), rx)
    }
    pub fn new(connection: MidiOutputConnection) -> Result<Self, String> {
        Self::with_sink(connection)
    }
    fn with_sink(sink: impl Sink) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::new(CAPACITY)),
            wake: Condvar::new(),
        });
        let worker = shared.clone();
        let thread = thread::Builder::new()
            .name("doslet-midi".into())
            .spawn(move || run(sink, worker))
            .map_err(|e| format!("Cannot start MIDI scheduler: {e}"))?;
        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }
    pub fn check(&self) -> Result<(), String> {
        match &self.shared.state.lock().unwrap().error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
    pub fn push(&self, nanos: u64, bytes: &[u8]) -> Result<(), String> {
        self.shared.state.lock().unwrap().push(nanos, bytes)?;
        self.shared.wake.notify_one();
        Ok(())
    }
    pub fn set_active(&self, active: bool) {
        let mut state = self.shared.state.lock().unwrap();
        state.cancel();
        state.active = active;
        drop(state);
        self.shared.wake.notify_one();
    }
    pub fn start_clock(&self, guest: u64, now: Instant) {
        self.shared.state.lock().unwrap().clock = Some((guest, now));
        self.shared.wake.notify_one();
    }
    pub fn reset(&self, message: &[u8]) {
        let mut state = self.shared.state.lock().unwrap();
        state.cancel();
        state.reset = Some(Message::new(message));
        state.epoch = state.epoch.wrapping_add(1);
        state.sent = 0;
        state.late = 0;
        state.max_late = Duration::ZERO;
        drop(state);
        self.shared.wake.notify_one();
    }
    pub fn stats(&self) -> (u64, usize, u64, f64) {
        let state = self.shared.state.lock().unwrap();
        (
            state.sent,
            state.queue.len(),
            state.late,
            state.max_late.as_secs_f64() * 1000.0,
        )
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        let mut state = self.shared.state.lock().unwrap();
        state.cancel();
        state.stop = true;
        drop(state);
        self.shared.wake.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
fn silence(sink: &mut impl Sink) -> Result<(), String> {
    let mut result = Ok(());
    for channel in 0..16 {
        for cc in [64, 120] {
            if let Err(error) = sink.send(&[0xB0 | channel, cc, 0]) {
                result = Err(error);
            }
        }
    }
    result
}
fn run(mut sink: impl Sink, shared: Arc<Shared>) {
    loop {
        let action = {
            let mut state = shared.state.lock().unwrap();
            loop {
                match state.next(Instant::now()) {
                    Action::Wait(Some(delay)) => {
                        state = shared.wake.wait_timeout(state, delay).unwrap().0
                    }
                    Action::Wait(None) => state = shared.wake.wait(state).unwrap(),
                    action => break action,
                }
            }
        };
        // No host IO under the queue lock; a stalled driver cannot block guest execution.
        let (result, timing) = match action {
            Action::Send {
                message,
                deadline,
                epoch,
            } => {
                let late = Instant::now().saturating_duration_since(deadline);
                (sink.send(message.bytes()), Some((epoch, late)))
            }
            Action::Silence => (silence(&mut sink), None),
            Action::Reset(message) => (sink.send(message.bytes()), None),
            Action::Stop => return,
            Action::Wait(_) => unreachable!(),
        };
        let mut state = shared.state.lock().unwrap();
        match result {
            Ok(()) => {
                if let Some((epoch, late)) = timing
                    && epoch == state.epoch
                {
                    state.sent += 1;
                    state.late += u64::from(late > LATE_TOLERANCE);
                    state.max_late = state.max_late.max(late);
                }
            }
            Err(error) => {
                // Latch once; no retry/error ping-pong on an unavailable device.
                if state.error.is_none() {
                    state.error = Some(format!("Host MIDI send failed: {error}"));
                    state.cancel();
                    state.active = false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn reconnect_resets_a_paused_receiver_before_new_guest_messages() {
        let (output, rx) = Output::mock();
        let reset = doslet_midi_routing::Mode::Mt32.reset_message();
        output.reset(reset);
        // Reset is control traffic: it needs neither active playback nor a clock.
        let mut messages = Vec::new();
        loop {
            let (_, message) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            let initialized = message == reset;
            messages.push(message);
            if initialized {
                break;
            }
        }
        assert_eq!(messages.len(), 33);
        assert!(
            messages[..32]
                .iter()
                .all(|m| m[0] & 0xf0 == 0xb0 && matches!(m[1], 64 | 120))
        );
        output.set_active(true);
        output.push(0, &[0xb1, 7, 100]).unwrap();
        output.push(0, &[0x91, 60, 100]).unwrap();
        output.start_clock(0, Instant::now());
        loop {
            let (_, message) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            if message == [0xb1, 7, 100] {
                break;
            }
            assert_eq!(message[0] & 0xf0, 0xb0);
        }
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)).unwrap().1,
            [0x91, 60, 100]
        );
        output.push(5_000_000_000, &[0x91, 61, 100]).unwrap();
        drop(output); // Disconnect cancels the delayed old note and silences all voices.
        assert!(rx.try_iter().all(|(_, m)| m[0] & 0xf0 == 0xb0));
    }

    #[test]
    fn deadlines_preserve_guest_intervals_and_message_order() {
        let start = Instant::now();
        let mut state = State::new(4);
        state.active = true;
        state.clock = Some((1_000_000_000, start));
        for (nanos, note) in [
            (1_010_000_000, 60),
            (1_010_000_000, 61),
            (1_025_000_000, 62),
        ] {
            state.push(nanos, &[0x90, note, 100]).unwrap();
        }
        assert!(
            matches!(state.next(start), Action::Wait(Some(d)) if d == Duration::from_millis(10))
        );
        for note in [60, 61] {
            match state.next(start + Duration::from_millis(10)) {
                Action::Send {
                    message, deadline, ..
                } => {
                    assert_eq!(message.bytes(), &[0x90, note, 100]);
                    assert_eq!(deadline, start + Duration::from_millis(10));
                }
                _ => panic!("scheduled message missing"),
            }
        }
        assert!(
            matches!(state.next(start + Duration::from_millis(10)), Action::Wait(Some(d)) if d == Duration::from_millis(15))
        );
        assert!(
            matches!(state.next(start + Duration::from_millis(100)), Action::Send { deadline, .. } if deadline == start + Duration::from_millis(25))
        );
        assert!(matches!(state.next(start), Action::Wait(None)));
    }

    #[test]
    fn cancellation_discards_future_notes_and_rebases_on_resume() {
        let start = Instant::now();
        let mut state = State::new(4);
        state.active = true;
        state.clock = Some((100, start));
        state.push(200, &[0x90, 60, 100]).unwrap();
        let epoch = state.epoch;
        state.cancel();
        // A pause still counts a send already in progress.
        assert_eq!(state.epoch, epoch);
        assert!(state.queue.is_empty());
        assert!(matches!(state.next(start), Action::Silence));
        // Replay waits for the new clock, so a pause cannot accumulate wall time.
        state.push(100, &[0x90, 61, 100]).unwrap();
        assert!(matches!(
            state.next(start + Duration::from_secs(60)),
            Action::Wait(None)
        ));
        state.clock = Some((100, start + Duration::from_secs(60)));
        assert!(
            matches!(state.next(start + Duration::from_secs(60)), Action::Send { message, .. } if message.bytes() == [0x90, 61, 100])
        );
    }

    #[test]
    fn queue_capacity_time_and_sysex_are_bounded() {
        let mut state = State::new(2);
        state.active = true;
        state.push(10, &[0x90, 60, 100]).unwrap();
        assert!(state.push(9, &[0x90, 61, 100]).is_err());
        assert!(state.push(10, &[1; 1025]).is_err());
        let mut sysex = [1; 1024];
        sysex[0] = 0xF0;
        sysex[1023] = 0xF7;
        state.push(10, &sysex).unwrap();
        assert!(state.push(10, &[0x80, 60, 0]).is_err());
        assert_eq!(state.queue.len(), 2);
        assert_eq!(state.queue[1].message.bytes(), sysex);
    }

    struct Mock(mpsc::Sender<(Instant, Vec<u8>)>);
    impl Sink for Mock {
        fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.0
                .send((Instant::now(), bytes.to_vec()))
                .map_err(|e| e.to_string())
        }
    }

    #[test]
    fn sender_runs_without_ui_ticks_and_pause_wakes_it_before_future_note() {
        let (tx, rx) = mpsc::channel();
        let output = Output::with_sink(Mock(tx)).unwrap();
        output.set_active(true);
        // The producer queues everything once, then does no work while the sender runs.
        output.push(40_000_000, &[0x90, 60, 100]).unwrap();
        output.push(80_000_000, &[0x80, 60, 0]).unwrap();
        output.push(5_000_000_000, &[0x90, 61, 100]).unwrap();
        let start = Instant::now();
        output.start_clock(0, start);
        let mut notes = Vec::new();
        while notes.len() < 2 {
            let (at, message) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            if message[0] & 0xF0 != 0xB0 {
                notes.push((at, message));
            }
        }
        assert!(notes[0].0 >= start + Duration::from_millis(40));
        assert!(notes[1].0 >= start + Duration::from_millis(80));
        assert_eq!(notes[0].1, [0x90, 60, 100]);
        assert_eq!(notes[1].1, [0x80, 60, 0]);
        output.set_active(false);
        for _ in 0..32 {
            let (_, message) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(message[0] & 0xF0, 0xB0);
        }
        assert_eq!(output.stats().0, 2);
        assert_eq!(output.stats().1, 0);
        let end = Instant::now();
        drop(output); // Must wake a timed/untimed wait; cannot wait for the 5-second note.
        assert!(end.elapsed() < Duration::from_secs(2));
        assert!(rx.try_iter().all(|(_, message)| message[0] & 0xF0 == 0xB0));
    }

    #[test]
    fn slow_host_io_does_not_hold_the_producer_queue_lock() {
        struct Blocked {
            entered: mpsc::Sender<()>,
            unblock: mpsc::Receiver<()>,
        }
        impl Sink for Blocked {
            fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
                if bytes[0] == 0x90 {
                    self.entered.send(()).unwrap();
                    self.unblock.recv_timeout(Duration::from_secs(2)).unwrap();
                }
                Ok(())
            }
        }
        let (entered_tx, entered_rx) = mpsc::channel();
        let (unblock_tx, unblock_rx) = mpsc::channel();
        let output = Output::with_sink(Blocked {
            entered: entered_tx,
            unblock: unblock_rx,
        })
        .unwrap();
        output.set_active(true);
        output.start_clock(0, Instant::now());
        output.push(0, &[0x90, 60, 100]).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(output.shared.state.try_lock().is_ok());
        output.push(10_000_000, &[0x80, 60, 0]).unwrap();
        output.set_active(false);
        assert_eq!(output.stats().1, 0);
        unblock_tx.send(()).unwrap();
        drop(output);
    }

    #[test]
    fn device_failure_is_latched_and_future_notes_are_discarded() {
        struct Broken(mpsc::Sender<()>);
        impl Sink for Broken {
            fn send(&mut self, _: &[u8]) -> Result<(), String> {
                self.0.send(()).unwrap();
                Err("disconnected".into())
            }
        }
        let (tx, rx) = mpsc::channel();
        let output = Output::with_sink(Broken(tx)).unwrap();
        output.set_active(true);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        // A failure silences once more (32 messages), then parks. No infinite retry.
        for _ in 0..63 {
            rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        assert!(output.check().unwrap_err().contains("disconnected"));
        assert!(output.push(0, &[0x90, 60, 100]).is_err());
        assert_eq!(output.stats().1, 0);
        assert!(matches!(
            rx.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }
}
