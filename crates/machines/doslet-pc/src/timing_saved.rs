// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

impl Timing {
    pub(crate) fn saved_clock_matches(&self, mhz: u16, nanos: u64) -> bool {
        self.cpu.mhz == u64::from(mhz) && self.time.as_nanos() == nanos
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct PcStepClock {
    #[prost(uint64, tag = "1")]
    fraction: u64,
    #[prost(uint64, tag = "2")]
    mhz: u64,
    #[prost(uint64, tag = "3")]
    whole: u64,
    #[prost(uint64, tag = "4")]
    remainder: u64,
}
impl State for StepClock {
    type Wire = PcStepClock;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            fraction: self.fraction,
            mhz: self.mhz,
            whole: self.whole,
            remainder: self.remainder,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            fraction: w.fraction,
            mhz: w.mhz,
            whole: w.whole,
            remainder: w.remainder,
        };
        if !((1..=1000).contains(&value.mhz)
            && value.fraction < value.mhz
            && value.whole == 4000 / value.mhz
            && value.remainder == 4000 % value.mhz)
        {
            return Err(InvalidState("PcStepClock invariants"));
        }
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct PcTiming {
    #[prost(message, optional, tag = "1")]
    cpu: Option<<StepClock as State>::Wire>,
    #[prost(message, optional, tag = "2")]
    time: Option<<EmuTime as State>::Wire>,
    #[prost(uint64, tag = "3")]
    pit_fraction: u64,
    #[prost(uint64, tag = "4")]
    pcm_fraction: u64,
}
impl State for Timing {
    type Wire = PcTiming;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            cpu: Some(State::save(&self.cpu)),
            time: Some(State::save(&self.time)),
            pit_fraction: self.pit_fraction,
            pcm_fraction: self.pcm_fraction,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            cpu: <StepClock as State>::load(doslet_state::required(w.cpu, "PcTiming.cpu")?)?,
            external_cpu: false,
            tiers: None,
            time: <EmuTime as State>::load(doslet_state::required(w.time, "PcTiming.time")?)?,
            pit_fraction: w.pit_fraction,
            pcm_fraction: w.pcm_fraction,
        };
        if !(value.pit_fraction < 1_000_000_000 && value.pcm_fraction < 1_000_000_000) {
            return Err(InvalidState("PcTiming invariants"));
        }
        Ok(value)
    }
}
