//! Generate a procedure module for one executable under its live load layout:
//! capture where the loader placed each LE object, then compile the bound
//! procedures into one `procedures.wasm` with embedded metadata. The desktop
//! runs this in the background; the headless examples expose each step.
use doslet_aot::{
    DefaultBitness, ImageLimits, ObjectAddress, SymbolicImage,
    binding::{BindingLimits, DosFlatBindingPlan, DosFlatObjectBinding, DosFlatSelector},
    cfg::{CfgLimits, StaticCfg},
    wasm::memory_procedures::{MemoryProcedureLimits, PreparedMemoryProcedures},
};
use doslet_executable::{LeFixupSourceType, LeImage, LeLimits};
use doslet_runtime::{Session, SessionStatus};
use doslet_x86::SegmentRegister;
use doslet_x86_wasm::static_program::procedure_memory::{
    ProcedureCompileOptions, ProcedureGraphProgram, ProcedureTerminatorKind, bundle,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, error::Error, time::Instant};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

pub struct Anchor {
    object: u32,
    offset: u32,
    bytes: [u8; 32],
}

/// Distinct, relocation-free 32-byte runs spread over the file-backed code.
pub fn anchors(image: &SymbolicImage) -> Vec<Anchor> {
    let mut result = Vec::new();
    for object in image
        .objects()
        .iter()
        .filter(|o| o.metadata().is_executable())
    {
        let mut fixed = vec![false; object.bytes().len()];
        for span in object.file_backed_spans() {
            let start = span.object_offset as usize;
            fixed[start..start + span.byte_len as usize].fill(true);
        }
        for r in image
            .relocations()
            .filter(|r| r.source.object_number == object.object_number())
        {
            let start = r.source.offset as usize;
            fixed[start..start + r.kind.width() as usize].fill(false);
        }
        let stride = (object.bytes().len() / 8).max(32);
        for start in (0..object.bytes().len()).step_by(stride).take(8) {
            let end = (start + stride).min(object.bytes().len());
            for offset in start..end.saturating_sub(31) {
                if !fixed[offset..offset + 32].iter().all(|&b| b) {
                    continue;
                }
                let bytes = &object.bytes()[offset..offset + 32];
                let mut seen = [false; 256];
                for &byte in bytes {
                    seen[byte as usize] = true;
                }
                if seen.iter().filter(|&&v| v).count() < 10 {
                    continue;
                }
                result.push(Anchor {
                    object: object.object_number(),
                    offset: offset as u32,
                    bytes: bytes.try_into().unwrap(),
                });
                break;
            }
        }
        if result.len() >= 128 {
            break;
        }
    }
    result
}

fn winner<T: Copy + Ord>(votes: &BTreeMap<T, usize>) -> Option<T> {
    let mut ranked: Vec<_> = votes
        .iter()
        .map(|(&value, &count)| (count, value))
        .collect();
    ranked.sort_unstable_by(|a, b| b.cmp(a));
    let &(count, value) = ranked.first()?;
    (count >= 2 && ranked.get(1).is_none_or(|&(next, _)| count > next * 2)).then_some(value)
}

fn descriptor(session: &Session, selector: u16) -> Option<DosFlatSelector> {
    let control = session.control_registers();
    let table = if selector & 4 != 0 {
        control.ldtr?
    } else {
        control.gdtr
    };
    let offset = u32::from(selector & !7);
    if offset + 7 > u32::from(table.limit) {
        return None;
    }
    let mut raw = [0; 8];
    if !session.debug_read_memory(table.base.checked_add(offset)?, &mut raw)
        || raw[5] & 0x90 != 0x90
    {
        return None;
    }
    let base = u32::from(raw[2])
        | (u32::from(raw[3]) << 8)
        | (u32::from(raw[4]) << 16)
        | (u32::from(raw[7]) << 24);
    let mut limit = u32::from(raw[0]) | (u32::from(raw[1]) << 8) | (u32::from(raw[6] & 15) << 16);
    if raw[6] & 0x80 != 0 {
        limit = (limit << 12) | 0xfff;
    }
    Some(DosFlatSelector {
        value: selector,
        base,
        limit,
        default_bitness: if raw[6] & 0x40 != 0 {
            DefaultBitness::Bits32
        } else {
            DefaultBitness::Bits16
        },
    })
}

