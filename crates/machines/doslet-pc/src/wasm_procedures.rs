//! Native prepared procedures over the machine's single RAM backing.
//!
//! Installation is a trusted compiler boundary. Sources are post-relocation
//! bytes; entry verifies them against the live bus and caches page generations.
//! Generated stores protect every compiled source page before their effects.

use alloc::{boxed::Box, string::String, vec::Vec};
use doslet_bus::{CodeCheck, MemoryBus};
use doslet_wasm_aot::procedure_memory::rle::{RleAdmission, RleMemory, prepare_rle};
use doslet_wasm_aot::procedure_memory::{Exit, SharedProcedureMemory};
use doslet_x86::procedure_calls::{
    FrozenCode, MAX_PROCEDURE_INSTRUCTIONS, ProcedureContext, ProcedureRegisters,
    memory::{
        MemoryProcedureBus, MemoryProcedureEntry, MemoryProcedureError, MemoryProcedureOutcome,
        MemoryProcedureRegistry, MemoryProcedureResult, MemoryProcedureResultError,
    },
};
use doslet_x86::{Access, Cpu, Gpr32, Registers, SegmentCache, SegmentRegister};
pub use doslet_x86_wasm::static_program::procedure_memory::CopyKernel;
pub use doslet_x86_wasm::static_program::procedure_memory::RleKernel;
pub use doslet_x86_wasm::static_program::procedure_memory::bundle;
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
    /// A compiler-proved, finite whole-procedure kernel. Its live arguments
    /// determine admission cost; IRQ delivery may wait until its return.
    pub kernel: Option<CopyKernel>,
    /// Complete finite RLE computation, with a live input-prefix proof.
    pub rle_kernel: Option<RleKernel>,
    /// Generic statically terminating CFG, independent of any semantic idiom.
    pub finite_computation: Option<FiniteComputation>,
    pub kernel_interrupts: KernelInterrupts,
    pub code: Vec<CodePart>,
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
    /// Effects reported by generated WASM; interpreter bus effects are already
    /// accounted by the ordinary PC bus and are not added here again.
    pub device_reads: u64,
    pub device_writes: u64,
    pub device_changes: u64,
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
    guards: Vec<SourceGuard>,
    checked: bool,
    matches: bool,
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
    bindings: Vec<PreparedBinding>,
    source_pages: Vec<u8>,
    stats: Stats,
    failure: Option<ExecutionError>,
    last_error: Option<String>,
    #[cfg(feature = "wasm-procedure-diagnostics")]
    diagnostic_mode: DiagnosticMode,
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

impl PcMachine {
    /// Compile and install one prepared module, then migrate RAM, VRAM and page
    /// generations once. Subsequent interpreter, device and WASM accesses
    /// borrow the same bytes. Validation failure leaves the machine unchanged.
    pub fn install_wasm_procedures(
        &mut self,
        wasm: &[u8],
        bindings: Vec<Binding>,
    ) -> Result<(), String> {
        self.install_procedures(Some(wasm), bindings)
    }

    /// Interpret admitted finite semantic procedures through RET before IRQ
    /// delivery. This reference policy retains the ordinary RAM/VRAM owners;
    /// no WASM is compiled. Other instructions keep ordinary IRQ boundaries.
    pub fn install_interpreted_procedure_boundaries(
        &mut self,
        bindings: Vec<Binding>,
    ) -> Result<(), String> {
        self.install_procedures(None, bindings)
    }

