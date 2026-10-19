//! Opt-in dispatch of externally proved, frozen near32 procedures.
//!
//! This is separate from [`Cpu::step`], which still executes one instruction.
//! The original register-only leaf ABI is defined here. [`memory`] provides a
//! separate entry hook for backends that execute guest memory and the real RET.
//!
//! The entering guest CALL must already have executed, including its stack
//! write. The executor replaces the leaf body; the CPU performs its outer RET.
//! Registration is a trusted assertion that the body has no memory, port,
//! stack, segment, FPU, control-register or internal guest CALL effects. In
//! particular, a balanced PUSH/POP or CALL/RET still writes guest memory and is
//! outside this ABI. A separate binding pass can establish this contract
//! without coupling the x86 decoder to a particular compiler or backend.
//!
//! The owner must freeze the proved code and its mapping for the registry's
//! entire use, and discard the registry when either can change. No source-byte
//! or generation guards are performed here. This experimental assumption is
//! not production source-invalidation support. The owner must also exclude an
//! attached debugger, service pending interrupts, and grant a budget strictly
//! within the next device/IRQ horizon. There are no interior interrupt polls.

use crate::{Access, Cpu, Gpr32, SegmentCache, SegmentRegister, flags};
use doslet_bus::{BusError, MemoryBus};

pub mod memory;

/// A finite upper bound on the trusted body-plus-RET work declaration.
pub const MAX_PROCEDURE_INSTRUCTIONS: u32 = 4096;
/// Bound setup work even when the registry borrows an externally built table.
pub const MAX_PROCEDURE_ENTRIES: usize = 100_000;

/// Explicit acknowledgement that the owner freezes code and address mappings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrozenCode {
    /// No live guards are installed. Discard bindings before code/mapping changes.
    AssumeImmutable,
}

/// Exact code and stack context for a protected-mode 32-bit binding. CS must
/// cover the full flat 32-bit range; SS must have base zero and a 32-bit stack.
/// Constructed from the CPU after the owner has established its segments.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureContext {
    cs: SegmentCache,
    ss: SegmentCache,
    cr0: u32,
    cpl: u8,
}

impl ProcedureContext {
    /// Capture a supported mode, independently of transient execution hazards.
    /// Unsynchronised public CR0 writes decline until an interpreter step has
    /// materialised the hidden segment caches, just as ordinary execution does.
    pub fn capture(cpu: &Cpu) -> Option<Self> {
        let registers = &cpu.registers;
        let cs = registers.segment_cache(SegmentRegister::Cs);
        let ss = registers.segment_cache(SegmentRegister::Ss);
        (cpu.control.cr0 & 0x8000_0001 == 1
            && registers.protected
            && cs.selector == registers.cs
            && ss.selector == registers.ss
            && cs.base == 0
            && cs.limit == u32::MAX
            && cs.usable
            && cs.access & 0x98 == 0x98
            && ss.base == 0
            && cs.default32
            && ss.default32)
            .then_some(Self {
                cs,
                ss,
                cr0: cpu.control.cr0,
                cpl: registers.cpl,
            })
    }
}

/// All eight GPRs in x86 encoding order (EAX, ECX, EDX, EBX, ESP, EBP, ESI,
/// EDI), and EFLAGS. Only arithmetic flags may change. The register-only leaf
/// ABI preserves ESP; the [`memory`] ABI commits the backend's resulting ESP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureRegisters {
    pub gpr: [u32; 8],
    pub flags: u32,
}

/// Register-only leaf ABI with an explicit outer near-return encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureAbi {
    pub return_width: u8,
    pub stack_cleanup: u16,
}

impl ProcedureAbi {
    /// Register-only body, 32-bit stack, near RET32 and no argument cleanup.
    pub const REGISTER_LEAF_NEAR32: Self = Self {
        return_width: 4,
        stack_cleanup: 0,
    };
}

/// A trusted proof/binding supplied by the compiler's setup pass. Work includes
/// the outer RET, excludes the entering CALL, and must be exact for every input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureEntry {
    pub entry_ip: u32,
    pub context: ProcedureContext,
    pub function_id: u32,
    pub abi: ProcedureAbi,
    pub instruction_cost: u32,
}

/// Setup validation failure. No guest state is involved in registry creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureRegistryError {
    TooManyEntries,
    EntriesNotStrictlySorted,
    UnsupportedAbi,
    InvalidInstructionCost,
}

/// Borrowed immutable bindings sorted strictly by entry EIP. A given EIP has
/// one binding; its complete captured context must match at invocation time.
#[derive(Debug)]
pub struct ProcedureRegistry<'a> {
    entries: &'a [ProcedureEntry],
}

impl<'a> ProcedureRegistry<'a> {
    pub fn new(
        entries: &'a [ProcedureEntry],
        _frozen_code: FrozenCode,
    ) -> Result<Self, ProcedureRegistryError> {
        if entries.len() > MAX_PROCEDURE_ENTRIES {
            return Err(ProcedureRegistryError::TooManyEntries);
        }
        if entries
            .windows(2)
            .any(|pair| pair[0].entry_ip >= pair[1].entry_ip)
        {
            return Err(ProcedureRegistryError::EntriesNotStrictlySorted);
        }
        for entry in entries {
            if entry.abi != ProcedureAbi::REGISTER_LEAF_NEAR32 {
                return Err(ProcedureRegistryError::UnsupportedAbi);
            }
            if !(1..=MAX_PROCEDURE_INSTRUCTIONS).contains(&entry.instruction_cost) {
                return Err(ProcedureRegistryError::InvalidInstructionCost);
            }
        }
        Ok(Self { entries })
    }

