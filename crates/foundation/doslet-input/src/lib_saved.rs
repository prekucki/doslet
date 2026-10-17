// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

fn index(value: u32, bound: u32, field: &'static str) -> Result<u8, InvalidState> {
    if value >= bound {
        return Err(InvalidState(field));
    }
    doslet_state::narrow(value, field)
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputPhysicalKey {
    #[prost(uint32, tag = "1")]
    value: u32,
}
impl State for PhysicalKey {
    type Wire = InputPhysicalKey;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            value: u32::from(self.0),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self(doslet_state::narrow(w.value, "InputPhysicalKey.0")?);
        if !(value.0 < 256) {
            return Err(InvalidState("InputPhysicalKey invariants"));
        }
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputInputSourceId {
    #[prost(uint32, tag = "1")]
    value: u32,
}
impl State for InputSourceId {
    type Wire = InputInputSourceId;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            value: u32::from(self.0),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self(doslet_state::narrow(w.value, "InputInputSourceId.0")?);
        if !(usize::from(value.0) < MAX_SOURCES) {
            return Err(InvalidState("InputInputSourceId invariants"));
        }
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputControlKey {
    #[prost(message, optional, tag = "1")]
    value0: Option<<PhysicalKey as State>::Wire>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputControlButton {
    #[prost(uint32, tag = "1")]
    value0: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputControlAxis {
    #[prost(uint32, tag = "1")]
    value0: u32,
}

#[derive(Clone, PartialEq, prost::Oneof)]
enum InputControlKind {
    #[prost(message, tag = "1")]
    Key(InputControlKey),
    #[prost(message, tag = "2")]
    Button(InputControlButton),
    #[prost(message, tag = "3")]
    Axis(InputControlAxis),
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct InputControl {
    #[prost(oneof = "InputControlKind", tags = "1,2,3")]
    kind: Option<InputControlKind>,
}
impl State for Control {
    type Wire = InputControl;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            kind: Some(match self {
                Self::Key(value0) => InputControlKind::Key(InputControlKey {
                    value0: Some(State::save(value0)),
                }),
                Self::Button(value0) => InputControlKind::Button(InputControlButton {
                    value0: u32::from(*(value0)),
                }),
                Self::Axis(value0) => InputControlKind::Axis(InputControlAxis {
                    value0: u32::from(*(value0)),
                }),
            }),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        Ok(match doslet_state::required(w.kind, "InputControl kind")? {
            InputControlKind::Key(v) => Self::Key(<PhysicalKey as State>::load(
                doslet_state::required(v.value0, "InputControlKey.value0")?,
            )?),
            InputControlKind::Button(v) => {
                Self::Button(index(v.value0, 32, "InputControlButton.value0")?)
            }
            InputControlKind::Axis(v) => {
                Self::Axis(index(v.value0, 16, "InputControlAxis.value0")?)
            }
        })
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputEndpointKey {
    #[prost(message, optional, tag = "1")]
    value0: Option<<PhysicalKey as State>::Wire>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputEndpointButton {
    #[prost(uint32, tag = "1")]
    value0: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputEndpointAxis {
    #[prost(uint32, tag = "1")]
    value0: u32,
}

#[derive(Clone, PartialEq, prost::Oneof)]
enum InputEndpointKind {
    #[prost(message, tag = "1")]
    Key(InputEndpointKey),
    #[prost(message, tag = "2")]
    Button(InputEndpointButton),
    #[prost(message, tag = "3")]
    Axis(InputEndpointAxis),
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct InputEndpoint {
    #[prost(oneof = "InputEndpointKind", tags = "1,2,3")]
    kind: Option<InputEndpointKind>,
}
impl State for Endpoint {
    type Wire = InputEndpoint;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            kind: Some(match self {
                Self::Key(value0) => InputEndpointKind::Key(InputEndpointKey {
                    value0: Some(State::save(value0)),
                }),
                Self::Button(value0) => InputEndpointKind::Button(InputEndpointButton {
                    value0: u32::from(*(value0)),
                }),
                Self::Axis(value0) => InputEndpointKind::Axis(InputEndpointAxis {
                    value0: u32::from(*(value0)),
                }),
            }),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        Ok(
            match doslet_state::required(w.kind, "InputEndpoint kind")? {
                InputEndpointKind::Key(v) => Self::Key(<PhysicalKey as State>::load(
                    doslet_state::required(v.value0, "InputEndpointKey.value0")?,
                )?),
                InputEndpointKind::Button(v) => {
                    Self::Button(index(v.value0, 2, "InputEndpointButton.value0")?)
                }
                InputEndpointKind::Axis(v) => {
                    Self::Axis(index(v.value0, 2, "InputEndpointAxis.value0")?)
                }
            },
        )
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputBinding {
    #[prost(message, optional, tag = "1")]
    source: Option<<InputSourceId as State>::Wire>,
    #[prost(message, optional, tag = "2")]
    control: Option<<Control as State>::Wire>,
    #[prost(message, optional, tag = "3")]
    target: Option<<Endpoint as State>::Wire>,
    #[prost(sint32, tag = "4")]
    scale: i32,
}
impl State for Binding {
    type Wire = InputBinding;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            source: Some(State::save(&self.source)),
            control: Some(State::save(&self.control)),
            target: Some(State::save(&self.target)),
            scale: i32::from(self.scale),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            source: <InputSourceId as State>::load(doslet_state::required(
                w.source,
                "InputBinding.source",
            )?)?,
            control: <Control as State>::load(doslet_state::required(
                w.control,
                "InputBinding.control",
            )?)?,
            target: <Endpoint as State>::load(doslet_state::required(
                w.target,
                "InputBinding.target",
            )?)?,
            scale: doslet_state::narrow(w.scale, "InputBinding.scale")?,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputGuestEventKey {
    #[prost(message, optional, tag = "1")]
    key: Option<<PhysicalKey as State>::Wire>,
    #[prost(bool, tag = "2")]
    pressed: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputGuestEventButton {
    #[prost(uint32, tag = "1")]
    button: u32,
    #[prost(bool, tag = "2")]
    pressed: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputGuestEventAxis {
    #[prost(uint32, tag = "1")]
    axis: u32,
    #[prost(sint32, tag = "2")]
    value: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputGuestEventReleaseAll {}

#[derive(Clone, PartialEq, prost::Oneof)]
enum InputGuestEventKind {
    #[prost(message, tag = "1")]
    Key(InputGuestEventKey),
    #[prost(message, tag = "2")]
    Button(InputGuestEventButton),
    #[prost(message, tag = "3")]
    Axis(InputGuestEventAxis),
    #[prost(message, tag = "4")]
    ReleaseAll(InputGuestEventReleaseAll),
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct InputGuestEvent {
    #[prost(oneof = "InputGuestEventKind", tags = "1,2,3,4")]
    kind: Option<InputGuestEventKind>,
}
impl State for GuestEvent {
    type Wire = InputGuestEvent;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            kind: Some(match self {
                Self::Key { key, pressed } => InputGuestEventKind::Key(InputGuestEventKey {
                    key: Some(State::save(key)),
                    pressed: *(pressed),
                }),
                Self::Button { button, pressed } => {
                    InputGuestEventKind::Button(InputGuestEventButton {
                        button: u32::from(*(button)),
                        pressed: *(pressed),
                    })
                }
                Self::Axis { axis, value } => InputGuestEventKind::Axis(InputGuestEventAxis {
                    axis: u32::from(*(axis)),
                    value: i32::from(*(value)),
                }),
                Self::ReleaseAll => InputGuestEventKind::ReleaseAll(InputGuestEventReleaseAll {}),
            }),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        Ok(
            match doslet_state::required(w.kind, "InputGuestEvent kind")? {
                InputGuestEventKind::Key(v) => Self::Key {
                    key: <PhysicalKey as State>::load(doslet_state::required(
                        v.key,
                        "InputGuestEventKey.key",
                    )?)?,
                    pressed: v.pressed,
                },
                InputGuestEventKind::Button(v) => Self::Button {
                    button: index(v.button, 2, "InputGuestEventButton.button")?,
                    pressed: v.pressed,
                },
                InputGuestEventKind::Axis(v) => Self::Axis {
                    axis: index(v.axis, 2, "InputGuestEventAxis.axis")?,
                    value: doslet_state::narrow(v.value, "InputGuestEventAxis.value")?,
                },
                InputGuestEventKind::ReleaseAll(_v) => Self::ReleaseAll,
            },
        )
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputSource {
    #[prost(uint64, tag = "1")]
    generation: u64,
    #[prost(uint64, tag = "2")]
    sequence: u64,
    #[prost(bool, tag = "3")]
    connected: bool,
    #[prost(bool, tag = "4")]
    seen: bool,
    #[prost(bool, tag = "5")]
    registered: bool,
    #[prost(uint64, repeated, tag = "6")]
    keys: Vec<u64>,
    #[prost(uint32, tag = "7")]
    buttons: u32,
    #[prost(sint32, repeated, tag = "8")]
    axes: Vec<i32>,
    #[prost(bool, tag = "9")]
    keyboard: bool,
    #[prost(bool, tag = "10")]
    joystick: bool,
    #[prost(uint64, repeated, tag = "11")]
    suppressed_keys: Vec<u64>,
    #[prost(uint32, tag = "12")]
    suppressed_buttons: u32,
    #[prost(bool, repeated, tag = "13")]
    suppressed_axes: Vec<bool>,
}
impl State for Source {
    type Wire = InputSource;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            generation: self.generation,
            sequence: self.sequence,
            connected: self.connected,
            seen: self.seen,
            registered: self.registered,
            keys: self.keys.to_vec(),
            buttons: self.buttons,
            axes: self.axes.iter().map(|v| i32::from(*(v))).collect(),
            keyboard: self.keyboard,
            joystick: self.joystick,
            suppressed_keys: self.suppressed_keys.to_vec(),
            suppressed_buttons: self.suppressed_buttons,
            suppressed_axes: self.suppressed_axes.to_vec(),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            generation: w.generation,
            sequence: w.sequence,
            connected: w.connected,
            seen: w.seen,
            registered: w.registered,
            keys: doslet_state::narrow(
                w.keys
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "InputSource.keys length",
            )?,
            buttons: w.buttons,
            axes: doslet_state::narrow(
                w.axes
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        doslet_state::narrow(v, "InputSource.axes")
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "InputSource.axes length",
            )?,
            keyboard: w.keyboard,
            joystick: w.joystick,
            suppressed_keys: doslet_state::narrow(
                w.suppressed_keys
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "InputSource.suppressed_keys length",
            )?,
            suppressed_buttons: w.suppressed_buttons,
            suppressed_axes: doslet_state::narrow(
                w.suppressed_axes
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "InputSource.suppressed_axes length",
            )?,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputInputRouterBindingsItem {
    #[prost(message, optional, tag = "1")]
    value: Option<<Binding as State>::Wire>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct InputInputRouter {
    #[prost(uint64, tag = "1")]
    epoch: u64,
    #[prost(message, repeated, tag = "2")]
    sources: Vec<<Source as State>::Wire>,
    #[prost(message, repeated, tag = "3")]
    bindings: Vec<InputInputRouterBindingsItem>,
    #[prost(uint64, tag = "4")]
    count: u64,
    #[prost(sint32, repeated, tag = "5")]
    values: Vec<i32>,
    #[prost(bool, repeated, tag = "6")]
    held: Vec<bool>,
    #[prost(bool, repeated, tag = "7")]
    suppressed: Vec<bool>,
    #[prost(message, repeated, tag = "8")]
    edges: Vec<<GuestEvent as State>::Wire>,
    #[prost(uint64, tag = "9")]
    head: u64,
    #[prost(uint64, tag = "10")]
    len: u64,
    #[prost(bool, tag = "11")]
    release_latch: bool,
}
impl State for InputRouter {
    type Wire = InputInputRouter;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            epoch: self.epoch,
            sources: self.sources.iter().map(State::save).collect(),
            bindings: self
                .bindings
                .iter()
                .map(|v| InputInputRouterBindingsItem {
                    value: (v).as_ref().map(State::save),
                })
                .collect(),
            count: self.count as u64,
            values: self.values.iter().map(|v| i32::from(*(v))).collect(),
            held: self.held.to_vec(),
            suppressed: self.suppressed.to_vec(),
            edges: self.edges.iter().map(State::save).collect(),
            head: self.head as u64,
            len: self.len as u64,
            release_latch: self.release_latch,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            epoch: w.epoch,
            sources: doslet_state::narrow(
                w.sources
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { <Source as State>::load(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "InputInputRouter.sources length",
            )?,
            bindings: doslet_state::narrow(
                w.bindings
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        v.value
                            .map(|v| -> Result<_, InvalidState> { <Binding as State>::load(v) })
                            .transpose()
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "InputInputRouter.bindings length",
            )?,
            count: doslet_state::narrow(w.count, "InputInputRouter.count")?,
            values: doslet_state::narrow(
                w.values
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        doslet_state::narrow(v, "InputInputRouter.values")
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "InputInputRouter.values length",
            )?,
            held: doslet_state::narrow(
                w.held
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "InputInputRouter.held length",
            )?,
            suppressed: doslet_state::narrow(
                w.suppressed
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "InputInputRouter.suppressed length",
            )?,
            edges: doslet_state::narrow(
                w.edges
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { <GuestEvent as State>::load(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "InputInputRouter.edges length",
            )?,
            head: doslet_state::narrow(w.head, "InputInputRouter.head")?,
            len: doslet_state::narrow(w.len, "InputInputRouter.len")?,
            release_latch: w.release_latch,
        };
        if !(value.count <= MAX_BINDINGS && value.head < MAX_EDGES && value.len <= MAX_EDGES) {
            return Err(InvalidState("InputInputRouter invariants"));
        }
        Ok(value)
    }
}
