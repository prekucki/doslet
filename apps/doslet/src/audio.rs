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
    peak: f32,
    error: Option<String>,
}
impl Queue {
    fn next(&mut self) -> [f32; 2] {
        if !self.active {
            return [0.0; 2];
        }
        if self.len == 0 {
            self.underruns += 1;
            return [0.0; 2];
        }
        let frame = self.frames[self.head];
        self.head = (self.head + 1) % CAPACITY;
        self.len -= 1;
        self.consumed += 1;
        self.peak = self.peak.max(frame[0].abs()).max(frame[1].abs());
        frame
    }
}
pub struct Output {
    _stream: cpal::Stream,
    queue: Arc<Mutex<Queue>>,
    pub rate: u32,
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
            peak: 0.0,
            error: None,
        }));
        let config = supported.config();
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
            queue,
            rate,
        })
    }
    pub fn available(&self) -> usize {
        let q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        CAPACITY - q.len
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
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).active = active;
    }
    pub fn clear(&self) {
        let mut q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.head = 0;
        q.len = 0;
        q.consumed = 0;
        q.underruns = 0;
        q.peak = 0.0;
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
