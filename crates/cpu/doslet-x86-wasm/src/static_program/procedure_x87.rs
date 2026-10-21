//! Setup-time stack and initial-value liveness for exact PC24 procedures.
use super::*;
use crate::static_program::{X87Instruction, X87Operation};

pub(in crate::static_program) struct Plan {
    /// Physical slots relative to entry TOP, one starting TOP per block.
    pub tops: Vec<u8>,
    pub permissions: X87Procedure,
    pub effects: X87Effects,
}

/// Dependence on entry values and signed stack topology at declared exits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum X87EffectClass {
    Unknown,
    MetadataOnly,
    InitialValues,
    IndependentBalanced,
    IndependentUnbalanced,
}

/// A declared exit after its block's completed instruction prefix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct X87ExitBalance {
    pub address: Address,
    pub kind: ProcedureTerminatorKind,
    /// Signed stack depth change: pushes are positive, pops negative.
    pub delta: Option<i64>,
}

/// Setup analysis only; this does not authorize dropping retained x87 state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct X87Effects {
    pub class: X87EffectClass,
    pub read_mask: u8,
    pub write_mask: u8,
    pub initial_values_required: bool,
    pub reads_status: bool,
    pub reads_control: bool,
    pub reads_pointers: bool,
    pub writes_status: bool,
    pub writes_control: bool,
    pub writes_pointers: bool,
    /// Signed depth extrema over all instruction boundaries, including entry.
    /// Unknown for inconsistent joins, nonzero-depth cycles or overflow.
    pub stack_min: Option<i64>,
    pub stack_max: Option<i64>,
    pub exit_balances: Vec<X87ExitBalance>,
    pub summary_known: bool,
    pub declared_exits_balanced: bool,
    /// Includes budget, memory-proof and numeric refusal boundaries. Declared
    /// balanced exits alone never establish this stronger property.
    pub all_boundaries_balanced: bool,
    pub preserves_full_state: bool,
}

impl X87Effects {
    pub(in crate::static_program) fn try_clone(&self) -> Result<Self, ProcedureCompileError> {
        let mut exit_balances = reserved(self.exit_balances.len())?;
        exit_balances.extend_from_slice(&self.exit_balances);
        Ok(Self {
            exit_balances,
            class: self.class,
            read_mask: self.read_mask,
            write_mask: self.write_mask,
            initial_values_required: self.initial_values_required,
            reads_status: self.reads_status,
            reads_control: self.reads_control,
            reads_pointers: self.reads_pointers,
            writes_status: self.writes_status,
            writes_control: self.writes_control,
            writes_pointers: self.writes_pointers,
            stack_min: self.stack_min,
            stack_max: self.stack_max,
            summary_known: self.summary_known,
            declared_exits_balanced: self.declared_exits_balanced,
            all_boundaries_balanced: self.all_boundaries_balanced,
            preserves_full_state: self.preserves_full_state,
        })
    }
}

fn depth_delta(operation: X87Operation) -> i64 {
    use X87Operation::*;
    match operation {
        LoadReal32 { .. } | LoadInteger32 { .. } | Duplicate { .. } => 1,
        ArithmeticRegister { pop, .. }
        | CompareReal32 { pop, .. }
        | StoreRegister { pop, .. }
        | StoreInteger32 { pop, .. } => -i64::from(pop),
        CompareRegister { pops, .. } => -i64::from(pops),
        _ => 0,
    }
}

fn summarize(
    function: &ProcedureGraphFunction,
    targets: &[[Option<usize>; 2]],
    order: &[usize],
    permissions: X87Procedure,
) -> Result<X87Effects, ProcedureCompileError> {
    let mut depths = reserved(function.blocks.len())?;
    depths.resize(function.blocks.len(), None::<i64>);
    depths[order[0]] = Some(0);
    let mut known = true;
    let (mut minimum, mut maximum) = (0, 0);

    let mut writes_status = false;
    let mut exits = reserved(function.blocks.len())?;
    for &index in order {
        let mut depth = depths[index];
        for instruction in &function.blocks[index].instructions {
            if let Operation::X87(instruction) = instruction.operation {
                writes_status |= !matches!(instruction.operation, X87Operation::StoreStatusAx);
                depth =
                    depth.and_then(|value| value.checked_add(depth_delta(instruction.operation)));
                if let Some(value) = depth {
                    minimum = minimum.min(value);
                    maximum = maximum.max(value);
                } else {
                    known = false;
                }
            }
        }
        for target in targets[index].into_iter().flatten() {
            if depths[target].is_none() {
                depths[target] = depth;
            } else if depths[target] != depth {
                known = false;
            }
        }
        if targets[index].iter().all(Option::is_none) {
            let terminator = function.blocks[index].terminator;
            exits.push(X87ExitBalance {
                address: terminator.address,
                kind: terminator.kind,
                delta: depth,
            });
        }
    }
    if !known {
        for exit in &mut exits {
            exit.delta = None;
        }
    }
    let balanced = known && !exits.is_empty() && exits.iter().all(|exit| exit.delta == Some(0));
    let initial_values_required = permissions.read_mask != 0;
    Ok(X87Effects {
        class: if !known {
            X87EffectClass::Unknown
        } else if !writes_status {
            X87EffectClass::MetadataOnly
        } else if initial_values_required {
            X87EffectClass::InitialValues
        } else if balanced {
            X87EffectClass::IndependentBalanced
        } else {
            X87EffectClass::IndependentUnbalanced
        },
        read_mask: permissions.read_mask,
        write_mask: permissions.write_mask,
        initial_values_required,
        // Status is preserved/updated by partial bit writes and is explicitly
        // observed by FNSTSW even when no value slot is read.
        reads_status: true,
        // Entry admission checks PC24 precision and masked exceptions even
        // for metadata-only instructions.
        reads_control: true,
        reads_pointers: false,
        writes_status,
        writes_control: false,
        writes_pointers: writes_status,
        stack_min: known.then_some(minimum),
        stack_max: known.then_some(maximum),
        exit_balances: exits,
        summary_known: known,
        declared_exits_balanced: balanced,
        all_boundaries_balanced: false,
        preserves_full_state: !writes_status,
    })
}

