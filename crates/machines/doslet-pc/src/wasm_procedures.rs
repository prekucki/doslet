//! Native prepared procedures over the machine's single RAM backing.
//!
//! Installation is a trusted compiler boundary. Sources are post-relocation
//! bytes; entry verifies them against the live bus and caches page generations.
//! Generated stores protect every compiled source page before their effects.

use alloc::{boxed::Box, string::String, vec::Vec};
use doslet_bus::{CodeCheck, MemoryBus};
#[cfg(not(target_arch = "wasm32"))]
pub use doslet_wasm_aot::CompiledCache;
#[cfg(not(target_arch = "wasm32"))]
use doslet_wasm_aot::procedure_memory::Exit;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use doslet_wasm_aot::procedure_memory::SharedProcedureMemory;
use doslet_x86_wasm::static_program::procedure_memory::rle::{
    RleAdmission, RleMemory, prepare_rle,
};
#[cfg(target_arch = "wasm32")]
mod browser;
#[cfg(target_arch = "wasm32")]
pub use browser::BrowserCall;
#[cfg(target_arch = "wasm32")]
use browser::Exit;
#[cfg(target_arch = "wasm32")]
pub(crate) use browser::SharedProcedureMemory;
use doslet_x86::procedure_calls::{
    FrozenCode, MAX_PROCEDURE_INSTRUCTIONS, ProcedureContext, ProcedureRegisters,
    memory::{
        MemoryProcedureBus, MemoryProcedureEntry, MemoryProcedureError, MemoryProcedureFpu,
        MemoryProcedureOutcome, MemoryProcedureRegistry, MemoryProcedureResult,
        MemoryProcedureResultError,
    },
};
use doslet_x86::{Access, Cpu, Gpr32, Registers, SegmentCache, SegmentRegister};
pub use doslet_x86_wasm::static_program::procedure_memory::CopyKernel;
pub use doslet_x86_wasm::static_program::procedure_memory::RleKernel;
pub use doslet_x86_wasm::static_program::procedure_memory::X87Procedure;
pub use doslet_x86_wasm::static_program::procedure_memory::bundle;
use doslet_x86_wasm::static_program::procedure_memory::{
    X87_BOUNDARY_OFFSET, X87_SELECTORS_OFFSET,
};
use doslet_x86_wasm::static_program::{
    Address, Frame,
    memory::{
        DeviceReadDescriptor, DeviceReadKind, DeviceWriteDescriptor, SegmentBounds, StackAddress,
    },
};

use crate::{MachineError, PcBus, PcMachine, memory::storage::Storage};

mod interpreted;

const MAX_BINDINGS: usize = 4096;
/// Direct-memory descriptor version required by a prepared native bundle.
pub const MEMORY_ABI_VERSION: u32 = doslet_x86_wasm::static_program::memory::VERSION;
const MAX_SOURCE_PARTS: usize = 65_536;
const MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const PAGE_BYTES: usize = 4096;
/// Representable bound for a proved RLE computation in the supported maximum
/// 32-MiB RAM: disjoint input/output spans imply N+K <= RAM bytes, and original
/// work is at most 23 + 14*(N+K). Copy kernels have a smaller bound.
pub const MAX_ATOMIC_PROCEDURE_WORK: u32 = 32 * 1024 * 1024 * 14 + 23;

pub struct CodePart {
    pub linear: u32,
    pub expected: Vec<u8>,
}

/// The code segment procedures were bound to. Entry, terminal and guard
/// addresses are EIPs in it; source parts are linear addresses, its base plus
/// those EIPs. A call needs a live CS with this base and at least this
/// inclusive limit. DOS/4GW programs run flat; a DJGPP program's CS has its
/// memory block's base and a smaller limit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeSegment {
    pub base: u32,
    pub limit: u32,
}

impl CodeSegment {
    pub const FLAT: Self = Self {
        base: 0,
        limit: u32::MAX,
    };

    /// The linear address of an EIP in this segment, unless it lies outside.
    fn linear(self, ip: u32) -> Option<u32> {
        (ip <= self.limit).then(|| self.base.checked_add(ip))?
    }
}

impl Default for CodeSegment {
    fn default() -> Self {
        Self::FLAT
    }
}

/// IRQ delivery at a whole-computation boundary, selected by the bundle owner.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KernelInterrupts {
    #[default]
    Defer,
    /// Admit only if the complete live cost fits before the next known IRQ.
    /// Audio sample deadlines still do not split the computation.
    BeforeNextIrq,
}

pub struct Binding {
    pub function_id: u32,
    pub entry_ip: u32,
    pub maximum_instructions: u32,
    /// Compiler-authored executed sites, independent of immutable byte guards.
    /// Exit boundary bytes stay guarded but never contribute a tier or work.
    /// Legacy None retains conservative decoding of all source bytes.
    pub execution_sites: Option<Vec<ExecutionSite>>,
    /// Explicit PC24 coprocessor permissions, relative to the entry TOP.
    /// Absence keeps legacy scalar admission and calls free of FPU transfers.
    pub x87: Option<X87Procedure>,
    /// A compiler-proved, finite whole-procedure kernel. Its live arguments
    /// determine admission cost; IRQ delivery may wait until its return.
    pub kernel: Option<CopyKernel>,
    /// Complete finite RLE computation, with a live input-prefix proof.
    pub rle_kernel: Option<RleKernel>,
    /// Generic statically terminating CFG, independent of any semantic idiom.
    pub finite_computation: Option<FiniteComputation>,
    pub kernel_interrupts: KernelInterrupts,
    /// Shared by every binding of one installation.
    pub code_segment: CodeSegment,
    pub code: Vec<CodePart>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecutionSite {
    /// Architectural EIP in the binding's code segment, unchanged by rebasing.
    pub ip: u32,
    pub length: u8,
}

/// Compiler terminals used by the optional original-x86 boundary reference.
/// No vectors are copied or allocated while executing an admitted call.
pub struct FiniteComputation {
    pub exits: Vec<u32>,
    pub returns: Vec<u32>,
    pub guards: Vec<FiniteLoopGuard>,
}

pub struct FiniteLoopGuard {
    pub entry_ip: u32,
    /// EAX, ECX, EDX, EBX, ESP, EBP, ESI, EDI, matching the call frame.
    pub counter: u8,
    pub decrement: bool,
    pub maximum_trips: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub calls: u64,
    pub translated_instructions: u64,
    pub interpreted_calls: u64,
    pub interpreted_instructions: u64,
    pub kernel_calls: u64,
    pub kernel_instructions: u64,
    pub side_exits: u64,
    pub source_refusals: u64,
    pub budget_refusals: u64,
    pub kernel_irq_refusals: u64,
    /// A procedure cannot prove one live label for all its source instructions.
    pub tier_refusals: u64,
    pub translated_tiers: [u64; 4],
    /// Effects reported by generated WASM; interpreter bus effects are already
    /// accounted by the ordinary PC bus and are not added here again.
    pub device_reads: u64,
    pub device_writes: u64,
    pub device_changes: u64,
}

/// Accepted execution totals for one prepared procedure entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProcedureEntryStats {
    pub calls: u64,
    pub translated_instructions: u64,
    pub side_exits: u64,
}

/// Controlled interpreter variants retaining the installed plan and shared RAM.
/// This opt-in diagnostic selection is excluded from portable machine states.
#[cfg(feature = "wasm-procedure-diagnostics")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiagnosticMode {
    /// Keep procedure dispatch, registered entries and borrowed interpreter RAM.
    #[default]
    Normal,
    /// Disable dispatch and registered entries, retaining borrowed interpreter RAM.
    SharedWithoutEntries,
    /// Also use the ordinary PC bus for interpreter batches over shared RAM.
    SharedPcBus,
}

/// Fatal errors cannot fall back: guest stores may already have committed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecutionError {
    Backend,
    InvalidResult(MemoryProcedureResultError),
}

struct SourceGuard {
    start: usize,
    end: usize,
    check: Option<CodeCheck>,
}

struct PreparedBinding {
    binding: Binding,
    function_index: u32,
    /// Control-time admission policy; compiled code and statistics stay intact.
    enabled: bool,
    guards: Vec<SourceGuard>,
    checked: bool,
    matches: bool,
    /// Decoded original instruction boundaries, prepared once from frozen code.
    /// Unsupported or oversized proofs retain the ordinary interpreter path.
    instructions: Option<Vec<SourceInstruction>>,
    canonical_copy: bool,
    /// The uniform tier of the guarded source bytes and the label generation
    /// it was classified under.
    uniform: Option<(u64, Option<u8>)>,
    execution: ProcedureEntryStats,
}

struct SourceInstruction {
    address: u32,
    length: u8,
    prefix_length: u8,
    flow: doslet_x86::translation::NearFlow,
}

