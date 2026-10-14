//! Small WASM audio module, independent of iced, the PC and browser APIs.
use doslet_midi_synth::stream::Stream;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;
const BLOCK: usize = 2048;
const MIDI: usize = 1024;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct AudioEngine {
    stream: Stream,
    midi: [u8; MIDI],
    stereo: [[f32; 2]; BLOCK],
    left: [f32; BLOCK],
    right: [f32; BLOCK],
    peak: f32,
    frames: u64,
}
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl AudioEngine {
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new(rate: u32) -> Self {
        Self {
            stream: Stream::new(rate).expect("validated audio rate"),
            midi: [0; MIDI],
            stereo: [[0.0; 2]; BLOCK],
            left: [0.0; BLOCK],
            right: [0.0; BLOCK],
            peak: 0.0,
            frames: 0,
        }
    }
    // Pointers expose owned, fixed host buffers. No guest memory and no unsafe
    // Rust; JS creates views only after all setup allocations have completed.
    pub fn midi_ptr(&self) -> usize {
        self.midi.as_ptr() as usize
    }
    pub fn left_ptr(&self) -> usize {
        self.left.as_ptr() as usize
    }
    pub fn right_ptr(&self) -> usize {
        self.right.as_ptr() as usize
    }
    pub fn capacity(&self) -> usize {
        BLOCK
    }
    pub fn reset(&mut self, nanos: f64) {
        self.stream.reset(nanos as u64);
        self.peak = 0.0;
        self.frames = 0;
    }
    /// The caller fills the fixed MIDI buffer first. Numeric errors avoid
    /// strings, exceptions and wasm-bindgen allocations on the audio thread.
    pub fn receive(&mut self, nanos: f64, count: usize) -> u32 {
        if count > MIDI {
            return 1;
        }
        match self.stream.receive(nanos as u64, &self.midi[..count]) {
            Ok(()) => 0,
            Err(_) => 2,
        }
    }
    pub fn render(&mut self, count: usize) -> u32 {
        if count > BLOCK {
            return 1;
        }
        let rendered = match self.stream.render(&mut self.stereo[..count]) {
            Ok(n) => n,
            Err(_) => return 3,
        };
        for (i, frame) in self.stereo[..count].iter().enumerate() {
            self.left[i] = frame[0];
            self.right[i] = frame[1];
            self.peak = self.peak.max(frame[0].abs()).max(frame[1].abs());
        }
        self.frames += rendered as u64;
        0
    }
    pub fn nanos(&self) -> f64 {
        self.stream.nanos() as f64
    }
    pub fn notes(&self) -> f64 {
        self.stream.notes() as f64
    }
    pub fn frames(&self) -> f64 {
        self.frames as f64
    }
    pub fn peak(&self) -> f32 {
        self.peak
    }
    pub fn underruns(&self) -> f64 {
        self.stream.underruns() as f64
    }
}
