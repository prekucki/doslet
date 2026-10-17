//! Bounded native block cache. Discovery/execution allocate nothing. Compilation
//! is an explicit maintenance operation, outside emulation and audio callbacks.
use super::{Cpu, cache};
use crate::{Access, SegmentCache, SegmentRegister, flags};
use crate::{DynamicExitReason, DynamicExitRecord, DynamicStats};
use alloc::{collections::BTreeMap, string::String, sync::Arc, vec::Vec};
use doslet_bus::{CodeCheck, CpuBus, MemoryBus};
use doslet_x86_jit::{
    CompiledBlock, CompiledTrace, Compiler, Instruction, MAX_COMPILED_BLOCKS, MAX_INSTRUCTIONS,
    Operation, TraceInstruction, TraceOperation, TraceSource,
};

const CAPACITY: usize = 1024;
const MAX_BYTES: usize = 15 * MAX_INSTRUCTIONS;
const HOT_COUNT: u16 = 16;
const EXIT_CAPACITY: usize = 8192;
const CODE_BANKS: usize = 16;
const FUNCTIONS_PER_BANK: usize = MAX_COMPILED_BLOCKS / CODE_BANKS;

fn exit_reason(reason: doslet_x86_jit::TraceExitReason) -> DynamicExitReason {
    use doslet_x86_jit::TraceExitReason as R;
    match reason {
        R::Budget => DynamicExitReason::Budget,
        R::ExternalEdge => DynamicExitReason::GraphLimit,
        R::LoadGuard => DynamicExitReason::LoadGuard,
        R::StoreBounds => DynamicExitReason::StoreBounds,
        R::ActiveCode => DynamicExitReason::ActiveCode,
        R::GenerationBounds => DynamicExitReason::GenerationBounds,
        R::GenerationExhausted => DynamicExitReason::GenerationExhausted,
        R::Uninstrumented => DynamicExitReason::RegisterEnd,
    }
}

fn record_exit(records: &mut [DynamicExitRecord], dropped: &mut u64, mut event: DynamicExitRecord) {
    if records.is_empty() {
        return;
    }
    let hash = u64::from(event.entry_ip).wrapping_mul(0x9e3779b185ebca87)
        ^ u64::from(event.exit_ip).rotate_left(21)
        ^ (u64::from(event.cs) << 32)
        ^ u64::from(event.entry_linear).rotate_left(43)
        ^ u64::from(event.exit_linear).rotate_left(11)
        ^ event.reason as u64;
    let start = hash as usize & (EXIT_CAPACITY - 1);
    for offset in 0..EXIT_CAPACITY {
        let slot = &mut records[(start + offset) & (EXIT_CAPACITY - 1)];
        if slot.count == 0 {
            event.count = 1;
            *slot = event;
            return;
        }
        if slot.cs == event.cs
            && slot.entry_ip == event.entry_ip
            && slot.exit_ip == event.exit_ip
            && slot.reason == event.reason
            && slot.entry_linear == event.entry_linear
            && slot.exit_linear == event.exit_linear
        {
            slot.count = slot.count.saturating_add(1);
            slot.steps = slot.steps.saturating_add(event.steps);
            return;
        }
    }
    *dropped = dropped.saturating_add(1);
}

enum NativeBus<'a, B> {
    Read(&'a B),
    Write(&'a mut B),
}
impl<B> NativeBus<'_, B> {
    fn as_ref(&self) -> &B {
        match self {
            Self::Read(bus) => bus,
            Self::Write(bus) => bus,
        }
    }
}

#[derive(Clone)]
struct PreparedTrace {
    instructions: [TraceInstruction; MAX_INSTRUCTIONS],
    count: u8,
    length: u8,
    segments: u8,
    write_segments: u8,
    uses_stack: bool,
    dynamic_control: bool,
}
impl PreparedTrace {
    fn decode(bytes: &[u8]) -> Option<Self> {
        let bytes = &bytes[..bytes.len().min(MAX_BYTES)];
        let mut trace = Self {
            instructions: [TraceInstruction {
                offset: 0,
                length: 1,
                operation: TraceOperation::Move {
                    target: 0,
                    source: TraceSource::Register(0),
                },
            }; MAX_INSTRUCTIONS],
            count: 0,
            length: 0,
            segments: 0,
            write_segments: 0,
            uses_stack: false,
            dynamic_control: false,
        };
        // Fixed discovery storage: 0=unseen, 1=start, 2=interior, 3=unsupported.
        let mut status = [0_u8; MAX_BYTES];
        let mut pending = [false; MAX_BYTES];
        pending[0] = true;
        while usize::from(trace.count) < MAX_INSTRUCTIONS {
            let Some(offset) = (0..bytes.len()).find(|&i| pending[i] && status[i] == 0) else {
                break;
            };
            pending[offset] = false;
            let Some((instruction, segments, write_segments)) = cache::native_trace_instruction(
                &bytes[offset..bytes.len().min(offset + 15)],
                offset as u8,
            ) else {
                status[offset] = 3;
                continue;
            };
            let end = offset + usize::from(instruction.length);
            if status[offset..end].iter().any(|&value| value != 0) {
                return None;
            }
            status[offset] = 1;
            status[offset + 1..end].fill(2);
            if pending[offset + 1..end].iter().any(|&value| value) {
                return None;
            }
            trace.instructions[usize::from(trace.count)] = instruction;
            trace.count += 1;
            trace.length = trace.length.max(end as u8);
            trace.segments |= segments;
            trace.write_segments |= write_segments;
            trace.uses_stack |= matches!(
                instruction.operation,
                TraceOperation::Push { .. }
                    | TraceOperation::Pop { .. }
                    | TraceOperation::Call { .. }
                    | TraceOperation::CallIndirect { .. }
                    | TraceOperation::Return { .. }
            );
            trace.dynamic_control |= matches!(
                instruction.operation,
                TraceOperation::Call { .. }
                    | TraceOperation::CallIndirect { .. }
                    | TraceOperation::Return { .. }
            );
            let mut enqueue = |target: u32| -> Option<()> {
                let target = target as usize;
                if target < bytes.len() {
                    if status[target] == 2 {
                        return None;
                    }
                    if status[target] == 0 {
                        pending[target] = true;
                    }
                }
                Some(())
            };
            match instruction.operation {
                TraceOperation::Branch { condition, target } => {
                    enqueue(target)?;
                    if condition.is_some() {
                        enqueue(end as u32)?;
                    }
                }
                TraceOperation::Call { target } => {
                    enqueue(target)?;
                    enqueue(end as u32)?;
                }
                TraceOperation::Return { .. } => {}
                _ => enqueue(end as u32)?,
            }
        }
        if trace.count < 2 {
            return None;
        }
        trace.instructions[..usize::from(trace.count)].sort_unstable_by_key(|i| i.offset);
        Some(trace)
    }