#[derive(Clone, Copy)]
enum TierProof {
    Uniform(u8),
    Complete([u64; 4]),
}

impl TierProof {
    fn counts(&self, maximum: u32) -> [u64; 4] {
        match self {
            Self::Uniform(tier) => {
                let mut counts = [0; 4];
                counts[usize::from(*tier)] = u64::from(maximum);
                counts
            }
            Self::Complete(counts) => *counts,
        }
    }
}

/// Segment capabilities depend on selectors, hidden caches and PE mode, never
/// on the current GPRs/EIP. No CPU or RAM borrow survives a foreign call.
#[derive(Clone, Copy, PartialEq, Eq)]
struct WindowContext {
    selectors: [u16; 6],
    caches: [SegmentCache; 6],
    protected: bool,
}

#[derive(Default)]
pub(crate) struct CallWindows {
    context: Option<WindowContext>,
    windows: [[SegmentBounds; 6]; 2],
}

impl CallWindows {
    fn resolve(&mut self, registers: &Registers) -> [[SegmentBounds; 6]; 2] {
        let context = WindowContext {
            selectors: SegmentRegister::ALL.map(|segment| registers.selector(segment)),
            caches: SegmentRegister::ALL.map(|segment| registers.segment_cache(segment)),
            protected: registers.protected_mode(),
        };
        if self.context != Some(context) {
            self.windows = [Access::Read, Access::Write].map(|access| {
                SegmentRegister::ALL.map(|segment| {
                    registers
                        .window(segment, access)
                        .map_or(SegmentBounds::default(), |window| {
                            let (base, first, last) = window.parts();
                            SegmentBounds {
                                valid: true,
                                base,
                                first,
                                last,
                            }
                        })
                })
            });
            self.context = Some(context);
        }
        self.windows
    }
}

impl PreparedBinding {
    fn uniform_tier(&self, cpu: &Cpu, bus: &PcBus) -> Option<u8> {
        let instructions = self.instructions.as_ref()?;
        let mut common = None;
        for instruction in instructions {
            let tier = cpu.execution_tier_for_instruction_at(
                bus,
                instruction.address,
                instruction.prefix_length,
            );
            if common.is_some_and(|previous| previous != tier) {
                return None;
            }
            common = Some(tier);
        }
        common
    }

    /// Classifying every source instruction costs a label search each, so
    /// the uniform result is kept per label generation. Labels compare code
    /// bytes, so the result is cached only while the guarded bytes match.
    fn tier_proof(&mut self, cpu: &Cpu, bus: &PcBus) -> Option<TierProof> {
        let generation = cpu.execution_labels_generation();
        let uniform = if !self.source_matches(bus) {
            self.uniform_tier(cpu, bus)
        } else if let Some((cached, tier)) = self.uniform
            && cached == generation
        {
            tier
        } else {
            let tier = self.uniform_tier(cpu, bus);
            self.uniform = Some((generation, tier));
            tier
        };
        if let Some(tier) = uniform {
            return Some(TierProof::Uniform(tier));
        }
        if !self.canonical_copy {
            return None;
        }
        let kernel = self.binding.kernel?;
        let count = cpu.registers.get32(Gpr32::Ecx);
        let expected = kernel.actual_work(count)?;
        let mut counts = [0u64; 4];
        for (site, instruction) in self.instructions.as_ref()?.iter().enumerate() {
            let tier = cpu.execution_tier_for_instruction_at(
                bus,
                instruction.address,
                instruction.prefix_length,
            );
            counts[usize::from(tier)] += u64::from(kernel.canonical_repetitions(count, site)?);
        }
        (counts.iter().sum::<u64>() == u64::from(expected)).then_some(TierProof::Complete(counts))
    }

    fn source_matches(&mut self, bus: &PcBus) -> bool {
        if self.checked
            && self.guards.iter().all(|guard| {
                guard
                    .check
                    .as_ref()
                    .is_some_and(|check| bus.code_valid(check))
            })
        {
            return self.matches;
        }
        self.matches = self.binding.code.iter().all(|part| {
            let start = part.linear as usize;
            bus.ram_view(start..start + part.expected.len()) == Some(part.expected.as_slice())
        });
        for guard in &mut self.guards {
            guard.check = bus.ram.code_check(guard.start..guard.end);
        }
        self.checked = true;
        self.matches
    }
}

pub(crate) struct Plan {
    backend: Backend,
    code: CodeSegment,
    bindings: Vec<PreparedBinding>,
    /// CPU hint indices name only enabled entries, not all retained bindings.
    entry_bindings: Vec<usize>,
    source_pages: Vec<u8>,
    stats: Stats,
    failure: Option<ExecutionError>,
    last_error: Option<String>,
    tier_accounting: bool,
    /// Calls stop; the shared backing and statistics remain.
    suspended: bool,
    #[cfg(feature = "wasm-procedure-diagnostics")]
    diagnostic_mode: DiagnosticMode,
}

impl Plan {
    fn diagnostic_dispatch_enabled(&self) -> bool {
        #[cfg(feature = "wasm-procedure-diagnostics")]
        {
            self.diagnostic_mode == DiagnosticMode::Normal
        }
        #[cfg(not(feature = "wasm-procedure-diagnostics"))]
        {
            true
        }
    }

    fn dispatches_entries(&self) -> bool {
        !self.suspended && self.diagnostic_dispatch_enabled()
    }
}

/// Build both control-time lists before publishing either. Binding indices
/// remain stable when the CPU's sorted entry list omits disabled procedures.
fn procedure_entry_hints(
    plan: &Plan,
    dispatch: bool,
    enabled: impl Fn(usize, &PreparedBinding) -> bool,
) -> Result<(Vec<u32>, Vec<usize>), &'static str> {
    let mut entries = Vec::new();
    let mut indices = Vec::new();
    if dispatch {
        let count = plan
            .bindings
            .iter()
            .enumerate()
            .filter(|(index, binding)| enabled(*index, binding))
            .count();
        entries
            .try_reserve_exact(count)
            .map_err(|_| "WASM entry allocation")?;
        indices
            .try_reserve_exact(count)
            .map_err(|_| "WASM entry allocation")?;
        for (index, binding) in plan.bindings.iter().enumerate() {
            if enabled(index, binding) {
                entries.push(binding.binding.entry_ip);
                indices.push(index);
            }
        }
    }
    Ok((entries, indices))
}

/// Sorted, disjoint parts in ordinary RAM and inside `code`, covering the
/// entry and every finite terminal and guard. `place` gives each part's
/// linear address from its recorded one, so a move can be checked first.
fn validate_sources(
    binding: &Binding,
    code: CodeSegment,
    ram_size: usize,
    place: impl Fn(u32) -> Option<u32>,
) -> Result<(), String> {
    let entry = code
        .linear(binding.entry_ip)
        .ok_or("WASM procedure entry outside its code segment")?;
    let mut previous_end = 0;
    let mut entry_covered = false;
    for part in &binding.code {
        let linear = place(part.linear).ok_or("WASM procedure source outside RAM")?;
        let length =
            u32::try_from(part.expected.len()).map_err(|_| "WASM procedure source too large")?;
        let end = linear
            .checked_add(length)
            .filter(|&end| length != 0 && end as usize <= ram_size)
            .ok_or("WASM procedure source outside RAM")?;
        if linear < 0x500 || (linear < 0xc0000 && end > 0xa0000) {
            return Err("WASM procedure source outside ordinary RAM".into());
        }
        if linear < code.base || end - 1 - code.base > code.limit {
            return Err("WASM procedure source outside its code segment".into());
        }
        if linear < previous_end {
            return Err("overlapping WASM procedure source parts".into());
        }
        previous_end = end;
        entry_covered |= (linear..end).contains(&entry);
    }
    if !entry_covered {
        return Err("WASM procedure entry has no source proof".into());
    }
    if let Some(sites) = &binding.execution_sites {
        if sites.is_empty() || sites.len() > MAX_SOURCE_PARTS {
            return Err("invalid WASM execution site count".into());
        }
        let mut previous_end = 0u64;
        for site in sites {
            let end = u64::from(site.ip) + u64::from(site.length);
            if !(1..=15).contains(&site.length)
                || u64::from(site.ip) < previous_end
                || end > u64::from(code.limit) + 1
            {
                return Err("invalid or overlapping WASM execution sites".into());
            }
            let linear = code
                .linear(site.ip)
                .ok_or("WASM execution site outside code segment")?;
            let end = linear
                .checked_add(u32::from(site.length))
                .filter(|&end| end as usize <= ram_size)
                .ok_or("WASM execution site outside RAM")?;
            if linear < 0x500 || linear < 0xc0000 && end > 0xa0000 {
                return Err("WASM execution site outside ordinary RAM".into());
            }
            if !binding.code.iter().any(|part| {
                place(part.linear).is_some_and(|at| {
                    at <= linear && u64::from(linear) < u64::from(at) + part.expected.len() as u64
                })
            }) {
                return Err("WASM execution site has no opcode source proof".into());
            }
            previous_end = u64::from(site.ip) + u64::from(site.length);
        }
        let contains = |ip| sites.binary_search_by_key(&ip, |site| site.ip).is_ok();
        if !contains(binding.entry_ip) {
            return Err("WASM execution sites omit the entry".into());
        }
        if let Some(finite) = &binding.finite_computation
            && (finite.exits.iter().any(|&ip| {
                sites.iter().any(|site| {
                    site.ip <= ip && u64::from(ip) < u64::from(site.ip) + u64::from(site.length)
                })
            }) || finite
                .returns
                .iter()
                .chain(finite.guards.iter().map(|guard| &guard.entry_ip))
                .any(|&ip| !contains(ip)))
        {
            return Err("WASM execution sites disagree with finite terminals or guards".into());
        }
    }
    let sourced = |ip: u32| {
        code.linear(ip).is_some_and(|linear| {
            binding
                .code
                .iter()
                .any(|part| place(part.linear) == Some(linear))
        })
    };
    if let Some(finite) = &binding.finite_computation {
        if !finite
            .exits
            .iter()
            .chain(&finite.returns)
            .all(|&ip| sourced(ip))
        {
            return Err("finite computation terminal has no instruction source".into());
        }
        if !finite.guards.iter().all(|guard| sourced(guard.entry_ip)) {
            return Err("finite loop guard has no instruction source".into());
        }
    }
    Ok(())
}

