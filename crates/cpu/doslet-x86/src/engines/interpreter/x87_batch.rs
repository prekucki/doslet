//! Batched execution of the common 387 forms inside interpreter slices.
//!
//! Each form performs the same `doslet-x87` operations, memory accesses and
//! pointer updates as `Cpu::x87_step`, in an order with the same final state.
//! Everything that could make the reference handler fail is checked before
//! the first effect: an available, unblocked FPU, operand tags, batch memory,
//! masked exceptions and a supported precision. A declined form changes
//! nothing; the slice ends there and the reference step executes it.
use super::*;
use crate::x87_decode::{Encoding, Mnemonic};

/// One decoded form of the batched subset; `index` selects ST(i).
#[derive(Clone, Copy)]
pub(super) struct X87 {
    op: Op,
    opcode: u8,
    modrm: u8,
    index: u8,
    /// Whether the reference handler records instruction/operand pointers.
    tracks: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    LoadReal {
        single: bool,
    },
    LoadExtended,
    LoadInteger {
        bytes: u8,
    },
    StoreReal {
        single: bool,
        pop: bool,
    },
    StoreExtended,
    StoreInteger {
        bytes: u8,
        pop: bool,
    },
    ArithmeticReal {
        single: bool,
        operation: u8,
    },
    CompareReal {
        single: bool,
        pop: bool,
    },
    ArithmeticInteger {
        bytes: u8,
        operation: u8,
    },
    CompareInteger {
        bytes: u8,
        pop: bool,
    },
    LoadControl,
    StoreControl,
    StoreStatus,
    /// ST(0) = ST(0) op ST(i) (D8), operations as in `X87::arithmetic_top`.
    ArithmeticTop {
        operation: u8,
    },
    /// ST(i) = ST(i) op ST(0) (DC), or with a pop (DE).
    ArithmeticInto {
        operation: u8,
        pop: bool,
    },
    CompareRegister {
        pops: u8,
    },
    ComparePopTwice,
    UnorderedCompare {
        pops: u8,
    },
    Duplicate,
    Exchange,
    StoreRegister {
        pop: bool,
    },
    Free,
    ChangeSign,
    Absolute,
    Constant(doslet_x87::Constant),
    TestZero,
    Examine,
    SquareRoot,
    RoundInteger,
    RotateTop {
        increment: bool,
    },
    Nop,
    ClearExceptions,
    Initialize,
    StatusToAx,
}

/// Number of `Op` variants. A prepared handler is monomorphized per variant,
/// so its precondition, operand and execution matches fold at compile time.
pub(super) const KINDS: usize = 34;
/// Variants below this have a memory operand.
pub(super) const MEMORY_KINDS: u8 = 13;

