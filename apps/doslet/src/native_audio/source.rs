//! Preserve hardware sample counts/rates; fill guest-time gaps with silence.
//! No interpolation, filtering or sample-rate conversion is performed here.
use doslet_pcm::SourceFrame;

pub(super) struct Timeline {
    frames: Box<[SourceFrame]>,
    head: usize,
    len: usize,
    anchor: u64,
    cursor: u64,
    rate: u32,
    last: Option<u64>,
    pub received: u64,
    pub nonzero: u64,
}

impl Timeline {
    pub fn new(rate: u32, nanos: u64) -> Self {
        Self {
            frames: vec![SourceFrame::default(); 16384].into_boxed_slice(),
            head: 0,
            len: 0,
            anchor: nanos,
            cursor: 0,
            rate,
            last: None,
            received: 0,
            nonzero: 0,
        }
    }
    pub fn receive(&mut self, frames: &[SourceFrame]) -> Result<(), String> {
        if frames.len() > self.frames.len() - self.len {
            return Err("Native PCM input queue full".into());
        }
        let mut last = self.last;
        for frame in frames {
            if !(1000..=192000).contains(&frame.rate) || last.is_some_and(|t| frame.nanos < t) {
                return Err("Invalid native PCM rate or timestamp".into());
            }
            last = Some(frame.nanos);
        }
        for &frame in frames {
            self.frames[(self.head + self.len) % self.frames.len()] = frame;
            self.len += 1;
            self.received += 1;
            self.nonzero += u64::from(frame.samples != [0; 2]);
        }
        self.last = last;
        Ok(())
    }
    pub fn render(&mut self, until: u64, output: &mut [[f32; 2]]) -> (usize, u32) {
        let mut count = 0;
        while count < output.len() {
            let time = self.anchor
                + (u128::from(self.cursor) * 1_000_000_000 / u128::from(self.rate)) as u64;
            if time >= until {
                break;
            }
            let front = (self.len != 0).then(|| self.frames[self.head]);
            let due = front.filter(|f| {
                f.nanos < until
                    && f.nanos <= time.saturating_add(500_000_000 / u64::from(self.rate))
            });
            if let Some(frame) = due {
                if frame.rate != self.rate {
                    if count != 0 {
                        break;
                    }
                    self.rate = frame.rate;
                    self.anchor = frame.nanos;
                    self.cursor = 0;
                }
                output[count] = frame.samples.map(|s| f32::from(s) / 32768.0);
                self.head = (self.head + 1) % self.frames.len();
                self.len -= 1;
            } else {
                output[count] = [0.0; 2];
            }
            self.cursor += 1;
            count += 1;
        }
        (count, self.rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_rates_preserve_every_sample_across_render_partitions() {
        for rate in [11025, 22050, 32000, 44100, 48000, 49716, 96000] {
            let mut timeline = Timeline::new(rate, 0);
            let frames: Vec<_> = (0..1000_u64)
                .map(|i| SourceFrame {
                    nanos: i * 1_000_000_000 / u64::from(rate),
                    rate,
                    samples: [i as i16 + 1, -(i as i16) - 1],
                })
                .collect();
            timeline.receive(&frames).unwrap();
            let mut actual = Vec::new();
            let mut block = [[0.0; 2]; 137];
            loop {
                let (count, actual_rate) =
                    timeline.render(1000 * 1_000_000_000 / u64::from(rate), &mut block);
                assert_eq!(actual_rate, rate);
                if count == 0 {
                    break;
                }
                actual.extend_from_slice(&block[..count]);
            }
            assert_eq!(
                actual,
                frames
                    .iter()
                    .map(|f| f.samples.map(|v| f32::from(v) / 32768.0))
                    .collect::<Vec<_>>()
            );
        }
    }
    #[test]
    fn format_changes_split_blocks_without_resampling_and_gaps_are_silent() {
        let mut timeline = Timeline::new(10000, 0);
        timeline
            .receive(&[
                SourceFrame {
                    nanos: 0,
                    rate: 10000,
                    samples: [1000; 2],
                },
                SourceFrame {
                    nanos: 100000,
                    rate: 20000,
                    samples: [2000; 2],
                },
                SourceFrame {
                    nanos: 150000,
                    rate: 20000,
                    samples: [3000; 2],
                },
                SourceFrame {
                    nanos: 300000,
                    rate: 20000,
                    samples: [4000; 2],
                },
            ])
            .unwrap();
        let mut block = [[0.0; 2]; 512];
        assert_eq!(timeline.render(350000, &mut block), (1, 10000));
        assert_eq!(block[0], [1000.0 / 32768.0; 2]);
        assert_eq!(timeline.render(350000, &mut block), (5, 20000));
        assert_eq!(
            &block[..5],
            &[
                [2000.0 / 32768.0; 2],
                [3000.0 / 32768.0; 2],
                [0.0; 2],
                [0.0; 2],
                [4000.0 / 32768.0; 2]
            ]
        );
    }
}
