//! Deferred CoreAudio DefaultOutput AudioUnit streams for macOS system mixing.
use super::{Output, Queue, Stream, Writer};
use coreaudio::audio_unit::{
    AudioUnit, IOType, SampleFormat, Scope, StreamFormat,
    audio_format::LinearPcmFlags,
    render_callback::{self, data},
};
use std::{
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

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
        name: &'static str,
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
        let spawned = thread::Builder::new()
            .name("doslet-coreaudio".into())
            .spawn(move || {
                let _ = control.thread.set(thread::current());
                let (generation, rate) = {
                    let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                    let rate = if q.len == 0 {
                        q.sample_rate
                    } else {
                        q.rates[q.head]
                    };
                    q.sample_rate = rate;
                    q.spectrum = doslet_pcm::spectrum::Spectrum::new(rate);
                    (q.generation, rate)
                };
                match create_unit(queue.clone(), rate, generation) {
                    Ok(unit) => {
                        control.acknowledge(Ok(generation));
                        eprintln!(
                            "[INFO doslet] Audio output: {name:?}, {rate} Hz stereo f32 (CoreAudio)"
                        );
                        let _ = ready.send(Ok(()));
                        if let Err(error) = run(unit, &queue, &control, rate, generation) {
                            queue.lock().unwrap_or_else(|e| e.into_inner()).error =
                                Some(error.clone());
                            control.acknowledge(Err(error));
                        }
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                    }
                }
            });
        let result = match spawned {
            Ok(thread) => {
                *launch = Launch::Running(thread);
                initialized
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|e| format!("CoreAudio setup: {e}"))
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
                return Err("CoreAudio reset acknowledgement timed out".into());
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

/// No AudioUnit, driver thread or sample buffers until first publish.
pub(super) fn deferred_at_rate(name: &'static str, rate: u32) -> Output {
    let queue = Arc::new(Mutex::new(Queue {
        frames: Box::default(),
        rates: Box::default(),
        sample_rate: rate,
        head: 0,
        len: 0,
        active: true,
        consumed: 0,
        underruns: 0,
        callback_frames: 512,
        refilling: true,
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
        _stream: Stream::CoreAudio { _driver: driver },
        writer: Writer {
            queue,
            secondary: None,
            fm: None,
            rate,
            macos: Some(control),
        },
    }
}

struct RenderContext {
    queue: Arc<Mutex<Queue>>,
    generation: u64,
    rate: u32,
    control_thread: thread::Thread,
}

impl RenderContext {
    fn render_interleaved(&mut self, frames: usize, out: &mut [f32]) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if q.generation != self.generation || !q.active {
            out.fill(0.0);
            return;
        }
        q.callback_frames = q.callback_frames.max(frames);
        let mut rate_boundary = false;
        for slot in out.as_chunks_mut::<2>().0 {
            *slot = if q.len != 0 && q.rates[q.head] != self.rate {
                rate_boundary = true;
                [0.0; 2]
            } else {
                q.next()
            };
        }
        drop(q);
        if rate_boundary {
            self.control_thread.unpark();
        }
    }
}

fn create_unit(queue: Arc<Mutex<Queue>>, rate: u32, generation: u64) -> Result<AudioUnit, String> {
    let mut unit =
        AudioUnit::new(IOType::DefaultOutput).map_err(|e| format!("CoreAudio create: {e}"))?;
    configure_unit(&mut unit, queue, rate, generation)?;
    Ok(unit)
}

fn configure_unit(
    unit: &mut AudioUnit,
    queue: Arc<Mutex<Queue>>,
    rate: u32,
    generation: u64,
) -> Result<(), String> {
    let format = StreamFormat {
        sample_rate: f64::from(rate),
        sample_format: SampleFormat::F32,
        flags: LinearPcmFlags::IS_FLOAT | LinearPcmFlags::IS_PACKED,
        channels: 2,
    };
    unit.set_stream_format(format, Scope::Input)
        .map_err(|e| format!("CoreAudio set_stream_format: {e}"))?;
    let mut context = RenderContext {
        queue,
        generation,
        rate,
        control_thread: thread::current(),
    };
    unit.set_render_callback(move |args: render_callback::Args<data::Interleaved<f32>>| {
        context.render_interleaved(args.num_frames, args.data.buffer);
        Ok(())
    })
    .map_err(|e| format!("CoreAudio set_render_callback: {e}"))?;
    Ok(())
}

fn run(
    mut unit: AudioUnit,
    queue: &Arc<Mutex<Queue>>,
    control: &Control,
    mut current_rate: u32,
    mut generation: u64,
) -> Result<(), String> {
    let mut playing = false;
    while !control.stop.load(Ordering::Acquire) {
        let (active, current, next_rate) = {
            let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
            let rate = if q.len != 0 && q.rates[q.head] != current_rate {
                let rate = q.rates[q.head];
                q.sample_rate = rate;
                q.spectrum = doslet_pcm::spectrum::Spectrum::new(rate);
                Some(rate)
            } else {
                None
            };
            (q.active, q.generation, rate)
        };
        if current != generation {
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
            if playing {
                unit.stop().map_err(|e| format!("CoreAudio stop: {e}"))?;
                playing = false;
            }
            unit.uninitialize()
                .map_err(|e| format!("CoreAudio uninitialize: {e}"))?;
            configure_unit(&mut unit, queue.clone(), rate, current)?;
            unit.initialize()
                .map_err(|e| format!("CoreAudio initialize: {e}"))?;
            current_rate = rate;
            generation = current;
            control.acknowledge(Ok(generation));
        } else if let Some(rate) = next_rate {
            if playing {
                unit.stop().map_err(|e| format!("CoreAudio stop: {e}"))?;
            }
            unit.uninitialize()
                .map_err(|e| format!("CoreAudio uninitialize: {e}"))?;
            configure_unit(&mut unit, queue.clone(), rate, current)?;
            unit.initialize()
                .map_err(|e| format!("CoreAudio initialize: {e}"))?;
            if playing {
                unit.start().map_err(|e| format!("CoreAudio start: {e}"))?;
            }
            current_rate = rate;
        }
        if active != playing {
            if active {
                unit.start().map_err(|e| format!("CoreAudio start: {e}"))?;
            } else {
                unit.stop().map_err(|e| format!("CoreAudio stop: {e}"))?;
            }
            playing = active;
        }
        thread::park_timeout(Duration::from_millis(2));
    }
    if playing {
        let _ = unit.stop();
    }
    let _ = unit.free_render_callback();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::CAPACITY;

    fn assert_dormant(writer: &Writer) {
        let queue = writer.queue.lock().unwrap();
        assert!(queue.frames.is_empty());
        assert!(queue.rates.is_empty());
        assert_eq!(queue.len, 0);
        drop(queue);
        let control = writer.macos.as_ref().unwrap();
        assert!(matches!(
            *control.launch.lock().unwrap(),
            Launch::Pending { .. }
        ));
        assert!(control.thread.get().is_none());
    }

    #[test]
    fn unused_system_outputs_survive_controls_without_connecting_or_allocating_samples() {
        let output = Output::new(super::super::Backend::Cpal, super::super::Mixer::System).unwrap();
        let writer = output.writer();
        writer.set_volume(35);
        writer.set_active(false);
        writer.clear().unwrap();
        writer.set_active(true);
        writer.push(&[]).unwrap();
        writer.push_hardware(false, 11025, &[]).unwrap();
        writer.push_hardware(true, 48000, &[]).unwrap();
        writer.check().unwrap();
        assert_eq!(writer.available(), CAPACITY);
        assert_eq!(writer.refill_nanos(), 0);
        assert_dormant(&writer);
        assert_dormant(writer.secondary.as_ref().unwrap());
        assert_dormant(writer.fm.as_ref().unwrap());
    }

    #[test]
    fn external_midi_only_worker_never_starts_any_local_system_output() {
        use crate::native_audio::{Config, Source, Worker};
        let output = Output::new(super::super::Backend::Cpal, super::super::Mixer::System).unwrap();
        let writer = output.writer();
        let mut worker = Worker::new(
            Config {
                source: Source::External,
                rate: 48000,
                nanos: 0,
                replay: Vec::new(),
            },
            writer.clone(),
        )
        .unwrap();
        for nanos in [10_000_000, 1_000_000_000, 2_000_000_000] {
            assert!(worker.ready().unwrap());
            worker.push_midi(&[0xc0, 20, 0x90, 60, 100]).unwrap();
            worker.render_until(nanos).unwrap();
            worker.submit().unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while worker.pending_nanos() != 0 {
                worker.check().unwrap();
                assert!(
                    Instant::now() < deadline,
                    "external MIDI batch did not complete"
                );
                thread::sleep(Duration::from_millis(1));
            }
        }
        assert_eq!(worker.stats(), Default::default());
        assert_dormant(&writer);
        assert_dormant(writer.secondary.as_ref().unwrap());
        assert_dormant(writer.fm.as_ref().unwrap());
    }

    #[test]
    fn paused_and_obsolete_sources_cannot_consume_current_generation_and_stop_at_rate_boundary() {
        let writer = Writer::mock(48000);
        let mut context = RenderContext {
            queue: writer.queue.clone(),
            generation: 0,
            rate: 48000,
            control_thread: thread::current(),
        };
        let mut out = [0.0; 4];
        writer.push(&[[0.25, -0.5]; 2]).unwrap();
        writer.set_active(false);
        context.render_interleaved(2, &mut out);
        assert_eq!(out, [0.0; 4]);
        assert_eq!(writer.available(), CAPACITY - 2);

        writer.set_active(true);
        context.render_interleaved(2, &mut out);
        assert_eq!(out, [0.25, -0.5, 0.25, -0.5]);

        writer.push(&[[0.75; 2]; 10]).unwrap();
        writer.clear().unwrap();
        writer.push(&[[-0.25; 2]; 2]).unwrap();
        context.render_interleaved(2, &mut out);
        assert_eq!(out, [0.0; 4]);
        assert_eq!(writer.available(), CAPACITY - 2);

        context.generation = 1;
        writer.push_at_rate(22050, &[[0.5; 2]; 2]).unwrap();
        let mut four = [0.0; 8];
        context.render_interleaved(4, &mut four);
        assert_eq!(four[..4], [-0.25, -0.25, -0.25, -0.25]);
        assert_eq!(four[4..], [0.0; 4]);
        assert_eq!(writer.available(), CAPACITY - 2);
    }

    #[test]
    fn coreaudio_unit_handles_rate_changes_and_reset_acknowledgement() {
        let output = deferred_at_rate("Doslet CoreAudio test", 11025);
        output.clear().unwrap();
        assert_dormant(&output.writer());
        output.push_at_rate(11025, &[[0.0; 2]; 256]).unwrap();
        output.push_at_rate(22050, &[[0.0; 2]; 512]).unwrap();
        output.push_at_rate(48000, &[[0.0; 2]; 1024]).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while output.queue.lock().unwrap().sample_rate != 48000 || output.available() != CAPACITY {
            output.check().unwrap();
            assert!(
                Instant::now() < deadline,
                "CoreAudio rate transitions did not complete"
            );
            thread::sleep(Duration::from_millis(5));
        }
        output.push_at_rate(32000, &[[0.0; 2]; 512]).unwrap();
        output.set_active(false);
        output.clear().unwrap();
        assert_eq!(output.available(), CAPACITY);
        output.push_at_rate(22050, &[[0.0; 2]; 256]).unwrap();
        output.set_active(true);
        let deadline = Instant::now() + Duration::from_secs(3);
        while output.available() != CAPACITY {
            output.check().unwrap();
            assert!(
                Instant::now() < deadline,
                "CoreAudio post-reset playback did not complete"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
}
