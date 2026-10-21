//! Static termination proofs and loop-forest planning for ordinary CPU IR.
//!
//! All cycles must be natural loops with one INC/DEC; JNZ latch. Contracting
//! child loops leaves a DAG, so each continuing iteration reaches its latch
//! exactly once. Child counters may temporarily occupy known-zero upper bits
//! of a constant-initialized outer countdown: every normal child exit restores
//! those bits to zero. One otherwise unknown dword induction may be guarded at
//! loop entry against a statically derived whole-function work bound. Unknown
//! writes, irreducible cycles and read-only data waits decline.

use super::*;
use crate::static_program::{Alu, Register, Source};

/// Keeps exact i32 completed-work accounting below the owner's atomic ceiling.
pub const MAX_FINITE_WORK: u32 = 469_762_071;

#[derive(Clone, Copy, Default)]
struct Set([u64; MAX_PROCEDURE_BLOCKS / 64]);

impl Set {
    fn insert(&mut self, index: usize) {
        self.0[index / 64] |= 1 << (index % 64);
    }
    fn has(self, index: usize) -> bool {
        self.0[index / 64] & (1 << (index % 64)) != 0
    }
    fn subset(self, other: Self) -> bool {
        self.0.iter().zip(other.0).all(|(&a, b)| a & !b == 0)
    }
    fn intersects(self, other: Self) -> bool {
        self.0.iter().zip(other.0).any(|(&a, b)| a & b != 0)
    }
    fn len(self) -> u32 {
        self.0.iter().map(|v| v.count_ones()).sum()
    }
}

pub(in crate::static_program) struct FiniteLoop {
    pub header: usize,
    pub latch: usize,
    pub exit: usize,
    induction_block: usize,
    induction_position: usize,
    members: Set,
    parent: Option<usize>,
    pub counter: Register,
    pub decrement: bool,
    pub trips: u32,
    pub guarded: bool,
    /// Bits which may be nonzero at iteration boundaries, in backing GPR space.
    live_bits: u32,
}

pub(in crate::static_program) struct FiniteRegion {
    /// Original block indexes; a child loop is represented by its header.
    pub order: Vec<usize>,
    pub positions: Vec<usize>,
    pub children: Vec<Option<usize>>,
    pub maximum_work: u32,
    /// Conservative affine envelope A * live_trips + B. At joins the two
    /// coefficients are maximized independently, avoiding unsound extrapolation
    /// from longest-path samples when different arms win at different counts.
    slope: u64,
    constant: u64,
}

pub(in crate::static_program) struct FinitePlan {
    pub loops: Vec<FiniteLoop>,
    /// Root DAG followed by one iteration DAG for each loop, at loop index + 1.
    pub regions: Vec<FiniteRegion>,
}

fn mask(register: Register) -> u32 {
    (u32::MAX >> (32 - register.width().bits())) << register.shift()
}

/// Decoder subdivisions may put JNZ in its own block. Follow its unique plain
/// predecessor chain to the final ZF writer, across flag-preserving operations.
fn induction(
    function: &ProcedureGraphFunction,
    structure: &GraphStructure,
    latch: usize,
) -> Option<(usize, usize, Register, bool)> {
    let mut node = latch;
    for _ in 0..function.blocks.len() {
        for (position, instruction) in function.blocks[node].instructions.iter().enumerate().rev() {
            match instruction.operation {
                Operation::Inc(register) => return Some((node, position, register, false)),
                Operation::Dec(register) => return Some((node, position, register, true)),
                Operation::X87(_)
                | Operation::Nop
                | Operation::Move { .. }
                | Operation::Extend { .. }
                | Operation::Lea { .. }
                | Operation::LoadMemory { .. }
                | Operation::ExtendMemory { .. }
                | Operation::StoreMemory { .. }
                | Operation::Push { .. }
                | Operation::Pop { .. }
                | Operation::Not(_)
                | Operation::StringLoad { .. }
                | Operation::StringStore { .. } => {}
                _ => return None,
            }
        }
        let mut predecessors = structure
            .targets
            .iter()
            .enumerate()
            .filter(|(_, edges)| edges.contains(&Some(node)));
        let (predecessor, edges) = predecessors.next()?;
        if predecessors.next().is_some() || *edges != [Some(node), None] {
            return None;
        }
        node = predecessor;
    }
    None
}

