// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct GameportGameport {
    #[prost(sint32, repeated, tag = "1")]
    axes: Vec<i32>,
    #[prost(bool, repeated, tag = "2")]
    buttons: Vec<bool>,
    #[prost(uint64, repeated, tag = "3")]
    deadlines: Vec<u64>,
}
impl State for Gameport {
    type Wire = GameportGameport;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            axes: self.axes.iter().map(|v| i32::from(*(v))).collect(),
            buttons: self.buttons.to_vec(),
            deadlines: self.deadlines.to_vec(),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            axes: doslet_state::narrow(
                w.axes
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        doslet_state::narrow(v, "GameportGameport.axes")
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "GameportGameport.axes length",
            )?,
            buttons: doslet_state::narrow(
                w.buttons
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "GameportGameport.buttons length",
            )?,
            deadlines: doslet_state::narrow(
                w.deadlines
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "GameportGameport.deadlines length",
            )?,
        };
        Ok(value)
    }
}
