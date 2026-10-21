//! Collapse one linear natural loop into an ordinary acyclic CFG node.
//!
//! Conditional early exits may leave the otherwise linear continuation path.
//! Irreducible graphs, nested loops and internal diamonds remain unsupported.
//! There is no guest-PC dispatcher in generated code.

use super::*;

pub(in crate::static_program) struct LoopPlan {
    /// Ordered continuation path including the final JMP/Jcc. Other arms may
    /// leave this loop, but cannot reach any member again.
    pub blocks: Vec<usize>,
    pub work: usize,
    pub limit: u32,
}

pub(super) fn plan(
    function: &ProcedureGraphFunction,
    targets: Vec<[Option<usize>; 2]>,
    by_entry: &[usize],
    entry: usize,
    limit: u32,
) -> Result<GraphPlan, ProcedureCompileError> {
    let count = function.blocks.len();
    let mut body = reserved(count)?;
    let mut members = reserved(count)?;
    members.resize(count, false);
    let mut incoming = reserved(count)?;
    incoming.resize(count, 0usize);
    let mut positions = reserved(count)?;
    positions.resize(count, usize::MAX);
    let mut order = reserved(count)?;
    // Reverse adjacency distinguishes an exit arm from a second continuing
    // path. At most two edge sources and one end offset per block add three
    // usize values to the validator's cumulative sixteen-usize/two-boolean
    // bound, including on 32-bit targets. Reuse the existing scratch for BFS.
    for edges in &targets {
        for &target in edges.iter().flatten() {
            incoming[target] += 1;
        }
    }
    let mut reverse_ends = reserved(count)?;
    let mut total = 0;
    for &degree in &incoming {
        reverse_ends.push(total + degree);
        total += degree;
    }
    let mut reverse_sources = reserved(total)?;
    reverse_sources.resize(total, 0usize);
    for index in 0..count {
        positions[index] = if index == 0 {
            0
        } else {
            reverse_ends[index - 1]
        };
    }
    for (source, edges) in targets.iter().enumerate() {
        for &target in edges.iter().flatten() {
            reverse_sources[positions[target]] = source;
            positions[target] += 1;
        }
    }
    // Try headers in source order so input block ordering cannot affect output.
    for &header in by_entry {
        body.clear();
        members.fill(false);
        members[header] = true;
        body.push(header);
        let mut cursor = 0;
        while cursor < body.len() {
            let node = body[cursor];
            let start = if node == 0 { 0 } else { reverse_ends[node - 1] };
            for &source in &reverse_sources[start..reverse_ends[node]] {
                if !members[source] {
                    members[source] = true;
                    body.push(source);
                }
            }
            cursor += 1;
        }
        // `members` now means "can reach header". The continued path must
        // have exactly one such successor until its backward latch is reached.
        body.clear();
        order.clear();
        order.resize(count, usize::MAX);
        let mut index = header;
        let mut found = false;
        while order[index] == usize::MAX {
            order[index] = body.len();
            body.push(index);
            let [first, second] = targets[index];
            if first == Some(header) || second == Some(header) {
                found = true;
                break;
            }
            let mut continuing = [first, second]
                .into_iter()
                .flatten()
                .filter(|&target| members[target]);
            let Some(next) = continuing.next() else {
                break;
            };
            if continuing.any(|other| other != next) {
                break;
            }
            index = next;
        }
        // Restore membership to the chosen path for contraction and entry
        // validation. Reverse adjacency remains intact for the next candidate.
        members.fill(false);
        for &member in &body {
            members[member] = true;
        }
        if !found || (members[entry] && entry != header) {
            continue;
        }
        let latch = *body.last().unwrap();
        // A rotated procedure entry can make a synthetic forward fallthrough
        // close this path. The bounded emitter reserves and charges one real
        // JMP/Jcc at its latch; zero-work fallthrough latches are unsupported.
        if !matches!(
            function.blocks[latch].terminator.kind,
            ProcedureTerminatorKind::Jump(_) | ProcedureTerminatorKind::Branch { .. }
        ) {
            continue;
        }
        if targets[latch]
            .iter()
            .flatten()
            .any(|&target| members[target] && target != header)
        {
            continue;
        }
        if targets.iter().enumerate().any(|(source, edges)| {
            !members[source]
                && edges
                    .iter()
                    .flatten()
                    .any(|&target| members[target] && target != header)
        }) {
            continue;
        }
        // Contract all body blocks to header, then use the same deterministic
        // forward-label ordering as the acyclic compiler.
        let group = |i| if members[i] { header } else { i };
        incoming.fill(0);
        for (source, edges) in targets.iter().enumerate() {
            for &target in edges.iter().flatten() {
                if !(members[source] && members[target]) {
                    incoming[group(target)] += 1;
                }
            }
        }
        positions.fill(usize::MAX);
        order.clear();
        for position in 0..count - body.len() + 1 {
            let Some(next) = by_entry.iter().copied().find(|&i| {
                (!members[i] || i == header) && positions[i] == usize::MAX && incoming[i] == 0
            }) else {
                break;
            };
            positions[next] = position;
            order.push(next);
            for (source, edges) in targets.iter().enumerate() {
                if group(source) == next {
                    for &target in edges.iter().flatten() {
                        if group(target) != next {
                            incoming[group(target)] -= 1;
                        }
                    }
                }
            }
        }
        if order.len() != count - body.len() + 1 {
            continue;
        }
        for &member in &body {
            positions[member] = positions[header];
        }
        debug_assert_eq!(order[0], group(entry));
        let work = body
            .iter()
            .map(|&i| {
                let block = &function.blocks[i];
                block.instructions.len() + real_terminator(block.terminator.kind)
            })
            .sum();
        return Ok(GraphPlan {
            order,
            positions,
            targets,
            maximum_work: u64::from(limit),
            bounded: Some(LoopPlan {
                blocks: body,
                work,
                limit,
            }),
            finite: None,
            x87: None,
        });
    }
    Err(ProcedureCompileError::CyclicGraph(function.id))
}