/// Page-bounded byte guards of a binding's sources. Their pages are marked in
/// `source_pages` and passed to `protect` for generated stores.
fn source_guards(
    binding: &Binding,
    instructions: Option<&[SourceInstruction]>,
    source_pages: &mut [u8],
    mut protect: impl FnMut(u32, u32) -> Result<(), String>,
) -> Result<Vec<SourceGuard>, String> {
    let mut guards = Vec::new();
    let count = binding
        .code
        .iter()
        .map(|part| (part.linear as usize % PAGE_BYTES + part.expected.len()).div_ceil(PAGE_BYTES))
        .sum();
    guards
        .try_reserve_exact(count)
        .map_err(|_| "WASM source guard allocation")?;
    for part in &binding.code {
        let first = part.linear as usize / PAGE_BYTES;
        let last = (part.linear as usize + part.expected.len() - 1) / PAGE_BYTES;
        source_pages[first..=last].fill(1);
        protect(part.linear, part.expected.len() as u32)?;
        let mut start = part.linear as usize;
        let end = start + part.expected.len();
        while start < end {
            let chunk_end = end.min((start / PAGE_BYTES + 1) * PAGE_BYTES);
            guards.push(SourceGuard {
                start,
                end: chunk_end,
                check: None,
            });
            start = chunk_end;
        }
    }
    // Mutable operand slots are excluded from byte matching, but a generated
    // store into any part of their instruction must still leave the module.
    for insn in instructions.into_iter().flatten() {
        if insn.prefix_length < insn.length {
            let end = (insn.address as usize).saturating_add(usize::from(insn.length));
            if end <= source_pages.len() * PAGE_BYTES {
                let first = insn.address as usize / PAGE_BYTES;
                let last = (end - 1) / PAGE_BYTES;
                source_pages[first..=last].fill(1);
                protect(insn.address, u32::from(insn.length))?;
            }
        }
    }
    Ok(guards)
}

/// Decode a complete x87 instruction's length and displacement without
/// interpreting an address. This is used only for explicit x87 source proofs.
fn x87_source_shape(bytes: &[u8]) -> Option<(u8, core::ops::Range<usize>)> {
    let mut at = 0;
    let mut address16 = false;
    loop {
        match *bytes.get(at)? {
            0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 => at += 1,
            0x67 => {
                address16 = true;
                at += 1;
            }
            _ => break,
        }
        if at >= 13 {
            return None;
        }
    }
    let opcode = *bytes.get(at)?;
    if !(0xd8..=0xdf).contains(&opcode) {
        return None;
    }
    let modrm = *bytes.get(at + 1)?;
    let group = (modrm >> 3) & 7;
    let emitted = if modrm >> 6 == 3 {
        match opcode {
            0xd8 => true,
            0xd9 => matches!(group, 0 | 1),
            0xdc => matches!(group, 0 | 1 | 4..=7),
            0xdd => matches!(group, 2 | 3),
            0xde => matches!(group, 0 | 1 | 4..=7) || modrm == 0xd9,
            0xdf => modrm == 0xe0,
            _ => false,
        }
    } else {
        match opcode {
            0xd8 => true,
            0xd9 => group == 0,
            0xdb => matches!(group, 0 | 2 | 3),
            _ => false,
        }
    };
    if !emitted {
        return None;
    }
    if !matches!(doslet_x86::x87_decode::decode(opcode, modrm),
        doslet_x86::x87_decode::Encoding::I387(instruction) if instruction.implemented())
    {
        return None;
    }
    at += 2;
    let mode = modrm >> 6;
    let rm = modrm & 7;
    let displacement = if mode == 3 {
        0
    } else if address16 {
        match mode {
            0 if rm == 6 => 2,
            1 => 1,
            2 => 2,
            _ => 0,
        }
    } else {
        let base = if rm == 4 {
            let sib = *bytes.get(at)?;
            at += 1;
            sib & 7
        } else {
            rm
        };
        match mode {
            0 if base == 5 => 4,
            1 => 1,
            2 => 4,
            _ => 0,
        }
    };
    let end = at + displacement;
    if end > 15 || bytes.len() < end {
        return None;
    }
    Some((end as u8, at..end))
}

fn source_instructions(binding: &Binding) -> Option<Vec<SourceInstruction>> {
    let Some(sites) = &binding.execution_sites else {
        return source_instructions_from_parts(&binding.code);
    };
    let mut instructions = Vec::new();
    instructions.try_reserve_exact(sites.len()).ok()?;
    for site in sites {
        let start = binding.code_segment.linear(site.ip)?;
        let end = start.checked_add(u32::from(site.length))?;
        let mut window = [0u8; 15];
        let mut holes = [0usize; 2];
        let mut hole_count = 0;
        let mut prefix_length = site.length;
        let mut cursor = start;
        let first = binding.code.partition_point(|part| {
            part.linear
                .checked_add(part.expected.len() as u32)
                .is_some_and(|end| end <= start)
        });
        for part in &binding.code[first..] {
            if part.linear >= end {
                break;
            }
            let low = part.linear.max(start);
            let high = part
                .linear
                .checked_add(part.expected.len() as u32)?
                .min(end);
            if high <= low {
                continue;
            }
            if low > cursor {
                if cursor == start || (low - cursor) % 4 != 0 {
                    return None;
                }
                while cursor < low {
                    if hole_count == holes.len() {
                        return None;
                    }
                    holes[hole_count] = (cursor - start) as usize;
                    prefix_length = prefix_length.min((cursor - start) as u8);
                    hole_count += 1;
                    cursor = cursor.checked_add(4)?;
                }
            }
            window[(low - start) as usize..(high - start) as usize].copy_from_slice(
                &part.expected[(low - part.linear) as usize..(high - part.linear) as usize],
            );
            cursor = high;
        }
        if cursor == start || (end - cursor) % 4 != 0 {
            return None;
        }
        while cursor < end {
            if hole_count == holes.len() {
                return None;
            }
            holes[hole_count] = (cursor - start) as usize;
            prefix_length = prefix_length.min((cursor - start) as u8);
            hole_count += 1;
            cursor = cursor.checked_add(4)?;
        }
        let bytes = &window[..usize::from(site.length)];
        let x87 = binding.x87.and_then(|_| x87_source_shape(bytes));
        if binding.x87.is_some()
            && bytes
                .iter()
                .copied()
                .find(|byte| !matches!(byte, 0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67))
                .is_some_and(|opcode| (0xd8..=0xdf).contains(&opcode))
            && x87.is_none()
        {
            return None;
        }
        if doslet_x86::translation::instruction_length(bytes, true) != Some(site.length)
            && x87.as_ref().map(|shape| shape.0) != Some(site.length)
        {
            return None;
        }
        for &offset in &holes[..hole_count] {
            let mut patched = window;
            patched[offset..offset + 4].fill(0xff);
            if !doslet_x86::translation::same_template32(
                bytes,
                &patched[..usize::from(site.length)],
            ) && !x87.as_ref().is_some_and(|(_, displacement)| {
                displacement.len() == 4 && displacement.start == offset
            }) {
                return None;
            }
        }
        instructions.push(SourceInstruction {
            address: start,
            length: site.length,
            prefix_length,
            flow: match x87 {
                Some(_) => doslet_x86::translation::NearFlow::Next,
                None => doslet_x86::translation::near_flow(bytes, true)?,
            },
        });
    }
    Some(instructions)
}

