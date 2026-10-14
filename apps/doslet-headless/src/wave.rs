use doslet_midi_synth::Timeline;
use std::{
    fs::File,
    io::{BufWriter, Seek, SeekFrom, Write},
    path::Path,
};

pub struct Wave {
    pub timeline: Timeline,
    output: BufWriter<File>,
    frames: u64,
    pub peak: f32,
    pub nonzero: u64,
    samples: [[f32; 2]; 256],
}
impl Wave {
    pub fn new(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let mut output = BufWriter::new(File::create(path)?);
        output.write_all(&[0; 44])?;
        Ok(Self {
            timeline: Timeline::new(48000)?,
            output,
            frames: 0,
            peak: 0.0,
            nonzero: 0,
            samples: [[0.0; 2]; 256],
        })
    }
    pub fn advance(&mut self, nanos: u64) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            let count = self.timeline.render_until(nanos, &mut self.samples)?;
            if count == 0 {
                break;
            }
            self.frames += count as u64;
            for frame in &self.samples[..count] {
                for sample in frame {
                    self.peak = self.peak.max(sample.abs());
                    let value = (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16;
                    self.nonzero += u64::from(value != 0);
                    self.output.write_all(&value.to_le_bytes())?;
                }
            }
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<(), Box<dyn std::error::Error>> {
        let size = u32::try_from(self.frames * 4)?;
        let riff_size = size.checked_add(36).ok_or("WAVE file exceeds RIFF limit")?;
        self.output.flush()?;
        self.output.seek(SeekFrom::Start(0))?;
        self.output.write_all(b"RIFF")?;
        self.output.write_all(&riff_size.to_le_bytes())?;
        self.output.write_all(b"WAVEfmt ")?;
        self.output.write_all(&16u32.to_le_bytes())?;
        self.output.write_all(&1u16.to_le_bytes())?;
        self.output.write_all(&2u16.to_le_bytes())?;
        self.output.write_all(&48000u32.to_le_bytes())?;
        self.output.write_all(&192000u32.to_le_bytes())?;
        self.output.write_all(&4u16.to_le_bytes())?;
        self.output.write_all(&16u16.to_le_bytes())?;
        self.output.write_all(b"data")?;
        self.output.write_all(&size.to_le_bytes())?;
        self.output.flush()?;
        Ok(())
    }
}
