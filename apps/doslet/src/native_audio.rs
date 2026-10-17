//! Desktop synthesis lives on one owner, apart from both the PC and CPAL.
//! Headless/no-device runs retain the same renderer synchronously.
mod worker;
use crate::native_synth::Timeline;
use doslet_midi_routing::Mode;
pub use worker::Worker;

pub enum Source {
    Rust,
    Apple,
    Mt32 {
        directory: std::path::PathBuf,
        mode: Mode,
    },
    External,
}
pub struct Config {
    pub source: Source,
    pub rate: u32,
    pub nanos: u64,
    /// Setup-only replay, never a live MIDI burst. May allocate during switches.
    pub replay: Vec<u8>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub notes: u64,
    pub sysex: u64,
    pub received: u64,
    pub nonzero: u64,
}

pub struct Renderer {
    pub midi: Timeline,
    pub mt32: Option<doslet_mt32::Timeline>,
    pub mixer: doslet_pcm::Mixer,
    pub samples: [[f32; 2]; 512],
    external: bool,
}
impl Renderer {
    fn new(config: &Config) -> Result<Self, String> {
        let mut midi = if matches!(config.source, Source::Apple) {
            Timeline::apple(config.rate)?
        } else {
            Timeline::new(config.rate)?
        };
        midi.skip_until(config.nanos)?;
        let mut mt32 = if let Source::Mt32 { directory, mode } = &config.source {
            let control =
                std::fs::read(directory.join("CM32L_CONTROL.ROM")).map_err(|e| e.to_string())?;
            let pcm = std::fs::read(directory.join("CM32L_PCM.ROM")).map_err(|e| e.to_string())?;
            let mut synth =
                doslet_mt32::Timeline::with_gm(&control, &pcm, config.rate, *mode == Mode::Gm)
                    .map_err(|e| e.to_string())?;
            synth.rebase(config.nanos);
            Some(synth)
        } else {
            None
        };
        for &byte in &config.replay {
            if let Some(synth) = &mut mt32 {
                synth.restore_byte(byte).map_err(|e| e.to_string())?;
            } else {
                midi.restore_byte(byte)?;
            }
        }
        let mut mixer = doslet_pcm::Mixer::new(config.rate).map_err(|e| e.to_string())?;
        mixer
            .reset(config.rate, config.nanos)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            midi,
            mt32,
            mixer,
            samples: [[0.0; 2]; 512],
            external: matches!(config.source, Source::External),
        })
    }
    fn render(&mut self, nanos: u64) -> Result<usize, String> {
        if self.external {
            self.mixer
                .render_until(nanos, &mut self.samples)
                .map_err(|e| e.to_string())
        } else {
            let count = if let Some(synth) = &mut self.mt32 {
                synth
                    .render_until(nanos, &mut self.samples)
                    .map_err(|e| e.to_string())?
            } else {
                self.midi.render_until(nanos, &mut self.samples)?
            };
            self.mixer.mix(&mut self.samples[..count]);
            Ok(count)
        }
    }
    fn push_midi(&mut self, nanos: u64, bytes: &[u8]) -> Result<(), String> {
        for &byte in bytes {
            if let Some(synth) = &mut self.mt32 {
                synth.push_byte(byte).map_err(|e| {
                    format!(
                        "{e} at guest {} ms (notes={}, SysEx={})",
                        nanos / 1_000_000,
                        synth.note_events(),
                        synth.sysex_messages()
                    )
                })?;
            } else if !self.external {
                self.midi.push_byte(byte)?;
            }
        }
        Ok(())
    }
    fn stats(&self) -> Stats {
        Stats {
            notes: self
                .mt32
                .as_ref()
                .map_or_else(|| self.midi.note_events(), |s| s.note_events()),
            sysex: self.mt32.as_ref().map_or(0, |s| s.sysex_messages()),
            received: self.mixer.received(),
            nonzero: self.mixer.nonzero(),
        }
    }
}

pub enum Engine {
    Inline(Box<Renderer>),
    Threaded(Worker),
}
impl Engine {
    pub fn new() -> Self {
        Self::Inline(Box::new(
            Renderer::new(&Config {
                source: Source::Rust,
                rate: 48000,
                nanos: 0,
                replay: Vec::new(),
            })
            .expect("valid default audio rate"),
        ))
    }
    /// Configuration is a control boundary: construct/replay on the synth owner
    /// and commit only on success. Never called for an ordinary execution batch.
    pub fn configure(
        &mut self,
        config: Config,
        output: Option<crate::audio::Writer>,
    ) -> Result<(), String> {
        match self {
            Self::Threaded(worker) => worker.configure(config),
            Self::Inline(_) => {
                *self = if let Some(output) = output {
                    Self::Threaded(Worker::new(config, output)?)
                } else {
                    Self::Inline(Box::new(Renderer::new(&config)?))
                };
                Ok(())
            }
        }
    }
    /// Reserve before advancing the PC, so saturation yields without dropping
    /// PCM/MIDI or waiting for synthesis. The reservation is reused until submit.
    pub fn ready(&mut self) -> Result<bool, String> {
        match self {
            Self::Inline(_) => Ok(true),
            Self::Threaded(w) => w.ready(),
        }
    }
    pub fn check(&self) -> Result<(), String> {
        match self {
            Self::Inline(_) => Ok(()),
            Self::Threaded(w) => w.check(),
        }
    }
    pub fn receive_pcm(&mut self, frames: &[doslet_pcm::Frame]) -> Result<(), String> {
        match self {
            Self::Inline(r) => r.mixer.receive(frames).map_err(|e| e.to_string()),
            Self::Threaded(w) => w.receive_pcm(frames),
        }
    }
    pub fn render_until(&mut self, nanos: u64, external: bool) -> Result<(), String> {
        match self {
            Self::Inline(r) => {
                r.external = external;
                while r.render(nanos)? != 0 {}
                Ok(())
            }
            Self::Threaded(w) => w.render_until(nanos),
        }
    }
    pub fn push_midi(&mut self, nanos: u64, bytes: &[u8]) -> Result<(), String> {
        match self {
            Self::Inline(r) => r.push_midi(nanos, bytes),
            Self::Threaded(w) => w.push_midi(bytes),
        }
    }
    pub fn submit(&mut self) -> Result<(), String> {
        match self {
            Self::Inline(_) => Ok(()),
            Self::Threaded(w) => w.submit(),
        }
    }
    pub fn pending_nanos(&self) -> u64 {
        match self {
            Self::Inline(_) => 0,
            Self::Threaded(w) => w.pending_nanos(),
        }
    }
    pub fn stats(&self) -> Stats {
        match self {
            Self::Inline(r) => r.stats(),
            Self::Threaded(w) => w.stats(),
        }
    }
    pub fn rate(&self) -> u32 {
        match self {
            Self::Inline(r) => r.midi.sample_rate(),
            Self::Threaded(w) => w.rate,
        }
    }
    pub fn is_mt32(&self) -> bool {
        match self {
            Self::Inline(r) => r.mt32.is_some(),
            Self::Threaded(w) => w.mt32,
        }
    }
    #[cfg(test)]
    pub fn inline(&self) -> &Renderer {
        match self {
            Self::Inline(r) => r,
            _ => panic!("expected no-device renderer"),
        }
    }
    #[cfg(test)]
    pub fn inline_mut(&mut self) -> &mut Renderer {
        match self {
            Self::Inline(r) => r,
            _ => panic!("expected no-device renderer"),
        }
    }
}