    fn install_procedures(
        &mut self,
        wasm: Option<&[u8]>,
        mut bindings: Vec<Binding>,
    ) -> Result<(), String> {
        if self.wasm_procedures.is_some() {
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
        let mut source_parts = 0usize;
        let mut source_bytes = 0usize;
        for binding in &mut bindings {
            if wasm.is_none()
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
            let mut previous_end = 0;
            let mut entry_covered = false;
            for part in &binding.code {
                source_bytes = source_bytes
                    .checked_add(part.expected.len())
                    .filter(|&count| count <= MAX_SOURCE_BYTES)
                    .ok_or("WASM procedure sources exceed 16 MiB")?;
                let length = u32::try_from(part.expected.len())
                    .map_err(|_| "WASM procedure source too large")?;
                let end = part
                    .linear
                    .checked_add(length)
                    .filter(|&end| length != 0 && end as usize <= self.bus.ram_size())
                    .ok_or("WASM procedure source outside RAM")?;
                if part.linear < 0x500 || (part.linear < 0xc0000 && end > 0xa0000) {
                    return Err("WASM procedure source outside ordinary RAM".into());
                }
                if part.linear < previous_end {
                    return Err("overlapping WASM procedure source parts".into());
                }
                previous_end = end;
                entry_covered |= (part.linear..end).contains(&binding.entry_ip);
            }
            if !entry_covered {
                return Err("WASM procedure entry has no source proof".into());
            }
            if let Some(finite) = &binding.finite_computation {
                for terminal in finite.exits.iter().chain(&finite.returns) {
                    if !binding.code.iter().any(|part| part.linear == *terminal) {
                        return Err("finite computation terminal has no instruction source".into());
                    }
                }
                for guard in &finite.guards {
                    if !binding
                        .code
                        .iter()
                        .any(|part| part.linear == guard.entry_ip)
                    {
                        return Err("finite loop guard has no instruction source".into());
                    }
                }
            }
        }
        let mut owner = wasm
            .map(|wasm| SharedProcedureMemory::new(wasm, &ids, self.bus.ram_size()))
            .transpose()?;
        let mut prepared = Vec::new();
        let mut source_pages = alloc::vec![0u8; self.bus.ram_size().div_ceil(PAGE_BYTES)];
        let mut entry_ips = Vec::new();
        prepared
            .try_reserve_exact(bindings.len())
            .map_err(|_| "WASM binding allocation")?;
        entry_ips
            .try_reserve_exact(bindings.len())
            .map_err(|_| "WASM entry allocation")?;
        for binding in bindings {
            let function_index = ids
                .binary_search(&binding.function_id)
                .expect("validated procedure ID") as u32;
            let mut guards = Vec::new();
            let count = binding
                .code
                .iter()
                .map(|part| {
                    (part.linear as usize % PAGE_BYTES + part.expected.len()).div_ceil(PAGE_BYTES)
                })
                .sum();
            guards
                .try_reserve_exact(count)
                .map_err(|_| "WASM source guard allocation")?;
            for part in &binding.code {
                let first = part.linear as usize / PAGE_BYTES;
                let last = (part.linear as usize + part.expected.len() - 1) / PAGE_BYTES;
                source_pages[first..=last].fill(1);
                if let Some(owner) = &mut owner {
                    owner.protect_source(part.linear, part.expected.len() as u32)?;
                }
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
            entry_ips.push(binding.entry_ip);
            prepared.push(PreparedBinding {
                binding,
                function_index,
                guards,
                checked: false,
                matches: false,
            });
        }
        // Publish hints only after every fallible preparation step. They never
        // authorize execution: context, budget and source proofs stay below.
        self.cpu
            .set_prepared_entries(entry_ips)
            .map_err(String::from)?;
        if let Some(mut owner) = owner {
            // Setup-only copies. The old allocation is dropped by replace_storage.
            owner
                .ram_mut()
                .copy_from_slice(self.bus.ram.storage.bytes());
            owner
                .generations_mut()
                .copy_from_slice(self.bus.ram.storage.generations());
            self.bus.vga.transfer_memory_into(owner.video_mut());
            self.bus.ram.replace_storage(Storage::Wasm(Box::new(owner)));
        }
        self.cpu.set_dynamic_core(false);
        self.wasm_procedures = Some(Plan {
            backend: if wasm.is_some() {
                Backend::Wasm
            } else {
                Backend::Interpreter
            },
            bindings: prepared,
            source_pages,
            stats: Stats::default(),
            failure: None,
            last_error: None,
            #[cfg(feature = "wasm-procedure-diagnostics")]
            diagnostic_mode: DiagnosticMode::Normal,
        });
        Ok(())
    }

    pub fn wasm_procedure_stats(&self) -> Option<Stats> {
        self.wasm_procedures.as_ref().map(|plan| plan.stats)
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
        let entries = if mode == DiagnosticMode::Normal {
            plan.bindings
                .iter()
                .map(|binding| binding.binding.entry_ip)
                .collect()
        } else {
            Vec::new()
        };
        self.cpu.set_prepared_entries(entries)?;
        self.wasm_procedures
            .as_mut()
            .expect("installed diagnostic plan")
            .diagnostic_mode = mode;
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
        let Some(plan) = &self.wasm_procedures else {
            return Ok(None);
        };
        // Membership is cached independently of code bytes. This hint avoids
        // searching the registry for ordinary interpreter batches; the source
        // and architectural proofs still authorize every foreign invocation.
        let Some(index) = self.cpu.prepared_entry_index() else {
            return Ok(None);
        };
        let Some(binding) = plan
            .bindings
            .get(index)
            .filter(|binding| binding.binding.entry_ip == self.cpu.registers.ip)
        else {
            return Ok(None);
        };
        if self.vga_wait.candidate.is_some()
            || self.memory_wait_sites.is_some()
            || self.batch_yield_ip.is_some()
            || !self.bus.a20_enabled()
            || !self.bus.ram.direct_execution_ready()
            || (self.cpu.accepts_maskable_interrupt() && self.bus.pic.has_interrupt())
            || self.cpu.execution_labels_enabled()
        {
            return Ok(None);
        }
        let Some(context) = ProcedureContext::capture(&self.cpu) else {
            return Ok(None);
        };
        self.bus.wasm_rle_call = None;
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
            && !self.procedure_fits_before_irq(maximum_instructions);
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
            self.cpu
                .try_call_memory_procedure(&mut self.bus, &registry, grant)
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
        let instructions = match outcome {
            MemoryProcedureOutcome::Declined(_) => return Ok(None),
            MemoryProcedureOutcome::Complete { instructions } => instructions,
            MemoryProcedureOutcome::SideExit { instructions } => {
                plan.stats.side_exits += 1;
                instructions
            }
        };
        plan.stats.calls += 1;
        if plan.backend == Backend::Interpreter {
            plan.stats.interpreted_calls += 1;
            plan.stats.interpreted_instructions += u64::from(instructions);
        } else {
            plan.stats.translated_instructions += u64::from(instructions);
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