    fn entry_valid(&self, registers: &crate::Registers) -> bool {
        if self.uses_stack && !registers.stack32() {
            return false;
        }
        if self.dynamic_control {
            let cs = registers.segment_cache(SegmentRegister::Cs);
            if cs.base != 0
                || cs.limit != u32::MAX
                || registers.address(SegmentRegister::Cs, 0, 1, Access::Execute) != Ok(0)
                || registers.address(SegmentRegister::Cs, u32::MAX, 1, Access::Execute)
                    != Ok(u32::MAX)
            {
                return false;
            }
        }
        for segment in SegmentRegister::ALL {
            let bit = 1 << segment as u8;
            if (self.segments | self.write_segments) & bit != 0 {
                let cache = registers.segment_cache(segment);
                if cache.base != 0
                    || cache.limit != u32::MAX
                    || registers.address(segment, 0, 4, Access::Read) != Ok(0)
                    || registers.address(segment, u32::MAX - 3, 4, Access::Read) != Ok(u32::MAX - 3)
                {
                    return false;
                }
                if self.write_segments & bit != 0
                    && (registers.address(segment, 0, 4, Access::Write) != Ok(0)
                        || registers.address(segment, u32::MAX - 3, 4, Access::Write)
                            != Ok(u32::MAX - 3))
                {
                    return false;
                }
            }
        }
        // Missing successors are interpreter exits. Conservatively validate
        // even untaken targets; an actual guest fault remains interpreter work.
        let valid_exit = |offset: u32| {
            self.instructions[..usize::from(self.count)]
                .iter()
                .any(|i| u32::from(i.offset) == offset)
                || registers
                    .address(
                        SegmentRegister::Cs,
                        registers.ip.wrapping_add(offset),
                        1,
                        Access::Execute,
                    )
                    .is_ok()
        };
        self.instructions[..usize::from(self.count)]
            .iter()
            .all(|i| {
                let next = u32::from(i.offset) + u32::from(i.length);
                match i.operation {
                    TraceOperation::Branch { condition, target } => {
                        valid_exit(target) && (condition.is_none() || valid_exit(next))
                    }
                    TraceOperation::Call { target } => valid_exit(target) && valid_exit(next),
                    TraceOperation::Return { .. } => true,
                    _ => valid_exit(next),
                }
            })
    }
}

/// A positive segment-safety result, scoped to one execution-cache key (CS,
/// entry IP, privilege and mode) and one prepared trace. Guest GPRs may change
/// freely; changed segment selectors or hidden descriptors require revalidation.
struct ValidatedSegments {
    caches: [SegmentCache; 6],
    selectors: [u16; 6],
}
impl ValidatedSegments {
    fn capture(registers: &crate::Registers) -> Self {
        Self {
            caches: SegmentRegister::ALL.map(|segment| registers.segment_cache(segment)),
            selectors: SegmentRegister::ALL.map(|segment| registers.selector(segment)),
        }
    }
    fn matches(&self, registers: &crate::Registers, used: u8) -> bool {
        SegmentRegister::ALL.into_iter().all(|segment| {
            let index = segment as usize;
            used & (1 << index) == 0
                || (self.selectors[index] == registers.selector(segment)
                    && self.caches[index] == registers.segment_cache(segment))
        })
    }
}

#[derive(Clone, Copy)]
struct NativeBlock {
    bank: usize,
    function: NativeFunction,
}
#[derive(Clone, Copy)]
enum NativeFunction {
    Registers(CompiledBlock),
    Trace {
        code: CompiledTrace,
        register_fallback: Option<CompiledBlock>,
    },
}

/// Address-independent prepared IR plus the exact bytes it represents. It can
/// only be bound to RAM after byte, CS and generation validation.
#[derive(Clone)]
struct PreparedBlock {
    instructions: [Instruction; MAX_INSTRUCTIONS],
    bytes: [u8; MAX_BYTES],
    count: u8,
    register_count: u8,
    length: u8,
    trace: Option<PreparedTrace>,
    labelled: bool,
    register_tiers: [u8; MAX_INSTRUCTIONS],
    trace_tiers: [u8; MAX_INSTRUCTIONS],
    register_counts: [u64; 4],
}
impl PreparedBlock {
    fn fingerprint(&self) -> u64 {
        self.bytes[..usize::from(self.length)]
            .iter()
            .chain(self.register_tiers.iter())
            .chain(self.trace_tiers.iter())
            .chain(core::iter::once(&(self.labelled as u8)))
            .fold(0xcbf29ce484222325, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            })
    }
    fn same_code(&self, other: &Self) -> bool {
        self.labelled == other.labelled
            && self.register_tiers == other.register_tiers
            && self.trace_tiers == other.trace_tiers
            && self.length == other.length
            && self.bytes[..usize::from(self.length)] == other.bytes[..usize::from(other.length)]
    }
    fn label(&mut self, profile: &crate::execution_tiers::ExecutionProfile, address: u32) {
        self.labelled = true;
        let mut offset = 0;
        for i in 0..usize::from(self.register_count) {
            let end = offset + usize::from(self.instructions[i].length);
            let tier = profile.classify(
                address.wrapping_add(offset as u32),
                true,
                &self.bytes[offset..end],
            );
            self.register_tiers[i] = tier;
            self.register_counts[usize::from(tier)] += 1;
            offset = end;
        }
        if let Some(trace) = &self.trace {
            for i in 0..usize::from(trace.count) {
                let instruction = trace.instructions[i];
                let offset = usize::from(instruction.offset);
                self.trace_tiers[i] = profile.classify(
                    address.wrapping_add(offset as u32),
                    true,
                    &self.bytes[offset..offset + usize::from(instruction.length)],
                );
            }
        }
    }
    fn count_register_steps(&self, counts: &mut [u64; 4], steps: u32) {
        if !self.labelled || self.register_count == 0 {
            return;
        }
        let passes = u64::from(steps / u32::from(self.register_count));
        for (total, per_pass) in counts.iter_mut().zip(self.register_counts) {
            *total += per_pass * passes;
        }
        for tier in &self.register_tiers[..(steps % u32::from(self.register_count)) as usize] {
            counts[usize::from(*tier)] += 1;
        }
    }
    fn decode(bytes: &[u8]) -> Self {
        let mut block = Self {
            instructions: [Instruction {
                length: 1,
                operation: Operation::Exchange { left: 0, right: 0 },
            }; MAX_INSTRUCTIONS],
            bytes: [0; MAX_BYTES],
            count: 0,
            register_count: 0,
            length: 0,
            trace: None,
            labelled: false,
            register_tiers: [2; MAX_INSTRUCTIONS],
            trace_tiers: [2; MAX_INSTRUCTIONS],
            register_counts: [0; 4],
        };
        while usize::from(block.count) < MAX_INSTRUCTIONS {
            let offset = usize::from(block.length);
            let Some(i) = cache::native_instruction(
                &bytes[offset..bytes.len().min(offset + 15)],
                block.length,
            ) else {
                break;
            };
            block.instructions[usize::from(block.count)] = i;
            block.count += 1;
            block.length += i.length;
            if matches!(i.operation, Operation::LoopBranch { .. }) {
                break;
            }
        }
        block.register_count = block.count;
        if usize::from(block.count) < MAX_INSTRUCTIONS
            && !block.instructions[..usize::from(block.count)]
                .last()
                .is_some_and(|i| matches!(i.operation, Operation::LoopBranch { .. }))
            && let Some(trace) = PreparedTrace::decode(bytes)
            && trace.length > block.length
        {
            block.count = trace.count;
            block.length = trace.length;
            block.trace = Some(trace);
        }
        let length = usize::from(block.length);
        block.bytes[..length].copy_from_slice(&bytes[..length]);
        block
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    cs: SegmentCache,
    selector: u16,
    ip: u32,
    protected: bool,
    cpl: u8,
}
#[derive(Default)]
struct Entry {
    key: Option<Key>,
    check: Option<CodeCheck>,
    prepared: Option<PreparedBlock>,
    validated_segments: Option<ValidatedSegments>,
    compiled: Option<NativeBlock>,
    hits: u16,
    attempted: bool,
}
struct Template {
    block: PreparedBlock,
    compiled: Option<NativeBlock>,
    attempted: bool,
}
struct NativeCode {
    block: PreparedBlock,
    code: NativeBlock,
}
// Compiler ownership determines which executable-memory bank may be reclaimed.
fn compile_native(
    compiler: &mut Compiler,
    block: &PreparedBlock,
    exit_profiling: bool,
) -> Result<NativeFunction, String> {
    let code = if let Some(trace) = &block.trace {
        let instructions = &trace.instructions[..usize::from(trace.count)];
        let code = if block.labelled {
            compiler.compile_trace_tiered(
                instructions,
                &block.trace_tiers[..usize::from(trace.count)],
            )?
        } else if exit_profiling {
            compiler.compile_trace_profiled(instructions)?
        } else {
            compiler.compile_trace(instructions)?
        };
        let register_fallback = if block.register_count >= 2 {
            Some(compiler.compile(&block.instructions[..usize::from(block.register_count)])?)
        } else {
            None
        };
        NativeFunction::Trace {
            code,
            register_fallback,
        }
    } else {
        NativeFunction::Registers(
            compiler.compile(&block.instructions[..usize::from(block.count)])?,
        )
    };
    Ok(code)
}
/// An exclusively owned compiler bank and bounded, address-independent IR.
/// The host may move this to a compiler thread. It contains no guest RAM/state.
/// Return it to the originating CPU with `finish_dynamic_compilation`.
pub struct DynamicCompilation {
    identity: Arc<()>,
    bank: usize,
    compiler: Option<Compiler>,
    retired: Option<Compiler>,
    capacity: usize,
    blocks: Vec<PreparedBlock>,
    functions: Vec<NativeFunction>,
    profiling: bool,
    error: Option<String>,
}
impl DynamicCompilation {
    /// All Cranelift work, including executable-memory allocation, occurs here.
    pub fn compile(mut self) -> Self {
        drop(self.retired.take());
        let result = (|| {
            if self.compiler.is_none() {
                self.compiler = Some(Compiler::with_capacity(self.capacity)?);
            }
            let compiler = self.compiler.as_mut().unwrap();
            for block in &self.blocks {
                self.functions
                    .push(compile_native(compiler, block, self.profiling)?);
            }
            Ok::<_, String>(())
        })();
        self.error = result.err();
        self
    }
}

