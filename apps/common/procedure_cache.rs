//! Compile the procedures of one executable, bound at the live layout of
//! its LE objects or COFF sections (see `le_layout`), into one
//! `procedures.wasm` with embedded metadata. The desktop builds it when a
//! layout is first seen; the headless examples expose each step.
use doslet_aot::{
    ImageLimits, ObjectAddress, SymbolicImage,
    binding::{BindingLimits, DosFlatBindingPlan, DosFlatObjectBinding},
    cfg::{CfgLimits, StaticCfg},
    wasm::memory_procedures::{MemoryProcedureLimits, PreparedMemoryProcedures},
};
use doslet_executable::{LeImage, LeLimits};
use doslet_x86_wasm::static_program::{
    Operation, Source,
    memory::{MemoryAddress, VIRTUAL_DISPLACEMENT},
    procedure_memory::{
        ProcedureAnalysisOptions, ProcedureColor, ProcedureCompileOptions,
        ProcedureExecutionStrategy, ProcedureGraphFunction, ProcedureGraphProgram,
        ProcedureTerminatorKind, X87EffectClass, bundle,
    },
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::error::Error;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// How procedures are selected and compiled.
pub struct Recipe {
    pub max_work_per_call: Option<u32>,
    /// Separate reservation for recognized copy kernels; None uses the ordinary limit.
    pub copy_work_per_call: Option<u32>,
    pub copy_kernels: bool,
    pub computation_kernels: bool,
    /// Retain ordinary bounded cyclic fragments even without a finite-loop proof.
    /// Requires an explicit work limit; hardware grants still govern entry.
    pub bounded_fragments: bool,
    /// Also admit acyclic fallback fragments with a longest path of at least 32.
    pub long_fragments: bool,
    /// Opt in to exact masked-PC24 locals and the extended FPU call boundary.
    pub pc24_locals: bool,
    pub kernels_only: bool,
    pub kernel_interrupts: Option<&'static str>,
    /// Recorded in the metadata when HOT guidance chose the entries.
    pub hot_profile: bool,
}
impl Recipe {
    /// The desktop default: whole finite computations and copy kernels,
    /// entered only before a pending IRQ would be delivered.
    pub const DESKTOP: Self = Self {
        max_work_per_call: Some(128),
        copy_work_per_call: Some(4096),
        copy_kernels: true,
        computation_kernels: true,
        bounded_fragments: true,
        long_fragments: true,
        pc24_locals: false,
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

/// The code segment bound EIPs run in: `base` is added to every binding to
/// address the live code, and a call needs a CS limit of at least `limit`.
/// LE bindings are linear in flat code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodeSegment {
    pub base: u32,
    pub limit: u32,
}

impl CodeSegment {
    pub const FLAT: Self = Self {
        base: 0,
        limit: u32::MAX,
    };
}

/// What one module is compiled from.
pub struct Target<'a> {
    /// The executable file, whose SHA-256 the metadata records.
    pub exe: &'a [u8],
    /// Its image, with a COFF program's code as loaded.
    pub image: &'a SymbolicImage,
    pub bindings: &'a [DosFlatObjectBinding],
    pub code: CodeSegment,
    /// The DPMI mode that produced this layout.
    pub dpmi: &'a str,
    /// The layout's cache key.
    pub layout: &'a str,
}

/// Compile the procedures of the LE program `exe` bound at `bindings` into
/// one module whose metadata names `dpmi`, the mode that produced this
/// layout, and `layout`, the layout's cache key.
#[allow(
    dead_code,
    reason = "the desktop compiles any layout through build_target"
)]
pub fn build(
    exe: &[u8],
    bindings: &[DosFlatObjectBinding],
    dpmi: &str,
    layout: &str,
    recipe: &Recipe,
    keep: &mut Keep<'_>,
    report: &mut dyn FnMut(String),
) -> Result<Module> {
    let le = LeImage::discover(exe, LeLimits::default())?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    build_target(
        &Target {
            exe,
            image: &image,
            bindings,
            code: CodeSegment::FLAT,
            dpmi,
            layout,
        },
        recipe,
        keep,
        report,
    )
}

