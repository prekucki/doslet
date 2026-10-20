//! Bounded, straight-line predicate provenance. Byte identities preserve GPR
//! aliases; dependencies distinguish a repeated external read from an induction
//! variable. Symbolic address equality permits disjoint local temporaries,
//! without assuming that different segment registers have different bases.

use iced_x86::{Instruction, Mnemonic, OpKind, Register};

use super::{MAX_LOOP_INSTRUCTIONS, WaitEvidence};

const REG_BYTES: usize = 8 * 4;
const CONSTANT: u32 = 0x100;
const ZERO: Byte = Byte {
    identity: CONSTANT,
    deps: Dependencies::EMPTY,
};

#[derive(Clone, Copy, Default)]
struct Dependencies {
    reads: u32,
    inputs: u32,
    unknown: bool,
}

impl Dependencies {
    const EMPTY: Self = Self {
        reads: 0,
        inputs: 0,
        unknown: false,
    };

    fn merge(self, other: Self) -> Self {
        Self {
            reads: self.reads | other.reads,
            inputs: self.inputs | other.inputs,
            unknown: self.unknown || other.unknown,
        }
    }
}

#[derive(Clone, Copy)]
struct Byte {
    identity: u32,
    deps: Dependencies,
}

#[derive(Clone, Copy)]
struct Value {
    bytes: [Byte; 4],
    width: usize,
}

impl Value {
    fn constant(value: u32, width: usize) -> Self {
        Self {
            bytes: value.to_le_bytes().map(|byte| Byte {
                identity: CONSTANT + u32::from(byte),
                deps: Dependencies::EMPTY,
            }),
            width,
        }
    }

    fn constant_value(self) -> Option<u32> {
        let mut value = 0;
        for (index, byte) in self.bytes[..self.width].iter().enumerate() {
            if !(CONSTANT..CONSTANT + 256).contains(&byte.identity) {
                return None;
            }
            value |= (byte.identity - CONSTANT) << (index * 8);
        }
        Some(value)
    }

    fn deps(self) -> Dependencies {
        self.bytes[..self.width]
            .iter()
            .fold(Dependencies::EMPTY, |deps, byte| deps.merge(byte.deps))
    }

    fn same(self, other: Self) -> bool {
        self.width == other.width
            && self.bytes[..self.width]
                .iter()
                .zip(&other.bytes[..other.width])
                .all(|(first, second)| first.identity == second.identity)
    }
}

#[derive(Clone, Copy)]
struct Address {
    segment: Register,
    bits: u8,
    base: [u32; 4],
    index: [u32; 4],
    scale: u32,
    displacement: u32,
    deps: Dependencies,
}

#[derive(Clone, Copy)]
struct Access {
    address: Address,
    width: usize,
}

#[derive(Clone, Copy)]
struct Store {
    access: Access,
    value: Value,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Alias {
    Disjoint,
    Overlap,
    Unknown,
}

impl Access {
    fn alias(self, other: Self) -> Alias {
        let first = self.address;
        let second = other.address;
        // A declared flat CS does not declare the loaded DS, SS, ES, FS or GS.
        // Their offsets alone cannot establish linear disjointness.
        if first.segment != second.segment {
            return Alias::Unknown;
        }
        if first.base != second.base
            || first.index != second.index
            || first.scale != second.scale
            || (first.bits != second.bits && (first.base != [0; 4] || first.index != [0; 4]))
        {
            return Alias::Unknown;
        }
        let mask = if first.bits == 16 && second.bits == 16 {
            0xffff
        } else {
            u32::MAX
        };
        // Modular distances conservatively include wraparound aliases and the
        // complete 1/2/4-byte access widths, not just equal starting offsets.
        let forward = second.displacement.wrapping_sub(first.displacement) & mask;
        let reverse = first.displacement.wrapping_sub(second.displacement) & mask;
        if forward < self.width as u32 || reverse < other.width as u32 {
            Alias::Overlap
        } else {
            Alias::Disjoint
        }
    }

