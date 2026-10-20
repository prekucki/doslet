//! Native acceptance adapter for the static-program ABI, separate from ABI-v1.
//!
//! The runner owns imported memory and a runtime callback. Only the 72-byte
//! frame crosses this native test boundary; guest memory belongs to the runtime.
//! Browser and WASM-to-WASM linking are separate integration tests.

use crate::Artifact;
use doslet_x86_wasm::static_program::{Frame, abi};
use wasmtime::{Caller, Linker, Memory, MemoryType, Store, TypedFunc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    Service,
    Fallback,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Continue,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Budget,
    Runtime,
}

/// Synchronous service using small boundary state. An error stops this runner;
/// partially completed runtime effects are never replayed automatically.
pub trait Runtime {
    type Error: core::fmt::Display;
    fn callback(&mut self, frame: &mut Frame, reason: Reason) -> Result<Control, Self::Error>;

    /// Reserve permission for a whole region before any effects. This method
    /// must not change architectural state or device time, or reenter generated
    /// execution. The frame is intentionally unavailable: its memory copy can be
    /// stale while generated registers are live. Reconcile successful reserved
    /// work at callbacks and returns. See the static compiler's admission contract.
    fn admit(&mut self, _region_id: u32, _required: u32) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

struct State<R> {
    runtime: R,
    error: Option<String>,
}

pub struct Runner<R: 'static> {
    store: Store<State<R>>,
    memory: Memory,
    run: TypedFunc<u32, u32>,
    failed: bool,
}

impl Artifact {
    pub fn instantiate_static<R: Runtime + 'static>(
        &self,
        runtime: R,
    ) -> Result<Runner<R>, String> {
        let mut store = Store::new(
            &self.engine,
            State {
                runtime,
                error: None,
            },
        );
        let memory =
            Memory::new(&mut store, MemoryType::new(1, None)).map_err(|e| e.to_string())?;
        let mut linker = Linker::new(&self.engine);
        linker
            .define(&store, abi::IMPORT_MODULE, abi::MEMORY_IMPORT, memory)
            .map_err(|e| e.to_string())?;
        linker
            .func_wrap(
                abi::IMPORT_MODULE,
                abi::RUNTIME_IMPORT,
                |mut caller: Caller<'_, State<R>>, pointer: u32, reason: u32| -> u32 {
                    let result = (|| {
                        let memory = caller
                            .get_export("memory")
                            .and_then(|e| e.into_memory())
                            .ok_or("static module has no memory export")?;
                        let (bytes, state) = memory.data_and_store_mut(&mut caller);
                        let start = usize::try_from(pointer)
                            .map_err(|_| "frame pointer does not fit host")?;
                        let bytes = bytes
                            .get_mut(start..)
                            .ok_or("frame pointer outside memory")?;
                        let mut frame = Frame::read(bytes).map_err(|e| e.to_string())?;
                        let reason = match reason {
                            abi::REASON_SERVICE => Reason::Service,
                            abi::REASON_FALLBACK => Reason::Fallback,
                            _ => return Err("unknown static callback reason".into()),
                        };
                        let control = state
                            .runtime
                            .callback(&mut frame, reason)
                            .map_err(|e| e.to_string())?;
                        frame.write(bytes).map_err(|e| e.to_string())?;
                        Ok::<_, String>(control)
                    })();
                    match result {
                        Ok(Control::Continue) => abi::CONTINUE,
                        Ok(Control::Exit) => abi::EXIT,
                        Err(error) => {
                            caller.data_mut().error = Some(error);
                            abi::EXIT
                        }
                    }
                },
            )
            .map_err(|e| e.to_string())?;
        linker
            .func_wrap(
                abi::IMPORT_MODULE,
                abi::ADMIT_IMPORT,
                |mut caller: Caller<'_, State<R>>,
                 pointer: u32,
                 region_id: u32,
                 required: u32|
                 -> wasmtime::Result<u32> {
                    let result = if pointer == 0 {
                        caller
                            .data_mut()
                            .runtime
                            .admit(region_id, required)
                            .map(u32::from)
                            .map_err(|e| e.to_string())
                    } else {
                        Err("static admission received an unexpected frame pointer".into())
                    };
                    result.map_err(|error| {
                        caller.data_mut().error = Some(error.clone());
                        wasmtime::Error::msg(error)
                    })
                },
            )
            .map_err(|e| e.to_string())?;
        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(|e| e.to_string())?;
        let run = instance
            .get_typed_func::<u32, u32>(&mut store, abi::RUN_EXPORT)
            .map_err(|e| e.to_string())?;
        Ok(Runner {
            store,
            memory,
            run,
            failed: false,
        })
    }
}

impl<R: Runtime + 'static> Runner<R> {
    pub fn runtime(&self) -> &R {
        &self.store.data().runtime
    }
    pub fn runtime_mut(&mut self) -> &mut R {
        &mut self.store.data_mut().runtime
    }

    pub fn run(&mut self, frame: &mut Frame) -> Result<Exit, String> {
        if self.failed {
            return Err("static AOT runner stopped after an error".into());
        }
        frame
            .write(self.memory.data_mut(&mut self.store))
            .map_err(|e| e.to_string())?;
        let result = self.run.call(&mut self.store, 0);
        // Preserve the last boundary frame even after a runtime error. Runtime
        // or trap effects may be partial, so every error permanently stops entry.
        *frame = Frame::read(self.memory.data(&self.store)).map_err(|e| e.to_string())?;
        if let Some(error) = self.store.data_mut().error.take() {
            self.failed = true;
            return Err(format!("static AOT runtime: {error}"));
        }
        match result {
            Ok(abi::EXIT_BUDGET) => Ok(Exit::Budget),
            Ok(abi::EXIT_RUNTIME) => Ok(Exit::Runtime),
            Ok(other) => {
                self.failed = true;
                Err(format!("invalid static AOT exit {other}"))
            }
            Err(error) => {
                self.failed = true;
                Err(format!("static AOT execution: {error}"))
            }
        }
    }
}

#[cfg(test)]
mod tests;
