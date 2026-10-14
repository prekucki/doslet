//! Bounded, allocation-free MIDI wire receiver and procedural stereo synthesizer.
//! These are basic program families, not sample-based GM or Roland LA emulation.
#![no_std]
pub mod stream;
mod tables;
pub mod wire;

const VOICES: usize = 64;
#[derive(Clone, Copy)]
struct Channel {
    program: u8,
    volume: u8,
    expression: u8,
    pan: u8,
    sustain: bool,
    bend: u16,
    /// Pitch-bend sensitivity in semitones (RPN 0), up to two octaves.
    bend_range: u8,
    /// Selected registered parameter (MSB, LSB); 127/127 is null.
    rpn: [u8; 2],
}
impl Channel {
    const DEFAULT: Self = Self {
        program: 0,
        volume: 100,
        expression: 127,
        pan: 64,
        sustain: false,
        bend: 8192,
        bend_range: 2,
        rpn: [127, 127],
    };
}
#[derive(Clone, Copy)]
struct Voice {
    active: bool,
    channel: u8,
    note: u8,
    velocity: u8,
    program: u8,
    phase: u32,
    step: u32,
    envelope: f32,
    released: bool,
    key_down: bool,
    age: u64,
    noise: u32,
    attacking: bool,
}
impl Voice {
    const EMPTY: Self = Self {
        active: false,
        channel: 0,
        note: 0,
        velocity: 0,
        program: 0,
        phase: 0,
        step: 0,
        envelope: 0.0,
        released: false,
        key_down: false,
        age: 0,
        noise: 1,
        attacking: false,
    };
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MidiError {
    SampleRate,
    UnsupportedSystem(u8),
    TimeReversed,
}
impl core::fmt::Display for MidiError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::SampleRate => f.write_str("MIDI sample rate must be 22050..192000 Hz"),
            Self::UnsupportedSystem(status) => {
                write!(f, "unsupported MIDI system message {status:02X}")
            }
            Self::TimeReversed => f.write_str("MIDI renderer time moved backwards"),
        }
    }
}
impl core::error::Error for MidiError {}

/// Maps the guest nanosecond timeline to audio frames without host-clock reads
/// or allocations. Feed wire bytes after rendering to their receive timestamp.
pub struct Timeline {
    synth: Synth,
    frames: u64,
}
impl Timeline {
    pub fn new(rate: u32) -> Result<Self, MidiError> {
        Ok(Self {
            synth: Synth::new(rate)?,
            frames: 0,
        })
    }
    pub fn push_byte(&mut self, byte: u8) -> Result<(), MidiError> {
        self.synth.push_byte(byte)
    }
    pub fn note_events(&self) -> u64 {
        self.synth.note_events()
    }
    pub fn sample_rate(&self) -> u32 {
        self.synth.sample_rate()
    }
    /// A host MIDI port synthesizes elsewhere. Advance its sample cursor without
    /// wasting CPU rendering discarded samples. Switching back uses State replay.
    pub fn skip_until(&mut self, nanos: u64) -> Result<(), MidiError> {
        let target = (u128::from(nanos) * u128::from(self.sample_rate()) / 1_000_000_000) as u64;
        if target < self.frames {
            return Err(MidiError::TimeReversed);
        }
        self.frames = target;
        Ok(())
    }
    pub fn render_until(
        &mut self,
        nanos: u64,
        output: &mut [[f32; 2]],
    ) -> Result<usize, MidiError> {
        let target = (u128::from(nanos) * u128::from(self.sample_rate()) / 1_000_000_000) as u64;
        let remaining = target
            .checked_sub(self.frames)
            .ok_or(MidiError::TimeReversed)?;
        let count = remaining.min(output.len() as u64) as usize;
        self.synth.render(&mut output[..count]);
        self.frames += count as u64;
        Ok(count)
    }
}