/// Compile one module for `target`. Sources record linear addresses, the
/// code segment's base plus each bound address; entries and terminals stay
/// EIPs. Modules of a code segment other than the flat one name it.
pub fn build_target(
    target: &Target<'_>,
    recipe: &Recipe,
    keep: &mut Keep<'_>,
    report: &mut dyn FnMut(String),
) -> Result<Module> {
    let Target {
        exe,
        image,
        bindings,
        code,
        dpmi,
        layout,
    } = *target;
    let binding = DosFlatBindingPlan::new(image, bindings, BindingLimits::default())?;
    let extra_entries = Vec::<ObjectAddress>::new();
    // Local replay experiment only. Normal application builds still discover
    // entries from the executable; no game-specific addresses are embedded.
    #[cfg(test)]
    let extra_entries = {
        let mut entries = extra_entries;
        if let Ok(value) = std::env::var("DOSLET_AOT_TEST_ENTRY_EIPS") {
            for raw in value.split(',') {
                if entries.len() == 64 {
                    return Err("too many diagnostic AOT entries".into());
                }
                let raw = raw.trim();
                let ip = u32::from_str_radix(raw.strip_prefix("0x").unwrap_or(raw), 16)?;
                let entry = bindings
                    .iter()
                    .find_map(|object| {
                        let offset = ip.checked_sub(object.linear_base)?;
                        let source = image.object(object.object_number)?;
                        ((offset as usize) < source.bytes().len()).then_some(ObjectAddress {
                            object_number: object.object_number,
                            offset,
                        })
                    })
                    .ok_or("diagnostic AOT entry outside bound source bytes")?;
                entries.push(entry);
            }
            eprintln!("Diagnostic AOT entry EIPs: {value}");
        }
        entries
    };
    let graph = StaticCfg::analyze_bound(&binding, &extra_entries, CfgLimits::default())?;
    let limits = MemoryProcedureLimits {
        max_work_per_call: recipe.max_work_per_call,
        ..MemoryProcedureLimits::default()
    };
    let prepared = if recipe.pc24_locals {
        PreparedMemoryProcedures::from_bound_graph_with_pc24_fragments(
            &graph, &binding, limits, 32,
        )?
    } else if recipe.long_fragments {
        PreparedMemoryProcedures::from_bound_graph_with_long_fragments(
            &graph, &binding, limits, 32,
        )?
    } else if recipe.bounded_fragments {
        if recipe.max_work_per_call.is_none() {
            return Err("bounded fragments require an explicit work limit".into());
        }
        PreparedMemoryProcedures::from_bound_graph_with_bounded_fragments(&graph, &binding, limits)?
    } else if recipe.computation_kernels {
        PreparedMemoryProcedures::from_bound_graph_with_computations(&graph, &binding, limits)?
    } else {
        PreparedMemoryProcedures::from_bound_graph(&graph, &binding, limits)?
    };
    let options = ProcedureCompileOptions::default();
    // Classification and proof construction precede all WASM/native emission.
    // Preserve the recipe's existing backend precedence and admission scope.
    let analysis_options = ProcedureAnalysisOptions {
        ordinary_work_limit: recipe.max_work_per_call,
        copy_work_limit: recipe.max_work_per_call.and_then(|limit| {
            if recipe.pc24_locals || recipe.computation_kernels {
                Some(recipe.copy_work_per_call.unwrap_or(limit))
            } else {
                recipe.copy_kernels.then_some(limit)
            }
        }),
        computation_kernels: recipe.max_work_per_call.is_some()
            && (recipe.pc24_locals || recipe.computation_kernels),
        pc24_locals: recipe.max_work_per_call.is_some() && recipe.pc24_locals,
    };
    let mut selected = Vec::new();
    let mut selected_indices = Vec::new();
    let mut declined = 0usize;
    for (index, function) in prepared.program().functions.iter().enumerate() {
        let sources = prepared.sources(index).ok_or("missing procedure sources")?;
        if !keep(&graph, image, sources)? {
            continue;
        }
        let mut function = function.clone();
        if let Err(error) = linearize_virtual_slots(code, &mut function) {
            declined += 1;
            report(format!(
                "Declined object{}+{:#x}: {error}",
                prepared.entries()[index].object_number,
                prepared.entries()[index].offset
            ));
            continue;
        }
        let program = ProcedureGraphProgram {
            functions: vec![function.clone()],
        };
        match program.prepare(&options, &analysis_options) {
            Ok(prepared)
                if recipe.kernels_only
                    && !matches!(
                        prepared
                            .color(function.id)
                            .expect("analyzed entry")
                            .strategy,
                        ProcedureExecutionStrategy::Finite
                            | ProcedureExecutionStrategy::GuardedFinite
                            | ProcedureExecutionStrategy::Copy
                            | ProcedureExecutionStrategy::Rle
                    ) =>
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
    let analyzed = program.prepare(&options, &analysis_options)?;
    let colors = analyzed.colors().to_vec();
    report_colors(&colors, report);
    let compiled = analyzed.compile()?;
    report(format!(
        "Selected {} of {} procedure/region entries; unsupported graph shapes={declined}; ordinary_work_limit={:?}",
        program.functions.len(),
        prepared.entries().len(),
        recipe.max_work_per_call
    ));
    let mut functions = Vec::new();
    for (&index, function) in selected_indices.iter().zip(&program.functions) {
        let mut sources = Vec::new();
        let mut execution_sites = Vec::new();
        let mut execution_sites_supported = true;
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
            let eip = binding
                .object_binding(address.object_number)
                .and_then(|o| o.linear_base.checked_add(address.offset))
                .ok_or("source EIP overflow")?;
            let linear = code.base.checked_add(eip).ok_or("linear source overflow")?;
            if function.blocks.iter().any(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| instruction.address.offset == eip)
                    || block.terminator.address.offset == eip
                        && !matches!(
                            block.terminator.kind,
                            ProcedureTerminatorKind::Exit | ProcedureTerminatorKind::Fallthrough(_)
                        )
            }) {
                execution_sites.push(json!({"ip":eip,"length":node.byte_len}));
                let x87_site = function
                    .blocks
                    .iter()
                    .flat_map(|block| &block.instructions)
                    .any(|instruction| {
                        instruction.address.offset == eip
                            && matches!(instruction.operation, Operation::X87(_))
                    });
                execution_sites_supported &= x87_site
                    || (doslet_x86::translation::instruction_length(&bytes, true)
                        == Some(node.byte_len)
                        && doslet_x86::translation::near_flow(&bytes, true).is_some());
                for slot in instruction_virtual_slots(function, eip)
                    .into_iter()
                    .flatten()
                {
                    let offset = slot.checked_sub(linear).map(|offset| offset as usize);
                    if let Some(offset) = offset.filter(|&offset| offset + 4 <= bytes.len()) {
                        let mut patched = bytes.clone();
                        for byte in &mut patched[offset..offset + 4] {
                            *byte ^= 0xff;
                        }
                        execution_sites_supported &=
                            x87_site || doslet_x86::translation::same_template32(&bytes, &patched);
                    } else {
                        execution_sites_supported = false;
                    }
                }
            }
            let mut cursor = 0usize;
            for slot in instruction_virtual_slots(function, eip)
                .into_iter()
                .flatten()
            {
                if slot > linear
                    && let Ok(off) = usize::try_from(slot - linear)
                    && off >= cursor
                    && off + 4 <= bytes.len()
                {
                    if off > cursor {
                        sources.push(json!({
                            "linear": linear + cursor as u32,
                            "bytes": &bytes[cursor..off],
                        }));
                    }
                    cursor = off + 4;
                }
            }
            if cursor < bytes.len() {
                sources.push(json!({
                    "linear": linear + cursor as u32,
                    "bytes": &bytes[cursor..],
                }));
            }
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
        execution_sites
            .sort_unstable_by_key(|site| site["ip"].as_u64().expect("generated execution EIP"));
        // IR lowering can support forms outside the ordinary diagnostic
        // decoder. Keep those modules installable with legacy tier fallback.
        let execution_sites = execution_sites_supported.then_some(execution_sites);
        let x87 = compiled
            .x87(function.id)
            .map(|masks| json!({"read_mask":masks.read_mask,"write_mask":masks.write_mask}));
        let color = colors
            .binary_search_by_key(&function.id, |color| color.id)
            .map(|index| &colors[index])
            .map_err(|_| "missing procedure color")?;
        functions.push(json!({"id":function.id.0,"entry":function.entry.offset,"execution_sites":execution_sites,
            "x87":x87,
            "analysis":color_json(color),
            "maximum_work":compiled.maximum_work(function.id).ok_or("missing work")?,"semantic_kernel":kernel,
            "finite_computation":finite,
            "kernel_interrupts":recipe.kernel_interrupts.unwrap_or("defer"),"sources":sources}));
    }
    let entries = functions.len();
    let mut manifest = json!({"version":2,"memory_abi_version":doslet_x86_wasm::static_program::memory::VERSION,
        "code_sha256":format!("{:x}",Sha256::digest(compiled.wasm())),
        "exe_sha256":format!("{:x}",Sha256::digest(exe)),"dpmi":dpmi,"layout":layout,
        "maximum_work_per_call":recipe.max_work_per_call,"hot_profile_selected":recipe.hot_profile,
        "copy_kernels":recipe.copy_kernels,"computation_kernels":recipe.computation_kernels,
        "bounded_fragments":recipe.bounded_fragments,"long_fragments":recipe.long_fragments,
        "pc24_locals":recipe.pc24_locals,
        "copy_work_per_call":recipe.copy_work_per_call,
        "analysis_version":1,
        "kernels_only":recipe.kernels_only,"functions":functions});
    // Flat modules keep their earlier metadata; the loader defaults to flat.
    if code != CodeSegment::FLAT {
        manifest["code_segment"] = json!({"base":code.base,"limit":code.limit});
    }
    let metadata = serde_json::to_vec(&manifest)?;
    Ok(Module {
        bytes: bundle::append(compiled.wasm(), &metadata)?,
        entries,
    })
}

