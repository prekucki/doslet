//! Service continuations never retain a borrow of guest RAM or cache storage.
use super::*;
use doslet_storage::{CommitBatch, RequestTag, payload_digest};
#[derive(Clone, Copy)]
pub(super) enum CopyTarget {
    Segment(u16, u16),
    Linear(u32),
}
pub(super) enum Continuation {
    RawStage {
        offset: u64,
        target: CopyTarget,
        length: usize,
        done: usize,
        ax: u16,
    },
    WriteStage {
        handle: u16,
        segment: u16,
        offset: u16,
        length: usize,
        done: usize,
    },
    ReadStage {
        handle: u16,
        position: u32,
        target: CopyTarget,
        length: usize,
        done: usize,
    },
    Resolve(u8),
    Copy {
        target: CopyTarget,
        length: usize,
        copied: usize,
        ax: u16,
    },
    Commit,
    HostCommit,
    ExitCommit(u8),
    Return,
}
pub(super) struct Persistent {
    namespace: [u8; 32],
    base: [u8; 32],
    fencing_token: u64,
    revision: u64,
    next_request: u64,
    buffer: Vec<u8>,
    pending: Option<(RequestTag, CommitBatch)>,
    emitted: bool,
    result: Option<Result<u64, StorageError>>,
    attempts: u8,
    unresolved: bool,
}
impl Session {
    pub fn hardware_config(&self) -> HardwareConfig {
        self.machine.hardware_config()
    }
    pub fn key_event(
        &mut self,
        key: doslet_input::PhysicalKey,
        pressed: bool,
    ) -> Result<(), RuntimeError> {
        self.input
            .accept_key(key, pressed)
            .map_err(RuntimeError::Input)
    }
    pub fn physical_key(&mut self, usage: u16, pressed: bool) -> Result<(), RuntimeError> {
        self.key_event(doslet_input::PhysicalKey(usage), pressed)
    }
    /// Switch input delivery at a batch boundary without replacing the machine.
    pub fn set_keyboard_mode(
        &mut self,
        mode: doslet_config::KeyboardMode,
    ) -> Result<(), RuntimeError> {
        if mode != self.hardware_config().keyboard {
            self.release_keys()?;
            let toggles = self.machine.bus.read_u8(0x417)? & 0xf0;
            self.set_key_modifiers(toggles)?;
            self.machine.set_keyboard_mode(mode)?;
        }
        Ok(())
    }
    pub fn release_keys(&mut self) -> Result<(), RuntimeError> {
        self.input.release_all();
        self.machine.release_keys()?;
        self.keyboard_prefix = false;
        self.joystick_axes = [0; 2];
        self.joystick_buttons = [false; 2];
        self.machine.set_joystick([0; 2], [false; 2]);
        Ok(())
    }
    pub fn set_joystick(&mut self, axes: [i16; 2], buttons: [bool; 2]) {
        if let Err(error) = self.input.accept_joystick(axes, buttons) {
            self.fault = Some(RuntimeError::Input(error));
            self.status = SessionStatus::Faulted;
        }
    }
    pub fn route_input(&mut self, event: doslet_input::InputEvent) -> Result<(), RuntimeError> {
        self.input.push(event).map_err(RuntimeError::Input)
    }
    pub fn release_input_source(
        &mut self,
        source: doslet_input::InputSourceId,
    ) -> Result<(), RuntimeError> {
        self.input
            .release_source(source)
            .map_err(RuntimeError::Input)
    }
    pub fn connect_input_keyboard(
        &mut self,
        source: doslet_input::InputSourceId,
        generation: u64,
    ) -> Result<(), RuntimeError> {
        self.input
            .connect_keyboard(source, generation)
            .map_err(RuntimeError::Input)
    }
    pub fn connect_input_joystick(
        &mut self,
        source: doslet_input::InputSourceId,
        generation: u64,
    ) -> Result<(), RuntimeError> {
        self.input
            .connect_joystick(source, generation)
            .map_err(RuntimeError::Input)
    }
    pub fn input_router_mut(&mut self) -> &mut doslet_input::InputRouter {
        &mut self.input
    }
    pub(super) fn apply_input(&mut self) {
        for _ in 0..64 {
            let Some(event) = self.input.next_event() else {
                break;
            };
            self.menu_idle_input();
            match event {
                doslet_input::GuestEvent::Key { key, pressed } => {
                    if let Err(error) = self.machine.key_event(key, pressed) {
                        self.fault = Some(error.into());
                        self.status = SessionStatus::Faulted;
                    }
                }
                doslet_input::GuestEvent::Axis { axis, value } => {
                    if let Some(axis) = self.joystick_axes.get_mut(axis as usize) {
                        *axis = value;
                        self.machine
                            .set_joystick(self.joystick_axes, self.joystick_buttons);
                    }
                }
                doslet_input::GuestEvent::Button { button, pressed } => {
                    if let Some(button) = self.joystick_buttons.get_mut(button as usize) {
                        *button = pressed;
                        self.machine
                            .set_joystick(self.joystick_axes, self.joystick_buttons);
                    }
                }
                doslet_input::GuestEvent::ReleaseAll => {
                    if let Err(error) = self.machine.release_keys() {
                        self.fault = Some(error.into());
                        self.status = SessionStatus::Faulted;
                    }
                    self.keyboard_prefix = false;
                    self.joystick_axes = [0; 2];
                    self.joystick_buttons = [false; 2];
                    self.machine.set_joystick([0; 2], [false; 2]);
                }
            }
        }
    }
    pub fn with_cdrom_source(
        mut self,
        letter: u8,
        source: CachedSource,
    ) -> Result<Self, RuntimeError> {
        self.attach_cdrom_source(letter, source)?;
        Ok(self)
    }
    pub fn attach_cdrom_source(
        &mut self,
        letter: u8,
        mut source: CachedSource,
    ) -> Result<(), RuntimeError> {
        if self.status != SessionStatus::Ready || self.cdrom.is_some() {
            return Err(RuntimeError::InvalidLaunch);
        }
        source.reset_epoch(self.epoch);
        self.cdrom = Some(CdromImage::from_source(letter, source)?);
        Ok(())
    }
    pub fn base_identity(&self) -> [u8; 32] {
        self.base_identity
    }
    pub fn storage_waiting(&self) -> bool {
        self.storage_wait
    }
    pub fn pending_commit(&self) -> bool {
        self.persistent
            .as_ref()
            .is_some_and(|store| store.pending.is_some())
    }
    pub fn unresolved_commit(&self) -> bool {
        self.persistent
            .as_ref()
            .is_some_and(|store| store.unresolved)
    }
    pub fn committed_revision(&self) -> Option<u64> {
        self.persistent.as_ref().map(|store| store.revision)
    }
    pub fn dirty_overlay(&self) -> bool {
        self.drive.dirty()
    }
    pub fn with_persistent_store(
        mut self,
        namespace: [u8; 32],
        base: [u8; 32],
        fencing_token: u64,
        revision: u64,
        payload: Option<&[u8]>,
    ) -> Result<Self, RuntimeError> {
        self.attach_persistent_store(namespace, base, fencing_token, revision, payload)?;
        Ok(self)
    }
    pub fn attach_persistent_store(
        &mut self,
        namespace: [u8; 32],
        base: [u8; 32],
        fencing_token: u64,
        revision: u64,
        payload: Option<&[u8]>,
    ) -> Result<(), RuntimeError> {
        if self.status != SessionStatus::Ready || self.persistent.is_some() {
            return Err(RuntimeError::InvalidLaunch);
        }
        if base != self.base_identity {
            return Err(RuntimeError::Storage(StorageError::MissingBase));
        }
        if let Some(payload) = payload {
            self.import_overlay(payload)
                .map_err(RuntimeError::Storage)?;
            self.drive.acknowledge_all();
        }
        self.persistent = Some(Persistent {
            namespace,
            base,
            fencing_token,
            revision,
            next_request: 1,
            buffer: Vec::with_capacity(self.drive.snapshot_capacity() + 36),
            pending: None,
            emitted: false,
            result: None,
            attempts: 0,
            unresolved: false,
        });
        Ok(())
    }
    /// Detach the host storage lease at a settled boundary so a portable state
    /// snapshot (`save_state`) can be written on shutdown.
    pub fn detach_persistent_store(&mut self) -> Result<(), RuntimeError> {
        if self.storage_wait || self.pending_commit() {
            return Err(RuntimeError::Storage(StorageError::Unavailable));
        }
        self.persistent = None;
        Ok(())
    }
    /// Reattach a host storage lease to a session restored from a state snapshot,
    /// keeping the snapshot's live file handles and overlay.
    pub fn attach_resumed_persistent_store(
        &mut self,
        namespace: [u8; 32],
        base: [u8; 32],
        fencing_token: u64,
        revision: u64,
    ) -> Result<(), RuntimeError> {
        if self.persistent.is_some() {
            return Err(RuntimeError::InvalidLaunch);
        }
        if base != self.base_identity {
            return Err(RuntimeError::Storage(StorageError::MissingBase));
        }
        self.persistent = Some(Persistent {
            namespace,
            base,
            fencing_token,
            revision,
            next_request: 1,
            buffer: Vec::with_capacity(self.drive.snapshot_capacity() + 36),
            pending: None,
            emitted: false,
            result: None,
            attempts: 0,
            unresolved: false,
        });
        Ok(())
    }
    pub fn export_overlay(&mut self) -> Result<Vec<u8>, StorageError> {
        let mut body = Vec::with_capacity(self.drive.snapshot_capacity());
        self.drive
            .encode_snapshot(&mut body)
            .map_err(|_| StorageError::Corrupt)?;
        let mut result = Vec::with_capacity(body.len() + 36);
        result.extend_from_slice(b"DSV1");
        result.extend_from_slice(&self.base_identity);
        result.extend_from_slice(&body);
        Ok(result)
    }
    pub fn import_overlay(&mut self, payload: &[u8]) -> Result<(), StorageError> {
        if self.status != SessionStatus::Ready || self.pending_commit() {
            return Err(StorageError::BusyWriter);
        }
        if payload.get(..4) != Some(b"DSV1")
            || payload.get(4..36) != Some(self.base_identity.as_slice())
        {
            return Err(StorageError::MissingBase);
        }
        self.drive
            .restore_snapshot(&payload[36..])
            .map_err(|_| StorageError::Corrupt)?;
        self.drive.mark_dirty();
        Ok(())
    }
    /// Called by the host outside execution. A commit request copies only its
    /// bounded changed-data payload; core execution serializes into reserved RAM.
    pub fn next_host_request(&mut self) -> Option<HostRequest> {
        if let Some(store) = &mut self.persistent
            && let Some((tag, batch)) = &store.pending
            && !store.emitted
        {
            store.emitted = true;
            return Some(HostRequest::Commit {
                tag: *tag,
                batch: batch.clone(),
            });
        }
        self.cdrom.as_mut().and_then(CdromImage::next_request)
    }
    pub fn complete_host_request(
        &mut self,
        completion: HostCompletion,
    ) -> Result<(), StorageError> {
        match completion {
            HostCompletion::Commit { tag, result } => {
                let Some(store) = &mut self.persistent else {
                    return Err(StorageError::StaleCompletion);
                };
                if store
                    .pending
                    .as_ref()
                    .is_none_or(|(pending, _)| *pending != tag)
                    || store.result.is_some()
                {
                    return Err(StorageError::StaleCompletion);
                }
                if result == Err(StorageError::Unavailable) {
                    // The transaction may have committed before transport failed.
                    // Retry the exact durable mutation at most three times; never
                    // publish a new payload or permit reset while status is unknown.
                    if store.attempts < 3 {
                        store.attempts += 1;
                        store.emitted = false;
                        let next = store.next_request;
                        store.next_request = next.checked_add(1).ok_or(StorageError::Quota)?;
                        if let Some((tag, _)) = &mut store.pending {
                            tag.id = next;
                        }
                    } else {
                        store.unresolved = true;
                    }
                    self.storage_wait = true;
                    return Ok(());
                }
                store.result = Some(result);
                self.storage_wait = false;
                // Finalize the acknowledgement immediately, even while paused.
                // This settles reset safety without executing guest IRET.
                let host_only = matches!(
                    self.continuation,
                    Some(Continuation::HostCommit | Continuation::ExitCommit(_))
                );
                self.resume_storage().map_err(|_| StorageError::Corrupt)?;
                if !host_only {
                    self.continuation = Some(Continuation::Return);
                }
                Ok(())
            }
            completion => {
                self.cdrom
                    .as_mut()
                    .ok_or(StorageError::StaleCompletion)?
                    .complete(completion)?;
                self.storage_wait = self.cdrom.as_ref().is_some_and(|cd| cd.pending() != 0);
                Ok(())
            }
        }
    }
    pub(super) fn begin_commit(&mut self) -> Result<(), RuntimeError> {
        if !self.drive.dirty() {
            return Ok(());
        }
        let Some(store) = &mut self.persistent else {
            return Ok(());
        };
        if store.pending.is_some() {
            return Err(RuntimeError::Storage(StorageError::BusyWriter));
        }
        // Reuse the acknowledged payload as the next serialization buffer.
        self.drive
            .encode_snapshot(&mut store.buffer)
            .map_err(|_| RuntimeError::Storage(StorageError::Quota))?;
        let body_len = store.buffer.len();
        store.buffer.resize(body_len + 36, 0);
        store.buffer.copy_within(..body_len, 36);
        store.buffer[..4].copy_from_slice(b"DSV1");
        store.buffer[4..36].copy_from_slice(&store.base);
        let mutation_id = store
            .revision
            .checked_add(1)
            .ok_or(RuntimeError::Storage(StorageError::Quota))?;
        let id = store.next_request;
        store.next_request = id
            .checked_add(1)
            .ok_or(RuntimeError::Storage(StorageError::Quota))?;
        let tag = RequestTag {
            epoch: self.epoch,
            id,
            generation: store.fencing_token,
            slot: 0,
        };
        let payload = core::mem::take(&mut store.buffer);
        let batch = CommitBatch {
            namespace: store.namespace,
            base: store.base,
            fencing_token: store.fencing_token,
            expected_revision: store.revision,
            mutation_id,
            digest: payload_digest(&payload),
            payload,
        };
        store.pending = Some((tag, batch));
        store.emitted = false;
        store.result = None;
        store.attempts = 1;
        store.unresolved = false;
        self.continuation = Some(Continuation::Commit);
        self.storage_wait = true;
        Ok(())
    }
    pub(super) fn begin_copy(&mut self, target: CopyTarget, length: usize, ax: u16) {
        self.continuation = Some(Continuation::Copy {
            target,
            length,
            copied: 0,
            ax,
        });
    }
    /// Each visit performs at most one 4-KiB guest copy; no CPU instruction or
    /// hardware clock executes until resolution, copying and return finalize.
    pub(super) fn resume_storage(&mut self) -> Result<bool, RuntimeError> {
        let Some(continuation) = self.continuation.take() else {
            return Ok(true);
        };
        let host_only = matches!(
            continuation,
            Continuation::HostCommit | Continuation::ExitCommit(_)
        );
        let exit = if let Continuation::ExitCommit(code) = continuation {
            Some(code)
        } else {
            None
        };
        match continuation {
            Continuation::RawStage {
                offset,
                target,
                length,
                mut done,
                ax,
            } => {
                let end = (done + 4096).min(length);
                let result = self
                    .cdrom
                    .as_mut()
                    .ok_or(15u16)
                    .and_then(|cd| cd.raw(offset + done as u64, &mut self.staging[done..end]));
                match result {
                    Err(cdrom::PENDING) => {
                        self.continuation = Some(Continuation::RawStage {
                            offset,
                            target,
                            length,
                            done,
                            ax,
                        });
                        self.storage_wait = true;
                        Ok(false)
                    }
                    Err(_) => {
                        self.dos_error(15)?;
                        Ok(true)
                    }
                    Ok(_) => {
                        done = end;
                        if done < length {
                            self.continuation = Some(Continuation::RawStage {
                                offset,
                                target,
                                length,
                                done,
                                ax,
                            });
                            Ok(false)
                        } else {
                            self.begin_copy(target, length, ax);
                            Ok(false)
                        }
                    }
                }
            }
            Continuation::WriteStage {
                handle,
                segment,
                offset,
                length,
                mut done,
            } => {
                let end = (done + 4096).min(length);
                for i in done..end {
                    self.staging[i - done] = self
                        .machine
                        .bus
                        .read_u8(linear_address(segment, offset.wrapping_add(i as u16)))?;
                }
                self.drive
                    .write_prepared(handle, done, &self.staging[..end - done])
                    .map_err(|_| RuntimeError::Storage(StorageError::Corrupt))?;
                done = end;
                if done < length {
                    self.continuation = Some(Continuation::WriteStage {
                        handle,
                        segment,
                        offset,
                        length,
                        done,
                    });
                    Ok(false)
                } else {
                    self.machine.cpu.registers.set(Gpr16::Ax, length as u16);
                    self.set_carry(false)?;
                    Ok(true)
                }
            }
            Continuation::ReadStage {
                handle,
                position,
                target,
                length,
                mut done,
            } => {
                let end = (done + 4096).min(length);
                let output = &mut self.staging[done..end];
                let result = if CdromImage::owns_handle(handle) {
                    self.cdrom
                        .as_mut()
                        .ok_or(6u16)
                        .and_then(|cd| cd.read_at_into(handle, position + done as u32, output))
                        .map(|_| ())
                } else {
                    self.drive
                        .read_at_into(handle, position + done as u32, output)
                };
                match result {
                    Err(cdrom::PENDING) => {
                        self.continuation = Some(Continuation::ReadStage {
                            handle,
                            position,
                            target,
                            length,
                            done,
                        });
                        self.storage_wait = true;
                        Ok(false)
                    }
                    Err(code) => {
                        self.dos_error(code)?;
                        Ok(true)
                    }
                    Ok(()) => {
                        done = end;
                        if done < length {
                            self.continuation = Some(Continuation::ReadStage {
                                handle,
                                position,
                                target,
                                length,
                                done,
                            });
                            return Ok(false);
                        }
                        if CdromImage::owns_handle(handle) {
                            self.cdrom
                                .as_mut()
                                .unwrap()
                                .finish_read(handle, length)
                                .map_err(|_| RuntimeError::Storage(StorageError::Corrupt))?;
                        } else {
                            self.drive
                                .finish_read(handle, length)
                                .map_err(|_| RuntimeError::Storage(StorageError::Corrupt))?;
                        }
                        self.begin_copy(target, length, length as u16);
                        if length <= 4096 {
                            self.resume_storage()
                        } else {
                            Ok(false)
                        }
                    }
                }
            }
            Continuation::Return => Ok(true),
            Continuation::Resolve(vector) => {
                self.active_service = vector;
                self.service(vector)?;
                Ok(self.continuation.is_none())
            }
            Continuation::Copy {
                target,
                length,
                mut copied,
                ax,
            } => {
                let end = (copied + 4096).min(length);
                for offset in copied..end {
                    let address = match target {
                        CopyTarget::Segment(segment, start) => {
                            linear_address(segment, start.wrapping_add(offset as u16))
                        }
                        CopyTarget::Linear(start) => start + offset as u32,
                    };
                    self.machine.bus.write_u8(address, self.staging[offset])?;
                }
                self.disk_read_bytes = self.disk_read_bytes.saturating_add((end - copied) as u64);
                copied = end;
                if copied < length {
                    self.continuation = Some(Continuation::Copy {
                        target,
                        length,
                        copied,
                        ax,
                    });
                    Ok(false)
                } else {
                    self.machine.cpu.registers.set(Gpr16::Ax, ax);
                    self.set_carry(false)?;
                    Ok(true)
                }
            }
            Continuation::Commit | Continuation::HostCommit | Continuation::ExitCommit(_) => {
                let store = self
                    .persistent
                    .as_mut()
                    .ok_or(RuntimeError::Storage(StorageError::Corrupt))?;
                let Some(result) = store.result.take() else {
                    self.continuation = Some(if let Some(code) = exit {
                        Continuation::ExitCommit(code)
                    } else if host_only {
                        Continuation::HostCommit
                    } else {
                        Continuation::Commit
                    });
                    self.storage_wait = true;
                    return Ok(false);
                };
                let (_, batch) = store
                    .pending
                    .take()
                    .ok_or(RuntimeError::Storage(StorageError::Corrupt))?;
                store.buffer = batch.payload;
                store.buffer.clear();
                match result {
                    Ok(revision) if revision == store.revision + 1 => {
                        store.revision = revision;
                        self.drive.acknowledge_all();
                        if let Some(code) = exit {
                            self.status = SessionStatus::Exited(code);
                        }
                        if !host_only {
                            self.set_carry(false)?;
                        }
                    }
                    Ok(_) => {
                        if exit.is_some() {
                            self.fault = Some(RuntimeError::Storage(StorageError::Corrupt));
                            self.status = SessionStatus::Faulted;
                        }
                        if !host_only {
                            self.dos_error(5)?;
                        }
                    }
                    Err(error) => {
                        if exit.is_some() {
                            self.fault = Some(RuntimeError::Storage(error));
                            self.status = SessionStatus::Faulted;
                        }
                        if !host_only {
                            self.dos_error(5)?;
                        }
                    }
                };
                Ok(true)
            }
        }
    }
    pub(super) fn register_handle(&mut self, backend: u16, mode: u8) -> Result<u16, u16> {
        let mount = MountId(u8::from(CdromImage::owns_handle(backend)));
        let result = self.handles.open(OpenFile {
            mount,
            node: NodeId(u64::from(backend)),
            position: 0,
            mode,
        });
        if result.is_err() {
            if mount.0 == 1 {
                if let Some(cd) = &mut self.cdrom {
                    let _ = cd.close(backend);
                }
            } else {
                let _ = self.drive.close(backend);
            }
        }
        result
    }
    pub(super) fn backend_handle(&self, handle: u16) -> Result<u16, u16> {
        self.handles.get(handle).map(|file| file.node.0 as u16)
    }
    pub(super) fn close_handle(&mut self, handle: u16) -> Result<(), u16> {
        let file = *self.handles.get(handle)?;
        self.handles.close(handle)?;
        if !self.handles.any_open(file.mount, file.node) {
            if file.mount.0 == 1 {
                self.cdrom.as_mut().ok_or(6u16)?.close(file.node.0 as u16)?;
            } else {
                self.drive.close(file.node.0 as u16)?;
            }
        }
        Ok(())
    }
    pub(super) fn force_duplicate_handle(&mut self, source: u16, target: u16) -> Result<(), u16> {
        self.handles.get(source)?;
        if source == target {
            return Ok(());
        }
        if !(5..5 + doslet_vfs::HANDLE_COUNT as u16).contains(&target) {
            return Err(6);
        }
        if self.handles.get(target).is_ok() {
            self.close_handle(target)?;
        }
        self.handles.force_duplicate(source, target)
    }
}