impl Op {
    /// Variant number and payload bits; `join` rebuilds the operation.
    fn split(self) -> (u8, u8) {
        let b = u8::from;
        match self {
            Op::LoadReal { single } => (0, b(single)),
            Op::LoadExtended => (1, 0),
            Op::LoadInteger { bytes } => (2, bytes),
            Op::StoreReal { single, pop } => (3, b(single) | (b(pop) << 1)),
            Op::StoreExtended => (4, 0),
            Op::StoreInteger { bytes, pop } => (5, bytes | (b(pop) << 4)),
            Op::ArithmeticReal { single, operation } => (6, operation | (b(single) << 4)),
            Op::CompareReal { single, pop } => (7, b(single) | (b(pop) << 1)),
            Op::ArithmeticInteger { bytes, operation } => (8, operation | (bytes << 4)),
            Op::CompareInteger { bytes, pop } => (9, bytes | (b(pop) << 4)),
            Op::LoadControl => (10, 0),
            Op::StoreControl => (11, 0),
            Op::StoreStatus => (12, 0),
            Op::ArithmeticTop { operation } => (13, operation),
            Op::ArithmeticInto { operation, pop } => (14, operation | (b(pop) << 4)),
            Op::CompareRegister { pops } => (15, pops),
            Op::ComparePopTwice => (16, 0),
            Op::UnorderedCompare { pops } => (17, pops),
            Op::Duplicate => (18, 0),
            Op::Exchange => (19, 0),
            Op::StoreRegister { pop } => (20, b(pop)),
            Op::Free => (21, 0),
            Op::ChangeSign => (22, 0),
            Op::Absolute => (23, 0),
            Op::Constant(constant) => (24, constant as u8),
            Op::TestZero => (25, 0),
            Op::Examine => (26, 0),
            Op::SquareRoot => (27, 0),
            Op::RoundInteger => (28, 0),
            Op::RotateTop { increment } => (29, b(increment)),
            Op::Nop => (30, 0),
            Op::ClearExceptions => (31, 0),
            Op::Initialize => (32, 0),
            Op::StatusToAx => (33, 0),
        }
    }
    #[inline(always)]
    fn join(kind: u8, payload: u8) -> Self {
        use doslet_x87::Constant::*;
        let bit = |n: u8| payload & (1 << n) != 0;
        let low = payload & 15;
        let high = payload >> 4;
        match kind {
            0 => Op::LoadReal { single: bit(0) },
            1 => Op::LoadExtended,
            2 => Op::LoadInteger { bytes: payload },
            3 => Op::StoreReal {
                single: bit(0),
                pop: bit(1),
            },
            4 => Op::StoreExtended,
            5 => Op::StoreInteger {
                bytes: low,
                pop: bit(4),
            },
            6 => Op::ArithmeticReal {
                single: bit(4),
                operation: low,
            },
            7 => Op::CompareReal {
                single: bit(0),
                pop: bit(1),
            },
            8 => Op::ArithmeticInteger {
                bytes: high,
                operation: low,
            },
            9 => Op::CompareInteger {
                bytes: low,
                pop: bit(4),
            },
            10 => Op::LoadControl,
            11 => Op::StoreControl,
            12 => Op::StoreStatus,
            13 => Op::ArithmeticTop { operation: payload },
            14 => Op::ArithmeticInto {
                operation: low,
                pop: bit(4),
            },
            15 => Op::CompareRegister { pops: payload },
            16 => Op::ComparePopTwice,
            17 => Op::UnorderedCompare { pops: payload },
            18 => Op::Duplicate,
            19 => Op::Exchange,
            20 => Op::StoreRegister { pop: bit(0) },
            21 => Op::Free,
            22 => Op::ChangeSign,
            23 => Op::Absolute,
            24 => {
                Op::Constant([One, Log2Ten, Log2E, Pi, Log10Two, LnTwo, Zero][usize::from(low % 7)])
            }
            25 => Op::TestZero,
            26 => Op::Examine,
            27 => Op::SquareRoot,
            28 => Op::RoundInteger,
            29 => Op::RotateTop { increment: bit(0) },
            30 => Op::Nop,
            31 => Op::ClearExceptions,
            32 => Op::Initialize,
            _ => Op::StatusToAx,
        }
    }
}

impl X87 {
    /// The variant number selecting this form's prepared handler.
    pub(super) fn kind(self) -> u8 {
        self.op.split().0
    }
    /// ModR/M, opcode bits, pointer tracking, ST index and payload in one
    /// prepared instruction word.
    pub(super) fn pack(self) -> u32 {
        u32::from(self.modrm)
            | (u32::from(self.opcode & 7) << 8)
            | (u32::from(self.tracks) << 11)
            | (u32::from(self.index) << 12)
            | (u32::from(self.op.split().1) << 16)
    }
    #[inline(always)]
    pub(super) fn unpack<const KIND: u8>(word: u32) -> Self {
        Self {
            op: Op::join(KIND, (word >> 16) as u8),
            opcode: 0xD8 | ((word >> 8) & 7) as u8,
            modrm: word as u8,
            index: ((word >> 12) & 7) as u8,
            tracks: word & 0x800 != 0,
        }
    }