    fn same(self, other: Self) -> bool {
        self.width == other.width
            && self.address.segment == other.address.segment
            && self.address.bits == other.address.bits
            && self.address.base == other.address.base
            && self.address.index == other.address.index
            && self.address.scale == other.address.scale
            && self.address.displacement == other.address.displacement
    }
}

pub(super) struct Analysis {
    registers: [Byte; REG_BYTES],
    reads: [Option<Access>; MAX_LOOP_INSTRUCTIONS as usize],
    stores: [Option<Store>; MAX_LOOP_INSTRUCTIONS as usize],
    store_count: usize,
    next_identity: u32,
    predicate: Dependencies,
    flags: Dependencies,
}

impl Analysis {
    pub(super) fn new() -> Self {
        let mut registers = [ZERO; REG_BYTES];
        for (index, byte) in registers.iter_mut().enumerate() {
            *byte = Byte {
                identity: index as u32 + 1,
                deps: Dependencies {
                    inputs: 1 << index,
                    ..Dependencies::EMPTY
                },
            };
        }
        Self {
            registers,
            reads: [None; MAX_LOOP_INSTRUCTIONS as usize],
            stores: [None; MAX_LOOP_INSTRUCTIONS as usize],
            store_count: 0,
            next_identity: 0x1000,
            predicate: Dependencies::EMPTY,
            flags: Dependencies {
                unknown: true,
                ..Dependencies::EMPTY
            },
        }
    }

    pub(super) fn visit(&mut self, instruction: &Instruction, slot: usize) {
        use Mnemonic as M;
        let mnemonic = instruction.mnemonic();
        if mnemonic == M::Nop {
            return;
        }
        if mnemonic == M::Lea {
            let address = self.address(instruction);
            let width = instruction.op0_register().size();
            let value = if address.base == [0; 4] && address.index == [0; 4] {
                Value::constant(address.displacement, width)
            } else if address.index == [0; 4] && address.displacement == 0 {
                // Preserve the identity of LEA reg,[reg] and its aliases.
                self.value_from_key(address.base, address.deps, width)
            } else {
                self.fresh(address.deps, width)
            };
            self.write(instruction, value);
            return;
        }
        if matches!(mnemonic, M::Mov | M::Movsx | M::Movzx) {
            let mut value = self.operand(instruction, 1, slot);
            let width = self.operand_width(instruction, 0);
            if mnemonic == M::Movsx {
                let negative = value.bytes[value.width - 1];
                let extension = if let Some(constant) = value.constant_value() {
                    Byte {
                        identity: CONSTANT
                            + if constant & (1 << (value.width * 8 - 1)) != 0 {
                                255
                            } else {
                                0
                            },
                        deps: Dependencies::EMPTY,
                    }
                } else {
                    self.fresh(negative.deps, 1).bytes[0]
                };
                value.bytes[value.width..width].fill(extension);
            } else if mnemonic == M::Movzx {
                value.bytes[value.width..width].fill(ZERO);
            }
            value.width = width;
            self.write(instruction, value);
            return;
        }
        let first = self.operand(instruction, 0, slot);
        let second = if instruction.op_count() == 2 {
            self.operand(instruction, 1, slot)
        } else {
            Value::constant(1, first.width)
        };
        if matches!(mnemonic, M::Cmp | M::Test) {
            let deps = if mnemonic == M::Cmp && first.same(second) {
                Dependencies::EMPTY
            } else if mnemonic == M::Test {
                self.bitwise(M::And, first, second).deps()
            } else {
                first.deps().merge(second.deps())
            };
            self.flags = deps;
            self.predicate = deps;
            return;
        }
        let mut deps = first.deps().merge(second.deps());
        if matches!(mnemonic, M::Adc | M::Sbb) {
            deps = deps.merge(self.flags);
        }
        let value = if matches!(mnemonic, M::And | M::Or | M::Xor) {
            self.bitwise(mnemonic, first, second)
        } else if mnemonic == M::Sub && first.same(second) {
            Value::constant(0, first.width)
        } else if matches!(mnemonic, M::Add | M::Sub) && second.constant_value() == Some(0) {
            first
        } else if let Some(constant) = constant_operation(mnemonic, first, second) {
            Value::constant(constant, first.width)
        } else {
            self.fresh(deps, first.width)
        };
        if instruction.rflags_modified() != 0 {
            self.flags = if matches!(mnemonic, M::Inc | M::Dec) {
                value.deps().merge(self.flags)
            } else {
                value.deps()
            };
        }
        self.write(instruction, value);
    }

