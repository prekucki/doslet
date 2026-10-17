// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct PcPcMachine {
    #[prost(message, optional, tag = "1")]
    cpu: Option<<Cpu as State>::Wire>,
    #[prost(message, optional, tag = "2")]
    bus: Option<<PcBus as State>::Wire>,
    #[prost(message, optional, tag = "3")]
    timing: Option<<timing::Timing as State>::Wire>,
    #[prost(uint64, tag = "8")]
    interrupt_entries: u64,
}
impl State for PcMachine {
    type Wire = PcPcMachine;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            cpu: Some(State::save(&self.cpu)),
            bus: Some(State::save(&self.bus)),
            timing: Some(State::save(&self.timing)),
            interrupt_entries: self.interrupt_entries,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            cpu: <Cpu as State>::load(doslet_state::required(w.cpu, "PcPcMachine.cpu")?)?,
            bus: <PcBus as State>::load(doslet_state::required(w.bus, "PcPcMachine.bus")?)?,
            timing: <timing::Timing as State>::load(doslet_state::required(
                w.timing,
                "PcPcMachine.timing",
            )?)?,
            interrupt_entries: w.interrupt_entries,
            vga_wait: Default::default(),
            memory_wait_sites: None,
            memory_wait_steps: 0,
            batch_yield_ip: None,
        };
        if !value
            .timing
            .saved_clock_matches(value.hardware_config().cpu_mhz, value.bus.guest_nanos)
        {
            return Err(InvalidState("PC device/CPU clock mismatch"));
        }
        Ok(value)
    }
}
