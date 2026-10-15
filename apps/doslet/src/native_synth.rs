//! Sample-rendering native outputs share the guest timeline and CPAL mixer.
pub const APPLE_LABEL: &str = "Apple DLS synth";

pub fn choices() -> Vec<String> {
    let choices = vec!["Rust synth".into()];
    #[cfg(target_os = "macos")]
    let choices = {
        let mut choices = choices;
        choices.push(APPLE_LABEL.into());
        choices
    };
    choices
}

pub enum Timeline {
    Rust(Box<doslet_midi_synth::Timeline>),
    #[cfg(target_os = "macos")]
    Apple(Box<doslet_apple_midi::Timeline>),
}

impl Timeline {
    pub fn new(rate: u32) -> Result<Self, String> {
        doslet_midi_synth::Timeline::new(rate)
            .map(|s| Self::Rust(Box::new(s)))
            .map_err(|e| e.to_string())
    }
    pub fn apple(rate: u32) -> Result<Self, String> {
        #[cfg(target_os = "macos")]
        {
            doslet_apple_midi::Timeline::new(rate).map(|s| Self::Apple(Box::new(s)))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = rate;
            Err("Apple MIDI output is available only on native macOS".into())
        }
    }
    pub fn sample_rate(&self) -> u32 {
        match self {
            Self::Rust(s) => s.sample_rate(),
            #[cfg(target_os = "macos")]
            Self::Apple(s) => s.sample_rate(),
        }
    }
    pub fn note_events(&self) -> u64 {
        match self {
            Self::Rust(s) => s.note_events(),
            #[cfg(target_os = "macos")]
            Self::Apple(s) => s.note_events(),
        }
    }
    pub fn skip_until(&mut self, nanos: u64) -> Result<(), String> {
        match self {
            Self::Rust(s) => s.skip_until(nanos).map_err(|e| e.to_string()),
            #[cfg(target_os = "macos")]
            Self::Apple(s) => s.skip_until(nanos),
        }
    }
    pub fn push_byte(&mut self, byte: u8) -> Result<(), String> {
        match self {
            Self::Rust(s) => s.push_byte(byte).map_err(|e| e.to_string()),
            #[cfg(target_os = "macos")]
            Self::Apple(s) => s.push_byte(byte),
        }
    }
    pub fn restore_byte(&mut self, byte: u8) -> Result<(), String> {
        match self {
            Self::Rust(s) => s.push_byte(byte).map_err(|e| e.to_string()),
            #[cfg(target_os = "macos")]
            Self::Apple(s) => s.restore_byte(byte),
        }
    }
    pub fn render_until(&mut self, nanos: u64, samples: &mut [[f32; 2]]) -> Result<usize, String> {
        match self {
            Self::Rust(s) => s.render_until(nanos, samples).map_err(|e| e.to_string()),
            #[cfg(target_os = "macos")]
            Self::Apple(s) => s.render_until(nanos, samples),
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use doslet_midi_routing::{Mode, Routing};

    #[test]
    fn apple_output_recovers_expression_after_a_translated_mt32_reset() {
        fn send(router: &mut Routing, synth: &mut Timeline, bytes: &[u8]) {
            for &byte in bytes {
                router
                    .push(byte, Mode::Gm, &mut |message| {
                        for &byte in message {
                            synth.push_byte(byte).unwrap();
                        }
                    })
                    .unwrap();
            }
        }
        fn peak(synth: &mut Timeline, until: u64) -> f32 {
            let mut samples = [[0.0; 2]; 512];
            let mut peak = 0.0_f32;
            loop {
                let count = synth.render_until(until, &mut samples).unwrap();
                if count == 0 {
                    return peak;
                }
                for sample in samples[..count].iter().flatten() {
                    peak = peak.max(sample.abs());
                }
            }
        }
        let mut router = Routing::new(Mode::Mt32);
        let mut synth = Timeline::apple(48000).unwrap();
        send(
            &mut router,
            &mut synth,
            &[0xB1, 11, 0, 0xC1, 0, 0x91, 60, 100],
        );
        assert!(peak(&mut synth, 100_000_000) < 0.0001);
        send(
            &mut router,
            &mut synth,
            &[0xF0, 0x41, 0x10, 0x16, 0x12, 0x7F, 0, 0, 0, 1, 0xF7],
        );
        send(&mut router, &mut synth, &[0x91, 64, 100]);
        assert!(peak(&mut synth, 300_000_000) > 0.0001);
    }
}