    pub fn entries(&self) -> &'a [ProcedureEntry] {
        self.entries
    }

    fn find(&self, ip: u32) -> Option<&ProcedureEntry> {
        self.entries
            .binary_search_by_key(&ip, |entry| entry.entry_ip)
            .ok()
            .map(|index| &self.entries[index])
    }
}

/// Backend execution must be bounded and allocation-free after setup. It has
/// no guest-memory/CPU access and returns only small values. Errors or invalid
/// results never commit CPU state; backend-private effects are its own concern.
pub trait ProcedureExecutor {
    type Error;

    fn execute(
        &mut self,
        function_id: u32,
        input: ProcedureRegisters,
    ) -> Result<ProcedureResult, Self::Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcedureResult {
    pub registers: ProcedureRegisters,
    /// Exact body-plus-RET guest work, matching the registered declaration.
    pub instructions: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureDecline {
    UnknownEntry,
    Context,
    Halted,
    DebugOrVirtual8086,
    InterruptShadow,
    ExecutionLabels,
    Budget,
    EntryAccess,
    ReturnStack,
    ReturnTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureCallOutcome {
    Declined(ProcedureDecline),
    Executed { instructions: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcedureResultError {
    InstructionCount,
    StackPointer,
    ControlFlags,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProcedureCallError<E> {
    Backend(E),
    InvalidResult(ProcedureResultError),
    Bus(BusError),
}

impl Cpu {
    /// Try one entire registered leaf at its current entry, after the guest's
    /// entering CALL has executed. A decline has no CPU or bus effects. A
    /// backend error can follow a side-effect-free ordinary-RAM stack read but
    /// never commits CPU state. Normal interpretation can resume on a decline.
    ///
    /// `budget` is an owner-granted instruction budget, already restricted to
    /// the next device/IRQ horizon. An active execution-label map declines:
    /// this slice has no per-invocation tier/context accounting contract yet.
    pub fn try_call_procedure<B: MemoryBus, E: ProcedureExecutor>(
        &mut self,
        bus: &mut B,
        registry: &ProcedureRegistry<'_>,
        executor: &mut E,
        budget: u32,
    ) -> Result<ProcedureCallOutcome, ProcedureCallError<E::Error>> {
        use ProcedureCallOutcome::{Declined, Executed};
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
        if self.execution_labels_enabled() {
            return Ok(Declined(Decline::ExecutionLabels));
        }
        if ProcedureContext::capture(self) != Some(entry.context) {
            return Ok(Declined(Decline::Context));
        }
        if entry.instruction_cost > budget {
            return Ok(Declined(Decline::Budget));
        }
        if self
            .registers
            .address(SegmentRegister::Cs, entry.entry_ip, 1, Access::Execute)
            .is_err()
        {
            return Ok(Declined(Decline::EntryAccess));
        }
        let input = ProcedureRegisters {
            gpr: core::array::from_fn(|code| self.registers.get32(Gpr32::decode(code as u8))),
            flags: self.flags,
        };
        let stack = input.gpr[Gpr32::Esp as usize];
        let Ok(address) = self
            .registers
            .address(SegmentRegister::Ss, stack, 4, Access::Read)
        else {
            return Ok(Declined(Decline::ReturnStack));
        };
        if !bus.is_plain_ram(address, 4) {
            return Ok(Declined(Decline::ReturnStack));
        }
        let target = bus.read_u32(address).map_err(ProcedureCallError::Bus)?;
        if self
            .registers
            .address(SegmentRegister::Cs, target, 1, Access::Execute)
            .is_err()
        {
            return Ok(Declined(Decline::ReturnTarget));
        }
        let result = executor
            .execute(entry.function_id, input)
            .map_err(ProcedureCallError::Backend)?;
        let invalid = if result.instructions != entry.instruction_cost {
            Some(ProcedureResultError::InstructionCount)
        } else if result.registers.gpr[Gpr32::Esp as usize] != stack {
            Some(ProcedureResultError::StackPointer)
        } else if (input.flags ^ result.registers.flags)
            & !(flags::CARRY
                | flags::PARITY
                | flags::AUXILIARY
                | flags::ZERO
                | flags::SIGN
                | flags::OVERFLOW)
            != 0
        {
            Some(ProcedureResultError::ControlFlags)
        } else {
            None
        };
        if let Some(reason) = invalid {
            return Err(ProcedureCallError::InvalidResult(reason));
        }
        // All fallible work precedes this commit. RET32 pops using SS.B, which
        // this ABI requires to be 32-bit; wrapping ESP matches ordinary RET.
        for (code, value) in result.registers.gpr.into_iter().enumerate() {
            self.registers.set32(Gpr32::decode(code as u8), value);
        }
        self.registers.set32(Gpr32::Esp, stack.wrapping_add(4));
        self.registers.ip = target;
        self.flags = result.registers.flags;
        Ok(Executed {
            instructions: entry.instruction_cost,
        })
    }
}

#[cfg(test)]
mod tests;