    /// Classify an implemented 387 encoding; transcendental, environment,
    /// BCD and other rare forms stay with the reference interpreter.
    pub(super) fn decode(opcode: u8, modrm: u8) -> Option<Self> {
        let Encoding::I387(instruction) = crate::x87_decode::decode(opcode, modrm) else {
            return None;
        };
        if !instruction.implemented() {
            return None;
        }
        use Mnemonic::*;
        // The same exclusion list as the reference handler.
        let tracks = !matches!(
            instruction.mnemonic,
            Fldenv
                | Fnstenv
                | Frstor
                | Fnsave
                | Fldcw
                | Fnstcw
                | Fnstsw
                | Fnclex
                | Fninit
                | Fneni
                | Fndisi
                | Fnsetpm
        );
        let group = (modrm >> 3) & 7;
        let mut index = modrm & 7;
        let op = if modrm >> 6 != 3 {
            match (opcode, group) {
                (0xD9 | 0xDD, 0) => Op::LoadReal {
                    single: opcode == 0xD9,
                },
                (0xDB, 5) => Op::LoadExtended,
                (0xDF, 0) => Op::LoadInteger { bytes: 2 },
                (0xDB, 0) => Op::LoadInteger { bytes: 4 },
                (0xDF, 5) => Op::LoadInteger { bytes: 8 },
                (0xD9 | 0xDD, 2 | 3) => Op::StoreReal {
                    single: opcode == 0xD9,
                    pop: group == 3,
                },
                (0xDB, 7) => Op::StoreExtended,
                (0xDB | 0xDF, 2 | 3) => Op::StoreInteger {
                    bytes: if opcode == 0xDB { 4 } else { 2 },
                    pop: group == 3,
                },
                (0xDF, 7) => Op::StoreInteger {
                    bytes: 8,
                    pop: true,
                },
                (0xD8 | 0xDC, 2 | 3) => Op::CompareReal {
                    single: opcode == 0xD8,
                    pop: group == 3,
                },
                (0xD8 | 0xDC, operation) => Op::ArithmeticReal {
                    single: opcode == 0xD8,
                    operation,
                },
                (0xDA | 0xDE, 2 | 3) => Op::CompareInteger {
                    bytes: if opcode == 0xDA { 4 } else { 2 },
                    pop: group == 3,
                },
                (0xDA | 0xDE, operation) => Op::ArithmeticInteger {
                    bytes: if opcode == 0xDA { 4 } else { 2 },
                    operation,
                },
                (0xD9, 5) => Op::LoadControl,
                (0xD9, 7) => Op::StoreControl,
                (0xDD, 7) => Op::StoreStatus,
                _ => return None,
            }
        } else {
            match (opcode, modrm) {
                // FADD, FMUL, FSUB, FSUBR, FDIV and FDIVR ST(0), ST(i).
                (0xD8, 0xC0..=0xCF | 0xE0..=0xFF) => Op::ArithmeticTop { operation: group },
                (0xD8, 0xD0..=0xDF) => Op::CompareRegister {
                    pops: u8::from(modrm & 8 != 0),
                },
                // ST(i) = ST(i) op ST(0): /4 is FSUBR (ST(0) − ST(i)) and
                // /6 is FDIVR, so the reverse operations have even groups.
                (0xDC | 0xDE, 0xC0..=0xCF | 0xE0..=0xFF) => Op::ArithmeticInto {
                    operation: [0, 1, 2, 3, 5, 4, 7, 6][usize::from(group)],
                    pop: opcode == 0xDE,
                },
                (0xDE, 0xD9) => Op::ComparePopTwice,
                (0xDD, 0xE0..=0xEF) => Op::UnorderedCompare {
                    pops: u8::from(modrm & 8 != 0),
                },
                (0xDA, 0xE9) => {
                    index = 1;
                    Op::UnorderedCompare { pops: 2 }
                }
                (0xD9, 0xC0..=0xC7) => Op::Duplicate,
                (0xD9, 0xC8..=0xCF) => Op::Exchange,
                (0xDD, 0xD0..=0xDF) => Op::StoreRegister {
                    pop: modrm & 8 != 0,
                },
                (0xDD, 0xC0..=0xC7) => Op::Free,
                (0xD9, 0xE0) => Op::ChangeSign,
                (0xD9, 0xE1) => Op::Absolute,
                (0xD9, 0xE4) => Op::TestZero,
                (0xD9, 0xE5) => Op::Examine,
                (0xD9, 0xE8..=0xEE) => {
                    use doslet_x87::Constant::*;
                    Op::Constant(
                        [One, Log2Ten, Log2E, Pi, Log10Two, LnTwo, Zero][usize::from(index)],
                    )
                }
                (0xD9, 0xFA) => Op::SquareRoot,
                (0xD9, 0xFC) => Op::RoundInteger,
                (0xD9, 0xF6 | 0xF7) => Op::RotateTop {
                    increment: modrm & 1 != 0,
                },
                (0xD9, 0xD0) | (0xDB, 0xE0 | 0xE1 | 0xE4) => Op::Nop,
                (0xDB, 0xE2) => Op::ClearExceptions,
                (0xDB, 0xE3) => Op::Initialize,
                (0xDF, 0xE0) => Op::StatusToAx,
                _ => return None,
            }
        };
        Some(Self {
            op,
            opcode,
            modrm,
            index,
            tracks,
        })
    }