    pub(super) fn finish(&self) -> (WaitEvidence, u32) {
        let mut changed = 0;
        for (index, byte) in self.registers.iter().enumerate() {
            if byte.identity != index as u32 + 1 {
                changed |= 1 << index;
            }
        }
        let predicate = self.predicate;
        if predicate.reads == 0 || predicate.inputs & changed != 0 {
            return (WaitEvidence::LocalProgress, predicate.reads);
        }
        let mut uncertain = predicate.unknown;
        for (index, read) in self.reads.iter().enumerate() {
            if predicate.reads & (1 << index) == 0 {
                continue;
            }
            let Some(read) = read else {
                uncertain = true;
                continue;
            };
            if read.address.deps.inputs & changed != 0 {
                return (WaitEvidence::LocalProgress, predicate.reads);
            }
            // A pointer fetched from memory is not a proven repeated location.
            uncertain |= read.address.deps.reads != 0 || read.address.deps.unknown;
            for store in self.stores[..self.store_count].iter().flatten() {
                match read.alias(store.access) {
                    Alias::Overlap => return (WaitEvidence::LocalProgress, predicate.reads),
                    Alias::Unknown => uncertain = true,
                    Alias::Disjoint => (),
                }
            }
        }
        (
            if uncertain {
                WaitEvidence::Uncertain
            } else {
                WaitEvidence::ExternalMemory
            },
            predicate.reads,
        )
    }

    fn fresh(&mut self, deps: Dependencies, width: usize) -> Value {
        let mut bytes = [ZERO; 4];
        for byte in &mut bytes[..width] {
            *byte = Byte {
                identity: self.next_identity,
                deps,
            };
            self.next_identity += 1;
        }
        Value { bytes, width }
    }

    fn register(&self, register: Register) -> Value {
        let (start, width) = register_span(register);
        let mut bytes = [ZERO; 4];
        bytes[..width].copy_from_slice(&self.registers[start..start + width]);
        Value { bytes, width }
    }

    fn operand_width(&self, instruction: &Instruction, operand: u32) -> usize {
        match instruction.op_kind(operand) {
            OpKind::Register => instruction.op_register(operand).size(),
            OpKind::Memory => instruction.memory_size().size(),
            OpKind::Immediate8 => 1,
            OpKind::Immediate16 | OpKind::Immediate8to16 => 2,
            _ => 4,
        }
    }

    fn operand(&mut self, instruction: &Instruction, operand: u32, slot: usize) -> Value {
        match instruction.op_kind(operand) {
            OpKind::Register => self.register(instruction.op_register(operand)),
            OpKind::Memory => {
                let access = Access {
                    address: self.address(instruction),
                    width: instruction.memory_size().size(),
                };
                // Only an exact last store can forward; an intervening possible
                // alias stops the search. Partial stores remain conservative.
                for store in self.stores[..self.store_count].iter().rev().flatten() {
                    if access.same(store.access) {
                        return store.value;
                    }
                    if access.alias(store.access) != Alias::Disjoint {
                        break;
                    }
                }
                self.reads[slot] = Some(access);
                self.fresh(
                    access.address.deps.merge(Dependencies {
                        reads: 1 << slot,
                        ..Dependencies::EMPTY
                    }),
                    access.width,
                )
            }
            _ => Value::constant(
                instruction.immediate(operand) as u32,
                self.operand_width(instruction, operand),
            ),
        }
    }

    fn write(&mut self, instruction: &Instruction, value: Value) {
        if instruction.op0_kind() == OpKind::Register {
            let (start, width) = register_span(instruction.op0_register());
            self.registers[start..start + width].copy_from_slice(&value.bytes[..width]);
        } else {
            self.stores[self.store_count] = Some(Store {
                access: Access {
                    address: self.address(instruction),
                    width: value.width,
                },
                value,
            });
            self.store_count += 1;
        }
    }

