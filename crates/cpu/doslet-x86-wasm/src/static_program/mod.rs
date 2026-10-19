//! Experimental offline register-IR compiler, independent of executable formats.
//!
//! This API is separate from the profile compiler and its ABI. It requires the
//! WebAssembly tail-call extension. Known region edges are direct `return_call`s;
//! address lookup occurs at entry, near returns and after a runtime callback. No guest
//! executable decoder, device scheduler or runtime is provided here. Optional
//! direct accesses use the explicit borrowed-memory descriptor in [`memory`].
//!
//! The caller imports an unshared wasm32 memory as `doslet_static.memory` and a
//! synchronous `(frame: i32, reason: i32) -> i32` function as
//! `doslet_static.runtime`. The module exports `run(frame: i32) -> i32` and the
//! imported memory as `memory`. See [`abi`] and [`Frame`] for the complete wire
//! contract. The caller must give each invocation exclusive ownership of its
//! frame and must not reenter `run` from a callback. Callback failure/traps are
//! propagated by the engine; no rollback is promised.
//!
//! A region completes atomically with respect to callbacks. Counted regions
//! charge all completed instructions once at their exit, including JMP/Jcc/RET.
//! The host instruction budget is soft: a positive budget can overshoot by at
//! most `max_region_instructions - 1`. It is not a hardware deadline. HOT regions
//! do not change that budget or the charged counter, and make the total executed
//! instruction count unavailable. A separate structural region quota calls the
//! runtime even in all-HOT or zero-instruction loops. The callback must implement
//! host cancellation/device scheduling and return Exit when execution should
//! yield; returning Continue can intentionally keep a single `run` call alive.
//!
//! With [`CompileOptions::runtime_admission`], each nonempty region first calls
//! `doslet_static.admit(frame: i32, region_id: i32, required: i32) -> i32`.
//! `required` includes its JMP/Jcc/RET. A nonzero result reserves permission for the
//! whole region; zero flushes completed prior work at the refused region's entry
//! and invokes the ordinary fallback callback before any region effects. The
//! callback may account prior work, interpret an instruction or service an IRQ,
//! refresh its proofs/horizon, and continue through the updated frame address.
//! Zero-work regions retain the structural quota and need no admission.
//!
//! Admission may inspect/provision code and segment proofs and reserve an
//! event-free instruction horizon, but must not change architectural state,
//! device time or any frame field while the generated register locals are live.
//! In particular, frame registers/address/counters can be stale during admission
//! and are not a current-state proof. Admission must not reenter `run`. Successful
//! reservations are reconciled with completed work at every ordinary callback
//! and return, including a soft-budget return; the runtime must refresh or stop
//! when exact accounting is unavailable. A trap may leave reservations or effects
//! partial and stops execution without rollback. This hard hardware-horizon
//! contract is independent of the soft host budget. Uncounted HOT regions are
//! rejected in admitted mode until they have a separate device-time policy.
//!
//! With [`CompileOptions::direct_memory`], an isolated counted memory operation
//! checks its complete segment, guest and backing spans before admission, then
//! accesses the borrowed backing with inline wasm loads or stores. Failure
//! flushes at the instruction's entry and requests ordinary fallback without
//! reserving work. The optional [`memory`] descriptor follows the CPU frame;
//! the runtime must keep it and its mappings stable during admission/execution
//! and reconcile its device latch/count mirrors at every runtime boundary.
//! Successful direct accesses make no per-access runtime call. `StoreMemory`
//! accepts ordinary RAM only, checks its complete generation/code-page tables,
//! and rejects every protected source page or exhausted generation before
//! admission. It updates each touched physical page generation once before the
//! next region can execute. The runtime reconciles the staged RAM-write count
//! at each boundary; the actual RAM and generation table change inline.
//! `AluMemory` and `CompareMemory` share the read guards and device effects
//! of `LoadMemory`; CMP/TEST preserve every GPR and no read changes RAM generations.
//! `ExtendMemory` shares those reads using its byte/word source width, preserves
//! flags and extends into a word/dword target only after the complete read.
//! `SignedMultiplyMemory` shares those reads at its word/dword target width,
//! truncates the signed product and changes only CF/OF, as does `SignedMultiply`.
//! `WideMultiply` and `WideMultiplyMemory` retain the full unsigned MUL or signed
//! IMUL product in AX, DX:AX or EDX:EAX and change only CF/OF. The memory form
//! shares the complete read proof and effects at its explicit operand width.
//! `AluMemoryStore` and `UnaryMemory` require both read/write segment windows
//! to describe the same ordinary RAM operand and use the store generation proof.
//! These updates preserve GPRs, perform no device reads and share ALU flag logic.
//! `Push` and `Pop` accept word/dword register operands (and PUSH immediates)
//! over ordinary RAM. They require the descriptor's explicit live SS.B stack
//! address capability; neither code width nor instruction address size supplies
//! it. Missing capabilities decline before admission or effects. PUSH captures
//! its source before changing SP/ESP; POP increments before assigning its target.
//! Near [`Terminator::Return`] reads an ordinary-RAM word/dword target, adjusts
//! the live stack pointer by that width and an unsigned immediate, and transfers
//! through the same static address dispatcher. Known entry aliases need no
//! runtime callback; unknown targets fall back after the completed return.
//! Other operation types retain their original frame ABI and bytecode.
//!
//! With [`CompileOptions::inline_admission`], every nonempty counted region
//! instead checks the descriptor's runtime-provided instruction grant and source
//! page-generation guards inline. Successful checks reserve the complete region
//! by transferring its cost from `INLINE_GRANT` to `INLINE_RESERVED`; they call
//! no imported function. A refusal requests ordinary fallback at the region's
//! entry without reserving work. The runtime must provision valid source proofs,
//! eligible CPU/mapping context and an event-free horizon, keep the guard table
//! stable, and reconcile aggregate reservations at every ordinary boundary.
//! This mode requires `direct_memory` and excludes `runtime_admission` and HOT
//! regions. Zero-work regions retain the structural quota without admission.
//!
//! [`CompileOptions::memory_write_helper`] additionally permits an isolated MOV
//! store to call `memory_write(frame, linear, width_bytes, value) -> accepted`
//! when its direct-RAM proof fails. Descriptor, complete writable-segment and
//! nonwrapping linear-span checks still precede inline admission. The helper
//! receives live source values without a CPU-frame flush: it must preserve the
//! frame, commit pending descriptor effects/reservations without advancing time,
//! perform the bus write, then refresh the descriptor on acceptance (`1`). A
//! decline (`0`) must leave all state unchanged; generated code then undoes only
//! the current reservation and falls back at its original entry. Other statuses
//! trap. Accepted stores charge one instruction and continue by ordinary edges.
//! This option requires inline admission and keeps RAM's direct store path.
//!
//! Compilation validates the entire IR before emitting code and has hard bounds
//! as well as caller-selected lower limits. Index/output reservations are
//! fallible; `wasm-encoder`'s internal, bounded allocations are infallible.