fn strategy_name(strategy: ProcedureExecutionStrategy) -> &'static str {
    match strategy {
        ProcedureExecutionStrategy::Acyclic => "acyclic",
        ProcedureExecutionStrategy::BoundedLoop => "bounded-loop",
        ProcedureExecutionStrategy::Finite => "finite",
        ProcedureExecutionStrategy::GuardedFinite => "guarded-finite",
        ProcedureExecutionStrategy::Copy => "copy",
        ProcedureExecutionStrategy::Rle => "rle",
    }
}

fn x87_class_name(color: &ProcedureColor) -> &'static str {
    match color.x87.as_ref().map(|effects| effects.class) {
        None => "none",
        Some(X87EffectClass::Unknown) => "unknown",
        Some(X87EffectClass::MetadataOnly) => "metadata-only",
        Some(X87EffectClass::InitialValues) => "initial-values",
        Some(X87EffectClass::IndependentBalanced) => "independent-balanced",
        Some(X87EffectClass::IndependentUnbalanced) => "independent-unbalanced",
    }
}

fn report_colors(colors: &[ProcedureColor], report: &mut dyn FnMut(String)) {
    let mut counts = std::collections::BTreeMap::new();
    for color in colors {
        *counts
            .entry((strategy_name(color.strategy), x87_class_name(color)))
            .or_insert(0usize) += 1;
    }
    report(format!(
        "Pre-AOT analysis: {} entries classified before WASM emission; x87 balance refers to declared exits only",
        colors.len()
    ));
    for ((strategy, x87), count) in counts {
        report(format!(
            "  execution={strategy}, x87={x87}: {count} entries"
        ));
    }
}

