//! Bounded native block cache. Discovery/execution allocate nothing. Compilation
//! is an explicit maintenance operation, outside emulation and audio callbacks.
use super::{Cpu, cache};
use crate::{Access, SegmentCache, SegmentRegister, flags};
use crate::{DynamicExitReason, DynamicExitRecord, DynamicStats};
use alloc::{collections::BTreeMap, string::String, vec::Vec};
use doslet_bus::{CodeCheck, CpuBus, MemoryBus};
use doslet_x86_jit::{
    CompiledBlock, CompiledTrace, Compiler, Instruction, MAX_INSTRUCTIONS, Operation,
    TraceInstruction, TraceOperation, TraceSource,
};

const CAPACITY: usize = 1024;
const MAX_BYTES: usize = 15 * MAX_INSTRUCTIONS;
const HOT_COUNT: u16 = 16;
const EXIT_CAPACITY: usize = 8192;

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
enum NativeBlock {
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
}
impl PreparedBlock {
    fn fingerprint(&self) -> u64 {
        self.bytes[..usize::from(self.length)]
            .iter()
            .fold(0xcbf29ce484222325, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            })
    }
    fn same_code(&self, other: &Self) -> bool {
        self.length == other.length
            && self.bytes[..usize::from(self.length)] == other.bytes[..usize::from(other.length)]
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
}
struct NativeCode {
    block: PreparedBlock,
    code: NativeBlock,
}
// This pool survives execution-cache collisions and CS changes. It grows only
// during explicit preparation, bounded by Compiler's lifetime capacity.
fn compile_unique(
    compiler: &mut Option<Compiler>,
    pool: &mut BTreeMap<u64, NativeCode>,
    block: &PreparedBlock,
    exit_profiling: bool,
) -> Result<(NativeBlock, bool), String> {
    let hash = block.fingerprint();
    if let Some(native) = pool
        .get(&hash)
        .filter(|native| native.block.same_code(block))
    {
        return Ok((native.code, false));
    }
    if compiler.is_none() {
        *compiler = Some(Compiler::new()?);
    }
    let compiler = compiler.as_mut().unwrap();
    let code = if let Some(trace) = &block.trace {
        let instructions = &trace.instructions[..usize::from(trace.count)];
        let code = if exit_profiling {
            compiler.compile_trace_profiled(instructions)?
        } else {
            compiler.compile_trace(instructions)?
        };
        let register_fallback = if block.register_count >= 2 {
            Some(compiler.compile(&block.instructions[..usize::from(block.register_count)])?)
        } else {
            None
        };
        NativeBlock::Trace {
            code,
            register_fallback,
        }
    } else {
        NativeBlock::Registers(compiler.compile(&block.instructions[..usize::from(block.count)])?)
    };
    pool.insert(
        hash,
        NativeCode {
            block: block.clone(),
            code,
        },
    );
    Ok((code, true))
}
pub(super) struct Cache {
    entries: Vec<Entry>,
    compiler: Option<Compiler>,
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
            entries: (0..CAPACITY).map(|_| Entry::default()).collect(),
            compiler: None,
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
        if self.dynamic.compiler.is_some() {
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
    /// and request executable memory. Failure latches once; interpretation keeps
    /// working, and already compiled code remains available.
    pub fn prepare_dynamic(&mut self, limit: usize) -> usize {
        let cache = &mut self.dynamic;
        if !cache.enabled || cache.failure.is_some() || limit == 0 {
            return 0;
        }
        let mut completed = 0;
        // Previously observed blocks are prepared before their first execution.
        for template in &mut cache.templates {
            if template.compiled.is_some() {
                continue;
            }
            let result = compile_unique(
                &mut cache.compiler,
                &mut cache.pool,
                &template.block,
                !cache.exit_records.is_empty(),
            );
            match result {
                Ok((code, compiled)) => {
                    template.compiled = Some(code);
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
            entry.attempted = true;
            let result = compile_unique(
                &mut cache.compiler,
                &mut cache.pool,
                block,
                !cache.exit_records.is_empty(),
            );
            match result {
                Ok((code, compiled)) => {
                    entry.compiled = Some(code);
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
        if budget < 2 || !self.dynamic_ready() {
            return 0;
        }
        let Some(mut instruction) = self.batch_instruction(bus) else {
            return 0;
        };
        let budget = budget.min(256).min(limit(bus));
        let mut completed = 0;
        let mut native_steps = 0;
        let mut native_blocks = 0;
        while completed < budget {
            let remaining = budget - completed;
            let steps = if remaining >= 2 && instruction.native_candidate() {
                self.try_native_block(NativeBus::Write(bus), remaining, || remaining)
            } else {
                0
            };
            if steps != 0 {
                completed += steps;
                native_steps += steps;
                native_blocks += 1;
            } else {
                self.execute_batched(bus, instruction);
                completed += 1;
            }
            if completed == budget {
                break;
            }
            let Some(next) = self.batch_instruction(bus) else {
                break;
            };
            instruction = next;
        }
        self.dynamic.stats.execution_batches += u64::from(completed != 0);
        self.dynamic.stats.executed_blocks += native_blocks;
        self.dynamic.stats.executed_steps += u64::from(native_steps);
        self.dynamic.stats.batched_interpreter_steps += u64::from(completed - native_steps);
        completed
    }

    fn try_native_block<B: MemoryBus>(
        &mut self,
        mut memory: NativeBus<'_, B>,
        budget: u32,
        limit: impl FnOnce() -> u32,
    ) -> u32 {
        let bus = memory.as_ref();
        let r = &self.registers;
        let Some((window, cs)) = self.decode_cache.code_context(r) else {
            return 0;
        };
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
                let block = PreparedBlock::decode(bytes);
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
        let compiler = self.dynamic.compiler.as_ref().unwrap();
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
        match code {
            NativeBlock::Registers(code) => {
                let steps = compiler.execute(code, registers, &mut self.flags, ip, budget);
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
            NativeBlock::Trace {
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
