//! Structural recognition for a complete paired-byte strided copy procedure.

use super::*;
use crate::static_program::{Alu, Condition, Register, Shift, ShiftCount, Source, memory};

/// A semantic copy contract, independent of guest addresses or executable names.
/// The first version admits DS:[ESI] source RAM and DS:[EDI] planar VGA writes.
/// For a positive ECX, byte i is copied to destination i * stride. Zero declines:
/// the source idiom has a distinct one-byte behavior for zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CopyKernel {
    pub stride: u32,
    pub max_work_per_call: u32,
    pub prefix_nops: u32,
    pub tail_nops: u32,
}

impl CopyKernel {
    /// Validate metadata reconstructed by an owner, for example from a manifest.
    pub fn is_valid(self) -> bool {
        self.stride != 0
            && self.stride <= u32::MAX / 2
            && (1..=MAX_WORK_PER_CALL).contains(&self.max_work_per_call)
            && self.prefix_nops <= MAX_PROCEDURE_OPERATIONS as u32
            && self.tail_nops <= MAX_PROCEDURE_OPERATIONS as u32
            && self.prefix_nops + self.tail_nops <= MAX_PROCEDURE_OPERATIONS as u32 - 15
    }

    /// Exact original guest instruction count, including padding and RET.
    /// None means that this call must decline before any architectural effects.
    pub fn actual_work(self, count: u32) -> Option<u32> {
        if !self.is_valid() || count == 0 {
            return None;
        }
        let work = if count == 1 {
            6
        } else {
            let work = 6u64 + u64::from(self.prefix_nops) + 7 * u64::from(count / 2);
            work + u64::from(count & 1) * (2 + u64::from(self.tail_nops))
        };
        (work <= u64::from(self.max_work_per_call)).then_some(work as u32)
    }

    /// Number of original sites in canonical fallthrough/loop/tail order.
    /// The owner must independently prove this source CFG order before using
    /// these indices; sorted source addresses alone are insufficient.
    pub fn canonical_sites(self) -> Option<usize> {
        self.is_valid()
            .then_some(15 + self.prefix_nops as usize + self.tail_nops as usize)
    }

