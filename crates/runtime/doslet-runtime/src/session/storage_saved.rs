// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeCopyTargetSegment {
    #[prost(uint32, tag = "1")]
    value0: u32,
    #[prost(uint32, tag = "2")]
    value1: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeCopyTargetLinear {
    #[prost(uint32, tag = "1")]
    value0: u32,
}

#[derive(Clone, PartialEq, prost::Oneof)]
enum RuntimeCopyTargetKind {
    #[prost(message, tag = "1")]
    Segment(RuntimeCopyTargetSegment),
    #[prost(message, tag = "2")]
    Linear(RuntimeCopyTargetLinear),
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeCopyTarget {
    #[prost(oneof = "RuntimeCopyTargetKind", tags = "1,2")]
    kind: Option<RuntimeCopyTargetKind>,
}
impl State for CopyTarget {
    type Wire = RuntimeCopyTarget;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            kind: Some(match self {
                Self::Segment(value0, value1) => {
                    RuntimeCopyTargetKind::Segment(RuntimeCopyTargetSegment {
                        value0: u32::from(*(value0)),
                        value1: u32::from(*(value1)),
                    })
                }
                Self::Linear(value0) => {
                    RuntimeCopyTargetKind::Linear(RuntimeCopyTargetLinear { value0: *(value0) })
                }
            }),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        Ok(
            match doslet_state::required(w.kind, "RuntimeCopyTarget kind")? {
                RuntimeCopyTargetKind::Segment(v) => Self::Segment(
                    doslet_state::narrow(v.value0, "RuntimeCopyTargetSegment.value0")?,
                    doslet_state::narrow(v.value1, "RuntimeCopyTargetSegment.value1")?,
                ),
                RuntimeCopyTargetKind::Linear(v) => Self::Linear(v.value0),
            },
        )
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeContinuationRawStage {
    #[prost(uint64, tag = "1")]
    offset: u64,
    #[prost(message, optional, tag = "2")]
    target: Option<<CopyTarget as State>::Wire>,
    #[prost(uint64, tag = "3")]
    length: u64,
    #[prost(uint64, tag = "4")]
    done: u64,
    #[prost(uint32, tag = "5")]
    ax: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeContinuationWriteStage {
    #[prost(uint32, tag = "1")]
    handle: u32,
    #[prost(uint32, tag = "2")]
    segment: u32,
    #[prost(uint32, tag = "3")]
    offset: u32,
    #[prost(uint64, tag = "4")]
    length: u64,
    #[prost(uint64, tag = "5")]
    done: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeContinuationReadStage {
    #[prost(uint32, tag = "1")]
    handle: u32,
    #[prost(uint32, tag = "2")]
    position: u32,
    #[prost(message, optional, tag = "3")]
    target: Option<<CopyTarget as State>::Wire>,
    #[prost(uint64, tag = "4")]
    length: u64,
    #[prost(uint64, tag = "5")]
    done: u64,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeContinuationResolve {
    #[prost(uint32, tag = "1")]
    value0: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeContinuationCopy {
    #[prost(message, optional, tag = "1")]
    target: Option<<CopyTarget as State>::Wire>,
    #[prost(uint64, tag = "2")]
    length: u64,
    #[prost(uint64, tag = "3")]
    copied: u64,
    #[prost(uint32, tag = "4")]
    ax: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeContinuationCommit {}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeContinuationHostCommit {}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeContinuationExitCommit {
    #[prost(uint32, tag = "1")]
    value0: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeContinuationReturn {}

#[derive(Clone, PartialEq, prost::Oneof)]
enum RuntimeContinuationKind {
    #[prost(message, tag = "1")]
    RawStage(RuntimeContinuationRawStage),
    #[prost(message, tag = "2")]
    WriteStage(RuntimeContinuationWriteStage),
    #[prost(message, tag = "3")]
    ReadStage(RuntimeContinuationReadStage),
    #[prost(message, tag = "4")]
    Resolve(RuntimeContinuationResolve),
    #[prost(message, tag = "5")]
    Copy(RuntimeContinuationCopy),
    #[prost(message, tag = "6")]
    Commit(RuntimeContinuationCommit),
    #[prost(message, tag = "7")]
    HostCommit(RuntimeContinuationHostCommit),
    #[prost(message, tag = "8")]
    ExitCommit(RuntimeContinuationExitCommit),
    #[prost(message, tag = "9")]
    Return(RuntimeContinuationReturn),
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeContinuation {
    #[prost(oneof = "RuntimeContinuationKind", tags = "1,2,3,4,5,6,7,8,9")]
    kind: Option<RuntimeContinuationKind>,
}
impl State for Continuation {
    type Wire = RuntimeContinuation;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            kind: Some(match self {
                Self::RawStage {
                    offset,
                    target,
                    length,
                    done,
                    ax,
                } => RuntimeContinuationKind::RawStage(RuntimeContinuationRawStage {
                    offset: *(offset),
                    target: Some(State::save(target)),
                    length: *(length) as u64,
                    done: *(done) as u64,
                    ax: u32::from(*(ax)),
                }),
                Self::WriteStage {
                    handle,
                    segment,
                    offset,
                    length,
                    done,
                } => RuntimeContinuationKind::WriteStage(RuntimeContinuationWriteStage {
                    handle: u32::from(*(handle)),
                    segment: u32::from(*(segment)),
                    offset: u32::from(*(offset)),
                    length: *(length) as u64,
                    done: *(done) as u64,
                }),
                Self::ReadStage {
                    handle,
                    position,
                    target,
                    length,
                    done,
                } => RuntimeContinuationKind::ReadStage(RuntimeContinuationReadStage {
                    handle: u32::from(*(handle)),
                    position: *(position),
                    target: Some(State::save(target)),
                    length: *(length) as u64,
                    done: *(done) as u64,
                }),
                Self::Resolve(value0) => {
                    RuntimeContinuationKind::Resolve(RuntimeContinuationResolve {
                        value0: u32::from(*(value0)),
                    })
                }
                Self::Copy {
                    target,
                    length,
                    copied,
                    ax,
                } => RuntimeContinuationKind::Copy(RuntimeContinuationCopy {
                    target: Some(State::save(target)),
                    length: *(length) as u64,
                    copied: *(copied) as u64,
                    ax: u32::from(*(ax)),
                }),
                Self::Commit => RuntimeContinuationKind::Commit(RuntimeContinuationCommit {}),
                Self::HostCommit => {
                    RuntimeContinuationKind::HostCommit(RuntimeContinuationHostCommit {})
                }
                Self::ExitCommit(value0) => {
                    RuntimeContinuationKind::ExitCommit(RuntimeContinuationExitCommit {
                        value0: u32::from(*(value0)),
                    })
                }
                Self::Return => RuntimeContinuationKind::Return(RuntimeContinuationReturn {}),
            }),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        Ok(
            match doslet_state::required(w.kind, "RuntimeContinuation kind")? {
                RuntimeContinuationKind::RawStage(v) => Self::RawStage {
                    offset: v.offset,
                    target: <CopyTarget as State>::load(doslet_state::required(
                        v.target,
                        "RuntimeContinuationRawStage.target",
                    )?)?,
                    length: doslet_state::narrow(v.length, "RuntimeContinuationRawStage.length")?,
                    done: doslet_state::narrow(v.done, "RuntimeContinuationRawStage.done")?,
                    ax: doslet_state::narrow(v.ax, "RuntimeContinuationRawStage.ax")?,
                },
                RuntimeContinuationKind::WriteStage(v) => Self::WriteStage {
                    handle: doslet_state::narrow(v.handle, "RuntimeContinuationWriteStage.handle")?,
                    segment: doslet_state::narrow(
                        v.segment,
                        "RuntimeContinuationWriteStage.segment",
                    )?,
                    offset: doslet_state::narrow(v.offset, "RuntimeContinuationWriteStage.offset")?,
                    length: doslet_state::narrow(v.length, "RuntimeContinuationWriteStage.length")?,
                    done: doslet_state::narrow(v.done, "RuntimeContinuationWriteStage.done")?,
                },
                RuntimeContinuationKind::ReadStage(v) => Self::ReadStage {
                    handle: doslet_state::narrow(v.handle, "RuntimeContinuationReadStage.handle")?,
                    position: v.position,
                    target: <CopyTarget as State>::load(doslet_state::required(
                        v.target,
                        "RuntimeContinuationReadStage.target",
                    )?)?,
                    length: doslet_state::narrow(v.length, "RuntimeContinuationReadStage.length")?,
                    done: doslet_state::narrow(v.done, "RuntimeContinuationReadStage.done")?,
                },
                RuntimeContinuationKind::Resolve(v) => Self::Resolve(doslet_state::narrow(
                    v.value0,
                    "RuntimeContinuationResolve.value0",
                )?),
                RuntimeContinuationKind::Copy(v) => Self::Copy {
                    target: <CopyTarget as State>::load(doslet_state::required(
                        v.target,
                        "RuntimeContinuationCopy.target",
                    )?)?,
                    length: doslet_state::narrow(v.length, "RuntimeContinuationCopy.length")?,
                    copied: doslet_state::narrow(v.copied, "RuntimeContinuationCopy.copied")?,
                    ax: doslet_state::narrow(v.ax, "RuntimeContinuationCopy.ax")?,
                },
                RuntimeContinuationKind::Commit(_v) => Self::Commit,
                RuntimeContinuationKind::HostCommit(_v) => Self::HostCommit,
                RuntimeContinuationKind::ExitCommit(v) => Self::ExitCommit(doslet_state::narrow(
                    v.value0,
                    "RuntimeContinuationExitCommit.value0",
                )?),
                RuntimeContinuationKind::Return(_v) => Self::Return,
            },
        )
    }
}