impl Cpu {
    /// Detach a bank for compilation without waiting. While it is away, its
    /// existing handles fall back to interpretation. At most one job is in flight.
    pub fn take_dynamic_compilation(&mut self, limit: usize) -> Option<DynamicCompilation> {
        let cache = &mut self.dynamic;
        if !cache.enabled || cache.failure.is_some() || cache.compiling || limit == 0 {
            return None;
        }
        let limit = limit.min(8);
        let mut blocks: Vec<PreparedBlock> = Vec::new();
        for template in &mut cache.templates {
            if blocks.len() == limit {
                break;
            }
            if template.attempted {
                continue;
            }
            if let Some(native) = cache
                .pool
                .get(&template.block.fingerprint())
                .filter(|n| n.block.same_code(&template.block))
            {
                template.compiled = Some(native.code);
            } else if !blocks.iter().any(|b| b.same_code(&template.block)) {
                blocks.push(template.block.clone());
            }
            template.attempted = true;
        }
        for _ in 0..CAPACITY {
            if blocks.len() == limit {
                break;
            }
            let index = cache.cursor;
            cache.cursor = (cache.cursor + 1) % CAPACITY;
            let entry = &mut cache.entries[index];
            if entry.attempted || entry.hits < HOT_COUNT {
                continue;
            }
            let Some(block) = &entry.prepared else {
                continue;
            };
            if block.count < 2 {
                continue;
            }
            if let Some(native) = cache
                .pool
                .get(&block.fingerprint())
                .filter(|n| n.block.same_code(block))
            {
                entry.compiled = Some(native.code);
            } else if !blocks.iter().any(|b| b.same_code(block)) {
                blocks.push(block.clone());
            }
            entry.attempted = true;
        }
        if blocks.is_empty() {
            return None;
        }
        let required: usize = blocks
            .iter()
            .map(|b| 1 + usize::from(b.trace.is_some() && b.register_count >= 2))
            .sum();
        let bank = if let Some(index) = cache.banks.iter().position(|b| {
            b.compiler
                .as_ref()
                .is_some_and(|c| c.remaining_capacity() >= required)
        }) {
            index
        } else if cache.banks.len() < cache.bank_limit {
            cache.banks.push(CodeBank {
                compiler: None,
                last_used: 0,
            });
            cache.banks.len() - 1
        } else {
            let index = cache
                .banks
                .iter()
                .enumerate()
                .min_by_key(|(_, b)| b.last_used)
                .unwrap()
                .0;
            for entry in &mut cache.entries {
                if entry.compiled.is_some_and(|c| c.bank == index) {
                    entry.compiled = None;
                    entry.attempted = false;
                    entry.hits = 0;
                }
            }
            for template in &mut cache.templates {
                if template.compiled.is_some_and(|c| c.bank == index) {
                    template.compiled = None;
                }
            }
            cache.pool.retain(|_, n| n.code.bank != index);
            // Reclamation is transferred to the compiler worker too.
            index
        };
        let mut compiler = cache.banks[bank].compiler.take();
        let retired = if compiler
            .as_ref()
            .is_some_and(|c| c.remaining_capacity() < required)
        {
            cache.stats.code_cache_evictions += 1;
            compiler.take()
        } else {
            None
        };
        cache.compiling = true;
        Some(DynamicCompilation {
            identity: Arc::clone(&cache.identity),
            bank,
            compiler,
            retired,
            capacity: cache.bank_function_limit,
            blocks,
            functions: Vec::with_capacity(limit),
            profiling: !cache.exit_records.is_empty(),
            error: None,
        })
    }

    /// Install completed code at a host boundary. Results from a reset/replaced
    /// CPU are rejected. Entries still validate actual code bytes on execution.
    pub fn finish_dynamic_compilation(&mut self, mut job: DynamicCompilation) -> usize {
        let cache = &mut self.dynamic;
        if !Arc::ptr_eq(&job.identity, &cache.identity) {
            return 0;
        }
        cache.compiling = false;
        cache.banks[job.bank].compiler = job.compiler.take();
        cache.touch_bank(job.bank);
        let completed = job.functions.len();
        for (block, function) in job.blocks.into_iter().zip(job.functions) {
            let code = NativeBlock {
                bank: job.bank,
                function,
            };
            for entry in &mut cache.entries {
                if entry.prepared.as_ref().is_some_and(|b| b.same_code(&block)) {
                    entry.compiled = Some(code);
                    entry.attempted = true;
                }
            }
            for template in &mut cache.templates {
                if template.block.same_code(&block) {
                    template.compiled = Some(code);
                }
            }
            cache
                .pool
                .insert(block.fingerprint(), NativeCode { block, code });
        }
        cache.stats.compiled_blocks += completed as u64;
        if let Some(error) = job.error {
            cache.failure = Some(error);
            cache.stats.compilation_failures += 1;
        }
        completed
    }
}

