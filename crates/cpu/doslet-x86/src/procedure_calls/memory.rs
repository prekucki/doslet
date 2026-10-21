//! Entry dispatch for frozen procedures executing directly in owned guest RAM.
//!
//! The backend executes stack operations and the actual outer near RET, and
//! returns CPU state at an instruction boundary with exact completed work.
//! The entering CALL has already executed. Unlike the register-only leaf ABI,
//! this hook never reads a presumed return slot or adjusts ESP on return.
//!
//! Registration is a trusted compiler/owner proof. Code and its mappings must
//! remain frozen, and generated stores must reject protected code pages before
//! effects. The backend must validate live segment windows, complete RAM spans,
//! mappings and write-invalidation requirements before each instruction's first
//! effect, or preflight the entire finite procedure before any effect.
//! Unsupported operations and unavailable memory exit before that
//! instruction, preserving an exact completed prefix. Ports, segment/control
//! changes, mode changes and internal guest CALLs are excluded. An explicitly
//! admitted coprocessor backend stages its small result and commits through
//! `finish_memory_procedure` after the scalar prefix has passed validation.
//!
//! A backend error or malformed result is fatal for this invocation: RAM may
//! already contain committed stores although CPU state has not been committed.
//! There is no rollback. The owner must stop and must not interpret the entry
//! again after such an error. An ordinary side exit instead commits the exact
//! prefix state; interpretation resumes with the *current* instruction once.

use super::{
    FrozenCode, MAX_PROCEDURE_ENTRIES, ProcedureContext, ProcedureDecline, ProcedureRegisters,
};
use crate::{Access, Cpu, Gpr32, SegmentRegister, flags};
use doslet_bus::MemoryBus;

/// Whole proved computations return their actual u32 work count. The caller
/// supplies their admission proof independently of its soft interpreter budget.
pub const MAX_MEMORY_PROCEDURE_INSTRUCTIONS: u32 = u32::MAX;

/// Trusted binding for a whole procedure. The maximum includes its outer RET
/// and excludes its entering CALL; acyclic paths may consume less work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryProcedureEntry {
    pub entry_ip: u32,
    pub context: ProcedureContext,
    pub function_id: u32,
    pub maximum_instructions: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryProcedureRegistryError {
    TooManyEntries,
    EntriesNotStrictlySorted,
    InvalidMaximumInstructions,
}

/// Borrowed bindings sorted strictly by entry EIP. No execution allocates or
/// compiles; the owner supplies both the frozen-code proof and prepared backend.
#[derive(Debug)]
pub struct MemoryProcedureRegistry<'a> {
    entries: &'a [MemoryProcedureEntry],
}

impl<'a> MemoryProcedureRegistry<'a> {
    pub fn new(
        entries: &'a [MemoryProcedureEntry],
        _frozen_code: FrozenCode,
    ) -> Result<Self, MemoryProcedureRegistryError> {
        if entries.len() > MAX_PROCEDURE_ENTRIES {
            return Err(MemoryProcedureRegistryError::TooManyEntries);
        }
        if entries
            .windows(2)
            .any(|pair| pair[0].entry_ip >= pair[1].entry_ip)
        {
            return Err(MemoryProcedureRegistryError::EntriesNotStrictlySorted);
        }
        if entries.iter().any(|entry| entry.maximum_instructions == 0) {
            return Err(MemoryProcedureRegistryError::InvalidMaximumInstructions);
        }
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &'a [MemoryProcedureEntry] {
        self.entries
    }

    fn find(&self, ip: u32) -> Option<&MemoryProcedureEntry> {
        self.entries
            .binary_search_by_key(&ip, |entry| entry.entry_ip)
            .ok()
            .map(|index| &self.entries[index])
    }
}

/// A single mutable owner of guest RAM and its prepared procedure backend.
///
/// Execution must be bounded and allocation-free after setup, using the actual
/// RAM backing with no copy. Build and validate every memory/segment context
/// from this invocation's live `cpu`, including DS/ES/FS/GS when used. Validate
/// each complete memory access and any RET/branch target before its first effect;
/// reject code-page stores and preserve bus invalidation/accounting contracts.
/// An unsupported instruction or failed access proof returns a side exit before
/// that instruction. All earlier completed instructions remain committed.
///
/// `cpu` is a temporary read-only view of the sole CPU. Do not retain its borrow
/// or reenter execution; copy only small register/window values into the backend
/// before borrowing its owned RAM/runtime. Never create a second mutable CPU.
pub trait MemoryProcedureBus: MemoryBus {
    type Error;

