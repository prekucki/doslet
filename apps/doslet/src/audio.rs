//! Host output owns only audio frames, never the emulated machine.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};

#[cfg(target_os = "linux")]
mod pulse;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Backend {
    Auto,
    #[default]
    Cpal,
    Pulse,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum Mixer {
    #[default]
    Application,
    System,
}
impl std::fmt::Display for Mixer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Application => "Internal",
            Self::System => "System (experimental)",
        })
    }
}

const CAPACITY: usize = 16384;
struct Queue {
    frames: Box<[[f32; 2]]>,
    rates: Box<[u32]>,
    sample_rate: u32,
    head: usize,
    len: usize,
    active: bool,
    consumed: u64,
    underruns: u64,
    callback_frames: usize,
    refilling: bool,
    peak: f32,
    gain: f32,
    spectrum: doslet_pcm::spectrum::Spectrum,
    error: Option<String>,
    generation: u64,
    waker: Option<std::task::Waker>,
}
impl Queue {
    fn refill_nanos(&mut self, sample_rate: u32) -> u64 {
        let rate = sample_rate as usize;
        let high = (self.callback_frames.saturating_mul(3) + rate / 125)
            .max(rate * 30 / 1000)
            .min(rate * 80 / 1000)
            .min(CAPACITY - 512);
        let low = (self.callback_frames.saturating_mul(2) + rate / 250)
            .max(rate * 15 / 1000)
            .min(high * 2 / 3);
        if self.len < low {
            self.refilling = true;
        }
        if self.len >= high {
            self.refilling = false;
        }
        if self.active && self.refilling {
            (high.saturating_sub(self.len) as u64 * 1_000_000_000).div_ceil(u64::from(sample_rate))
        } else {
            0
        }
    }
    fn next(&mut self) -> [f32; 2] {
        if !self.active {
            return [0.0; 2];
        }
        if self.len == 0 {
            self.underruns += 1;
            self.spectrum.push([0.0; 2]);
            return [0.0; 2];
        }
        let frame = self.frames[self.head].map(|value| value * self.gain);
        self.head = (self.head + 1) % CAPACITY;
        self.len -= 1;
        self.consumed += 1;
        self.peak = self.peak.max(frame[0].abs()).max(frame[1].abs());
        self.spectrum.push(frame);
        frame
    }
}
pub struct Output {
    _stream: Stream,
    writer: Writer,
}
enum Stream {
    #[cfg(target_os = "linux")]
    System {
        _outputs: [Box<Output>; 3],
    },
    Cpal {
        _stream: cpal::Stream,
    },
    #[cfg(target_os = "linux")]
    Pulse {
        _driver: pulse::Driver,
    },
}
/// The synthesis owner shares frames and playback telemetry, never the device
/// handle or a synthesizer, with CPAL. Stream creation/disposal stays on its owner.
#[derive(Clone)]
pub struct Writer {
    queue: Arc<Mutex<Queue>>,
    secondary: Option<Box<Writer>>,
    fm: Option<Box<Writer>>,
    pub rate: u32,
    #[cfg(target_os = "linux")]
    pulse: Option<Arc<pulse::Control>>,
}
impl std::ops::Deref for Output {
    type Target = Writer;
    fn deref(&self) -> &Writer {
        &self.writer
    }
}
impl Output {
    pub fn new(backend: Backend, mixer: Mixer) -> Result<Self, String> {
        if mixer == Mixer::System {
            #[cfg(target_os = "linux")]
            {
                let pcm = pulse::deferred_at_rate(c"Doslet — Sound Blaster PCM", 22050);
                let fm = pulse::deferred_at_rate(c"Doslet — AdLib / OPL FM", 48000);
                let midi = pulse::deferred_at_rate(c"Doslet — MIDI", 48000);
                let mut writer = midi.writer();
                writer.secondary = Some(Box::new(pcm.writer()));
                writer.fm = Some(Box::new(fm.writer()));
                return Ok(Self {
                    _stream: Stream::System {
                        _outputs: [Box::new(pcm), Box::new(midi), Box::new(fm)],
                    },
                    writer,
                });
            }
            #[cfg(not(target_os = "linux"))]
            return Err("System mixing is available only on Linux".into());
        }
        #[cfg(target_os = "linux")]
        if backend != Backend::Cpal {
            match pulse::open() {
                Ok(output) => return Ok(output),
                Err(error) if backend == Backend::Auto => {
                    eprintln!("[WARN doslet] {error}; using CPAL audio")
                }
                Err(error) => return Err(error),
            }
        }
        #[cfg(not(target_os = "linux"))]
        if backend == Backend::Pulse {
            return Err("PulseAudio output is available only on Linux".into());
        }
        Self::cpal()
    }