pub struct Synth {
    rate: u32,
    channels: [Channel; 16],
    voices: [Voice; VOICES],
    status: u8,
    data: [u8; 2],
    received: u8,
    sysex: bool,
    age: u64,
    notes: u64,
}
impl Synth {
    pub fn new(rate: u32) -> Result<Self, MidiError> {
        if !(22050..=192000).contains(&rate) {
            return Err(MidiError::SampleRate);
        }
        Ok(Self {
            rate,
            channels: [Channel::DEFAULT; 16],
            voices: [Voice::EMPTY; VOICES],
            status: 0,
            data: [0; 2],
            received: 0,
            sysex: false,
            age: 0,
            notes: 0,
        })
    }
    pub fn sample_rate(&self) -> u32 {
        self.rate
    }
    pub fn note_events(&self) -> u64 {
        self.notes
    }
    pub fn reset(&mut self) {
        self.channels.fill(Channel::DEFAULT);
        self.voices.fill(Voice::EMPTY);
        self.status = 0;
        self.received = 0;
        self.sysex = false;
    }
    pub fn push_byte(&mut self, byte: u8) -> Result<(), MidiError> {
        if byte >= 0xF8 {
            if byte == 0xFF {
                self.reset();
            }
            return Ok(()); // MIDI realtime does not disturb running status or partial messages.
        }
        if byte == 0xF7 {
            self.sysex = false;
            self.status = 0;
            self.received = 0;
            return Ok(());
        }
        if self.sysex && byte < 0x80 {
            return Ok(());
        }
        if byte & 0x80 != 0 {
            self.received = 0;
            self.sysex = byte == 0xF0;
            if byte >= 0xF0 {
                self.status = 0;
                return Err(MidiError::UnsupportedSystem(byte));
            }
            self.status = byte;
            return Ok(());
        }
        if self.status == 0 {
            return Ok(());
        }
        self.data[usize::from(self.received)] = byte;
        self.received += 1;
        let needed = if matches!(self.status & 0xF0, 0xC0 | 0xD0) {
            1
        } else {
            2
        };
        if self.received == needed {
            self.message();
            self.received = 0;
        }
        Ok(())
    }
    fn step(&self, note: u8, bend: u16, range: u8) -> u32 {
        // BEND spans +/-2 semitones in 1/64-semitone entries. Larger offsets move
        // whole semitones to the note, so the default range keeps exact results.
        let offset = (i32::from(bend) - 8192) * i32::from(range);
        let semitones = if offset.abs() <= 16384 {
            0
        } else {
            offset.div_euclid(8192)
        };
        let position = (offset - semitones * 8192 + 16384) as usize;
        let note = (i32::from(note) + semitones).clamp(0, 127) as usize;
        let index = position / 128;
        let fraction = (position % 128) as u64;
        let a = u64::from(tables::BEND[index]);
        let b = u64::from(tables::BEND[(index + 1).min(256)]);
        let scale = a + ((b - a) * fraction / 128);
        (((u64::from(tables::STEP_48000[note]) * scale) >> 24) * 48000 / u64::from(self.rate))
            as u32
    }
    fn message(&mut self) {
        let channel = self.status & 15;
        let [first, second] = self.data;
        match self.status & 0xF0 {
            0x80 => self.note_off(channel, first),
            0x90 if second == 0 => self.note_off(channel, first),
            0x90 => {
                let slot = self
                    .voices
                    .iter()
                    .position(|v| v.active && v.channel == channel && v.note == first)
                    .or_else(|| self.voices.iter().position(|v| !v.active))
                    .unwrap_or_else(|| {
                        self.voices
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, v)| v.age)
                            .unwrap()
                            .0
                    });
                let state = self.channels[usize::from(channel)];
                self.age += 1;
                self.notes += 1;
                self.voices[slot] = Voice {
                    active: true,
                    channel,
                    note: first,
                    velocity: second,
                    program: state.program,
                    step: self.step(first, state.bend, state.bend_range),
                    age: self.age,
                    noise: 0x9E37_79B9 ^ u32::from(first),
                    attacking: true,
                    ..Voice::EMPTY
                };
                self.voices[slot].key_down = true;
            }
            0xB0 => {
                let state = &mut self.channels[usize::from(channel)];
                match first {
                    7 => state.volume = second,
                    10 => state.pan = second,
                    11 => state.expression = second,
                    101 => state.rpn[0] = second,
                    100 => state.rpn[1] = second,
                    6 if state.rpn == [0, 0] => {
                        state.bend_range = second.min(24);
                        self.retune(channel);
                    }
                    64 => {
                        state.sustain = second >= 64;
                        if !state.sustain {
                            for v in &mut self.voices {
                                if v.channel == channel && !v.key_down {
                                    v.released = true;
                                }
                            }
                        }
                    }
                    120 => {
                        for v in &mut self.voices {
                            if v.channel == channel {
                                v.active = false;
                            }
                        }
                    }
                    121 => {
                        state.expression = 127;
                        state.bend = 8192;
                        state.sustain = false;
                        state.rpn = [127, 127];
                        for v in &mut self.voices {
                            if v.channel == channel && !v.key_down {
                                v.released = true;
                            }
                        }
                        self.retune(channel);
                    }
                    123..=127 => {
                        for v in &mut self.voices {
                            if v.channel == channel {
                                v.key_down = false;
                                v.released = !state.sustain;
                            }
                        }
                    }
                    _ => {} // Bank/effect/other controls have no procedural-voice counterpart.
                }
            }
            0xC0 => self.channels[usize::from(channel)].program = first,
            0xE0 => {
                self.channels[usize::from(channel)].bend =
                    u16::from(first) | (u16::from(second) << 7);
                self.retune(channel);
            }
            _ => {} // Poly/channel pressure is accepted; pressure-based timbre is pending.
        }
    }
    fn retune(&mut self, channel: u8) {
        let state = self.channels[usize::from(channel)];
        for index in 0..VOICES {
            let v = self.voices[index];
            if v.active && v.channel == channel {
                self.voices[index].step = self.step(v.note, state.bend, state.bend_range);
            }
        }
    }
    fn note_off(&mut self, channel: u8, note: u8) {
        for v in &mut self.voices {
            if v.active && v.channel == channel && v.note == note {
                v.key_down = false;
                v.released = !self.channels[usize::from(channel)].sustain;
            }
        }
    }
    pub fn render(&mut self, output: &mut [[f32; 2]]) {
        let attack = 1.0 / (self.rate as f32 * 0.008);
        let release = 1.0 / (self.rate as f32 * 0.15);
        let decay = 1.0 / (self.rate as f32 * 3.0);
        for frame in output {
            let mut sum = [0.0f32; 2];
            for v in &mut self.voices {
                if !v.active {
                    continue;
                }
                if v.released {
                    v.envelope = (v.envelope - release).max(0.0);
                    if v.envelope == 0.0 {
                        v.active = false;
                        continue;
                    }
                } else if v.attacking {
                    v.envelope = (v.envelope + attack).min(1.0);
                    if v.envelope == 1.0 {
                        v.attacking = false;
                    }
                } else if v.channel == 9 {
                    v.envelope = (v.envelope - 1.0 / (self.rate as f32 * 0.18)).max(0.0);
                } else if v.program < 8 || (24..32).contains(&v.program) {
                    v.envelope = (v.envelope - decay).max(0.0);
                }
                let wave = if v.channel == 9 {
                    v.noise ^= v.noise << 13;
                    v.noise ^= v.noise >> 17;
                    v.noise ^= v.noise << 5;
                    (v.noise as i32) as f32 / i32::MAX as f32
                } else {
                    let family = v.program / 8;
                    let mut wave = sine(v.phase);
                    for (harmonic, weight) in match family {
                        2 | 7 | 10 => [(2, 0.35), (3, 0.18), (4, 0.08)],
                        5 | 6 => [(2, 0.12), (3, 0.08), (4, 0.04)],
                        _ => [(2, 0.18), (3, 0.04), (4, 0.02)],
                    } {
                        if u64::from(v.step) * harmonic < 1 << 31 {
                            wave += sine(v.phase.wrapping_mul(harmonic as u32)) * weight;
                        }
                    }
                    wave / 1.6
                };
                v.phase = v.phase.wrapping_add(v.step);
                let state = self.channels[usize::from(v.channel)];
                let amplitude = wave * v.envelope * f32::from(v.velocity) / 127.0
                    * f32::from(state.volume)
                    / 127.0
                    * f32::from(state.expression)
                    / 127.0
                    * 0.25;
                let pan = tables::PAN[usize::from(state.pan)];
                sum[0] += amplitude * pan[0];
                sum[1] += amplitude * pan[1];
            }
            *frame = sum.map(|sample| sample / (1.0 + sample.abs()));
        }
    }
}
fn sine(phase: u32) -> f32 {
    let index = (phase >> 22) as usize;
    let fraction = (phase & 0x3F_FFFF) as f32 / 4194304.0;
    let a = tables::SINE[index];
    a + (tables::SINE[(index + 1) & 1023] - a) * fraction
}
#[cfg(test)]
mod tests {
    use super::*;
    fn send(synth: &mut Synth, bytes: &[u8]) {
        for byte in bytes {
            synth.push_byte(*byte).unwrap();
        }
    }
    #[test]
    fn running_status_realtime_and_release_produce_bounded_audio() {
        let mut synth = Synth::new(48000).unwrap();
        send(&mut synth, &[0x90, 69, 0xF8, 100, 72, 100]);
        assert_eq!(synth.note_events(), 2);
        let mut frames = [[0.0; 2]; 4800];
        synth.render(&mut frames);
        assert!(frames.iter().flatten().any(|sample| sample.abs() > 0.01));
        assert!(
            frames
                .iter()
                .flatten()
                .all(|sample| sample.is_finite() && sample.abs() < 1.0)
        );
        send(&mut synth, &[69, 0, 72, 0]); // Note-on velocity zero is note-off.
        for _ in 0..2 {
            synth.render(&mut frames);
        }
        assert!(
            frames
                .iter()
                .rev()
                .take(200)
                .flatten()
                .all(|sample| *sample == 0.0)
        );
    }
    #[test]
    fn sustain_pan_pitch_bend_and_reset_change_the_active_voices() {
        let mut synth = Synth::new(48000).unwrap();
        send(&mut synth, &[0xB0, 10, 0, 64, 127, 0xC0, 48, 0x90, 69, 100]);
        let initial = synth.voices[0].step;
        send(&mut synth, &[0xE0, 127, 127]);
        assert!(synth.voices[0].step > initial);
        send(&mut synth, &[0x80, 69, 0]);
        let mut frames = [[0.0; 2]; 4800];
        synth.render(&mut frames);
        assert!(frames.iter().any(|frame| frame[0].abs() > 0.01));
        assert!(frames.iter().all(|frame| frame[1] == 0.0));
        send(&mut synth, &[0xB0, 64, 0]);
        for _ in 0..2 {
            synth.render(&mut frames);
        }
        assert!(
            frames
                .iter()
                .rev()
                .take(200)
                .flatten()
                .all(|sample| *sample == 0.0)
        );
        synth.reset();
        assert_eq!(synth.channels[0].bend, 8192);
    }
    #[test]
    fn rpn_zero_sets_bend_range_and_keeps_the_default_two_semitones() {
        let mut synth = Synth::new(48000).unwrap();
        let plain = |note: u8, bend: u16| {
            let index = usize::from(bend / 64);
            let a = u64::from(tables::BEND[index]);
            let b = u64::from(tables::BEND[(index + 1).min(256)]);
            let scale = a + (b - a) * u64::from(bend % 64) / 64;
            ((u64::from(tables::STEP_48000[usize::from(note)]) * scale) >> 24) as u32
        };
        for bend in [0, 1, 63, 4096, 8191, 8192, 8255, 12000, 16383] {
            assert_eq!(synth.step(60, bend, 2), plain(60, bend), "bend {bend}");
        }
        // A 12-semitone range moves an exact full-scale offset (16384) one octave.
        assert_eq!(synth.step(48, 16384, 12), synth.step(60, 8192, 2));
        assert_eq!(synth.step(60, 0, 12), synth.step(48, 8192, 2));
        send(&mut synth, &[0x90, 60, 100, 0xE0, 127, 127]);
        let two = synth.voices[0].step;
        send(&mut synth, &[0xB0, 101, 0, 100, 0, 6, 12]);
        assert!(synth.voices[0].step > two);
        assert_eq!(synth.channels[0].bend_range, 12);
        send(&mut synth, &[0xB0, 101, 127, 100, 127, 6, 1]);
        assert_eq!(
            synth.channels[0].bend_range, 12,
            "null RPN ignores data entry"
        );
    }
    #[test]
    fn fixed_voice_pool_and_batch_partition_keep_output_identical() {
        let mut a = Synth::new(44100).unwrap();
        let mut b = Synth::new(44100).unwrap();
        for note in 20..100 {
            send(&mut a, &[0x90, note, 100]);
            send(&mut b, &[0x90, note, 100]);
        }
        assert_eq!(a.voices.iter().filter(|voice| voice.active).count(), 64);
        let mut whole = [[0.0; 2]; 512];
        let mut split = [[0.0; 2]; 512];
        a.render(&mut whole);
        for chunk in split.chunks_mut(37) {
            b.render(chunk);
        }
        assert_eq!(whole, split);
        assert_eq!(a.push_byte(0xF0), Err(MidiError::UnsupportedSystem(0xF0)));
    }
}
