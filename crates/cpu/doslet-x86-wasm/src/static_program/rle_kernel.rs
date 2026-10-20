//! Complete byte RLE computation, identified by operations and control flow.
//!
//! The owner proves a finite consumed source prefix before entry. In particular,
//! zero-length run tokens consume two source bytes and need that independent
//! cursor bound; output count alone is not a termination proof.

use super::*;
use crate::static_program::{Alu, Condition, Register, Source, memory};

/// Fixed semantic contract: EAX source, EBX destination, positive signed ECX
/// output count, and two dword pointer arguments (remaining run and run value).
/// Returns with RET 8, preserving ESI/EDI/EBP. Only ordinary disjoint RAM is
/// admitted by the owner; every write must exclude compiled code. The source
/// and all write spans must be mutually disjoint.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RleKernel;

impl RleKernel {
    /// Static representable ceiling; actual work is proved afresh by the owner.
    pub const fn maximum_work(self) -> u64 {
        u32::MAX as u64
    }
}

/// Trusted owner-to-generated-code call capability, outside guest RAM. All
/// pointers are validated WASM backing offsets, including a 24-byte stack shadow
/// starting at entry ESP - 24. The owner validates mapping, permissions, complete
/// disjoint spans, source pages, generation capacity and a finite source prefix.
/// It also excludes guest address/stack wrap and requires 32-bit CPU and stack.
/// This header must be rebuilt for every call, including after any refusal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RleCall {
    pub source_pointer: u32,
    pub destination_pointer: u32,
    pub count_pointer: u32,
    pub value_pointer: u32,
    pub stack_pointer: u32,
    pub return_target: u32,
    pub work: u32,
    pub output_count: u32,
    pub initial_run: u32,
    pub initial_value: u32,
    /// Finite prefix consumed before reaching the requested output count,
    /// including both bytes of every zero-length run token.
    pub source_length: u32,
    /// Original guest offset in EBX after the prologue loads the count pointer.
    pub count_offset: u32,
    /// Original count/value store instruction totals, not their byte counts.
    pub count_writes: u32,
    pub value_writes: u32,
}

impl RleCall {
    pub const OFFSET: u32 = memory::OFFSET + memory::BYTES as u32;
    pub const BYTES: usize = 64;
    pub const VERSION: u32 = 1;

    pub fn write(self, bytes: &mut [u8]) -> Option<()> {
        let bytes = bytes.get_mut(..Self::BYTES)?;
        let words = [
            Self::VERSION,
            self.source_pointer,
            self.destination_pointer,
            self.count_pointer,
            self.value_pointer,
            self.stack_pointer,
            self.return_target,
            self.work,
            self.output_count,
            self.initial_run,
            self.initial_value,
            self.source_length,
            self.count_offset,
            self.count_writes,
            self.value_writes,
            0,
        ];
        for (word, output) in words.into_iter().zip(bytes.as_chunks_mut::<4>().0) {
            output.copy_from_slice(&word.to_le_bytes());
        }
        Some(())
    }

    pub fn read(bytes: &[u8]) -> Option<Self> {
        let bytes = bytes.get(..Self::BYTES)?;
        let mut words = [0; 16];
        for (word, input) in words.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *word = u32::from_le_bytes(*input);
        }
        if words[0] != Self::VERSION || words[15] != 0 {
            return None;
        }
        Some(Self {
            source_pointer: words[1],
            destination_pointer: words[2],
            count_pointer: words[3],
            value_pointer: words[4],
            stack_pointer: words[5],
            return_target: words[6],
            work: words[7],
            output_count: words[8],
            initial_run: words[9],
            initial_value: words[10],
            source_length: words[11],
            count_offset: words[12],
            count_writes: words[13],
            value_writes: words[14],
        })
    }
}