    /// Memory operand width and access, for memory forms.
    fn memory(self) -> Option<(u8, Access)> {
        let real = |single| if single { 4 } else { 8 };
        Some(match self.op {
            Op::LoadReal { single }
            | Op::ArithmeticReal { single, .. }
            | Op::CompareReal { single, .. } => (real(single), Access::Read),
            Op::LoadExtended => (10, Access::Read),
            Op::LoadInteger { bytes }
            | Op::ArithmeticInteger { bytes, .. }
            | Op::CompareInteger { bytes, .. } => (bytes, Access::Read),
            Op::StoreReal { single, .. } => (real(single), Access::Write),
            Op::StoreExtended => (10, Access::Write),
            Op::StoreInteger { bytes, .. } => (bytes, Access::Write),
            Op::LoadControl => (2, Access::Read),
            Op::StoreControl | Op::StoreStatus => (2, Access::Write),
            _ => return None,
        })
    }

    /// True when every reference failure path is excluded for this state.
    #[cfg_attr(not(target_arch = "wasm32"), inline(always))]
    fn ready(self, fpu: &doslet_x87::X87) -> bool {
        let control = fpu.control();
        // No unmasked exception can be raised, and PC=01 is not reserved.
        let numeric = control & 0x3F == 0x3F && (control >> 8) & 3 != 1;
        let full = |index| !fpu.is_empty(index);
        let push = fpu.is_empty(7);
        let i = self.index;
        match self.op {
            Op::LoadReal { .. } => push && numeric,
            Op::LoadExtended | Op::LoadInteger { .. } | Op::Constant(_) => push,
            Op::Duplicate => full(i) && push,
            Op::StoreExtended | Op::StoreRegister { .. } | Op::ChangeSign | Op::Absolute => full(0),
            Op::StoreReal { .. }
            | Op::StoreInteger { .. }
            | Op::ArithmeticReal { .. }
            | Op::CompareReal { .. }
            | Op::ArithmeticInteger { .. }
            | Op::CompareInteger { .. }
            | Op::TestZero
            | Op::SquareRoot
            | Op::RoundInteger => full(0) && numeric,
            Op::ArithmeticTop { .. }
            | Op::ArithmeticInto { .. }
            | Op::CompareRegister { .. }
            | Op::UnorderedCompare { .. } => full(0) && full(i) && numeric,
            Op::ComparePopTwice => full(0) && full(1) && numeric,
            Op::Exchange => full(0) && full(i),
            Op::LoadControl
            | Op::StoreControl
            | Op::StoreStatus
            | Op::Free
            | Op::Examine
            | Op::RotateTop { .. }
            | Op::Nop
            | Op::ClearExceptions
            | Op::Initialize
            | Op::StatusToAx => true,
        }
    }
}

const NO_FAILURE: &str = "batched x87 preconditions exclude reference failures";

impl Cpu {
    /// Execute decoded IR `x` at the current IP, or decline without effect.
    pub(super) fn x87_batched<B: CpuBus>(
        &mut self,
        bus: &mut B,
        windows: &mut Windows,
        x: X87,
        target: Target,
        next: u32,
    ) -> bool {
        self.x87_execute(bus, windows, x, next, |r| match target.resolve(r) {
            Operand::Memory { segment, offset } => Some((segment, offset)),
            Operand::Register(_) => None,
        })
    }

