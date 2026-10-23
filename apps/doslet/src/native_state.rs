//! Native file adapter only; embedded media stay local to the restored Session.
use doslet_runtime::{Session, SessionStatus};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::Path,
};

pub fn save(path: &Path, session: &mut Session) -> Result<(), Box<dyn std::error::Error>> {
    if session.v86_enabled() {
        return Err("saving v86 CPU state is not implemented".into());
    }
    session.detach_persistent_store()?;
    let bytes = session.save_state()?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let file = File::create(&tmp)?;
        let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        encoder.write_all(&bytes)?;
        let file = encoder.finish()?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn load(
    path: &Path,
    jit: bool,
    media: doslet_runtime::session::StateResources,
) -> Result<Box<Session>, Box<dyn std::error::Error>> {
    // Protobuf reconstruction has larger debug-build frames than execution.
    // Prepare one new owner on a bounded setup stack, then transfer ownership;
    // no guest memory is cloned and this thread never runs the guest.
    let path = path.to_owned();
    std::thread::Builder::new()
        .name("doslet-state-load".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || decode(&path, jit, media).map_err(|e| e.to_string()))?
        .join()
        .map_err(|_| "snapshot decoder thread failed")?
        .map_err(Into::into)
}

fn decode(
    path: &Path,
    jit: bool,
    media: doslet_runtime::session::StateResources,
) -> Result<Box<Session>, Box<dyn std::error::Error>> {
    let file = File::open(path)?;
    let reader: Box<dyn Read> = if path.extension().is_some_and(|e| e == "gz") {
        Box::new(flate2::read::GzDecoder::new(file))
    } else {
        Box::new(file)
    };
    let mut bytes = Vec::new();
    reader
        .take(1024 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    let mut session = Session::load_state_with_resources(&bytes, &media)?;
    session.renew_epoch()?;
    if !matches!(
        session.status(),
        SessionStatus::Running | SessionStatus::Ready | SessionStatus::Paused
    ) {
        return Err("snapshot cannot resume execution".into());
    }
    session.set_dynamic_core(jit);
    // Historical presentation queues are not played again. Device/DMA state is retained.
    let mut pcm = [doslet_pcm::EMPTY; 256];
    while session.drain_pcm_timed(&mut pcm) != 0 {}
    let mut midi = [0; 1024];
    while session.drain_midi(&mut midi) != 0 {}
    session.start();
    Ok(Box::new(session))
}