#[cfg(target_has_atomic = "64")]
static NEXT_EPOCH: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);
#[cfg(not(target_has_atomic = "64"))]
static NEXT_EPOCH: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(1);
pub(super) fn new_epoch() -> Result<u64, RuntimeError> {
    let epoch = NEXT_EPOCH
        .try_update(
            core::sync::atomic::Ordering::Relaxed,
            core::sync::atomic::Ordering::Relaxed,
            |current| current.checked_add(1),
        )
        .map_err(|_| RuntimeError::Storage(StorageError::Quota))?;
    #[cfg(target_has_atomic = "64")]
    {
        Ok(epoch)
    }
    #[cfg(not(target_has_atomic = "64"))]
    {
        Ok(u64::from(epoch))
    }
}

impl Session {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    /// Start a fresh host attachment after restoring a portable snapshot.
    pub fn renew_epoch(&mut self) -> Result<(), RuntimeError> {
        if self.continuation.is_some() || self.storage_wait || self.persistent.is_some() {
            return Err(RuntimeError::InvalidLaunch);
        }
        let epoch = new_epoch()?;
        self.release_keys()?;
        self.epoch = epoch;
        self.input.reset(epoch);
        if let Some(cd) = &mut self.cdrom
            && let cdrom::Media::Remote(provider) = &mut cd.image
        {
            provider.source_mut().reset_epoch(epoch);
        }
        Ok(())
    }
    pub fn set_epoch(&mut self, epoch: u64) -> Result<(), RuntimeError> {
        if self.status != SessionStatus::Ready || self.continuation.is_some() {
            return Err(RuntimeError::InvalidLaunch);
        }
        self.epoch = epoch;
        self.input.reset(epoch);
        if let Some(cd) = &mut self.cdrom
            && let cdrom::Media::Remote(provider) = &mut cd.image
        {
            provider.source_mut().reset_epoch(epoch);
        }
        Ok(())
    }
    /// Explicit host import/flush: no guest interrupt frame or registers change.
    pub fn request_overlay_commit(&mut self) -> Result<(), RuntimeError> {
        if !matches!(self.status, SessionStatus::Ready | SessionStatus::Paused)
            || self.continuation.is_some()
        {
            return Err(RuntimeError::InvalidLaunch);
        }
        self.begin_commit()?;
        if self.continuation.is_some() {
            self.continuation = Some(Continuation::HostCommit);
        }
        Ok(())
    }
}

impl Session {
    pub(super) fn finish_exit(&mut self, code: u8) -> Result<(), RuntimeError> {
        self.drive.close_all();
        self.handles.clear();
        if let Some(cd) = &mut self.cdrom {
            cd.close_all();
        }
        self.begin_commit()?;
        if self.continuation.is_some() {
            self.continuation = Some(Continuation::ExitCommit(code));
        } else {
            self.status = SessionStatus::Exited(code);
        }
        Ok(())
    }
}

#[cfg(feature = "save-state")]
#[path = "storage_saved.rs"]
mod saved_state;
