use super::{Config, Renderer, Source, Stats};
use crate::audio::Writer;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc::{self, Receiver, SyncSender, TryRecvError},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const SLOTS: usize = 8;
const MAX_PENDING_NANOS: u64 = 80_000_000;
// One slot can drain the entire PC PCM queue and translated MIDI batch.
struct Batch {
    pcm: Box<[doslet_pcm::Frame]>,
    pcm_len: usize,
    midi: Box<[u8]>,
    midi_len: usize,
    nanos: u64,
}
impl Batch {
    fn new() -> Self {
        Self {
            pcm: vec![doslet_pcm::EMPTY; 4096].into_boxed_slice(),
            pcm_len: 0,
            midi: vec![0; 16384].into_boxed_slice(),
            midi_len: 0,
            nanos: 0,
        }
    }
}
struct Configure {
    config: Config,
    reply: SyncSender<Result<(), String>>,
}
struct Shared {
    stop: AtomicBool,
    failed: AtomicBool,
    completed: AtomicU64,
    stats: Mutex<Stats>,
    error: Mutex<Option<String>>,
}
impl Shared {
    fn publish(&self, renderer: &Renderer, nanos: u64) {
        *self.stats.lock().unwrap_or_else(|e| e.into_inner()) = renderer.stats();
        self.completed.store(nanos, Ordering::Release);
    }
    fn fail(&self, error: String, output: &Writer) {
        *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
        output.set_active(false);
        self.failed.store(true, Ordering::Release);
    }
}

