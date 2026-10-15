//! Single-instruction cache. Unsupported forms use the original interpreter.
use super::{Cpu, CpuError, StepOutcome};
use crate::state::SegmentWindow;
use crate::{
    Access, Gpr16, Gpr32, Registers, SegmentCache, SegmentRegister, decode::Operand, flags,
};
use doslet_bus::{CodeCheck, CpuBus};

const CAPACITY: usize = 512;
const NO_REGISTER: u8 = 8;

#[cfg(feature = "profiling")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DecodeCacheStats {
    pub hits: u64,
    pub misses: u64,
    /// Resident entries rejected after a write, mapping or CS-context change.
    pub invalidations: u64,
    pub fallbacks: u64,
    /// Transitions between observed default-32-bit CS contexts (not entries).
    pub context_switches: u64,
    /// Resident entries discarded by a context transition.
    pub context_invalidations: u64,
    /// Mutually exclusive reasons for a matching entry's stale code check.
    pub write_invalidations: u64,
    pub mapping_invalidations: u64,
    pub owner_invalidations: u64,
    pub unavailable_invalidations: u64,
    pub segment_invalidations: u64,
    /// Lookups finding an occupied slot with a different instruction key.
    pub collisions: u64,
    /// Occupied slots replaced by another key after successful decoding.
    pub evictions: u64,
    pub decode_attempts: u64,
    pub failed_decodes: u64,
    pub fallback_hits: u64,
    /// Interpreter batches without the native core, and the steps they ran.
    pub batches: u64,
    pub batched_steps: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Context {
    cs: SegmentCache,
    selector: u16,
    protected: bool,
}
#[derive(Clone, Copy)]
struct Entry {
    address: u32,
    check: CodeCheck,
    instruction: Instruction,
}
pub(super) struct Cache {
    entries: [Option<Entry>; CAPACITY],
    context: Option<Context>,
    /// Registers lineage, CS version, selector and PE mode under which
    /// `code32` and `context` were last resolved.
    key: Option<(u32, u64, u16, bool)>,
    code32: bool,
    /// Execute window of `context`; None leaves its faults to the interpreter.
    window: Option<SegmentWindow>,
    pub enabled: bool,
    #[cfg(feature = "profiling")]
    pub stats: DecodeCacheStats,
}
impl Default for Cache {
    fn default() -> Self {
        Self {
            entries: [None; CAPACITY],
            context: None,
            key: None,
            code32: false,
            window: None,
            enabled: true,
            #[cfg(feature = "profiling")]
            stats: DecodeCacheStats::default(),
        }
    }
}
impl core::fmt::Debug for Cache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("DecodeCache")
            .field("enabled", &self.enabled)
            .field("capacity", &CAPACITY)
            .finish_non_exhaustive()
    }
}
impl Cache {
    pub fn clear(&mut self) {
        self.entries.fill(None);
        self.context = None;
        self.key = None;
        self.code32 = false;
        self.window = None;
    }
    /// Full context check. Entries are keyed by linear address and decoded only
    /// for default-32-bit code, so they remain valid in every 32-bit context;
    /// each hit still checks its offset range against the current window.
    #[inline(always)]
    fn resolve_context(&mut self, r: &Registers) {
        let context = Context {
            cs: r.segment_cache(SegmentRegister::Cs),
            selector: r.cs,
            protected: r.protected,
        };
        self.code32 = context.cs.default32;
        if self.code32 && self.context != Some(context) {
            #[cfg(feature = "profiling")]
            {
                self.stats.context_switches += u64::from(self.context.is_some());
            }
            self.context = Some(context);
            // The context fixes every CS check except the instruction's offset range.
            self.window = r.window(SegmentRegister::Cs, Access::Execute);
        }
    }
    #[inline(always)]
    pub(super) fn code_context(&mut self, r: &Registers) -> Option<(SegmentWindow, SegmentCache)> {
        // The hidden CS cache changes only with a new version in the same
        // lineage or with PE mode; the selector is public, so it is keyed too.
        // Keep the tuple: a packed [u64; 2] key measured ~5 ns/step slower in
        // Chrome WASM, although native code was unaffected.
        let key = (r.cs_lineage, r.cs_version, r.cs, r.protected);
        let known = r.cs_lineage != 0 && r.cs_version != u64::MAX && self.key == Some(key);
        #[cfg(debug_assertions)]
        if known {
            let cs = r.segment_cache(SegmentRegister::Cs);
            debug_assert_eq!(self.code32, cs.default32);
            debug_assert!(
                !self.code32
                    || self.context
                        == Some(Context {
                            cs,
                            selector: r.cs,
                            protected: r.protected,
                        })
            );
        }
        if !known {
            self.resolve_context(r);
            self.key = Some(key);
        }
        if !self.code32 {
            return None;
        }
        Some((self.window?, self.context?.cs))
    }
    fn instruction<B: CpuBus>(&mut self, r: &Registers, bus: &B) -> Option<Instruction> {
        if !self.enabled {
            return None;
        }
        let (window, _) = self.code_context(r)?;
        self.instruction_in(window, r.ip, bus)
    }
    /// Look up `ip` in an already resolved 32-bit code context.
    #[inline]
    fn instruction_in<B: CpuBus>(
        &mut self,
        window: SegmentWindow,
        ip: u32,
        bus: &B,
    ) -> Option<Instruction> {
        let address = window.address(ip, 1)?;
        let index = ((address ^ (address >> 9)) as usize) & (CAPACITY - 1);
        if let Some(entry) = self.entries[index]
            && entry.address == address
        {
            let segment_valid = window.address(ip, entry.instruction.length).is_some();
            if segment_valid && bus.code_valid(&entry.check) {
                if matches!(entry.instruction.operation, Operation::Unsupported) {
                    #[cfg(feature = "profiling")]
                    {
                        self.stats.fallback_hits += 1;
                    }
                    return None;
                }
                #[cfg(feature = "profiling")]
                {
                    self.stats.hits += 1;
                }
                return Some(entry.instruction);
            }
            self.entries[index] = None;
            #[cfg(feature = "profiling")]
            {
                self.stats.invalidations += 1;
                // Classify with a fresh check; only profiling builds pay for it.
                if !segment_valid {
                    self.stats.segment_invalidations += 1;
                } else if let Some(check) = bus.code_check(address, entry.instruction.length) {
                    if check.space >> 32 != entry.check.space >> 32 {
                        self.stats.owner_invalidations += 1;
                    } else if check.space != entry.check.space {
                        self.stats.mapping_invalidations += 1;
                    } else {
                        self.stats.write_invalidations += 1;
                    }
                } else {
                    self.stats.unavailable_invalidations += 1;
                }
            }
        }
        #[cfg(feature = "profiling")]
        {
            self.stats.misses += 1;
            self.stats.collisions += u64::from(self.entries[index].is_some());
        }
        let bytes = bus.code_bytes(address, 15)?;
        #[cfg(feature = "profiling")]
        {
            self.stats.decode_attempts += 1;
        }
        let Some(instruction) = Instruction::decode(bytes) else {
            #[cfg(feature = "profiling")]
            {
                self.stats.failed_decodes += 1;
            }
            // Remember the rejection while the bytes the decoder saw stay
            // unchanged, so the reference path runs without another attempt.
            let length = bytes.len() as u8;
            if let Some(check) = bus.code_check(address, length) {
                self.entries[index] = Some(Entry {
                    address,
                    check,
                    instruction: Instruction::UNSUPPORTED,
                });
            }
            return None;
        };
        // Failed probes leave registers/bus untouched. Let byte fetching report
        // the original fault and partial IP progress at a code-segment boundary.
        window.address(ip, instruction.length)?;
        let check = bus.code_check(address, instruction.length)?;
        #[cfg(feature = "profiling")]
        {
            self.stats.evictions += u64::from(self.entries[index].is_some());
        }
        self.entries[index] = Some(Entry {
            address,
            check,
            instruction,
        });
        Some(instruction)
    }
}