    fn address(&self, instruction: &Instruction) -> Address {
        let base = instruction.memory_base();
        let index = instruction.memory_index();
        let bits = if base.is_gpr16()
            || index.is_gpr16()
            || (base == Register::None
                && index == Register::None
                && instruction.memory_displ_size() == 2)
        {
            16
        } else {
            32
        };
        let mut address = Address {
            segment: instruction.memory_segment(),
            bits,
            base: [0; 4],
            index: [0; 4],
            scale: instruction.memory_index_scale(),
            displacement: instruction.memory_displacement32(),
            deps: Dependencies::EMPTY,
        };
        for (register, scale, is_index) in [(base, 1, false), (index, address.scale, true)] {
            if register == Register::None {
                continue;
            }
            let value = self.register(register);
            address.deps = address.deps.merge(value.deps());
            if let Some(constant) = value.constant_value() {
                address.displacement = address
                    .displacement
                    .wrapping_add(constant.wrapping_mul(scale));
            } else {
                let key = value.bytes.map(|byte| byte.identity);
                if is_index {
                    address.index = key;
                } else {
                    address.base = key;
                }
            }
        }
        if address.index == [0; 4] {
            address.scale = 1;
        }
        if address.scale == 1
            && (address.base == [0; 4] || (address.index != [0; 4] && address.base > address.index))
        {
            core::mem::swap(&mut address.base, &mut address.index);
        }
        if bits == 16 {
            address.displacement &= 0xffff;
        }
        address
    }

    fn value_from_key(&mut self, key: [u32; 4], deps: Dependencies, width: usize) -> Value {
        Value {
            bytes: key.map(|identity| Byte { identity, deps }),
            width,
        }
    }

    fn bitwise(&mut self, mnemonic: Mnemonic, first: Value, second: Value) -> Value {
        let mut result = first;
        for index in 0..first.width {
            let left = first.bytes[index];
            let right = second.bytes[index];
            let left_const = (CONSTANT..CONSTANT + 256)
                .contains(&left.identity)
                .then_some(left.identity.wrapping_sub(CONSTANT));
            let right_const = (CONSTANT..CONSTANT + 256)
                .contains(&right.identity)
                .then_some(right.identity.wrapping_sub(CONSTANT));
            result.bytes[index] = match (mnemonic, left_const, right_const) {
                (Mnemonic::Xor, _, _) if left.identity == right.identity => ZERO,
                (Mnemonic::And, Some(0), _) | (Mnemonic::And, _, Some(0)) => ZERO,
                (Mnemonic::Or, Some(255), _) | (Mnemonic::Or, _, Some(255)) => {
                    Value::constant(255, 1).bytes[0]
                }
                (Mnemonic::And, Some(255), _) | (Mnemonic::Or | Mnemonic::Xor, Some(0), _) => right,
                (Mnemonic::And, _, Some(255)) | (Mnemonic::Or | Mnemonic::Xor, _, Some(0)) => left,
                (_, Some(left), Some(right)) => {
                    Value::constant(
                        match mnemonic {
                            Mnemonic::And => left & right,
                            Mnemonic::Or => left | right,
                            _ => left ^ right,
                        },
                        1,
                    )
                    .bytes[0]
                }
                _ => self.fresh(left.deps.merge(right.deps), 1).bytes[0],
            };
        }
        result
    }
}

fn register_span(register: Register) -> (usize, usize) {
    let high = usize::from(matches!(
        register,
        Register::AH | Register::CH | Register::DH | Register::BH
    ));
    (
        register.full_register32().number() * 4 + high,
        register.size(),
    )
}

fn constant_operation(mnemonic: Mnemonic, first: Value, second: Value) -> Option<u32> {
    use Mnemonic as M;
    let left = first.constant_value()?;
    let right = second.constant_value()?;
    Some(match mnemonic {
        M::Add | M::Inc => left.wrapping_add(right),
        M::Sub | M::Dec => left.wrapping_sub(right),
        M::Neg => left.wrapping_neg(),
        M::Not => !left,
        // ADC/SBB require actual carry, not just constant operands.
        _ => return None,
    })
}
