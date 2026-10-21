// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeSessionStatusReady {}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeSessionStatusRunning {}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeSessionStatusPaused {}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeSessionStatusExited {
    #[prost(uint32, tag = "1")]
    value0: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeSessionStatusHalted {}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeSessionStatusFaulted {}

#[derive(Clone, PartialEq, prost::Oneof)]
enum RuntimeSessionStatusKind {
    #[prost(message, tag = "1")]
    Ready(RuntimeSessionStatusReady),
    #[prost(message, tag = "2")]
    Running(RuntimeSessionStatusRunning),
    #[prost(message, tag = "3")]
    Paused(RuntimeSessionStatusPaused),
    #[prost(message, tag = "4")]
    Exited(RuntimeSessionStatusExited),
    #[prost(message, tag = "5")]
    Halted(RuntimeSessionStatusHalted),
    #[prost(message, tag = "6")]
    Faulted(RuntimeSessionStatusFaulted),
}
#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeSessionStatus {
    #[prost(oneof = "RuntimeSessionStatusKind", tags = "1,2,3,4,5,6")]
    kind: Option<RuntimeSessionStatusKind>,
}
impl State for SessionStatus {
    type Wire = RuntimeSessionStatus;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            kind: Some(match self {
                Self::Ready => RuntimeSessionStatusKind::Ready(RuntimeSessionStatusReady {}),
                Self::Running => RuntimeSessionStatusKind::Running(RuntimeSessionStatusRunning {}),
                Self::Paused => RuntimeSessionStatusKind::Paused(RuntimeSessionStatusPaused {}),
                Self::Exited(value0) => {
                    RuntimeSessionStatusKind::Exited(RuntimeSessionStatusExited {
                        value0: u32::from(*(value0)),
                    })
                }
                Self::Halted => RuntimeSessionStatusKind::Halted(RuntimeSessionStatusHalted {}),
                Self::Faulted => RuntimeSessionStatusKind::Faulted(RuntimeSessionStatusFaulted {}),
            }),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        Ok(
            match doslet_state::required(w.kind, "RuntimeSessionStatus kind")? {
                RuntimeSessionStatusKind::Ready(_v) => Self::Ready,
                RuntimeSessionStatusKind::Running(_v) => Self::Running,
                RuntimeSessionStatusKind::Paused(_v) => Self::Paused,
                RuntimeSessionStatusKind::Exited(v) => Self::Exited(doslet_state::narrow(
                    v.value0,
                    "RuntimeSessionStatusExited.value0",
                )?),
                RuntimeSessionStatusKind::Halted(_v) => Self::Halted,
                RuntimeSessionStatusKind::Faulted(_v) => Self::Faulted,
            },
        )
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeSessionDtaTuple {
    #[prost(uint32, tag = "1")]
    item0: u32,
    #[prost(uint32, tag = "2")]
    item1: u32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeSession {
    #[prost(message, optional, tag = "1")]
    machine: Option<<PcMachine as State>::Wire>,
    #[prost(message, optional, tag = "2")]
    status: Option<<SessionStatus as State>::Wire>,
    #[prost(uint64, tag = "4")]
    steps: u64,
    #[prost(bytes = "vec", tag = "5")]
    console: Vec<u8>,
    #[prost(bool, tag = "6")]
    console_truncated: bool,
    #[prost(uint32, optional, tag = "7")]
    extended_key: Option<u32>,
    #[prost(message, optional, tag = "8")]
    mouse: Option<<doslet_dos::mouse::Mouse as State>::Wire>,
    #[prost(double, repeated, tag = "9")]
    mouse_fraction: Vec<f64>,
    #[prost(uint64, optional, tag = "10")]
    mouse_last_read: Option<u64>,
    #[prost(uint64, tag = "11")]
    dos_time_origin_nanos: u64,
    #[prost(uint32, tag = "12")]
    dos_time_origin_centis: u32,
    #[prost(message, optional, tag = "13")]
    memory: Option<<MemoryArena as State>::Wire>,
    #[prost(message, optional, tag = "14")]
    drive: Option<<ReadOnlyDrive as State>::Wire>,
    #[prost(message, optional, tag = "15")]
    dta: Option<RuntimeSessionDtaTuple>,
    #[prost(uint32, tag = "16")]
    overlays: u32,
    #[prost(bool, tag = "17")]
    break_check: bool,
    #[prost(message, optional, tag = "18")]
    cdrom: Option<<CdromImage as State>::Wire>,
    #[prost(message, optional, tag = "19")]
    handles: Option<<HandleTable as State>::Wire>,
    #[prost(bytes = "vec", tag = "20")]
    staging: Vec<u8>,
    #[prost(message, optional, tag = "21")]
    continuation: Option<<storage::Continuation as State>::Wire>,
    #[prost(bool, tag = "22")]
    storage_wait: bool,
    #[prost(bool, tag = "23")]
    timeslice_yielded: bool,
    #[prost(uint64, tag = "24")]
    epoch: u64,
    #[prost(uint32, tag = "25")]
    active_service: u32,
    #[prost(bytes = "vec", tag = "27")]
    base_identity: Vec<u8>,
    #[prost(bool, tag = "28")]
    keyboard_prefix: bool,
    #[prost(message, optional, tag = "29")]
    input: Option<<doslet_input::InputRouter as State>::Wire>,
    #[prost(sint32, repeated, tag = "30")]
    joystick_axes: Vec<i32>,
    #[prost(bool, repeated, tag = "31")]
    joystick_buttons: Vec<bool>,
    #[prost(uint64, optional, tag = "32")]
    mouse_last_relative_read: Option<u64>,
    #[prost(message, repeated, tag = "33")]
    patches: Vec<patches::MemoryPatch>,
    #[prost(uint32, tag = "34")]
    patches_applied: u32,
    /// The runtime DPMI host's own records; its tables live in RAM.
    #[prost(message, optional, tag = "35")]
    dpmi: Option<<dpmi::Host as State>::Wire>,
    /// Running and waiting DOS processes; absent before EXEC existed.
    #[prost(message, optional, tag = "36")]
    processes: Option<RuntimeSessionProcesses>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeSessionParent {
    #[prost(uint32, tag = "1")]
    psp: u32,
    #[prost(uint32, tag = "2")]
    dta_segment: u32,
    #[prost(uint32, tag = "3")]
    dta_offset: u32,
    #[prost(message, optional, tag = "4")]
    context: Option<<doslet_x86::CpuContext as State>::Wire>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct RuntimeSessionProcesses {
    #[prost(uint32, tag = "1")]
    psp: u32,
    #[prost(uint32, tag = "2")]
    return_code: u32,
    #[prost(uint32, tag = "3")]
    dpmi_client_psp: u32,
    #[prost(message, repeated, tag = "4")]
    parents: Vec<RuntimeSessionParent>,
}
impl Session {
    pub(super) fn save_wire_referenced(&self) -> RuntimeSession {
        let mut wire = self.save_wire_media(false);
        wire.drive = Some(self.drive.save_referenced());
        wire.cdrom = self.cdrom.as_ref().map(CdromImage::save_referenced);
        wire
    }
    pub(super) fn save_wire_media(&self, media: bool) -> RuntimeSession {
        RuntimeSession {
            machine: Some(State::save(&*self.machine)),
            status: Some(State::save(&self.status)),
            steps: self.steps,
            patches: self.patches.clone(),
            patches_applied: self.patches_applied as u32,
            dpmi: self.dpmi.as_deref().map(State::save),
            processes: Some(RuntimeSessionProcesses {
                psp: u32::from(self.psp),
                return_code: u32::from(self.return_code),
                dpmi_client_psp: u32::from(self.dpmi_client_psp),
                parents: self
                    .parents
                    .iter()
                    .flatten()
                    .map(|parent| RuntimeSessionParent {
                        psp: u32::from(parent.psp),
                        dta_segment: u32::from(parent.dta.0),
                        dta_offset: u32::from(parent.dta.1),
                        context: Some(parent.context.save()),
                    })
                    .collect(),
            }),
            console: self.console.to_vec(),
            console_truncated: self.console_truncated,
            extended_key: self.extended_key.as_ref().map(|v| u32::from(*(v))),
            mouse: Some(State::save(&self.mouse)),
            mouse_fraction: self.mouse_fraction.to_vec(),
            mouse_last_read: self.mouse_last_read.as_ref().map(|v| *(v)),
            mouse_last_relative_read: self.mouse_last_relative_read,
            dos_time_origin_nanos: self.dos_time_origin_nanos,
            dos_time_origin_centis: self.dos_time_origin_centis,
            memory: Some(State::save(&self.memory)),
            drive: Some(if media {
                State::save(&self.drive)
            } else {
                State::save(&ReadOnlyDrive::new())
            }),
            dta: Some(RuntimeSessionDtaTuple {
                item0: u32::from(self.dta.0),
                item1: u32::from(self.dta.1),
            }),
            overlays: self.overlays,
            break_check: self.break_check,
            cdrom: self.cdrom.as_ref().filter(|_| media).map(State::save),
            handles: Some(State::save(&self.handles)),
            staging: self.staging.to_vec(),
            continuation: self.continuation.as_ref().map(State::save),
            storage_wait: self.storage_wait,
            timeslice_yielded: self.timeslice_yielded,
            epoch: self.epoch,
            active_service: u32::from(self.active_service),
            base_identity: self.base_identity.to_vec(),
            keyboard_prefix: self.keyboard_prefix,
            input: Some(State::save(&self.input)),
            joystick_axes: self.joystick_axes.iter().map(|v| i32::from(*(v))).collect(),
            joystick_buttons: self.joystick_buttons.to_vec(),
        }
    }
    pub(super) fn load_wire(w: RuntimeSession) -> Result<Self, InvalidState> {
        Self::load_wire_media(w, None)
    }
    pub(super) fn load_wire_media(
        w: RuntimeSession,
        media: Option<&super::saved::StateResources>,
    ) -> Result<Self, InvalidState> {
        let root = doslet_dos::loader::PSP_SEGMENT;
        let processes = w.processes.unwrap_or(RuntimeSessionProcesses {
            psp: u32::from(root),
            return_code: 0,
            // Earlier states had one process, the only possible client.
            dpmi_client_psp: if w.dpmi.is_some() { u32::from(root) } else { 0 },
            parents: Vec::new(),
        });
        if processes.parents.len() > doslet_vfs::MAX_PROCESS_DEPTH {
            return Err(InvalidState("RuntimeSession.processes.parents length"));
        }
        let mut parents = process::no_parents();
        for (slot, parent) in parents.iter_mut().zip(processes.parents) {
            *slot = Some(process::Parent {
                psp: doslet_state::narrow(parent.psp, "RuntimeSession.processes.parents.psp")?,
                dta: (
                    doslet_state::narrow(parent.dta_segment, "RuntimeSession.processes.dta")?,
                    doslet_state::narrow(parent.dta_offset, "RuntimeSession.processes.dta")?,
                ),
                context: doslet_x86::CpuContext::load(doslet_state::required(
                    parent.context,
                    "RuntimeSession.processes.parents.context",
                )?)?,
            });
        }
        let value = Self {
            psp: doslet_state::narrow(processes.psp, "RuntimeSession.processes.psp")?,
            parents,
            return_code: doslet_state::narrow(
                processes.return_code,
                "RuntimeSession.processes.return_code",
            )?,
            dpmi_client_psp: doslet_state::narrow(
                processes.dpmi_client_psp,
                "RuntimeSession.processes.dpmi_client_psp",
            )?,
            // Set and consumed within one step, never at a batch boundary.
            parent_resumed: false,
            machine: alloc::boxed::Box::new(<PcMachine as State>::load(doslet_state::required(
                w.machine,
                "RuntimeSession.machine",
            )?)?),
            status: <SessionStatus as State>::load(doslet_state::required(
                w.status,
                "RuntimeSession.status",
            )?)?,
            steps: w.steps,
            disk_read_bytes: 0,
            patches: w.patches,
            patches_applied: w.patches_applied as usize,
            console: w.console,
            console_truncated: w.console_truncated,
            extended_key: w
                .extended_key
                .map(|v| -> Result<_, InvalidState> {
                    doslet_state::narrow(v, "RuntimeSession.extended_key")
                })
                .transpose()?,
            mouse: <doslet_dos::mouse::Mouse as State>::load(doslet_state::required(
                w.mouse,
                "RuntimeSession.mouse",
            )?)?,
            mouse_fraction: doslet_state::narrow(
                w.mouse_fraction
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "RuntimeSession.mouse_fraction length",
            )?,
            mouse_last_read: w
                .mouse_last_read
                .map(|v| -> Result<_, InvalidState> { Ok(v) })
                .transpose()?,
            mouse_last_relative_read: w.mouse_last_relative_read,
            dos_time_origin_nanos: w.dos_time_origin_nanos,
            dos_time_origin_centis: w.dos_time_origin_centis,
            memory: <MemoryArena as State>::load(doslet_state::required(
                w.memory,
                "RuntimeSession.memory",
            )?)?,
            drive: ReadOnlyDrive::load_referenced(
                doslet_state::required(w.drive, "RuntimeSession.drive")?,
                media.map(|m| &m.drive),
            )?,
            dta: {
                let v = doslet_state::required(w.dta, "RuntimeSession.dta")?;
                (
                    doslet_state::narrow(v.item0, "RuntimeSession.dta.item0")?,
                    doslet_state::narrow(v.item1, "RuntimeSession.dta.item1")?,
                )
            },
            overlays: w.overlays,
            break_check: w.break_check,
            cdrom: w
                .cdrom
                .map(|v| -> Result<_, InvalidState> {
                    CdromImage::load_referenced(v, media.and_then(|m| m.cdrom.as_ref()))
                })
                .transpose()?,
            handles: <HandleTable as State>::load(doslet_state::required(
                w.handles,
                "RuntimeSession.handles",
            )?)?,
            staging: (w.staging).into_boxed_slice(),
            continuation: w
                .continuation
                .map(|v| -> Result<_, InvalidState> { <storage::Continuation as State>::load(v) })
                .transpose()?,
            storage_wait: w.storage_wait,
            timeslice_yielded: w.timeslice_yielded,
            epoch: w.epoch,
            active_service: doslet_state::narrow(
                w.active_service,
                "RuntimeSession.active_service",
            )?,
            base_identity: doslet_state::narrow(
                w.base_identity,
                "RuntimeSession.base_identity length",
            )?,
            keyboard_prefix: w.keyboard_prefix,
            input: <doslet_input::InputRouter as State>::load(doslet_state::required(
                w.input,
                "RuntimeSession.input",
            )?)?,
            joystick_axes: doslet_state::narrow(
                w.joystick_axes
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        doslet_state::narrow(v, "RuntimeSession.joystick_axes")
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "RuntimeSession.joystick_axes length",
            )?,
            joystick_buttons: doslet_state::narrow(
                w.joystick_buttons
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { Ok(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "RuntimeSession.joystick_buttons length",
            )?,
            fault: None,
            persistent: None,
            idle: Default::default(),
            dpmi: w
                .dpmi
                .map(|v| <dpmi::Host as State>::load(v).map(Box::new))
                .transpose()?,
            // Set only within one service call, never at a batch boundary.
            service_flags: None,
            menu_idle: Default::default(),
            observer: Default::default(),
            synchronization: Default::default(),
            analyzer: Default::default(),
        };
        value
            .validate_patches(&value.patches)
            .map_err(InvalidState)?;
        if value.patches_applied > value.patches.len() {
            return Err(InvalidState("applied patch count exceeds plan length"));
        }
        if value
            .dpmi
            .as_ref()
            .is_some_and(|host| !host.fits(value.machine.bus.ram_size() as u32))
        {
            return Err(InvalidState("DPMI memory exceeds installed RAM"));
        }
        if value.parents.iter().flatten().count() != value.handles.depth() {
            return Err(InvalidState(
                "RuntimeSession processes and handle depth differ",
            ));
        }
        let mut value = value;
        let precise = value.dpmi.as_ref().is_some_and(|h| h.handles_exceptions());
        value.machine.cpu.set_precise_faults(precise);
        Ok(value)
    }
}
