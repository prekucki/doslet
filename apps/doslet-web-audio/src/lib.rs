//! Small WASM audio module, independent of iced, the PC and browser APIs.
use doslet_midi_synth::stream::Stream;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;
const BLOCK: usize = 2048;
const MIDI: usize = 1024;

#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
pub struct AudioEngine {
    stream: Stream,
    pcm: doslet_pcm::Mixer,
    pcm_wire: [f64; 128 * 4],
    midi: [u8; MIDI],
    stereo: [[f32; 2]; BLOCK],
    left: [f32; BLOCK],
    right: [f32; BLOCK],
    peak: f32,
    spectrum: doslet_pcm::spectrum::Spectrum,
    frames: u64,
}
#[cfg_attr(target_arch = "wasm32", wasm_bindgen)]
impl AudioEngine {
    #[cfg_attr(target_arch = "wasm32", wasm_bindgen(constructor))]
    pub fn new(rate: u32) -> Self {
        Self {
            stream: Stream::new(rate).expect("validated audio rate"),
            pcm: doslet_pcm::Mixer::new(rate).expect("validated audio rate"),
            pcm_wire: [0.0; 128 * 4],
            midi: [0; MIDI],
            stereo: [[0.0; 2]; BLOCK],
            left: [0.0; BLOCK],
            right: [0.0; BLOCK],
            peak: 0.0,
            spectrum: doslet_pcm::spectrum::Spectrum::new(rate),
            frames: 0,
        }
    }
    // Pointers expose owned, fixed host buffers. No guest memory and no unsafe
    // Rust; JS creates views only after all setup allocations have completed.
    pub fn midi_ptr(&self) -> usize {
        self.midi.as_ptr() as usize
    }
    pub fn pcm_ptr(&self) -> usize {
        self.pcm_wire.as_ptr() as usize
    }
    pub fn pcm_frames(&self) -> f64 {
        self.pcm.received() as f64
    }
    pub fn pcm_nonzero(&self) -> f64 {
        self.pcm.nonzero() as f64
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
        self.pcm
            .reset(self.stream.sample_rate(), nanos as u64)
            .expect("existing sample rate");
        self.peak = 0.0;
        self.spectrum.reset();
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
        self.pcm.mix(&mut self.stereo[..rendered]);
        for (i, frame) in self.stereo[..count].iter().enumerate() {
            self.left[i] = frame[0];
            self.right[i] = frame[1];
            self.peak = self.peak.max(frame[0].abs()).max(frame[1].abs());
            self.spectrum.push(*frame);
        }
        self.frames += rendered as u64;
        0
    }
    pub fn receive_pcm(&mut self, count: usize) -> u32 {
        if count > 128 {
            return 4;
        }
        let mut frames = [doslet_pcm::EMPTY; 128];
        for (wire, frame) in self
            .pcm_wire
            .as_chunks::<4>()
            .0
            .iter()
            .zip(frames[..count].iter_mut())
        {
            if wire.iter().any(|v| !v.is_finite())
                || wire[0] < 0.0
                || wire[0] > 9_007_199_254_740_991.0
                || wire[1] < 1.0
                || wire[1] > f64::from(u32::MAX)
                || wire[2..].iter().any(|v| *v < -32768.0 || *v > 32767.0)
                || wire.iter().any(|v| *v != (*v as i64) as f64)
            {
                return 4;
            }
            *frame = doslet_pcm::Frame {
                nanos: wire[0] as u64,
                duration: wire[1] as u32,
                samples: [wire[2] as i16, wire[3] as i16],
            };
        }
        match self.pcm.receive(&frames[..count]) {
            Ok(()) => 0,
            Err(_) => 5,
        }
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
    pub fn spectrum_band(&self, band: usize) -> f32 {
        self.spectrum.levels().get(band).copied().unwrap_or(0.0)
    }
    pub fn clear_spectrum(&mut self) {
        self.spectrum.reset();
    }
    pub fn underruns(&self) -> f64 {
        self.stream.underruns() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn spectrum_measures_played_pcm_and_clears_on_pause_or_reset() {
        let mut engine = AudioEngine::new(48000);
        for batch in 0..16 {
            for (offset, frame) in engine
                .pcm_wire
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .enumerate()
            {
                let i = batch * 128 + offset;
                let sample =
                    ((i as f32 * 2000.0 * core::f32::consts::TAU / 48000.0).sin() * 16000.0) as i16;
                frame.copy_from_slice(&[
                    (i as u64 * 1_000_000_000 / 48000) as f64,
                    20834.0,
                    f64::from(sample),
                    -f64::from(sample),
                ]);
            }
            assert_eq!(engine.receive_pcm(128), 0);
        }
        assert_eq!(engine.receive(100_000_000.0, 0), 0);
        assert_eq!(engine.render(2048), 0);
        assert!(engine.spectrum_band(8) > 0.8);
        engine.clear_spectrum();
        assert_eq!(engine.spectrum.levels(), [0.0; doslet_pcm::spectrum::BANDS]);
        // Reset also removes a partially collected window.
        assert_eq!(engine.render(128), 0);
        engine.reset(0.0);
        assert_eq!(engine.spectrum.levels(), [0.0; doslet_pcm::spectrum::BANDS]);
    }

    #[test]
    fn pcm_only_worklet_renders_and_reset_underrun_do_not_replay_stale_audio() {
        let mut engine = AudioEngine::new(48000);
        engine.pcm_wire[..4].copy_from_slice(&[0.0, 1_000_000.0, 16000.0, -16000.0]);
        assert_eq!(engine.receive_pcm(1), 0);
        assert_eq!(engine.receive(1_000_000.0, 0), 0);
        assert_eq!(engine.render(48), 0);
        assert!(engine.left[..48].iter().all(|sample| *sample > 0.4));
        assert!(engine.right[..48].iter().all(|sample| *sample < -0.4));
        assert_eq!(engine.notes(), 0.0);
        assert_eq!(engine.pcm_nonzero(), 1.0);
        assert_eq!(engine.render(48), 0); // No committed guest time: hold cursor and silence.
        assert_eq!(engine.left[..48], [0.0; 48]);
        engine.reset(0.0);
        assert_eq!(engine.receive(1_000_000.0, 0), 0);
        assert_eq!(engine.render(48), 0);
        assert_eq!(engine.left[..48], [0.0; 48]);
    }
}
