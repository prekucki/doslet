//! Host output owns only audio frames, never the emulated machine.
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};

const CAPACITY: usize = 16384;
struct Queue {
    frames: Box<[[f32; 2]]>,
    head: usize,
    len: usize,
    active: bool,
    consumed: u64,
    underruns: u64,
    callback_frames: usize,
    refilling: bool,
    peak: f32,
    spectrum: doslet_pcm::spectrum::Spectrum,
    error: Option<String>,
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
        let frame = self.frames[self.head];
        self.head = (self.head + 1) % CAPACITY;
        self.len -= 1;
        self.consumed += 1;
        self.peak = self.peak.max(frame[0].abs()).max(frame[1].abs());
        self.spectrum.push(frame);
        frame
    }
}
pub struct Output {
    _stream: cpal::Stream,
    writer: Writer,
}
/// The synthesis owner shares frames and playback telemetry, never the device
/// handle or a synthesizer, with CPAL. Stream creation/disposal stays on its owner.
#[derive(Clone)]
pub struct Writer {
    queue: Arc<Mutex<Queue>>,
    pub rate: u32,
}
impl std::ops::Deref for Output {
    type Target = Writer;
    fn deref(&self) -> &Writer {
        &self.writer
    }
}
impl Output {
    pub fn new() -> Result<Self, String> {
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
            head: 0,
            len: 0,
            active: true,
            consumed: 0,
            underruns: 0,
            callback_frames: 0,
            refilling: true,
            peak: 0.0,
            spectrum: doslet_pcm::spectrum::Spectrum::new(rate),
            error: None,
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
            _stream: stream,
            writer: Writer { queue, rate },
        })
    }
    pub fn writer(&self) -> Writer {
        self.writer.clone()
    }
}
impl Writer {
    #[cfg(test)]
    pub fn mock(rate: u32) -> Self {
        Self {
            queue: Arc::new(Mutex::new(Queue {
                frames: vec![[0.0; 2]; CAPACITY].into_boxed_slice(),
                head: 0,
                len: 0,
                active: true,
                consumed: 0,
                underruns: 0,
                callback_frames: 256,
                refilling: true,
                peak: 0.0,
                spectrum: doslet_pcm::spectrum::Spectrum::new(rate),
                error: None,
            })),
            rate,
        }
    }
    #[cfg(test)]
    pub fn consume(&self, count: usize) -> Vec<[f32; 2]> {
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        (0..count).map(|_| queue.next()).collect()
    }
    pub fn check(&self) -> Result<(), String> {
        let q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.error.clone().map_or(Ok(()), Err)
    }
    pub fn available(&self) -> usize {
        let q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        CAPACITY - q.len
    }
    /// Refill below two callback blocks, with margin, until the high watermark.
    /// Bounded by the reserved queue and 80 ms; no audio-device reconfiguration.
    pub fn refill_nanos(&self) -> u64 {
        self.queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .refill_nanos(self.rate)
    }

    pub fn push(&self, frames: &[[f32; 2]]) -> Result<(), String> {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(error) = &q.error {
            return Err(error.clone());
        }
        if frames.len() > CAPACITY - q.len {
            return Err("Audio output queue full".into());
        }
        for frame in frames {
            let index = (q.head + q.len) % CAPACITY;
            q.frames[index] = *frame;
            q.len += 1;
        }
        Ok(())
    }
    pub fn set_active(&self, active: bool) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.active = active;
        if !active {
            q.spectrum.reset();
        }
    }
    pub fn clear(&self) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.head = 0;
        q.len = 0;
        q.consumed = 0;
        q.underruns = 0;
        q.refilling = true;
        q.peak = 0.0;
        q.spectrum.reset();
    }
    pub fn spectrum(&self) -> [f32; doslet_pcm::spectrum::BANDS] {
        self.queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .spectrum
            .levels()
    }
    pub fn stats(&self) -> (u64, u64, f32) {
        let q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        (q.consumed, q.underruns, q.peak)
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
    fn refill_watermarks_keep_reserve_without_chattering_or_unbounded_lead() {
        let mut queue = Queue {
            frames: vec![[0.0; 2]; CAPACITY].into_boxed_slice(),
            head: 0,
            len: 0,
            active: true,
            consumed: 0,
            underruns: 0,
            callback_frames: 256,
            refilling: true,
            peak: 0.0,
            spectrum: doslet_pcm::spectrum::Spectrum::new(48000),
            error: None,
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
            frames: vec![[0.0; 2]; CAPACITY].into_boxed_slice(),
            head: 0,
            len: 1024,
            active: true,
            consumed: 0,
            underruns: 0,
            callback_frames: 0,
            refilling: true,
            peak: 0.0,
            spectrum: doslet_pcm::spectrum::Spectrum::new(48000),
            error: None,
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
