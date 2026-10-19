//! Native acceptance owner for direct-memory procedure exports.
//!
//! RAM lives in one Wasmtime memory. Native reference execution may borrow that
//! backing through `memory_mut`, releasing the borrow before entering WASM.
//! This adapter supplies no device scheduler, source validation or IRQ delivery;
//! its caller must satisfy the compiler's frozen-code and bounded-work contract.

use doslet_x86_wasm::static_program::{
    abi, memory,
    procedure_memory::{COMPLETE, CompiledProcedureProgram, ProcedureId, SIDE_EXIT},
};
use wasmtime::{Instance, Memory, MemoryType, Store, TypedFunc};

mod shared;
pub use shared::{SharedProcedureLayout, SharedProcedureMemory};
pub mod rle;

pub struct MemoryProcedureArtifact {
    artifact: super::Artifact,
    exports: Vec<(ProcedureId, String)>,
}

pub struct MemoryProcedureRunner {
    store: Store<()>,
    memory: Memory,
    function: TypedFunc<u32, u32>,
    failed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Complete,
    SideExit,
}

impl MemoryProcedureArtifact {
    pub fn compile(program: &CompiledProcedureProgram) -> Result<Self, String> {
        let artifact = super::Artifact::compile(program.wasm())?;
        let exports = program
            .procedure_ids()
            .map(|id| {
                (
                    id,
                    program
                        .export_name(id)
                        .expect("compiler metadata")
                        .to_owned(),
                )
            })
            .collect();
        Ok(Self { artifact, exports })
    }

    pub fn compile_seconds(&self) -> f64 {
        self.artifact.compile_seconds
    }

    pub fn load_seconds(&self) -> f64 {
        self.artifact.load_seconds
    }

    /// Allocate a fixed-size backing; generated code has no memory-grow import.
    pub fn instantiate(
        &self,
        id: ProcedureId,
        pages: u32,
    ) -> Result<MemoryProcedureRunner, String> {
        let name = self
            .exports
            .iter()
            .find(|(key, _)| *key == id)
            .map(|(_, name)| name)
            .ok_or("unknown memory procedure")?;
        let mut store = Store::new(&self.artifact.engine, ());
        let memory = Memory::new(&mut store, MemoryType::new(pages, Some(pages)))
            .map_err(|e| format!("procedure memory: {e:#}"))?;
        let instance = Instance::new(&mut store, &self.artifact.module, &[memory.into()])
            .map_err(|e| format!("memory procedure instantiation: {e:#}"))?;
        let function = instance
            .get_typed_func::<u32, u32>(&mut store, name)
            .map_err(|e| format!("memory procedure export: {e:#}"))?;
        Ok(MemoryProcedureRunner {
            store,
            memory,
            function,
            failed: false,
        })
    }
}

impl MemoryProcedureRunner {
    pub fn memory(&self) -> &[u8] {
        self.memory.data(&self.store)
    }

    pub fn memory_mut(&mut self) -> &mut [u8] {
        self.memory.data_mut(&mut self.store)
    }

    /// Execute an already initialized boundary frame. A trap can follow guest
    /// stores: poison the runner so an accidental retry cannot replay them.
    pub fn call(&mut self, frame_pointer: u32) -> Result<Exit, String> {
        if self.failed {
            return Err("memory procedure runner stopped after an execution failure".into());
        }
        let start = frame_pointer as usize;
        let end = start
            .checked_add(abi::FRAME_BYTES + memory::BYTES)
            .ok_or("procedure frame overflow")?;
        if end > self.memory.data_size(&self.store) {
            return Err("procedure frame outside linear memory".into());
        }
        match self.function.call(&mut self.store, frame_pointer) {
            Ok(COMPLETE) => Ok(Exit::Complete),
            Ok(SIDE_EXIT) => Ok(Exit::SideExit),
            Ok(status) => {
                self.failed = true;
                Err(format!("unexpected memory procedure status {status}"))
            }
            Err(error) => {
                self.failed = true;
                Err(format!("memory procedure execution: {error:#}"))
            }
        }
    }
}
