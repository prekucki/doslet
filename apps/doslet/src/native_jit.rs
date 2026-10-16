//! Host-owned compiler thread. The emulation owner only submits/receives jobs.
#[cfg(all(
    feature = "dynamic-core",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
mod supported {
    use doslet_runtime::{DynamicCompilation, Session};
    use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

    #[derive(Default)]
    pub struct Worker {
        channels: Option<(SyncSender<DynamicCompilation>, Receiver<DynamicCompilation>)>,
        running: bool,
        failed: bool,
    }
    impl Worker {
        pub fn maintain(&mut self, session: &mut Session) {
            if self.failed {
                return;
            }
            if self.running {
                match self.channels.as_ref().unwrap().1.try_recv() {
                    Ok(result) => {
                        session.finish_dynamic_compilation(result);
                        self.running = false;
                    }
                    Err(TryRecvError::Empty) => return,
                    Err(TryRecvError::Disconnected) => {
                        eprintln!("[ERROR doslet] JIT compiler worker stopped; using interpreter");
                        self.failed = true;
                        session.set_dynamic_core(false);
                        return;
                    }
                }
            }
            if !session.dynamic_core_enabled() {
                return;
            }
            if self.channels.is_none() {
                let (submit, jobs) = mpsc::sync_channel::<DynamicCompilation>(1);
                let (complete, results) = mpsc::sync_channel(1);
                if let Err(error) =
                    std::thread::Builder::new()
                        .name("doslet-jit".into())
                        .spawn(move || {
                            while let Ok(job) = jobs.recv() {
                                if complete.send(job.compile()).is_err() {
                                    break;
                                }
                            }
                        })
                {
                    eprintln!(
                        "[ERROR doslet] Cannot start JIT compiler worker: {error}; using interpreter"
                    );
                    self.failed = true;
                    session.set_dynamic_core(false);
                    return;
                }
                self.channels = Some((submit, results));
            }
            if let Some(job) = session.take_dynamic_compilation(2) {
                match self.channels.as_ref().unwrap().0.try_send(job) {
                    Ok(()) => self.running = true,
                    Err(_) => {
                        eprintln!(
                            "[ERROR doslet] JIT compiler queue unavailable; using interpreter"
                        );
                        self.failed = true;
                        session.set_dynamic_core(false);
                    }
                }
            }
        }
    }
    // Dropping both channels lets an in-flight job finish and reclaim its bank
    // on the compiler thread; shutdown/reset never waits for Cranelift.
}
#[cfg(all(
    feature = "dynamic-core",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use supported::Worker;

#[cfg(not(all(
    feature = "dynamic-core",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
#[derive(Default)]
pub struct Worker;
#[cfg(not(all(
    feature = "dynamic-core",
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
impl Worker {
    pub fn maintain(&mut self, _session: &mut doslet_runtime::Session) {}
}
