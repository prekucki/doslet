//! Fixed-buffer analysis of the final output PCM, independent of guest timing.

pub const BANDS: usize = 16;
const SIZE: usize = 1024;
const EDGES: [f32; BANDS + 1] = [
    40.0, 63.0, 100.0, 160.0, 250.0, 400.0, 630.0, 1000.0, 1600.0, 2500.0, 3500.0, 5000.0, 7000.0,
    10000.0, 13000.0, 16000.0, 20000.0,
];

/// Packed stereo FFT: real input is left, imaginary input is right. Channels
/// are separated after transformation so opposite-phase stereo cannot cancel.
pub struct Spectrum {
    samples: [[f32; 2]; SIZE],
    window: [f32; SIZE],
    twiddle: [[f32; 2]; SIZE / 2],
    bins: [(usize, usize); BANDS],
    levels: [f32; BANDS],
    position: usize,
    decay: f32,
}
impl Spectrum {
    pub fn new(rate: u32) -> Self {
        assert!(rate > 0);
        let window = core::array::from_fn(|i| {
            0.5 - 0.5 * libm::cosf(core::f32::consts::TAU * i as f32 / SIZE as f32)
        });
        let twiddle = core::array::from_fn(|i| {
            let angle = -core::f32::consts::TAU * i as f32 / SIZE as f32;
            [libm::cosf(angle), libm::sinf(angle)]
        });
        let bins = core::array::from_fn(|band| {
            let bin = |frequency| libm::ceilf(frequency * SIZE as f32 / rate as f32) as usize;
            (
                bin(EDGES[band]).clamp(1, SIZE / 2),
                bin(EDGES[band + 1]).min(SIZE / 2),
            )
        });
        Self {
            samples: [[0.0; 2]; SIZE],
            window,
            twiddle,
            bins,
            levels: [0.0; BANDS],
            position: 0,
            decay: libm::expf(-(SIZE as f32 / rate as f32) / 0.16),
        }
    }
    pub fn reset(&mut self) {
        self.samples.fill([0.0; 2]);
        self.position = 0;
        self.levels.fill(0.0);
    }
    pub fn levels(&self) -> [f32; BANDS] {
        self.levels
    }
    /// Called for samples actually sent to the host, including underrun silence.
    /// No allocation, locks, guest buffers or unbounded history.
    pub fn push(&mut self, frame: [f32; 2]) {
        let weight = self.window[self.position];
        self.samples[self.position] = frame.map(|v| {
            if v.is_finite() {
                v.clamp(-1.0, 1.0) * weight
            } else {
                0.0
            }
        });
        self.position += 1;
        if self.position == SIZE {
            self.analyze();
            self.position = 0;
        }
    }
    fn analyze(&mut self) {
        for i in 0..SIZE {
            let reversed = i.reverse_bits() >> (usize::BITS - SIZE.ilog2());
            if reversed > i {
                self.samples.swap(i, reversed);
            }
        }
        let mut width = 2;
        while width <= SIZE {
            for start in (0..SIZE).step_by(width) {
                for offset in 0..width / 2 {
                    let [wr, wi] = self.twiddle[offset * SIZE / width];
                    let [br, bi] = self.samples[start + offset + width / 2];
                    let re = wr * br - wi * bi;
                    let im = wr * bi + wi * br;
                    let [ar, ai] = self.samples[start + offset];
                    self.samples[start + offset] = [ar + re, ai + im];
                    self.samples[start + offset + width / 2] = [ar - re, ai - im];
                }
            }
            width *= 2;
        }
        for (band, &(start, end)) in self.bins.iter().enumerate() {
            let mut power = 0.0_f32;
            for bin in start..end {
                let [ar, ai] = self.samples[bin];
                let [br, bi] = self.samples[SIZE - bin];
                let left = ((ar + br) * (ar + br) + (ai - bi) * (ai - bi)) * 0.25;
                let right = ((ai + bi) * (ai + bi) + (br - ar) * (br - ar)) * 0.25;
                power = power.max(left.max(right));
            }
            let amplitude = libm::sqrtf(power) * (4.0 / SIZE as f32);
            let level = ((20.0 * libm::log10f(amplitude.max(1e-9)) + 60.0) / 60.0).clamp(0.0, 1.0);
            self.levels[band] = level.max(self.levels[band] * self.decay);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tones_land_in_their_band_including_right_only_and_opposite_phase_audio() {
        for rate in [22050, 44100, 48000, 96000] {
            for (frequency, band) in [(300.0, 4), (2000.0, 8), (6000.0, 11)] {
                for routing in 0..3 {
                    let mut spectrum = Spectrum::new(rate);
                    for i in 0..SIZE * 3 {
                        let sample = 0.5
                            * libm::sinf(
                                core::f32::consts::TAU * frequency * i as f32 / rate as f32,
                            );
                        spectrum.push(match routing {
                            0 => [sample, 0.0],
                            1 => [0.0, sample],
                            _ => [sample, -sample],
                        });
                    }
                    let levels = spectrum.levels();
                    let strongest = levels
                        .iter()
                        .enumerate()
                        .max_by(|a, b| a.1.total_cmp(b.1))
                        .unwrap()
                        .0;
                    assert_eq!(
                        strongest, band,
                        "rate={rate} frequency={frequency} routing={routing}"
                    );
                }
            }
        }
    }
    #[test]
    fn silence_decays_and_reset_clears_incomplete_windows() {
        let mut spectrum = Spectrum::new(48000);
        for i in 0..SIZE * 2 {
            spectrum.push([libm::sinf(core::f32::consts::TAU * 2000.0 * i as f32 / 48000.0); 2]);
        }
        assert!(spectrum.levels()[8] > 0.8);
        for _ in 0..48000 {
            spectrum.push([0.0; 2]);
        }
        assert!(spectrum.levels().iter().all(|v| *v < 0.01));
        spectrum.push([1.0; 2]);
        spectrum.reset();
        assert_eq!(spectrum.levels(), [0.0; BANDS]);
        for _ in 0..SIZE {
            spectrum.push([f32::NAN, f32::INFINITY]);
        }
        assert_eq!(spectrum.levels(), [0.0; BANDS]);
    }
}
