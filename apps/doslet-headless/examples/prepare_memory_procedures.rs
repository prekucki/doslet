//! Offline original-EXE conversion for a complete native procedure test bundle.
#[allow(dead_code)]
#[path = "../../common/procedure_input.rs"]
mod procedure_input;
use doslet_aot::{
    ImageLimits, SymbolicImage,
    binding::{BindingLimits, DosFlatBindingPlan},
    cfg::{CfgLimits, StaticCfg},
    profile::{BoundGuidance, GuidanceLimits, Profile, ProfileLimits},
    wasm::memory_procedures::{MemoryProcedureLimits, PreparedMemoryProcedures},
};
use doslet_executable::{LeImage, LeLimits};
use doslet_static_game_probe::prepare::bindings_from_json;
use doslet_x86_wasm::static_program::procedure_memory::{
    ProcedureCompileOptions, ProcedureGraphProgram, ProcedureTerminatorKind, bundle,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{error::Error, fs, io::Read, path::Path};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("offline AOT input limit exceeded".into());
    }
    Ok(bytes)
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() < 3 {
        return Err("usage: prepare_memory_procedures CONFIG.toml BINDINGS.json OUTPUT_DIR [--max-work-per-call N] [--hot-profile JSON --profile-bindings JSON] [--copy-kernels | --computation-kernels] [--kernels-only] [--kernel-interrupts defer|before-irq]".into());
    }
    let mut max_work_per_call = None;
    let mut profile_path = None;
    let mut profile_bindings = None;
    let mut copy_kernels = false;
    let mut computation_kernels = false;
    let mut kernels_only = false;
    let mut kernel_interrupts = None;
    let mut options = args[3..].iter();
    while let Some(option) = options.next() {
        if option == "--computation-kernels" && !computation_kernels {
            computation_kernels = true;
            copy_kernels = true;
            continue;
        }
        if option == "--copy-kernels" && !copy_kernels {
            copy_kernels = true;
            continue;
        }
        if option == "--kernels-only" && !kernels_only {
            kernels_only = true;
            continue;
        }
        let value = options.next().ok_or("conversion option requires a value")?;
        match option.to_str() {
            Some("--max-work-per-call") if max_work_per_call.is_none() => {
                max_work_per_call =
                    Some(value.to_str().ok_or("invalid work bound")?.parse::<u32>()?);
            }
            Some("--hot-profile") if profile_path.is_none() => {
                profile_path = Some(Path::new(value))
            }
            Some("--profile-bindings") if profile_bindings.is_none() => {
                profile_bindings = Some(Path::new(value))
            }
            Some("--kernel-interrupts") if kernel_interrupts.is_none() => {
                kernel_interrupts = Some(match value.to_str() {
                    Some("before-irq") => "before-irq",
                    Some("defer") => "defer",
                    _ => return Err("kernel interrupts must be before-irq or defer".into()),
                });
            }
            _ => return Err("invalid or repeated conversion option".into()),
        }
    }
    if computation_kernels {
        max_work_per_call.get_or_insert(4096);
    }
    if profile_path.is_some() != profile_bindings.is_some() {
        return Err("conversion options require values and both HOT profile inputs".into());
    }
    if (copy_kernels && max_work_per_call.is_none()) || (kernels_only && !copy_kernels) {
        return Err(
            "copy kernels require --max-work-per-call; --kernels-only requires --copy-kernels"
                .into(),
        );
    }
    let config = Path::new(&args[0]);
    let procedure_input::Input { exe, .. } = procedure_input::load(config)?;
    let declarations = bindings_from_json(&exe, &read(Path::new(&args[1]), 64 * 1024)?)?;
    let le = LeImage::discover(&exe, LeLimits::default())?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    let binding = DosFlatBindingPlan::new(&image, &declarations, BindingLimits::default())?;
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default())?;
    let prepare = if computation_kernels {
        PreparedMemoryProcedures::from_bound_graph_with_computations
    } else {
        PreparedMemoryProcedures::from_bound_graph
    };
    let prepared = prepare(
        &graph,
        &binding,
        MemoryProcedureLimits {
            max_work_per_call,
            ..MemoryProcedureLimits::default()
        },
    )?;
    let verified_hot = if let (Some(path), Some(layout)) = (profile_path, profile_bindings) {
        let profile = Profile::parse(&read(path, 64 * 1024 * 1024)?, ProfileLimits::default())?;
        let declarations = bindings_from_json(&exe, &read(layout, 64 * 1024)?)?;
        let historical = DosFlatBindingPlan::new(&image, &declarations, BindingLimits::default())?;
        let guidance = BoundGuidance::bind(
            &profile,
            &graph,
            &historical,
            None,
            GuidanceLimits::default(),
        )?;
        println!("HOT source verification: {:?}", guidance.report());
        Some(
            guidance
                .instructions()
                .iter()
                .map(|instruction| instruction.verified_hot())
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    let compile = |program: &ProcedureGraphProgram| {
        let options = ProcedureCompileOptions::default();
        match max_work_per_call {
            Some(limit) if computation_kernels => {
                program.compile_with_computation_kernels(&options, limit)
            }
            Some(limit) if copy_kernels => program.compile_with_copy_kernels(&options, limit),
            Some(limit) => program.compile_with_work_limit(&options, limit),
            None => program.compile(&options),
        }
    };
    let mut selected = Vec::new();
    let mut selected_indices = Vec::new();
    let mut unsupported = 0usize;
    for (index, function) in prepared.program().functions.iter().enumerate() {
        if let Some(hot) = &verified_hot {
            let mut hot_nodes = 0usize;
            let mut relevant_nodes = 0usize;
            for address in prepared.sources(index).ok_or("missing procedure sources")? {
                let at = graph
                    .nodes()
                    .binary_search_by_key(address, |node| node.address)
                    .map_err(|_| "missing procedure node")?;
                if graph.nodes()[at].byte_len == 1
                    && image
                        .object(address.object_number)
                        .and_then(|object| object.bytes().get(address.offset as usize))
                        .copied()
                        == Some(0x90)
                {
                    continue;
                }
                relevant_nodes += 1;
                hot_nodes += usize::from(hot[at]);
            }
            // Selection is optimization guidance only. It never assigns the
            // caller's tier to the other instructions or relaxes code proofs.
            if hot_nodes == 0 || hot_nodes * 2 < relevant_nodes {
                continue;
            }
        }
        let program = ProcedureGraphProgram {
            functions: vec![function.clone()],
        };
        match compile(&program) {
            Ok(compiled)
                if kernels_only
                    && compiled.semantic_kernel(function.id).is_none()
                    && compiled.rle_kernel(function.id).is_none()
                    && compiled.finite_maximum_work(function.id).is_none() =>
            {
                continue;
            }
            Ok(_) => {}
            Err(error) => {
                unsupported += 1;
                println!(
                    "Declined object{}+{:#x}: {error}",
                    prepared.entries()[index].object_number,
                    prepared.entries()[index].offset
                );
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
    println!(
        "Selected {} of {} procedure/region entries; unsupported graph shapes={unsupported}; ordinary_work_limit={max_work_per_call:?}",
        program.functions.len(),
        prepared.entries().len()
    );
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
                println!(
                    "Semantic strided-copy kernel: entry={:#x}, stride={}",
                    function.entry.offset, kernel.stride
                );
                json!({"kind":"paired-strided-copy", "stride":kernel.stride,
                "maximum_work":kernel.max_work_per_call, "prefix_nops":kernel.prefix_nops,
                "tail_nops":kernel.tail_nops})
            })
            .or_else(|| {
                compiled.rle_kernel(function.id).map(|kernel| {
                    println!("Semantic RLE kernel: entry={:#x}", function.entry.offset);
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
            println!(
                "Finite computational CFG: entry={:#x}, maximum_work={work}, exits={}, returns={}",
                function.entry.offset,
                exits.len(),
                returns.len()
            );
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
            "kernel_interrupts":kernel_interrupts.unwrap_or("defer"),"sources":sources}));
    }
    let output = Path::new(&args[2]);
    fs::create_dir_all(output)?;
    let manifest = json!({"version":2,"memory_abi_version":doslet_x86_wasm::static_program::memory::VERSION,"code_sha256":format!("{:x}",Sha256::digest(compiled.wasm())),
        "exe_sha256":format!("{:x}",Sha256::digest(&exe)),"maximum_work_per_call":max_work_per_call,"hot_profile_selected":verified_hot.is_some(),"copy_kernels":copy_kernels,"computation_kernels":computation_kernels,"kernels_only":kernels_only,"functions":functions});
    let metadata = serde_json::to_vec(&manifest)?;
    let module = bundle::append(compiled.wasm(), &metadata)?;
    fs::write(output.join("procedures.wasm"), &module)?;
    println!(
        "Prepared {} original-EXE procedure/region entries in one {}-byte WASM module ({} bytes of embedded metadata): {}",
        functions.len(),
        module.len(),
        metadata.len(),
        output.join("procedures.wasm").display()
    );
    Ok(())
}
