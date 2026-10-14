//! Independent Sound Blaster 16 DSP control/probe foundation.
//! Control responses are immediate; playback consumes explicit DMA units and
//! returns stereo frames/block IRQ events. Busy/reset timing and ADC are pending.
#![no_std]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DspError {
    UnsupportedCommand(u8),
    InvalidSampleRate,
    NoTransfer,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Format {
    pub word: bool,
    pub stereo: bool,
    pub signed: bool,
}
#[derive(Clone, Copy, Debug)]
struct Playback {
    format: Format,
    length: u32,
    remaining: u32,
    auto_init: bool,
    paused: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleEvent {
    pub frame: Option<[i16; 2]>,
    pub raised_irq: bool,
}
#[derive(Debug)]
pub struct Dsp {
    reset: bool,
    reply: [u8; 2],
    reply_len: u8,
    last: u8,
    parameter: Option<u8>,
    rate_high: Option<u8>,
    test: u8,
    speaker: bool,
    sample_rate: u32,
    input_rate: u16,
    dma_parameters: [u8; 3],
    dma_parameter_len: usize,
    playback: Option<Playback>,
    left: Option<i16>,
    irq8: bool,
    irq16: bool,
}
impl Default for Dsp {
    fn default() -> Self {
        Self {
            reset: false,
            reply: [0; 2],
            reply_len: 0,
            last: 0xAA,
            parameter: None,
            rate_high: None,
            test: 0,
            speaker: false,
            sample_rate: 22050,
            input_rate: 22050,
            dma_parameters: [0; 3],
            dma_parameter_len: 0,
            playback: None,
            left: None,
            irq8: false,
            irq16: false,
        }
    }
}
impl Dsp {
    pub fn write_reset(&mut self, value: u8) {
        if value & 1 != 0 {
            *self = Self::default();
            self.reset = true;
        } else if self.reset {
            self.reset = false;
            self.response(&[0xAA]);
        }
    }
    pub fn read_status(&self) -> u8 {
        if self.reply_len != 0 { 0x80 } else { 0 }
    }
    pub fn write_status(&self) -> u8 {
        if self.reset { 0x80 } else { 0 }
    }
    pub fn read_data(&mut self) -> u8 {
        if self.reply_len != 0 {
            self.last = self.reply[0];
            self.reply[0] = self.reply[1];
            self.reply_len -= 1;
        }
        self.last
    }
    pub fn speaker_enabled(&self) -> bool {
        self.speaker
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    pub fn playback_format(&self) -> Option<Format> {
        self.playback
            .filter(|state| !state.paused)
            .map(|state| state.format)
    }
    pub fn irq_status(&self) -> u8 {
        u8::from(self.irq8) | (u8::from(self.irq16) << 1)
    }
    pub fn acknowledge_irq(&mut self, word: bool) {
        if word {
            self.irq16 = false;
        } else {
            self.irq8 = false;
        }
    }
    /// Consume a single DMA unit. Stereo output waits for both channels; block
    /// IRQ state and length are counted in units, independent of host batches.
    pub fn accept_sample(&mut self, value: u16) -> Result<SampleEvent, DspError> {
        let state = self
            .playback
            .as_mut()
            .filter(|state| !state.paused)
            .ok_or(DspError::NoTransfer)?;
        let format = state.format;
        let sample = if format.word {
            if format.signed {
                value as i16
            } else {
                value.wrapping_sub(0x8000) as i16
            }
        } else if format.signed {
            i16::from(value as u8 as i8) << 8
        } else {
            (i16::from(value as u8) - 128) << 8
        };
        let frame = if format.stereo {
            if let Some(left) = self.left.take() {
                Some([left, sample])
            } else {
                self.left = Some(sample);
                None
            }
        } else {
            Some([sample, sample])
        };
        state.remaining -= 1;
        let mut raised_irq = false;
        if state.remaining == 0 {
            let irq = if format.word {
                &mut self.irq16
            } else {
                &mut self.irq8
            };
            raised_irq = !*irq;
            *irq = true;
            if state.auto_init {
                state.remaining = state.length;
            } else {
                self.playback = None;
                self.left = None;
            }
        }
        Ok(SampleEvent { frame, raised_irq })
    }
    fn response(&mut self, bytes: &[u8]) {
        self.reply[..bytes.len()].copy_from_slice(bytes);
        self.reply_len = bytes.len() as u8;
    }
    pub fn write_command(&mut self, value: u8) -> Result<(), DspError> {
        if self.reset {
            return Ok(());
        }
        if let Some(command) = self.parameter.take() {
            match command {
                0xB0..=0xB7 | 0xC0..=0xC7 => {
                    self.dma_parameters[self.dma_parameter_len] = value;
                    self.dma_parameter_len += 1;
                    if self.dma_parameter_len == 3 {
                        let [mode, low, high] = self.dma_parameters;
                        let length = u32::from(u16::from_le_bytes([low, high])) + 1;
                        self.playback = Some(Playback {
                            format: Format {
                                word: command & 0x10 != 0,
                                stereo: mode & 0x20 != 0,
                                signed: mode & 0x10 != 0,
                            },
                            length,
                            remaining: length,
                            auto_init: command & 4 != 0,
                            paused: false,
                        });
                        self.left = None;
                        self.dma_parameter_len = 0;
                    } else {
                        self.parameter = Some(command);
                    }
                }
                0xE0 => self.response(&[!value]),
                0xE4 => self.test = value,
                0x40 => self.sample_rate = 1_000_000 / (256 - u32::from(value)),
                0x41 | 0x42 => {
                    if let Some(high) = self.rate_high.take() {
                        let rate = u16::from_be_bytes([high, value]);
                        if rate == 0 {
                            return Err(DspError::InvalidSampleRate);
                        }
                        if command == 0x41 {
                            self.sample_rate = u32::from(rate);
                        } else {
                            self.input_rate = rate;
                        }
                    } else {
                        self.rate_high = Some(value);
                        self.parameter = Some(command);
                    }
                }
                _ => unreachable!(),
            }
        } else {
            match value {
                0xB0..=0xB7 | 0xC0..=0xC7 => {
                    self.parameter = Some(value);
                    self.dma_parameter_len = 0;
                }
                0xD0 | 0xD5 => {
                    if let Some(state) = &mut self.playback {
                        state.paused = true;
                    }
                }
                0xD4 | 0xD6 => {
                    if let Some(state) = &mut self.playback {
                        state.paused = false;
                    }
                }
                0xD9 | 0xDA => {
                    if let Some(state) = &mut self.playback {
                        state.auto_init = false;
                    }
                }
                0xE0 | 0xE4 | 0x40 | 0x41 | 0x42 => self.parameter = Some(value),
                0xE1 => self.response(&[4, 5]),
                0xE8 => self.response(&[self.test]),
                0xD1 => self.speaker = true,
                0xD3 => self.speaker = false,
                0xD8 => self.response(&[if self.speaker { 0xFF } else { 0 }]),
                _ => return Err(DspError::UnsupportedCommand(value)),
            }
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reset_edge_and_version_polling_produce_bounded_real_responses() {
        let mut dsp = Dsp::default();
        dsp.write_reset(0);
        assert_eq!(dsp.read_status(), 0);
        dsp.write_reset(1);
        assert_eq!(dsp.write_status(), 0x80);
        dsp.write_reset(0);
        assert_eq!(dsp.read_status(), 0x80);
        assert_eq!(dsp.read_data(), 0xAA);
        assert_eq!(dsp.read_status(), 0);
        dsp.write_command(0xE1).unwrap();
        assert_eq!(dsp.read_data(), 4);
        assert_eq!(dsp.read_status(), 0x80);
        assert_eq!(dsp.read_data(), 5);
        assert_eq!(dsp.read_status(), 0);
    }
    #[test]
    fn parameter_bytes_are_data_and_unsupported_dma_stops_explicitly() {
        let mut dsp = Dsp::default();
        dsp.write_command(0xE0).unwrap();
        dsp.write_command(0xE1).unwrap();
        assert_eq!(dsp.read_data(), 0x1E);
        dsp.write_command(0xE4).unwrap();
        dsp.write_command(0x55).unwrap();
        dsp.write_command(0xE8).unwrap();
        assert_eq!(dsp.read_data(), 0x55);
        dsp.write_command(0x40).unwrap();
        dsp.write_command(156).unwrap();
        assert_eq!(dsp.sample_rate(), 10000);
        dsp.write_command(0x41).unwrap();
        dsp.write_command(0x56).unwrap();
        dsp.write_command(0x22).unwrap();
        assert_eq!(dsp.sample_rate(), 22050);
        dsp.write_command(0xD1).unwrap();
        dsp.write_command(0xD8).unwrap();
        assert_eq!(dsp.read_data(), 0xFF);
        assert_eq!(
            dsp.write_command(0xBC),
            Err(DspError::UnsupportedCommand(0xBC))
        );
        assert!(dsp.speaker_enabled());
    }
    #[test]
    fn stereo_auto_init_sample_units_irq_ack_and_pause_preserve_block_state() {
        let mut dsp = Dsp::default();
        for byte in [0xB6, 0x30, 3, 0] {
            dsp.write_command(byte).unwrap();
        }
        assert_eq!(dsp.accept_sample(0x8000).unwrap().frame, None);
        assert_eq!(
            dsp.accept_sample(0x7FFF).unwrap().frame,
            Some([i16::MIN, i16::MAX])
        );
        dsp.write_command(0xD5).unwrap();
        assert_eq!(dsp.accept_sample(0), Err(DspError::NoTransfer));
        dsp.write_command(0xD6).unwrap();
        dsp.accept_sample(0).unwrap();
        let event = dsp.accept_sample(0).unwrap();
        assert!(event.raised_irq);
        assert_eq!(event.frame, Some([0, 0]));
        assert_eq!(dsp.irq_status(), 2);
        dsp.acknowledge_irq(false);
        assert_eq!(dsp.irq_status(), 2);
        dsp.acknowledge_irq(true);
        assert_eq!(dsp.irq_status(), 0);
        dsp.write_command(0xD9).unwrap();
        for _ in 0..4 {
            dsp.accept_sample(0).unwrap();
        }
        assert_eq!(dsp.playback_format(), None);
    }
}
