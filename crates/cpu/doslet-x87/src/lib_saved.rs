// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct X87X87FiniteItem {
    #[prost(message, optional, tag = "1")]
    value: Option<<Finite as State>::Wire>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct X87X87 {
    #[prost(uint32, tag = "1")]
    control: u32,
    #[prost(uint32, tag = "2")]
    status: u32,
    #[prost(uint32, tag = "3")]
    tags: u32,
    #[prost(double, repeated, tag = "4")]
    values: Vec<f64>,
    #[prost(message, repeated, tag = "5")]
    finite: Vec<X87X87FiniteItem>,
    #[prost(bytes = "vec", repeated, tag = "6")]
    encodings: Vec<Vec<u8>>,
    #[prost(message, optional, tag = "7")]
    pointers: Option<<environment::Pointers as State>::Wire>,
}
impl State for X87 {
    type Wire = X87X87;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            control: u32::from(self.control),
            status: u32::from(self.status),
            tags: u32::from(self.tags),
            values: self.values.to_vec(),
            finite: self
                .finite
                .iter()
                .map(|v| X87X87FiniteItem {
                    value: (v).as_ref().map(State::save),
                })
                .collect(),
            encodings: self
                .encodings
                .iter()
                .map(|v| (v).to_le_bytes().to_vec())
                .collect(),
            pointers: Some(State::save(&self.pointers)),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            control: doslet_state::narrow(w.control, "X87X87.control")?,
            status: doslet_state::narrow(w.status, "X87X87.status")?,
            tags: doslet_state::narrow(w.tags, "X87X87.tags")?,
            values: doslet_state::narrow(
                w.values
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "X87X87.values length",
            )?,
            finite: doslet_state::narrow(
                w.finite
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        v.value
                            .map(|v| -> Result<_, InvalidState> { <Finite as State>::load(v) })
                            .transpose()
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "X87X87.finite length",
            )?,
            encodings: doslet_state::narrow(
                w.encodings
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        Ok(u128::from_le_bytes(doslet_state::narrow(
                            v,
                            "X87X87.encodings",
                        )?))
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "X87X87.encodings length",
            )?,
            pointers: <environment::Pointers as State>::load(doslet_state::required(
                w.pointers,
                "X87X87.pointers",
            )?)?,
        };
        Ok(value)
    }
}