impl ProcedureGraphFunction {
    /// Require the complete fixed RLE operation/edge contract after all graph
    /// validity checks. Relocation, block ordering and synthetic subdivisions
    /// do not affect recognition. Extra or unsupported operations are refused.
    pub fn recognize_rle_kernel(&self) -> Option<RleKernel> {
        graph::validate_structure(self).ok()?;
        recognize(self)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Cursor {
    block: usize,
    instruction: usize,
}

struct Matcher<'a> {
    function: &'a ProcedureGraphFunction,
    // Track every real instruction/terminator independently: no pattern stage
    // may accidentally reuse a previously matched stage through a changed edge.
    visited: Vec<Cursor>,
}

impl Matcher<'_> {
    fn target(&self, address: Address) -> Option<Cursor> {
        Some(Cursor {
            block: self
                .function
                .blocks
                .iter()
                .position(|b| b.entry == address)?,
            instruction: 0,
        })
    }
    fn normalize(&self, mut cursor: Cursor) -> Option<Cursor> {
        for _ in 0..=self.function.blocks.len() {
            let block = &self.function.blocks[cursor.block];
            if cursor.instruction < block.instructions.len() {
                return Some(cursor);
            }
            if let ProcedureTerminatorKind::Fallthrough(target) = block.terminator.kind {
                cursor = self.target(target)?;
            } else {
                return Some(cursor);
            }
        }
        None
    }
    fn visit(&mut self, cursor: Cursor) -> Option<()> {
        if self.visited.contains(&cursor) {
            return None;
        }
        self.visited.push(cursor);
        Some(())
    }
    fn ops(&mut self, cursor: &mut Cursor, expected: &[Operation]) -> Option<()> {
        for operation in expected {
            *cursor = self.normalize(*cursor)?;
            let actual = self.function.blocks[cursor.block]
                .instructions
                .get(cursor.instruction)?;
            if actual.operation != *operation {
                return None;
            }
            self.visit(*cursor)?;
            cursor.instruction += 1;
        }
        Some(())
    }
    fn terminator(&mut self, cursor: &mut Cursor) -> Option<ProcedureTerminatorKind> {
        *cursor = self.normalize(*cursor)?;
        let block = &self.function.blocks[cursor.block];
        if cursor.instruction != block.instructions.len() {
            return None;
        }
        let kind = block.terminator.kind;
        self.visit(*cursor)?;
        Some(kind)
    }
    fn branch(&mut self, cursor: &mut Cursor, expected: Condition) -> Option<(Cursor, Cursor)> {
        let ProcedureTerminatorKind::Branch {
            condition,
            taken,
            not_taken,
        } = self.terminator(cursor)?
        else {
            return None;
        };
        if condition != expected {
            return None;
        }
        Some((
            self.normalize(self.target(taken)?)?,
            self.normalize(self.target(not_taken)?)?,
        ))
    }
    fn jump(&mut self, cursor: &mut Cursor, expected: Cursor) -> Option<()> {
        let ProcedureTerminatorKind::Jump(target) = self.terminator(cursor)? else {
            return None;
        };
        (self.normalize(self.target(target)?)? == expected).then_some(())
    }
}

fn address(base: Register, index: Option<Register>, displacement: u32) -> memory::MemoryAddress {
    memory::MemoryAddress {
        base: Some(base),
        index,
        scale: 1,
        displacement,
        address16: false,
        segment: if base == Register::Esp || base == Register::Ebp {
            memory::Segment::Ss
        } else {
            memory::Segment::Ds
        },
    }
}
fn mov(target: Register, source: Register) -> Operation {
    Operation::Move {
        target,
        source: Source::Register(source),
    }
}
fn alu(target: Register, kind: Alu, source: Source) -> Operation {
    Operation::Alu {
        target,
        kind,
        source,
    }
}
fn imm(target: Register, kind: Alu, value: u32) -> Operation {
    alu(target, kind, Source::Immediate(value))
}
fn load(target: Register, base: Register, displacement: u32) -> Operation {
    Operation::LoadMemory {
        target,
        address: address(base, None, displacement),
    }
}
fn store(base: Register, displacement: u32, source: Register) -> Operation {
    Operation::StoreMemory {
        address: address(base, None, displacement),
        width: source.width(),
        source: Source::Register(source),
    }
}
fn compare(base: Register) -> Operation {
    Operation::CompareMemory {
        test: false,
        address: address(base, None, 0),
        width: Width::Dword,
        source: Source::Immediate(0),
    }
}
fn cmp_bound() -> Operation {
    Operation::AluMemory {
        kind: Alu::Cmp,
        target: Register::Edx,
        address: address(Register::Esp, None, 4),
    }
}
fn lea(target: Register, base: Register, displacement: u32) -> Operation {
    Operation::Lea {
        target,
        address: address(base, None, displacement),
    }
}

