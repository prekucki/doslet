//! Rust protocol adapter. The reactor copies PCM; this owner handles control I/O.
use super::{CAPACITY, Output, PLAYBACK_RESERVE_MS, Queue, Stream, Writer};
use doslet_pulse::{Client, PlaybackSource, PlaybackStream, connect_named, playback_params, wait};
use std::{
    pin::Pin,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    task::{Context, Poll},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const RATE: u32 = 48000;
pub(super) struct Control {
    stop: AtomicBool,
    thread: OnceLock<thread::Thread>,
    applied: Mutex<Result<u64, String>>,
    changed: Condvar,
    launch: Mutex<Launch>,
}
enum Launch {
    Pending {
        queue: Arc<Mutex<Queue>>,
        name: &'static std::ffi::CStr,
    },
    Running(JoinHandle<()>),
    Failed(String),
    Stopped,
}
impl Control {
    pub fn ensure_started(self: &Arc<Self>) -> Result<(), String> {
        let mut launch = self.launch.lock().unwrap_or_else(|e| e.into_inner());
        let (queue, name) = match &*launch {
            Launch::Pending { queue, name } => (queue.clone(), *name),
            Launch::Running(_) => return Ok(()),
            Launch::Failed(error) => return Err(error.clone()),
            Launch::Stopped => return Err("Audio output was closed".into()),
        };
        let control = self.clone();
        let (ready, initialized) = mpsc::sync_channel(1);
        let spawned = thread::Builder::new().name("doslet-pulse".into()).spawn(move || {
            let _ = control.thread.set(thread::current());
            let generation = queue.lock().unwrap_or_else(|e| e.into_inner()).generation;
            let setup = connect_named(name).and_then(|client| {
                let stream = create(&client, queue.clone(), generation, name)?;
                Ok((client, stream))
            });
            match setup {
                Ok((client, stream)) => {
                    let rate = stream.sample_spec().sample_rate;
                    control.acknowledge(Ok(generation));
                    eprintln!("[INFO doslet] Audio output: {name:?}, {rate} Hz stereo f32 (Rust PulseAudio)");
                    let _ = ready.send(Ok(()));
                    if let Err(error) = recoverable_run(client, stream, &queue, &control, name, generation) {
                        queue.lock().unwrap_or_else(|e| e.into_inner()).error = Some(error.clone());
                        control.acknowledge(Err(error));
                    }
                }
                Err(error) => { let _ = ready.send(Err(error)); }
            }
        });
        let result = match spawned {
            Ok(thread) => {
                *launch = Launch::Running(thread);
                initialized
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|e| format!("PulseAudio setup: {e}"))
                    .and_then(|result| result)
            }
            Err(error) => Err(error.to_string()),
        };
        if let Err(error) = &result {
            self.stop.store(true, Ordering::Release);
            self.wake();
            let previous = std::mem::replace(&mut *launch, Launch::Failed(error.clone()));
            if let Launch::Running(thread) = previous {
                let _ = thread.join();
            }
            self.acknowledge(Err(error.clone()));
        }
        result
    }
    pub fn wake(&self) {
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }
    pub fn wait_for(&self, generation: u64) -> Result<(), String> {
        match &*self.launch.lock().unwrap_or_else(|e| e.into_inner()) {
            Launch::Pending { .. } => return Ok(()),
            Launch::Failed(error) => return Err(error.clone()),
            Launch::Stopped => return Err("Audio output was closed".into()),
            Launch::Running(_) => {}
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut applied = self.applied.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            match &*applied {
                Ok(current) if *current >= generation => return Ok(()),
                Err(error) => return Err(error.clone()),
                _ => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("PulseAudio reset acknowledgement timed out".into());
            }
            applied = self
                .changed
                .wait_timeout(applied, remaining)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    fn acknowledge(&self, result: Result<u64, String>) {
        *self.applied.lock().unwrap_or_else(|e| e.into_inner()) = result;
        self.changed.notify_all();
    }
}

pub(super) struct Driver {
    control: Arc<Control>,
}
impl Drop for Driver {
    fn drop(&mut self) {
        self.control.stop.store(true, Ordering::Release);
        self.control.wake();
        let previous = std::mem::replace(
            &mut *self
                .control
                .launch
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
            Launch::Stopped,
        );
        if let Launch::Running(thread) = previous {
            let _ = thread.join();
        }
    }
}

struct Source {
    queue: Arc<Mutex<Queue>>,
    generation: u64,
    rate: u32,
}
impl PlaybackSource for Source {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, bytes: &mut [u8]) -> Poll<usize> {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if q.generation == self.generation && q.len != 0 && q.rates[q.head] != self.rate {
            return Poll::Ready(0); // Finish this format segment before changing rate.
        }
        if q.generation != self.generation || !q.active || q.len == 0 {
            q.waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        let mut count = (bytes.len() / 8).min(q.len).min(512);
        count = (0..count)
            .take_while(|i| q.rates[(q.head + i) % CAPACITY] == self.rate)
            .count();
        if count == 0 {
            return Poll::Pending;
        }
        q.callback_frames = q.callback_frames.max(count);
        for bytes in bytes[..count * 8].as_chunks_mut::<8>().0 {
            let frame = q.pop();
            bytes[..4].copy_from_slice(&frame[0].to_le_bytes());
            bytes[4..].copy_from_slice(&frame[1].to_le_bytes());
        }
        Poll::Ready(count * 8)
    }
}

pub(super) fn open() -> Result<Output, String> {
    open_named(c"Doslet — Mixed audio")
}

pub(super) fn open_named(name: &'static std::ffi::CStr) -> Result<Output, String> {
    open_at_rate(name, RATE)
}
pub(super) fn open_at_rate(name: &'static std::ffi::CStr, rate: u32) -> Result<Output, String> {
    let output = deferred_at_rate(name, rate);
    output
        .writer
        .pulse
        .as_ref()
        .expect("Pulse control")
        .ensure_started()?;
    Ok(output)
}

/// No connection, stream, driver thread or sample buffers until first publish.
pub(super) fn deferred_at_rate(name: &'static std::ffi::CStr, rate: u32) -> Output {
    let queue = Arc::new(Mutex::new(Queue {
        frames: Box::default(),
        rates: Box::default(),
        sample_rate: rate,
        head: 0,
        len: 0,
        active: true,
        consumed: 0,
        underruns: 0,
        callback_frames: 480,
        refilling: true,
        host_clock_reserve: false,
        priming: false,
        peak: 0.0,
        gain: 1.0,
        spectrum: doslet_pcm::spectrum::Spectrum::new(rate),
        error: None,
        generation: 0,
        waker: None,
    }));
    let control = Arc::new(Control {
        stop: AtomicBool::new(false),
        thread: OnceLock::new(),
        applied: Mutex::new(Ok(0)),
        changed: Condvar::new(),
        launch: Mutex::new(Launch::Pending {
            queue: queue.clone(),
            name,
        }),
    });
    let driver = Driver {
        control: control.clone(),
    };
    Output {
        _stream: Stream::Pulse { _driver: driver },
        writer: Writer {
            queue,
            secondary: None,
            fm: None,
            rate,
            pulse: Some(control),
        },
    }
}

fn stream_params(
    rate: u32,
    name: &'static std::ffi::CStr,
) -> Result<doslet_pulse::protocol::PlaybackStreamParams, String> {
    let mut params = playback_params(rate, 2, name)?;
    #[cfg(test)]
    if std::env::var_os("DOSLET_PULSE_PROBE_LEGACY_BUFFER").is_some() {
        return Ok(params); // Explicit A/B reference, absent from normal builds.
    }
    // Every PC backend is a real-time producer and can be delayed by host I/O
    // or scheduling. Buffer at the server; Source must answer requests now.
    // Automatic prebuffering also stops the read index running ahead forever
    // after an underrun, unlike the manual-start pre_buffering=0 policy.
    let bytes = rate * PLAYBACK_RESERVE_MS as u32 / 1000 * 8;
    params.buffer_attr.target_length = bytes;
    params.buffer_attr.pre_buffering = bytes;
    // Keep this reserve in the stream itself. With adjust_latency, the server
    // subtracts the device latency from target_length, leaving less PCM to
    // absorb a producer stall (notably with a large PipeWire graph quantum).
    params.flags.adjust_latency = false;
    Ok(params)
}

fn underrun_delta(previous: &mut u64, reported_bytes: u64) -> u64 {
    // Compatibility servers can encode unknown/negative values in this u64.
    // Do not turn an unavailable report into 2^61 missed stereo frames.
    if reported_bytes > i64::MAX as u64 {
        return 0;
    }
    let current = reported_bytes / 8;
    let delta = current.checked_sub(*previous).unwrap_or(current);
    *previous = current;
    delta
}

fn startup_reserve() -> Duration {
    #[cfg(test)]
    if std::env::var_os("DOSLET_PULSE_PROBE_LEGACY_BUFFER").is_some() {
        return Duration::ZERO;
    }
    Duration::from_millis(PLAYBACK_RESERVE_MS as u64)
}

fn create(
    client: &Client,
    queue: Arc<Mutex<Queue>>,
    generation: u64,
    name: &'static std::ffi::CStr,
) -> Result<PlaybackStream, String> {
    let rate = {
        let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
        let rate = if q.len == 0 {
            q.sample_rate
        } else {
            q.rates[q.head]
        };
        q.sample_rate = rate;
        q.spectrum = doslet_pcm::spectrum::Spectrum::new(rate);
        rate
    };
    let mut params = stream_params(rate, name)?;
    params.sink_name = std::env::var("PULSE_SINK")
        .ok()
        .map(std::ffi::CString::new)
        .transpose()
        .map_err(|e| format!("Invalid PULSE_SINK: {e}"))?;
    #[cfg(test)]
    if params.sink_name.as_deref() == Some(c"doslet-audio-probe-output") {
        // Isolate opt-in null-sink tests from the user's saved game mute state.
        params
            .props
            .set(doslet_pulse::protocol::Prop::MediaRole, c"doslet-test");
        params.flags.start_muted = Some(false);
        params.flags.no_move = true;
    }
    // Compatibility servers can silently fall back to the default output for
    // an unknown explicit name. Honor the requested routing or fail first.
    if let Some(sink) = &params.sink_name {
        wait(client.sink_info_by_name(sink.clone()))
            .map_err(|error| format!("Requested PulseAudio sink {sink:?}: {error}"))?;
    }
    let expected = params.sample_spec;
    let stream = wait(client.create_playback_stream(
        params,
        Source {
            queue,
            generation,
            rate,
        },
    ))?;
    if *stream.sample_spec() != expected {
        return Err("PulseAudio changed the requested PCM format".into());
    }
    Ok(stream)
}

fn run(
    client: Client,
    mut stream: PlaybackStream,
    queue: &Arc<Mutex<Queue>>,
    control: &Control,
    mut generation: u64,
    name: &'static std::ffi::CStr,
) -> Result<(), String> {
    let mut playing = false;
    // Explicitly keep the new stream corked while a wall-clock producer
    // supplies its reserve. Compatibility servers may begin an uncorked
    // stream before the requested prebuffer has accumulated.
    let mut start_after = Instant::now() + startup_reserve();
    let mut next_stats = Instant::now();
    let mut previous_underrun = 0;
    while !control.stop.load(Ordering::Acquire) {
        let (active, current, rate_changed) = {
            let q = queue.lock().unwrap_or_else(|e| e.into_inner());
            (
                q.active,
                q.generation,
                q.len != 0 && q.rates[q.head] != stream.sample_spec().sample_rate,
            )
        };
        if current == generation && rate_changed {
            if !finish_segment(&stream, queue, control, generation, playing)? {
                continue;
            }
            wait(stream.delete())?;
            stream = create(&client, queue.clone(), current, name)?;
            playing = false;
            start_after = Instant::now(); // The previous format already drained.
            previous_underrun = 0;
        }
        if current != generation {
            // Delete rather than merely flush: bytes already queued inside the
            // client reactor must never escape into the next generation.
            wait(stream.delete())?;
            stream = create(&client, queue.clone(), current, name)?;
            generation = current;
            playing = false;
            start_after = Instant::now() + startup_reserve();
            previous_underrun = 0;
            control.acknowledge(Ok(generation));
        }
        if active != playing && (!active || Instant::now() >= start_after) {
            if active {
                wait(stream.uncork())?;
            } else {
                wait(stream.cork())?;
            }
            playing = active;
        }
        if Instant::now() >= next_stats {
            let timing = wait(stream.timing_info())?;
            // Timing reports the current underrun interval, not a lifetime
            // counter. Accumulate observed deltas; short gaps between queries
            // remain outside this sampled diagnostic.
            let delta = underrun_delta(&mut previous_underrun, timing.underrun_for);
            let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
            q.underruns = q.underruns.saturating_add(delta);
            drop(q);
            next_stats = Instant::now() + Duration::from_millis(100);
        }
        thread::park_timeout(Duration::from_millis(2));
    }
    Ok(())
}

/// Rate changes drain the old format, but pause/reset/shutdown must remain
/// responsive. A reset cancels this drain and deletes the obsolete stream.
fn finish_segment(
    stream: &PlaybackStream,
    queue: &Mutex<Queue>,
    control: &Control,
    generation: u64,
    mut playing: bool,
) -> Result<bool, String> {
    use std::future::Future;
    let mut future = std::pin::pin!(stream.play_all());
    let mut cx = Context::from_waker(std::task::Waker::noop());
    let mut active_wait = Duration::ZERO;
    let mut previous = Instant::now();
    loop {
        let (active, current) = {
            let q = queue.lock().unwrap_or_else(|e| e.into_inner());
            (q.active, q.generation)
        };
        if current != generation || control.stop.load(Ordering::Acquire) {
            return Ok(false);
        }
        if active != playing {
            if active {
                wait(stream.uncork())?;
            } else {
                wait(stream.cork())?;
            }
            playing = active;
        }
        let now = Instant::now();
        if active {
            active_wait += now.duration_since(previous);
        }
        previous = now;
        if let Poll::Ready(result) = future.as_mut().poll(&mut cx) {
            return result.map(|()| true).map_err(|e| e.to_string());
        }
        if active_wait > Duration::from_secs(2) {
            return Err("PulseAudio format drain timed out".into());
        }
        thread::park_timeout(Duration::from_millis(2));
    }
}

fn recoverable_run(
    client: Client,
    stream: PlaybackStream,
    queue: &Arc<Mutex<Queue>>,
    control: &Control,
    name: &'static std::ffi::CStr,
    generation: u64,
) -> Result<(), String> {
    let mut connection = (client, stream, generation);
    // A finite budget for this output's lifetime, including repeated short-lived
    // reconnections. Explicitly reopening audio creates a new budget.
    let mut retries = 0;
    loop {
        let (client, stream, generation) = connection;
        let mut error = match run(client, stream, queue, control, generation, name) {
            Ok(()) => return Ok(()),
            Err(error) => error,
        };
        loop {
            if control.stop.load(Ordering::Acquire) {
                return Ok(());
            }
            if retries == 3 {
                return Err(format!(
                    "PulseAudio recovery exhausted after 3 attempts: {error}"
                ));
            }
            retries += 1;
            eprintln!("[WARN doslet] PulseAudio reconnect {retries}/3: {error}");
            thread::park_timeout(Duration::from_millis(100 * retries));
            if control.stop.load(Ordering::Acquire) {
                return Ok(());
            }
            let generation = queue.lock().unwrap_or_else(|e| e.into_inner()).generation;
            let setup = connect_named(name).and_then(|client| {
                let stream = create(&client, queue.clone(), generation, name)?;
                Ok((client, stream, generation))
            });
            match setup {
                Ok(ready) => {
                    connection = ready;
                    control.acknowledge(Ok(generation));
                    break;
                }
                Err(failure) => error = failure,
            }
        }
    }
}

#[cfg(test)]
mod tests;