struct CodeBank {
    compiler: Option<Compiler>,
    last_used: u64,
}
pub(super) struct Cache {
    identity: Arc<()>,
    compiling: bool,
    entries: Vec<Entry>,
    banks: Vec<CodeBank>,
    bank_limit: usize,
    bank_function_limit: usize,
    use_clock: u64,
    enabled: bool,
    failure: Option<String>,
    stats: DynamicStats,
    cursor: usize,
    templates: Vec<Template>,
    pool: BTreeMap<u64, NativeCode>,
    exit_records: Vec<DynamicExitRecord>,
    exit_dropped: u64,
}
impl Default for Cache {
    fn default() -> Self {
        Self {
            identity: Arc::new(()),
            compiling: false,
            entries: (0..CAPACITY).map(|_| Entry::default()).collect(),
            banks: Vec::with_capacity(CODE_BANKS),
            bank_limit: CODE_BANKS,
            bank_function_limit: FUNCTIONS_PER_BANK,
            use_clock: 0,
            enabled: true,
            failure: None,
            stats: DynamicStats::default(),
            cursor: 0,
            templates: Vec::new(),
            pool: BTreeMap::new(),
            exit_records: Vec::new(),
            exit_dropped: 0,
        }
    }
}
impl Cache {
    fn touch_bank(&mut self, bank: usize) {
        self.use_clock = self.use_clock.saturating_add(1);
        self.banks[bank].last_used = self.use_clock;
    }

    fn compile_unique(&mut self, block: &PreparedBlock) -> Result<(NativeBlock, bool), String> {
        let hash = block.fingerprint();
        if let Some(native) = self.pool.get(&hash).filter(|n| n.block.same_code(block)) {
            let code = native.code;
            self.touch_bank(code.bank);
            return Ok((code, false));
        }
        // A trace and its register fallback must fit in the same bank. Check
        // both slots before emitting either function, including the final slot.
        let required = 1 + usize::from(block.trace.is_some() && block.register_count >= 2);
        let bank = if let Some(index) = self.banks.iter().position(|bank| {
            bank.compiler
                .as_ref()
                .map_or(0, Compiler::remaining_capacity)
                >= required
        }) {
            index
        } else {
            // Construct first: a setup failure leaves existing code usable.
            let fresh = CodeBank {
                compiler: Some(Compiler::with_capacity(self.bank_function_limit)?),
                last_used: 0,
            };
            if self.banks.len() < self.bank_limit {
                self.banks.push(fresh);
                self.banks.len() - 1
            } else {
                let index = self
                    .banks
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, bank)| bank.last_used)
                    .unwrap()
                    .0;
                // No generated call is active during host maintenance. Remove
                // every handle before dropping executable storage or reusing IDs.
                for entry in &mut self.entries {
                    if entry.compiled.is_some_and(|code| code.bank == index) {
                        entry.compiled = None;
                        entry.attempted = false;
                        // Eviction must not queue cold historical entries for
                        // immediate recompilation without fresh execution.
                        entry.hits = 0;
                    }
                }
                for template in &mut self.templates {
                    if template.compiled.is_some_and(|code| code.bank == index) {
                        template.compiled = None;
                    }
                }
                self.pool.retain(|_, native| native.code.bank != index);
                self.banks[index] = fresh;
                self.stats.code_cache_evictions += 1;
                index
            }
        };
        let function = compile_native(
            self.banks[bank]
                .compiler
                .as_mut()
                .expect("synchronous bank"),
            block,
            !self.exit_records.is_empty(),
        )?;
        let code = NativeBlock { bank, function };
        self.touch_bank(bank);
        self.pool.insert(
            hash,
            NativeCode {
                block: block.clone(),
                code,
            },
        );
        Ok((code, true))
    }
}
impl core::fmt::Debug for Cache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DynamicCore")
            .field("enabled", &self.enabled)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}
