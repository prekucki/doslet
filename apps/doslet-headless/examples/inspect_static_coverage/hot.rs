//! Profile labels joined to exact relocated instructions and a bounded replay.
//! Candidate walks are inclusive, potentially overlapping CFG bodies, not ABI
//! proofs or host-cost measurements. Calls are recorded but never traversed.
use super::{Category, Entry, Result, Row, print_row, read, source_window};
use doslet_aot::{
    ObjectAddress,
    binding::{BindingLimits, DosFlatBindingPlan},
    cfg::{EdgeKind, EdgeTarget, StaticCfg},
    profile::{BoundGuidance, GuidanceLimits, Profile, ProfileLimits, Reason, Tier},
    wasm::memory_procedures::PreparedMemoryProcedures,
};
use doslet_runtime::Session;
use doslet_static_game_probe::prepare::bindings_from_json;
use doslet_x86::{Gpr32, SegmentRegister};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap},
    path::Path,
};

const MAX_BODY: usize = 4096;

#[derive(Clone, Copy, Debug)]
pub(super) struct Snapshot {
    gprs: [u32; 8],
    segment_bases: [u32; 3],
    video: (u8, u8, u8, u16, u8, u8),
}
impl Snapshot {
    pub(super) fn capture(session: &Session) -> Self {
        let registers = session.registers();
        Self {
            gprs: [
                Gpr32::Eax,
                Gpr32::Ecx,
                Gpr32::Edx,
                Gpr32::Ebx,
                Gpr32::Esp,
                Gpr32::Ebp,
                Gpr32::Esi,
                Gpr32::Edi,
            ]
            .map(|register| registers.get32(register)),
            segment_bases: [
                SegmentRegister::Ds,
                SegmentRegister::Es,
                SegmentRegister::Ss,
            ]
            .map(|segment| registers.segment_cache(segment).base),
            video: session.video_memory_configuration(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn report(
    rows: &[Row],
    graph: &StaticCfg<'_>,
    binding: &DosFlatBindingPlan<'_>,
    prepared: &PreparedMemoryProcedures,
    exe: &[u8],
    profile_path: &Path,
    historical_path: &Path,
    first_states: &HashMap<Entry, Snapshot>,
) -> Result<()> {
    let profile = Profile::parse(
        &read(profile_path, 64 * 1024 * 1024)?,
        ProfileLimits::default(),
    )?;
    let declarations = bindings_from_json(exe, &read(historical_path, 64 * 1024)?)?;
    let historical =
        DosFlatBindingPlan::new(graph.image(), &declarations, BindingLimits::default())?;
    let guidance = BoundGuidance::bind(
        &profile,
        graph,
        &historical,
        None,
        GuidanceLimits::default(),
    )?;
    let guidance = guidance.rebind_to(binding, GuidanceLimits::default())?;
    let verified: Vec<_> = guidance
        .instructions()
        .iter()
        .map(|instruction| instruction.verified_hot())
        .collect();
    let index = |address| {
        graph
            .nodes()
            .binary_search_by_key(&address, |node| node.address)
            .ok()
    };
    let mut visits = vec![0u64; graph.nodes().len()];
    let mut hot_rows = Vec::new();
    for row in rows {
        if !matches!(
            row.category,
            Category::KnownLowered | Category::KnownUnsupported
        ) {
            continue;
        }
        if let Some(at) = row.address.and_then(index) {
            visits[at] += row.count;
            if verified[at] {
                hot_rows.push(row);
            }
        }
    }
    println!(
        "HOT guidance: {:?}; identity={:?} identity_check={:?}; historical signatures and exact relocation bytes checked; labels carried by object-relative address",
        guidance.report(),
        profile.identity(),
        guidance.identity_check()
    );
    println!(
        "HOT verified instructions={} observed_HOT_steps={}; profile labels are not frequencies, CPU residency or whole-procedure safety proofs",
        verified.iter().filter(|&&hot| hot).count(),
        hot_rows.iter().map(|row| row.count).sum::<u64>()
    );
    println!("Observed verified HOT instructions (top30; replay counts):");
    for row in hot_rows.iter().take(30) {
        print_row(row);
    }
    println!(
        "First pre-instruction GPR samples (EAX,ECX,EDX,EBX,ESP,EBP,ESI,EDI; DS,ES,SS bases); video=(plane_mask,sequencer_memory,graphics_mode,CRTC_start,CRTC_14,CRTC_17), hexadecimal; examples only, not span proofs:"
    );
    for row in hot_rows.iter().take(8) {
        if let Some(snapshot) = first_states.get(&row.entry) {
            println!(
                "HOT_sample linear={:#x} gprs={:08x?} segment_bases={:08x?} video={:x?}",
                row.entry.linear, snapshot.gprs, snapshot.segment_bases, snapshot.video
            );
        }
    }
    // Missing CFG reachability and runtime-patched immediates are distinct
    // from missing profile labels. Match observed signatures independently,
    // rebasing only actual executable relocations. This is diagnostic evidence
    // about this observed instruction, never admission of mutable code.
    let mut outside_hot = Vec::new();
    for row in rows
        .iter()
        .filter(|row| matches!(row.category, Category::OutsideCfg | Category::ChangedBytes))
    {
        let Some(address) = row.address else { continue };
        let Some(source_object) = historical.object_binding(address.object_number) else {
            continue;
        };
        let Some(pc) = source_object.linear_base.checked_add(address.offset) else {
            continue;
        };
        let nodes = profile.nodes();
        let start = nodes.partition_point(|node| node.pc < pc);
        let group: Vec<_> = nodes[start..]
            .iter()
            .take_while(|node| node.pc == pc)
            .filter(|node| (node.bits == doslet_aot::DefaultBitness::Bits32) == row.entry.code32)
            .collect();
        if group.is_empty() || row.compared == 0 {
            continue;
        }
        let mut matches = true;
        for node in group {
            if node.tier != Tier::Hot
                || matches!(node.reason, Reason::CodeConflict | Reason::InterruptFrame)
            {
                matches = false;
                break;
            }
            let mut expected = node.bytes;
            for patch in historical
                .object_patches(address.object_number)
                .into_iter()
                .flatten()
            {
                if u64::from(patch.source.offset) >= u64::from(address.offset) + row.compared as u64
                {
                    break;
                }
                if u64::from(patch.source.offset) + patch.bytes().len() as u64
                    <= u64::from(address.offset)
                {
                    continue;
                }
                let Some(current_patch) =
                    binding
                        .object_patches(address.object_number)
                        .and_then(|patches| {
                            patches
                                .binary_search_by_key(&patch.source, |p| p.source)
                                .ok()
                                .map(|at| &patches[at])
                        })
                else {
                    matches = false;
                    break;
                };
                for (offset, &historical_byte) in patch.bytes().iter().enumerate() {
                    let at = u64::from(patch.source.offset) + offset as u64;
                    if at < u64::from(address.offset)
                        || at >= u64::from(address.offset) + row.compared as u64
                    {
                        continue;
                    }
                    let local = (at - u64::from(address.offset)) as usize;
                    if expected[local] != historical_byte {
                        matches = false;
                    }
                    let Some(&current_byte) = current_patch.bytes().get(offset) else {
                        matches = false;
                        break;
                    };
                    expected[local] = current_byte;
                }
            }
            matches &= expected[..row.compared] == row.entry.bytes[..row.compared];
        }
        if matches {
            outside_hot.push(row);
        }
    }
    println!(
        "Observed HOT signatures outside CFG or with changed static source (top20; original profile matched, executable relocations rebased; not immutable-code proofs): total_steps={}",
        outside_hot.iter().map(|row| row.count).sum::<u64>()
    );
    for row in outside_hot.iter().take(20) {
        print_row(row);
        if let Some(snapshot) = first_states.get(&row.entry) {
            println!(
                "HOT_sample linear={:#x} gprs={:08x?} segment_bases={:08x?} video={:x?}",
                row.entry.linear, snapshot.gprs, snapshot.segment_bases, snapshot.video
            );
        }
    }
    let mut roots = BTreeMap::<ObjectAddress, String>::new();
    for &entry in prepared.entries() {
        roots.insert(entry, "accepted acyclic memory leaf".into());
    }
    for diagnostic in prepared.diagnostics() {
        let bytes = graph
            .nodes()
            .binary_search_by_key(&diagnostic.instruction, |node| node.address)
            .ok()
            .and_then(|at| {
                let length = graph.nodes()[at].byte_len;
                source_window(binding, diagnostic.instruction, usize::from(length))
                    .map(|bytes| format!("{:02x?}", &bytes[..usize::from(length)]))
            })
            .unwrap_or_else(|| "unavailable".into());
        roots.insert(
            diagnostic.entry,
            format!(
                "{:?} at object{}+{:#x} bytes={bytes}",
                diagnostic.reason,
                diagnostic.instruction.object_number,
                diagnostic.instruction.offset
            ),
        );
    }
    let mut candidates = Vec::new();
    let mut stamps = vec![0usize; graph.nodes().len()];
    for (epoch, (&root, rejection)) in roots.iter().enumerate() {
        let Some(first) = index(root) else { continue };
        let mut body = Vec::new();
        let mut calls = 0usize;
        let mut incomplete = false;
        let epoch = epoch + 1;
        stamps[first] = epoch;
        body.push(first);
        let mut cursor = 0;
        while cursor < body.len() {
            let address = graph.nodes()[body[cursor]].address;
            let start = graph.edges().partition_point(|edge| edge.source < address);
            for edge in graph.edges()[start..]
                .iter()
                .take_while(|edge| edge.source == address)
            {
                if edge.kind == EdgeKind::Call {
                    calls += 1;
                    continue;
                }
                let EdgeTarget::Address(target) = edge.target else {
                    continue;
                };
                let Some(next) = index(target) else { continue };
                if stamps[next] == epoch {
                    continue;
                }
                if body.len() == MAX_BODY {
                    incomplete = true;
                    continue;
                }
                stamps[next] = epoch;
                body.push(next);
            }
            cursor += 1;
        }
        let hot_nodes = body.iter().filter(|&&at| verified[at]).count();
        if hot_nodes == 0 {
            continue;
        }
        let hot_steps: u64 = body
            .iter()
            .filter(|&&at| verified[at])
            .map(|&at| visits[at])
            .sum();
        let all_steps: u64 = body.iter().map(|&at| visits[at]).sum();
        let entry_visits = visits[first];
        let linear = binding
            .object_binding(root.object_number)
            .and_then(|object| object.linear_base.checked_add(root.offset))
            .ok_or("candidate root binding missing")?;
        candidates.push((
            hot_steps,
            hot_nodes,
            root,
            linear,
            all_steps,
            entry_visits,
            body.len(),
            calls,
            incomplete,
            rejection,
        ));
    }
    candidates.sort_unstable_by_key(|row| (Reverse(row.0), Reverse(row.1), row.2));
    println!(
        "HOT candidate CFG bodies (top40; inclusive overlapping bodies, calls not followed; <=4096 nodes each; counts must not be summed):"
    );
    for (
        hot_steps,
        hot_nodes,
        root,
        linear,
        all_steps,
        entry_visits,
        nodes,
        calls,
        incomplete,
        rejection,
    ) in candidates.iter().take(40)
    {
        println!(
            "HOT_candidate object{}+{:#x} linear={linear:#x} entry_visits={entry_visits} observed_HOT_steps={hot_steps} observed_body_steps={all_steps} static_HOT_nodes={hot_nodes} nodes={nodes} calls={calls} truncated={incomplete} admission={rejection}",
            root.object_number, root.offset
        );
        if let Some(snapshot) = rows
            .iter()
            .find(|row| row.address == Some(*root))
            .and_then(|row| first_states.get(&row.entry))
        {
            println!(
                "HOT_entry_sample linear={linear:#x} gprs={:08x?} segment_bases={:08x?} video={:x?}",
                snapshot.gprs, snapshot.segment_bases, snapshot.video
            );
        }
    }
    Ok(())
}