fn source_instructions_from_parts(parts: &[CodePart]) -> Option<Vec<SourceInstruction>> {
    let mut instructions = Vec::new();
    let mut skip_in_next = 0usize;
    for (part_index, part) in parts.iter().enumerate() {
        if skip_in_next > part.expected.len() {
            return None;
        }
        let mut cursor = skip_in_next;
        skip_in_next = 0;
        while cursor < part.expected.len() {
            if instructions.len() >= MAX_SOURCE_PARTS {
                return None;
            }
            let remaining = &part.expected[cursor..];
            if let Some(length) = doslet_x86::translation::instruction_length(remaining, true)
                && length != 0
                && usize::from(length) <= remaining.len()
            {
                instructions.try_reserve(1).ok()?;
                instructions.push(SourceInstruction {
                    address: part.linear.checked_add(cursor as u32)?,
                    length,
                    prefix_length: length,
                    flow: doslet_x86::translation::near_flow(remaining, true)?,
                });
                cursor += usize::from(length);
                continue;
            }
            // Reconstruct an instruction whose 4-byte displacement/immediate
            // virtual-register slot was excluded from the immutable source part.
            let prefix_len = remaining.len();
            if prefix_len == 0 || prefix_len + 4 > 15 {
                return None;
            }
            let gap_start = part.linear.checked_add(part.expected.len() as u32)?;
            let after_gap = gap_start.checked_add(4)?;
            let mut window = [0u8; 15];
            window[..prefix_len].copy_from_slice(remaining);
            if let Some(next_part) = parts.get(part_index + 1)
                && next_part.linear == after_gap
            {
                let copy_len = next_part.expected.len().min(15 - (prefix_len + 4));
                window[prefix_len + 4..prefix_len + 4 + copy_len]
                    .copy_from_slice(&next_part.expected[..copy_len]);
            }
            let length = doslet_x86::translation::instruction_length(&window, true)?;
            if usize::from(length) < prefix_len + 4 {
                return None;
            }
            let suffix_needed = usize::from(length) - (prefix_len + 4);
            if suffix_needed != 0 {
                let next_part = parts.get(part_index + 1)?;
                if next_part.linear == after_gap && next_part.expected.len() >= suffix_needed {
                    skip_in_next = suffix_needed;
                } else if suffix_needed != 4 {
                    return None;
                }
            }
            instructions.try_reserve(1).ok()?;
            instructions.push(SourceInstruction {
                address: part.linear.checked_add(cursor as u32)?,
                length,
                prefix_length: prefix_len as u8,
                flow: doslet_x86::translation::near_flow(&window[..usize::from(length)], true)?,
            });
            cursor = part.expected.len();
        }
    }
    if skip_in_next != 0 {
        return None;
    }
    Some(instructions)
}

fn canonical_copy_sources(
    kernel: CopyKernel,
    entry: u32,
    instructions: &[SourceInstruction],
) -> bool {
    // The trusted kernel declaration proves the semantic idiom. Independently
    // prove that sorted source sites follow its exact CFG before assigning the
    // contract's roles: three entry sites, prefix padding, seven loop sites,
    // AND/JZ, tail padding, byte load/store and RET. Reordered blocks decline.
    use doslet_x86::translation::NearFlow;
    if kernel.canonical_sites() != Some(instructions.len())
        || instructions
            .first()
            .is_none_or(|site| site.address != entry)
        || instructions.windows(2).any(|sites| {
            sites[0].address.checked_add(u32::from(sites[0].length)) != Some(sites[1].address)
        })
    {
        return false;
    }
    let prefix = kernel.prefix_nops as usize;
    let tail = kernel.tail_nops as usize;
    let initial = 2;
    let backedge = 9 + prefix;
    let final_branch = 11 + prefix;
    let tail_load = 12 + prefix + tail;
    let ret = instructions.len() - 1;
    for (site, instruction) in instructions.iter().enumerate() {
        let branch = if site == initial {
            Some((4, tail_load))
        } else if site == backedge {
            Some((5, 3 + prefix))
        } else if site == final_branch {
            Some((4, ret))
        } else {
            None
        };
        let valid = if let Some((expected, target)) = branch {
            matches!(instruction.flow, NearFlow::Branch { condition: Some(condition), displacement }
                if condition == expected && instruction.address.wrapping_add(u32::from(instruction.length)).wrapping_add_signed(displacement) == instructions[target].address)
        } else if site == ret {
            instruction.flow == (NearFlow::Return { pop: 0, width: 4 })
        } else {
            instruction.flow == NearFlow::Next
        };
        if !valid {
            return false;
        }
    }
    true
}

pub(crate) struct PreparedRleCall {
    function_index: u32,
    frame: Frame,
    descriptor: doslet_x86_wasm::static_program::memory::Descriptor,
    admission: RleAdmission,
}

fn call_frame(cpu: &Cpu) -> Frame {
    Frame {
        gpr: [
            Gpr32::Eax,
            Gpr32::Ecx,
            Gpr32::Edx,
            Gpr32::Ebx,
            Gpr32::Esp,
            Gpr32::Ebp,
            Gpr32::Esi,
            Gpr32::Edi,
        ]
        .map(|register| cpu.registers.get32(register)),
        eflags: cpu.flags,
        address: Address {
            space: 0,
            offset: cpu.registers.ip,
        },
        ..Frame::default()
    }
}

impl PcBus {
    fn prepare_rle_call(
        &mut self,
        cpu: &Cpu,
        function_index: u32,
        source_pages: &[u8],
    ) -> Option<u32> {
        self.wasm_rle_call = None;
        let windows = self.wasm_call_windows.resolve(&cpu.registers);
        let stack = if cpu.registers.stack32() {
            StackAddress::Bits32
        } else {
            StackAddress::Bits16
        };
        let frame = call_frame(cpu);
        let (descriptor, admission) = match &self.ram.storage {
            Storage::Wasm(owner) => {
                let descriptor = owner.descriptor(windows, stack);
                let admission = owner.prepare_rle(&frame, &descriptor)?;
                (descriptor, admission)
            }
            _ => {
                let descriptor = doslet_x86_wasm::static_program::memory::Descriptor {
                    windows,
                    stack_address: stack,
                    ..Default::default()
                };
                let admission = prepare_rle(
                    RleMemory {
                        ram: self.ram.storage.bytes(),
                        generations: self.ram.storage.generations(),
                        protected_pages: source_pages,
                        ram_pointer: 0,
                    },
                    &frame,
                    &windows,
                    stack,
                )?;
                (descriptor, admission)
            }
        };
        let work = admission.work();
        if work > MAX_ATOMIC_PROCEDURE_WORK {
            return None;
        }
        cpu.registers
            .address(
                SegmentRegister::Cs,
                admission.return_target(),
                1,
                Access::Execute,
            )
            .ok()?;
        self.wasm_rle_call = Some(PreparedRleCall {
            function_index,
            frame,
            descriptor,
            admission,
        });
        Some(work)
    }

    fn finish_procedure(
        &mut self,
        frame: Frame,
        exit: Exit,
        effects: doslet_x86_wasm::static_program::memory::Effects,
    ) -> Result<MemoryProcedureResult, String> {
        self.wasm_device_effects = effects;
        self.ram.complete_direct_writes(effects.ram_writes);
        self.device_accesses = self
            .device_accesses
            .wrapping_add(effects.device_reads)
            .wrapping_add(effects.device_writes);
        if effects.device_reads != 0 || effects.device_writes != 0 {
            self.vga.complete_direct_reads(effects.latches);
        }
        self.vga.complete_direct_writes(effects.device_changes);
        let instructions = frame
            .exact_total()
            .and_then(|count| u32::try_from(count).ok())
            .ok_or("WASM procedure returned an invalid instruction count")?;
        Ok(MemoryProcedureResult {
            registers: ProcedureRegisters {
                gpr: frame.gpr,
                flags: frame.eflags,
            },
            instruction_pointer: frame.address.offset,
            instructions,
            completed: exit == Exit::Complete,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    Wasm,
    Interpreter,
}

enum Module<'a> {
    #[cfg(not(target_arch = "wasm32"))]
    Native(&'a [u8], Option<&'a mut dyn CompiledCache>),
    #[cfg(target_arch = "wasm32")]
    Browser(BrowserCall),
    Interpreter(core::marker::PhantomData<&'a ()>),
}

impl PcMachine {
    /// Compile and install one prepared module, then migrate RAM, VRAM and page
    /// generations once. Subsequent interpreter, device and WASM accesses
    /// borrow the same bytes. Validation failure leaves the machine unchanged.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn install_wasm_procedures(
        &mut self,
        wasm: &[u8],
        bindings: Vec<Binding>,
    ) -> Result<(), String> {
        self.install_procedures(Module::Native(wasm, None), bindings, false)
    }

    /// As `install_wasm_procedures`, reusing compatible native code from the host.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn install_wasm_procedures_cached(
        &mut self,
        wasm: &[u8],
        bindings: Vec<Binding>,
        cache: &mut dyn CompiledCache,
    ) -> Result<(), String> {
        self.install_procedures(Module::Native(wasm, Some(cache)), bindings, false)
    }

    /// As `install_wasm_procedures_cached`, replacing installed procedures,
    /// for example after a program patched its own code. RAM, page
    /// generations and VRAM move once from the current owner to the new one;
    /// statistics restart. Validation failure keeps the installed plan.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn replace_wasm_procedures_cached(
        &mut self,
        wasm: &[u8],
        bindings: Vec<Binding>,
        cache: &mut dyn CompiledCache,
    ) -> Result<(), String> {
        self.install_procedures(Module::Native(wasm, Some(cache)), bindings, true)
    }