impl Cpu {
    /// Allocate bounded diagnostics before compiling any native code. Changing
    /// the mode after preparation is rejected rather than mixing compiler ABIs.
    pub fn set_dynamic_exit_profiling(&mut self, enabled: bool) -> Result<(), &'static str> {
        if enabled != self.dynamic.exit_records.is_empty() {
            return Ok(());
        }
        if !self.dynamic.banks.is_empty() {
            return Err("set exit profiling before native preparation");
        }
        self.dynamic.exit_records = if enabled {
            alloc::vec![DynamicExitRecord::default(); EXIT_CAPACITY]
        } else {
            Vec::new()
        };
        self.dynamic.exit_dropped = 0;
        Ok(())
    }
    pub fn dynamic_exit_profile(&self) -> &[DynamicExitRecord] {
        &self.dynamic.exit_records
    }
    pub fn dynamic_exit_profile_dropped(&self) -> u64 {
        self.dynamic.exit_dropped
    }
    pub fn set_dynamic_core(&mut self, enabled: bool) {
        self.dynamic.enabled = enabled;
    }
    pub fn dynamic_core_enabled(&self) -> bool {
        self.dynamic.enabled
    }
    pub fn dynamic_stats(&self) -> DynamicStats {
        self.dynamic.stats
    }
    pub fn dynamic_error(&self) -> Option<&str> {
        self.dynamic.failure.as_deref()
    }

    /// Import bounded observed code, never native machine code or trusted memory
    /// mappings. Address metadata is nonbinding; all current decoded bytes must
    /// match a prepared pool entry before reuse.
    /// Decode every record before accepting the profile. This setup operation allocates.
    pub fn load_dynamic_profile(&mut self, bytes: &[u8]) -> Result<(), &'static str> {
        if bytes.len() < 6
            || bytes.len() > crate::DYNAMIC_PROFILE_MAX_BYTES
            || &bytes[..4] != b"DSJ1"
        {
            return Err("invalid dynamic core profile header or size");
        }
        let count = usize::from(u16::from_le_bytes([bytes[4], bytes[5]]));
        if count > 256 {
            return Err("too many dynamic profile blocks");
        }
        let mut templates = Vec::with_capacity(count);
        let mut cursor = 6;
        for _ in 0..count {
            let header = bytes
                .get(cursor..cursor + 5)
                .ok_or("truncated dynamic profile record")?;
            // Retain the on-disk address hint for compatibility, but bind by
            // the complete current decoded bytes rather than a short prefix.
            let _address_hint = u32::from_le_bytes(header[..4].try_into().unwrap());
            let length = usize::from(header[4]);
            if length > MAX_BYTES {
                return Err("invalid dynamic profile code length");
            }
            cursor += 5;
            let code = bytes
                .get(cursor..cursor + length)
                .ok_or("truncated dynamic profile code")?;
            let block = PreparedBlock::decode(code);
            if block.count < 2 || usize::from(block.length) != length {
                return Err("unsupported dynamic profile code");
            }
            templates.push(Template {
                block,
                compiled: None,
                attempted: false,
            });
            cursor += length;
        }
        if cursor != bytes.len() {
            return Err("trailing dynamic profile data");
        }
        self.dynamic.templates = templates;
        Ok(())
    }

    /// Export observed hot code into caller-owned storage. No native pointers,
    /// host paths, RAM snapshots or executable permissions are serialized.
    pub fn write_dynamic_profile(&self, output: &mut [u8]) -> usize {
        if output.len() < crate::DYNAMIC_PROFILE_MAX_BYTES {
            return 0;
        }
        output[..4].copy_from_slice(b"DSJ1");
        let mut cursor = 6;
        let mut count = 0_u16;
        for entry in &self.dynamic.entries {
            let (Some(key), Some(block)) = (entry.key, &entry.prepared) else {
                continue;
            };
            if entry.hits < HOT_COUNT || block.count < 2 {
                continue;
            }
            let length = usize::from(block.length);
            output[cursor..cursor + 4]
                .copy_from_slice(&key.cs.base.wrapping_add(key.ip).to_le_bytes());
            output[cursor + 4] = block.length;
            cursor += 5;
            output[cursor..cursor + length].copy_from_slice(&block.bytes[..length]);
            cursor += length;
            count += 1;
            if count == 256 {
                break;
            }
        }
        output[4..6].copy_from_slice(&count.to_le_bytes());
        cursor
    }

    /// Host maintenance: compile at most `limit.min(8)` hot blocks. May allocate
    /// and request executable memory. Full code banks are reclaimed by LRU.
    /// Compiler failure latches once; interpretation keeps
    /// working, and already compiled code remains available.
    pub fn prepare_dynamic(&mut self, limit: usize) -> usize {
        let cache = &mut self.dynamic;
        if !cache.enabled || cache.failure.is_some() || limit == 0 || cache.compiling {
            return 0;
        }
        let mut completed = 0;
        // Previously observed blocks are prepared before their first execution.
        for index in 0..cache.templates.len() {
            if cache.templates[index].attempted {
                continue;
            }
            let block = cache.templates[index].block.clone();
            let result = cache.compile_unique(&block);
            cache.templates[index].attempted = true;
            match result {
                Ok((code, compiled)) => {
                    cache.templates[index].compiled = Some(code);
                    cache.stats.compiled_blocks += u64::from(compiled);
                    completed += 1;
                }
                Err(error) => {
                    cache.failure = Some(error);
                    cache.stats.compilation_failures += 1;
                    return completed;
                }
            }
            if completed >= limit.min(8) {
                return completed;
            }
        }
        for _ in 0..CAPACITY {
            let index = cache.cursor;
            cache.cursor = (cache.cursor + 1) % CAPACITY;
            let entry = &cache.entries[index];
            if entry.attempted || entry.hits < HOT_COUNT {
                continue;
            }
            let Some(block) = &entry.prepared else {
                continue;
            };
            if block.count < 2 {
                continue;
            }
            let block = block.clone();
            let result = cache.compile_unique(&block);
            cache.entries[index].attempted = true;
            match result {
                Ok((code, compiled)) => {
                    cache.entries[index].compiled = Some(code);
                    cache.stats.compiled_blocks += u64::from(compiled);
                    completed += 1;
                }
                Err(error) => {
                    cache.failure = Some(error);
                    cache.stats.compilation_failures += 1;
                    break;
                }
            }
            if completed >= limit.min(8) {
                break;
            }
        }
        completed
    }

    /// Try a register block/backedge or a bounded prefix of a read-only RAM trace.
    /// The machine must bound `budget` before its next device event and deliver
    /// eligible interrupts before calling.
    /// Returning zero has no guest side effects and requests interpreter fallback.
    pub fn dynamic_steps<B: MemoryBus>(&mut self, bus: &B, budget: u32) -> u32 {
        self.dynamic_steps_with_limit(bus, budget, || budget)
    }

    /// Resolve a device deadline only after finding a usable compiled block.
    /// Unsupported interpreter instructions do not pay for deadline arithmetic.
    pub fn dynamic_steps_with_limit<B: MemoryBus>(
        &mut self,
        bus: &B,
        budget: u32,
        limit: impl FnOnce() -> u32,
    ) -> u32 {
        if budget < 2 || !self.dynamic_ready() {
            return 0;
        }
        let steps = self.try_native_block(NativeBus::Read(bus), budget, limit);
        self.dynamic.stats.executed_blocks += u64::from(steps != 0);
        self.dynamic.stats.executed_steps += u64::from(steps);
        steps
    }

    /// Execute a native block with an exclusive bus borrow, allowing guarded
    /// stores when the bus provides an ordinary writable RAM view.
    pub fn dynamic_steps_mut<B: MemoryBus>(&mut self, bus: &mut B, budget: u32) -> u32 {
        if budget < 2 || !self.dynamic_ready() {
            return 0;
        }
        let steps = self.try_native_block(NativeBus::Write(bus), budget, || budget);
        self.dynamic.stats.executed_blocks += u64::from(steps != 0);
        self.dynamic.stats.executed_steps += u64::from(steps);
        steps
    }

    fn dynamic_ready(&self) -> bool {
        self.dynamic.enabled
            && !self.halted
            && self.interrupt_shadow == 0
            && self.flags & (flags::TRAP | flags::RESUME | flags::VIRTUAL) == 0
            && self.control.cr0 & 0x8000_0000 == 0
            && self.registers.protected
            && self.protected_mode()
    }

    /// Execute a bounded mixture of native blocks and decoded ordinary-RAM
    /// instructions. The machine checks pending IRQs first and supplies the next
    /// hardware deadline. I/O, MMIO, faultable operands and mode/IF changes stop
    /// before execution, so hardware and scheduling counters can advance once.
    pub fn dynamic_batch<B: CpuBus>(
        &mut self,
        bus: &mut B,
        budget: u32,
        limit: impl FnOnce(&B) -> u32,
    ) -> u32 {
        if self.execution_labels_enabled() {
            self.dynamic_batch_mode::<B, false, true>(bus, budget, limit, [0; 2])
        } else {
            self.dynamic_batch_mode::<B, false, false>(bus, budget, limit, [0; 2])
        }
    }

    /// Optional scheduler yield at block-head arrival, including JIT exits.
    pub fn dynamic_batch_until_block<B: CpuBus>(
        &mut self,
        bus: &mut B,
        budget: u32,
        limit: impl FnOnce(&B) -> u32,
        head: u32,
    ) -> u32 {
        self.dynamic_batch_until_blocks(bus, budget, limit, [head; 2])
    }

    /// Optional scheduler hints for two selected block heads.
    pub fn dynamic_batch_until_blocks<B: CpuBus>(
        &mut self,
        bus: &mut B,
        budget: u32,
        limit: impl FnOnce(&B) -> u32,
        heads: [u32; 2],
    ) -> u32 {
        if self.execution_labels_enabled() {
            self.dynamic_batch_mode::<B, true, true>(bus, budget, limit, heads)
        } else {
            self.dynamic_batch_mode::<B, true, false>(bus, budget, limit, heads)
        }
    }

    fn dynamic_batch_mode<B: CpuBus, const YIELD: bool, const COUNT: bool>(
        &mut self,
        bus: &mut B,
        budget: u32,
        limit: impl FnOnce(&B) -> u32,
        heads: [u32; 2],
    ) -> u32 {
        if budget < 2 || !self.dynamic_ready() {
            return if YIELD {
                self.interpret_batch_until_blocks(bus, budget, limit, heads)
            } else {
                self.interpret_batch(bus, budget, limit)
            };
        }
        // Native blocks and batched instructions cannot load segment registers,
        // so the CS context and data windows stay valid for the whole slice.
        let Some((code, mut windows)) = self.batch_context() else {
            return 0;
        };
        let budget = budget.min(256).min(limit(bus));
        let mut completed = 0;
        let mut native_steps = 0;
        let mut native_blocks = 0;
        'blocks: while completed < budget {
            if YIELD && completed != 0 && heads.contains(&self.registers.ip) {
                break;
            }
            let Some((index, count, check)) = self.batch_block(code, bus) else {
                break;
            };
            for position in 0..count {
                if YIELD && completed != 0 && heads.contains(&self.registers.ip) {
                    break 'blocks;
                }
                let instruction = self.block_instruction(index, position);
                let remaining = budget - completed;
                if remaining >= 2 && instruction.native_candidate() {
                    let steps = self.try_native_block_mode::<B, YIELD>(
                        NativeBus::Write(bus),
                        remaining,
                        || remaining,
                        heads,
                    );
                    if steps != 0 {
                        completed += steps;
                        native_steps += steps;
                        native_blocks += 1;
                        continue 'blocks;
                    }
                }
                let next = self
                    .registers
                    .ip
                    .wrapping_add(u32::from(instruction.length()));
                if !self.try_prepared(
                    bus,
                    &mut windows,
                    code,
                    instruction,
                    index * super::cache::BLOCK_INSTRUCTIONS + position,
                ) {
                    break 'blocks;
                }
                if COUNT {
                    self.execution_counts[instruction.tier()] += 1;
                }
                completed += 1;
                // As in the interpreter batch: control transfers, repeated
                // strings, the budget and code stores end the block.
                if completed == budget || self.registers.ip != next || !bus.code_valid(&check) {
                    continue 'blocks;
                }
            }
        }
        self.dynamic.stats.execution_batches += u64::from(completed != 0);
        self.dynamic.stats.executed_blocks += native_blocks;
        self.dynamic.stats.executed_steps += u64::from(native_steps);
        self.dynamic.stats.batched_interpreter_steps += u64::from(completed - native_steps);
        completed
    }

    fn try_native_block<B: MemoryBus>(
        &mut self,
        memory: NativeBus<'_, B>,
        budget: u32,
        limit: impl FnOnce() -> u32,
    ) -> u32 {
        self.try_native_block_mode::<B, false>(memory, budget, limit, [0; 2])
    }

    fn try_native_block_mode<B: MemoryBus, const YIELD: bool>(
        &mut self,
        mut memory: NativeBus<'_, B>,
        budget: u32,
        limit: impl FnOnce() -> u32,
        heads: [u32; 2],
    ) -> u32 {
        let bus = memory.as_ref();
        let r = &self.registers;
        let Some((window, cs)) = self.decode_cache.code_context(r) else {
            return 0;
        };
        if !cs.default32 {
            return 0;
        }
        let Some(address) = window.address(r.ip, 1) else {
            return 0;
        };
        let key = Key {
            cs,
            selector: r.cs,
            ip: r.ip,
            protected: r.protected,
            cpl: r.cpl,
        };
        let index = ((address ^ (address >> 10)) as usize) & (CAPACITY - 1);
        let entry = &mut self.dynamic.entries[index];
        if entry.key != Some(key) {
            *entry = Entry {
                key: Some(key),
                ..Entry::default()
            };
        }
        let valid = entry
            .check
            .as_ref()
            .is_some_and(|check| bus.code_valid(check));
        if !valid {
            self.dynamic.stats.invalidations += u64::from(entry.check.is_some());
            // Generation changes may affect data elsewhere on the same page.
            // Rebind unchanged bytes without discarding prepared/compiled IR.
            let unchanged = entry.prepared.as_ref().is_some_and(|block| {
                block.length != 0
                    && bus.code_bytes(address, block.length)
                        == Some(&block.bytes[..usize::from(block.length)])
            });
            if !unchanged {
                let Some(bytes) = bus.code_bytes(address, MAX_BYTES as u8) else {
                    return 0;
                };
                // Imported templates already enter this pool during explicit
                // preparation. Reuse only the complete current representation:
                // an older, shorter register prefix must not pin a RAM trace.
                let mut block = PreparedBlock::decode(bytes);
                if let Some(profile) = &self.decode_cache.profile {
                    block.label(profile, address);
                }
                let native = if block.count >= 2 {
                    self.dynamic
                        .pool
                        .get(&block.fingerprint())
                        .filter(|native| native.block.same_code(&block))
                } else {
                    None
                };
                entry.compiled = native.map(|native| native.code);
                entry.attempted = native.is_some();
                entry.hits = if native.is_some() { HOT_COUNT } else { 0 };
                self.dynamic.stats.prepared_hits += u64::from(native.is_some());
                entry.validated_segments = None;
                entry.prepared = Some(block);
            }
            let block = entry.prepared.as_ref().unwrap();
            let length = block.length.max(1);
            if window.address(r.ip, length).is_none() {
                return 0;
            }
            entry.check = bus.code_check(address, length);
            if entry.check.is_none() {
                return 0;
            }
        }
        entry.hits = entry.hits.saturating_add(1);
        let block = entry.prepared.as_ref().unwrap();
        // A native trace can cross an interior boundary or loop back to its
        // entry several times. Execute such a block through the decoded path
        // so every requested arrival remains visible to the scheduler.
        if YIELD
            && heads
                .iter()
                .any(|head| head.wrapping_sub(r.ip) < u32::from(block.length))
        {
            return 0;
        }
        let Some(code) = entry.compiled else {
            return 0;
        };
        let minimum = if block.trace.is_some() {
            2
        } else {
            u32::from(block.count)
        };
        if budget < minimum {
            return 0;
        }
        let budget = budget.min(limit());
        if budget < minimum {
            return 0;
        }
        let mut trace_ready = true;
        if let Some(trace) = &block.trace
            && !entry.validated_segments.as_ref().is_some_and(|context| {
                context.matches(&self.registers, trace.segments | trace.write_segments)
            })
        {
            if !trace.entry_valid(&self.registers) {
                entry.validated_segments = None;
                trace_ready = false;
            } else {
                entry.validated_segments = Some(ValidatedSegments::capture(&self.registers));
            }
        }
        let (registers, ip) = self.registers.native_state();
        self.dynamic.use_clock = self.dynamic.use_clock.saturating_add(1);
        self.dynamic.banks[code.bank].last_used = self.dynamic.use_clock;
        let Some(compiler) = self.dynamic.banks[code.bank].compiler.as_ref() else {
            return 0;
        };
        let profiling = !self.dynamic.exit_records.is_empty();
        let event = |exit_ip, reason, steps: u32| DynamicExitRecord {
            cs: key.selector,
            entry_ip: key.ip,
            exit_ip,
            entry_linear: key.cs.base.wrapping_add(key.ip),
            exit_linear: key.cs.base.wrapping_add(exit_ip),
            reason,
            count: 1,
            steps: u64::from(steps),
        };
        match code.function {
            NativeFunction::Registers(code) => {
                let steps = compiler.execute(code, registers, &mut self.flags, ip, budget);
                block.count_register_steps(&mut self.execution_counts, steps);
                if profiling {
                    let looping = block.instructions[..usize::from(block.register_count)]
                        .last()
                        .is_some_and(|i| matches!(i.operation, Operation::LoopBranch { .. }));
                    let reason = if looping
                        && *ip == key.ip
                        && budget - steps < u32::from(block.register_count)
                    {
                        DynamicExitReason::Budget
                    } else if looping {
                        DynamicExitReason::LoopExit
                    } else {
                        DynamicExitReason::RegisterEnd
                    };
                    record_exit(
                        &mut self.dynamic.exit_records,
                        &mut self.dynamic.exit_dropped,
                        event(*ip, reason, steps),
                    );
                }
                steps
            }
            NativeFunction::Trace {
                code,
                register_fallback,
            } => {
                let trace = block.trace.as_ref().unwrap();
                // A broader trace must not remove a previously usable native
                // register prefix on buses or segments outside its RAM contract.
                let (steps, reason) = if !trace_ready {
                    (0, DynamicExitReason::SegmentGuard)
                } else {
                    (|| {
                        if trace.write_segments != 0 {
                            let NativeBus::Write(bus) = &mut memory else {
                                return (0, DynamicExitReason::ReadOnlyBus);
                            };
                            let Some(ram) = bus.native_ram_mut() else {
                                return (0, DynamicExitReason::RamUnavailable);
                            };
                            // Protect all pages occupied by this trace, including gaps.
                            // A store to another compiled block invalidates its page
                            // generation before any later cache lookup.
                            let protected_start = address & !4095;
                            let protected_end =
                                (u64::from(address) + u64::from(block.length) + 4095) & !4095;
                            let protected_length =
                                (protected_end - u64::from(protected_start)) as u32;
                            let report = compiler.execute_trace_writable_report_with_exclusion(
                                code,
                                registers,
                                &mut self.flags,
                                ip,
                                budget,
                                ram.linear_base,
                                ram.bytes,
                                ram.page_generations,
                                protected_start,
                                protected_length,
                                ram.excluded_start,
                                ram.excluded_len,
                            );
                            if block.labelled {
                                self.execution_counts[0] += u64::from(report.tier_counts[0]);
                                self.execution_counts[1] += u64::from(report.tier_counts[1]);
                                self.execution_counts[3] += u64::from(report.tier_counts[2]);
                                self.execution_counts[2] += u64::from(
                                    report.steps - report.tier_counts.iter().sum::<u32>(),
                                );
                            }
                            (report.steps, exit_reason(report.reason))
                        } else {
                            let ram = if trace.segments == 0 {
                                doslet_bus::NativeRam {
                                    linear_base: 0,
                                    bytes: &[],
                                    excluded_start: 0,
                                    excluded_len: 0,
                                }
                            } else {
                                let Some(ram) = memory.as_ref().native_ram() else {
                                    return (0, DynamicExitReason::RamUnavailable);
                                };
                                ram
                            };
                            let report = compiler.execute_trace_report_with_exclusion(
                                code,
                                registers,
                                &mut self.flags,
                                ip,
                                budget,
                                ram.linear_base,
                                ram.bytes,
                                ram.excluded_start,
                                ram.excluded_len,
                            );
                            if block.labelled {
                                self.execution_counts[0] += u64::from(report.tier_counts[0]);
                                self.execution_counts[1] += u64::from(report.tier_counts[1]);
                                self.execution_counts[3] += u64::from(report.tier_counts[2]);
                                self.execution_counts[2] += u64::from(
                                    report.steps - report.tier_counts.iter().sum::<u32>(),
                                );
                            }
                            (report.steps, exit_reason(report.reason))
                        }
                    })()
                };
                self.dynamic.stats.ram_trace_steps += u64::from(steps);
                self.dynamic.stats.ram_trace_entries += u64::from(steps != 0);
                if profiling {
                    let reason = if reason == DynamicExitReason::GraphLimit {
                        let exit_address = key.cs.base.wrapping_add(*ip);
                        let supported = memory
                            .as_ref()
                            .code_bytes(exit_address, 15)
                            .and_then(|bytes| cache::native_trace_instruction(bytes, 0))
                            .is_some();
                        if supported {
                            DynamicExitReason::GraphLimit
                        } else {
                            DynamicExitReason::UnsupportedInstruction
                        }
                    } else {
                        reason
                    };
                    record_exit(
                        &mut self.dynamic.exit_records,
                        &mut self.dynamic.exit_dropped,
                        event(*ip, reason, steps),
                    );
                }
                if steps == 0
                    && budget >= u32::from(block.register_count)
                    && let Some(code) = register_fallback
                {
                    let completed = compiler.execute(code, registers, &mut self.flags, ip, budget);
                    block.count_register_steps(&mut self.execution_counts, completed);
                    if profiling {
                        record_exit(
                            &mut self.dynamic.exit_records,
                            &mut self.dynamic.exit_dropped,
                            event(*ip, DynamicExitReason::RegisterEnd, completed),
                        );
                    }
                    completed
                } else {
                    steps
                }
            }
        }
    }
}