    /// Execute `x`, whose memory operand `operand` resolves, at the current
    /// IP, or decline without any effect. Inlined into each prepared handler.
    #[inline(always)]
    pub(super) fn x87_execute<B: CpuBus>(
        &mut self,
        bus: &mut B,
        windows: &mut Windows,
        x: X87,
        next: u32,
        operand: impl FnOnce(&Registers) -> Option<(SegmentRegister, u32)>,
    ) -> bool {
        if self.control.cr0 & 12 != 0 {
            return false;
        }
        let Some(fpu) = self.fpu.as_ref() else {
            return false;
        };
        if !x.ready(fpu) {
            return false;
        }
        let memory = match x.memory() {
            Some((width, access)) => {
                let Some((segment, offset)) = operand(&self.registers) else {
                    return false;
                };
                let Some(address) =
                    windows.linear(&self.registers, bus, segment, offset, width, access)
                else {
                    return false;
                };
                Some((segment, offset, address))
            }
            None => None,
        };
        let address = memory.map_or(0, |(_, _, address)| address);
        let r = &self.registers;
        let protected = self.control.cr0 & 1 != 0;
        let instruction = if protected {
            r.ip
        } else {
            (u32::from(r.cs) << 4).wrapping_add(r.ip)
        };
        let cs = r.cs;
        let operand = memory.map(|(segment, offset, linear)| {
            (if protected { offset } else { linear }, r.selector(segment))
        });
        let fpu = self.fpu.as_mut().unwrap();
        let read_real = |bus: &mut B, single: bool| {
            let low = bus.read_u32(address).expect(PLAIN_RAM);
            if single {
                u64::from(low)
            } else {
                u64::from(low) | (u64::from(bus.read_u32(address + 4).expect(PLAIN_RAM)) << 32)
            }
        };
        let read_integer = |bus: &mut B, bytes: u8| match bytes {
            2 => i64::from(bus.read_u16(address).expect(PLAIN_RAM) as i16),
            4 => i64::from(bus.read_u32(address).expect(PLAIN_RAM) as i32),
            _ => {
                let low = bus.read_u32(address).expect(PLAIN_RAM);
                let high = bus.read_u32(address + 4).expect(PLAIN_RAM);
                (u64::from(low) | (u64::from(high) << 32)) as i64
            }
        };
        let fast = self.decode_cache.x87_fast;
        let mut pop = false;
        let i = x.index;
        match x.op {
            Op::LoadReal { single } => {
                let bits = read_real(bus, single);
                if fast {
                    fpu.fast_load_real(bits, single)
                } else {
                    fpu.load_real(bits, single)
                }
                .expect(NO_FAILURE);
            }
            Op::LoadExtended => {
                let mut bytes = [0; 10];
                for (offset, byte) in bytes.iter_mut().enumerate() {
                    *byte = bus.read_u8(address + offset as u32).expect(PLAIN_RAM);
                }
                fpu.load_extended(bytes).expect(NO_FAILURE);
            }
            Op::LoadInteger { bytes } => {
                let value = read_integer(bus, bytes);
                if fast {
                    fpu.fast_push_integer(value)
                } else {
                    fpu.push_integer(value)
                }
                .expect(NO_FAILURE);
            }
            Op::StoreReal {
                single,
                pop: popping,
            } => {
                let status = fpu.status();
                let value = if fast {
                    fpu.fast_store_float(single)
                } else {
                    fpu.store_float(single)
                }
                .expect(NO_FAILURE);
                if single && !value.is_nan() && f64::from(value as f32) != value {
                    // The reference reports this as unsupported; leave it there.
                    fpu.restore_status(status);
                    return false;
                }
                if single {
                    bus.write_u32(address, (value as f32).to_bits())
                        .expect(PLAIN_RAM);
                } else {
                    let bits = value.to_bits();
                    bus.write_u32(address, bits as u32).expect(PLAIN_RAM);
                    bus.write_u32(address + 4, (bits >> 32) as u32)
                        .expect(PLAIN_RAM);
                }
                pop = popping;
            }
            Op::StoreExtended => {
                let bytes = fpu.store_extended().expect(NO_FAILURE);
                for (offset, byte) in bytes.into_iter().enumerate() {
                    bus.write_u8(address + offset as u32, byte)
                        .expect(PLAIN_RAM);
                }
                pop = true;
            }
            Op::StoreInteger {
                bytes,
                pop: popping,
            } => {
                let value = fpu.store_integer(bytes * 8).expect(NO_FAILURE);
                if bytes == 2 {
                    bus.write_u16(address, value as u16).expect(PLAIN_RAM);
                } else {
                    bus.write_u32(address, value as u32).expect(PLAIN_RAM);
                    if bytes == 8 {
                        bus.write_u32(address + 4, (value >> 32) as u32)
                            .expect(PLAIN_RAM);
                    }
                }
                pop = popping;
            }
            Op::ArithmeticReal { single, operation } => {
                let bits = read_real(bus, single);
                if fast {
                    fpu.fast_arithmetic_real(bits, single, operation)
                } else {
                    fpu.arithmetic_real(bits, single, operation)
                }
                .expect(NO_FAILURE);
            }
            Op::CompareReal { single, pop } => {
                let bits = read_real(bus, single);
                fpu.compare_real(bits, single, pop).expect(NO_FAILURE);
            }
            Op::ArithmeticInteger { bytes, operation } => {
                let value = read_integer(bus, bytes);
                if fast {
                    fpu.fast_arithmetic_integer(value, operation)
                } else {
                    fpu.arithmetic_integer(value, operation)
                }
                .expect(NO_FAILURE);
            }
            Op::CompareInteger { bytes, pop } => {
                let value = read_integer(bus, bytes);
                fpu.compare_integer(value, pop).expect(NO_FAILURE);
            }
            Op::LoadControl => {
                let value = bus.read_u16(address).expect(PLAIN_RAM);
                fpu.load_control(value);
            }
            Op::StoreControl => bus.write_u16(address, fpu.control()).expect(PLAIN_RAM),
            Op::StoreStatus => bus.write_u16(address, fpu.status()).expect(PLAIN_RAM),
            Op::ArithmeticTop { operation } => if fast {
                fpu.fast_arithmetic_top(i, operation)
            } else {
                fpu.arithmetic_top(i, operation)
            }
            .expect(NO_FAILURE),
            Op::ArithmeticInto { operation, pop } => if fast {
                fpu.fast_arithmetic_into(i, operation, pop)
            } else {
                fpu.arithmetic_into(i, operation, pop)
            }
            .expect(NO_FAILURE),
            Op::CompareRegister { pops } => fpu.compare_register(i, pops).expect(NO_FAILURE),
            Op::ComparePopTwice => fpu.compare_pop_twice().expect(NO_FAILURE),
            Op::UnorderedCompare { pops } => fpu.unordered_compare(i, pops).expect(NO_FAILURE),
            Op::Duplicate => if fast {
                fpu.fast_duplicate(i)
            } else {
                fpu.duplicate(i)
            }
            .expect(NO_FAILURE),
            Op::Exchange => fpu.exchange(i).expect(NO_FAILURE),
            Op::StoreRegister { pop } => fpu.store_register(i, pop).expect(NO_FAILURE),
            Op::Free => fpu.free(i),
            Op::ChangeSign => fpu.change_sign().expect(NO_FAILURE),
            Op::Absolute => fpu.absolute().expect(NO_FAILURE),
            Op::Constant(constant) => fpu.load_constant(constant).expect(NO_FAILURE),
            Op::TestZero => fpu.test_zero().expect(NO_FAILURE),
            Op::Examine => fpu.examine(),
            Op::SquareRoot => fpu.square_root().expect(NO_FAILURE),
            Op::RoundInteger => fpu.round_integer().expect(NO_FAILURE),
            Op::RotateTop { increment } => fpu.rotate_top(increment),
            Op::Nop => {}
            Op::ClearExceptions => fpu.clear_exceptions(),
            Op::Initialize => fpu.initialize(),
            Op::StatusToAx => {
                let status = fpu.status();
                self.registers.set(Gpr16::Ax, status);
            }
        }
        let fpu = self.fpu.as_mut().unwrap();
        if x.tracks {
            fpu.record_instruction(
                instruction,
                cs,
                (u16::from(x.opcode & 7) << 8) | u16::from(x.modrm),
            );
            if let Some((offset, selector)) = operand {
                fpu.record_operand(offset, selector);
            }
        }
        if pop {
            fpu.pop().expect(NO_FAILURE);
        }
        self.registers.ip = next;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prepared_words_rebuild_every_batched_form() {
        let mut forms = 0;
        let mut kinds = [false; KINDS];
        for opcode in 0xD8..=0xDF {
            for modrm in 0..=0xFF {
                let Some(x) = X87::decode(opcode, modrm) else {
                    continue;
                };
                let kind = x.kind();
                kinds[usize::from(kind)] = true;
                assert_eq!(x.memory().is_some(), kind < MEMORY_KINDS);
                let word = x.pack();
                let rebuilt = X87 {
                    op: Op::join(kind, (word >> 16) as u8),
                    opcode: 0xD8 | ((word >> 8) & 7) as u8,
                    modrm: word as u8,
                    index: ((word >> 12) & 7) as u8,
                    tracks: word & 0x800 != 0,
                };
                assert_eq!(rebuilt.op, x.op, "{opcode:02X} {modrm:02X}");
                assert_eq!(
                    (rebuilt.opcode, rebuilt.modrm, rebuilt.index, rebuilt.tracks),
                    (x.opcode, x.modrm, x.index, x.tracks)
                );
                forms += 1;
            }
        }
        assert!(kinds.iter().all(|&used| used));
        assert!(forms > 1000, "{forms}");
    }
}