/// Register writes are exact for disjoint byte aliases. Implicit stack writes
/// cover all ESP bits because SS.B belongs to the live descriptor.
fn writes(operation: Operation, register: Register) -> bool {
    let overlaps =
        |other: Register| other.index() == register.index() && mask(other) & mask(register) != 0;
    match operation {
        Operation::X87(_)
        | Operation::Nop
        | Operation::StoreMemory { .. }
        | Operation::CompareMemory { .. }
        | Operation::AluMemoryStore { .. }
        | Operation::UnaryMemory { .. } => false,
        Operation::Move { target, .. }
        | Operation::Extend { target, .. }
        | Operation::SignedMultiply { target, .. }
        | Operation::Lea { target, .. }
        | Operation::LoadMemory { target, .. }
        | Operation::ExtendMemory { target, .. }
        | Operation::SignedMultiplyMemory { target, .. }
        | Operation::Shift { target, .. }
        | Operation::DoubleShift { target, .. }
        | Operation::Inc(target)
        | Operation::Dec(target)
        | Operation::Neg(target)
        | Operation::Not(target) => overlaps(target),
        Operation::Alu { kind, target, .. } | Operation::AluMemory { kind, target, .. } => {
            !matches!(kind, Alu::Cmp | Alu::Test) && overlaps(target)
        }
        Operation::WideMultiply { width, .. } | Operation::WideMultiplyMemory { width, .. } => {
            match width {
                Width::Byte => overlaps(Register::Ax),
                Width::Word => overlaps(Register::Ax) || overlaps(Register::Dx),
                Width::Dword => overlaps(Register::Eax) || overlaps(Register::Edx),
            }
        }
        Operation::Push { .. } => overlaps(Register::Esp),
        Operation::Pop { target } => overlaps(Register::Esp) || overlaps(target),
        Operation::Exchange { left, right } => overlaps(left) || overlaps(right),
        Operation::StringLoad {
            width, address16, ..
        } => {
            overlaps(if address16 {
                Register::Si
            } else {
                Register::Esi
            }) || overlaps(match width {
                Width::Byte => Register::Al,
                Width::Word => Register::Ax,
                Width::Dword => Register::Eax,
            })
        }
        Operation::StringStore { address16, .. } => overlaps(if address16 {
            Register::Di
        } else {
            Register::Edi
        }),
    }
}

fn dominators(structure: &GraphStructure) -> Result<Vec<Set>, ProcedureCompileError> {
    let count = structure.targets.len();
    let mut all = Set::default();
    for i in 0..count {
        all.insert(i);
    }
    let mut dom = reserved(count)?;
    dom.resize(count, all);
    dom[structure.entry] = Set::default();
    dom[structure.entry].insert(structure.entry);
    loop {
        let mut changed = false;
        for &node in &structure.by_entry {
            if node == structure.entry {
                continue;
            }
            let mut value = all;
            for (source, edges) in structure.targets.iter().enumerate() {
                if edges.contains(&Some(node)) {
                    for (part, predecessor) in value.0.iter_mut().zip(dom[source].0) {
                        *part &= predecessor;
                    }
                }
            }
            value.insert(node);
            if value.0 != dom[node].0 {
                dom[node] = value;
                changed = true;
            }
        }
        if !changed {
            return Ok(dom);
        }
    }
}

/// A unique constant assignment dominates this header and executes again on
/// every enclosing-loop iteration. Reject other overlapping definitions outside
/// this loop, even when more elaborate reaching-definition analysis could pass.
fn initial_value(
    function: &ProcedureGraphFunction,
    loops: &[FiniteLoop],
    loop_index: usize,
    dom: &[Set],
) -> Option<u32> {
    let lp = &loops[loop_index];
    let mut value = None;
    for (index, block) in function.blocks.iter().enumerate() {
        if lp.members.has(index) {
            continue;
        }
        for instruction in &block.instructions {
            if !writes(instruction.operation, lp.counter) {
                continue;
            }
            let Operation::Move {
                target,
                source: Source::Immediate(constant),
            } = instruction.operation
            else {
                return None;
            };
            if target != lp.counter || value.is_some() || !dom[lp.header].has(index) {
                return None;
            }
            if lp
                .parent
                .is_some_and(|parent| !loops[parent].members.has(index))
            {
                return None;
            }
            value = Some(constant);
        }
    }
    value
}