/// Bindings of every LE object in the running session. Preferred image
/// addresses never select a base; anchored live code and its relocated
/// operands do, and every executable fixup must match the loaded bytes.
pub fn discover(
    session: &Session,
    image: &SymbolicImage,
    anchors: &[Anchor],
) -> Result<Vec<DosFlatObjectBinding>> {
    let ram_end = usize::try_from(session.memory_usage().installed_bytes)?;
    let ram = session
        .ram_view(0x100000..ram_end)
        .ok_or("extended RAM unavailable")?;
    let mut votes = BTreeMap::<u32, BTreeMap<u32, usize>>::new();
    for anchor in anchors {
        let mut matches = Vec::new();
        for (offset, bytes) in ram.windows(32).enumerate() {
            if bytes != anchor.bytes {
                continue;
            }
            let Some(base) = (offset as u32 + 0x100000).checked_sub(anchor.offset) else {
                continue;
            };
            matches.push(base);
            if matches.len() > 64 {
                break;
            }
        }
        // Repeated fill patterns are poor anchors and must not create an
        // unbounded candidate table from guest-controlled bytes.
        if matches.len() <= 64 {
            for base in matches {
                *votes
                    .entry(anchor.object)
                    .or_default()
                    .entry(base)
                    .or_default() += 1;
            }
        }
    }
    let mut bases = BTreeMap::new();
    let mut active_code = false;
    for (&number, votes) in &votes {
        let size = image.object(number).ok_or("missing object")?.bytes().len() as u64;
        let pc = u64::from(session.debug_pc());
        // A loader may retain an unrelocated file copy elsewhere in RAM.
        // Only the candidate containing the active PC can anchor live code.
        let active: BTreeMap<_, _> = votes
            .iter()
            .filter(|&(&base, _)| pc >= u64::from(base) && pc < u64::from(base) + size)
            .map(|(&base, &count)| (base, count))
            .collect();
        if let Some(base) = winner(&active) {
            bases.insert(number, base);
            active_code = true;
        } else if let Some(base) = winner(votes) {
            bases.insert(number, base);
        }
    }
    if !active_code {
        return Err("not executing uniquely anchored LE code yet".into());
    }
    // Infer other objects from the relocated addresses in that anchored code.
    for _ in 0..image.objects().len() {
        let mut target_votes = BTreeMap::<u32, BTreeMap<u32, usize>>::new();
        for r in image.relocations() {
            let (Some(&base), Some(addend)) = (bases.get(&r.source.object_number), r.target.offset)
            else {
                continue;
            };
            if !matches!(
                r.kind,
                LeFixupSourceType::Offset32 | LeFixupSourceType::SelfRelative32
            ) {
                continue;
            }
            let mut raw = [0; 4];
            if !session.debug_read_memory(base.wrapping_add(r.source.offset), &mut raw) {
                continue;
            }
            let value = u32::from_le_bytes(raw);
            let target = match r.kind {
                LeFixupSourceType::Offset32 => value.wrapping_sub(addend),
                _ => value
                    .wrapping_add(base)
                    .wrapping_add(r.source.offset)
                    .wrapping_add(4)
                    .wrapping_sub(addend),
            };
            *target_votes
                .entry(r.target.object_number)
                .or_default()
                .entry(target)
                .or_default() += 1;
        }
        for (&number, votes) in &target_votes {
            if !bases.contains_key(&number)
                && let Some(base) = winner(votes)
            {
                bases.insert(number, base);
            }
        }
    }
    let mut result = Vec::new();
    for object in image.objects() {
        let number = object.object_number();
        let base = *bases
            .get(&number)
            .ok_or("object lacks relocated base evidence")?;
        let select = |alias: bool| -> Result<Option<DosFlatSelector>> {
            let mut values = BTreeMap::<u16, usize>::new();
            for r in image.relocations().filter(|r| {
                r.target.object_number == number
                    && r.kind == LeFixupSourceType::Selector16
                    && r.is_alias == alias
            }) {
                let source = bases
                    .get(&r.source.object_number)
                    .ok_or("selector source missing")?;
                let mut raw = [0; 2];
                if session.debug_read_memory(source.wrapping_add(r.source.offset), &mut raw) {
                    *values.entry(u16::from_le_bytes(raw)).or_default() += 1;
                }
            }
            let observed = match values.len() {
                0 => None,
                1 => values.keys().next().copied(),
                _ => return Err("conflicting live selector fixups".into()),
            };
            let bits = if alias {
                DefaultBitness::Bits16
            } else {
                object.default_bitness()
            };
            if let Some(value) = observed {
                return descriptor(session, value)
                    .map(Some)
                    .ok_or_else(|| "live selector descriptor absent".into());
            }
            if bits == DefaultBitness::Bits32 {
                let segment = if object.metadata().is_executable() {
                    SegmentRegister::Cs
                } else {
                    SegmentRegister::Ds
                };
                return descriptor(session, session.registers().segment_cache(segment).selector)
                    .map(Some)
                    .ok_or_else(|| "flat descriptor absent".into());
            }
            let control = session.control_registers();
            let mut candidates = Vec::new();
            let mut matching_base = Vec::new();
            for (table, ti) in [(Some(control.gdtr), 0), (control.ldtr, 4)] {
                let Some(table) = table else {
                    continue;
                };
                for index in 1..(u32::from(table.limit) + 1) / 8 {
                    let value = (index as u16 * 8) | ti;
                    if let Some(d) = descriptor(session, value)
                        && d.base == base
                    {
                        matching_base.push(d);
                        if d.limit == 0xffff && d.default_bitness == bits {
                            candidates.push(d);
                        }
                    }
                }
            }
            if candidates.is_empty() && !alias {
                // Some loaded objects have addresses but no live selector.
                // The binder rejects this if a Selector16 fixup requires one;
                // the bound CFG never admits an address-only execution view.
                return Ok(None);
            }
            if candidates.len() != 1 {
                return Err(format!(
                    "object{number} base{base:#x} alias={alias}: narrow descriptor is absent or ambiguous; descriptors={matching_base:?}; bases={bases:?}; anchors={votes:?}"
                ).into());
            }
            Ok(Some(candidates[0]))
        };
        let normal = select(false)?;
        let needs_alias = image
            .relocations()
            .any(|r| r.target.object_number == number && r.is_alias);
        result.push(DosFlatObjectBinding {
            object_number: number,
            linear_base: base,
            normal,
            alias: if needs_alias {
                Some(select(true)?.ok_or("alias descriptor missing")?)
            } else {
                None
            },
        });
    }
    let plan = DosFlatBindingPlan::new(image, &result, BindingLimits::default())?;
    // Validate every relocation field in executable objects. Mutable data fields
    // may already have been updated by startup code and are not an exact oracle.
    for patch in plan.patches() {
        if !image
            .object(patch.source.object_number)
            .ok_or("patch object")?
            .metadata()
            .is_executable()
        {
            continue;
        }
        let base = bases[&patch.source.object_number];
        let mut raw = [0; 6];
        let bytes = &mut raw[..patch.bytes().len()];
        if !session.debug_read_memory(
            base.checked_add(patch.source.offset)
                .ok_or("patch address overflow")?,
            bytes,
        ) || bytes != patch.bytes()
        {
            return Err(format!(
                "loaded code fixup differs at object{}+{:#x}",
                patch.source.object_number, patch.source.offset
            )
            .into());
        }
    }
    Ok(result)
}

