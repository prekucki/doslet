use doslet_dos::files::ReadOnlyDrive;
use doslet_runtime::{Session, SessionStatus};
use doslet_storage::{
    CachedSource, HostCompletion, HostRequest, RamStore, SourceId, SourceIdentity, StorageError,
};
use doslet_x86::Gpr16;
include!("../../../../tests/fixtures/iso9660.rs");
fn cd_program() -> Vec<u8> {
    let mut program = vec![0; 0x130];
    let code = [
        0xBA, 0, 2, 0xB8, 0, 0x3D, 0xCD, 0x21, 0x89, 0xC3, 0xBA, 0, 3, 0xB9, 4, 0, 0xB4, 0x3F,
        0xCD, 0x21, 0xB4, 0x3E, 0xCD, 0x21, 0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    program[..code.len()].copy_from_slice(&code);
    let path = b"D:\\HEROES\\SOUND\\HEROES01.62S\0";
    program[0x100..0x100 + path.len()].copy_from_slice(path);
    program
}
#[test]
fn remote_iso_suspends_without_guest_time_or_cpu_retry_and_rejects_duplicate() {
    let image = test_iso();
    let identity = SourceIdentity {
        id: SourceId(7),
        generation: 4,
        length: image.len() as u64,
        validator: [8; 32],
    };
    let mut session = Session::new(&cd_program())
        .unwrap()
        .with_cdrom_source(b'D', CachedSource::new(identity, 1))
        .unwrap();
    session.start();
    session.advance(1000);
    assert!(session.storage_waiting());
    let time = session.emulated_nanos();
    let ip = session.registers().ip;
    assert_eq!(session.advance(10000), 0);
    assert_eq!(session.emulated_nanos(), time);
    assert_eq!(session.registers().ip, ip);
    assert_eq!(session.disk_read_bytes(), 0);
    let request = session.next_host_request().unwrap();
    let HostRequest::Read {
        tag,
        offset,
        length,
        ..
    } = request
    else {
        panic!()
    };
    assert_eq!(offset, 0);
    assert_eq!(length, image.len() as u32);
    session.pause();
    session
        .complete_host_request(HostCompletion::Read {
            tag,
            bytes: image,
            result: Ok(()),
        })
        .unwrap();
    assert_eq!(session.status(), SessionStatus::Paused);
    assert_eq!(session.emulated_nanos(), time);
    assert_eq!(
        session.complete_host_request(HostCompletion::Read {
            tag,
            bytes: Vec::new(),
            result: Ok(())
        }),
        Err(StorageError::StaleCompletion)
    );
    session.start();
    session.advance(1000);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.ram_view(0x10300..0x10304).unwrap(), b"data");
    assert_eq!(session.disk_read_bytes(), 4);
}
fn save_program() -> Vec<u8> {
    let mut program = vec![0; 0x120];
    let code = [
        0xBA, 0, 2, 0x31, 0xC9, 0xB4, 0x3C, 0xCD, 0x21, 0x89, 0xC3, 0xBA, 0x10, 2, 0xB9, 4, 0,
        0xB4, 0x40, 0xCD, 0x21, 0xB4, 0x3E, 0xCD, 0x21, 0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    program[..code.len()].copy_from_slice(&code);
    program[0x100..0x109].copy_from_slice(b"SAVE.SAV\0");
    program[0x110..0x114].copy_from_slice(b"save");
    program
}
fn save_session() -> Session {
    Session::new(&save_program())
        .unwrap()
        .with_drive(ReadOnlyDrive::new().with_write_overlay(16384))
}
#[cfg(feature = "save-state")]
#[test]
fn diagnostic_replay_uses_private_writes_and_does_not_inherit_the_live_store() {
    let mut live = save_session();
    let base = live.base_identity();
    live.attach_persistent_store([1; 32], base, 77, 9, None)
        .unwrap();
    assert!(live.check_working_state().is_ok());
    assert!(
        live.save_state().is_err(),
        "portable state policy stays unchanged"
    );
    let initial_files = live.export_overlay().unwrap();
    let mut replay = live.capture_working_state().unwrap().restore().unwrap();
    assert_eq!(replay.committed_revision(), None);
    replay.start();
    replay.advance(1000);
    assert_eq!(replay.status(), SessionStatus::Exited(0));
    assert!(!replay.storage_waiting());
    assert!(!replay.pending_commit());
    assert!(replay.next_host_request().is_none());
    assert!(replay.dirty_overlay());
    assert_ne!(replay.export_overlay().unwrap(), initial_files);
    assert_eq!(live.export_overlay().unwrap(), initial_files);
    assert_eq!(live.committed_revision(), Some(9));
    assert!(!live.dirty_overlay());
    assert_eq!(live.steps(), 0);
    assert!(live.next_host_request().is_none());

    // The live owner still makes a real commit request for the same guest write.
    live.start();
    live.advance(1000);
    assert!(live.pending_commit());
    assert!(live.check_working_state().is_err());
    assert!(live.capture_working_state().is_err());
    assert!(matches!(
        live.next_host_request(),
        Some(HostRequest::Commit { .. })
    ));
}

#[test]
fn persistent_create_and_close_await_atomic_ack_with_no_auto_resume() {
    let session = save_session();
    let base = session.base_identity();
    let mut session = session
        .with_persistent_store([1; 32], base, 77, 0, None)
        .unwrap();
    let mut store = RamStore::new([1; 32], base, 1 << 20);
    store.fencing_token = 77;
    session.start();
    session.advance(1000);
    assert!(session.pending_commit());
    let time = session.emulated_nanos();
    assert_eq!(session.advance(1000), 0);
    assert_eq!(session.emulated_nanos(), time);
    let HostRequest::Commit { tag, batch } = session.next_host_request().unwrap() else {
        panic!()
    };
    assert_eq!(tag.epoch, session.epoch());
    let revision = store.commit(&batch).unwrap();
    session.pause();
    session
        .complete_host_request(HostCompletion::Commit {
            tag,
            result: Ok(revision),
        })
        .unwrap();
    assert!(!session.pending_commit());
    assert_eq!(session.status(), SessionStatus::Paused);
    assert_eq!(session.committed_revision(), Some(1));
    assert_eq!(
        session.complete_host_request(HostCompletion::Commit {
            tag,
            result: Ok(revision)
        }),
        Err(StorageError::StaleCompletion)
    );
    session.start();
    session.advance(1000);
    assert!(session.pending_commit());
    let HostRequest::Commit { tag, batch } = session.next_host_request().unwrap() else {
        panic!()
    };
    let revision = store.commit(&batch).unwrap();
    session
        .complete_host_request(HostCompletion::Commit {
            tag,
            result: Ok(revision),
        })
        .unwrap();
    session.advance(1000);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert!(!session.dirty_overlay());
    assert_eq!(session.committed_revision(), Some(2));
    let recovered = save_session()
        .with_persistent_store([1; 32], base, 78, 2, Some(store.payload()))
        .unwrap();
    assert!(!recovered.dirty_overlay());
}
#[test]
fn failed_commit_retains_exportable_dirty_data_and_old_epoch_is_rejected() {
    let session = save_session();
    let base = session.base_identity();
    let mut session = session
        .with_persistent_store([1; 32], base, 77, 0, None)
        .unwrap();
    session.start();
    session.advance(1000);
    let HostRequest::Commit { tag, .. } = session.next_host_request().unwrap() else {
        panic!()
    };
    let mut stale = tag;
    stale.epoch += 1;
    assert_eq!(
        session.complete_host_request(HostCompletion::Commit {
            tag: stale,
            result: Ok(1)
        }),
        Err(StorageError::StaleCompletion)
    );
    assert!(session.pending_commit());
    session
        .complete_host_request(HostCompletion::Commit {
            tag,
            result: Err(StorageError::Quota),
        })
        .unwrap();
    assert!(session.dirty_overlay());
    assert_eq!(session.committed_revision(), Some(0));
    assert!(!session.export_overlay().unwrap().is_empty());
}
#[test]
fn configured_bios_reports_real_ram_and_input_mode() {
    let config = doslet_config::HardwareConfig {
        memory_mib: 4,
        ..Default::default()
    };
    let mut session =
        Session::new_configured(&[0xB8, 0, 0x88, 0xCD, 0x15, 0xEB, 0xFE], config).unwrap();
    session.start();
    session.advance(3);
    assert_eq!(session.registers().get(Gpr16::Ax), 3072);
    assert_eq!(session.hardware_config(), config);
}
#[test]
fn large_local_read_stages_then_copies_in_bounded_windows_without_guest_time() {
    let mut program = vec![0; 0x110];
    let code = [
        0xBA, 0, 2, 0xB8, 0, 0x3D, 0xCD, 0x21, 0x89, 0xC3, 0xBA, 0, 0x10, 0xB9, 0, 0x50, 0xB4,
        0x3F, 0xCD, 0x21, 0xEB, 0xFE,
    ];
    program[..code.len()].copy_from_slice(&code);
    program[0x100..0x105].copy_from_slice(b"FILE\0");
    let mut drive = ReadOnlyDrive::new();
    drive.mount(b"FILE", vec![7; 0x5000]).unwrap();
    let mut session = Session::new(&program).unwrap().with_drive(drive);
    session.start();
    for _ in 0..32 {
        if session.registers().cs == 0xF000 && session.registers().get(Gpr16::Ax) >> 8 == 0x3F {
            break;
        }
        session.advance(1);
    }
    let time = session.emulated_nanos();
    session.advance(5);
    assert_eq!(session.emulated_nanos(), time);
    assert!(
        session
            .ram_view(0x11000..0x16000)
            .unwrap()
            .iter()
            .all(|b| *b == 0)
    );
    session.advance(1);
    assert_eq!(session.emulated_nanos(), time);
    assert!(
        session
            .ram_view(0x11000..0x12000)
            .unwrap()
            .iter()
            .all(|b| *b == 7)
    );
    assert!(
        session
            .ram_view(0x12000..0x16000)
            .unwrap()
            .iter()
            .all(|b| *b == 0)
    );
    session.advance(4);
    assert!(session.emulated_nanos() > time);
    assert!(
        session
            .ram_view(0x11000..0x16000)
            .unwrap()
            .iter()
            .all(|b| *b == 7)
    );
    assert_eq!(session.registers().get(Gpr16::Ax), 0x5000);
    assert_eq!(session.disk_read_bytes(), 0x5000);
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn cold_iso_miss_and_persistent_prepare_allocate_nothing_inside_execution() {
    let mut image = test_iso();
    image.resize(4 * 1024 * 1024, 0);
    let sectors = (image.len() / 2048) as u32;
    image[16 * 2048 + 80..16 * 2048 + 84].copy_from_slice(&sectors.to_le_bytes());
    image[16 * 2048 + 84..16 * 2048 + 88].copy_from_slice(&sectors.to_be_bytes());
    let identity = SourceIdentity {
        id: SourceId(7),
        generation: 1,
        length: image.len() as u64,
        validator: [3; 32],
    };
    let mut session = Session::new(&cd_program())
        .unwrap()
        .with_cdrom_source(b'D', CachedSource::new(identity, 1))
        .unwrap();
    session.start();
    let measured = allocation_counter::measure(|| {
        session.advance(1000);
    });
    assert_eq!(measured.count_total, 0);
    let HostRequest::Read {
        tag,
        offset,
        length,
        ..
    } = session.next_host_request().unwrap()
    else {
        panic!()
    };
    assert_eq!(length, 128 * 1024);
    assert_eq!(offset, 0);
    let bytes = image[..length as usize].to_vec();
    session
        .complete_host_request(HostCompletion::Read {
            tag,
            bytes,
            result: Ok(()),
        })
        .unwrap();
    let measured = allocation_counter::measure(|| {
        session.advance(1000);
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    let session = save_session();
    let base = session.base_identity();
    let mut session = session
        .with_persistent_store([1; 32], base, 1, 0, None)
        .unwrap();
    session.start();
    let measured = allocation_counter::measure(|| {
        session.advance(1000);
    });
    assert_eq!(measured.count_total, 0);
    assert!(session.pending_commit());
}
#[test]
fn known_abort_reuses_revision_id_and_lost_ack_retries_identical_mutation() {
    let session = save_session();
    let base = session.base_identity();
    let mut session = session
        .with_persistent_store([1; 32], base, 1, 0, None)
        .unwrap();
    let mut store = RamStore::new([1; 32], base, 1 << 20);
    session.start();
    session.advance(1000);
    let HostRequest::Commit { tag, batch } = session.next_host_request().unwrap() else {
        panic!()
    };
    assert_eq!(batch.mutation_id, 1);
    session
        .complete_host_request(HostCompletion::Commit {
            tag,
            result: Err(StorageError::Quota),
        })
        .unwrap();
    session.advance(1000);
    let HostRequest::Commit { tag, batch } = session.next_host_request().unwrap() else {
        panic!()
    };
    assert_eq!(batch.mutation_id, 1);
    let revision = store.commit(&batch).unwrap();
    session
        .complete_host_request(HostCompletion::Commit {
            tag,
            result: Err(StorageError::Unavailable),
        })
        .unwrap();
    let HostRequest::Commit {
        tag: retry,
        batch: retried,
    } = session.next_host_request().unwrap()
    else {
        panic!()
    };
    assert_eq!(batch, retried);
    assert_ne!(tag.id, retry.id);
    assert_eq!(
        session.complete_host_request(HostCompletion::Commit {
            tag,
            result: Ok(revision)
        }),
        Err(StorageError::StaleCompletion)
    );
    assert_eq!(store.commit(&retried), Ok(revision));
    session
        .complete_host_request(HostCompletion::Commit {
            tag: retry,
            result: Ok(revision),
        })
        .unwrap();
    session.advance(1000);
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert_eq!(session.committed_revision(), Some(1));
}
#[test]
fn unknown_commit_transport_retries_are_finite_and_keep_reset_blocked() {
    let session = save_session();
    let base = session.base_identity();
    let mut session = session
        .with_persistent_store([1; 32], base, 1, 0, None)
        .unwrap();
    session.start();
    session.advance(1000);
    for _ in 0..3 {
        let HostRequest::Commit { tag, .. } = session.next_host_request().unwrap() else {
            panic!()
        };
        session
            .complete_host_request(HostCompletion::Commit {
                tag,
                result: Err(StorageError::Unavailable),
            })
            .unwrap();
    }
    assert!(session.pending_commit());
    assert!(session.unresolved_commit());
    assert!(session.next_host_request().is_none());
    assert!(session.storage_waiting());
    assert!(!session.export_overlay().unwrap().is_empty());
}
#[test]
fn dos_exit_implicitly_closes_and_waits_for_dirty_persistent_files() {
    let mut program = save_program();
    program[21..25].fill(0x90);
    let session = Session::new(&program)
        .unwrap()
        .with_drive(ReadOnlyDrive::new().with_write_overlay(16384));
    let base = session.base_identity();
    let mut session = session
        .with_persistent_store([1; 32], base, 1, 0, None)
        .unwrap();
    let mut store = RamStore::new([1; 32], base, 1 << 20);
    session.start();
    for expected in 1..=2 {
        session.advance(1000);
        assert!(session.pending_commit());
        assert_eq!(session.status(), SessionStatus::Running);
        let HostRequest::Commit { tag, batch } = session.next_host_request().unwrap() else {
            panic!()
        };
        let revision = store.commit(&batch).unwrap();
        assert_eq!(revision, expected);
        session
            .complete_host_request(HostCompletion::Commit {
                tag,
                result: Ok(revision),
            })
            .unwrap();
    }
    assert_eq!(session.status(), SessionStatus::Exited(0));
    assert!(!session.dirty_overlay());
}
#[test]
fn changed_base_and_malformed_snapshot_bounds_are_rejected_without_mutation() {
    let mut lower = ReadOnlyDrive::new().with_write_overlay(8192);
    lower.mount(b"BASE", b"original".to_vec()).unwrap();
    let mut source = Session::new(&[0xEB, 0xFE]).unwrap().with_drive(lower);
    let exported = source.export_overlay().unwrap();
    let mut changed = ReadOnlyDrive::new().with_write_overlay(8192);
    changed.mount(b"BASE", b"changed".to_vec()).unwrap();
    let mut target = Session::new(&[0xEB, 0xFE]).unwrap().with_drive(changed);
    let before = target.export_overlay().unwrap();
    assert_eq!(
        target.import_overlay(&exported),
        Err(StorageError::MissingBase)
    );
    assert_eq!(target.export_overlay().unwrap(), before);
    let mut empty = save_session();
    let valid = empty.export_overlay().unwrap();
    for length in 0..valid.len() {
        assert!(empty.import_overlay(&valid[..length]).is_err());
    }
    assert_eq!(empty.export_overlay().unwrap(), valid);
    let mut malformed = valid.clone();
    malformed[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(empty.import_overlay(&malformed).is_err());
    let mut malformed = valid.clone();
    malformed[44..52].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(empty.import_overlay(&malformed).is_err());
    assert_eq!(empty.export_overlay().unwrap(), valid);
}
#[test]
fn importing_over_persisted_created_files_replaces_old_upper_namespace() {
    fn snapshot(bytes: &[u8]) -> Vec<u8> {
        let mut drive = ReadOnlyDrive::new().with_write_overlay(8192);
        let handle = drive.create(b"SAVE", 0).unwrap();
        drive
            .write_buffer(handle, bytes.len() as u16)
            .unwrap()
            .copy_from_slice(bytes);
        drive.close(handle).unwrap();
        let mut session = Session::new(&[0xEB, 0xFE]).unwrap().with_drive(drive);
        session.export_overlay().unwrap()
    }
    let previous = snapshot(b"old");
    let replacement = snapshot(b"replacement");
    let mut session = save_session();
    let base = session.base_identity();
    session
        .attach_persistent_store([1; 32], base, 1, 2, Some(&previous))
        .unwrap();
    assert!(!session.dirty_overlay());
    session.import_overlay(&replacement).unwrap();
    assert_eq!(session.export_overlay().unwrap(), replacement);
    assert!(session.dirty_overlay());
    session.request_overlay_commit().unwrap();
    let HostRequest::Commit { batch, .. } = session.next_host_request().unwrap() else {
        panic!()
    };
    assert_eq!(batch.payload, replacement);
    assert_eq!(batch.expected_revision, 2);
    assert_eq!(batch.mutation_id, 3);
}