/// Diagnostic metadata, never trusted to bypass the owner's admission checks.
fn color_json(color: &ProcedureColor) -> serde_json::Value {
    let x87 = color.x87.as_ref().map(|effects| {
        let exits: Vec<_> = effects.exit_balances.iter().map(|exit| {
            json!({"ip":exit.address.offset,
                "kind":if matches!(exit.kind, ProcedureTerminatorKind::Return { .. }) { "return" } else { "interpreter" },
                "stack_delta":exit.delta})
        }).collect();
        json!({
            "read_mask":effects.read_mask,"write_mask":effects.write_mask,
            "initial_values_required":effects.initial_values_required,
            "reads_status":effects.reads_status,"reads_control":effects.reads_control,
            "reads_pointers":effects.reads_pointers,
            "writes_status":effects.writes_status,"writes_control":effects.writes_control,
            "writes_pointers":effects.writes_pointers,
            "stack_min":effects.stack_min,"stack_max":effects.stack_max,
            "summary_known":effects.summary_known,
            "declared_exits_balanced":effects.declared_exits_balanced,
            "all_boundaries_balanced":effects.all_boundaries_balanced,
            "preserves_full_state":effects.preserves_full_state,"exits":exits
        })
    });
    json!({"execution":strategy_name(color.strategy),"x87_class":x87_class_name(color),
        "maximum_work":color.maximum_work,
        "interpreter_exits":color.interpreter_exits.iter().map(|address| address.offset).collect::<Vec<_>>(),
        "returns":color.returns.iter().map(|ret| ret.address.offset).collect::<Vec<_>>(),
        "x87_effects":x87})
}

