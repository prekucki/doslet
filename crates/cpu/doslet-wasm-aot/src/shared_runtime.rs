//! Two WASM instances with one memory and a direct WASM runtime callback.
//!
//! The runtime exports `memory` and `static_runtime(i32, i32) -> i32`. The
//! generated program imports both. The linker binds the runtime's WASM function
//! directly; there is no Rust callback, dispatch-buffer clearing or RAM copy.
//! The runtime owns frame allocation and machine state. Native frame transfers
//! below are explicit setup/inspection operations, outside generated execution.
//! Admitted programs additionally require `static_admit(i32, i32, i32) -> i32`.
//! It is linked directly as `doslet_static.admit` and obeys the static compiler's
//! admission contract, including stale-frame and reservation reconciliation rules.
//! Optional memory-store helpers bind `static_memory_write` directly as
//! `doslet_static.memory_write(i32, i32, i32, i32) -> i32`.

use crate::{Artifact, static_program::Exit};
use doslet_x86_wasm::static_program::{Frame, abi};
use wasmtime::{Instance, Linker, Memory, Store, TypedFunc, WasmParams, WasmResults};

pub struct SharedArtifacts {
    pub runtime: Artifact,
    pub program: Artifact,
}

impl SharedArtifacts {
    pub fn compile(runtime: &[u8], program: &[u8]) -> Result<Self, String> {
        let runtime = Artifact::compile(runtime)?;
        let program = Artifact::compile_on(&runtime.engine, program)?;
        Ok(Self { runtime, program })
    }

    pub fn instantiate(&self) -> Result<SharedRunner, String> {
        let mut store = Store::new(&self.runtime.engine, ());
        let mut linker = Linker::new(&self.runtime.engine);
        let runtime = linker
            .instantiate(&mut store, &self.runtime.module)
            .map_err(|e| e.to_string())?;
        let memory = runtime
            .get_memory(&mut store, "memory")
            .ok_or("WASM runtime has no memory export")?;
        let callback = runtime
            .get_typed_func::<(u32, u32), u32>(&mut store, "static_runtime")
            .map_err(|e| e.to_string())?;
        linker
            .define(&store, abi::IMPORT_MODULE, abi::MEMORY_IMPORT, memory)
            .map_err(|e| e.to_string())?;
        linker
            .define(
                &store,
                abi::IMPORT_MODULE,
                abi::RUNTIME_IMPORT,
                *callback.func(),
            )
            .map_err(|e| e.to_string())?;
        if let Some(export) = runtime.get_export(&mut store, "static_admit") {
            let admit = export.into_func().ok_or(
                "WASM runtime static_admit must be a function with signature (i32, i32, i32) -> i32",
            )?;
            admit.typed::<(u32, u32, u32), u32>(&store).map_err(|e| {
                format!("WASM runtime static_admit must have signature (i32, i32, i32) -> i32: {e}")
            })?;
            linker
                .define(&store, abi::IMPORT_MODULE, abi::ADMIT_IMPORT, admit)
                .map_err(|e| e.to_string())?;
        } else if self.program.module.imports().any(|import| {
            import.module() == abi::IMPORT_MODULE && import.name() == abi::ADMIT_IMPORT
        }) {
            return Err(
                "admitted WASM program requires runtime export static_admit(i32, i32, i32) -> i32"
                    .into(),
            );
        }
        if let Some(export) = runtime.get_export(&mut store, "static_memory_write") {
            let write = export
                .into_func()
                .ok_or("WASM runtime static_memory_write must be a function")?;
            write
                .typed::<(u32, u32, u32, u32), u32>(&store)
                .map_err(|e| format!("WASM runtime static_memory_write must have signature (i32, i32, i32, i32) -> i32: {e}"))?;
            linker
                .define(&store, abi::IMPORT_MODULE, abi::MEMORY_WRITE_IMPORT, write)
                .map_err(|e| e.to_string())?;
        } else if self.program.module.imports().any(|import| {
            import.module() == abi::IMPORT_MODULE && import.name() == abi::MEMORY_WRITE_IMPORT
        }) {
            return Err("WASM memory-write helpers require runtime export static_memory_write(i32, i32, i32, i32) -> i32".into());
        }
        let program = linker
            .instantiate(&mut store, &self.program.module)
            .map_err(|e| e.to_string())?;
        let run = program
            .get_typed_func::<u32, u32>(&mut store, abi::RUN_EXPORT)
            .map_err(|e| e.to_string())?;
        Ok(SharedRunner {
            store,
            runtime,
            memory,
            run,
            failed: false,
        })
    }
}