/// Run a prepared, not yet started session until its LE code is live and
/// anchored. Media writes stay in the session's temporary overlay.
pub fn capture(
    session: &mut Session,
    exe: &[u8],
    max_steps: u64,
    deadline: Instant,
) -> Result<Vec<DosFlatObjectBinding>> {
    let le = LeImage::discover(exe, LeLimits::default())?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    let anchors = anchors(&image);
    session.set_dynamic_core(false);
    session.set_decode_cache(true);
    session.set_idle_loops(false);
    session.set_vga_wait_acceleration(false);
    session.start();
    let mut next_probe = 0;
    let mut reason = String::from("protected-mode code was not reached");
    let mut pcm = [doslet_pcm::EMPTY; 4096];
    let mut midi = [0; 4096];
    while session.steps() < max_steps && Instant::now() < deadline {
        session.advance(4096.min((max_steps - session.steps()) as u32));
        while session.drain_pcm_timed(&mut pcm) != 0 {}
        while session.drain_midi(&mut midi) != 0 {}
        if session.status() != SessionStatus::Running {
            return Err(format!("guest {:?}: {:?}", session.status(), session.fault()).into());
        }
        let cs = session.registers().segment_cache(SegmentRegister::Cs);
        if session.steps() < next_probe
            || !cs.default32
            || cs.base != 0
            || cs.limit != u32::MAX
            || session.control_registers().cr0 & 0x8000_0001 != 1
        {
            continue;
        }
        next_probe = session.steps() + 100_000;
        match discover(session, &image, &anchors) {
            Ok(bindings) => return Ok(bindings),
            Err(error) => reason = error.to_string(),
        }
    }
    Err(format!(
        "live code layout not found after {} steps: {reason}",
        session.steps()
    )
    .into())
}