fn instruction_virtual_slots(function: &ProcedureGraphFunction, eip: u32) -> [Option<u32>; 2] {
    for block in &function.blocks {
        for instruction in &block.instructions {
            if instruction.address.offset != eip {
                continue;
            }
            let mut slots = operation_virtual_slots(instruction.operation);
            if let [Some(a), Some(b)] = slots
                && a > b
            {
                slots = [Some(b), Some(a)];
            }
            return slots;
        }
    }
    [None, None]
}

fn operation_virtual_slots(operation: Operation) -> [Option<u32>; 2] {
    let addr_slot = |address: MemoryAddress| {
        (!address.address16 && address.scale & VIRTUAL_DISPLACEMENT != 0)
            .then_some(address.displacement)
    };
    let src_slot = |source: Source| match source {
        Source::VirtualRegister(slot) => Some(slot),
        _ => None,
    };
    match operation {
        Operation::X87(instruction) => [instruction.operation.address().and_then(addr_slot), None],
        Operation::Move { source, .. }
        | Operation::Alu { source, .. }
        | Operation::Push { source, .. }
        | Operation::WideMultiply { source, .. } => [src_slot(source), None],
        Operation::SignedMultiply { left, right, .. } => [src_slot(left), src_slot(right)],
        Operation::Lea { address, .. }
        | Operation::LoadMemory { address, .. }
        | Operation::ExtendMemory { address, .. }
        | Operation::AluMemory { address, .. }
        | Operation::UnaryMemory { address, .. }
        | Operation::WideMultiplyMemory { address, .. } => [addr_slot(address), None],
        Operation::StoreMemory {
            address, source, ..
        }
        | Operation::CompareMemory {
            address, source, ..
        }
        | Operation::AluMemoryStore {
            address, source, ..
        }
        | Operation::SignedMultiplyMemory {
            address, source, ..
        } => [addr_slot(address), src_slot(source)],
        _ => [None, None],
    }
}

fn function_uses_virtual_slots(function: &ProcedureGraphFunction) -> bool {
    function.blocks.iter().any(|block| {
        block
            .instructions
            .iter()
            .any(|instruction| operation_uses_virtual_slots(instruction.operation))
    })
}