#[derive(Clone, Copy)]
enum Target {
    Register(u8),
    Memory {
        segment: SegmentRegister,
        base: u8,
        index: u8,
        scale: u8,
        displacement: u32,
        address32: bool,
    },
}
impl Target {
    fn resolve(self, r: &Registers) -> Operand {
        match self {
            Self::Register(code) => Operand::Register(code),
            Self::Memory {
                segment,
                base,
                index,
                scale,
                displacement,
                address32,
            } => {
                let value = |code| {
                    if code == NO_REGISTER {
                        0
                    } else {
                        let value = r.get32(Gpr32::decode(code));
                        if address32 {
                            value
                        } else {
                            u32::from(value as u16)
                        }
                    }
                };
                let offset = value(base)
                    .wrapping_add(value(index).wrapping_shl(u32::from(scale)))
                    .wrapping_add(displacement);
                Operand::Memory {
                    segment,
                    offset: if address32 {
                        offset
                    } else {
                        u32::from(offset as u16)
                    },
                }
            }
        }
    }
}
#[derive(Clone, Copy)]
enum Value {
    Operand(Target),
    Immediate(u32),
}
impl Value {
    fn read<B: CpuBus>(self, cpu: &Cpu, bus: &mut B, dword: bool) -> Result<u32, CpuError> {
        match self {
            Self::Immediate(value) => Ok(value),
            Self::Operand(target) => read(target.resolve(&cpu.registers), cpu, bus, dword),
        }
    }
}
fn read<B: CpuBus>(target: Operand, cpu: &Cpu, bus: &mut B, dword: bool) -> Result<u32, CpuError> {
    if dword {
        target.read32(&cpu.registers, bus)
    } else {
        target.read(&cpu.registers, bus, false).map(u32::from)
    }
}
fn write<B: CpuBus>(
    target: Operand,
    cpu: &mut Cpu,
    bus: &mut B,
    dword: bool,
    value: u32,
) -> Result<(), CpuError> {
    if dword {
        target.write32(&mut cpu.registers, bus, value)
    } else {
        target.write(&mut cpu.registers, bus, false, value as u16)
    }
}
fn read_width<B: CpuBus>(
    target: Operand,
    cpu: &Cpu,
    bus: &mut B,
    width: u8,
) -> Result<u32, CpuError> {
    match width {
        4 => target.read32(&cpu.registers, bus),
        _ => target.read(&cpu.registers, bus, width == 2).map(u32::from),
    }
}
fn write_width<B: CpuBus>(
    target: Operand,
    cpu: &mut Cpu,
    bus: &mut B,
    width: u8,
    value: u32,
) -> Result<(), CpuError> {
    match width {
        4 => target.write32(&mut cpu.registers, bus, value),
        _ => target.write(&mut cpu.registers, bus, width == 2, value as u16),
    }
}
#[derive(Clone, Copy)]
enum Operation {
    Move {
        target: Target,
        source: Value,
    },
    Lea {
        register: u8,
        target: Target,
    },
    Alu {
        target: Target,
        source: Value,
        operation: u8,
        store: bool,
    },
    Exchange {
        target: Target,
        register: u8,
    },
    Inc {
        register: u8,
        decrement: bool,
    },
    Unary {
        target: Target,
        negate: bool,
    },
    Push(Value),
    Pop(u8),
    Branch {
        condition: Option<u8>,
        displacement: i32,
    },
    Call(i32),
    CallIndirect(Value),
    Return(u16),
    Input,
    Output,
    /// LOOPNE/LOOPE/LOOP (E0-E2) and JECXZ (E3), selected by `kind`.
    Loop {
        kind: u8,
        displacement: i32,
        address32: bool,
    },
    /// MOVS (A4), STOS (AA) or LODS (AC); one iteration per step under REP.
    String {
        kind: u8,
        width: u8,
        segment: SegmentRegister,
        address32: bool,
        repeat: bool,
    },
    /// MUL, IMUL, DIV or IDIV of the accumulator (F6/F7 /4-/7).
    MulDiv {
        target: Target,
        operation: u8,
    },
    /// CWDE (98) or CDQ (99).
    Extend {
        cdq: bool,
    },
    /// MOVZX/MOVSX (0F B6/B7/BE/BF) into a dword register.
    MoveExtend {
        register: u8,
        source: Target,
        word: bool,
        signed: bool,
    },
    /// A remembered decoder rejection; never executed from the cache.
    Unsupported,
}
#[derive(Clone, Copy)]
pub(super) struct Instruction {
    length: u8,
    /// Position before a faultable data read that precedes immediate fetching.
    read_length: u8,
    dword: bool,
    operation: Operation,
}