pub struct Worker {
    submit: SyncSender<Box<Batch>>,
    recycled: Receiver<Box<Batch>>,
    control: SyncSender<Configure>,
    staging: Option<Box<Batch>>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    submitted_nanos: u64,
    pub rate: u32,
    pub mt32: bool,
}
impl Worker {
    pub fn new(config: Config, output: Writer) -> Result<Self, String> {
        let (submit, pending) = mpsc::sync_channel(SLOTS);
        let (recycle, recycled) = mpsc::sync_channel(SLOTS);
        for _ in 0..SLOTS {
            recycle
                .try_send(Box::new(Batch::new()))
                .map_err(|e| e.to_string())?;
        }
        let (control, controls) = mpsc::sync_channel(1);
        let (ready, initialized) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            completed: AtomicU64::new(config.nanos),
            stats: Mutex::new(Stats::default()),
            error: Mutex::new(None),
        });
        let state = shared.clone();
        let rate = config.rate;
        let mt32 = matches!(config.source, Source::Mt32 { .. });
        let nanos = config.nanos;
        let thread = thread::Builder::new()
            .name("doslet-audio".into())
            .spawn(move || {
                // Apple Audio Units, ROMs, synth voices and resampler are created,
                // used and dropped here. No Send promise for a synth is necessary.
                let renderer = match Renderer::new(&config) {
                    Ok(renderer) => renderer,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                state.publish(&renderer, config.nanos);
                output.clear();
                if ready.send(Ok(())).is_err() {
                    return;
                }
                run(renderer, output, pending, recycle, controls, state);
            })
            .map_err(|e| format!("Cannot start audio worker: {e}"))?;
        let worker = Self {
            submit,
            recycled,
            control,
            staging: None,
            shared,
            thread: Some(thread),
            submitted_nanos: nanos,
            rate,
            mt32,
        };
        initialized
            .recv()
            .map_err(|_| "Audio worker stopped during setup".to_string())??;
        Ok(worker)
    }
    fn wake(&self) {
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }
    pub fn check(&self) -> Result<(), String> {
        if self.shared.failed.load(Ordering::Acquire) {
            return Err(self
                .shared
                .error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
                .unwrap_or_else(|| "Audio worker failed".into()));
        }
        if self
            .thread
            .as_ref()
            .is_none_or(|thread| thread.is_finished())
        {
            return Err("Audio worker stopped unexpectedly".into());
        }
        Ok(())
    }
    pub fn configure(&mut self, config: Config) -> Result<(), String> {
        let nanos = config.nanos;
        let rate = config.rate;
        let mt32 = matches!(config.source, Source::Mt32 { .. });
        let (reply, result) = mpsc::sync_channel(1);
        self.control
            .send(Configure { config, reply })
            .map_err(|_| "Audio worker stopped".to_string())?;
        self.wake();
        result
            .recv()
            .map_err(|_| "Audio worker stopped during configuration".to_string())??;
        // The worker has cancelled all older batches and cleared CPAL before
        // acknowledging. This owner cannot enqueue across that boundary.
        if let Some(batch) = &mut self.staging {
            batch.pcm_len = 0;
            batch.midi_len = 0;
            batch.nanos = nanos;
        }
        self.submitted_nanos = nanos;
        self.rate = rate;
        self.mt32 = mt32;
        Ok(())
    }
    pub fn ready(&mut self) -> Result<bool, String> {
        self.check()?;
        if self.pending_nanos() >= MAX_PENDING_NANOS {
            return Ok(false);
        }
        if self.staging.is_none() {
            match self.recycled.try_recv() {
                Ok(mut batch) => {
                    batch.pcm_len = 0;
                    batch.midi_len = 0;
                    batch.nanos = self.submitted_nanos;
                    self.staging = Some(batch);
                }
                Err(TryRecvError::Empty) => return Ok(false),
                Err(TryRecvError::Disconnected) => return Err("Audio worker stopped".into()),
            }
        }
        Ok(true)
    }
    fn batch(&mut self) -> Result<&mut Batch, String> {
        self.staging
            .as_deref_mut()
            .ok_or_else(|| "Audio batch was not reserved".into())
    }
    pub fn receive_pcm(&mut self, frames: &[doslet_pcm::Frame]) -> Result<(), String> {
        let batch = self.batch()?;
        if frames.len() > batch.pcm.len() - batch.pcm_len {
            return Err("Audio input PCM batch full".into());
        }
        batch.pcm[batch.pcm_len..batch.pcm_len + frames.len()].copy_from_slice(frames);
        batch.pcm_len += frames.len();
        Ok(())
    }
    pub fn render_until(&mut self, nanos: u64) -> Result<(), String> {
        if nanos < self.submitted_nanos {
            return Err("Audio time moved backwards".into());
        }
        self.batch()?.nanos = nanos;
        Ok(())
    }
    pub fn push_midi(&mut self, bytes: &[u8]) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        let batch = self.batch()?;
        if bytes.len() > batch.midi.len() - batch.midi_len {
            return Err("Audio input MIDI batch full".into());
        }
        batch.midi[batch.midi_len..batch.midi_len + bytes.len()].copy_from_slice(bytes);
        batch.midi_len += bytes.len();
        Ok(())
    }
    pub fn submit(&mut self) -> Result<(), String> {
        if let Some(batch) = self.staging.take() {
            self.submitted_nanos = batch.nanos;
            // A reserved slot guarantees queue space: there are only SLOTS
            // packets in existence, including the one being sent here.
            self.submit
                .try_send(batch)
                .map_err(|_| "Audio worker queue unavailable".to_string())?;
            self.wake();
        }
        Ok(())
    }
    pub fn pending_nanos(&self) -> u64 {
        self.submitted_nanos
            .saturating_sub(self.shared.completed.load(Ordering::Acquire))
    }
    pub fn stats(&self) -> Stats {
        *self.shared.stats.lock().unwrap_or_else(|e| e.into_inner())
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.wake();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    mut renderer: Renderer,
    output: Writer,
    pending: Receiver<Box<Batch>>,
    recycle: SyncSender<Box<Batch>>,
    controls: Receiver<Configure>,
    shared: Arc<Shared>,
) {
    let mut current: Option<Box<Batch>> = None;
    while !shared.stop.load(Ordering::Acquire) {
        // Control has priority over a full/paused sink. A reset or shutdown
        // never waits for queued sound to be played.
        if let Ok(command) = controls.try_recv() {
            match Renderer::new(&command.config) {
                Ok(replacement) => {
                    renderer = replacement;
                    if let Some(batch) = current.take() {
                        let _ = recycle.try_send(batch);
                    }
                    while let Ok(batch) = pending.try_recv() {
                        let _ = recycle.try_send(batch);
                    }
                    output.clear();
                    *shared.error.lock().unwrap_or_else(|e| e.into_inner()) = None;
                    shared.failed.store(false, Ordering::Release);
                    shared.publish(&renderer, command.config.nanos);
                    let _ = command.reply.send(Ok(()));
                }
                Err(error) => {
                    let _ = command.reply.send(Err(error));
                }
            }
        }
        if shared.failed.load(Ordering::Acquire) {
            thread::park();
            continue;
        }
        if let Err(error) = output.check() {
            shared.fail(error, &output);
            continue;
        }
        if current.is_none() {
            match pending.try_recv() {
                Ok(batch) => {
                    if let Err(error) = renderer.mixer.receive(&batch.pcm[..batch.pcm_len]) {
                        shared.fail(error.to_string(), &output);
                    }
                    current = Some(batch);
                }
                Err(TryRecvError::Empty) => {
                    thread::park();
                    continue;
                }
                Err(TryRecvError::Disconnected) => break,
            }
            if shared.failed.load(Ordering::Acquire) {
                continue;
            }
        }
        if output.available() < renderer.samples.len() {
            // The callback only consumes; it does not perform synthesis or
            // notify this worker. Poll a full sink at a bounded 2-ms interval.
            thread::park_timeout(Duration::from_millis(2));
            continue;
        }
        let batch = current.as_ref().unwrap();
        let result = renderer.render(batch.nanos).and_then(|count| {
            if count == 0 {
                renderer.push_midi(batch.nanos, &batch.midi[..batch.midi_len])?;
            } else {
                output.push(&renderer.samples[..count])?;
            }
            Ok(count)
        });
        match result {
            Ok(0) => {
                shared.publish(&renderer, batch.nanos);
                let _ = recycle.try_send(current.take().unwrap());
            }
            Ok(_) => {}
            Err(error) => shared.fail(error, &output),
        }
    }
}

#[cfg(test)]
mod tests;
