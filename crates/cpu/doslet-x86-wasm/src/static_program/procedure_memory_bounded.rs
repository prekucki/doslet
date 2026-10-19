//! Collapse one linear natural loop into an ordinary acyclic CFG node.
//!
//! This deliberately rejects irreducible graphs, nested loops and branches
//! inside the loop body. There is no guest-PC dispatcher in generated code.

use super::*;

pub(in crate::static_program) struct LoopPlan {
    /// Ordered body including the final JMP/Jcc, starting at its sole entry.
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
    // Only the conditional latch may branch inside the natural loop. Try
    // headers in source order, making output independent of input block order.
    for &header in by_entry {
        body.clear();
        members.fill(false);
        let mut index = header;
        let mut found = false;
        while !members[index] {
            members[index] = true;
            body.push(index);
            let [first, second] = targets[index];
            if first == Some(header) || second == Some(header) {
                found = true;
                break;
            }
            match (first, second) {
                (Some(next), None) => index = next,
                (Some(next), Some(other)) if next == other => index = next,
                _ => break,
            }
        }
        if !found || (members[entry] && entry != header) {
            continue;
        }
        let latch = *body.last().unwrap();
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
        });
    }
    Err(ProcedureCompileError::CyclicGraph(function.id))
}