#[cfg(all(
    feature = "dynamic-core",
    not(target_arch = "wasm32"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
impl Instruction {
    pub(super) fn native_candidate(self) -> bool {
        match self.operation {
            Operation::Move {
                target: Target::Register(_),
                source,
            } => {
                matches!(
                    source,
                    Value::Immediate(_)
                        | Value::Operand(Target::Register(_))
                        | Value::Operand(Target::Memory {
                            address32: true,
                            ..
                        })
                )
            }
            Operation::Move {
                target: Target::Memory {
                    address32: true, ..
                },
                source: Value::Immediate(_) | Value::Operand(Target::Register(_)),
            } => true,
            Operation::Alu {
                target,
                source,
                operation,
                store,
            } => {
                let readable = |target| {
                    matches!(
                        target,
                        Target::Register(_)
                            | Target::Memory {
                                address32: true,
                                ..
                            }
                    )
                };
                readable(target)
                    && match source {
                        Value::Immediate(_) => true,
                        Value::Operand(source) => readable(source),
                    }
                    && if store {
                        self.dword && matches!(target, Target::Register(_))
                    } else {
                        self.dword || matches!(operation, 4 | 7)
                    }
            }
            Operation::Lea {
                target: Target::Memory {
                    address32: true, ..
                },
                ..
            }
            | Operation::Push(_)
            | Operation::Pop(_)
            | Operation::Call(_)
            | Operation::CallIndirect(_)
            | Operation::Return(_) => true,
            Operation::Exchange {
                target: Target::Memory {
                    address32: true, ..
                },
                ..
            } => self.dword,
            Operation::Inc { .. }
            | Operation::Exchange {
                target: Target::Register(_),
                ..
            }
            | Operation::Unary {
                target: Target::Register(_),
                ..
            } => self.dword,
            _ => false,
        }
    }
}

/// Decode default-32-bit trace operations without resolving data addresses.
/// Used segment assumptions are validated and cached at native entry.
#[cfg(all(
    feature = "dynamic-core",
    not(target_arch = "wasm32"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub(super) fn native_trace_instruction(
    bytes: &[u8],
    offset: u8,
) -> Option<(doslet_x86_jit::TraceInstruction, u8, u8)> {
    use doslet_x86_jit::{DwordMemoryAddress, TraceInstruction, TraceOperation as Op, TraceSource};
    let i = Instruction::decode(bytes)?;
    let mut segments = 0;
    let mut write_segments = 0;
    let memory = |target| match target {
        Target::Memory {
            segment,
            base,
            index,
            scale,
            displacement,
            address32: true,
        } => Some((
            segment,
            DwordMemoryAddress {
                base: (base != NO_REGISTER).then_some(base),
                index: (index != NO_REGISTER).then_some(index),
                scale,
                displacement,
            },
        )),
        _ => None,
    };
    let mut source = |v, byte| match v {
        Value::Immediate(v) => Some(TraceSource::Immediate(v)),
        Value::Operand(Target::Register(r)) => Some(if byte {
            TraceSource::ByteRegister(r)
        } else {
            TraceSource::Register(r)
        }),
        Value::Operand(target) => {
            let (segment, address) = memory(target)?;
            segments |= 1 << segment as u8;
            Some(if byte {
                TraceSource::ByteMemory(address)
            } else {
                TraceSource::Memory(address)
            })
        }
    };
    let operation = match i.operation {
        Operation::Move {
            target: Target::Register(target),
            source: value,
        } => {
            let source = source(value, !i.dword)?;
            if i.dword {
                Op::Move { target, source }
            } else {
                Op::MoveByte { target, source }
            }
        }
        Operation::Move {
            target,
            source: value @ (Value::Immediate(_) | Value::Operand(Target::Register(_))),
        } => {
            let source = source(value, !i.dword)?;
            let (segment, address) = memory(target)?;
            write_segments |= 1 << segment as u8;
            if i.dword {
                Op::Store { address, source }
            } else {
                Op::StoreByte { address, source }
            }
        }
        Operation::Alu {
            target,
            source: value,
            operation,
            store: false,
        } if matches!(operation, 4 | 7) => Op::Compare {
            left: source(Value::Operand(target), !i.dword)?,
            right: source(value, !i.dword)?,
            operation,
            byte: !i.dword,
        },
        Operation::Alu {
            target: Target::Register(target),
            source: value,
            operation,
            store,
        } if i.dword => Op::Alu {
            target,
            source: source(value, false)?,
            operation,
            store,
        },
        Operation::Lea {
            register: target,
            target: address,
        } => Op::Lea {
            target,
            address: memory(address)?.1,
        },
        Operation::Inc {
            register,
            decrement,
        } if i.dword => Op::Inc {
            register,
            decrement,
        },
        Operation::Exchange {
            target: Target::Register(left),
            register: right,
        } if i.dword => Op::Exchange { left, right },
        Operation::Exchange { target, register } if i.dword => {
            let (segment, address) = memory(target)?;
            segments |= 1 << segment as u8;
            write_segments |= 1 << segment as u8;
            Op::ExchangeMemory { address, register }
        }
        Operation::Unary {
            target: Target::Register(register),
            negate,
        } if i.dword => Op::Unary { register, negate },
        Operation::Push(value) => {
            let source = source(value, false)?;
            write_segments |= 1 << SegmentRegister::Ss as u8;
            Op::Push { source }
        }
        Operation::Pop(register) => {
            segments |= 1 << SegmentRegister::Ss as u8;
            Op::Pop { register }
        }
        Operation::Call(displacement) => {
            write_segments |= 1 << SegmentRegister::Ss as u8;
            Op::Call {
                target: u32::from(offset)
                    .wrapping_add(u32::from(i.length))
                    .wrapping_add_signed(displacement),
            }
        }
        Operation::CallIndirect(value) => {
            let target = source(value, false)?;
            write_segments |= 1 << SegmentRegister::Ss as u8;
            Op::CallIndirect { target }
        }
        Operation::Return(release) => {
            segments |= 1 << SegmentRegister::Ss as u8;
            Op::Return { release }
        }
        Operation::Branch {
            condition,
            displacement,
        } => Op::Branch {
            condition,
            target: u32::from(offset)
                .wrapping_add(u32::from(i.length))
                .wrapping_add_signed(displacement),
        },
        _ => return None,
    };
    Some((
        TraceInstruction {
            offset,
            length: i.length,
            operation,
        },
        segments,
        write_segments,
    ))
}

/// Upper bound of one batch; the machine's event deadline usually ends it first.
pub(super) const BATCH_STEPS: u32 = 256;
const PLAIN_RAM: &str = "validated batch access and plain-RAM bus contract";

/// A validated operand: a register or a plain-RAM linear address.
#[derive(Clone, Copy)]
enum Place {
    Register(u8),
    Ram(u32),
}
#[derive(Clone, Copy)]
enum Source {
    Immediate(u32),
    Place(Place),
}

/// Data-segment windows resolved once per batch. Batched instructions cannot
/// load segment registers, so the hidden caches stay unchanged throughout.
#[derive(Default)]
pub(super) struct Windows([[Option<Option<SegmentWindow>>; 2]; 6]);
impl Windows {
    #[inline]
    fn linear<B: CpuBus>(
        &mut self,
        r: &Registers,
        bus: &B,
        segment: SegmentRegister,
        offset: u32,
        length: u8,
        access: Access,
    ) -> Option<u32> {
        let slot = &mut self.0[segment as usize][usize::from(access == Access::Write)];
        let window = *slot.get_or_insert_with(|| r.window(segment, access));
        let address = window?.address(offset, length)?;
        bus.is_plain_ram(address, length).then_some(address)
    }
    #[inline]
    fn place<B: CpuBus>(
        &mut self,
        r: &Registers,
        bus: &B,
        target: Target,
        length: u8,
        access: Access,
    ) -> Option<Place> {
        match target.resolve(r) {
            Operand::Register(code) => Some(Place::Register(code)),
            Operand::Memory { segment, offset } => self
                .linear(r, bus, segment, offset, length, access)
                .map(Place::Ram),
        }
    }
    #[inline]
    fn source<B: CpuBus>(
        &mut self,
        r: &Registers,
        bus: &B,
        value: Value,
        length: u8,
    ) -> Option<Source> {
        match value {
            Value::Immediate(value) => Some(Source::Immediate(value)),
            Value::Operand(target) => self
                .place(r, bus, target, length, Access::Read)
                .map(Source::Place),
        }
    }
}

impl Cpu {
    /// Execute a bounded run of decoded ordinary-RAM instructions without the
    /// native core. The machine checks pending IRQs first and supplies the next
    /// hardware deadline. I/O, MMIO, faultable operands and mode/IF changes stop
    /// before execution, so hardware and scheduling counters can advance once.
    pub fn interpret_batch<B: CpuBus>(
        &mut self,
        bus: &mut B,
        budget: u32,
        limit: impl FnOnce(&B) -> u32,
    ) -> u32 {
        if budget < 2 || !self.batch_ready() || !self.decode_cache.enabled {
            return 0;
        }
        let Some((code, mut windows)) = self.batch_context() else {
            return 0;
        };
        let budget = budget.min(BATCH_STEPS).min(limit(bus));
        let mut completed = 0;
        while completed < budget {
            let Some(instruction) = self.batch_lookup(code, bus) else {
                break;
            };
            if !self.try_batched(bus, &mut windows, code, instruction) {
                break;
            }
            completed += 1;
        }
        #[cfg(feature = "profiling")]
        {
            self.decode_cache.stats.batches += u64::from(completed != 0);
            self.decode_cache.stats.batched_steps += u64::from(completed);
        }
        completed
    }

    /// Validate every address an instruction uses, then perform it on those
    /// linear addresses. Returns false, with no effect, when any check fails:
    /// the reference step then reports faults and touches devices itself.
    /// Effects follow `execute_cached` in order and in flag computation.
    pub(super) fn try_batched<B: CpuBus>(
        &mut self,
        bus: &mut B,
        windows: &mut Windows,
        code: SegmentWindow,
        i: Instruction,
    ) -> bool {
        let r = &self.registers;
        let length = if i.dword { 4 } else { 1 };
        let next = r.ip.wrapping_add(u32::from(i.length));
        let jump = |displacement: i32| {
            let target = next.wrapping_add_signed(displacement);
            code.address(target, 1).map(|_| target)
        };
        let push_address = |windows: &mut Windows, bus: &B| {
            let sp = self.stack_pointer().wrapping_sub(4);
            let sp = if r.stack32() {
                sp
            } else {
                u32::from(sp as u16)
            };
            windows
                .linear(r, bus, SegmentRegister::Ss, sp, 4, Access::Write)
                .map(|address| (sp, address))
        };
        match i.operation {
            Operation::Move { target, source } => {
                let Some(source) = windows.source(r, bus, source, length) else {
                    return false;
                };
                let Some(target) = windows.place(r, bus, target, length, Access::Write) else {
                    return false;
                };
                let value = self.load_source(bus, source, i.dword);
                self.store(bus, target, i.dword, value);
            }
            Operation::Lea { register, target } => {
                let Operand::Memory { offset, .. } = target.resolve(r) else {
                    unreachable!()
                };
                self.registers.set32(Gpr32::decode(register), offset);
            }
            Operation::Alu {
                target,
                source,
                operation,
                store,
            } => {
                let Some(place) = windows.place(r, bus, target, length, Access::Read) else {
                    return false;
                };
                if store
                    && windows
                        .place(r, bus, target, length, Access::Write)
                        .is_none()
                {
                    return false;
                }
                let Some(source) = windows.source(r, bus, source, length) else {
                    return false;
                };
                let left = self.load(bus, place, i.dword);
                let right = self.load_source(bus, source, i.dword);
                let (value, new_flags) = if i.dword {
                    flags::alu32(self.flags, left, right, operation)
                } else {
                    let (value, flags) =
                        flags::alu(self.flags, left as u16, right as u16, operation, false);
                    (u32::from(value), flags)
                };
                self.flags = new_flags;
                if store {
                    self.store(bus, place, i.dword, value);
                }
            }
            Operation::Exchange { target, register } => {
                let Some(place) = windows.place(r, bus, target, length, Access::Read) else {
                    return false;
                };
                if windows
                    .place(r, bus, target, length, Access::Write)
                    .is_none()
                {
                    return false;
                }
                let other = Place::Register(register);
                let left = self.load(bus, place, i.dword);
                let right = self.load(bus, other, i.dword);
                self.store(bus, place, i.dword, right);
                self.store(bus, other, i.dword, left);
            }
            Operation::Inc {
                register,
                decrement,
            } => {
                let register = Gpr32::decode(register);
                let (value, next_flags) = flags::alu32(
                    self.flags,
                    self.registers.get32(register),
                    1,
                    if decrement { 5 } else { 0 },
                );
                self.flags = (next_flags & !flags::CARRY) | (self.flags & flags::CARRY);
                self.registers.set32(register, value);
            }
            Operation::Unary { target, negate } => {
                let Some(place) = windows.place(r, bus, target, 4, Access::Read) else {
                    return false;
                };
                if windows.place(r, bus, target, 4, Access::Write).is_none() {
                    return false;
                }
                let value = self.load(bus, place, true);
                let value = if negate {
                    let (value, new_flags) = flags::alu32(self.flags, 0, value, 5);
                    self.flags = new_flags;
                    value
                } else {
                    !value
                };
                self.store(bus, place, true, value);
            }
            Operation::Push(value) => {
                let Some(source) = windows.source(r, bus, value, 4) else {
                    return false;
                };
                let Some((sp, address)) = push_address(windows, bus) else {
                    return false;
                };
                let value = self.load_source(bus, source, true);
                self.store(bus, Place::Ram(address), true, value);
                self.set_stack_pointer(sp);
            }
            Operation::Pop(register) => {
                let sp = self.stack_pointer();
                let Some(address) =
                    windows.linear(r, bus, SegmentRegister::Ss, sp, 4, Access::Read)
                else {
                    return false;
                };
                let value = self.load(bus, Place::Ram(address), true);
                self.adjust_stack(4);
                self.registers.set32(Gpr32::decode(register), value);
            }
            Operation::Branch {
                condition,
                displacement,
            } => {
                if condition.is_none_or(|condition| flags::condition(self.flags, condition)) {
                    let Some(target) = jump(displacement) else {
                        return false;
                    };
                    self.registers.ip = target;
                    return true;
                }
            }
            Operation::Call(displacement) => {
                let Some(target) = jump(displacement) else {
                    return false;
                };
                let Some((sp, address)) = push_address(windows, bus) else {
                    return false;
                };
                self.store(bus, Place::Ram(address), true, next);
                self.set_stack_pointer(sp);
                self.registers.ip = target;
                return true;
            }
            Operation::CallIndirect(value) => {
                let Some(source) = windows.source(r, bus, value, 4) else {
                    return false;
                };
                let Some((sp, address)) = push_address(windows, bus) else {
                    return false;
                };
                let target = self.load_source(bus, source, true);
                if code.address(target, 1).is_none() {
                    return false;
                }
                self.store(bus, Place::Ram(address), true, next);
                self.set_stack_pointer(sp);
                self.registers.ip = target;
                return true;
            }
            Operation::Return(release) => {
                let sp = self.stack_pointer();
                let Some(address) =
                    windows.linear(r, bus, SegmentRegister::Ss, sp, 4, Access::Read)
                else {
                    return false;
                };
                let target = self.load(bus, Place::Ram(address), true);
                if code.address(target, 1).is_none() {
                    return false;
                }
                self.adjust_stack(4);
                self.registers.ip = target;
                self.adjust_stack(u32::from(release));
                return true;
            }
            Operation::Loop {
                kind,
                displacement,
                address32,
            } => {
                let count = self.address_register(Gpr16::Cx, address32);
                let take = if kind == 3 {
                    count == 0
                } else {
                    let count = if address32 {
                        count.wrapping_sub(1)
                    } else {
                        u32::from((count as u16).wrapping_sub(1))
                    };
                    count != 0 && (kind == 2 || (self.flags & flags::ZERO != 0) == (kind == 1))
                };
                if take && jump(displacement).is_none() {
                    return false;
                }
                return self.delegate_batched(bus, i);
            }
            Operation::String {
                kind,
                width,
                segment,
                address32,
                repeat,
            } => {
                if !repeat || self.address_register(Gpr16::Cx, address32) != 0 {
                    let si = self.address_register(Gpr16::Si, address32);
                    let di = self.address_register(Gpr16::Di, address32);
                    if kind != 0xAA
                        && windows
                            .linear(r, bus, segment, si, width, Access::Read)
                            .is_none()
                    {
                        return false;
                    }
                    if kind != 0xAC
                        && windows
                            .linear(r, bus, SegmentRegister::Es, di, width, Access::Write)
                            .is_none()
                    {
                        return false;
                    }
                }
                return self.delegate_batched(bus, i);
            }
            Operation::MulDiv { target, .. } => {
                if windows
                    .place(r, bus, target, length, Access::Read)
                    .is_none()
                {
                    return false;
                }
                return self.delegate_batched(bus, i);
            }
            Operation::Extend { .. } => return self.delegate_batched(bus, i),
            Operation::MoveExtend { source, word, .. } => {
                let width = if word { 2 } else { 1 };
                if windows.place(r, bus, source, width, Access::Read).is_none() {
                    return false;
                }
                return self.delegate_batched(bus, i);
            }
            Operation::Input | Operation::Output | Operation::Unsupported => return false,
        }
        self.registers.ip = next;
        true
    }

    /// Execute a validated instruction through the reference-equivalent cached
    /// path. Only a divide error remains possible; it has no effect except IP,
    /// which is restored so the reference step reports it.
    fn delegate_batched<B: CpuBus>(&mut self, bus: &mut B, i: Instruction) -> bool {
        let ip = self.registers.ip;
        if self.execute_cached(bus, i).is_err() {
            self.registers.ip = ip;
            return false;
        }
        true
    }

    fn load<B: CpuBus>(&self, bus: &mut B, place: Place, dword: bool) -> u32 {
        match place {
            Place::Register(code) if dword => self.registers.get32(Gpr32::decode(code)),
            Place::Register(code) => u32::from(self.registers.get_byte(code)),
            Place::Ram(address) if dword => bus.read_u32(address).expect(PLAIN_RAM),
            Place::Ram(address) => u32::from(bus.read_u8(address).expect(PLAIN_RAM)),
        }
    }

    fn load_source<B: CpuBus>(&self, bus: &mut B, source: Source, dword: bool) -> u32 {
        match source {
            Source::Immediate(value) => value,
            Source::Place(place) => self.load(bus, place, dword),
        }
    }

    fn store<B: CpuBus>(&mut self, bus: &mut B, place: Place, dword: bool, value: u32) {
        match place {
            Place::Register(code) if dword => self.registers.set32(Gpr32::decode(code), value),
            Place::Register(code) => self.registers.set_byte(code, value as u8),
            Place::Ram(address) if dword => bus.write_u32(address, value).expect(PLAIN_RAM),
            Place::Ram(address) => bus.write_u8(address, value as u8).expect(PLAIN_RAM),
        }
    }

    /// The CS execute window and empty data windows for one slice.
    pub(super) fn batch_context(&mut self) -> Option<(SegmentWindow, Windows)> {
        let (code, _) = self.decode_cache.code_context(&self.registers)?;
        Some((code, Windows::default()))
    }

    /// Look up the current instruction; batched instructions cannot change the
    /// CS context resolved by `batch_context`.
    #[inline]
    pub(super) fn batch_lookup<B: CpuBus>(
        &mut self,
        code: SegmentWindow,
        bus: &B,
    ) -> Option<Instruction> {
        self.decode_cache
            .instruction_in(code, self.registers.ip, bus)
    }

    /// The per-step checks of `step` are invariant across a batch: batched
    /// instructions cannot set TF, change CR0, load segments or open a shadow.
    fn batch_ready(&self) -> bool {
        !self.halted
            && self.interrupt_shadow == 0
            && self.flags & (flags::TRAP | flags::RESUME | flags::VIRTUAL) == 0
            && self.control.cr0 & 0x8000_0000 == 0
            && self.registers.protected
            && self.protected_mode()
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl Reader<'_> {
    fn byte(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.position)?;
        self.position += 1;
        Some(byte)
    }
    fn word(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes([self.byte()?, self.byte()?]))
    }
    fn dword(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes([
            self.byte()?,
            self.byte()?,
            self.byte()?,
            self.byte()?,
        ]))
    }
    fn immediate(&mut self, dword: bool) -> Option<u32> {
        if dword {
            self.dword()
        } else {
            self.byte().map(u32::from)
        }
    }
    fn target(
        &mut self,
        modrm: u8,
        address32: bool,
        override_segment: Option<SegmentRegister>,
    ) -> Option<Target> {
        let mode = modrm >> 6;
        let code = modrm & 7;
        if mode == 3 {
            return Some(Target::Register(code));
        }
        let (base, index, scale, mut displacement, stack) = if address32 {
            if code == 4 {
                let sib = self.byte()?;
                let base = sib & 7;
                let index = (sib >> 3) & 7;
                let (base, displacement, stack) = if base == 5 && mode == 0 {
                    (NO_REGISTER, self.dword()?, false)
                } else {
                    (base, 0, matches!(base, 4 | 5))
                };
                (
                    base,
                    if index == 4 { NO_REGISTER } else { index },
                    sib >> 6,
                    displacement,
                    stack,
                )
            } else if code == 5 && mode == 0 {
                (NO_REGISTER, NO_REGISTER, 0, self.dword()?, false)
            } else {
                (code, NO_REGISTER, 0, 0, code == 5)
            }
        } else {
            let (base, index, stack) = match code {
                0 => (3, 6, false),
                1 => (3, 7, false),
                2 => (5, 6, true),
                3 => (5, 7, true),
                4 => (6, NO_REGISTER, false),
                5 => (7, NO_REGISTER, false),
                6 if mode == 0 => (NO_REGISTER, NO_REGISTER, false),
                6 => (5, NO_REGISTER, true),
                _ => (3, NO_REGISTER, false),
            };
            (
                base,
                index,
                0,
                if mode == 0 && code == 6 {
                    u32::from(self.word()?)
                } else {
                    0
                },
                stack,
            )
        };
        displacement = displacement.wrapping_add(match mode {
            1 => self.byte()? as i8 as i32 as u32,
            2 if address32 => self.dword()?,
            2 => u32::from(self.word()?),
            _ => 0,
        });
        Some(Target::Memory {
            base,
            index,
            scale,
            displacement,
            address32,
            segment: override_segment.unwrap_or(if stack {
                SegmentRegister::Ss
            } else {
                SegmentRegister::Ds
            }),
        })
    }
}
impl Instruction {
    /// One byte of offset range: the reference path reports its own faults.
    const UNSUPPORTED: Self = Self {
        length: 1,
        read_length: 1,
        dword: false,
        operation: Operation::Unsupported,
    };

    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader { bytes, position: 0 };
        let mut operand32 = true;
        let mut address32 = true;
        let mut segment = None;
        let mut repeat = false;
        let opcode = loop {
            match r.byte()? {
                0x66 => operand32 = false,
                0x67 => address32 = false,
                0xF2 | 0xF3 => repeat = true,
                byte @ (0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65) => {
                    segment = Some(match byte {
                        0x26 => SegmentRegister::Es,
                        0x2E => SegmentRegister::Cs,
                        0x36 => SegmentRegister::Ss,
                        0x64 => SegmentRegister::Fs,
                        0x65 => SegmentRegister::Gs,
                        _ => SegmentRegister::Ds,
                    })
                }
                byte => break byte,
            }
        };
        if matches!(opcode, 0xA4 | 0xA5 | 0xAA | 0xAB | 0xAC | 0xAD) {
            // REPNE repeats MOVS/STOS/LODS exactly like REP.
            let width = if opcode & 1 == 0 {
                1
            } else if operand32 {
                4
            } else {
                2
            };
            if r.position > 15 {
                return None;
            }
            return Some(Self {
                length: r.position as u8,
                read_length: r.position as u8,
                dword: width == 4,
                operation: Operation::String {
                    kind: opcode & 0xFE,
                    width,
                    segment: segment.unwrap_or(SegmentRegister::Ds),
                    address32,
                    repeat,
                },
            });
        }
        // Other word operands retain the reference path.
        if !operand32 {
            return None;
        }
        let mut dword = opcode & 1 != 0;
        let mut read_length = None;
        let operation = match opcode {
            0x88..=0x8B | 0x8D | 0xC6 | 0xC7 | 0x80 | 0x81 | 0x83 | 0x84..=0x87 => {
                let modrm = r.byte()?;
                let target = r.target(modrm, address32, segment)?;
                let register = (modrm >> 3) & 7;
                let source = Value::Operand(Target::Register(register));
                match opcode {
                    0x88 | 0x89 => Operation::Move { target, source },
                    0x8A | 0x8B => Operation::Move {
                        target: Target::Register(register),
                        source: Value::Operand(target),
                    },
                    0x8D if matches!(target, Target::Memory { .. }) => {
                        Operation::Lea { register, target }
                    }
                    0xC6 | 0xC7 if register == 0 => Operation::Move {
                        target,
                        source: Value::Immediate(r.immediate(dword)?),
                    },
                    0x86 | 0x87 => Operation::Exchange { target, register },
                    0x80 | 0x81 | 0x83 | 0x84 | 0x85 => {
                        if matches!(opcode, 0x81 | 0x83) {
                            read_length = Some(r.position as u8);
                        }
                        let source = match opcode {
                            0x80 | 0x81 => Value::Immediate(r.immediate(dword)?),
                            0x83 => Value::Immediate(r.byte()? as i8 as i32 as u32),
                            _ => source,
                        };
                        let operation = if opcode >= 0x84 { 4 } else { register };
                        Operation::Alu {
                            target,
                            source,
                            operation,
                            store: opcode < 0x84 && operation != 7,
                        }
                    }
                    _ => return None,
                }
            }
            code if code <= 0x3D && code & 7 <= 5 => {
                let operation = code >> 3;
                let (target, source) = if code & 7 >= 4 {
                    (Target::Register(0), Value::Immediate(r.immediate(dword)?))
                } else {
                    let modrm = r.byte()?;
                    let rm = r.target(modrm, address32, segment)?;
                    let reg = Target::Register((modrm >> 3) & 7);
                    if code & 2 != 0 {
                        (reg, Value::Operand(rm))
                    } else {
                        (rm, Value::Operand(reg))
                    }
                };
                Operation::Alu {
                    target,
                    source,
                    operation,
                    store: operation != 7,
                }
            }
            0x40..=0x4F => {
                dword = true;
                Operation::Inc {
                    register: opcode & 7,
                    decrement: opcode >= 0x48,
                }
            }
            0x50..=0x57 => Operation::Push(Value::Operand(Target::Register(opcode & 7))),
            0x58..=0x5F => Operation::Pop(opcode & 7),
            0x68 => Operation::Push(Value::Immediate(r.dword()?)),
            0x6A => Operation::Push(Value::Immediate(r.byte()? as i8 as i32 as u32)),
            0x90..=0x97 => {
                dword = true;
                Operation::Exchange {
                    target: Target::Register(0),
                    register: opcode & 7,
                }
            }
            0xA0..=0xA3 => {
                let displacement = if address32 {
                    r.dword()?
                } else {
                    u32::from(r.word()?)
                };
                let memory = Target::Memory {
                    segment: segment.unwrap_or(SegmentRegister::Ds),
                    base: NO_REGISTER,
                    index: NO_REGISTER,
                    scale: 0,
                    displacement,
                    address32,
                };
                let (target, source) = if opcode < 0xA2 {
                    (Target::Register(0), Value::Operand(memory))
                } else {
                    (memory, Value::Operand(Target::Register(0)))
                };
                Operation::Move { target, source }
            }
            0xA8 | 0xA9 => Operation::Alu {
                target: Target::Register(0),
                source: Value::Immediate(r.immediate(dword)?),
                operation: 4,
                store: false,
            },
            0xB0..=0xBF => {
                dword = opcode >= 0xB8;
                Operation::Move {
                    target: Target::Register(opcode & 7),
                    source: Value::Immediate(r.immediate(dword)?),
                }
            }
            0x70..=0x7F | 0xEB => Operation::Branch {
                condition: if opcode == 0xEB {
                    None
                } else {
                    Some(opcode & 15)
                },
                displacement: i32::from(r.byte()? as i8),
            },
            0x0F => {
                let second = r.byte()?;
                if matches!(second, 0xB6 | 0xB7 | 0xBE | 0xBF) {
                    let modrm = r.byte()?;
                    dword = true;
                    Operation::MoveExtend {
                        register: (modrm >> 3) & 7,
                        source: r.target(modrm, address32, segment)?,
                        word: second & 1 != 0,
                        signed: second & 8 != 0,
                    }
                } else if (0x80..=0x8F).contains(&second) {
                    Operation::Branch {
                        condition: Some(second & 15),
                        displacement: r.dword()? as i32,
                    }
                } else {
                    return None;
                }
            }
            0xE0..=0xE3 => Operation::Loop {
                kind: opcode & 3,
                displacement: i32::from(r.byte()? as i8),
                address32,
            },
            0x98 | 0x99 => {
                dword = true;
                Operation::Extend {
                    cdq: opcode == 0x99,
                }
            }
            0xE8 => Operation::Call(r.dword()? as i32),
            0xE9 => Operation::Branch {
                condition: None,
                displacement: r.dword()? as i32,
            },
            0xC2 => Operation::Return(r.word()?),
            0xC3 => Operation::Return(0),
            0xFF => {
                let modrm = r.byte()?;
                let target = r.target(modrm, address32, segment)?;
                match (modrm >> 3) & 7 {
                    2 => Operation::CallIndirect(Value::Operand(target)),
                    6 => Operation::Push(Value::Operand(target)),
                    _ => return None,
                }
            }
            0xF6 | 0xF7 => {
                let modrm = r.byte()?;
                let target = r.target(modrm, address32, segment)?;
                match (modrm >> 3) & 7 {
                    0 => {
                        read_length = Some(r.position as u8);
                        Operation::Alu {
                            target,
                            source: Value::Immediate(r.immediate(dword)?),
                            operation: 4,
                            store: false,
                        }
                    }
                    2 | 3 if dword => Operation::Unary {
                        target,
                        negate: modrm & 0x38 == 0x18,
                    },
                    operation @ 4..=7 => Operation::MulDiv { target, operation },
                    _ => return None,
                }
            }
            0xEC => Operation::Input,
            0xEE => Operation::Output,
            _ => return None,
        };
        if r.position > 15 {
            return None;
        }
        Some(Self {
            length: r.position as u8,
            read_length: read_length.unwrap_or(r.position as u8),
            dword,
            operation,
        })
    }
}

