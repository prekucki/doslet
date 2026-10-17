// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct SbDspFormat {
    #[prost(bool, tag = "1")]
    word: bool,
    #[prost(bool, tag = "2")]
    stereo: bool,
    #[prost(bool, tag = "3")]
    signed: bool,
}
impl State for Format {
    type Wire = SbDspFormat;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            word: self.word,
            stereo: self.stereo,
            signed: self.signed,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            word: w.word,
            stereo: w.stereo,
            signed: w.signed,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SbDspPlayback {
    #[prost(message, optional, tag = "1")]
    format: Option<<Format as State>::Wire>,
    #[prost(uint32, tag = "2")]
    length: u32,
    #[prost(uint32, tag = "3")]
    remaining: u32,
    #[prost(bool, tag = "4")]
    auto_init: bool,
    #[prost(bool, tag = "5")]
    paused: bool,
    #[prost(bool, optional, tag = "6")]
    previous_block_acknowledged: Option<bool>,
}
impl State for Playback {
    type Wire = SbDspPlayback;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            format: Some(State::save(&self.format)),
            length: self.length,
            remaining: self.remaining,
            auto_init: self.auto_init,
            paused: self.paused,
            previous_block_acknowledged: self.previous_block_acknowledged.as_ref().map(|v| *(v)),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            format: <Format as State>::load(doslet_state::required(
                w.format,
                "SbDspPlayback.format",
            )?)?,
            length: w.length,
            remaining: w.remaining,
            auto_init: w.auto_init,
            paused: w.paused,
            previous_block_acknowledged: w
                .previous_block_acknowledged
                .map(|v| -> Result<_, InvalidState> { Ok(v) })
                .transpose()?,
        };
        if !(value.length != 0 && value.length <= 65536 && value.remaining <= value.length) {
            return Err(InvalidState("SbDspPlayback invariants"));
        }
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SbDspDiagnostics {
    #[prost(uint64, tag = "1")]
    resets: u64,
    #[prost(uint64, tag = "2")]
    commands: u64,
    #[prost(uint32, tag = "3")]
    last_command: u32,
    #[prost(uint64, tag = "4")]
    dma_requests: u64,
    #[prost(uint32, tag = "5")]
    last_dma_command: u32,
    #[prost(uint64, tag = "6")]
    dma_units: u64,
    #[prost(uint64, tag = "7")]
    block_irq_deadlines: u64,
    #[prost(uint64, tag = "8")]
    block_irq_deadline_misses: u64,
}
impl State for Diagnostics {
    type Wire = SbDspDiagnostics;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            resets: self.resets,
            commands: self.commands,
            last_command: u32::from(self.last_command),
            dma_requests: self.dma_requests,
            last_dma_command: u32::from(self.last_dma_command),
            dma_units: self.dma_units,
            block_irq_deadlines: self.block_irq_deadlines,
            block_irq_deadline_misses: self.block_irq_deadline_misses,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            resets: w.resets,
            commands: w.commands,
            last_command: doslet_state::narrow(w.last_command, "SbDspDiagnostics.last_command")?,
            dma_requests: w.dma_requests,
            last_dma_command: doslet_state::narrow(
                w.last_dma_command,
                "SbDspDiagnostics.last_dma_command",
            )?,
            dma_units: w.dma_units,
            block_irq_deadlines: w.block_irq_deadlines,
            block_irq_deadline_misses: w.block_irq_deadline_misses,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct SbDspDsp {
    #[prost(message, optional, tag = "1")]
    diagnostics: Option<<Diagnostics as State>::Wire>,
    #[prost(bool, tag = "2")]
    reset: bool,
    #[prost(bytes = "vec", tag = "3")]
    reply: Vec<u8>,
    #[prost(uint32, tag = "4")]
    reply_len: u32,
    #[prost(uint32, tag = "5")]
    last: u32,
    #[prost(uint32, optional, tag = "6")]
    parameter: Option<u32>,
    #[prost(uint32, optional, tag = "7")]
    rate_high: Option<u32>,
    #[prost(uint32, tag = "8")]
    test: u32,
    #[prost(bool, tag = "9")]
    speaker: bool,
    #[prost(uint32, tag = "10")]
    sample_rate: u32,
    #[prost(uint32, tag = "11")]
    input_rate: u32,
    #[prost(bytes = "vec", tag = "12")]
    dma_parameters: Vec<u8>,
    #[prost(uint64, tag = "13")]
    dma_parameter_len: u64,
    #[prost(message, optional, tag = "14")]
    playback: Option<<Playback as State>::Wire>,
    #[prost(message, optional, tag = "15")]
    configured_format: Option<<Format as State>::Wire>,
    #[prost(sint32, optional, tag = "16")]
    left: Option<i32>,
    #[prost(bool, tag = "17")]
    irq8: bool,
    #[prost(bool, tag = "18")]
    irq16: bool,
}
impl State for Dsp {
    type Wire = SbDspDsp;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            diagnostics: Some(State::save(&self.diagnostics)),
            reset: self.reset,
            reply: self.reply.to_vec(),
            reply_len: u32::from(self.reply_len),
            last: u32::from(self.last),
            parameter: self.parameter.as_ref().map(|v| u32::from(*(v))),
            rate_high: self.rate_high.as_ref().map(|v| u32::from(*(v))),
            test: u32::from(self.test),
            speaker: self.speaker,
            sample_rate: self.sample_rate,
            input_rate: u32::from(self.input_rate),
            dma_parameters: self.dma_parameters.to_vec(),
            dma_parameter_len: self.dma_parameter_len as u64,
            playback: self.playback.as_ref().map(State::save),
            configured_format: self.configured_format.as_ref().map(State::save),
            left: self.left.as_ref().map(|v| i32::from(*(v))),
            irq8: self.irq8,
            irq16: self.irq16,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            diagnostics: <Diagnostics as State>::load(doslet_state::required(
                w.diagnostics,
                "SbDspDsp.diagnostics",
            )?)?,
            reset: w.reset,
            reply: doslet_state::narrow(w.reply, "SbDspDsp.reply length")?,
            reply_len: doslet_state::narrow(w.reply_len, "SbDspDsp.reply_len")?,
            last: doslet_state::narrow(w.last, "SbDspDsp.last")?,
            parameter: w
                .parameter
                .map(|v| -> Result<_, InvalidState> {
                    doslet_state::narrow(v, "SbDspDsp.parameter")
                })
                .transpose()?,
            rate_high: w
                .rate_high
                .map(|v| -> Result<_, InvalidState> {
                    doslet_state::narrow(v, "SbDspDsp.rate_high")
                })
                .transpose()?,
            test: doslet_state::narrow(w.test, "SbDspDsp.test")?,
            speaker: w.speaker,
            sample_rate: w.sample_rate,
            input_rate: doslet_state::narrow(w.input_rate, "SbDspDsp.input_rate")?,
            dma_parameters: doslet_state::narrow(
                w.dma_parameters,
                "SbDspDsp.dma_parameters length",
            )?,
            dma_parameter_len: doslet_state::narrow(
                w.dma_parameter_len,
                "SbDspDsp.dma_parameter_len",
            )?,
            playback: w
                .playback
                .map(|v| -> Result<_, InvalidState> { <Playback as State>::load(v) })
                .transpose()?,
            configured_format: w
                .configured_format
                .map(|v| -> Result<_, InvalidState> { <Format as State>::load(v) })
                .transpose()?,
            left: w
                .left
                .map(|v| -> Result<_, InvalidState> { doslet_state::narrow(v, "SbDspDsp.left") })
                .transpose()?,
            irq8: w.irq8,
            irq16: w.irq16,
        };
        if !(value.reply_len <= 2
            && value.dma_parameter_len <= 3
            && value.sample_rate != 0
            && value.sample_rate <= 1_000_000)
        {
            return Err(InvalidState("SbDspDsp invariants"));
        }
        Ok(value)
    }
}