    fn execute_memory_procedure(
        &mut self,
        function_id: u32,
        cpu: &Cpu,
    ) -> Result<MemoryProcedureResult, Self::Error>;

    /// Commit any small coprocessor result after the CPU has validated the
    /// returned scalar prefix. Failure must leave the coprocessor unchanged.
    /// Scalar backends have no coprocessor state to transfer.
    fn finish_memory_procedure(
        &mut self,
        _fpu: Option<&mut MemoryProcedureFpu>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Coprocessor state borrowed only while committing a validated procedure.
pub type MemoryProcedureFpu = doslet_x87::X87;

/// State after an exact prefix of completed instructions. A side exit points to
/// the first instruction still to interpret; completion includes the real RET.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryProcedureResult {
    pub registers: ProcedureRegisters,
    pub instruction_pointer: u32,
    pub instructions: u32,
    pub completed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryProcedureOutcome {
    /// No backend invocation and no guest effects; ordinary stepping may follow.
    Declined(ProcedureDecline),
    Complete {
        instructions: u32,
    },
    /// CPU and RAM contain this exact prefix. Resume at the returned current
    /// instruction, never replay the entry or its committed prefix.
    SideExit {
        instructions: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryProcedureResultError {
    InstructionCount,
    ControlFlags,
    InstructionPointer,
}

/// Fatal invocation failure. CPU results are uncommitted but guest-memory
/// effects may already be committed; neither retry nor entry replay is valid.
#[derive(Debug, PartialEq, Eq)]
pub enum MemoryProcedureError<E> {
    Backend(E),
    InvalidResult(MemoryProcedureResultError),
}

enum TierAccounting {
    Uniform(u8),
    Complete([u64; 4]),
}

impl Cpu {
    /// Try a registered whole procedure after its entering CALL. The owner
    /// first services pending IRQs, excludes an attached debugger and selects
    /// its IRQ-delivery policy. Ordinary precise procedures reserve a grant
    /// strictly within the next device horizon. An explicitly atomic finite
    /// computation may defer IRQ entry until return; the owner must still
    /// service all elapsed device events. The full registered maximum must
    /// fit the grant. It may be computed from this invocation's arguments.
    /// Pending IRQs, time advancement and caller/tier accounting remain owned
    /// by the machine; an active execution-label map declines.
    ///
    /// A side exit commits the returned prefix state including ESP and EIP.
    /// Interpret its current instruction once. Any error is fatal: guest stores
    /// may have happened, so stop instead of interpreting/retrying the entry.
    pub fn try_call_memory_procedure<B: MemoryProcedureBus>(
        &mut self,
        bus: &mut B,
        registry: &MemoryProcedureRegistry<'_>,
        budget: u32,
    ) -> Result<MemoryProcedureOutcome, MemoryProcedureError<B::Error>> {
        self.try_call_memory_procedure_inner(bus, registry, budget, None)
    }

    /// The owner has classified every reachable source instruction against the
    /// current byte-guarded execution profile and proved one common tier. This
    /// trusted proof includes loops, RET and every possible side-exit prefix;
    /// an entry/caller label alone does not satisfy it. Charge only the exact
    /// completed work after validating and committing the backend's result.
    pub fn try_call_memory_procedure_in_tier<B: MemoryProcedureBus>(
        &mut self,
        bus: &mut B,
        registry: &MemoryProcedureRegistry<'_>,
        budget: u32,
        tier: u8,
    ) -> Result<MemoryProcedureOutcome, MemoryProcedureError<B::Error>> {
        assert!(tier < 4 && self.execution_labels_enabled());
        self.try_call_memory_procedure_inner(
            bus,
            registry,
            budget,
            Some(TierAccounting::Uniform(tier)),
        )
    }

    /// Trusted exact per-tier counts for an all-or-decline finite computation.
    /// A complete result must match their sum; an ordinary side exit may commit
    /// zero instructions only. No counts are charged for that refusal. The owner
    /// derives this proof from original instruction sites and live arguments.
    pub fn try_call_memory_procedure_with_tier_counts<B: MemoryProcedureBus>(
        &mut self,
        bus: &mut B,
        registry: &MemoryProcedureRegistry<'_>,
        budget: u32,
        counts: [u64; 4],
    ) -> Result<MemoryProcedureOutcome, MemoryProcedureError<B::Error>> {
        assert!(self.execution_labels_enabled());
        assert!(
            counts
                .into_iter()
                .try_fold(0u64, u64::checked_add)
                .is_some_and(|total| (1..=u64::from(u32::MAX)).contains(&total))
        );
        self.try_call_memory_procedure_inner(
            bus,
            registry,
            budget,
            Some(TierAccounting::Complete(counts)),
        )
    }

    fn try_call_memory_procedure_inner<B: MemoryProcedureBus>(
        &mut self,
        bus: &mut B,
        registry: &MemoryProcedureRegistry<'_>,
        budget: u32,
        tier: Option<TierAccounting>,
    ) -> Result<MemoryProcedureOutcome, MemoryProcedureError<B::Error>> {
        use MemoryProcedureOutcome::{Complete, Declined, SideExit};
        use ProcedureDecline as Decline;

        let Some(entry) = registry.find(self.registers.ip) else {
            return Ok(Declined(Decline::UnknownEntry));
        };
        if self.is_halted() {
            return Ok(Declined(Decline::Halted));
        }
        if self.flags & (flags::TRAP | flags::RESUME | flags::VIRTUAL) != 0 {
            return Ok(Declined(Decline::DebugOrVirtual8086));
        }
        if self.has_interrupt_shadow() {
            return Ok(Declined(Decline::InterruptShadow));
        }
        if self.execution_labels_enabled() && tier.is_none() {
            return Ok(Declined(Decline::ExecutionLabels));
        }
        if ProcedureContext::capture(self) != Some(entry.context) {
            return Ok(Declined(Decline::Context));
        }
        if entry.maximum_instructions > budget {
            return Ok(Declined(Decline::Budget));
        }
        if self
            .registers
            .address(SegmentRegister::Cs, entry.entry_ip, 1, Access::Execute)
            .is_err()
        {
            return Ok(Declined(Decline::EntryAccess));
        }

        let result = bus
            .execute_memory_procedure(entry.function_id, self)
            .map_err(MemoryProcedureError::Backend)?;
        let invalid = if result.instructions > entry.maximum_instructions
            || (result.completed && result.instructions == 0)
            || matches!(&tier, Some(TierAccounting::Complete(counts))
                if (result.completed && counts.iter().sum::<u64>() != u64::from(result.instructions))
                    || (!result.completed && result.instructions != 0))
        {
            Some(MemoryProcedureResultError::InstructionCount)
        } else if (self.flags ^ result.registers.flags)
            & !(flags::CARRY
                | flags::PARITY
                | flags::AUXILIARY
                | flags::ZERO
                | flags::SIGN
                | flags::OVERFLOW)
            != 0
        {
            Some(MemoryProcedureResultError::ControlFlags)
        } else if self
            .registers
            .address(
                SegmentRegister::Cs,
                result.instruction_pointer,
                1,
                Access::Execute,
            )
            .is_err()
        {
            Some(MemoryProcedureResultError::InstructionPointer)
        } else {
            None
        };
        if let Some(reason) = invalid {
            return Err(MemoryProcedureError::InvalidResult(reason));
        }

        bus.finish_memory_procedure(self.fpu.as_mut())
            .map_err(MemoryProcedureError::Backend)?;

        // Only small architectural values commit here. RAM already reflects
        // the backend's completed prefix, including stack stores and the RET.
        for (code, value) in result.registers.gpr.into_iter().enumerate() {
            self.registers.set32(Gpr32::decode(code as u8), value);
        }
        self.registers.ip = result.instruction_pointer;
        self.flags = result.registers.flags;
        if let Some(tier) = tier {
            let counts = match tier {
                TierAccounting::Uniform(tier) => {
                    let mut counts = [0; 4];
                    counts[usize::from(tier)] = u64::from(result.instructions);
                    counts
                }
                TierAccounting::Complete(counts) if result.completed => counts,
                TierAccounting::Complete(_) => [0; 4],
            };
            self.account_execution(counts);
        }
        Ok(if result.completed {
            Complete {
                instructions: result.instructions,
            }
        } else {
            SideExit {
                instructions: result.instructions,
            }
        })
    }
}

#[cfg(test)]
mod tests;