/// AOT planning uses EIPs for code identities. Virtual operand slots instead
/// name physical guest RAM locations loaded by the WASM prologue. Convert only
/// those slot addresses, never the value subsequently loaded from a slot.
fn linearize_virtual_slots(code: CodeSegment, function: &mut ProcedureGraphFunction) -> Result<()> {
    if !function_uses_virtual_slots(function) {
        return Ok(());
    }
    let slot = |address: &mut u32| -> Result<()> {
        let last = address
            .checked_add(3)
            .filter(|&last| last <= code.limit)
            .ok_or("virtual operand slot outside code segment")?;
        code.base
            .checked_add(last)
            .ok_or("virtual operand slot linear span overflow")?;
        *address = code
            .base
            .checked_add(*address)
            .ok_or("virtual operand slot linear address overflow")?;
        Ok(())
    };
    let source = |source: &mut Source| -> Result<()> {
        if let Source::VirtualRegister(address) = source {
            slot(address)?;
        }
        Ok(())
    };
    let address = |address: &mut MemoryAddress| -> Result<()> {
        if address.scale & VIRTUAL_DISPLACEMENT != 0 {
            if address.address16 {
                return Err("virtual operand slot requires a 32-bit address".into());
            }
            slot(&mut address.displacement)?;
        }
        Ok(())
    };
    for instruction in function
        .blocks
        .iter_mut()
        .flat_map(|block| &mut block.instructions)
    {
        match &mut instruction.operation {
            Operation::X87(x87) => {
                if let Some(mut operand) = x87.operation.address() {
                    address(&mut operand)?;
                    x87.operation = x87.operation.with_address(operand);
                }
            }
            Operation::Move { source: value, .. }
            | Operation::Alu { source: value, .. }
            | Operation::Push { source: value, .. }
            | Operation::WideMultiply { source: value, .. } => source(value)?,
            Operation::SignedMultiply { left, right, .. } => {
                source(left)?;
                source(right)?;
            }
            Operation::Lea { address: value, .. }
            | Operation::LoadMemory { address: value, .. }
            | Operation::ExtendMemory { address: value, .. }
            | Operation::AluMemory { address: value, .. }
            | Operation::UnaryMemory { address: value, .. }
            | Operation::WideMultiplyMemory { address: value, .. } => address(value)?,
            Operation::StoreMemory {
                address: target,
                source: value,
                ..
            }
            | Operation::CompareMemory {
                address: target,
                source: value,
                ..
            }
            | Operation::AluMemoryStore {
                address: target,
                source: value,
                ..
            }
            | Operation::SignedMultiplyMemory {
                address: target,
                source: value,
                ..
            } => {
                address(target)?;
                source(value)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn operation_uses_virtual_slots(operation: Operation) -> bool {
    let virtual_address = |address: MemoryAddress| address.scale & VIRTUAL_DISPLACEMENT != 0;
    let virtual_source = |source: Source| matches!(source, Source::VirtualRegister(_));
    match operation {
        Operation::X87(instruction) => instruction.operation.address().is_some_and(virtual_address),
        Operation::Move { source, .. }
        | Operation::Alu { source, .. }
        | Operation::Push { source, .. }
        | Operation::WideMultiply { source, .. } => virtual_source(source),
        Operation::SignedMultiply { left, right, .. } => {
            virtual_source(left) || virtual_source(right)
        }
        Operation::Lea { address, .. }
        | Operation::LoadMemory { address, .. }
        | Operation::ExtendMemory { address, .. }
        | Operation::AluMemory { address, .. }
        | Operation::UnaryMemory { address, .. }
        | Operation::WideMultiplyMemory { address, .. } => virtual_address(address),
        Operation::StoreMemory {
            address, source, ..
        }
        | Operation::CompareMemory {
            address, source, ..
        }
        | Operation::AluMemoryStore {
            address, source, ..
        }
        | Operation::SignedMultiplyMemory {
            address, source, ..
        } => virtual_address(address) || virtual_source(source),
        _ => false,
    }
}

#[cfg(test)]
mod virtual_slot_tests {
    use super::*;
    use doslet_x86_wasm::static_program::{
        Address, Alu, MemoryUnary, Register, Width,
        memory::Segment,
        procedure_memory::{
            ProcedureBlock, ProcedureId, ProcedureInstruction, ProcedureTerminator,
            ProcedureTerminatorKind,
        },
    };

    fn function(operation: Operation) -> ProcedureGraphFunction {
        let entry = Address {
            space: 0,
            offset: 0x1000,
        };
        let ret = Address {
            space: 0,
            offset: 0x1001,
        };
        ProcedureGraphFunction {
            id: ProcedureId(1),
            entry,
            blocks: vec![ProcedureBlock {
                entry,
                instructions: vec![ProcedureInstruction {
                    address: entry,
                    operation,
                }],
                terminator: ProcedureTerminator {
                    address: ret,
                    kind: ProcedureTerminatorKind::Return {
                        width: Width::Dword,
                        pop_bytes: 0,
                    },
                },
            }],
        }
    }

    fn virtual_source() -> Source {
        Source::VirtualRegister(0x2000)
    }

    fn virtual_address() -> MemoryAddress {
        MemoryAddress {
            base: None,
            index: None,
            scale: 1 | VIRTUAL_DISPLACEMENT,
            displacement: 0x2000,
            address16: false,
            segment: Segment::Ds,
        }
    }

    fn ordinary_address() -> MemoryAddress {
        MemoryAddress {
            scale: 1,
            ..virtual_address()
        }
    }

    #[test]
    fn flat_virtual_slots_remain_allowed_and_nonflat_plain_code_is_allowed() {
        let virtual_immediate = function(Operation::Move {
            target: Register::Eax,
            source: virtual_source(),
        });
        let plain = function(Operation::Move {
            target: Register::Eax,
            source: Source::Immediate(7),
        });
        let mut flat = virtual_immediate.clone();
        linearize_virtual_slots(CodeSegment::FLAT, &mut flat).unwrap();
        assert_eq!(
            operation_virtual_slots(flat.blocks[0].instructions[0].operation),
            [Some(0x2000), None]
        );
        let mut based = virtual_immediate;
        linearize_virtual_slots(
            CodeSegment {
                base: 0x12_0000,
                limit: 0xffff,
            },
            &mut based,
        )
        .unwrap();
        assert_eq!(
            operation_virtual_slots(based.blocks[0].instructions[0].operation),
            [Some(0x122000), None]
        );
        let mut plain = plain;
        linearize_virtual_slots(
            CodeSegment {
                base: 0x12_0000,
                limit: 0xffff,
            },
            &mut plain,
        )
        .unwrap();
    }

    #[test]
    fn nonflat_conversion_covers_virtual_register_source_forms() {
        let src = virtual_source();
        let reg = Register::Eax;
        let address = ordinary_address();
        let operations = [
            Operation::Move {
                target: reg,
                source: src,
            },
            Operation::Alu {
                kind: Alu::Add,
                target: reg,
                source: src,
            },
            Operation::Push {
                width: Width::Dword,
                source: src,
            },
            Operation::WideMultiply {
                signed: false,
                width: Width::Dword,
                source: src,
            },
            Operation::SignedMultiply {
                target: reg,
                left: src,
                right: Source::Register(reg),
            },
            Operation::SignedMultiply {
                target: reg,
                left: Source::Register(reg),
                right: src,
            },
            Operation::StoreMemory {
                address,
                width: Width::Dword,
                source: src,
            },
            Operation::CompareMemory {
                test: false,
                address,
                width: Width::Dword,
                source: src,
            },
            Operation::AluMemoryStore {
                kind: Alu::Add,
                address,
                width: Width::Dword,
                source: src,
            },
            Operation::SignedMultiplyMemory {
                target: reg,
                address,
                source: src,
            },
        ];
        for operation in operations {
            let mut function = function(operation);
            assert!(function_uses_virtual_slots(&function), "{operation:?}");
            linearize_virtual_slots(
                CodeSegment {
                    base: 0x12_0000,
                    limit: 0xffff,
                },
                &mut function,
            )
            .unwrap();
            for slot in operation_virtual_slots(function.blocks[0].instructions[0].operation)
                .into_iter()
                .flatten()
            {
                assert_eq!(slot, 0x122000, "{operation:?}");
            }
        }
    }

    #[test]
    fn nonflat_conversion_covers_virtual_displacement_forms() {
        let address = virtual_address();
        let operations = [
            Operation::Lea {
                target: Register::Eax,
                address,
            },
            Operation::LoadMemory {
                target: Register::Eax,
                address,
            },
            Operation::ExtendMemory {
                target: Register::Eax,
                width: Width::Byte,
                address,
                signed: false,
            },
            Operation::AluMemory {
                kind: Alu::Add,
                target: Register::Eax,
                address,
            },
            Operation::UnaryMemory {
                kind: MemoryUnary::Inc,
                address,
                width: Width::Dword,
            },
            Operation::WideMultiplyMemory {
                signed: false,
                width: Width::Dword,
                address,
            },
            Operation::StoreMemory {
                address,
                width: Width::Dword,
                source: Source::Register(Register::Eax),
            },
            Operation::CompareMemory {
                test: false,
                address,
                width: Width::Dword,
                source: Source::Register(Register::Eax),
            },
            Operation::AluMemoryStore {
                kind: Alu::Add,
                address,
                width: Width::Dword,
                source: Source::Register(Register::Eax),
            },
            Operation::SignedMultiplyMemory {
                target: Register::Eax,
                address,
                source: Source::Register(Register::Eax),
            },
        ];
        for operation in operations {
            let mut function = function(operation);
            assert!(function_uses_virtual_slots(&function), "{operation:?}");
            linearize_virtual_slots(
                CodeSegment {
                    base: 0x12_0000,
                    limit: 0xffff,
                },
                &mut function,
            )
            .unwrap();
            assert_eq!(
                operation_virtual_slots(function.blocks[0].instructions[0].operation),
                [Some(0x122000), None],
                "{operation:?}"
            );
        }
        let mut invalid_16_bit = virtual_address();
        invalid_16_bit.address16 = true;
        let mut invalid = function(Operation::Lea {
            target: Register::Eax,
            address: invalid_16_bit,
        });
        assert!(function_uses_virtual_slots(&invalid));
        assert!(
            linearize_virtual_slots(
                CodeSegment {
                    base: 0x12_0000,
                    limit: 0xffff,
                },
                &mut invalid,
            )
            .is_err()
        );
    }

    #[test]
    fn flat_source_slot_extraction_is_unchanged() {
        let function = function(Operation::Move {
            target: Register::Eax,
            source: virtual_source(),
        });
        assert_eq!(
            operation_virtual_slots(function.blocks[0].instructions[0].operation),
            [Some(0x2000), None]
        );
    }
    #[test]
    fn nonflat_slot_span_and_segment_overflow_decline() {
        for (base, limit, slot) in [
            (0x120000, 0x2002, 0x2000),
            (u32::MAX - 0x2001, u32::MAX, 0x2000),
            (0, u32::MAX, u32::MAX - 1),
        ] {
            let mut function = function(Operation::Move {
                target: Register::Eax,
                source: Source::VirtualRegister(slot),
            });
            assert!(linearize_virtual_slots(CodeSegment { base, limit }, &mut function).is_err());
        }
    }
    #[test]
    fn source_holes_find_instruction_by_eip_and_return_linear_slots() {
        let mut function = function(Operation::Move {
            target: Register::Eax,
            source: Source::VirtualRegister(0x1001),
        });
        linearize_virtual_slots(
            CodeSegment {
                base: 0x120000,
                limit: 0xffff,
            },
            &mut function,
        )
        .unwrap();
        assert_eq!(
            instruction_virtual_slots(&function, 0x1000),
            [Some(0x121001), None]
        );
        assert_eq!(instruction_virtual_slots(&function, 0x121000), [None, None]);
    }
}
