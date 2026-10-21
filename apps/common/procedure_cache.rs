//! Compile the procedures of one executable, bound at the live layout of
//! its LE objects (see `le_layout`), into one `procedures.wasm` with embedded
//! metadata. The desktop builds it when a layout is first seen; the headless
//! examples expose each step.
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
        ProcedureCompileOptions, ProcedureGraphFunction, ProcedureGraphProgram,
        ProcedureTerminatorKind, bundle,
    },
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::error::Error;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

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
/// metadata names `dpmi`, the mode that produced this layout, and `layout`,
/// the layout's cache key.
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
            let mut cursor = 0usize;
            for slot in instruction_virtual_slots(function, linear)
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
        functions.push(json!({"id":function.id.0,"entry":function.entry.offset,
            "maximum_work":compiled.maximum_work(function.id).ok_or("missing work")?,"semantic_kernel":kernel,
            "finite_computation":finite,
            "kernel_interrupts":recipe.kernel_interrupts.unwrap_or("defer"),"sources":sources}));
    }
    let entries = functions.len();
    let manifest = json!({"version":2,"memory_abi_version":doslet_x86_wasm::static_program::memory::VERSION,
        "code_sha256":format!("{:x}",Sha256::digest(compiled.wasm())),
        "exe_sha256":format!("{:x}",Sha256::digest(exe)),"dpmi":dpmi,"layout":layout,
        "maximum_work_per_call":recipe.max_work_per_call,"hot_profile_selected":recipe.hot_profile,
        "copy_kernels":recipe.copy_kernels,"computation_kernels":recipe.computation_kernels,
        "kernels_only":recipe.kernels_only,"functions":functions});
    let metadata = serde_json::to_vec(&manifest)?;
    Ok(Module {
        bytes: bundle::append(compiled.wasm(), &metadata)?,
        entries,
    })
}

fn instruction_virtual_slots(function: &ProcedureGraphFunction, linear: u32) -> [Option<u32>; 2] {
    let addr_slot = |address: MemoryAddress| {
        (!address.address16 && address.scale & VIRTUAL_DISPLACEMENT != 0)
            .then_some(address.displacement)
    };
    let src_slot = |source: Source| match source {
        Source::VirtualRegister(slot) => Some(slot),
        _ => None,
    };
    for block in &function.blocks {
        for instruction in &block.instructions {
            if instruction.address.offset != linear {
                continue;
            }
            let mut slots = match instruction.operation {
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
            };
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