/// Share decoding with the native backend; unsupported or faultable operations
/// terminate a native block before any guest state changes.
#[cfg(all(
    feature = "dynamic-core",
    not(target_arch = "wasm32"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub(super) fn native_instruction(
    bytes: &[u8],
    block_offset: u8,
) -> Option<doslet_x86_jit::Instruction> {
    use doslet_x86_jit::{Instruction as Native, Operation as Op, Source};
    let i = Instruction::decode(bytes)?;
    // Branches have no byte/dword data operand. For short Jcc the decoder's
    // generic width bit follows opcode bit 0, so both values must be accepted.
    // The shared decoder has already rejected operand-size override 66h.
    if !i.dword && !matches!(i.operation, Operation::Branch { .. }) {
        return None;
    }
    let source = |v| match v {
        Value::Immediate(v) => Some(Source::Immediate(v)),
        Value::Operand(Target::Register(r)) => Some(Source::Register(r)),
        _ => None,
    };
    let operation = match i.operation {
        Operation::Move {
            target: Target::Register(target),
            source: s,
        } => Op::Move {
            target,
            source: source(s)?,
        },
        Operation::Alu {
            target: Target::Register(target),
            source: s,
            operation,
            store,
        } => Op::Alu {
            target,
            source: source(s)?,
            operation,
            store,
        },
        Operation::Inc {
            register,
            decrement,
        } => Op::Inc {
            register,
            decrement,
        },
        Operation::Exchange {
            target: Target::Register(left),
            register: right,
        } => Op::Exchange { left, right },
        Operation::Unary {
            target: Target::Register(register),
            negate,
        } => Op::Unary { register, negate },
        Operation::Branch {
            condition,
            displacement,
        } if block_offset != 0
            && i64::from(block_offset) + i64::from(i.length) + i64::from(displacement) == 0 =>
        {
            Op::LoopBranch { condition }
        }
        _ => return None,
    };
    Some(Native {
        length: i.length,
        operation,
    })
}
impl Cpu {
    pub(super) fn cached_step<B: CpuBus>(
        &mut self,
        bus: &mut B,
    ) -> Option<Result<StepOutcome, CpuError>> {
        let instruction = self.decode_cache.instruction(&self.registers, bus)?;
        Some(self.execute_cached(bus, instruction))
    }
    fn execute_cached<B: CpuBus>(
        &mut self,
        bus: &mut B,
        instruction: Instruction,
    ) -> Result<StepOutcome, CpuError> {
        let ip = self.registers.ip;
        let next = ip.wrapping_add(u32::from(instruction.length));
        self.registers.ip = ip.wrapping_add(u32::from(instruction.read_length));
        let dword = instruction.dword;
        match instruction.operation {
            Operation::Move { target, source } => {
                let value = source.read(self, bus, dword)?;
                write(target.resolve(&self.registers), self, bus, dword, value)?;
            }
            Operation::Lea { register, target } => {
                let Operand::Memory { offset, .. } = target.resolve(&self.registers) else {
                    unreachable!()
                };
                self.registers.set32(Gpr32::decode(register), offset);
            }
            Operation::Alu {
                target,
                source,
                operation,
                store,
            } => {
                let target = target.resolve(&self.registers);
                let left = read(target, self, bus, dword)?;
                self.registers.ip = next;
                let right = source.read(self, bus, dword)?;
                let (value, new_flags) = if dword {
                    flags::alu32(self.flags, left, right, operation)
                } else {
                    let (value, flags) =
                        flags::alu(self.flags, left as u16, right as u16, operation, false);
                    (u32::from(value), flags)
                };
                self.flags = new_flags;
                if store {
                    write(target, self, bus, dword, value)?;
                }
            }
            Operation::Exchange { target, register } => {
                let target = target.resolve(&self.registers);
                let source = Operand::Register(register);
                let left = read(target, self, bus, dword)?;
                let right = read(source, self, bus, dword)?;
                write(target, self, bus, dword, right)?;
                write(source, self, bus, dword, left)?;
            }
            Operation::Inc {
                register,
                decrement,
            } => {
                let register = Gpr32::decode(register);
                let (value, next_flags) = flags::alu32(
                    self.flags,
                    self.registers.get32(register),
                    1,
                    if decrement { 5 } else { 0 },
                );
                self.flags = (next_flags & !flags::CARRY) | (self.flags & flags::CARRY);
                self.registers.set32(register, value);
            }
            Operation::Unary { target, negate } => {
                let target = target.resolve(&self.registers);
                let value = read(target, self, bus, true)?;
                let value = if negate {
                    let (value, new_flags) = flags::alu32(self.flags, 0, value, 5);
                    self.flags = new_flags;
                    value
                } else {
                    !value
                };
                write(target, self, bus, true, value)?;
            }
            Operation::Push(value) => {
                let value = value.read(self, bus, true)?;
                self.push32(bus, value)?;
            }
            Operation::Pop(register) => {
                let value = self.pop32(bus)?;
                self.registers.set32(Gpr32::decode(register), value);
            }
            Operation::Branch {
                condition,
                displacement,
            } => {
                if condition.is_none_or(|condition| flags::condition(self.flags, condition)) {
                    self.near_jump(next.wrapping_add_signed(displacement))?;
                }
            }
            Operation::Call(displacement) => {
                self.near_jump(next.wrapping_add_signed(displacement))?;
                self.push32(bus, next)?;
            }
            Operation::CallIndirect(target) => {
                // FF /2 reads the target before pushing, and validates the
                // destination after the push, matching the reference's faults.
                let target = target.read(self, bus, true)?;
                self.push32(bus, next)?;
                self.near_jump(target)?;
            }
            Operation::Return(release) => {
                let target = self.pop32(bus)?;
                self.near_jump(target)?;
                self.adjust_stack(u32::from(release));
            }
            Operation::Input => {
                let value = bus.read_port_u8(self.registers.get32(Gpr32::Edx) as u16)?;
                self.registers.set_byte(0, value);
            }
            Operation::Output => bus.write_port_u8(
                self.registers.get32(Gpr32::Edx) as u16,
                self.registers.get_byte(0),
            )?,
            Operation::Loop {
                kind,
                displacement,
                address32,
            } => {
                self.registers.ip = next;
                let take = if kind == 3 {
                    self.address_register(Gpr16::Cx, address32) == 0
                } else {
                    let count = self.address_register(Gpr16::Cx, address32).wrapping_sub(1);
                    self.set_address_register(Gpr16::Cx, count, address32);
                    let count = self.address_register(Gpr16::Cx, address32);
                    count != 0 && (kind == 2 || (self.flags & flags::ZERO != 0) == (kind == 1))
                };
                if take {
                    self.near_jump(next.wrapping_add_signed(displacement))?;
                }
            }
            Operation::String {
                kind,
                width,
                segment,
                address32,
                repeat,
            } => {
                self.registers.ip = next;
                let count = self.address_register(Gpr16::Cx, address32);
                if repeat && count == 0 {
                    return Ok(StepOutcome::Continue);
                }
                let si = self.address_register(Gpr16::Si, address32);
                let di = self.address_register(Gpr16::Di, address32);
                let source = Operand::Memory {
                    segment,
                    offset: si,
                };
                let target = Operand::Memory {
                    segment: SegmentRegister::Es,
                    offset: di,
                };
                match kind {
                    0xA4 => {
                        let value = read_width(source, self, bus, width)?;
                        write_width(target, self, bus, width, value)?;
                    }
                    0xAA => {
                        let value = read_width(Operand::Register(0), self, bus, width)?;
                        write_width(target, self, bus, width, value)?;
                    }
                    _ => {
                        let value = read_width(source, self, bus, width)?;
                        write_width(Operand::Register(0), self, bus, width, value)?;
                    }
                }
                let amount = i32::from(width);
                let delta = if self.flags & flags::DIRECTION == 0 {
                    amount
                } else {
                    -amount
                };
                if kind != 0xAA {
                    self.set_address_register(Gpr16::Si, si.wrapping_add_signed(delta), address32);
                }
                if kind != 0xAC {
                    self.set_address_register(Gpr16::Di, di.wrapping_add_signed(delta), address32);
                }
                if repeat {
                    self.set_address_register(Gpr16::Cx, count - 1, address32);
                    if count > 1 {
                        self.registers.ip = ip;
                    }
                }
            }
            Operation::MulDiv { target, operation } => {
                self.registers.ip = next;
                let value = read(target.resolve(&self.registers), self, bus, dword)?;
                self.multiply_divide(operation, value, dword, ip)?;
            }
            Operation::Extend { cdq } => {
                self.registers.ip = next;
                if cdq {
                    let sign = self.registers.get32(Gpr32::Eax) & 0x8000_0000 != 0;
                    self.registers
                        .set32(Gpr32::Edx, if sign { u32::MAX } else { 0 });
                } else {
                    let value = self.registers.get(Gpr16::Ax) as i16 as i32 as u32;
                    self.registers.set32(Gpr32::Eax, value);
                }
            }
            Operation::MoveExtend {
                register,
                source,
                word,
                signed,
            } => {
                self.registers.ip = next;
                let value = source
                    .resolve(&self.registers)
                    .read(&self.registers, bus, word)?;
                let value = match (signed, word) {
                    (true, true) => value as i16 as i32 as u32,
                    (true, false) => value as u8 as i8 as i32 as u32,
                    _ => u32::from(value),
                };
                self.registers.set32(Gpr32::decode(register), value);
            }
            Operation::Unsupported => unreachable!("lookups return None for rejections"),
        }
        Ok(StepOutcome::Continue)
    }

    /// F6/F7 /4-/7 on AL/AX or EAX, as the byte and dword reference paths.
    fn multiply_divide(
        &mut self,
        operation: u8,
        value: u32,
        dword: bool,
        ip: u32,
    ) -> Result<(), CpuError> {
        let divide_error = CpuError::DivideError {
            cs: self.registers.cs,
            ip,
        };
        if dword {
            if operation < 6 {
                let accumulator = self.registers.get32(Gpr32::Eax);
                let (product, overflow) = if operation == 4 {
                    let product = u64::from(accumulator) * u64::from(value);
                    (product, product >> 32 != 0)
                } else {
                    let product = i64::from(accumulator as i32) * i64::from(value as i32);
                    (product as u64, product != i64::from(product as i32))
                };
                self.registers.set32(Gpr32::Eax, product as u32);
                self.registers.set32(Gpr32::Edx, (product >> 32) as u32);
                self.set_multiply_overflow(overflow);
                return Ok(());
            }
            let dividend = (u64::from(self.registers.get32(Gpr32::Edx)) << 32)
                | u64::from(self.registers.get32(Gpr32::Eax));
            let result = if operation == 6 {
                dividend
                    .checked_div(u64::from(value))
                    .filter(|quotient| *quotient <= u64::from(u32::MAX))
                    .map(|quotient| (quotient as u32, (dividend % u64::from(value)) as u32))
            } else {
                (dividend as i64)
                    .checked_div(i64::from(value as i32))
                    .filter(|quotient| i32::try_from(*quotient).is_ok())
                    .map(|quotient| {
                        (
                            quotient as u32,
                            ((dividend as i64) % i64::from(value as i32)) as u32,
                        )
                    })
            };
            let (quotient, remainder) = result.ok_or(divide_error)?;
            self.registers.set32(Gpr32::Eax, quotient);
            self.registers.set32(Gpr32::Edx, remainder);
            return Ok(());
        }
        let ax = self.registers.get(Gpr16::Ax);
        let value = value as u8;
        if operation < 6 {
            let product = if operation == 4 {
                u32::from(ax as u8) * u32::from(value)
            } else {
                (i32::from(ax as u8 as i8) * i32::from(value as i8)) as u32
            };
            let overflow = if operation == 4 {
                product > 0xFF
            } else {
                product as i32 != i32::from(product as i8)
            };
            self.registers.set(Gpr16::Ax, product as u16);
            self.set_multiply_overflow(overflow);
            return Ok(());
        }
        let (dividend, divisor) = if operation == 7 {
            (i64::from(ax as i16), i64::from(value as i8))
        } else {
            (i64::from(ax), i64::from(value))
        };
        if divisor == 0 {
            return Err(divide_error);
        }
        let quotient = dividend / divisor;
        let remainder = dividend % divisor;
        let range = if operation == 7 { -128..=127 } else { 0..=255 };
        if !range.contains(&quotient) {
            return Err(divide_error);
        }
        self.registers.set_byte(0, quotient as u8);
        self.registers.set_byte(4, remainder as u8);
        Ok(())
    }

    fn set_multiply_overflow(&mut self, overflow: bool) {
        self.flags = (self.flags & !(flags::CARRY | flags::OVERFLOW))
            | if overflow {
                flags::CARRY | flags::OVERFLOW
            } else {
                0
            };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Gpr32;
    use doslet_bus::{BusError, MemoryBus, PortBus};

    /// Flat RAM whose single write generation validates every code span.
    struct Flat {
        ram: [u8; 0x200],
        generation: u64,
    }
    impl MemoryBus for Flat {
        fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
            let byte = self.ram.get(address as usize);
            byte.copied().ok_or(BusError::UnmappedMemory(address))
        }
        fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
            let byte = self.ram.get_mut(address as usize);
            *byte.ok_or(BusError::UnmappedMemory(address))? = value;
            self.generation += 1;
            Ok(())
        }
        fn code_check(&self, address: u32, length: u8) -> Option<CodeCheck> {
            (address as usize + usize::from(length) <= self.ram.len()).then_some(CodeCheck {
                space: 1 << 32,
                pages: [0; 2],
                generations: [self.generation; 2],
            })
        }
        fn code_valid(&self, check: &CodeCheck) -> bool {
            check.generations[0] == self.generation
        }
        fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
            let start = address as usize;
            self.ram
                .get(start..(start + usize::from(length)).min(self.ram.len()))
        }
    }
    impl PortBus for Flat {
        fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
            Err(BusError::UnmappedPort(port))
        }
        fn write_port_u8(&mut self, port: u16, _: u8) -> Result<(), BusError> {
            Err(BusError::UnmappedPort(port))
        }
    }

    #[test]
    fn exhausted_cs_lineage_or_version_resolves_the_context_every_step() {
        for exhausted in [
            |r: &mut Registers| r.cs_lineage = 0,
            |r: &mut Registers| r.cs_version = u64::MAX,
        ] {
            let mut bus = Flat {
                ram: [0; 0x200],
                generation: 0,
            };
            bus.ram[..5].copy_from_slice(&[0xB8, 1, 0, 0, 0]);
            bus.ram[0x100..0x105].copy_from_slice(&[0xB8, 2, 0, 0, 0]);
            let mut cpu = Cpu::default();
            cpu.control.cr0 |= 1;
            let r = &mut cpu.registers;
            r.sync_mode(true);
            r.cs = 8;
            r.caches[SegmentRegister::Cs as usize] = SegmentCache {
                selector: 8,
                base: 0,
                limit: 0x1FF,
                access: 0x9B,
                flags: 0x40,
                default32: true,
                usable: true,
            };
            exhausted(r);
            cpu.step(&mut bus).unwrap();
            assert_eq!(cpu.registers.get32(Gpr32::Eax), 1);
            // Rebase CS without a version change; only a full check notices.
            cpu.registers.caches[SegmentRegister::Cs as usize].base = 0x100;
            cpu.registers.caches[SegmentRegister::Cs as usize].limit = 0xFF;
            cpu.registers.ip = 0;
            cpu.step(&mut bus).unwrap();
            assert_eq!(cpu.registers.get32(Gpr32::Eax), 2);
        }
    }
}
