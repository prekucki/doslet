//! Single-instruction cache. Unsupported forms use the original interpreter.
use super::{Cpu, CpuError, StepOutcome};
use crate::state::SegmentWindow;
use crate::{Access, Gpr32, Registers, SegmentCache, SegmentRegister, decode::Operand, flags};
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
    /// Full context check; entries survive only an unchanged 32-bit context.
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
                let removed = self.entries.iter().filter(|e| e.is_some()).count() as u64;
                self.stats.context_invalidations += removed;
                self.stats.invalidations += removed;
            }
            self.clear();
            self.code32 = true;
            self.context = Some(context);
            // The context fixes every CS check except the instruction's offset range.
            self.window = r.window(SegmentRegister::Cs, Access::Execute);
        }
    }
    fn instruction<B: CpuBus>(&mut self, r: &Registers, bus: &B) -> Option<Instruction> {
        if !self.enabled {
            return None;
        }
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
        let window = self.window?;
        let address = window.address(r.ip, 1)?;
        let index = ((address ^ (address >> 9)) as usize) & (CAPACITY - 1);
        if let Some(entry) = self.entries[index]
            && entry.address == address
        {
            let segment_valid = window.address(r.ip, entry.instruction.length).is_some();
            if segment_valid && bus.code_valid(&entry.check) {
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
            return None;
        };
        // Failed probes leave registers/bus untouched. Let byte fetching report
        // the original fault and partial IP progress at a code-segment boundary.
        window.address(r.ip, instruction.length)?;
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
    Return(u16),
    Input,
    Output,
}
#[derive(Clone, Copy)]
struct Instruction {
    length: u8,
    /// Position before a faultable data read that precedes immediate fetching.
    read_length: u8,
    dword: bool,
    operation: Operation,
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
    fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader { bytes, position: 0 };
        let mut operand32 = true;
        let mut address32 = true;
        let mut segment = None;
        let opcode = loop {
            match r.byte()? {
                0x66 => operand32 = false,
                0x67 => address32 = false,
                0xF2 | 0xF3 => {}
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
        // Word operands retain the reference path in this initial experiment.
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
                if !(0x80..=0x8F).contains(&second) {
                    return None;
                }
                Operation::Branch {
                    condition: Some(second & 15),
                    displacement: r.dword()? as i32,
                }
            }
            0xE8 => Operation::Call(r.dword()? as i32),
            0xE9 => Operation::Branch {
                condition: None,
                displacement: r.dword()? as i32,
            },
            0xC2 => Operation::Return(r.word()?),
            0xC3 => Operation::Return(0),
            0xF7 => {
                let modrm = r.byte()?;
                let target = r.target(modrm, address32, segment)?;
                match (modrm >> 3) & 7 {
                    0 => {
                        read_length = Some(r.position as u8);
                        Operation::Alu {
                            target,
                            source: Value::Immediate(r.dword()?),
                            operation: 4,
                            store: false,
                        }
                    }
                    2 | 3 => Operation::Unary {
                        target,
                        negate: modrm & 0x38 == 0x18,
                    },
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
        }
        Ok(StepOutcome::Continue)
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
