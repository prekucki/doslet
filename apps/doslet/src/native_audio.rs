//! Desktop synthesis lives on one owner, apart from both the PC and CPAL.
//! Headless/no-device runs retain the same renderer synchronously.
mod source;
mod worker;
use crate::native_synth::Timeline;
use doslet_midi_routing::Mode;
pub use worker::Worker;

#[derive(Clone)]
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
    pub midi: Option<Timeline>,
    source: Source,
    rate: u32,
    pub nanos: u64,
    pub mt32: Option<doslet_mt32::Timeline>,
    pub mixer: doslet_pcm::Mixer,
    pub samples: [[f32; 2]; 512],
    sources: Option<[source::Timeline; 2]>,
    external: bool,
}
impl Renderer {
    fn new(config: &Config) -> Result<Self, String> {
        let mut mixer = doslet_pcm::Mixer::new(config.rate).map_err(|e| e.to_string())?;
        mixer
            .reset(config.rate, config.nanos)
            .map_err(|e| e.to_string())?;
        let mut renderer = Self {
            midi: None,
            mt32: None,
            source: config.source.clone(),
            rate: config.rate,
            nanos: config.nanos,
            mixer,
            samples: [[0.0; 2]; 512],
            sources: None,
            external: matches!(config.source, Source::External),
        };
        if !config.replay.is_empty() {
            renderer.activate_midi(config.nanos)?;
            for &byte in &config.replay {
                if let Some(synth) = &mut renderer.mt32 {
                    synth.restore_byte(byte).map_err(|e| e.to_string())?;
                } else if let Some(synth) = &mut renderer.midi {
                    synth.restore_byte(byte)?;
                }
            }
        }
        Ok(renderer)
    }
    /// Runs on the host synthesis owner, never in the PC execution batch.
    fn activate_midi(&mut self, nanos: u64) -> Result<(), String> {
        if self.external || self.midi.is_some() || self.mt32.is_some() {
            return Ok(());
        }
        if let Source::Mt32 { directory, mode } = &self.source {
            let (control, pcm) = crate::native_roms::read(directory)?;
            let mut synth =
                doslet_mt32::Timeline::with_gm(&control, &pcm, self.rate, *mode == Mode::Gm)
                    .map_err(|e| e.to_string())?;
            synth.rebase(nanos);
            self.mt32 = Some(synth);
        } else {
            let mut synth = if matches!(self.source, Source::Apple) {
                Timeline::apple(self.rate)?
            } else {
                Timeline::new(self.rate)?
            };
            debug_assert_eq!(synth.sample_rate(), self.rate);
            synth.skip_until(nanos)?;
            self.midi = Some(synth);
        }
        Ok(())
    }
    fn render(&mut self, nanos: u64) -> Result<usize, String> {
        if self.external || (self.midi.is_none() && self.mt32.is_none()) {
            self.mixer
                .render_until(nanos, &mut self.samples)
                .map_err(|e| e.to_string())
        } else {
            let count = if let Some(synth) = &mut self.mt32 {
                synth
                    .render_until(nanos, &mut self.samples)
                    .map_err(|e| e.to_string())?
            } else {
                self.midi
                    .as_mut()
                    .expect("active MIDI renderer")
                    .render_until(nanos, &mut self.samples)?
            };
            self.mixer.mix(&mut self.samples[..count]);
            Ok(count)
        }
    }
    fn enable_sources(&mut self, nanos: u64) {
        self.sources = Some([
            source::Timeline::new(22050, nanos),
            source::Timeline::new(48000, nanos),
        ]);
    }
    fn render_midi(&mut self, nanos: u64) -> Result<usize, String> {
        if self.external || (self.mt32.is_none() && self.midi.is_none()) {
            return Ok(0);
        }
        let count = if let Some(synth) = &mut self.mt32 {
            synth
                .render_until(nanos, &mut self.samples)
                .map_err(|e| e.to_string())?
        } else {
            self.midi
                .as_mut()
                .expect("active MIDI renderer")
                .render_until(nanos, &mut self.samples)?
        };
        Ok(count)
    }
    fn push_midi(&mut self, nanos: u64, bytes: &[u8]) -> Result<(), String> {
        if !bytes.is_empty() {
            self.activate_midi(nanos)?;
        }
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
            } else if let Some(synth) = &mut self.midi {
                synth.push_byte(byte)?;
            }
        }
        Ok(())
    }
    fn stats(&self) -> Stats {
        Stats {
            notes: self.mt32.as_ref().map_or_else(
                || self.midi.as_ref().map_or(0, |s| s.note_events()),
                |s| s.note_events(),
            ),
            sysex: self.mt32.as_ref().map_or(0, |s| s.sysex_messages()),
            received: self.sources.as_ref().map_or_else(
                || self.mixer.received(),
                |s| s.iter().map(|s| s.received).sum(),
            ),
            nonzero: self.sources.as_ref().map_or_else(
                || self.mixer.nonzero(),
                |s| s.iter().map(|s| s.nonzero).sum(),
            ),
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
    pub fn receive_source(
        &mut self,
        fm: bool,
        frames: &[doslet_pcm::SourceFrame],
    ) -> Result<(), String> {
        match self {
            Self::Threaded(w) => w.receive_source(fm, frames),
            Self::Inline(_) => Err("Native source capture requires a host output".into()),
        }
    }
    pub fn render_until(&mut self, nanos: u64, external: bool) -> Result<(), String> {
        match self {
            Self::Inline(r) => {
                r.external = external;
                while r.render(nanos)? != 0 {}
                r.nanos = nanos;
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
            Self::Inline(r) => r.rate,
            Self::Threaded(w) => w.rate,
        }
    }
    pub fn is_mt32(&self) -> bool {
        match self {
            Self::Inline(r) => matches!(r.source, Source::Mt32 { .. }) || r.mt32.is_some(),
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

#[cfg(test)]
mod lazy_tests {
    use super::*;
    fn config(source: Source) -> Config {
        Config {
            source,
            rate: 48000,
            nanos: 2_000_000_000,
            replay: Vec::new(),
        }
    }
    #[test]
    fn pcm_and_silence_do_not_construct_a_midi_synth() {
        let mut r = Renderer::new(&config(Source::Rust)).unwrap();
        while r.render(2_010_000_000).unwrap() != 0 {}
        r.push_midi(2_010_000_000, &[]).unwrap();
        assert!(r.midi.is_none());
        assert!(r.mt32.is_none());
        r.push_midi(2_010_000_000, &[0x90, 60, 100]).unwrap();
        assert!(r.midi.is_some());
        assert!(r.mt32.is_none());
        assert_eq!(r.stats().notes, 1);
        let mut peak = 0.0_f32;
        loop {
            let count = r.render(2_030_000_000).unwrap();
            if count == 0 {
                break;
            }
            peak = r.samples[..count]
                .iter()
                .flatten()
                .fold(peak, |p, v| p.max(v.abs()));
        }
        assert!(peak > 0.0001);
    }
    #[test]
    fn roms_are_only_opened_when_midi_arrives_and_no_fallback_is_created() {
        let mut r = Renderer::new(&config(Source::Mt32 {
            directory: "/missing/doslet-lazy-roms".into(),
            mode: Mode::Mt32,
        }))
        .unwrap();
        while r.render_midi(2_010_000_000).unwrap() != 0 {}
        assert!(r.midi.is_none());
        assert!(r.mt32.is_none());
        assert!(r.push_midi(2_010_000_000, &[0x90, 60, 100]).is_err());
        assert!(r.midi.is_none());
        assert!(r.mt32.is_none());
    }
    #[test]
    #[ignore = "requires local DOSBox CM-32L ROMs; no host output is opened"]
    fn local_mt32_remains_dormant_until_the_first_midi_message() {
        let directory = crate::native_roms::detect().expect("local supported ROMs");
        let mut r = Renderer::new(&config(Source::Mt32 {
            directory,
            mode: Mode::Mt32,
        }))
        .unwrap();
        while r.render(2_010_000_000).unwrap() != 0 {}
        assert!(r.mt32.is_none());
        assert!(r.midi.is_none());
        r.push_midi(2_010_000_000, &[0x91, 60, 100]).unwrap();
        assert!(r.mt32.is_some());
        assert!(r.midi.is_none());
        assert_eq!(r.stats().notes, 1);
        let mut count = 0;
        loop {
            let n = r.render(2_030_000_000).unwrap();
            count += n;
            if n == 0 {
                break;
            }
        }
        assert_eq!(count, 960);
    }
    #[test]
    fn restored_notes_activate_immediately_but_external_output_never_synthesizes() {
        let mut c = config(Source::Rust);
        c.replay = vec![0x90, 60, 100];
        let r = Renderer::new(&c).unwrap();
        assert_eq!(r.stats().notes, 1);
        let mut r = Renderer::new(&config(Source::External)).unwrap();
        r.push_midi(2_010_000_000, &[0x90, 60, 100]).unwrap();
        assert!(r.midi.is_none());
        assert!(r.mt32.is_none());
    }

    #[test]
    fn lazy_midi_preserves_first_program_controllers_and_all_channels() {
        let c = config(Source::Rust);
        let mut lazy = Renderer::new(&c).unwrap();
        let mut eager = Renderer::new(&c).unwrap();
        eager.activate_midi(c.nanos).unwrap();
        // Include the first setup message and split wire messages across calls.
        for channel in 0..16 {
            for byte in [
                0xC0 | channel,
                48,
                0xB0 | channel,
                7,
                90,
                0xB0 | channel,
                11,
                100,
                0x90 | channel,
                60 + channel,
                100,
            ] {
                lazy.push_midi(c.nanos, &[byte]).unwrap();
                eager.push_midi(c.nanos, &[byte]).unwrap();
            }
        }
        assert_eq!(lazy.stats().notes, 16);
        loop {
            let n = lazy.render(c.nanos + 20_000_000).unwrap();
            assert_eq!(n, eager.render(c.nanos + 20_000_000).unwrap());
            assert_eq!(&lazy.samples[..n], &eager.samples[..n]);
            if n == 0 {
                break;
            }
        }
    }
}
