//! Optional second pass: bound x86 leaves to register-procedure WASM IR.
//!
//! This prepares immutable entry/ABI metadata from an already analyzed image.
//! It does not install runtime hooks or validate live source bytes. The first
//! execution adapter must explicitly assume frozen code and preserve the real
//! entering CALL/outer RET. Internal calls, memory and explicit SP/ESP effects
//! are rejected until their guest-memory obligations are implemented.

use super::{Budget, LoweringError, decode_node, register_operation};
use crate::{ObjectAddress, binding::DosFlatBindingPlan, procedures::ProcedurePlan};
use alloc::vec::Vec;
use doslet_x86_wasm::static_program::{
    Operation,
    procedure::{
        MAX_PROCEDURES, MAX_TOTAL_OPERATIONS, ProcedureFunction, ProcedureId, ProcedureOperation,
        ProcedureProgram,
    },
};
use iced_x86::{OpKind, Register};

#[derive(Clone, Copy, Debug)]
pub struct ProcedureLoweringLimits {
    pub max_input_procedures: usize,
    pub max_input_instructions: usize,
    pub max_owned_bytes: usize,
}
impl Default for ProcedureLoweringLimits {
    fn default() -> Self {
        Self {
            max_input_procedures: MAX_PROCEDURES,
            max_input_instructions: MAX_TOTAL_OPERATIONS,
            max_owned_bytes: 16 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterProcedureAbi {
    /// All GPR/flags by value; unchanged ESP; owner performs final RET32.
    RegisterLeafNear32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RegisterProcedureEntry {
    pub function_id: ProcedureId,
    pub entry: ObjectAddress,
    pub linear_address: u32,
    pub abi: RegisterProcedureAbi,
    /// Includes final guest RET, but excludes the entering guest CALL.
    pub instruction_cost: u32,
    pub byte_len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureLoweringRejection {
    ReturnAbi,
    StackRegister,
    UnsupportedInstruction,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureLoweringDiagnostic {
    pub entry: ObjectAddress,
    pub instruction: ObjectAddress,
    pub reason: ProcedureLoweringRejection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureLoweringError {
    DifferentBinding,
    InputLimit,
    Lowering(LoweringError),
}
impl core::fmt::Display for ProcedureLoweringError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "register procedure lowering: {self:?}")
    }
}
impl core::error::Error for ProcedureLoweringError {}
impl From<LoweringError> for ProcedureLoweringError {
    fn from(value: LoweringError) -> Self {
        Self::Lowering(value)
    }
}

#[derive(Debug)]
pub struct PreparedRegisterProcedures {
    program: ProcedureProgram,
    entries: Vec<RegisterProcedureEntry>,
    diagnostics: Vec<ProcedureLoweringDiagnostic>,
}
impl PreparedRegisterProcedures {
    /// Frozen-image preparation only. The returned entry list is not a live
    /// source-validity proof and carries no permanent speed-tier classification.
    pub fn from_bound_leaves(
        plan: &ProcedurePlan<'_, '_>,
        binding: &DosFlatBindingPlan<'_>,
        limits: ProcedureLoweringLimits,
    ) -> Result<Self, ProcedureLoweringError> {
        if !plan.graph().matches_binding(binding) {
            return Err(ProcedureLoweringError::DifferentBinding);
        }
        if plan.procedures().len() > limits.max_input_procedures
            || plan.procedures().len() > MAX_PROCEDURES
        {
            return Err(ProcedureLoweringError::InputLimit);
        }
        let count = plan
            .procedures()
            .iter()
            .try_fold(0_usize, |count, procedure| {
                count
                    .checked_add(procedure.instruction_count())
                    .filter(|&count| count <= limits.max_input_instructions)
            });
        if count.is_none() {
            return Err(ProcedureLoweringError::InputLimit);
        }
        let mut budget = Budget(limits.max_owned_bytes);
        let mut result = Self {
            program: ProcedureProgram::default(),
            entries: Vec::new(),
            diagnostics: Vec::new(),
        };
        budget.reserve(&mut result.program.functions, plan.procedures().len())?;
        budget.reserve(&mut result.entries, plan.procedures().len())?;
        budget.reserve(&mut result.diagnostics, plan.procedures().len())?;
        for (index, procedure) in plan.procedures().iter().enumerate() {
            let rejection = |instruction, reason| ProcedureLoweringDiagnostic {
                entry: procedure.entry,
                instruction,
                reason,
            };
            if procedure.return_operand_width != 4 || procedure.return_pop_bytes != 0 {
                result.diagnostics.push(rejection(
                    procedure.ret,
                    ProcedureLoweringRejection::ReturnAbi,
                ));
                continue;
            }
            let mut operations = Vec::new();
            budget.reserve(
                &mut operations,
                procedure.instruction_count().saturating_sub(1),
            )?;
            let mut rejected = None;
            for node in plan
                .instructions(index)
                .ok_or(ProcedureLoweringError::InputLimit)?
            {
                if node.address == procedure.ret {
                    break;
                }
                let decoded = decode_node(node, plan.graph(), binding)?;
                if (0..decoded.op_count()).any(|operand| {
                    decoded.op_kind(operand) == OpKind::Register
                        && matches!(decoded.op_register(operand), Register::SP | Register::ESP)
                }) {
                    rejected = Some(rejection(
                        node.address,
                        ProcedureLoweringRejection::StackRegister,
                    ));
                    break;
                }
                match register_operation(&decoded) {
                    Some(Operation::Lea { .. }) | None => {
                        rejected = Some(rejection(
                            node.address,
                            ProcedureLoweringRejection::UnsupportedInstruction,
                        ));
                        break;
                    }
                    Some(operation) => operations.push(ProcedureOperation::Execute(operation)),
                }
            }
            if let Some(diagnostic) = rejected {
                result.diagnostics.push(diagnostic);
                continue;
            }
            let function_id = ProcedureId(
                u32::try_from(result.program.functions.len())
                    .map_err(|_| ProcedureLoweringError::InputLimit)?,
            );
            let linear_address = binding
                .object_binding(procedure.entry.object_number)
                .and_then(|object| object.linear_base.checked_add(procedure.entry.offset))
                .ok_or(ProcedureLoweringError::InputLimit)?;
            result.program.functions.push(ProcedureFunction {
                id: function_id,
                operations,
            });
            result.entries.push(RegisterProcedureEntry {
                function_id,
                entry: procedure.entry,
                linear_address,
                abi: RegisterProcedureAbi::RegisterLeafNear32,
                instruction_cost: u32::try_from(procedure.instruction_count())
                    .map_err(|_| ProcedureLoweringError::InputLimit)?,
                byte_len: procedure.byte_len,
            });
        }
        Ok(result)
    }
    pub fn program(&self) -> &ProcedureProgram {
        &self.program
    }
    pub fn entries(&self) -> &[RegisterProcedureEntry] {
        &self.entries
    }
    pub fn diagnostics(&self) -> &[ProcedureLoweringDiagnostic] {
        &self.diagnostics
    }
}