pub struct SharedRunner {
    store: Store<()>,
    runtime: Instance,
    memory: Memory,
    run: TypedFunc<u32, u32>,
    failed: bool,
}

impl SharedRunner {
    /// Copy bounded fixture data into a buffer allocated by the runtime. This
    /// setup operation must finish before generated execution starts.
    pub fn write_bytes(&mut self, pointer: u32, bytes: &[u8]) -> Result<(), String> {
        if self.failed {
            return Err("shared WASM runner stopped after a trap".into());
        }
        self.memory
            .write(&mut self.store, pointer as usize, bytes)
            .map_err(|e| e.to_string())
    }

    /// Borrow an exported output buffer for setup/verification. The borrow ends
    /// before another runtime call can mutate or grow the shared memory.
    pub fn memory_view(&self, pointer: u32, length: u32) -> Result<&[u8], String> {
        let start = usize::try_from(pointer).map_err(|_| "output pointer does not fit host")?;
        let end = start
            .checked_add(length as usize)
            .ok_or("output extent overflow")?;
        self.memory
            .data(&self.store)
            .get(start..end)
            .ok_or_else(|| "output outside shared memory".into())
    }
    /// Runtime setup, reference execution and inspection exports remain typed.
    /// A trap stops subsequent execution rather than replaying partial effects.
    pub fn call_runtime<P: WasmParams, R: WasmResults>(
        &mut self,
        export: &str,
        parameters: P,
    ) -> Result<R, String> {
        if self.failed {
            return Err("shared WASM runner stopped after a trap".into());
        }
        let function = self
            .runtime
            .get_typed_func::<P, R>(&mut self.store, export)
            .map_err(|e| e.to_string())?;
        function.call(&mut self.store, parameters).map_err(|e| {
            self.failed = true;
            e.to_string()
        })
    }

    pub fn run(&mut self, frame_pointer: u32) -> Result<Exit, String> {
        if self.failed {
            return Err("shared WASM runner stopped after a trap".into());
        }
        match self.run.call(&mut self.store, frame_pointer) {
            Ok(abi::EXIT_BUDGET) => Ok(Exit::Budget),
            Ok(abi::EXIT_RUNTIME) => Ok(Exit::Runtime),
            Ok(other) => {
                self.failed = true;
                Err(format!("invalid shared WASM exit {other}"))
            }
            Err(error) => {
                self.failed = true;
                Err(format!("shared WASM execution: {error}"))
            }
        }
    }

    pub fn read_frame(&self, pointer: u32) -> Result<Frame, String> {
        let mut bytes = [0; abi::FRAME_BYTES];
        self.memory
            .read(&self.store, pointer as usize, &mut bytes)
            .map_err(|e| e.to_string())?;
        Frame::read(&bytes).map_err(|e| e.to_string())
    }

    pub fn write_frame(&mut self, pointer: u32, frame: &Frame) -> Result<(), String> {
        // Frame::write intentionally preserves reserved words.
        let mut bytes = [0; abi::FRAME_BYTES];
        self.memory
            .read(&self.store, pointer as usize, &mut bytes)
            .map_err(|e| e.to_string())?;
        frame.write(&mut bytes).map_err(|e| e.to_string())?;
        self.memory
            .write(&mut self.store, pointer as usize, &bytes)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests;