    /// Exact dynamic repetitions of one original canonical instruction site.
    /// Includes padding, all three conditional branches and the outer RET.
    pub fn canonical_repetitions(self, count: u32, site: usize) -> Option<u32> {
        self.actual_work(count)?;
        let sites = self.canonical_sites()?;
        if site >= sites {
            return None;
        }
        let prefix = self.prefix_nops as usize;
        let tail = self.tail_nops as usize;
        let pairs = count / 2;
        Some(if site < 3 || site == sites - 1 {
            1
        } else if site < 3 + prefix {
            u32::from(pairs != 0)
        } else if site < 10 + prefix {
            pairs
        } else if site < 12 + prefix {
            u32::from(pairs != 0)
        } else if site < 12 + prefix + tail {
            u32::from(pairs != 0 && count & 1 != 0)
        } else {
            count & 1
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Cursor {
    block: usize,
    instruction: usize,
}

struct Matcher<'a> {
    function: &'a ProcedureGraphFunction,
    consumed: usize,
}

impl Matcher<'_> {
    fn target(&self, address: Address) -> Option<Cursor> {
        self.function
            .blocks
            .iter()
            .position(|b| b.entry == address)
            .map(|block| Cursor {
                block,
                instruction: 0,
            })
    }

    fn normalize(&self, mut cursor: Cursor) -> Option<Cursor> {
        // Validation established reachability, but a malformed matching path
        // must still never cause an unbounded setup traversal.
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

    fn operation(&mut self, cursor: &mut Cursor) -> Option<Operation> {
        *cursor = self.normalize(*cursor)?;
        let instruction = self.function.blocks[cursor.block]
            .instructions
            .get(cursor.instruction)?;
        cursor.instruction += 1;
        self.consumed += 1;
        Some(instruction.operation)
    }

    fn expect(&mut self, cursor: &mut Cursor, operation: Operation) -> Option<()> {
        (self.operation(cursor)? == operation).then_some(())
    }

    fn branch(&mut self, cursor: &mut Cursor, condition: Condition) -> Option<(Cursor, Cursor)> {
        *cursor = self.normalize(*cursor)?;
        let block = &self.function.blocks[cursor.block];
        if cursor.instruction != block.instructions.len() {
            return None;
        }
        let ProcedureTerminatorKind::Branch {
            condition: cc,
            taken,
            not_taken,
        } = block.terminator.kind
        else {
            return None;
        };
        if cc != condition {
            return None;
        }
        self.consumed += 1;
        Some((
            self.normalize(self.target(taken)?)?,
            self.normalize(self.target(not_taken)?)?,
        ))
    }

    fn padding(&mut self, cursor: &mut Cursor) -> Option<u32> {
        let mut count = 0;
        loop {
            *cursor = self.normalize(*cursor)?;
            if self.function.blocks[cursor.block]
                .instructions
                .get(cursor.instruction)
                .map(|i| i.operation)
                != Some(Operation::Nop)
            {
                return Some(count);
            }
            self.operation(cursor)?;
            count += 1;
        }
    }
}

fn address(base: Register, displacement: u32) -> memory::MemoryAddress {
    memory::MemoryAddress {
        base: Some(base),
        index: None,
        scale: 1,
        displacement,
        address16: false,
        segment: memory::Segment::Ds,
    }
}

fn alu(target: Register, kind: Alu, value: u32) -> Operation {
    Operation::Alu {
        target,
        kind,
        source: Source::Immediate(value),
    }
}

pub(super) fn recognize(function: &ProcedureGraphFunction, limit: u32) -> Option<CopyKernel> {
    let mut m = Matcher {
        function,
        consumed: 0,
    };
    let mut cursor = m.target(function.entry)?;
    m.expect(
        &mut cursor,
        Operation::Move {
            target: Register::Ebx,
            source: Source::Register(Register::Ecx),
        },
    )?;
    match m.operation(&mut cursor)? {
        Operation::Shift {
            target: Register::Ebx,
            kind: Shift::Shr,
            count: ShiftCount::One | ShiftCount::Immediate(1),
        } => {}
        _ => return None,
    }
    let (tail, mut cursor) = m.branch(&mut cursor, Condition::Equal)?;
    let prefix_nops = m.padding(&mut cursor)?;
    let header = cursor;
    m.expect(
        &mut cursor,
        Operation::LoadMemory {
            target: Register::Ax,
            address: address(Register::Esi, 0),
        },
    )?;
    m.expect(
        &mut cursor,
        Operation::StoreMemory {
            width: Width::Byte,
            source: Source::Register(Register::Al),
            address: address(Register::Edi, 0),
        },
    )?;
    m.expect(&mut cursor, alu(Register::Esi, Alu::Add, 2))?;
    let Operation::Alu {
        target: Register::Edi,
        kind: Alu::Add,
        source: Source::Immediate(pair_stride),
    } = m.operation(&mut cursor)?
    else {
        return None;
    };
    if pair_stride == 0 || pair_stride & 1 != 0 {
        return None;
    }
    let stride = pair_stride / 2;
    m.expect(
        &mut cursor,
        Operation::StoreMemory {
            width: Width::Byte,
            source: Source::Register(Register::Ah),
            address: address(Register::Edi, stride.wrapping_neg()),
        },
    )?;
    m.expect(&mut cursor, Operation::Dec(Register::Ebx))?;
    let (backedge, mut cursor) = m.branch(&mut cursor, Condition::NotEqual)?;
    if backedge != header {
        return None;
    }
    m.expect(&mut cursor, alu(Register::Ecx, Alu::And, 1))?;
    let (ret, mut cursor) = m.branch(&mut cursor, Condition::Equal)?;
    let tail_nops = m.padding(&mut cursor)?;
    if cursor != tail {
        return None;
    }
    m.expect(
        &mut cursor,
        Operation::LoadMemory {
            target: Register::Al,
            address: address(Register::Esi, 0),
        },
    )?;
    m.expect(
        &mut cursor,
        Operation::StoreMemory {
            width: Width::Byte,
            source: Source::Register(Register::Al),
            address: address(Register::Edi, 0),
        },
    )?;
    cursor = m.normalize(cursor)?;
    let block = &function.blocks[cursor.block];
    if cursor != ret
        || cursor.instruction != block.instructions.len()
        || block.terminator.kind
            != (ProcedureTerminatorKind::Return {
                width: Width::Dword,
                pop_bytes: 0,
            })
    {
        return None;
    }
    m.consumed += 1;
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
    let kernel = CopyKernel {
        stride,
        max_work_per_call: limit,
        prefix_nops,
        tail_nops,
    };
    (m.consumed == total && kernel.is_valid()).then_some(kernel)
}

#[cfg(test)]
#[path = "copy_kernel_tests.rs"]
mod tests;
