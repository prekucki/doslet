// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct AtPending {
    #[prost(uint32, tag = "1")]
    value: u32,
}
impl State for Pending {
    type Wire = AtPending;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            value: match self {
                Self::None => 0,
                Self::Leds => 1,
                Self::Typematic => 2,
                Self::ScanSet => 3,
            },
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        match w.value {
            0 => Ok(Self::None),
            1 => Ok(Self::Leds),
            2 => Ok(Self::Typematic),
            3 => Ok(Self::ScanSet),
            _ => Err(InvalidState("AtPending tag")),
        }
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct AtKeyboard {
    #[prost(message, optional, tag = "1")]
    pending: Option<<Pending as State>::Wire>,
    #[prost(uint32, tag = "2")]
    leds: u32,
    #[prost(uint32, tag = "3")]
    typematic: u32,
    #[prost(uint32, tag = "4")]
    scan_set: u32,
    #[prost(bool, tag = "5")]
    scanning: bool,
    #[prost(uint32, tag = "6")]
    last_reply: u32,
    #[prost(uint64, repeated, tag = "7")]
    pressed: Vec<u64>,
    #[prost(message, optional, tag = "8")]
    repeat: Option<<PhysicalKey as State>::Wire>,
    #[prost(uint64, tag = "9")]
    deadline: u64,
}
impl State for Keyboard {
    type Wire = AtKeyboard;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            pending: Some(State::save(&self.pending)),
            leds: u32::from(self.leds),
            typematic: u32::from(self.typematic),
            scan_set: u32::from(self.scan_set),
            scanning: self.scanning,
            last_reply: u32::from(self.last_reply),
            pressed: self.pressed.to_vec(),
            repeat: self.repeat.as_ref().map(State::save),
            deadline: self.deadline,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            pending: <Pending as State>::load(doslet_state::required(
                w.pending,
                "AtKeyboard.pending",
            )?)?,
            leds: doslet_state::narrow(w.leds, "AtKeyboard.leds")?,
            typematic: doslet_state::narrow(w.typematic, "AtKeyboard.typematic")?,
            scan_set: doslet_state::narrow(w.scan_set, "AtKeyboard.scan_set")?,
            scanning: w.scanning,
            last_reply: doslet_state::narrow(w.last_reply, "AtKeyboard.last_reply")?,
            pressed: doslet_state::narrow(
                w.pressed
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "AtKeyboard.pressed length",
            )?,
            repeat: w
                .repeat
                .map(|v| -> Result<_, InvalidState> { <PhysicalKey as State>::load(v) })
                .transpose()?,
            deadline: w.deadline,
        };
        if !((1..=3).contains(&value.scan_set) && value.repeat.is_none_or(|key| key.0 < 256)) {
            return Err(InvalidState("AtKeyboard invariants"));
        }
        Ok(value)
    }
}
