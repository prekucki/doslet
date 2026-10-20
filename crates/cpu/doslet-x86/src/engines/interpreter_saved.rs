// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct X86CpuSnapshot {
    #[prost(uint32, repeated, tag = "1")]
    general: Vec<u32>,
    #[prost(uint32, tag = "2")]
    ip: u32,
    #[prost(uint32, repeated, tag = "3")]
    selectors: Vec<u32>,
    #[prost(message, repeated, tag = "4")]
    caches: Vec<<crate::SegmentCache as State>::Wire>,
    #[prost(bool, tag = "5")]
    protected: bool,
    #[prost(uint32, tag = "6")]
    cpl: u32,
    #[prost(uint32, tag = "7")]
    flags: u32,
    #[prost(message, optional, tag = "8")]
    control: Option<<ControlRegisters as State>::Wire>,
    #[prost(message, optional, tag = "9")]
    fpu: Option<<doslet_x87::X87 as State>::Wire>,
    #[prost(bool, tag = "10")]
    halted: bool,
    #[prost(uint32, tag = "11")]
    interrupt_shadow: u32,
}
impl State for CpuSnapshot {
    type Wire = X86CpuSnapshot;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            general: self.general.to_vec(),
            ip: self.ip,
            selectors: self.selectors.iter().map(|v| u32::from(*(v))).collect(),
            caches: self.caches.iter().map(State::save).collect(),
            protected: self.protected,
            cpl: u32::from(self.cpl),
            flags: self.flags,
            control: Some(State::save(&self.control)),
            fpu: self.fpu.as_ref().map(State::save),
            halted: self.halted,
            interrupt_shadow: u32::from(self.interrupt_shadow),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            general: doslet_state::narrow(
                w.general
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "X86CpuSnapshot.general length",
            )?,
            ip: w.ip,
            selectors: doslet_state::narrow(
                w.selectors
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        doslet_state::narrow(v, "X86CpuSnapshot.selectors")
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "X86CpuSnapshot.selectors length",
            )?,
            caches: doslet_state::narrow(
                w.caches
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { <crate::SegmentCache as State>::load(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "X86CpuSnapshot.caches length",
            )?,
            protected: w.protected,
            cpl: doslet_state::narrow(w.cpl, "X86CpuSnapshot.cpl")?,
            flags: w.flags,
            control: <ControlRegisters as State>::load(doslet_state::required(
                w.control,
                "X86CpuSnapshot.control",
            )?)?,
            fpu: w
                .fpu
                .map(|v| -> Result<_, InvalidState> { <doslet_x87::X87 as State>::load(v) })
                .transpose()?,
            halted: w.halted,
            interrupt_shadow: doslet_state::narrow(
                w.interrupt_shadow,
                "X86CpuSnapshot.interrupt_shadow",
            )?,
        };
        Ok(value)
    }
}

/// A saved CPU context, as runtime hosts keep for nested mode switches.
#[derive(Clone, PartialEq, prost::Message)]
pub struct X86CpuContext {
    #[prost(uint32, repeated, tag = "1")]
    general: Vec<u32>,
    #[prost(uint32, tag = "2")]
    ip: u32,
    #[prost(message, repeated, tag = "3")]
    caches: Vec<<crate::SegmentCache as State>::Wire>,
    #[prost(bool, tag = "4")]
    protected: bool,
    #[prost(uint32, tag = "5")]
    cpl: u32,
    #[prost(uint32, tag = "6")]
    flags: u32,
    #[prost(message, optional, tag = "7")]
    control: Option<<ControlRegisters as State>::Wire>,
    #[prost(bool, tag = "8")]
    halted: bool,
    #[prost(uint32, tag = "9")]
    interrupt_shadow: u32,
}
impl State for CpuContext {
    type Wire = X86CpuContext;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            general: self.general.to_vec(),
            ip: self.ip,
            caches: self.caches.iter().map(State::save).collect(),
            protected: self.protected,
            cpl: u32::from(self.cpl),
            flags: self.flags,
            control: Some(State::save(&self.control)),
            halted: self.halted,
            interrupt_shadow: u32::from(self.interrupt_shadow),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let caches = w
            .caches
            .into_iter()
            .map(<crate::SegmentCache as State>::load)
            .collect::<Result<Vec<_>, _>>()?;
        let value = Self {
            general: doslet_state::narrow(w.general, "X86CpuContext.general length")?,
            ip: w.ip,
            caches: doslet_state::narrow(caches, "X86CpuContext.caches length")?,
            protected: w.protected,
            cpl: doslet_state::narrow(w.cpl, "X86CpuContext.cpl")?,
            flags: w.flags,
            control: <ControlRegisters as State>::load(doslet_state::required(
                w.control,
                "X86CpuContext.control",
            )?)?,
            halted: w.halted,
            interrupt_shadow: doslet_state::narrow(
                w.interrupt_shadow,
                "X86CpuContext.interrupt_shadow",
            )?,
        };
        if value.cpl > 3 || value.interrupt_shadow > 2 {
            return Err(InvalidState("CPU context privilege/interrupt shadow"));
        }
        Ok(value)
    }
}
