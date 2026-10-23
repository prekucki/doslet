//! One bounded compilation job per CPU. Only generated WASM bytes and an
//! Engine cross this boundary; the Store, devices and RAM never leave their
//! exclusive emulation owner.
use super::*;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};

pub(super) struct Completed {
    pub pending: Pending,
    pub module: Module,
    pub nanos: u64,
}

pub(super) struct Compiler {
    jobs: SyncSender<Pending>,
    results: Receiver<Result<Completed>>,
    busy: bool,
}

impl Compiler {
    pub fn is_busy(&self) -> bool {
        self.busy
    }
    pub fn new(engine: Engine) -> Result<Self> {
        let (jobs, input) = mpsc::sync_channel::<Pending>(1);
        let (output, results) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("doslet-v86-jit".into())
            .spawn(move || {
                while let Ok(pending) = input.recv() {
                    let started = std::time::Instant::now();
                    let result = Module::new(&engine, &pending.bytes).map(|module| Completed {
                        pending,
                        module,
                        nanos: started.elapsed().as_nanos() as u64,
                    });
                    if output.send(result).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            jobs,
            results,
            busy: false,
        })
    }

    pub fn submit(&mut self, pending: Pending) -> Result<()> {
        if self.busy {
            return Err(wasmtime::Error::msg("overlapping v86 native compilation"));
        }
        self.jobs
            .try_send(pending)
            .map_err(|_| wasmtime::Error::msg("v86 compiler queue unavailable"))?;
        self.busy = true;
        Ok(())
    }

    pub fn poll(&mut self, blocking: bool) -> Result<Option<Completed>> {
        if !self.busy {
            return Ok(None);
        }
        let result = if blocking {
            self.results
                .recv()
                .map_err(|_| wasmtime::Error::msg("v86 compiler disconnected"))?
        } else {
            match self.results.try_recv() {
                Ok(result) => result,
                Err(TryRecvError::Empty) => return Ok(None),
                Err(TryRecvError::Disconnected) => {
                    return Err(wasmtime::Error::msg("v86 compiler disconnected"));
                }
            }
        };
        self.busy = false;
        result.map(Some)
    }
}