/// How procedures are selected and compiled.
pub struct Recipe {
    pub max_work_per_call: Option<u32>,
    pub copy_kernels: bool,
    pub computation_kernels: bool,
    pub kernels_only: bool,
    pub kernel_interrupts: Option<&'static str>,
    /// Recorded in the metadata when HOT guidance chose the entries.
    pub hot_profile: bool,
}
impl Recipe {
    /// The desktop default: whole finite computations and copy kernels,
    /// entered only before a pending IRQ would be delivered.
    pub const DESKTOP: Self = Self {
        max_work_per_call: Some(4096),
        copy_kernels: true,
        computation_kernels: true,
        kernels_only: false,
        kernel_interrupts: Some("before-irq"),
        hot_profile: false,
    };
}

pub struct Module {
    pub bytes: Vec<u8>,
    pub entries: usize,
}

/// Selection callback: the bound graph, the image and one entry's sources.
pub type Keep<'a> =
    dyn FnMut(&StaticCfg<'_>, &SymbolicImage, &[ObjectAddress]) -> Result<bool> + 'a;

/// Compile the procedures of `exe` bound at `bindings` into one module whose
/// metadata names `dpmi`, the mode that produced this layout.
pub fn build(
    exe: &[u8],
    bindings: &[DosFlatObjectBinding],
    dpmi: &str,
    recipe: &Recipe,
    keep: &mut Keep<'_>,
    report: &mut dyn FnMut(String),
) -> Result<Module> {
    let le = LeImage::discover(exe, LeLimits::default())?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    let binding = DosFlatBindingPlan::new(&image, bindings, BindingLimits::default())?;
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default())?;
    let prepare = if recipe.computation_kernels {
        PreparedMemoryProcedures::from_bound_graph_with_computations
    } else {
        PreparedMemoryProcedures::from_bound_graph
    };
    let prepared = prepare(
        &graph,
        &binding,
        MemoryProcedureLimits {
            max_work_per_call: recipe.max_work_per_call,
            ..MemoryProcedureLimits::default()
        },
    )?;
    let compile = |program: &ProcedureGraphProgram| {
        let options = ProcedureCompileOptions::default();
        match recipe.max_work_per_call {
            Some(limit) if recipe.computation_kernels => {
                program.compile_with_computation_kernels(&options, limit)
            }
            Some(limit) if recipe.copy_kernels => {
                program.compile_with_copy_kernels(&options, limit)
            }
            Some(limit) => program.compile_with_work_limit(&options, limit),
            None => program.compile(&options),
        }
    };
    let mut selected = Vec::new();
    let mut selected_indices = Vec::new();
    let mut declined = 0usize;
    for (index, function) in prepared.program().functions.iter().enumerate() {
        let sources = prepared.sources(index).ok_or("missing procedure sources")?;
        if !keep(&graph, &image, sources)? {
            continue;
        }
        let program = ProcedureGraphProgram {
            functions: vec![function.clone()],
        };
        match compile(&program) {
            Ok(compiled)
                if recipe.kernels_only
                    && compiled.semantic_kernel(function.id).is_none()
                    && compiled.rle_kernel(function.id).is_none()
                    && compiled.finite_maximum_work(function.id).is_none() =>
            {
                continue;
            }
            Ok(_) => {}
            Err(error) => {
                declined += 1;
                report(format!(
                    "Declined object{}+{:#x}: {error}",
                    prepared.entries()[index].object_number,
                    prepared.entries()[index].offset
                ));
                continue;
            }
        }
        selected.push(function.clone());
        selected_indices.push(index);
    }
    if selected.is_empty() {
        return Err("no eligible computational procedures selected".into());
    }
    let program = ProcedureGraphProgram {
        functions: selected,
    };
    let compiled = compile(&program)?;
    report(format!(
        "Selected {} of {} procedure/region entries; unsupported graph shapes={declined}; ordinary_work_limit={:?}",
        program.functions.len(),
        prepared.entries().len(),
        recipe.max_work_per_call
    ));
    let mut functions = Vec::new();
    for (&index, function) in selected_indices.iter().zip(&program.functions) {
        let mut sources = Vec::new();
        for address in prepared.sources(index).ok_or("missing procedure sources")? {
            let node = &graph.nodes()[graph
                .nodes()
                .binary_search_by_key(address, |node| node.address)
                .map_err(|_| "missing source node")?];
            let object = image
                .object(address.object_number)
                .ok_or("missing object")?;
            let start = address.offset as usize;
            let mut bytes = object
                .bytes()
                .get(start..start + node.byte_len as usize)
                .ok_or("source out of bounds")?
                .to_vec();
            let patches = binding
                .object_patches(address.object_number)
                .ok_or("missing patches")?;
            let first = patches.partition_point(|patch| {
                u64::from(patch.source.offset) + patch.bytes().len() as u64
                    <= u64::from(address.offset)
            });
            for patch in &patches[first..] {
                if patch.source.offset >= address.offset + u32::from(node.byte_len) {
                    break;
                }
                for (offset, &byte) in patch.bytes().iter().enumerate() {
                    let at = patch.source.offset as usize + offset;
                    if at >= start && at - start < bytes.len() {
                        bytes[at - start] = byte;
                    }
                }
            }
            let linear = binding
                .object_binding(address.object_number)
                .and_then(|o| o.linear_base.checked_add(address.offset))
                .ok_or("linear source overflow")?;
            sources.push(json!({"linear":linear,"bytes":bytes}));
        }
        let kernel = compiled
            .semantic_kernel(function.id)
            .map(|kernel| {
                report(format!(
                    "Semantic strided-copy kernel: entry={:#x}, stride={}",
                    function.entry.offset, kernel.stride
                ));
                json!({"kind":"paired-strided-copy", "stride":kernel.stride,
                "maximum_work":kernel.max_work_per_call, "prefix_nops":kernel.prefix_nops,
                "tail_nops":kernel.tail_nops})
            })
            .or_else(|| {
                compiled.rle_kernel(function.id).map(|kernel| {
                    report(format!(
                        "Semantic RLE kernel: entry={:#x}",
                        function.entry.offset
                    ));
                    json!({"kind":"rle-decode", "maximum_work":kernel.maximum_work()})
                })
            });
        let finite = compiled.finite_maximum_work(function.id).map(|work| {
            let mut exits = Vec::new();
            let mut returns = Vec::new();
            for block in &function.blocks {
                match block.terminator.kind {
                    ProcedureTerminatorKind::Exit => exits.push(block.terminator.address.offset),
                    ProcedureTerminatorKind::Return { .. } => {
                        returns.push(block.terminator.address.offset)
                    }
                    _ => {}
                }
            }
            report(format!(
                "Finite computational CFG: entry={:#x}, maximum_work={work}, exits={}, returns={}",
                function.entry.offset,
                exits.len(),
                returns.len()
            ));
            let guards: Vec<_> = compiled
                .finite_loop_guards(function.id)
                .expect("finite computation guard metadata")
                .iter()
                .map(|guard| {
                    json!({"entry":guard.entry.offset,
                    "counter":guard.counter.index(),"decrement":guard.decrement,
                    "maximum_trips":guard.maximum_trips})
                })
                .collect();
            json!({"exits":exits,"returns":returns,"guards":guards})
        });
        functions.push(json!({"id":function.id.0,"entry":function.entry.offset,
            "maximum_work":compiled.maximum_work(function.id).ok_or("missing work")?,"semantic_kernel":kernel,
            "finite_computation":finite,
            "kernel_interrupts":recipe.kernel_interrupts.unwrap_or("defer"),"sources":sources}));
    }
    let entries = functions.len();
    let manifest = json!({"version":2,"memory_abi_version":doslet_x86_wasm::static_program::memory::VERSION,
        "code_sha256":format!("{:x}",Sha256::digest(compiled.wasm())),
        "exe_sha256":format!("{:x}",Sha256::digest(exe)),"dpmi":dpmi,
        "maximum_work_per_call":recipe.max_work_per_call,"hot_profile_selected":recipe.hot_profile,
        "copy_kernels":recipe.copy_kernels,"computation_kernels":recipe.computation_kernels,
        "kernels_only":recipe.kernels_only,"functions":functions});
    let metadata = serde_json::to_vec(&manifest)?;
    Ok(Module {
        bytes: bundle::append(compiled.wasm(), &metadata)?,
        entries,
    })
}