use alloc::vec::Vec;

mod emit;
mod frame;
pub mod memory;
pub mod procedure;
pub mod procedure_memory;
pub use frame::{Frame, FrameError};

/// ABI constants are byte offsets into a little-endian 72-byte CPU frame.
pub mod abi {
    pub const VERSION: u32 = 1;
    pub const IMPORT_MODULE: &str = "doslet_static";
    pub const MEMORY_IMPORT: &str = "memory";
    pub const RUNTIME_IMPORT: &str = "runtime";
    pub const ADMIT_IMPORT: &str = "admit";
    pub const MEMORY_WRITE_IMPORT: &str = "memory_write";
    pub const RUN_EXPORT: &str = "run";
    pub const FRAME_BYTES: usize = 72;
    /// Eight u32s in EAX, ECX, EDX, EBX, ESP, EBP, ESI, EDI order.
    pub const GPR: u32 = 0;
    pub const EFLAGS: u32 = 32;
    pub const SPACE: u32 = 36;
    pub const OFFSET: u32 = 40;
    /// Reserved words at 44 and 68 are untouched by generated code.
    pub const BUDGET: u32 = 48;
    pub const CHARGED_WORK: u32 = 56;
    /// Sticky status bits; unknown bits are preserved.
    pub const STATUS: u32 = 64;
    pub const UNCOUNTED_SEEN: u32 = 1;
    pub const COUNTER_OVERFLOW: u32 = 2;
    pub const REASON_SERVICE: u32 = 0;
    pub const REASON_FALLBACK: u32 = 1;
    /// A callback may update every defined frame field, including the address.
    /// Zero resumes through static address lookup; any other value exits `run`.
    pub const CONTINUE: u32 = 0;
    pub const EXIT: u32 = 1;
    pub const EXIT_BUDGET: u32 = 0;
    pub const EXIT_RUNTIME: u32 = 1;
}