#[cfg(test)]
mod code_cache_tests {
    use super::*;

    extern crate std;

    #[test]
    fn asynchronous_compilation_owns_no_guest_state_and_rejects_replaced_cpu() {
        let mut cpu = Cpu::default();
        cpu.dynamic.entries[0] = Entry {
            prepared: Some(block(41, true)),
            hits: HOT_COUNT,
            ..Entry::default()
        };
        let job = cpu.take_dynamic_compilation(2).unwrap();
        assert!(cpu.take_dynamic_compilation(2).is_none());
        assert_eq!(cpu.prepare_dynamic(2), 0);
        let caller = std::thread::current().id();
        let result = std::thread::spawn(move || {
            assert_ne!(std::thread::current().id(), caller);
            job.compile()
        })
        .join()
        .unwrap();
        assert_eq!(cpu.finish_dynamic_compilation(result), 1);
        check_trace(&cpu.dynamic, cpu.dynamic.entries[0].compiled.unwrap(), 41);
        cpu.dynamic.entries[1] = Entry {
            prepared: Some(block(42, true)),
            hits: HOT_COUNT,
            ..Entry::default()
        };
        let job = cpu.take_dynamic_compilation(2).unwrap();
        let mut replacement = Cpu::default();
        assert_eq!(replacement.finish_dynamic_compilation(job.compile()), 0);
        assert!(replacement.dynamic.banks.is_empty());
        assert_eq!(replacement.dynamic.stats.compiled_blocks, 0);
    }