fn region(
    function: &ProcedureGraphFunction,
    structure: &GraphStructure,
    loops: &[FiniteLoop],
    owner: Option<usize>,
    regions: &[Option<FiniteRegion>],
) -> Result<FiniteRegion, ProcedureCompileError> {
    let reject = || ProcedureCompileError::CyclicGraph(function.id);
    let count = function.blocks.len();
    let contains = |index| owner.is_none_or(|i| loops[i].members.has(index));
    let mut groups = reserved(count)?;
    groups.extend(0..count);
    let mut children = reserved(count)?;
    children.resize(count, None);
    for (child, lp) in loops.iter().enumerate() {
        if lp.parent == owner {
            children[lp.header] = Some(child);
            for (node, group) in groups.iter_mut().enumerate() {
                if lp.members.has(node) {
                    *group = lp.header;
                }
            }
        }
    }
    let edges = |index: usize| {
        if let Some(child) = children[index] {
            [Some(groups[loops[child].exit]), None]
        } else if owner.is_some_and(|i| loops[i].latch == index) {
            [None; 2]
        } else {
            structure.targets[index].map(|target| target.map(|i| groups[i]))
        }
    };
    let mut positions = reserved(count)?;
    positions.resize(count, usize::MAX);
    let mut indegree = reserved(count)?;
    indegree.resize(count, 0usize);
    let mut nodes = 0;
    for &index in &structure.by_entry {
        if contains(index) && groups[index] == index {
            nodes += 1;
            for target in edges(index).into_iter().flatten() {
                if !contains(target) {
                    return Err(reject());
                }
                indegree[target] += 1;
            }
        }
    }
    let mut order = reserved(nodes)?;
    for position in 0..nodes {
        let index = structure
            .by_entry
            .iter()
            .copied()
            .find(|&index| {
                contains(index)
                    && groups[index] == index
                    && positions[index] == usize::MAX
                    && indegree[index] == 0
                    && !(owner.is_some_and(|i| loops[i].latch == index) && position + 1 != nodes)
            })
            .ok_or_else(reject)?;
        positions[index] = position;
        order.push(index);
        for target in edges(index).into_iter().flatten() {
            indegree[target] -= 1;
        }
    }
    let entry = owner.map_or(structure.entry, |i| loops[i].header);
    if order.first() != Some(&groups[entry]) {
        return Err(reject());
    }
    let mut longest = reserved(count)?;
    longest.resize(count, 0u64);
    let mut slopes = reserved(count)?;
    slopes.resize(count, 0u64);
    let mut constants = reserved(count)?;
    constants.resize(count, 0u64);
    for &index in order.iter().rev() {
        let (work, slope, constant) = if let Some(child) = children[index] {
            let body = regions[child + 1].as_ref().ok_or_else(reject)?;
            let trips = u64::from(loops[child].trips);
            let work = u64::from(body.maximum_work) * trips;
            if loops[child].guarded {
                (work, u64::from(body.maximum_work), 0)
            } else {
                (work, body.slope * trips, body.constant * trips)
            }
        } else {
            let block = &function.blocks[index];
            let work = (block.instructions.len() + real_terminator(block.terminator.kind)) as u64;
            (work, 0, work)
        };
        longest[index] = work
            + edges(index)
                .into_iter()
                .flatten()
                .map(|target| longest[target])
                .max()
                .unwrap_or(0);
        slopes[index] = slope
            + edges(index)
                .into_iter()
                .flatten()
                .map(|target| slopes[target])
                .max()
                .unwrap_or(0);
        constants[index] = constant
            + edges(index)
                .into_iter()
                .flatten()
                .map(|target| constants[target])
                .max()
                .unwrap_or(0);
        if longest[index] > u64::from(MAX_FINITE_WORK)
            || slopes[index] > u64::from(MAX_FINITE_WORK)
            || constants[index] > u64::from(MAX_FINITE_WORK)
        {
            return Err(reject());
        }
    }
    Ok(FiniteRegion {
        order,
        positions,
        children,
        maximum_work: longest[groups[entry]] as u32,
        slope: slopes[groups[entry]],
        constant: constants[groups[entry]],
    })
}