    /// Install exports from a module importing this worker's linear memory.
    /// The callback table is sorted by function ID and fixed before execution.
    #[cfg(target_arch = "wasm32")]
    pub fn install_browser_wasm_procedures(
        &mut self,
        callback: BrowserCall,
        bindings: Vec<Binding>,
    ) -> Result<(), String> {
        self.install_procedures(Module::Browser(callback), bindings, false)
    }

    /// Interpret admitted finite semantic procedures through RET before IRQ
    /// delivery. This reference policy retains the ordinary RAM/VRAM owners;
    /// no WASM is compiled. Other instructions keep ordinary IRQ boundaries.
    pub fn install_interpreted_procedure_boundaries(
        &mut self,
        bindings: Vec<Binding>,
    ) -> Result<(), String> {
        self.install_procedures(
            Module::Interpreter(core::marker::PhantomData),
            bindings,
            false,
        )
    }

    fn install_procedures(
        &mut self,
        module: Module<'_>,
        mut bindings: Vec<Binding>,
        replace: bool,
    ) -> Result<(), String> {
        let wasm = !matches!(&module, Module::Interpreter(_));
        if self.wasm_procedures.is_some() && !replace {
            return Err("WASM procedures already installed".into());
        }
        if !self.bus.ram.direct_execution_ready() {
            return Err("WASM procedures require unrecorded RAM with a valid code identity".into());
        }
        if bindings.is_empty() || bindings.len() > MAX_BINDINGS {
            return Err("WASM procedures require 1 through 4096 bindings".into());
        }
        bindings.sort_unstable_by_key(|binding| binding.entry_ip);
        if bindings
            .windows(2)
            .any(|pair| pair[0].entry_ip == pair[1].entry_ip)
        {
            return Err("duplicate WASM procedure entry".into());
        }
        let mut ids: Vec<_> = bindings.iter().map(|binding| binding.function_id).collect();
        ids.sort_unstable();
        if ids.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err("duplicate WASM procedure function ID".into());
        }
        let code = bindings[0].code_segment;
        if bindings.iter().any(|binding| binding.code_segment != code) {
            return Err("WASM procedures must share one code segment".into());
        }
        let mut source_parts = 0usize;
        let mut source_bytes = 0usize;
        let mut execution_sites = 0usize;
        for binding in &mut bindings {
            if binding.x87.is_some()
                && (!wasm
                    || binding.kernel.is_some()
                    || binding.rle_kernel.is_some()
                    || binding.execution_sites.is_none())
            {
                return Err("x87 procedures require an ordinary WASM body".into());
            }
            if !wasm
                && binding.kernel.is_none()
                && binding.rle_kernel.is_none()
                && binding.finite_computation.is_none()
            {
                return Err(
                    "interpreted procedure boundaries require finite semantic kernels".into(),
                );
            }
            if binding.rle_kernel.is_none()
                && binding.finite_computation.is_none()
                && !(1..=MAX_PROCEDURE_INSTRUCTIONS).contains(&binding.maximum_instructions)
            {
                return Err("WASM procedure maximum work outside 1 through 4096".into());
            }
            if binding.rle_kernel.is_some()
                && (binding.kernel.is_some()
                    || binding.finite_computation.is_some()
                    || u64::from(binding.maximum_instructions) != RleKernel.maximum_work())
            {
                return Err("invalid RLE procedure declaration".into());
            }
            if let Some(finite) = &mut binding.finite_computation {
                if binding.kernel.is_some()
                    || !(1..=MAX_ATOMIC_PROCEDURE_WORK).contains(&binding.maximum_instructions)
                    || finite.exits.len() + finite.returns.len() > 256
                    || finite.guards.len() > 1
                    || (finite.exits.is_empty() && finite.returns.is_empty())
                {
                    return Err("invalid finite computation declaration".into());
                }
                if finite.guards.iter().any(|guard| {
                    guard.counter >= 8
                        || !(1..=MAX_ATOMIC_PROCEDURE_WORK).contains(&guard.maximum_trips)
                }) {
                    return Err("invalid finite loop guard declaration".into());
                }
                finite.exits.sort_unstable();
                finite.returns.sort_unstable();
                if finite.exits.windows(2).any(|pair| pair[0] == pair[1])
                    || finite.returns.windows(2).any(|pair| pair[0] == pair[1])
                    || finite
                        .exits
                        .iter()
                        .any(|exit| finite.returns.binary_search(exit).is_ok())
                {
                    return Err("duplicate finite computation terminal".into());
                }
            }
            if binding.kernel.is_some_and(|kernel| {
                !kernel.is_valid() || kernel.max_work_per_call != binding.maximum_instructions
            }) {
                return Err("WASM kernel metadata differs from its work reservation".into());
            }
            source_parts = source_parts
                .checked_add(binding.code.len())
                .filter(|&count| count <= MAX_SOURCE_PARTS)
                .ok_or("WASM procedure sources exceed 65536 parts")?;
            binding.code.sort_unstable_by_key(|part| part.linear);
            for part in &binding.code {
                source_bytes = source_bytes
                    .checked_add(part.expected.len())
                    .filter(|&count| count <= MAX_SOURCE_BYTES)
                    .ok_or("WASM procedure sources exceed 16 MiB")?;
            }
            validate_sources(binding, code, self.bus.ram_size(), Some)?;
            if let Some(sites) = &binding.execution_sites {
                execution_sites = execution_sites
                    .checked_add(sites.len())
                    .filter(|&count| count <= MAX_SOURCE_PARTS)
                    .ok_or("WASM execution sites exceed 65536 entries")?;
                let instructions = source_instructions(binding)
                    .ok_or("WASM execution sites disagree with frozen instruction bytes")?;
                if let Some(kernel) = binding.kernel
                    && !canonical_copy_sources(
                        kernel,
                        code.linear(binding.entry_ip).expect("validated entry"),
                        &instructions,
                    )
                {
                    return Err("WASM execution sites disagree with canonical copy contract".into());
                }
            }
        }
        let mut owner = match module {
            #[cfg(not(target_arch = "wasm32"))]
            Module::Native(bytes, cache) => Some(SharedProcedureMemory::with_cache(
                bytes,
                &ids,
                self.bus.ram_size(),
                cache,
            )?),
            #[cfg(target_arch = "wasm32")]
            Module::Browser(callback) => Some(SharedProcedureMemory::new(
                &ids,
                self.bus.ram_size(),
                callback,
            )?),
            Module::Interpreter(_) => None,
        };
        let mut prepared = Vec::new();
        let mut source_pages = alloc::vec![0u8; self.bus.ram_size().div_ceil(PAGE_BYTES)];
        let mut entry_ips = Vec::new();
        let mut entry_bindings = Vec::new();
        prepared
            .try_reserve_exact(bindings.len())
            .map_err(|_| "WASM binding allocation")?;
        entry_ips
            .try_reserve_exact(bindings.len())
            .map_err(|_| "WASM entry allocation")?;
        entry_bindings
            .try_reserve_exact(bindings.len())
            .map_err(|_| "WASM entry allocation")?;
        for binding in bindings {
            let instructions = source_instructions(&binding);
            if binding.execution_sites.is_some() && instructions.is_none() {
                return Err("WASM execution site preparation failed".into());
            }
            let canonical_copy =
                binding
                    .kernel
                    .zip(instructions.as_ref())
                    .is_some_and(|(kernel, sources)| {
                        canonical_copy_sources(kernel, code.base + binding.entry_ip, sources)
                    });
            let function_index = ids
                .binary_search(&binding.function_id)
                .expect("validated procedure ID") as u32;
            let guards = source_guards(
                &binding,
                instructions.as_deref(),
                &mut source_pages,
                |linear, length| {
                    owner
                        .as_mut()
                        .map_or(Ok(()), |owner| owner.protect_source(linear, length))
                },
            )?;
            entry_ips.push(binding.entry_ip);
            entry_bindings.push(prepared.len());
            prepared.push(PreparedBinding {
                binding,
                function_index,
                enabled: true,
                guards,
                checked: false,
                matches: false,
                instructions,
                canonical_copy,
                uniform: None,
                execution: ProcedureEntryStats::default(),
            });
        }
        // Publish hints only after every fallible preparation step. They never
        // authorize execution: context, budget and source proofs stay below.
        self.cpu
            .set_prepared_entries_at(code.base, entry_ips)
            .map_err(String::from)?;
        if let Some(mut owner) = owner {
            // Setup-only copies. The old allocation is dropped by replace_storage.
            owner
                .ram_mut()
                .copy_from_slice(self.bus.ram.storage.bytes());
            owner
                .generations_mut()
                .copy_from_slice(self.bus.ram.storage.generations());
            // A replaced owner holds VRAM; otherwise VGA still owns it.
            match self.bus.ram.storage.video() {
                Some(video) => owner.video_mut().copy_from_slice(video),
                None => self.bus.vga.transfer_memory_into(owner.video_mut()),
            }
            self.bus.ram.replace_storage(Storage::Wasm(Box::new(owner)));
        }
        self.cpu.set_dynamic_core(false);
        self.wasm_procedures = Some(Plan {
            backend: if wasm {
                Backend::Wasm
            } else {
                Backend::Interpreter
            },
            code,
            bindings: prepared,
            entry_bindings,
            source_pages,
            stats: Stats::default(),
            failure: None,
            last_error: None,
            // Native callers retain their existing refusal unless they opt in,
            // as speed tiers do. Browser procedures always use this
            // conservative byte-validated policy.
            tier_accounting: cfg!(target_arch = "wasm32"),
            suspended: false,
            #[cfg(feature = "wasm-procedure-diagnostics")]
            diagnostic_mode: DiagnosticMode::Normal,
        });
        Ok(())
    }

    /// Stop or resume procedure calls, for example when they are too short
    /// to beat the interpreter. Memory stays shared with the installed plan.
    pub fn set_wasm_procedures_suspended(&mut self, suspended: bool) -> Result<(), &'static str> {
        let plan = self
            .wasm_procedures
            .as_ref()
            .ok_or("WASM procedures are not installed")?;
        if plan.suspended == suspended {
            return Ok(());
        }
        let (entries, indices) = procedure_entry_hints(
            plan,
            !suspended && plan.diagnostic_dispatch_enabled(),
            |_, binding| binding.enabled,
        )?;
        self.cpu.set_prepared_entries_at(plan.code.base, entries)?;
        let plan = self.wasm_procedures.as_mut().expect("installed plan");
        plan.suspended = suspended;
        plan.entry_bindings = indices;
        Ok(())
    }

    /// Remove installed procedures and return RAM, page generations and VRAM
    /// to their ordinary owners, as before installation. A suspended plan's
    /// shared backing still costs the interpreter time on every access
    /// outside batches. Needs unrecorded RAM, like installation; copies once.
    pub fn uninstall_wasm_procedures(&mut self) -> Result<(), &'static str> {
        if self.wasm_procedures.is_none() {
            return Err("WASM procedures are not installed");
        }
        if !self.bus.ram.direct_execution_ready() {
            return Err("WASM procedures can be removed only while RAM is not recorded");
        }
        self.cpu.set_prepared_entries(Vec::new())?;
        self.bus.wasm_rle_call = None;
        self.wasm_procedures = None;
        let owned = match &self.bus.ram.storage {
            Storage::Wasm(owner) => {
                self.bus.vga.restore_memory(owner.video());
                Some(Storage::Owned {
                    bytes: owner.ram().into(),
                    generations: owner.generations().into(),
                })
            }
            Storage::Owned { .. } => None,
        };
        if let Some(storage) = owned {
            self.bus.ram.replace_storage(storage);
        }
        Ok(())
    }

    /// The code segment the installed procedures are bound to.
    pub fn wasm_procedure_code_segment(&self) -> Option<CodeSegment> {
        self.wasm_procedures.as_ref().map(|plan| plan.code)
    }

    /// Follow code that moved with its segment to `base`, as a DJGPP program's
    /// does when DPMI 0503h moves its block. Compiled procedures name EIPs and
    /// are kept; source proofs, guards, page protections and entry hints move.
    /// Every moved source is validated before anything changes. The previous
    /// pages stay protected, so generated stores there leave to the interpreter.
    pub fn rebase_wasm_procedures(&mut self, base: u32) -> Result<(), String> {
        let ram_size = self.bus.ram_size();
        let plan = self
            .wasm_procedures
            .as_mut()
            .ok_or("WASM procedures are not installed")?;
        let old = plan.code;
        if old.base == base {
            return Ok(());
        }
        // Virtual operand slots are absolute addresses in generated code.
        // Changing only source proofs would leave those loads at the old base.
        if plan.bindings.iter().any(|prepared| {
            prepared.instructions.as_ref().is_none_or(|instructions| {
                instructions
                    .iter()
                    .any(|insn| insn.prefix_length < insn.length)
            })
        }) {
            return Err(
                "procedures with mutable or unknown operand metadata require recompilation before rebasing"
                    .into(),
            );
        }
        let code = CodeSegment {
            base,
            limit: old.limit,
        };
        let place = |linear: u32| base.checked_add(linear.checked_sub(old.base)?);
        for prepared in &plan.bindings {
            validate_sources(&prepared.binding, code, ram_size, place)?;
        }
        let mut owner = match &mut self.bus.ram.storage {
            #[cfg(feature = "wasm-procedures")]
            Storage::Wasm(owner) => Some(owner),
            _ => None,
        };
        let mut source_pages = alloc::vec![0u8; plan.source_pages.len()];
        for prepared in &mut plan.bindings {
            for part in &mut prepared.binding.code {
                part.linear = place(part.linear).expect("validated moved source");
            }
            prepared.binding.code_segment = code;
            prepared.instructions = source_instructions(&prepared.binding);
            prepared.guards = source_guards(
                &prepared.binding,
                prepared.instructions.as_deref(),
                &mut source_pages,
                |at, length| {
                    owner
                        .as_mut()
                        .map_or(Ok(()), |owner| owner.protect_source(at, length))
                },
            )?;
            prepared.checked = false;
            prepared.matches = false;
            prepared.uniform = None;
        }
        plan.source_pages = source_pages;
        plan.code = code;
        let (entries, indices) =
            procedure_entry_hints(plan, plan.dispatches_entries(), |_, binding| {
                binding.enabled
            })
            .map_err(String::from)?;
        self.cpu
            .set_prepared_entries_at(base, entries)
            .map_err(String::from)?;
        plan.entry_bindings = indices;
        Ok(())
    }

    /// None is installed and RAM is not being recorded for an idle or
    /// recurrence proof, which ends at a later batch boundary.
    pub fn wasm_procedures_installable(&self) -> bool {
        self.wasm_procedures.is_none() && self.bus.ram.direct_execution_ready()
    }

    /// Installed, and RAM is not being recorded, so a replacement can move it.
    pub fn wasm_procedures_replaceable(&self) -> bool {
        self.wasm_procedures.is_some() && self.bus.ram.direct_execution_ready()
    }

    /// Installed and not suspended.
    pub fn wasm_procedures_active(&self) -> bool {
        self.wasm_procedures
            .as_ref()
            .is_some_and(|plan| !plan.suspended)
    }

    pub fn wasm_procedure_stats(&self) -> Option<Stats> {
        self.wasm_procedures.as_ref().map(|plan| plan.stats)
    }

    pub fn wasm_procedure_entry_count(&self) -> usize {
        self.wasm_procedures
            .as_ref()
            .map_or(0, |plan| plan.bindings.len())
    }

    /// EIP of the indexed prepared entry; ordering is ascending by EIP.
    pub fn wasm_procedure_entry_ip(&self, index: usize) -> Option<u32> {
        self.wasm_procedures
            .as_ref()?
            .bindings
            .get(index)
            .map(|prepared| prepared.binding.entry_ip)
    }

    /// A copy of the indexed entry's accepted execution totals.
    pub fn wasm_procedure_entry_stats(&self, index: usize) -> Option<ProcedureEntryStats> {
        self.wasm_procedures
            .as_ref()?
            .bindings
            .get(index)
            .map(|prepared| prepared.execution)
    }

    /// Configured admission for the indexed entry, independent of suspension.
    pub fn wasm_procedure_entry_enabled(&self, index: usize) -> Option<bool> {
        Some(self.wasm_procedures.as_ref()?.bindings.get(index)?.enabled)
    }

    /// Change one interpreter-to-procedure entry without removing its function
    /// or statistics. Hint rebuilding may allocate here, outside execution.
    pub fn set_wasm_procedure_entry_enabled(
        &mut self,
        index: usize,
        enabled: bool,
    ) -> Result<(), &'static str> {
        let plan = self
            .wasm_procedures
            .as_ref()
            .ok_or("WASM procedures are not installed")?;
        let binding = plan
            .bindings
            .get(index)
            .ok_or("WASM procedure entry index outside plan")?;
        if binding.enabled == enabled {
            return Ok(());
        }
        let (entries, indices) =
            procedure_entry_hints(plan, plan.dispatches_entries(), |candidate, binding| {
                if candidate == index {
                    enabled
                } else {
                    binding.enabled
                }
            })?;
        self.cpu.set_prepared_entries_at(plan.code.base, entries)?;
        let plan = self.wasm_procedures.as_mut().expect("installed plan");
        plan.bindings[index].enabled = enabled;
        plan.entry_bindings = indices;
        Ok(())
    }

    /// Apply one selection in ascending entry-EIP order, rebuilding interpreter
    /// fences once. A bad length or failed preparation leaves admission intact.
    /// Disabled entries retain their compiled functions and accumulated totals.
    pub fn set_wasm_procedure_entries_enabled(
        &mut self,
        enabled: &[bool],
    ) -> Result<(), &'static str> {
        let plan = self
            .wasm_procedures
            .as_ref()
            .ok_or("WASM procedures are not installed")?;
        if enabled.len() != plan.bindings.len() {
            return Err("WASM procedure entry selection length differs from plan");
        }
        if plan
            .bindings
            .iter()
            .zip(enabled)
            .all(|(binding, enabled)| binding.enabled == *enabled)
        {
            return Ok(());
        }
        let (entries, indices) =
            procedure_entry_hints(plan, plan.dispatches_entries(), |index, _| enabled[index])?;
        self.cpu.set_prepared_entries_at(plan.code.base, entries)?;
        let plan = self.wasm_procedures.as_mut().expect("installed plan");
        for (binding, &enabled) in plan.bindings.iter_mut().zip(enabled) {
            binding.enabled = enabled;
        }
        plan.entry_bindings = indices;
        Ok(())
    }

    pub fn wasm_procedure_copy_tier_entries(&self) -> impl Iterator<Item = u32> + '_ {
        self.wasm_procedures
            .iter()
            .flat_map(|plan| plan.bindings.iter())
            .filter(|binding| binding.canonical_copy)
            .map(|binding| binding.binding.entry_ip)
    }

    /// Allow byte-validated uniform procedures and proved per-site copy counts.
    /// The browser adapter enables this; native speed tiers enable it with
    /// their labels.
    pub fn set_wasm_procedure_tier_accounting(
        &mut self,
        enabled: bool,
    ) -> Result<(), &'static str> {
        self.wasm_procedures
            .as_mut()
            .ok_or("WASM procedures are not installed")?
            .tier_accounting = enabled;
        Ok(())
    }

    pub fn wasm_procedure_last_error(&self) -> Option<&str> {
        self.wasm_procedures.as_ref()?.last_error.as_deref()
    }

    /// Change only dispatch and interpreter-bus selection. The installed plan,
    /// shared backing, source guards and accumulated statistics remain intact.
    /// Rebuilding normal entry hints may allocate here, outside execution.
    #[cfg(feature = "wasm-procedure-diagnostics")]
    pub fn set_wasm_procedure_diagnostic_mode(
        &mut self,
        mode: DiagnosticMode,
    ) -> Result<(), &'static str> {
        let plan = self
            .wasm_procedures
            .as_ref()
            .ok_or("WASM procedure diagnostics require an installed plan")?;
        if plan.diagnostic_mode == mode {
            return Ok(());
        }
        let (entries, indices) = procedure_entry_hints(
            plan,
            !plan.suspended && mode == DiagnosticMode::Normal,
            |_, binding| binding.enabled,
        )?;
        self.cpu.set_prepared_entries_at(plan.code.base, entries)?;
        let plan = self
            .wasm_procedures
            .as_mut()
            .expect("installed diagnostic plan");
        plan.diagnostic_mode = mode;
        plan.entry_bindings = indices;
        Ok(())
    }

    #[cfg(feature = "wasm-procedure-diagnostics")]
    pub(crate) fn wasm_procedure_dispatch_enabled(&self) -> bool {
        self.wasm_procedures
            .as_ref()
            .is_some_and(|plan| plan.diagnostic_mode == DiagnosticMode::Normal)
    }

    #[cfg(feature = "wasm-procedure-diagnostics")]
    pub(crate) fn wasm_procedure_borrowed_bus_enabled(&self) -> bool {
        self.wasm_procedures
            .as_ref()
            .is_none_or(|plan| plan.diagnostic_mode != DiagnosticMode::SharedPcBus)
    }

    pub(crate) fn check_wasm_procedure_failure(&self) -> Result<(), MachineError> {
        if let Some(failure) = self.wasm_procedures.as_ref().and_then(|plan| plan.failure) {
            Err(MachineError::WasmProcedure(failure))
        } else {
            Ok(())
        }
    }

    pub(crate) fn try_wasm_procedure(&mut self, budget: u32) -> Result<Option<u32>, MachineError> {
        #[cfg(feature = "wasm-procedure-diagnostics")]
        if !self.wasm_procedure_dispatch_enabled() {
            return Ok(None);
        }
        let Some(plan) = self.wasm_procedures.as_ref().filter(|plan| !plan.suspended) else {
            return Ok(None);
        };
        // Membership is cached independently of code bytes. This hint avoids
        // searching the registry for ordinary interpreter batches; the source
        // and architectural proofs still authorize every foreign invocation.
        let Some(hint) = self.cpu.prepared_entry_index() else {
            return Ok(None);
        };
        let Some(&index) = plan.entry_bindings.get(hint) else {
            return Ok(None);
        };
        if plan.bindings.get(index).is_none_or(|binding| {
            !binding.enabled || binding.binding.entry_ip != self.cpu.registers.ip
        }) {
            return Ok(None);
        }
        if self.vga_wait.candidate.is_some()
            || self.memory_wait_sites.is_some()
            || self.batch_yield_ip.is_some()
            || !self.bus.a20_enabled()
            || !self.bus.ram.direct_execution_ready()
            || (self.cpu.accepts_maskable_interrupt() && self.bus.pic.has_interrupt())
        {
            return Ok(None);
        }
        let Some(context) = ProcedureContext::capture(&self.cpu) else {
            return Ok(None);
        };
        // Entries are EIPs in the bound segment; its limit must cover every
        // source, branch and exit the compiler proved within that code.
        let (base, limit) = context.code_segment();
        if base != plan.code.base || limit < plan.code.limit {
            return Ok(None);
        }
        let tier = if self.cpu.execution_labels_enabled() {
            let plan = self.wasm_procedures.as_mut().expect("prepared plan");
            let tier = plan
                .tier_accounting
                .then(|| plan.bindings[index].tier_proof(&self.cpu, &self.bus))
                .flatten();
            let Some(tier) = tier else {
                plan.stats.tier_refusals += 1;
                return Ok(None);
            };
            Some(tier)
        } else {
            None
        };
        let plan = self.wasm_procedures.as_ref().expect("prepared plan");
        let binding = &plan.bindings[index];
        self.bus.wasm_rle_call = None;
        if let Storage::Wasm(owner) = &mut self.bus.ram.storage {
            owner.clear_pc24_boundary();
        }
        if let Some(x87) = binding.binding.x87 {
            if self.cpu.control.cr0 & 12 != 0 {
                return Ok(None);
            }
            let Some(fpu) = self.cpu.fpu.as_ref() else {
                return Ok(None);
            };
            let top = u32::from((fpu.status() >> 11) & 7);
            let read = x87.read_mask.rotate_left(top);
            let write = x87.write_mask.rotate_left(top);
            let mut boundary = [0; (X87_SELECTORS_OFFSET - X87_BOUNDARY_OFFSET) as usize];
            if !fpu.write_pc24_boundary(read, write, &mut boundary) {
                return Ok(None);
            }
            let Storage::Wasm(owner) = &mut self.bus.ram.storage else {
                return Ok(None);
            };
            owner.prepare_pc24_boundary(
                read,
                write,
                &boundary,
                SegmentRegister::ALL.map(|segment| self.cpu.registers.selector(segment)),
            );
        }
        let atomic = binding.binding.kernel.is_some()
            || binding.binding.rle_kernel.is_some()
            || binding.binding.finite_computation.is_some();
        let maximum_instructions = if binding.binding.rle_kernel.is_some() {
            let Some(work) =
                self.bus
                    .prepare_rle_call(&self.cpu, binding.function_index, &plan.source_pages)
            else {
                return Ok(None);
            };
            work
        } else if let Some(kernel) = binding.binding.kernel {
            let Some(work) = kernel.actual_work(self.cpu.registers.get32(Gpr32::Ecx)) else {
                return Ok(None);
            };
            work
        } else {
            binding.binding.maximum_instructions
        };
        let grant = if atomic {
            // This is one bounded computation under its procedure contract.
            // Device events are serviced afterward; IRQ entry waits for RET.
            budget.max(maximum_instructions)
        } else {
            self.timing
                .steps_to_event_nanos(&self.bus, budget.saturating_add(1), 1_000_000)
                .saturating_sub(1)
                .min(budget)
        };
        let irq_refused = maximum_instructions <= grant
            && atomic
            && binding.binding.kernel_interrupts == KernelInterrupts::BeforeNextIrq
            && !self.procedure_fits_before_irq_with_counts(
                maximum_instructions,
                tier.as_ref()
                    .map(|proof| proof.counts(maximum_instructions)),
            );
        let plan = self.wasm_procedures.as_mut().expect("prepared plan");
        let binding = &mut plan.bindings[index];
        if maximum_instructions > grant {
            plan.stats.budget_refusals += 1;
            return Ok(None);
        }
        if irq_refused {
            plan.stats.kernel_irq_refusals += 1;
            return Ok(None);
        }
        if !binding.source_matches(&self.bus) {
            plan.stats.source_refusals += 1;
            return Ok(None);
        }
        let entries = [MemoryProcedureEntry {
            entry_ip: binding.binding.entry_ip,
            function_id: binding.function_index,
            maximum_instructions,
            context,
        }];
        let registry = MemoryProcedureRegistry::new(&entries, FrozenCode::AssumeImmutable)
            .expect("validated prepared procedure");
        let rle_return = if plan.backend == Backend::Interpreter {
            self.bus
                .wasm_rle_call
                .take()
                .map(|call| call.admission.return_target())
        } else {
            None
        };
        let outcome = match if plan.backend == Backend::Interpreter {
            interpreted::execute(
                &mut self.cpu,
                &mut self.bus,
                binding.binding.kernel,
                rle_return,
                binding.binding.finite_computation.as_ref(),
                maximum_instructions,
            )
        } else {
            match tier {
                Some(TierProof::Uniform(tier)) => self.cpu.try_call_memory_procedure_in_tier(
                    &mut self.bus,
                    &registry,
                    grant,
                    tier,
                ),
                Some(TierProof::Complete(counts)) => {
                    self.cpu.try_call_memory_procedure_with_tier_counts(
                        &mut self.bus,
                        &registry,
                        grant,
                        counts,
                    )
                }
                None => self
                    .cpu
                    .try_call_memory_procedure(&mut self.bus, &registry, grant),
            }
        } {
            Ok(outcome) => outcome,
            Err(error) => {
                let failure = match error {
                    MemoryProcedureError::Backend(detail) => {
                        plan.last_error = Some(detail);
                        ExecutionError::Backend
                    }
                    MemoryProcedureError::InvalidResult(reason) => {
                        ExecutionError::InvalidResult(reason)
                    }
                };
                plan.failure = Some(failure);
                return Err(MachineError::WasmProcedure(failure));
            }
        };
        let (instructions, side_exit) = match outcome {
            MemoryProcedureOutcome::Declined(_) => return Ok(None),
            MemoryProcedureOutcome::Complete { instructions } => (instructions, false),
            MemoryProcedureOutcome::SideExit { instructions } => {
                plan.stats.side_exits += 1;
                (instructions, true)
            }
        };
        plan.stats.calls += 1;
        binding.execution.calls += 1;
        if plan.backend != Backend::Interpreter {
            binding.execution.translated_instructions += u64::from(instructions);
        }
        if side_exit {
            binding.execution.side_exits += 1;
        }
        if plan.backend == Backend::Interpreter {
            plan.stats.interpreted_calls += 1;
            plan.stats.interpreted_instructions += u64::from(instructions);
        } else {
            plan.stats.translated_instructions += u64::from(instructions);
            if let Some(proof) = tier.filter(|_| instructions != 0) {
                for (total, count) in plan
                    .stats
                    .translated_tiers
                    .iter_mut()
                    .zip(proof.counts(instructions))
                {
                    *total += count;
                }
            }
            plan.stats.device_reads += self.bus.wasm_device_effects.device_reads;
            plan.stats.device_writes += self.bus.wasm_device_effects.device_writes;
            plan.stats.device_changes += self.bus.wasm_device_effects.device_changes;
        }
        if atomic {
            plan.stats.kernel_calls += 1;
            plan.stats.kernel_instructions += u64::from(instructions);
        }
        if instructions == 0 {
            return Ok(Some(0));
        }
        if atomic {
            self.advance_procedure_hardware_steps(instructions)?;
        } else {
            self.advance_hardware_steps(instructions)?;
        }
        #[cfg(feature = "profiling")]
        {
            self.bus.stats.cpu_steps += u64::from(instructions);
        }
        Ok(Some(instructions))
    }
}