    #[test]
    fn asynchronous_publication_does_not_bind_changed_code_and_reclaims_banks() {
        let mut cpu = Cpu {
            dynamic: small_cache(false),
            ..Cpu::default()
        };
        for value in 0..8 {
            cpu.dynamic.entries[0] = Entry {
                prepared: Some(block(value, true)),
                hits: HOT_COUNT,
                ..Entry::default()
            };
            let job = cpu.take_dynamic_compilation(1).unwrap();
            if value == 0 {
                cpu.dynamic.entries[0].prepared = Some(block(99, true));
            }
            assert_eq!(cpu.finish_dynamic_compilation(job.compile()), 1);
            if value == 0 {
                assert!(cpu.dynamic.entries[0].compiled.is_none());
            } else {
                check_trace(
                    &cpu.dynamic,
                    cpu.dynamic.entries[0].compiled.unwrap(),
                    value,
                );
            }
            assert!(cpu.dynamic.banks.len() <= 2);
            assert!(cpu.dynamic.pool.len() <= 2);
            assert!(cpu.dynamic_error().is_none());
        }
        assert_eq!(cpu.dynamic.stats.code_cache_evictions, 6);
    }

    fn block(value: u32, trace: bool) -> PreparedBlock {
        let value = value.to_le_bytes();
        // MOV EAX,imm32; INC EAX; [MOV EBX,[EBX]]; unsupported NOP.
        let bytes = [
            0xB8, value[0], value[1], value[2], value[3], 0x40, 0x8B, 0x1B, 0x90,
        ];
        PreparedBlock::decode(&bytes[..if trace { 9 } else { 6 }])
    }

    fn small_cache(profiled: bool) -> Cache {
        Cache {
            bank_limit: 2,
            bank_function_limit: 2,
            exit_records: if profiled {
                alloc::vec![DynamicExitRecord::default(); EXIT_CAPACITY]
            } else {
                Vec::new()
            },
            ..Cache::default()
        }
    }

    fn check_trace(cache: &Cache, code: NativeBlock, value: u32) {
        let NativeFunction::Trace { code: trace, .. } = code.function else {
            panic!("expected RAM trace")
        };
        let mut registers = [0; 8];
        let mut flags = 0x203;
        let mut ip = 0x1000;
        assert_eq!(
            cache.banks[code.bank]
                .compiler
                .as_ref()
                .unwrap()
                .execute_trace(
                    trace,
                    &mut registers,
                    &mut flags,
                    &mut ip,
                    3,
                    0,
                    &123_u32.to_le_bytes(),
                ),
            3
        );
        assert_eq!(registers[0], value + 1);
        assert_eq!(registers[3], 123);
        assert_eq!(flags & 0x201, 0x201);
        assert_eq!(ip, 0x1008);
    }

