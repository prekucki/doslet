// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct X86ControlRegisters {
    #[prost(uint32, tag = "1")]
    cr0: u32,
    #[prost(uint32, tag = "2")]
    cr2: u32,
    #[prost(uint32, tag = "3")]
    cr3: u32,
    #[prost(message, optional, tag = "4")]
    gdtr: Option<<DescriptorTable as State>::Wire>,
    #[prost(message, optional, tag = "5")]
    idtr: Option<<DescriptorTable as State>::Wire>,
    #[prost(message, optional, tag = "6")]
    ldtr: Option<<DescriptorTable as State>::Wire>,
    #[prost(uint32, tag = "7")]
    ldtr_selector: u32,
    #[prost(message, optional, tag = "8")]
    task: Option<<SegmentCache as State>::Wire>,
}
impl State for ControlRegisters {
    type Wire = X86ControlRegisters;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            cr0: self.cr0,
            cr2: self.cr2,
            cr3: self.cr3,
            gdtr: Some(State::save(&self.gdtr)),
            idtr: Some(State::save(&self.idtr)),
            ldtr: self.ldtr.as_ref().map(State::save),
            ldtr_selector: u32::from(self.ldtr_selector),
            task: self.task.as_ref().map(State::save),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            cr0: w.cr0,
            cr2: w.cr2,
            cr3: w.cr3,
            gdtr: <DescriptorTable as State>::load(doslet_state::required(
                w.gdtr,
                "X86ControlRegisters.gdtr",
            )?)?,
            idtr: <DescriptorTable as State>::load(doslet_state::required(
                w.idtr,
                "X86ControlRegisters.idtr",
            )?)?,
            ldtr: w
                .ldtr
                .map(|v| -> Result<_, InvalidState> { <DescriptorTable as State>::load(v) })
                .transpose()?,
            ldtr_selector: doslet_state::narrow(
                w.ldtr_selector,
                "X86ControlRegisters.ldtr_selector",
            )?,
            task: w
                .task
                .map(|v| -> Result<_, InvalidState> { <SegmentCache as State>::load(v) })
                .transpose()?,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct X86DescriptorTable {
    #[prost(uint32, tag = "1")]
    base: u32,
    #[prost(uint32, tag = "2")]
    limit: u32,
}
impl State for DescriptorTable {
    type Wire = X86DescriptorTable;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            base: self.base,
            limit: u32::from(self.limit),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            base: w.base,
            limit: doslet_state::narrow(w.limit, "X86DescriptorTable.limit")?,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct X86SegmentCache {
    #[prost(uint32, tag = "1")]
    selector: u32,
    #[prost(uint32, tag = "2")]
    base: u32,
    #[prost(uint32, tag = "3")]
    limit: u32,
    #[prost(uint32, tag = "4")]
    access: u32,
    #[prost(uint32, tag = "5")]
    flags: u32,
    #[prost(bool, tag = "6")]
    default32: bool,
    #[prost(bool, tag = "7")]
    usable: bool,
}
impl State for SegmentCache {
    type Wire = X86SegmentCache;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            selector: u32::from(self.selector),
            base: self.base,
            limit: self.limit,
            access: u32::from(self.access),
            flags: u32::from(self.flags),
            default32: self.default32,
            usable: self.usable,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            selector: doslet_state::narrow(w.selector, "X86SegmentCache.selector")?,
            base: w.base,
            limit: w.limit,
            access: doslet_state::narrow(w.access, "X86SegmentCache.access")?,
            flags: doslet_state::narrow(w.flags, "X86SegmentCache.flags")?,
            default32: w.default32,
            usable: w.usable,
        };
        Ok(value)
    }
}