impl MemoryProcedureBus for PcBus {
    type Error = String;

    fn finish_memory_procedure(
        &mut self,
        fpu: Option<&mut MemoryProcedureFpu>,
    ) -> Result<(), String> {
        let Storage::Wasm(owner) = &mut self.ram.storage else {
            return Ok(());
        };
        if let Some((read, write, boundary)) = owner.pending_pc24_boundary() {
            fpu.ok_or("WASM procedure lost its x87 state")?
                .commit_pc24_boundary(read, write, boundary)
                .map_err(|error| alloc::format!("invalid WASM x87 result: {error:?}"))?;
            owner.clear_pc24_boundary();
        }
        Ok(())
    }

    fn execute_memory_procedure(
        &mut self,
        function_index: u32,
        cpu: &Cpu,
    ) -> Result<MemoryProcedureResult, String> {
        if !self.a20_enabled() || !self.ram.direct_execution_ready() {
            return Err("WASM procedure lost its direct RAM capability".into());
        }
        if let Some(call) = self.wasm_rle_call.take() {
            if call.function_index != function_index || call.frame != call_frame(cpu) {
                return Err("RLE entry changed after its computation proof".into());
            }
            let Storage::Wasm(owner) = &mut self.ram.storage else {
                return Err("WASM procedure RAM owner unavailable".into());
            };
            let mut frame = call.frame;
            let (exit, effects) = owner.execute_rle_index(
                function_index as usize,
                &mut frame,
                &call.descriptor,
                call.admission,
            )?;
            return self.finish_procedure(frame, exit, effects);
        }
        let windows = self.wasm_call_windows.resolve(&cpu.registers);
        let stack = if cpu.registers.stack32() {
            StackAddress::Bits32
        } else {
            StackAddress::Bits16
        };
        let mut frame = call_frame(cpu);
        // Capture only small controls. No borrowed slice survives execution of
        // the Store that owns both RAM and physical VGA memory.
        let mut device = self
            .vga
            .direct_read_view_with_memory(self.vram_view())
            .map(|view| {
                let mut device = DeviceReadDescriptor {
                    guest_start: doslet_vga::APERTURE_START,
                    guest_length: doslet_vga::APERTURE_LEN as u32,
                    backing_length: doslet_vga::VRAM_LEN as u32,
                    kind: DeviceReadKind::Linear,
                    base_offset: view.bank_offset,
                    latches: view.latches,
                    ..DeviceReadDescriptor::default()
                };
                if let Some(planar) = view.planar {
                    device.kind = DeviceReadKind::FourLanes;
                    device.chain4 = planar.chain4;
                    device.read_plane = planar.read_plane;
                    if let Some((compare, care)) = planar.compare {
                        device.compare_enabled = true;
                        device.compare = compare;
                        device.care = care;
                    }
                }
                device
            })
            .ok_or("WASM procedure lost its VGA memory capability")?;
        let write = self.vga.direct_write_controls();
        let Storage::Wasm(owner) = &mut self.ram.storage else {
            return Err("WASM procedure RAM owner unavailable".into());
        };
        device.pointer = owner.layout().video_pointer;
        let mut descriptor = owner.descriptor(windows, stack);
        descriptor.device = device;
        descriptor.device_write = DeviceWriteDescriptor {
            enabled: true,
            plane_mask: write.plane_mask,
            mode: write.mode,
            set_reset: write.set_reset,
            enable_set_reset: write.enable_set_reset,
            rotate: write.rotate,
            bit_mask: write.bit_mask,
        };
        descriptor.inline_grant = MAX_PROCEDURE_INSTRUCTIONS;
        let (exit, effects) =
            owner.execute_index(function_index as usize, &mut frame, &descriptor)?;
        self.finish_procedure(frame, exit, effects)
    }
}

#[cfg(test)]
mod tests;