pub(super) fn plan(
    function: &ProcedureGraphFunction,
    structure: &GraphStructure,
) -> Result<GraphPlan, ProcedureCompileError> {
    let reject = || ProcedureCompileError::CyclicGraph(function.id);
    let count = function.blocks.len();
    let dom = dominators(structure)?;
    let mut loops: Vec<FiniteLoop> = reserved(count)?;
    let mut pending = reserved(count)?;
    for &latch in &structure.by_entry {
        for header in structure.targets[latch].into_iter().flatten() {
            if !dom[latch].has(header) {
                continue;
            }
            if loops.iter().any(|lp| lp.header == header) {
                return Err(reject());
            }
            let ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                ..
            } = function.blocks[latch].terminator.kind
            else {
                return Err(reject());
            };
            let [Some(taken), Some(exit)] = structure.targets[latch] else {
                return Err(reject());
            };
            if taken != header || exit == header {
                return Err(reject());
            }
            let (induction_block, induction_position, counter, decrement) =
                induction(function, structure, latch).ok_or_else(reject)?;
            let mut members = Set::default();
            members.insert(header);
            if latch != header {
                members.insert(latch);
                pending.push(latch);
            }
            while let Some(node) = pending.pop() {
                for (source, edges) in structure.targets.iter().enumerate() {
                    if edges.contains(&Some(node)) && !members.has(source) {
                        if !dom[source].has(header) {
                            return Err(reject());
                        }
                        members.insert(source);
                        pending.push(source);
                    }
                }
            }
            if members.has(exit) {
                return Err(reject());
            }
            if !members.has(induction_block) {
                return Err(reject());
            }
            // Only the tested latch may leave this natural loop. Terminal RET
            // and interpreter exits are also finite and simply finish early.
            for (source, edges) in structure.targets.iter().enumerate() {
                if members.has(source)
                    && source != latch
                    && edges.iter().flatten().any(|&target| !members.has(target))
                {
                    return Err(reject());
                }
            }
            loops.push(FiniteLoop {
                header,
                latch,
                exit,
                induction_block,
                induction_position,
                members,
                parent: None,
                counter,
                decrement,
                trips: 0,
                guarded: false,
                live_bits: mask(counter),
            });
        }
    }
    for a in 0..loops.len() {
        for b in 0..loops.len() {
            if a == b || !loops[a].members.intersects(loops[b].members) {
                continue;
            }
            if loops[a].members.0 == loops[b].members.0 {
                return Err(reject());
            }
            if loops[a].members.subset(loops[b].members)
                && loops[a].members.len() < loops[b].members.len()
            {
                if loops[a]
                    .parent
                    .is_none_or(|parent| loops[b].members.len() < loops[parent].members.len())
                {
                    loops[a].parent = Some(b);
                }
            } else if !loops[b].members.subset(loops[a].members) {
                return Err(reject());
            }
        }
    }
    let mut guarded_loop = None;
    for i in 0..loops.len() {
        let initial = initial_value(function, &loops, i, &dom);
        let bits = loops[i].counter.width().bits();
        let trips = match initial {
            Some(value) if loops[i].decrement && value != 0 => u64::from(value),
            Some(value) if !loops[i].decrement && value != 0 => (1u64 << bits) - u64::from(value),
            None if bits == 32 => {
                if guarded_loop.replace(i).is_some() {
                    return Err(reject());
                }
                loops[i].guarded = true;
                // A placeholder for scalar planning; the symbolic envelope
                // below derives the accepted live trip cap for this loop.
                1
            }
            _ => 1u64 << bits,
        };
        if trips > u64::from(MAX_FINITE_WORK) {
            return Err(reject());
        }
        loops[i].trips = trips as u32;
        if let Some(value) = initial.filter(|&v| v != 0 && loops[i].decrement) {
            loops[i].live_bits = (u32::MAX >> value.leading_zeros()) << loops[i].counter.shift();
        }
    }
    // All writes are checked, including implicit accumulator/stack/string
    // destinations. An inner tested induction leaves zero on normal exit, so
    // it may use known-zero upper bits of an enclosing bounded countdown.
    for (i, lp) in loops.iter().enumerate() {
        for (index, block) in function.blocks.iter().enumerate() {
            if !lp.members.has(index) {
                continue;
            }
            for (position, instruction) in block.instructions.iter().enumerate() {
                if !writes(instruction.operation, lp.counter)
                    || (index == lp.induction_block && position == lp.induction_position)
                {
                    continue;
                }
                let restored = loops.iter().enumerate().any(|(child, inner)| {
                    child != i
                        && inner.members.subset(lp.members)
                        && inner.induction_block == index
                        && position == inner.induction_position
                        && inner.counter.index() == lp.counter.index()
                        && mask(inner.counter) & lp.live_bits == 0
                });
                if !restored {
                    return Err(reject());
                }
            }
        }
    }
    let mut regions = reserved(loops.len() + 1)?;
    regions.resize_with(loops.len() + 1, || None);
    // Smallest natural loops first, so their exact conservative reservation is
    // available to each enclosing DAG. No input-address-specific policy exists.
    let mut inner_first = reserved(loops.len())?;
    inner_first.extend(0..loops.len());
    inner_first.sort_unstable_by_key(|&i| {
        (
            loops[i].members.len(),
            function.blocks[loops[i].header].entry,
        )
    });
    for i in inner_first {
        regions[i + 1] = Some(region(function, structure, &loops, Some(i), &regions)?);
    }
    regions[0] = Some(region(function, structure, &loops, None, &regions)?);
    let root = regions[0].as_ref().unwrap();
    let maximum_work = if let Some(index) = guarded_loop {
        if root.slope == 0 {
            return Err(reject());
        }
        let trips = (u64::from(MAX_FINITE_WORK) - root.constant) / root.slope;
        if trips == 0 {
            return Err(reject());
        }
        loops[index].trips = trips as u32;
        root.slope * trips + root.constant
    } else {
        u64::from(root.maximum_work)
    };
    let mut finished = reserved(regions.len())?;
    finished.extend(regions.into_iter().map(Option::unwrap));
    let mut targets = reserved(count)?;
    targets.extend_from_slice(&structure.targets);
    Ok(GraphPlan {
        order: Vec::new(),
        positions: Vec::new(),
        targets,
        maximum_work,
        bounded: None,
        x87: None,
        finite: Some(FinitePlan {
            loops,
            regions: finished,
        }),
    })
}
