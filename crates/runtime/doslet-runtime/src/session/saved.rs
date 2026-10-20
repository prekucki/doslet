//! Versioned portable guest state; native code and host handles are never saved.
use super::*;
use doslet_state::InvalidState;
use prost::Message;

/// Version 4 adds the DPMI host's records.
const VERSION: u32 = 4;
const MODEL: u32 = 1;
const MAX_BYTES: usize = 1024 * 1024 * 1024;

#[derive(Clone, PartialEq, prost::Message)]
struct SavedSession {
    #[prost(string, tag = "1")]
    format: alloc::string::String,
    #[prost(uint32, tag = "2")]
    version: u32,
    #[prost(uint32, tag = "3")]
    machine_model: u32,
    #[prost(message, optional, tag = "4")]
    session: Option<super::saved_state::RuntimeSession>,
    #[prost(bool, tag = "5")]
    dynamic_core: bool,
    #[prost(bool, tag = "6")]
    vga_waits: bool,
    #[prost(bool, tag = "7")]
    idle_loops: bool,
}

/// Asset bindings for restoring a file state, independent of CPU health/status.
/// Resolution uses only immutable base files and local ISO data.
pub struct StateResources {
    pub(super) drive: ReadOnlyDrive,
    pub(super) cdrom: Option<CdromImage>,
}

/// In-process diagnostic fork. Media bytes are shared immutably; every writable
/// buffer (RAM, devices, file overlays and handle positions) has its own owner.
/// This is not a file format and never carries host storage leases.
pub struct WorkingState {
    wire: super::saved_state::RuntimeSession,
    drive: ReadOnlyDrive,
    cdrom: Option<CdromImage>,
}
impl WorkingState {
    pub fn restore(&self) -> Result<Session, InvalidState> {
        let mut session = Session::load_wire(self.wire.clone())?;
        session.drive = self.drive.clone();
        // load_wire deliberately leaves persistent=None: no lease, commit queue
        // or host writeback adapter is ever inherited by a diagnostic owner.
        session.cdrom = self.cdrom.as_ref().map(|cd| cd.fork_local().unwrap());
        // Diagnostic copies start without native code, tier timing or learned proofs.
        session.set_dynamic_core(false);
        Ok(session)
    }
}

impl Session {
    pub fn state_resources(&self) -> Result<StateResources, InvalidState> {
        if self.cdrom.as_ref().is_some_and(|cd| !cd.portable()) {
            return Err(InvalidState("state resources require local media"));
        }
        Ok(StateResources {
            drive: self.drive.clone(),
            cdrom: self.cdrom.as_ref().map(|cd| cd.fork_local().unwrap()),
        })
    }
    pub fn capture_working_state(&self) -> Result<WorkingState, InvalidState> {
        self.check_working_state()?;
        Ok(WorkingState {
            wire: self.save_wire_media(false),
            drive: self.drive.clone(),
            cdrom: self.cdrom.as_ref().map(|cd| cd.fork_local().unwrap()),
        })
    }

    /// Check replay capture availability without copying guest state.
    /// A persistent-store lease belongs to the live owner, not the replay.
    /// Replays retain private RAM overlays but never publish their writes.
    pub fn check_working_state(&self) -> Result<(), InvalidState> {
        if self.fault.is_some() || self.status == SessionStatus::Faulted {
            return Err(InvalidState("cannot save a faulted session"));
        }
        if self.storage_wait
            || self.pending_commit()
            || self.cdrom.as_ref().is_some_and(|cdrom| !cdrom.portable())
        {
            return Err(InvalidState(
                "replay requires local media and no host storage operation",
            ));
        }
        Ok(())
    }

    fn check_snapshot_boundary(&self) -> Result<(), InvalidState> {
        self.check_working_state()?;
        if self.persistent.is_some() {
            return Err(InvalidState(
                "save requires local media and no host storage operation/lease",
            ));
        }
        Ok(())
    }
    /// Capture a Protobuf state at an execution boundary. Immutable disk/file
    /// contents are external typed references; overlays and handle positions are
    /// saved. No host paths, leases, native code or analyzer proofs enter it.
    pub fn save_state(&self) -> Result<Vec<u8>, InvalidState> {
        self.check_snapshot_boundary()?;
        let file = SavedSession {
            format: "doslet-state".into(),
            version: VERSION,
            machine_model: MODEL,
            session: Some(self.save_wire_referenced()),
            dynamic_core: self.dynamic_core_enabled(),
            vga_waits: self.machine.vga_wait_acceleration_enabled(),
            idle_loops: self.idle.enabled,
        };
        if file.encoded_len() > MAX_BYTES {
            return Err(InvalidState("file exceeds 1 GiB limit"));
        }
        Ok(file.encode_to_vec())
    }