pub const MAX_REGIONS: usize = 131_072;
pub const MAX_REGION_INSTRUCTIONS: usize = 256;
pub const MAX_TOTAL_INSTRUCTIONS: usize = 1_048_576;
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_SERVICE_INTERVAL_REGIONS: u32 = 4096;

/// An opaque code-space/object identity and offset, with no linear-address guess.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Address {
    pub space: u32,
    pub offset: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RegionId(pub u32);

/// An additional static entry address for an existing region. The target keeps
/// its original code, identity, canonical entry and admission requirements.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryAlias {
    pub address: Address,
    pub target: RegionId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    Byte,
    Word,
    Dword,
}
impl Width {
    pub const fn bits(self) -> u32 {
        match self {
            Self::Byte => 8,
            Self::Word => 16,
            Self::Dword => 32,
        }
    }
    pub const fn mask(self) -> u32 {
        match self {
            Self::Byte => 0xff,
            Self::Word => 0xffff,
            Self::Dword => u32::MAX,
        }
    }
}

/// Only the i386 byte aliases exist: there are no SPL/BPL/SIL/DIL registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Register {
    Eax,
    Ecx,
    Edx,
    Ebx,
    Esp,
    Ebp,
    Esi,
    Edi,
    Ax,
    Cx,
    Dx,
    Bx,
    Sp,
    Bp,
    Si,
    Di,
    Al,
    Cl,
    Dl,
    Bl,
    Ah,
    Ch,
    Dh,
    Bh,
}
impl Register {
    pub const fn width(self) -> Width {
        match self {
            Self::Eax
            | Self::Ecx
            | Self::Edx
            | Self::Ebx
            | Self::Esp
            | Self::Ebp
            | Self::Esi
            | Self::Edi => Width::Dword,
            Self::Ax
            | Self::Cx
            | Self::Dx
            | Self::Bx
            | Self::Sp
            | Self::Bp
            | Self::Si
            | Self::Di => Width::Word,
            _ => Width::Byte,
        }
    }
    pub const fn index(self) -> u32 {
        match self {
            Self::Eax | Self::Ax | Self::Al | Self::Ah => 0,
            Self::Ecx | Self::Cx | Self::Cl | Self::Ch => 1,
            Self::Edx | Self::Dx | Self::Dl | Self::Dh => 2,
            Self::Ebx | Self::Bx | Self::Bl | Self::Bh => 3,
            Self::Esp | Self::Sp => 4,
            Self::Ebp | Self::Bp => 5,
            Self::Esi | Self::Si => 6,
            Self::Edi | Self::Di => 7,
        }
    }
    pub const fn shift(self) -> u32 {
        match self {
            Self::Ah | Self::Ch | Self::Dh | Self::Bh => 8,
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Register(Register),
    /// Must fit the destination width. Sign extension belongs to the lowering.
    Immediate(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Alu {
    Add,
    Adc,
    Sub,
    Sbb,
    And,
    Or,
    Xor,
    Cmp,
    Test,
}

/// Ordinary-RAM unary read/modify/write operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryUnary {
    Inc,
    Dec,
    Neg,
    Not,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shift {
    /// SHL and its identical SAL encoding.
    Shl,
    Shr,
    Sar,
    Rol,
    Ror,
    Rcl,
    Rcr,
}

/// The three x86 shift-count encodings; all counts are masked to five bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShiftCount {
    One,
    Immediate(u8),
    Cl,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DoubleShift {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Nop,
    Move {
        target: Register,
        source: Source,
    },
    /// MOVSX/MOVZX from a byte/word alias into a word/dword register. A word
    /// source may have a word target, matching the operand16 B7/BF encodings.
    /// Source values are captured before any aliased destination is written;
    /// flags are preserved. Like Move, this can share a register region or HOT.
    Extend {
        target: Register,
        source: Register,
        signed: bool,
    },
    /// Truncated two/three-operand signed IMUL into a word/dword register.
    /// Both sources have the target width and are captured before assignment.
    /// Only CF/OF change; other flags are retained like the reference CPU.
    /// This operation can share a register region and execute in HOT regions.
    SignedMultiply {
        target: Register,
        left: Source,
        right: Source,
    },
    /// One-operand unsigned MUL or signed IMUL with an implicit accumulator.
    /// Both inputs are captured before writing AX, DX:AX or EDX:EAX for the
    /// byte/word/dword width. Only CF/OF change; other flags are retained like
    /// the reference CPU. This can share a register region and execute in HOT.
    WideMultiply {
        signed: bool,
        width: Width,
        source: Source,
    },
    /// Compute an effective offset into a word/dword register, without reading
    /// memory or consulting segment windows. The address's segment is ignored.
    /// Full-width base/index arithmetic wraps at 32 bits, then address16 masks
    /// the result before the destination alias is written. Like Move, LEA can
    /// share a register region and execute without a memory descriptor or in HOT.
    Lea {
        target: Register,
        address: memory::MemoryAddress,
    },
    /// One guarded memory instruction, isolated in a counted region. The
    /// descriptor and the complete access are checked before runtime admission.
    LoadMemory {
        target: Register,
        address: memory::MemoryAddress,
    },
    /// One isolated counted MOVSX/MOVZX read. `width` is the byte/word source
    /// width, used for every read proof and device effect independently of the
    /// word/dword target. Flags and RAM generations are preserved.
    ExtendMemory {
        target: Register,
        width: Width,
        address: memory::MemoryAddress,
        signed: bool,
    },
    /// One isolated counted signed IMUL read at the word/dword target width.
    /// Multiplies memory by `source`; use the target register as `source` for
    /// two-operand IMUL, or an already width-extended immediate for three operands.
    /// The complete read proof precedes admission. Only CF/OF change; ordinary
    /// RAM generations and all other flags are preserved.
    SignedMultiplyMemory {
        target: Register,
        address: memory::MemoryAddress,
        source: Source,
    },
    /// One isolated counted MUL/IMUL read at the explicit operand width.
    /// The complete read proof precedes admission and every destination write.
    /// The implicit accumulator and full product match WideMultiply; ordinary
    /// RAM generations and flags other than CF/OF are preserved.
    WideMultiplyMemory {
        signed: bool,
        width: Width,
        address: memory::MemoryAddress,
    },
    /// One ordinary-RAM MOV store, with the same isolated-region requirement.
    /// Failed guards leave partial writes and fault ordering to the interpreter.
    StoreMemory {
        address: memory::MemoryAddress,
        width: Width,
        source: Source,
    },
    /// One non-repeated LODS. The source segment and address width come from
    /// the instruction; DF selects the signed SI/ESI adjustment after a read.
    StringLoad {
        width: Width,
        address16: bool,
        segment: memory::Segment,
    },
    /// One non-repeated STOS to ES:DI/EDI. The accumulator is captured before
    /// the guarded store; pointer adjustment preserves arithmetic flags.
    StringStore {
        width: Width,
        address16: bool,
    },
    /// One isolated counted register/immediate PUSH to ordinary RAM. Operand
    /// width is independent of the descriptor's live SS.B stack address width.
    Push {
        width: Width,
        source: Source,
    },
    /// One isolated counted ordinary-RAM POP into a word/dword register.
    /// Stack increment precedes assignment, including SP/ESP aliases.
    Pop {
        target: Register,
    },
    Alu {
        kind: Alu,
        target: Register,
        source: Source,
    },
    /// Register-left ALU/CMP/TEST with one guarded memory source. Like MOV
    /// loads, it requires an isolated counted region and a complete read proof.
    AluMemory {
        kind: Alu,
        target: Register,
        address: memory::MemoryAddress,
    },
    /// One isolated counted memory-left CMP (`test = false`) or TEST
    /// (`test = true`). All GPRs and memory remain unchanged; device reads
    /// retain their normal effects.
    CompareMemory {
        test: bool,
        address: memory::MemoryAddress,
        width: Width,
        source: Source,
    },
    /// Ordinary-RAM memory-left ALU update. CMP/TEST are rejected; use
    /// CompareMemory for their read-only semantics. Both segment windows and
    /// the complete write proof must pass before admission or the operand read.
    AluMemoryStore {
        kind: Alu,
        address: memory::MemoryAddress,
        width: Width,
        source: Source,
    },
    /// One isolated counted ordinary-RAM unary update, with the same complete
    /// read/write proof as AluMemoryStore. INC/DEC preserve CF; NOT all flags.
    UnaryMemory {
        kind: MemoryUnary,
        address: memory::MemoryAddress,
        width: Width,
    },
    Shift {
        kind: Shift,
        target: Register,
        count: ShiftCount,
    },
    /// SHLD/SHRD require equal word/dword operands. Word CL counts and word
    /// immediate counts masked above 16 are rejected: the reference CPU stops
    /// on those undefined counts, and register regions have no partial exit.
    DoubleShift {
        kind: DoubleShift,
        target: Register,
        source: Register,
        count: ShiftCount,
    },
    Inc(Register),
    Dec(Register),
    Neg(Register),
    Not(Register),
    Exchange {
        left: Register,
        right: Register,
    },
}

/// Intel Jcc conditions, in opcode low-nibble order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Condition {
    Overflow,
    NotOverflow,
    Below,
    AboveOrEqual,
    Equal,
    NotEqual,
    BelowOrEqual,
    Above,
    Sign,
    NotSign,
    Parity,
    NotParity,
    Less,
    GreaterOrEqual,
    LessOrEqual,
    Greater,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terminator {
    /// Region subdivision, not a completed guest instruction.
    Fallthrough(RegionId),
    Jump(RegionId),
    Branch {
        condition: Condition,
        taken: RegionId,
        not_taken: RegionId,
    },
    /// Isolated counted near RET/RET imm16 over ordinary RAM. The operand width
    /// masks the popped offset; the descriptor's live SS.B chooses SP or ESP.
    /// `target_space` is an opaque CPU address-space identity, not a selector.
    Return {
        width: Width,
        pop_bytes: u16,
        target_space: u32,
    },
    /// Stop before an unsupported instruction. Runtime owns its completion.
    Fallback(Address),
}
impl Terminator {
    pub const fn completed_instructions(self) -> usize {
        match self {
            Self::Jump(_) | Self::Branch { .. } | Self::Return { .. } => 1,
            Self::Fallthrough(_) | Self::Fallback(_) => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Accounting {
    #[default]
    Counted,
    UncountedHot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Region {
    pub id: RegionId,
    pub entry: Address,
    pub operations: Vec<Operation>,
    pub terminator: Terminator,
    pub accounting: Accounting,
}
impl Region {
    pub fn completed_instructions(&self) -> usize {
        self.operations
            .len()
            .saturating_add(self.terminator.completed_instructions())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Program {
    pub regions: Vec<Region>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompileOptions {
    /// Enable isolated inline RAM/device reads and ordinary-RAM updates/stacks.
    pub direct_memory: bool,
    /// Request whole-region admission before effects. See the module contract.
    /// Disabled by default; admitted programs cannot contain uncounted HOT work.
    pub runtime_admission: bool,
    /// Check runtime-provided source generations and instruction grants inline.
    /// Requires direct memory and excludes runtime admission and uncounted HOT.
    pub inline_admission: bool,
    /// Permit isolated MOV stores to call the bus helper after inline admission
    /// when the direct ordinary-RAM proof declines. Requires inline admission.
    pub memory_write_helper: bool,
    pub service_interval_regions: u32,
    /// Independent limit for regions and entry aliases. Their combined static
    /// dispatcher has at most twice this many entries.
    pub max_regions: usize,
    pub max_region_instructions: usize,
    pub max_total_instructions: usize,
    pub max_output_bytes: usize,
}
impl Default for CompileOptions {
    fn default() -> Self {
        Self {
            direct_memory: false,
            runtime_admission: false,
            inline_admission: false,
            memory_write_helper: false,
            service_interval_regions: 64,
            max_regions: MAX_REGIONS,
            max_region_instructions: MAX_REGION_INSTRUCTIONS,
            max_total_instructions: MAX_TOTAL_INSTRUCTIONS,
            max_output_bytes: MAX_OUTPUT_BYTES,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompileError {
    InvalidOptions,
    TooManyRegions,
    TooManyAliases,
    TooManyEntries,
    RegionTooLong(RegionId),
    AdmissionUncountedHot(RegionId),
    DirectMemoryDisabled(RegionId),
    InvalidMemoryAddress(RegionId),
    InvalidMemoryOperation(RegionId),
    MixedMemoryRegion(RegionId),
    MemoryUncountedHot(RegionId),
    TooManyInstructions,
    DuplicateId(RegionId),
    DuplicateAddress(Address),
    UnknownTarget { from: RegionId, target: RegionId },
    UnknownAliasTarget { address: Address, target: RegionId },
    WidthMismatch(RegionId),
    ImmediateOutOfRange(RegionId),
    InvalidShiftCount(RegionId),
    OutputTooLarge,
    Allocation,
}
impl core::fmt::Display for CompileError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "static-program compilation: {self:?}")
    }
}
impl core::error::Error for CompileError {}

#[derive(Debug)]
pub struct CompiledProgram {
    bytes: Vec<u8>,
    dispatcher_bytes: usize,
}
impl CompiledProgram {
    pub fn wasm(&self) -> &[u8] {
        &self.bytes
    }
    pub fn into_wasm(self) -> Vec<u8> {
        self.bytes
    }
    /// Sum of the dispatcher and its bounded address-helper body sizes,
    /// including local declarations and excluding their length prefixes.
    /// Useful when measuring large-module engine compilation.
    pub fn dispatcher_bytes(&self) -> usize {
        self.dispatcher_bytes
    }
}

impl Program {
    /// Input order is irrelevant: functions are sorted by RegionId, address
    /// decisions by Address. Equal semantic input produces identical bytes.
    pub fn compile(&self, options: &CompileOptions) -> Result<CompiledProgram, CompileError> {
        self.compile_with_aliases(options, &[])
    }

    /// Add immutable static dispatcher addresses for existing regions without
    /// duplicating their code or changing region/function identities. Addresses
    /// must be unique across canonical entries and aliases, even for the same
    /// target. Alias order does not affect the module or validation errors.
    /// Empty aliases preserve the bytecode produced by [`Self::compile`].
    pub fn compile_with_aliases(
        &self,
        options: &CompileOptions,
        aliases: &[EntryAlias],
    ) -> Result<CompiledProgram, CompileError> {
        let checked = Checked::new(self, options, aliases)?;
        let (bytes, dispatcher_bytes) = emit::module(&checked)?;
        Ok(CompiledProgram {
            bytes,
            dispatcher_bytes,
        })
    }
}

struct Checked<'a> {
    program: &'a Program,
    options: &'a CompileOptions,
    by_id: Vec<usize>,
    by_address: Vec<EntryAlias>,
}
impl<'a> Checked<'a> {
    fn new(
        program: &'a Program,
        options: &'a CompileOptions,
        aliases: &[EntryAlias],
    ) -> Result<Self, CompileError> {
        if options.service_interval_regions == 0
            || options.service_interval_regions > MAX_SERVICE_INTERVAL_REGIONS
            || options.max_regions > MAX_REGIONS
            || options.max_region_instructions == 0
            || options.max_region_instructions > MAX_REGION_INSTRUCTIONS
            || options.max_total_instructions > MAX_TOTAL_INSTRUCTIONS
            || options.max_output_bytes > MAX_OUTPUT_BYTES
            || (options.inline_admission && (!options.direct_memory || options.runtime_admission))
            || (options.memory_write_helper && !options.inline_admission)
        {
            return Err(CompileError::InvalidOptions);
        }
        if program.regions.len() > options.max_regions {
            return Err(CompileError::TooManyRegions);
        }
        // Bound both caller-controlled lengths before reserving an index or
        // copying any entries, including malformed/unknown aliases.
        if aliases.len() > options.max_regions {
            return Err(CompileError::TooManyAliases);
        }
        let max_entries = options
            .max_regions
            .checked_mul(2)
            .ok_or(CompileError::TooManyEntries)?;
        let entries = program
            .regions
            .len()
            .checked_add(aliases.len())
            .filter(|&count| count <= max_entries)
            .ok_or(CompileError::TooManyEntries)?;
        let mut by_id = Vec::new();
        let mut by_address = Vec::new();
        by_id
            .try_reserve_exact(program.regions.len())
            .map_err(|_| CompileError::Allocation)?;
        by_address
            .try_reserve_exact(entries)
            .map_err(|_| CompileError::Allocation)?;
        by_id.extend(0..program.regions.len());
        by_address.extend(program.regions.iter().map(|r| EntryAlias {
            address: r.entry,
            target: r.id,
        }));
        by_address.extend_from_slice(aliases);
        by_id.sort_unstable_by_key(|&i| program.regions[i].id);
        by_address.sort_unstable_by_key(|entry| entry.address);
        for pair in by_id.windows(2) {
            if program.regions[pair[0]].id == program.regions[pair[1]].id {
                return Err(CompileError::DuplicateId(program.regions[pair[0]].id));
            }
        }
        for pair in by_address.windows(2) {
            if pair[0].address == pair[1].address {
                return Err(CompileError::DuplicateAddress(pair[0].address));
            }
        }
        let checked = Self {
            program,
            options,
            by_id,
            by_address,
        };
        for entry in &checked.by_address {
            if checked.position(entry.target).is_none() {
                return Err(CompileError::UnknownAliasTarget {
                    address: entry.address,
                    target: entry.target,
                });
            }
        }
        let mut total = 0usize;
        // Deterministic validation errors, independent of the caller's ordering.
        for &index in &checked.by_id {
            let r = &program.regions[index];
            if (options.runtime_admission || options.inline_admission)
                && r.accounting == Accounting::UncountedHot
            {
                return Err(CompileError::AdmissionUncountedHot(r.id));
            }
            let count = r.completed_instructions();
            if count > options.max_region_instructions {
                return Err(CompileError::RegionTooLong(r.id));
            }
            total = total
                .checked_add(count)
                .ok_or(CompileError::TooManyInstructions)?;
            if total > options.max_total_instructions {
                return Err(CompileError::TooManyInstructions);
            }
            if let Terminator::Return { width, .. } = r.terminator {
                if width == Width::Byte {
                    return Err(CompileError::WidthMismatch(r.id));
                }
                if !options.direct_memory {
                    return Err(CompileError::DirectMemoryDisabled(r.id));
                }
                if !r.operations.is_empty() {
                    return Err(CompileError::MixedMemoryRegion(r.id));
                }
                if r.accounting != Accounting::Counted {
                    return Err(CompileError::MemoryUncountedHot(r.id));
                }
            }
            for op in &r.operations {
                validate_operation(op, r.id, options.direct_memory)?;
                if operation_accesses_memory(*op) {
                    if r.operations.len() != 1
                        || !matches!(
                            r.terminator,
                            Terminator::Fallthrough(_) | Terminator::Fallback(_)
                        )
                    {
                        return Err(CompileError::MixedMemoryRegion(r.id));
                    }
                    if r.accounting != Accounting::Counted {
                        return Err(CompileError::MemoryUncountedHot(r.id));
                    }
                }
            }
            let targets = match r.terminator {
                Terminator::Fallthrough(a) | Terminator::Jump(a) => [Some(a), None],
                Terminator::Branch {
                    taken, not_taken, ..
                } => [Some(taken), Some(not_taken)],
                Terminator::Fallback(_) | Terminator::Return { .. } => [None, None],
            };
            for target in targets.into_iter().flatten() {
                if checked.position(target).is_none() {
                    return Err(CompileError::UnknownTarget { from: r.id, target });
                }
            }
        }
        Ok(checked)
    }
    fn position(&self, id: RegionId) -> Option<usize> {
        self.by_id
            .binary_search_by_key(&id, |&i| self.program.regions[i].id)
            .ok()
    }
}

fn operation_accesses_memory(op: Operation) -> bool {
    matches!(
        &op,
        Operation::LoadMemory { .. }
            | Operation::ExtendMemory { .. }
            | Operation::SignedMultiplyMemory { .. }
            | Operation::WideMultiplyMemory { .. }
            | Operation::StoreMemory { .. }
            | Operation::StringLoad { .. }
            | Operation::StringStore { .. }
            | Operation::AluMemory { .. }
            | Operation::CompareMemory { .. }
            | Operation::AluMemoryStore { .. }
            | Operation::UnaryMemory { .. }
            | Operation::Push { .. }
            | Operation::Pop { .. }
    )
}

/// Shared operand/address validation; execution-boundary policy belongs to each compiler.
fn validate_operation(
    op: &Operation,
    id: RegionId,
    direct_memory: bool,
) -> Result<(), CompileError> {
    let extension = match *op {
        Operation::Extend { target, source, .. } => Some((target, source.width())),
        Operation::ExtendMemory { target, width, .. } => Some((target, width)),
        _ => None,
    };
    if let Some((target, width)) = extension
        && (target.width() == Width::Byte
            || width == Width::Dword
            || width.bits() > target.width().bits())
    {
        return Err(CompileError::WidthMismatch(id));
    }
    if matches!(
        op,
        Operation::Push {
            width: Width::Byte,
            ..
        }
    ) || matches!(
        op,
        Operation::Pop { target }
            | Operation::Lea { target, .. }
            | Operation::SignedMultiply { target, .. }
            | Operation::SignedMultiplyMemory { target, .. }
            if target.width() == Width::Byte
    ) {
        return Err(CompileError::WidthMismatch(id));
    }
    if matches!(
        op,
        Operation::AluMemoryStore {
            kind: Alu::Cmp | Alu::Test,
            ..
        }
    ) {
        return Err(CompileError::InvalidMemoryOperation(id));
    }
    if let Operation::DoubleShift {
        target,
        source,
        count,
        ..
    } = *op
    {
        if target.width() == Width::Byte || target.width() != source.width() {
            return Err(CompileError::WidthMismatch(id));
        }
        if target.width() == Width::Word
            && match count {
                ShiftCount::Cl => true,
                ShiftCount::Immediate(n) => n & 31 > 16,
                ShiftCount::One => false,
            }
        {
            return Err(CompileError::InvalidShiftCount(id));
        }
    }
    let memory_operation = operation_accesses_memory(*op);
    if memory_operation && !direct_memory {
        return Err(CompileError::DirectMemoryDisabled(id));
    }
    if let Operation::Lea { address, .. }
    | Operation::LoadMemory { address, .. }
    | Operation::ExtendMemory { address, .. }
    | Operation::SignedMultiplyMemory { address, .. }
    | Operation::WideMultiplyMemory { address, .. }
    | Operation::StoreMemory { address, .. }
    | Operation::AluMemory { address, .. }
    | Operation::CompareMemory { address, .. }
    | Operation::AluMemoryStore { address, .. }
    | Operation::UnaryMemory { address, .. } = *op
        && (!matches!(address.scale, 1 | 2 | 4 | 8)
            || (address.address16 && address.scale != 1)
            || address
                .base
                .into_iter()
                .chain(address.index)
                .any(|register| register.width() != Width::Dword))
    {
        return Err(CompileError::InvalidMemoryAddress(id));
    }
    let pairs = match *op {
        Operation::SignedMultiply {
            target,
            left,
            right,
        } => [Some((target.width(), left)), Some((target.width(), right))],
        Operation::Move { target, source }
        | Operation::Alu { target, source, .. }
        | Operation::SignedMultiplyMemory { target, source, .. } => {
            [Some((target.width(), source)), None]
        }
        Operation::StoreMemory { width, source, .. }
        | Operation::WideMultiply { width, source, .. }
        | Operation::Push { width, source }
        | Operation::CompareMemory { width, source, .. }
        | Operation::AluMemoryStore { width, source, .. } => [Some((width, source)), None],
        Operation::Exchange { left, right } => {
            [Some((left.width(), Source::Register(right))), None]
        }
        _ => [None, None],
    };
    for (width, source) in pairs.into_iter().flatten() {
        match source {
            Source::Register(s) if s.width() != width => {
                return Err(CompileError::WidthMismatch(id));
            }
            Source::Immediate(v) if v & !width.mask() != 0 => {
                return Err(CompileError::ImmediateOutOfRange(id));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod inline_admission_tests;
#[cfg(test)]
mod memory_tests;
#[cfg(test)]
mod store_tests;
#[cfg(test)]
mod tests;