/// Reads, value definitions, writable tag/value slots and next relative TOP.
pub(in crate::static_program) fn effects(instruction: X87Instruction, top: u8) -> (u8, u8, u8, u8) {
    let slot = |index: u8| (top + index) & 7;
    let bit = |index: u8| 1u8 << slot(index);
    let push = (top + 7) & 7;
    use X87Operation::*;
    match instruction.operation {
        LoadReal32 { .. } | LoadInteger32 { .. } => (0, 1 << push, 1 << push, push),
        Duplicate { index } => (bit(index), 1 << push, 1 << push, push),
        Exchange { index } => {
            let bits = bit(0) | bit(index);
            (bits, bits, bits, top)
        }
        ArithmeticReal32 { .. } => (bit(0), bit(0), bit(0), top),
        ArithmeticRegister {
            index,
            target_top,
            pop,
            ..
        } => {
            let destination = if target_top { bit(0) } else { bit(index) };
            (
                bit(0) | bit(index),
                destination,
                destination | if pop { bit(0) } else { 0 },
                if pop { slot(1) } else { top },
            )
        }
        StoreStatusAx => (0, 0, 0, top),
        CompareReal32 { pop, .. } => (
            bit(0),
            0,
            if pop { bit(0) } else { 0 },
            if pop { slot(1) } else { top },
        ),
        CompareRegister { index, pops } => {
            let popped = if pops == 2 {
                bit(0) | bit(1)
            } else if pops == 1 {
                bit(0)
            } else {
                0
            };
            (bit(0) | bit(index) | popped, 0, popped, slot(pops))
        }
        StoreRegister { index, pop } => (
            bit(0),
            bit(index),
            bit(index) | if pop { bit(0) } else { 0 },
            if pop { slot(1) } else { top },
        ),
        StoreInteger32 { pop, .. } => (
            bit(0),
            0,
            if pop { bit(0) } else { 0 },
            if pop { slot(1) } else { top },
        ),
    }
}

pub(in crate::static_program) fn plan(
    function: &ProcedureGraphFunction,
    targets: &[[Option<usize>; 2]],
) -> Result<Option<Plan>, ProcedureCompileError> {
    // Public IR is untrusted setup input. Validate every shape/address before
    // effects performs slot arithmetic, including malformed opcodes/indices.
    let mut position = 0;
    for block in &function.blocks {
        for instruction in &block.instructions {
            if matches!(instruction.operation, Operation::X87(_)) {
                crate::static_program::validate_operation(
                    &instruction.operation,
                    RegionId(function.id.0),
                    true,
                )
                .map_err(|reason| ProcedureCompileError::InvalidOperation {
                    procedure: function.id,
                    operation: position,
                    reason,
                })?;
            }
            position += 1;
        }
    }
    if !function.blocks.iter().any(|b| {
        b.instructions
            .iter()
            .any(|i| matches!(i.operation, Operation::X87(_)))
    }) {
        return Ok(None);
    }
    let invalid = || ProcedureCompileError::InvalidAddresses(function.id);
    let count = function.blocks.len();
    let mut tops = reserved(count)?;
    tops.resize(count, u8::MAX);
    let entry = function
        .blocks
        .iter()
        .position(|b| b.entry == function.entry)
        .ok_or_else(invalid)?;
    tops[entry] = 0;
    let mut queue = reserved(count)?;
    queue.push(entry);
    let mut read = reserved(count)?;
    read.resize(count, 0u8);
    let mut defined = reserved(count)?;
    defined.resize(count, 0u8);
    let mut writes = 0u8;
    let mut cursor = 0;
    while cursor < queue.len() {
        let index = queue[cursor];
        cursor += 1;
        let mut top = tops[index];
        for instruction in &function.blocks[index].instructions {
            if let Operation::X87(instruction) = instruction.operation {
                let (r, d, w, next) = effects(instruction, top);
                read[index] |= r & !defined[index];
                defined[index] |= d;
                writes |= w;
                top = next;
            }
        }
        for target in targets[index].into_iter().flatten() {
            if tops[target] == u8::MAX {
                tops[target] = top;
                queue.push(target);
            } else if tops[target] != top {
                return Err(invalid());
            }
        }
    }
    if queue.len() != count {
        return Err(invalid());
    }
    // Backward union liveness converges after at most eight bits per block.
    let mut live = reserved(count)?;
    live.extend_from_slice(&read);
    loop {
        let mut changed = false;
        for index in 0..count {
            let out = targets[index]
                .into_iter()
                .flatten()
                .fold(0, |bits, target| bits | live[target]);
            let value = read[index] | (out & !defined[index]);
            if live[index] != value {
                live[index] = value;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let permissions = X87Procedure {
        read_mask: live[entry],
        write_mask: writes,
    };
    let effects = summarize(function, targets, &queue, permissions)?;
    Ok(Some(Plan {
        tops,
        permissions,
        effects,
    }))
}

#[cfg(test)]
#[path = "procedure_x87_effect_tests.rs"]
mod effect_tests;