    /// Decode into a new owner. Errors never partially replace a live machine.
    /// Diagnostic/controller learning starts empty; the saved effective CPU
    /// clock and all architectural/device timing are retained.
    pub fn load_state(bytes: &[u8]) -> Result<Self, InvalidState> {
        Self::decode_state(bytes, None)
    }
    /// Resolve typed media references against an existing configured session.
    /// Only immutable resources are shared; the supplied session is not changed.
    pub fn load_state_with_media(bytes: &[u8], media: &Session) -> Result<Self, InvalidState> {
        Self::load_state_with_resources(bytes, &media.state_resources()?)
    }
    pub fn load_state_with_resources(
        bytes: &[u8],
        media: &StateResources,
    ) -> Result<Self, InvalidState> {
        Self::decode_state(bytes, Some(media))
    }
    fn decode_state(bytes: &[u8], media: Option<&StateResources>) -> Result<Self, InvalidState> {
        if bytes.len() > MAX_BYTES {
            return Err(InvalidState("file exceeds 1 GiB limit"));
        }
        let file = SavedSession::decode(bytes).map_err(|_| InvalidState("Protobuf encoding"))?;
        if file.format != "doslet-state"
            || !matches!(file.version, 1..=VERSION)
            || file.machine_model != MODEL
        {
            return Err(InvalidState("unsupported format/version/machine model"));
        }
        let mut session =
            Self::load_wire_media(doslet_state::required(file.session, "session")?, media)?;
        if session.storage_wait
            || session.status == SessionStatus::Faulted
            || session.console.len() > CONSOLE_CAPACITY
            || session.staging.len() != 65536
            || !session.mouse_fraction.iter().all(|value| value.is_finite())
        {
            return Err(InvalidState("session buffers/status"));
        }
        session.set_idle_loops(file.idle_loops);
        session.set_vga_wait_acceleration(file.vga_waits);
        session.set_dynamic_core(file.dynamic_core);
        session
            .console
            .reserve(CONSOLE_CAPACITY - session.console.len());
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("../../../../../tests/fixtures/iso9660.rs");
    #[allow(dead_code)]
    mod fixture {
        include!("../../../../../tests/fixtures/opl3.rs");
    }

    fn step(session: &mut Session, count: u32) -> Vec<doslet_pcm::Frame> {
        assert_eq!(session.advance(count), count);
        let mut frames = Vec::new();
        let mut buffer = [doslet_pcm::EMPTY; 256];
        loop {
            let n = session.drain_pcm_timed(&mut buffer);
            if n == 0 {
                break;
            }
            frames.extend_from_slice(&buffer[..n]);
        }
        frames
    }

    #[test]
    fn portable_resume_preserves_cpu_ram_devices_time_and_pcm() {
        // VGA mode, unaligned CPU phase, timer/IRQ service, repeated RAM writes.
        let mut original =
            Session::new(&[0xb8, 0x13, 0, 0xcd, 0x10, 0x40, 0xa3, 0, 3, 0xeb, 0xfa]).unwrap();
        original.set_cpu_mhz(133).unwrap();
        original.set_dynamic_core(false);
        original.set_idle_loops(false);
        original.start();
        step(&mut original, 4321);
        original.pointer_relative(1.5, -2.25);
        original.push_key(0x1e, b'a').unwrap();
        let bytes = original.save_state().unwrap();
        let mut restored = Session::load_state(&bytes).unwrap();
        assert_eq!(bytes, restored.save_state().unwrap());
        for count in [1, 2, 37, 1000, 8191] {
            assert_eq!(step(&mut original, count), step(&mut restored, count));
            assert!(
                original
                    .machine
                    .cpu
                    .matches(&restored.machine.cpu.snapshot())
            );
            assert_eq!(
                original.save_state().unwrap(),
                restored.save_state().unwrap()
            );
        }
    }

    #[test]
    fn referenced_files_require_matching_media_and_forks_are_independent() {
        let mut original = Session::new(&[0xeb, 0xfe]).unwrap();
        original.drive = ReadOnlyDrive::new().with_write_overlay(4096);
        original
            .drive
            .mount(b"DATA.BIN", vec![0xab; 1024 * 1024])
            .unwrap();
        let state = original.save_state().unwrap();
        let empty = Session::new(&[0xeb, 0xfe]).unwrap().save_state().unwrap();
        assert!(
            state.len() < empty.len() + 100_000,
            "base data must not be embedded"
        );
        assert!(Session::load_state(&state).is_err());
        let mut wrong = Session::new(&[0xeb, 0xfe]).unwrap();
        wrong
            .drive
            .mount(b"DATA.BIN", vec![0xac; 1024 * 1024])
            .unwrap();
        assert!(Session::load_state_with_media(&state, &wrong).is_err());
        let restored = Session::load_state_with_media(&state, &original).unwrap();
        assert_eq!(
            restored.drive.file(b"DATA.BIN").unwrap().as_ptr(),
            original.drive.file(b"DATA.BIN").unwrap().as_ptr()
        );
        let snapshot = original.capture_working_state().unwrap();
        let mut fork = snapshot.restore().unwrap();
        assert_eq!(
            fork.drive.file(b"DATA.BIN").unwrap().as_ptr(),
            original.drive.file(b"DATA.BIN").unwrap().as_ptr()
        );
        assert!(fork.debug_write_memory(0x200, &[42]));
        let mut byte = [0];
        original.debug_read_memory(0x200, &mut byte);
        assert_ne!(byte, [42]);
        let handle = fork.drive.open(b"DATA.BIN", 2).unwrap();
        fork.drive
            .write_buffer(handle, 4)
            .unwrap()
            .copy_from_slice(b"fork");
        assert_eq!(original.save_state().unwrap(), state);
    }

    #[test]
    fn iso_references_reuse_media_and_keep_handle_positions() {
        let mut original = Session::new(&[0xeb, 0xfe])
            .unwrap()
            .with_cdrom_image(b'D', test_iso())
            .unwrap();
        let cd = original.cdrom.as_mut().unwrap();
        let handle = cd
            .open(b"D:\x5cHEROES\x5cSOUND\x5cHEROES01.62S", 0)
            .unwrap();
        cd.seek(handle, 0, 2).unwrap();
        let state = original.save_state().unwrap();
        assert!(Session::load_state(&state).is_err());
        let mut restored = Session::load_state_with_media(&state, &original).unwrap();
        assert_eq!(
            restored
                .cdrom
                .as_mut()
                .unwrap()
                .prepare_read(handle, 4)
                .unwrap(),
            (2, 2)
        );
        let mut changed = test_iso();
        changed[23 * 2048] ^= 1;
        let wrong = Session::new(&[0xeb, 0xfe])
            .unwrap()
            .with_cdrom_image(b'D', changed)
            .unwrap();
        assert!(Session::load_state_with_media(&state, &wrong).is_err());
        let snapshot = original.capture_working_state().unwrap();
        let fork = snapshot.restore().unwrap();
        let cdrom::Media::Local(left) = &original.cdrom.as_ref().unwrap().image else {
            panic!()
        };
        let cdrom::Media::Local(right) = &fork.cdrom.as_ref().unwrap().image else {
            panic!()
        };
        assert_eq!(left.bytes().as_ptr(), right.bytes().as_ptr());
    }

    #[test]
    fn rejects_truncation_and_unknown_versions_before_restoring() {
        let session = Session::new(&[0xeb, 0xfe]).unwrap();
        let bytes = session.save_state().unwrap();
        for n in [0, 1, 10, bytes.len() / 2, bytes.len() - 1] {
            assert!(Session::load_state(&bytes[..n]).is_err(), "length={n}");
        }
        let mut file = SavedSession::decode(bytes.as_slice()).unwrap();
        file.version += 1;
        assert!(Session::load_state(&file.encode_to_vec()).is_err());
    }

    #[test]
    fn portable_resume_preserves_sounding_fm_and_open_file_overlay() {
        let mut original = Session::new(&fixture::opl3_tone_program()).unwrap();
        original.set_dynamic_core(false);
        original.set_idle_loops(false);
        original.drive = ReadOnlyDrive::new().with_write_overlay(4096);
        original
            .drive
            .mount(b"TEST.DAT", b"initial contents".to_vec())
            .unwrap();
        let handle = original.drive.open(b"TEST.DAT", 2).unwrap();
        original.drive.seek(handle, 0, 3).unwrap();
        original
            .drive
            .write_buffer(handle, 4)
            .unwrap()
            .copy_from_slice(b"edit");
        original.start();
        let sounding = step(&mut original, 100_000);
        assert!(sounding.iter().any(|frame| frame.samples != [0, 0]));
        let mut restored =
            Session::load_state_with_media(&original.save_state().unwrap(), &original).unwrap();
        for count in [17, 1001, 100_000] {
            assert_eq!(step(&mut original, count), step(&mut restored, count));
        }
        assert_eq!(
            original.save_state().unwrap(),
            restored.save_state().unwrap()
        );
        // The handle position and pending write survive; owners can diverge safely.
        assert_eq!(
            original.drive.seek(handle, 1, 0).unwrap(),
            restored.drive.seek(handle, 1, 0).unwrap()
        );
        restored.drive.close(handle).unwrap();
        assert!(original.drive.seek(handle, 1, 0).is_ok());
    }
}
