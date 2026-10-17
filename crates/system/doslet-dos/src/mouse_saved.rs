// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct DosButtonHistory {
    #[prost(uint32, tag = "1")]
    count: u32,
    #[prost(uint32, tag = "2")]
    x: u32,
    #[prost(uint32, tag = "3")]
    y: u32,
}
impl State for ButtonHistory {
    type Wire = DosButtonHistory;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            count: u32::from(self.count),
            x: u32::from(self.x),
            y: u32::from(self.y),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            count: doslet_state::narrow(w.count, "DosButtonHistory.count")?,
            x: doslet_state::narrow(w.x, "DosButtonHistory.x")?,
            y: doslet_state::narrow(w.y, "DosButtonHistory.y")?,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DosMouseXRangeTuple {
    #[prost(uint32, tag = "1")]
    item0: u32,
    #[prost(uint32, tag = "2")]
    item1: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DosMouseYRangeTuple {
    #[prost(uint32, tag = "1")]
    item0: u32,
    #[prost(uint32, tag = "2")]
    item1: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DosMouseMotionTuple {
    #[prost(sint32, tag = "1")]
    item0: i32,
    #[prost(sint32, tag = "2")]
    item1: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DosMouseMickeyRatioTuple {
    #[prost(uint32, tag = "1")]
    item0: u32,
    #[prost(uint32, tag = "2")]
    item1: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DosMouseHotspotTuple {
    #[prost(sint32, tag = "1")]
    item0: i32,
    #[prost(sint32, tag = "2")]
    item1: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DosMouseCallbackTuple {
    #[prost(uint32, tag = "1")]
    item0: u32,
    #[prost(uint32, tag = "2")]
    item1: u32,
    #[prost(uint32, tag = "3")]
    item2: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct DosMouse {
    #[prost(uint32, tag = "1")]
    x: u32,
    #[prost(uint32, tag = "2")]
    y: u32,
    #[prost(uint32, tag = "3")]
    buttons: u32,
    #[prost(message, optional, tag = "4")]
    x_range: Option<DosMouseXRangeTuple>,
    #[prost(message, optional, tag = "5")]
    y_range: Option<DosMouseYRangeTuple>,
    #[prost(sint32, tag = "6")]
    visible: i32,
    #[prost(message, optional, tag = "7")]
    motion: Option<DosMouseMotionTuple>,
    #[prost(message, optional, tag = "8")]
    mickey_ratio: Option<DosMouseMickeyRatioTuple>,
    #[prost(uint32, tag = "9")]
    double_speed_threshold: u32,
    #[prost(message, repeated, tag = "10")]
    presses: Vec<<ButtonHistory as State>::Wire>,
    #[prost(message, repeated, tag = "11")]
    releases: Vec<<ButtonHistory as State>::Wire>,
    #[prost(uint32, repeated, tag = "12")]
    masks: Vec<u32>,
    #[prost(message, optional, tag = "13")]
    hotspot: Option<DosMouseHotspotTuple>,
    #[prost(uint64, tag = "14")]
    revision: u64,
    #[prost(message, optional, tag = "15")]
    callback: Option<DosMouseCallbackTuple>,
    #[prost(uint32, tag = "16")]
    pending: u32,
    #[prost(bool, tag = "17")]
    callback_active: bool,
}
impl State for Mouse {
    type Wire = DosMouse;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            x: u32::from(self.x),
            y: u32::from(self.y),
            buttons: u32::from(self.buttons),
            x_range: Some(DosMouseXRangeTuple {
                item0: u32::from(self.x_range.0),
                item1: u32::from(self.x_range.1),
            }),
            y_range: Some(DosMouseYRangeTuple {
                item0: u32::from(self.y_range.0),
                item1: u32::from(self.y_range.1),
            }),
            visible: i32::from(self.visible),
            motion: Some(DosMouseMotionTuple {
                item0: i32::from(self.motion.0),
                item1: i32::from(self.motion.1),
            }),
            mickey_ratio: Some(DosMouseMickeyRatioTuple {
                item0: u32::from(self.mickey_ratio.0),
                item1: u32::from(self.mickey_ratio.1),
            }),
            double_speed_threshold: u32::from(self.double_speed_threshold),
            presses: self.presses.iter().map(State::save).collect(),
            releases: self.releases.iter().map(State::save).collect(),
            masks: self.masks.iter().map(|v| u32::from(*(v))).collect(),
            hotspot: Some(DosMouseHotspotTuple {
                item0: i32::from(self.hotspot.0),
                item1: i32::from(self.hotspot.1),
            }),
            revision: self.revision,
            callback: Some(DosMouseCallbackTuple {
                item0: u32::from(self.callback.0),
                item1: u32::from(self.callback.1),
                item2: u32::from(self.callback.2),
            }),
            pending: u32::from(self.pending),
            callback_active: self.callback_active,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            x: doslet_state::narrow(w.x, "DosMouse.x")?,
            y: doslet_state::narrow(w.y, "DosMouse.y")?,
            buttons: doslet_state::narrow(w.buttons, "DosMouse.buttons")?,
            x_range: {
                let v = doslet_state::required(w.x_range, "DosMouse.x_range")?;
                (
                    doslet_state::narrow(v.item0, "DosMouse.x_range.item0")?,
                    doslet_state::narrow(v.item1, "DosMouse.x_range.item1")?,
                )
            },
            y_range: {
                let v = doslet_state::required(w.y_range, "DosMouse.y_range")?;
                (
                    doslet_state::narrow(v.item0, "DosMouse.y_range.item0")?,
                    doslet_state::narrow(v.item1, "DosMouse.y_range.item1")?,
                )
            },
            visible: doslet_state::narrow(w.visible, "DosMouse.visible")?,
            motion: {
                let v = doslet_state::required(w.motion, "DosMouse.motion")?;
                (
                    doslet_state::narrow(v.item0, "DosMouse.motion.item0")?,
                    doslet_state::narrow(v.item1, "DosMouse.motion.item1")?,
                )
            },
            mickey_ratio: {
                let v = doslet_state::required(w.mickey_ratio, "DosMouse.mickey_ratio")?;
                (
                    doslet_state::narrow(v.item0, "DosMouse.mickey_ratio.item0")?,
                    doslet_state::narrow(v.item1, "DosMouse.mickey_ratio.item1")?,
                )
            },
            double_speed_threshold: doslet_state::narrow(
                w.double_speed_threshold,
                "DosMouse.double_speed_threshold",
            )?,
            presses: doslet_state::narrow(
                w.presses
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { <ButtonHistory as State>::load(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "DosMouse.presses length",
            )?,
            releases: doslet_state::narrow(
                w.releases
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { <ButtonHistory as State>::load(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "DosMouse.releases length",
            )?,
            masks: doslet_state::narrow(
                w.masks
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        doslet_state::narrow(v, "DosMouse.masks")
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "DosMouse.masks length",
            )?,
            hotspot: {
                let v = doslet_state::required(w.hotspot, "DosMouse.hotspot")?;
                (
                    doslet_state::narrow(v.item0, "DosMouse.hotspot.item0")?,
                    doslet_state::narrow(v.item1, "DosMouse.hotspot.item1")?,
                )
            },
            revision: w.revision,
            callback: {
                let v = doslet_state::required(w.callback, "DosMouse.callback")?;
                (
                    doslet_state::narrow(v.item0, "DosMouse.callback.item0")?,
                    doslet_state::narrow(v.item1, "DosMouse.callback.item1")?,
                    doslet_state::narrow(v.item2, "DosMouse.callback.item2")?,
                )
            },
            pending: doslet_state::narrow(w.pending, "DosMouse.pending")?,
            callback_active: w.callback_active,
        };
        if !(value.x_range.0 <= value.x_range.1 && value.y_range.0 <= value.y_range.1) {
            return Err(InvalidState("DosMouse invariants"));
        }
        Ok(value)
    }
}