    #[test]
    fn lru_retires_the_cold_bank_clears_all_handles_and_keeps_hot_code() {
        for profiled in [false, true] {
            let mut cache = small_cache(profiled);
            let a = block(10, true);
            let b = block(20, false);
            let c = block(30, false);
            let d = block(40, true);
            let (code_a, _) = cache.compile_unique(&a).unwrap();
            let (code_b, _) = cache.compile_unique(&b).unwrap();
            let (code_c, _) = cache.compile_unique(&c).unwrap();
            assert_ne!(code_a.bank, code_b.bank);
            assert_eq!(code_b.bank, code_c.bank);
            for (index, (block, compiled)) in [
                (a.clone(), code_a),
                (b.clone(), code_b),
                (c.clone(), code_c),
            ]
            .into_iter()
            .enumerate()
            {
                cache.entries[index] = Entry {
                    prepared: Some(block.clone()),
                    compiled: Some(compiled),
                    attempted: true,
                    hits: HOT_COUNT,
                    ..Entry::default()
                };
                cache.templates.push(Template {
                    block,
                    compiled: Some(compiled),
                    attempted: true,
                });
            }
            // Touch the older bank: eviction must follow recency, not creation order.
            cache.touch_bank(code_a.bank);
            let (code_d, _) = cache.compile_unique(&d).unwrap();
            assert_eq!(code_d.bank, code_b.bank);
            assert!(cache.entries[0].compiled.is_some());
            assert!(cache.templates[0].compiled.is_some());
            for index in [1, 2] {
                assert!(cache.entries[index].compiled.is_none());
                assert!(!cache.entries[index].attempted);
                assert_eq!(cache.entries[index].hits, 0);
                assert!(cache.templates[index].compiled.is_none());
                assert!(cache.templates[index].attempted);
            }
            assert_eq!(cache.pool.len(), 2);
            assert!(cache.pool.contains_key(&a.fingerprint()));
            assert!(cache.pool.contains_key(&d.fingerprint()));
            assert_eq!(cache.stats.code_cache_evictions, 1);
            assert!(cache.failure.is_none());
            check_trace(&cache, code_a, 10);
            check_trace(&cache, code_d, 40);
            // Reuse a full bank without eviction, then recompile an evicted block.
            assert!(!cache.compile_unique(&a).unwrap().1);
            assert_eq!(cache.stats.code_cache_evictions, 1);
            assert!(cache.compile_unique(&b).unwrap().1);
            assert_eq!(cache.stats.code_cache_evictions, 2);
            assert_eq!(cache.banks.len(), 2);
        }
    }

    #[test]
    fn preparation_continues_after_multiple_evictions_without_replaying_cold_templates() {
        let mut cpu = Cpu {
            dynamic: small_cache(false),
            ..Cpu::default()
        };
        let template = block(100, true);
        cpu.dynamic.templates.push(Template {
            block: template.clone(),
            compiled: None,
            attempted: false,
        });
        assert_eq!(cpu.prepare_dynamic(1), 1);
        let other = block(101, true);
        cpu.dynamic.compile_unique(&other).unwrap();
        let replacement = block(102, true);
        cpu.dynamic.compile_unique(&replacement).unwrap();
        assert!(cpu.dynamic.templates[0].compiled.is_none());
        assert!(cpu.dynamic.templates[0].attempted);
        for index in 0..8 {
            cpu.dynamic.entries[index] = Entry {
                prepared: Some(block(index as u32, true)),
                hits: HOT_COUNT,
                ..Entry::default()
            };
        }
        for _ in 0..8 {
            assert_eq!(cpu.prepare_dynamic(1), 1);
            assert!(cpu.dynamic_error().is_none());
            assert_eq!(cpu.dynamic_stats().compilation_failures, 0);
            assert!(cpu.dynamic.banks.len() <= 2);
            assert!(cpu.dynamic.pool.len() <= 2);
        }
        assert!(cpu.dynamic_stats().code_cache_evictions >= 8);
        assert!(!cpu.dynamic.pool.contains_key(&template.fingerprint()));
        let entry = cpu
            .dynamic
            .entries
            .iter()
            .find(|entry| entry.compiled.is_some())
            .unwrap();
        let value = u32::from_le_bytes(
            entry.prepared.as_ref().unwrap().bytes[1..5]
                .try_into()
                .unwrap(),
        );
        check_trace(&cpu.dynamic, entry.compiled.unwrap(), value);
    }

    #[test]
    fn preparation_compiles_past_the_previous_lifetime_limit() {
        let mut cpu = Cpu::default();
        for value in 0..=MAX_COMPILED_BLOCKS {
            cpu.dynamic.entries[0] = Entry {
                prepared: Some(block(value as u32, false)),
                hits: HOT_COUNT,
                ..Entry::default()
            };
            cpu.dynamic.cursor = 0;
            assert_eq!(cpu.prepare_dynamic(1), 1);
            assert!(cpu.dynamic_error().is_none());
        }
        assert_eq!(
            cpu.dynamic_stats().compiled_blocks,
            MAX_COMPILED_BLOCKS as u64 + 1
        );
        assert_eq!(cpu.dynamic_stats().compilation_failures, 0);
        assert_eq!(cpu.dynamic_stats().code_cache_evictions, 1);
        assert_eq!(cpu.dynamic.banks.len(), CODE_BANKS);
        let resident: usize = cpu
            .dynamic
            .banks
            .iter()
            .map(|bank| {
                FUNCTIONS_PER_BANK
                    - bank
                        .compiler
                        .as_ref()
                        .map_or(0, Compiler::remaining_capacity)
            })
            .sum();
        assert!(resident <= MAX_COMPILED_BLOCKS);
        let code = cpu.dynamic.entries[0].compiled.unwrap();
        let NativeFunction::Registers(function) = code.function else {
            panic!("expected register block")
        };
        let mut registers = [0; 8];
        let mut flags = 0x202;
        let mut ip = 0x1000;
        assert_eq!(
            cpu.dynamic.banks[code.bank]
                .compiler
                .as_ref()
                .unwrap()
                .execute(function, &mut registers, &mut flags, &mut ip, 2),
            2
        );
        assert_eq!(registers[0], MAX_COMPILED_BLOCKS as u32 + 1);
        assert_eq!(ip, 0x1006);
    }

    #[test]
    fn trace_and_fallback_reserve_two_slots_before_compiling() {
        let mut cache = Cache {
            bank_limit: 2,
            bank_function_limit: 3,
            ..Cache::default()
        };
        let (register, _) = cache.compile_unique(&block(0, false)).unwrap();
        let (trace, _) = cache.compile_unique(&block(1, true)).unwrap();
        assert_eq!(register.bank, trace.bank);
        assert_eq!(
            cache.banks[trace.bank]
                .compiler
                .as_ref()
                .unwrap()
                .remaining_capacity(),
            0
        );
        let (next, _) = cache.compile_unique(&block(2, true)).unwrap();
        assert_ne!(trace.bank, next.bank);
        // Only one slot remains; another two-function trace must retire a bank,
        // never leave an orphan trace after a failing fallback compilation.
        let (final_code, _) = cache.compile_unique(&block(3, true)).unwrap();
        assert_eq!(cache.stats.code_cache_evictions, 1);
        check_trace(&cache, next, 2);
        check_trace(&cache, final_code, 3);
        assert!(cache.failure.is_none());
    }
}
