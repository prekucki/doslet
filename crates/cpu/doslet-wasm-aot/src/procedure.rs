//! Native AOT execution of explicitly supplied, pure-register procedure IR.
//!
//! This adapter owns no guest memory. It does not enter/return from a guest
//! procedure, validate source bytes or materialize guest call frames. Those
//! obligations belong to the future interpreter integration.

use doslet_x86_wasm::static_program::procedure::{CompiledProcedureProgram, ProcedureId};
use wasmtime::{Instance, Store, TypedFunc};

type Arguments = (u32, u32, u32, u32, u32, u32, u32, u32, u32);
type Results = (u32, u32, u32, u32, u32, u32, u32, u32, u32, u64);

pub struct ProcedureArtifact {
    artifact: super::Artifact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureResult {
    pub gpr: [u32; 8],
    pub eflags: u32,
    /// Guest scalar instructions, CALLs and RETs, including repeated callees.
    pub completed_instructions: u64,
}

pub struct ProcedureRunner {
    store: Store<()>,
    function: TypedFunc<Arguments, Results>,
    id: ProcedureId,
}

impl ProcedureArtifact {
    /// Cranelift Speed AOT, followed by loading this instance's own artifact.
    pub fn compile(program: &CompiledProcedureProgram) -> Result<Self, String> {
        Ok(Self {
            artifact: super::Artifact::compile(program.wasm())?,
        })
    }

    pub fn compile_seconds(&self) -> f64 {
        self.artifact.compile_seconds
    }
    pub fn load_seconds(&self) -> f64 {
        self.artifact.load_seconds
    }

    pub fn instantiate(&self, id: ProcedureId) -> Result<ProcedureRunner, String> {
        let mut store = Store::new(&self.artifact.engine, ());
        let instance = Instance::new(&mut store, &self.artifact.module, &[])
            .map_err(|error| format!("procedure instantiation: {error:#}"))?;
        let name = format!("procedure_{}", id.0);
        let function = instance
            .get_typed_func::<Arguments, Results>(&mut store, &name)
            .map_err(|error| format!("procedure export {name}: {error:#}"))?;
        Ok(ProcedureRunner {
            store,
            function,
            id,
        })
    }
}

impl ProcedureRunner {
    pub fn procedure_id(&self) -> ProcedureId {
        self.id
    }
    /// Pure register execution. Guest ESP is an unchanged value in this ABI.
    pub fn call(&mut self, gpr: [u32; 8], eflags: u32) -> Result<ProcedureResult, String> {
        let (a, b, c, d, e, f, g, h, flags, work) = self
            .function
            .call(
                &mut self.store,
                (
                    gpr[0], gpr[1], gpr[2], gpr[3], gpr[4], gpr[5], gpr[6], gpr[7], eflags,
                ),
            )
            .map_err(|error| format!("procedure execution: {error:#}"))?;
        Ok(ProcedureResult {
            gpr: [a, b, c, d, e, f, g, h],
            eflags: flags,
            completed_instructions: work,
        })
    }
}

#[cfg(feature = "procedure-calls")]
impl doslet_x86::procedure_calls::ProcedureExecutor for ProcedureRunner {
    type Error = String;

    fn execute(
        &mut self,
        function_id: u32,
        input: doslet_x86::procedure_calls::ProcedureRegisters,
    ) -> Result<doslet_x86::procedure_calls::ProcedureResult, Self::Error> {
        if function_id != self.id.0 {
            return Err(format!(
                "procedure runner bound to {}, requested {function_id}",
                self.id.0
            ));
        }
        let result = self.call(input.gpr, input.flags)?;
        Ok(doslet_x86::procedure_calls::ProcedureResult {
            registers: doslet_x86::procedure_calls::ProcedureRegisters {
                gpr: result.gpr,
                flags: result.eflags,
            },
            instructions: u32::try_from(result.completed_instructions)
                .map_err(|_| "procedure work exceeds u32".to_owned())?,
        })
    }
}