pub(super) fn recognize(function: &ProcedureGraphFunction) -> Option<RleKernel> {
    use Register::*;
    use Source::Register as Reg;
    let total: usize = function
        .blocks
        .iter()
        .map(|b| {
            b.instructions.len()
                + usize::from(!matches!(
                    b.terminator.kind,
                    ProcedureTerminatorKind::Fallthrough(_)
                ))
        })
        .sum();
    let mut m = Matcher {
        function,
        visited: reserved(total).ok()?,
    };
    let mut cursor = m.target(function.entry)?;
    m.ops(
        &mut cursor,
        &[
            Operation::Push {
                width: Width::Dword,
                source: Reg(Esi),
            },
            Operation::Push {
                width: Width::Dword,
                source: Reg(Edi),
            },
            Operation::Push {
                width: Width::Dword,
                source: Reg(Ebp),
            },
            imm(Esp, Alu::Sub, 12),
            load(Ebp, Esp, 32),
            store(Esp, 8, Eax),
            store(Esp, 0, Ebx),
            store(Esp, 4, Ecx),
            load(Ebx, Esp, 28),
            load(Edi, Esp, 0),
            alu(Eax, Alu::Xor, Reg(Eax)),
            alu(Edx, Alu::Xor, Reg(Edx)),
        ],
    )?;
    cursor = m.normalize(cursor)?;
    let initial = cursor;
    m.ops(&mut cursor, &[compare(Ebx)])?;
    let (outer, mut cursor) = m.branch(&mut cursor, Condition::LessOrEqual)?;
    m.ops(&mut cursor, &[cmp_bound()])?;
    let (out, mut cursor) = m.branch(&mut cursor, Condition::GreaterOrEqual)?;
    if out != outer {
        return None;
    }
    m.ops(
        &mut cursor,
        &[
            mov(Esi, Edx),
            alu(Esi, Alu::Add, Reg(Edi)),
            load(Cl, Ebp, 0),
            store(Esi, 0, Cl),
            load(Ecx, Ebx, 0),
            Operation::Dec(Ecx),
            Operation::Inc(Edx),
            store(Ebx, 0, Ecx),
        ],
    )?;
    m.jump(&mut cursor, initial)?;
    let mut cursor = outer;
    m.ops(&mut cursor, &[cmp_bound()])?;
    let (epilogue, mut cursor) = m.branch(&mut cursor, Condition::GreaterOrEqual)?;
    m.ops(
        &mut cursor,
        &[
            load(Esi, Esp, 8),
            Operation::LoadMemory {
                target: Cl,
                address: address(Esi, Some(Eax), 0),
            },
            imm(Cl, Alu::And, 0xc0),
            Operation::Extend {
                target: Edi,
                source: Cl,
                signed: false,
            },
            lea(Esi, Eax, 1),
            imm(Edi, Alu::Cmp, 0xc0),
        ],
    )?;
    let (literal, mut cursor) = m.branch(&mut cursor, Condition::NotEqual)?;
    m.ops(
        &mut cursor,
        &[
            load(Edi, Esp, 8),
            alu(Eax, Alu::Add, Reg(Edi)),
            load(Al, Eax, 0),
            imm(Al, Alu::And, 0x3f),
            imm(Eax, Alu::And, 0xff),
            store(Ebx, 0, Eax),
            lea(Eax, Esi, 1),
            alu(Esi, Alu::Add, Reg(Edi)),
            Operation::ExtendMemory {
                target: Esi,
                width: Width::Byte,
                address: address(Esi, None, 0),
                signed: false,
            },
            load(Edi, Esp, 4),
            store(Ebp, 0, Esi),
        ],
    )?;
    cursor = m.normalize(cursor)?;
    let run = cursor;
    m.ops(&mut cursor, &[compare(Ebx)])?;
    let (out, mut cursor) = m.branch(&mut cursor, Condition::LessOrEqual)?;
    if out != outer {
        return None;
    }
    m.ops(&mut cursor, &[alu(Edx, Alu::Cmp, Reg(Edi))])?;
    let (out, mut cursor) = m.branch(&mut cursor, Condition::GreaterOrEqual)?;
    if out != outer {
        return None;
    }
    m.ops(
        &mut cursor,
        &[
            load(Ecx, Esp, 0),
            mov(Esi, Edx),
            alu(Esi, Alu::Add, Reg(Ecx)),
            load(Cl, Ebp, 0),
            store(Esi, 0, Cl),
            load(Esi, Ebx, 0),
            Operation::Dec(Esi),
            Operation::Inc(Edx),
            store(Ebx, 0, Esi),
        ],
    )?;
    m.jump(&mut cursor, run)?;
    let mut cursor = literal;
    m.ops(
        &mut cursor,
        &[
            mov(Edi, Eax),
            mov(Eax, Esi),
            load(Esi, Esp, 8),
            load(Ecx, Esp, 0),
            alu(Esi, Alu::Add, Reg(Edi)),
            mov(Edi, Edx),
            alu(Edi, Alu::Add, Reg(Ecx)),
            load(Cl, Esi, 0),
            Operation::Inc(Edx),
            store(Edi, 0, Cl),
        ],
    )?;
    m.jump(&mut cursor, outer)?;
    let mut cursor = epilogue;
    m.ops(
        &mut cursor,
        &[
            imm(Esp, Alu::Add, 12),
            Operation::Pop { target: Ebp },
            Operation::Pop { target: Edi },
            Operation::Pop { target: Esi },
        ],
    )?;
    if m.terminator(&mut cursor)?
        != (ProcedureTerminatorKind::Return {
            width: Width::Dword,
            pop_bytes: 8,
        })
    {
        return None;
    }
    (m.visited.len() == total).then_some(RleKernel)
}

#[cfg(test)]
#[path = "rle_kernel_tests.rs"]
mod tests;