    fn cpal() -> Result<Self, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("No audio output device")?;
        let supported = device
            .default_output_config()
            .map_err(|error| error.to_string())?;
        let rate = supported.sample_rate().0;
        if !(22050..=192000).contains(&rate) {
            return Err(format!("Unsupported audio sample rate: {rate}"));
        }
        let queue = Arc::new(Mutex::new(Queue {
            frames: vec![[0.0; 2]; CAPACITY].into_boxed_slice(),
            rates: vec![rate; CAPACITY].into_boxed_slice(),
            sample_rate: rate,
            head: 0,
            len: 0,
            active: true,
            consumed: 0,
            underruns: 0,
            callback_frames: 0,
            refilling: true,
            peak: 0.0,
            gain: 1.0,
            spectrum: doslet_pcm::spectrum::Spectrum::new(rate),
            error: None,
            generation: 0,
            waker: None,
        }));
        let config = supported.config();
        eprintln!(
            "[INFO doslet] Audio output: device={:?} rate={rate} Hz channels={} format={:?}",
            device.name().unwrap_or_default(),
            config.channels,
            supported.sample_format()
        );
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => build(&device, &config, queue.clone(), |v| v),
            cpal::SampleFormat::I16 => build(&device, &config, queue.clone(), |v| {
                (v.clamp(-1.0, 1.0) * 32767.0) as i16
            }),
            cpal::SampleFormat::U16 => build(&device, &config, queue.clone(), |v| {
                ((v.clamp(-1.0, 1.0) + 1.0) * 32767.5) as u16
            }),
            format => return Err(format!("Unsupported audio format: {format:?}")),
        }?;
        stream.play().map_err(|error| error.to_string())?;
        Ok(Self {
            _stream: Stream::Cpal { _stream: stream },
            writer: Writer {
                queue,
                secondary: None,
                fm: None,
                rate,
                #[cfg(target_os = "linux")]
                pulse: None,
            },
        })
    }
    pub fn writer(&self) -> Writer {
        self.writer.clone()
    }
    pub fn is_pulse(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.writer.pulse.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
}
impl Writer {
    pub fn set_volume(&self, percent: u8) {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).gain =
            f32::from(percent.min(100)) / 100.0;
    }

    #[cfg(test)]
    pub fn mock_separate(rate: u32) -> (Self, Self, Self) {
        let pcm = Self::mock(rate);
        let midi = Self::mock(rate);
        let mut output = midi.clone();
        output.secondary = Some(Box::new(pcm.clone()));
        output.fm = Some(Box::new(Self::mock(48000)));
        (output, pcm, midi)
    }
    pub fn mixer(&self) -> Mixer {
        if self.secondary.is_some() {
            Mixer::System
        } else {
            Mixer::Application
        }
    }
    #[cfg(test)]
    pub fn mock(rate: u32) -> Self {
        Self {
            secondary: None,
            fm: None,
            queue: Arc::new(Mutex::new(Queue {
                frames: vec![[0.0; 2]; CAPACITY].into_boxed_slice(),
                rates: vec![rate; CAPACITY].into_boxed_slice(),
                sample_rate: rate,
                head: 0,
                len: 0,
                active: true,
                consumed: 0,
                underruns: 0,
                callback_frames: 256,
                refilling: true,
                peak: 0.0,
                gain: 1.0,
                spectrum: doslet_pcm::spectrum::Spectrum::new(rate),
                error: None,
                generation: 0,
                waker: None,
            })),
            rate,
            #[cfg(target_os = "linux")]
            pulse: None,
        }
    }
    #[cfg(test)]
    pub fn consume(&self, count: usize) -> Vec<[f32; 2]> {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        (0..count).map(|_| queue.next()).collect()
    }
    pub fn check(&self) -> Result<(), String> {
        if let Some(fm) = &self.fm {
            fm.check()?;
        }
        if let Some(secondary) = &self.secondary {
            secondary.check()?;
        }
        let q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.error.clone().map_or(Ok(()), Err)
    }
    pub fn available(&self) -> usize {
        let q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let available = CAPACITY - q.len;
        drop(q);
        let available = self
            .fm
            .as_ref()
            .map_or(available, |s| available.min(s.available()));
        self.secondary
            .as_ref()
            .map_or(available, |s| available.min(s.available()))
    }
    /// Refill below two callback blocks, with margin, until the high watermark.
    /// Bounded by the reserved queue and 80 ms; no audio-device reconfiguration.
    pub fn refill_nanos(&self) -> u64 {
        self.refill_request().unwrap_or(0)
    }
    fn refill_request(&self) -> Option<u64> {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let rate = q.sample_rate;
        let mut request = (!q.frames.is_empty()).then(|| q.refill_nanos(rate));
        drop(q);
        for source in [&self.fm, &self.secondary].into_iter().flatten() {
            if let Some(nanos) = source.refill_request() {
                request = Some(request.map_or(nanos, |current| current.min(nanos)));
            }
        }
        request
    }

    /// The synthesis owner is the sole producer. Both queues must have space
    /// before either source is published; callbacks can only free that space.
    #[cfg(test)]
    pub fn push_sources(&self, pcm: &[[f32; 2]], midi: &[[f32; 2]]) -> Result<(), String> {
        let secondary = self
            .secondary
            .as_ref()
            .ok_or("Separate audio sources require system mixing")?;
        self.check()?;
        if pcm.len() != midi.len() || pcm.len() > self.available() {
            return Err("Separate audio source lengths or capacity mismatch".into());
        }
        secondary.push_at_rate(self.rate, pcm)?;
        self.push(midi)
    }

    pub fn push_hardware(&self, fm: bool, rate: u32, samples: &[[f32; 2]]) -> Result<(), String> {
        let output = if fm { &self.fm } else { &self.secondary };
        output
            .as_ref()
            .ok_or("Hardware sources require system mixing")?
            .push_at_rate(rate, samples)
    }

    pub fn push(&self, frames: &[[f32; 2]]) -> Result<(), String> {
        self.push_at_rate(self.rate, frames)
    }
    fn push_at_rate(&self, rate: u32, frames: &[[f32; 2]]) -> Result<(), String> {
        if !(1000..=192000).contains(&rate) {
            return Err(format!("Unsupported native audio rate: {rate}"));
        }
        if frames.is_empty() {
            return self.check();
        }
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(error) = &q.error {
            return Err(error.clone());
        }
        if frames.len() > CAPACITY - q.len {
            return Err("Audio output queue full".into());
        }
        if q.frames.is_empty() {
            q.frames = vec![[0.0; 2]; CAPACITY].into_boxed_slice();
            q.rates = vec![rate; CAPACITY].into_boxed_slice();
        }
        for frame in frames {
            let index = (q.head + q.len) % CAPACITY;
            q.frames[index] = *frame;
            q.rates[index] = rate;
            q.len += 1;
        }
        let waker = q.waker.take();
        drop(q);
        if let Some(waker) = waker {
            waker.wake();
        }
        #[cfg(target_os = "linux")]
        if let Some(control) = &self.pulse {
            control.ensure_started()?;
        }
        Ok(())
    }
    pub fn set_active(&self, active: bool) {
        if let Some(fm) = &self.fm {
            fm.set_active(active);
        }
        if let Some(secondary) = &self.secondary {
            secondary.set_active(active);
        }
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.active = active;
        if !active {
            q.spectrum.reset();
        }
        let waker = q.waker.take();
        drop(q);
        if let Some(waker) = waker {
            waker.wake();
        }
        #[cfg(target_os = "linux")]
        if let Some(control) = &self.pulse {
            control.wake();
        }
    }
    pub fn clear(&self) -> Result<(), String> {
        if let Some(fm) = &self.fm {
            fm.clear()?;
        }
        if let Some(secondary) = &self.secondary {
            secondary.clear()?;
        }
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.head = 0;
        q.len = 0;
        q.consumed = 0;
        q.underruns = 0;
        q.refilling = true;
        q.peak = 0.0;
        q.spectrum.reset();
        q.generation += 1;
        #[cfg(target_os = "linux")]
        let generation = q.generation;
        drop(q);
        #[cfg(target_os = "linux")]
        if let Some(control) = &self.pulse {
            control.wake();
            control.wait_for(generation)?;
        }
        Ok(())
    }
    pub fn spectrum(&self) -> [f32; doslet_pcm::spectrum::BANDS] {
        let mut levels = self
            .queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .spectrum
            .levels();
        for source in [&self.secondary, &self.fm].into_iter().flatten() {
            for (level, other) in levels.iter_mut().zip(source.spectrum()) {
                *level = level.max(other);
            }
        }
        levels
    }
    pub fn stats(&self) -> (u64, u64, f32) {
        let q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        let (consumed, underruns, peak) = (q.consumed, q.underruns, q.peak);
        drop(q);
        let mut underruns = underruns;
        let mut peak = peak;
        for source in [&self.secondary, &self.fm].into_iter().flatten() {
            let (_, other_underruns, other_peak) = source.stats();
            underruns = underruns.saturating_add(other_underruns);
            peak = peak.max(other_peak);
        }
        // Hardware sample counts have different units; keep MIDI/output-clock
        // frames as the common progress indicator instead of taking their min.
        (consumed, underruns, peak)
    }
}
fn build<T: cpal::SizedSample + Copy + Send + 'static>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    queue: Arc<Mutex<Queue>>,
    convert: fn(f32) -> T,
) -> Result<cpal::Stream, String> {
    let errors = queue.clone();
    let channels = usize::from(config.channels);
    device
        .build_output_stream(
            config,
            move |data: &mut [T], _| {
                let mut q = queue.lock().unwrap_or_else(|e| e.into_inner());
                q.callback_frames = q.callback_frames.max(data.len() / channels);
                for frame in data.chunks_mut(channels) {
                    let value = q.next();
                    for (index, sample) in frame.iter_mut().enumerate() {
                        *sample = convert(if channels == 1 {
                            (value[0] + value[1]) * 0.5
                        } else {
                            value.get(index).copied().unwrap_or(0.0)
                        });
                    }
                }
            },
            move |error| {
                errors.lock().unwrap_or_else(|e| e.into_inner()).error = Some(error.to_string());
            },
            None,
        )
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_changes_buffered_playback_without_changing_queued_audio() {
        let output = Writer::mock(48000);
        output.push(&[[0.8, -0.4]; 4]).unwrap();
        output.set_volume(25);
        assert_eq!(output.consume(1), [[0.2, -0.1]]);
        output.set_volume(0);
        assert_eq!(output.consume(1), [[0.0, 0.0]]);
        output.set_volume(255);
        assert_eq!(output.consume(1), [[0.8, -0.4]]);
        output.set_active(false);
        assert_eq!(output.consume(1), [[0.0, 0.0]]);
        output.set_active(true);
        assert_eq!(output.consume(1), [[0.8, -0.4]]);
    }

    #[test]
    fn refill_watermarks_keep_reserve_without_chattering_or_unbounded_lead() {
        let mut queue = Queue {
            rates: vec![48000; CAPACITY].into_boxed_slice(),
            sample_rate: 48000,
            frames: vec![[0.0; 2]; CAPACITY].into_boxed_slice(),
            head: 0,
            len: 0,
            active: true,
            consumed: 0,
            underruns: 0,
            callback_frames: 256,
            refilling: true,
            peak: 0.0,
            gain: 1.0,
            spectrum: doslet_pcm::spectrum::Spectrum::new(48000),
            error: None,
            generation: 0,
            waker: None,
        };
        assert_eq!(queue.refill_nanos(48000), 30_000_000);
        queue.len = 1000;
        assert!(queue.refill_nanos(48000) > 0);
        queue.len = 1440;
        assert_eq!(queue.refill_nanos(48000), 0);
        queue.len = 1000;
        assert_eq!(queue.refill_nanos(48000), 0);
        queue.len = 700;
        assert!(queue.refill_nanos(48000) > 0);
        queue.callback_frames = 4096;
        queue.len = 0;
        assert_eq!(queue.refill_nanos(48000), 80_000_000);
        queue.active = false;
        assert_eq!(queue.refill_nanos(48000), 0);
    }

    #[test]
    fn spectrum_tracks_consumed_frames_and_fades_on_output_underrun() {
        let mut queue = Queue {
            rates: vec![48000; CAPACITY].into_boxed_slice(),
            sample_rate: 48000,
            frames: vec![[0.0; 2]; CAPACITY].into_boxed_slice(),
            head: 0,
            len: 1024,
            active: true,
            consumed: 0,
            underruns: 0,
            callback_frames: 0,
            refilling: true,
            peak: 0.0,
            gain: 1.0,
            spectrum: doslet_pcm::spectrum::Spectrum::new(48000),
            error: None,
            generation: 0,
            waker: None,
        };
        for (i, frame) in queue.frames[..1024].iter_mut().enumerate() {
            *frame = [(i as f32 * std::f32::consts::TAU * 2000.0 / 48000.0).sin(); 2];
        }
        // Queuing samples must not animate the display ahead of playback.
        assert_eq!(queue.spectrum.levels(), [0.0; doslet_pcm::spectrum::BANDS]);
        for _ in 0..1024 {
            queue.next();
        }
        assert!(queue.spectrum.levels()[8] > 0.8);
        for _ in 0..48000 {
            assert_eq!(queue.next(), [0.0; 2]);
        }
        assert!(queue.spectrum.levels().iter().all(|level| *level < 0.01));
    }
}
