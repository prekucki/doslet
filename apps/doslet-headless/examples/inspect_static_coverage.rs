//! Bounded, read-only potential static coverage of a real saved-state replay.
//! This inventories CPU entries, not live generated-code admission or speed.
#[path = "inspect_static_coverage/hot.rs"]
mod hot;
#[allow(dead_code)]
#[path = "../../common/procedure_input.rs"]
mod procedure_input;
#[allow(dead_code)]
#[path = "../../common/wasm_game.rs"]
mod wasm_game;

use doslet_aot::{
    DefaultBitness, ImageLimits, ObjectAddress, SymbolicImage,
    binding::{BindingLimits, DosFlatBindingPlan},
    cfg::{CfgLimits, StaticCfg},
    procedures::{CallProcedureLimits, CallProcedurePlan, ProcedureLimits, ProcedurePlan},
    wasm::memory_procedures::{MemoryProcedureLimits, PreparedMemoryProcedures},
    wasm::{LoweringLimits, PreparedProgram},
};
use doslet_executable::{LeImage, LeLimits};
use doslet_runtime::{Session, SessionStatus};
use doslet_static_game_probe::prepare::bindings_from_json;
use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap},
    error::Error,
    fs::File,
    io::Read,
    path::Path,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const DEFAULT_STEPS: u64 = 2_000_000;
const MAX_STEPS: u64 = 20_000_000;
const MAX_UNIQUE: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Entry {
    linear: u32,
    ip: u32,
    code32: bool,
    available: u8,
    bytes: [u8; 15],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Category {
    KnownLowered,
    KnownUnsupported,
    OutsideCfg,
    ChangedBytes,
    OutsideObjects,
}
impl Category {
    const ALL: [Self; 5] = [
        Self::KnownLowered,
        Self::KnownUnsupported,
        Self::OutsideCfg,
        Self::ChangedBytes,
        Self::OutsideObjects,
    ];
    const fn name(self) -> &'static str {
        match self {
            Self::KnownLowered => "known lowered, source matches",
            Self::KnownUnsupported => "known unsupported, source matches",
            Self::OutsideCfg => "outside CFG, source matches",
            Self::ChangedBytes => "changed/unavailable source bytes",
            Self::OutsideObjects => "outside mapped objects",
        }
    }
}

struct Row {
    entry: Entry,
    count: u64,
    category: Category,
    address: Option<ObjectAddress>,
    compared: usize,
}

fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(format!("{} exceeds diagnostic input bound", path.display()).into());
    }
    Ok(bytes)
}

/// Patch only the requested instruction/window; never clone a loaded object.
fn source_window(
    binding: &DosFlatBindingPlan<'_>,
    address: ObjectAddress,
    length: usize,
) -> Option<[u8; 15]> {
    if length == 0 || length > 15 {
        return None;
    }
    let object = binding.image().object(address.object_number)?;
    let start = address.offset as usize;
    let original = object.bytes().get(start..start + length)?;
    let mut expected = [0u8; 15];
    expected[..length].copy_from_slice(original);
    let patches = binding
        .object_patches(address.object_number)
        .expect("validated object binding");
    let first = patches.partition_point(|patch| {
        u64::from(patch.source.offset) + patch.bytes().len() as u64 <= u64::from(address.offset)
    });
    for patch in &patches[first..] {
        if u64::from(patch.source.offset) >= u64::from(address.offset) + length as u64 {
            break;
        }
        for (index, &value) in patch.bytes().iter().enumerate() {
            let at = u64::from(patch.source.offset) + index as u64;
            if at >= u64::from(address.offset) && at - u64::from(address.offset) < length as u64 {
                expected[(at - u64::from(address.offset)) as usize] = value;
            }
        }
    }
    Some(expected)
}

fn source_matches(
    binding: &DosFlatBindingPlan<'_>,
    address: ObjectAddress,
    observed: &[u8],
) -> bool {
    source_window(binding, address, observed.len())
        .is_some_and(|expected| expected[..observed.len()] == *observed)
}

fn classify(
    entry: Entry,
    count: u64,
    binding: &DosFlatBindingPlan<'_>,
    graph: &StaticCfg<'_>,
    lowered: &[bool],
) -> Row {
    let address = binding.image().objects().iter().find_map(|object| {
        let loaded = binding.object_binding(object.object_number())?;
        let offset = entry.linear.checked_sub(loaded.linear_base)?;
        (offset < object.metadata().virtual_size).then_some(ObjectAddress {
            object_number: object.object_number(),
            offset,
        })
    });
    let Some(address) = address else {
        return Row {
            entry,
            count,
            category: Category::OutsideObjects,
            address: None,
            compared: 0,
        };
    };
    let node = graph
        .nodes()
        .binary_search_by_key(&address, |node| node.address)
        .ok();
    let available = usize::from(entry.available);
    let decoded = entry
        .code32
        .then(|| doslet_x86::analysis::decode(&entry.bytes[..available], entry.ip))
        .flatten();
    // Known nodes have an exact static length. Outside CFG, prefer the shared
    // decoder's length; unsupported forms compare the complete available window.
    let compared = node.map_or_else(
        || decoded.map_or(available, |decoded| usize::from(decoded.length)),
        |index| usize::from(graph.nodes()[index].byte_len),
    );
    let category = if compared > available
        || !source_matches(binding, address, &entry.bytes[..compared.min(available)])
    {
        Category::ChangedBytes
    } else if let Some(index) = node
        .filter(|&index| (graph.nodes()[index].bitness == DefaultBitness::Bits32) == entry.code32)
    {
        if lowered[index] {
            Category::KnownLowered
        } else {
            Category::KnownUnsupported
        }
    } else {
        Category::OutsideCfg
    };
    Row {
        entry,
        count,
        category,
        address: Some(address),
        compared,
    }
}

/// A compact opcode family hint, not an additional execution decoder. Precise
/// modeled effects/flow are printed separately through doslet_x86::analysis.
fn family(entry: &Entry) -> &'static str {
    let bytes = &entry.bytes[..usize::from(entry.available)];
    let mut at = 0;
    let mut rep = false;
    while let Some(&byte) = bytes.get(at) {
        match byte {
            0xf2 | 0xf3 => rep = true,
            0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67 | 0xf0 => {}
            _ => break,
        }
        at += 1;
    }
    let Some(&opcode) = bytes.get(at) else {
        return "unavailable";
    };
    let memory = bytes.get(at + 1).is_some_and(|modrm| modrm >> 6 != 3);
    match opcode {
        0xa4 | 0xa5 if rep => "REP MOVS",
        0xaa | 0xab if rep => "REP STOS",
        0xa6 | 0xa7 if rep => "REP CMPS",
        0xae | 0xaf if rep => "REP SCAS",
        0xac | 0xad if rep => "REP LODS",
        0xa4..=0xa7 | 0xaa..=0xaf => "string",
        0x50..=0x61 | 0x68 | 0x6a | 0x8f | 0x9c | 0x9d | 0xc8 | 0xc9 => "stack",
        0xe8 | 0x9a => "CALL",
        0xc2 | 0xc3 | 0xca | 0xcb => "RET",
        0xcf => "IRET",
        0x70..=0x7f | 0xe0..=0xe3 | 0xe9 | 0xeb => "branch",
        0x69 | 0x6b => "IMUL",
        0xf6 | 0xf7 if bytes.get(at + 1).is_some_and(|modrm| (modrm >> 3) & 7 >= 4) => {
            match (bytes[at + 1] >> 3) & 7 {
                4 => "MUL",
                5 => "IMUL",
                6 => "DIV",
                _ => "IDIV",
            }
        }
        0x98 => "CBW/CWDE",
        0x99 => "CWD/CDQ",
        0x00..=0x3f if opcode & 7 <= 3 && memory => "memory ALU",
        0x80..=0x85 | 0xf6 | 0xf7 | 0xfe if memory => "memory ALU/test",
        0x00..=0x3f if opcode & 7 <= 5 => "register ALU/test",
        0x40..=0x4f | 0x80..=0x85 | 0xf6 | 0xf7 | 0xfe => "register ALU/test",
        0xc0 | 0xc1 | 0xd0..=0xd3 => "shift/rotate",
        0x88..=0x8b | 0x8c | 0x8e | 0xc6 | 0xc7 if memory => "memory MOV",
        0xa0..=0xa3 => "memory MOV",
        0x8d => "LEA",
        0xff if bytes.get(at + 1).is_some_and(|modrm| (modrm >> 3) & 7 == 6) => "stack",
        0xff if bytes
            .get(at + 1)
            .is_some_and(|modrm| matches!((modrm >> 3) & 7, 2 | 3)) =>
        {
            "indirect CALL"
        }
        0xff if bytes
            .get(at + 1)
            .is_some_and(|modrm| matches!((modrm >> 3) & 7, 4 | 5)) =>
        {
            "indirect JMP"
        }
        0xff if memory => "memory ALU",
        0x0f if bytes
            .get(at + 1)
            .is_some_and(|byte| matches!(byte, 0xb6 | 0xb7)) =>
        {
            "MOVZX"
        }
        0x0f if bytes
            .get(at + 1)
            .is_some_and(|byte| matches!(byte, 0xbe | 0xbf)) =>
        {
            "MOVSX"
        }
        0x0f if bytes.get(at + 1) == Some(&0xaf) => "IMUL",
        0x0f if bytes
            .get(at + 1)
            .is_some_and(|byte| matches!(byte, 0xa4 | 0xa5 | 0xac | 0xad)) =>
        {
            "SHLD/SHRD"
        }
        0x0f if bytes
            .get(at + 1)
            .is_some_and(|byte| (0x80..=0x8f).contains(byte)) =>
        {
            "branch"
        }
        0xd8..=0xdf => "x87",
        0xe4..=0xe7 | 0xec..=0xef => "port I/O",
        0xcc..=0xce => "INT",
        _ => "other",
    }
}

fn print_row(row: &Row) {
    let entry = &row.entry;
    let bytes = entry.bytes[..usize::from(entry.available)]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    let location = row.address.map_or_else(
        || "unmapped".to_owned(),
        |address| format!("object{}+{:#x}", address.object_number, address.offset),
    );
    let effects = if entry.code32 {
        doslet_x86::analysis::decode(&entry.bytes[..usize::from(entry.available)], entry.ip)
            .map_or_else(
                || "effect/flow unavailable to shared analysis decoder".to_owned(),
                |decoded| {
                    format!(
                        "len{} width{} {:?}; {:?}",
                        decoded.length, decoded.width, decoded.effect, decoded.flow
                    )
                },
            )
    } else {
        "code16: shared analysis decoder requires code32".to_owned()
    };
    println!(
        "{:>10} {:#010x} code{} {:<24} {:<33} family={} compare={} bytes=[{}] {}",
        row.count,
        entry.linear,
        if entry.code32 { 32 } else { 16 },
        location,
        row.category.name(),
        family(entry),
        row.compared,
        bytes,
        effects
    );
}

fn main() -> Result<()> {
    let mut args = Vec::new();
    let mut max_work_per_call = None;
    let mut options = std::env::args_os().skip(1);
    while let Some(argument) = options.next() {
        if argument == "--max-work-per-call" && max_work_per_call.is_none() {
            max_work_per_call = Some(
                options
                    .next()
                    .and_then(|value| value.to_str().map(str::to_owned))
                    .ok_or("--max-work-per-call requires an integer")?
                    .parse::<u32>()?,
            );
        } else if argument
            .to_str()
            .is_some_and(|value| value.starts_with("--"))
        {
            return Err("invalid or repeated coverage option".into());
        } else {
            args.push(argument);
        }
    }
    if !(3..=4).contains(&args.len()) && args.len() != 6 {
        return Err(
            "usage: inspect_static_coverage CONFIG.toml INITIAL.state BINDINGS.json [STEPS [PROFILE.json HISTORICAL_BINDINGS.json]] [--max-work-per-call N]".into(),
        );
    }
    let steps = args
        .get(3)
        .map(|value| -> Result<u64> { Ok(value.to_str().ok_or("STEPS must be UTF-8")?.parse()?) })
        .transpose()?
        .unwrap_or(DEFAULT_STEPS);
    if steps == 0 || steps > MAX_STEPS {
        return Err(format!("STEPS must be 1..={MAX_STEPS}").into());
    }
    let config = Path::new(&args[0]);
    let procedure_input::Input { exe, media } = procedure_input::load(config)?;
    let state = read(Path::new(&args[1]), 256 * 1024 * 1024)?;
    let mut session = Session::load_state_with_media(&state, &media)?;
    let declarations = bindings_from_json(&exe, &read(Path::new(&args[2]), 64 * 1024)?)?;
    let le = LeImage::discover(&exe, LeLimits::default())?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    let binding = DosFlatBindingPlan::new(&image, &declarations, BindingLimits::default())?;
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default())?;
    let procedures = ProcedurePlan::discover(&graph, ProcedureLimits::default())?;
    let call_procedures = CallProcedurePlan::discover(&graph, CallProcedureLimits::default())?;
    let memory_procedures = PreparedMemoryProcedures::from_bound_graph(
        &graph,
        &binding,
        MemoryProcedureLimits {
            max_work_per_call,
            ..MemoryProcedureLimits::default()
        },
    )?;
    use doslet_x86_wasm::static_program::procedure_memory::{
        ProcedureCompileOptions, ProcedureGraphProgram,
    };
    let compile = |program: &ProcedureGraphProgram| {
        let options = ProcedureCompileOptions::default();
        match max_work_per_call {
            Some(limit) => program.compile_with_work_limit(&options, limit),
            None => program.compile(&options),
        }
    };
    let mut emitted = ProcedureGraphProgram {
        functions: Vec::new(),
    };
    let mut compile_rejections = BTreeMap::<String, usize>::new();
    for function in &memory_procedures.program().functions {
        let single = ProcedureGraphProgram {
            functions: vec![function.clone()],
        };
        match compile(&single) {
            Ok(_) => emitted.functions.push(function.clone()),
            Err(error) => *compile_rejections.entry(format!("{error:?}")).or_default() += 1,
        }
    }
    let memory_module = compile(&emitted)?;
    let prepared = PreparedProgram::from_bound_cfg(
        &graph,
        &binding,
        LoweringLimits {
            direct_memory: true,
            isolate_mutable_sources: true,
            ..LoweringLimits::default()
        },
    )?;
    let mut lowered = vec![false; graph.nodes().len()];
    if prepared.program().regions.len() != prepared.sources().len() {
        return Err("lowered region/source counts differ".into());
    }
    for (region, source) in prepared.program().regions.iter().zip(prepared.sources()) {
        let completed = region.completed_instructions();
        if region.id != source.id || completed > source.instructions().len() {
            return Err("lowered region/source identities differ".into());
        }
        // Unsupported terminal instructions can remain in source metadata.
        // Only the prefix actually completed by this region is lowered.
        for address in source.instructions().iter().take(completed) {
            let index = graph
                .nodes()
                .binary_search_by_key(address, |node| node.address)
                .map_err(|_| "lowered instruction missing from CFG")?;
            lowered[index] = true;
        }
    }
    wasm_game::setup(&mut session);
    session.set_adaptive_cpu(false);
    session.debug_cpu_mut().set_execution_labels(None)?;
    // Untimed validation only at replay boundaries; no added per-step checks.
    // Generations detect writes even if the original bytes are later restored.
    let mut procedure_versions = Vec::new();
    let mut procedure_source_matches = Vec::new();
    let mut memory_versions = Vec::new();
    let mut memory_source_matches = true;
    for index in 0..memory_procedures.entries().len() {
        for address in memory_procedures
            .sources(index)
            .ok_or("memory procedure sources missing")?
        {
            let node = &graph.nodes()[graph
                .nodes()
                .binary_search_by_key(address, |node| node.address)
                .map_err(|_| "memory procedure node missing")?];
            let linear = binding
                .object_binding(address.object_number)
                .and_then(|object| object.linear_base.checked_add(address.offset))
                .ok_or("memory procedure binding missing")?;
            let expected = source_window(&binding, *address, usize::from(node.byte_len))
                .ok_or("memory procedure source window missing")?;
            memory_source_matches &= session
                .debug_code_bytes(linear, node.byte_len)
                .is_some_and(|bytes| bytes == &expected[..usize::from(node.byte_len)]);
            memory_versions.push(session.debug_code_version(linear, node.byte_len));
        }
    }
    for index in 0..call_procedures.procedures().len() {
        let mut versions = Vec::new();
        let mut matches = true;
        for node in call_procedures
            .instructions(index)
            .ok_or("procedure nodes missing")?
        {
            let linear = binding
                .object_binding(node.address.object_number)
                .ok_or("procedure object binding missing")?
                .linear_base
                .checked_add(node.address.offset)
                .ok_or("procedure address overflow")?;
            let expected = source_window(&binding, node.address, usize::from(node.byte_len))
                .ok_or("procedure source window missing")?;
            matches &= session
                .debug_code_bytes(linear, node.byte_len)
                .is_some_and(|bytes| bytes == &expected[..usize::from(node.byte_len)]);
            versions.push(session.debug_code_version(linear, node.byte_len));
        }
        procedure_versions.push(versions);
        procedure_source_matches.push(matches);
    }
    let initial_steps = session.steps();
    let initial_nanos = session.emulated_nanos();
    let initial_irq = session.debug_irq_entries();
    let mut histogram = HashMap::<Entry, u64>::with_capacity(MAX_UNIQUE);
    let mut first_states = (args.len() == 6).then(|| HashMap::with_capacity(MAX_UNIQUE));
    let capacity = histogram.capacity();
    let mut pcm = [[0; 2]; 2048];
    let mut midi = [0; 2048];
    let mut cpu_steps = 0u64;
    let mut non_cpu_steps = 0u64;
    let mut calls = 0u64;
    let mut io_ports = BTreeMap::<(u32, u16), u64>::new();
    for _ in 0..steps {
        let before = session.steps();
        let snapshot = first_states
            .as_ref()
            .map(|_| hot::Snapshot::capture(&session));
        let observed = session.debug_observed_step();
        let completed = session.steps() - before;
        calls += 1;
        if completed > 1 {
            return Err("observed single step unexpectedly batched".into());
        }
        if let Some(observed) = observed {
            if completed != 1 {
                return Err(format!(
                    "observed instruction did not complete: {:?}",
                    session.fault()
                )
                .into());
            }
            cpu_steps += 1;
            let entry = Entry {
                linear: observed.linear,
                ip: observed.ip,
                code32: observed.code32,
                available: observed.available,
                bytes: observed.bytes,
            };
            // These unprefixed IN/OUT forms preserve DX. Sample the actual
            // port instead of inferring it from the following mask.
            if matches!(entry.bytes[0], 0xec..=0xef) {
                let port = session.registers().get(doslet_x86::Gpr16::Dx);
                *io_ports.entry((entry.linear, port)).or_default() += 1;
            }
            if let Some(count) = histogram.get_mut(&entry) {
                *count += 1;
            } else {
                if histogram.len() == MAX_UNIQUE {
                    return Err("65,536 unique CPU-entry limit exhausted".into());
                }
                histogram.insert(entry, 1);
                if let (Some(states), Some(snapshot)) = (&mut first_states, snapshot) {
                    states.insert(entry, snapshot);
                }
            }
        } else {
            non_cpu_steps += completed;
        }
        if calls.is_multiple_of(4096) {
            while session.drain_pcm(&mut pcm) != 0 {}
            while session.drain_midi(&mut midi) != 0 {}
        }
        if session.status() != SessionStatus::Running || completed == 0 || session.storage_waiting()
        {
            break;
        }
    }
    let completed = session.steps() - initial_steps;
    if histogram.capacity() != capacity
        || histogram.values().sum::<u64>() != cpu_steps
        || cpu_steps + non_cpu_steps != completed
    {
        return Err("coverage histogram/accounting invariant failed".into());
    }
    let mut rows: Vec<_> = histogram
        .into_iter()
        .map(|(entry, count)| classify(entry, count, &binding, &graph, &lowered))
        .collect();
    rows.sort_unstable_by_key(|row| (Reverse(row.count), row.entry));
    if args.len() == 6 {
        hot::report(
            &rows,
            &graph,
            &binding,
            &memory_procedures,
            &exe,
            Path::new(&args[4]),
            Path::new(&args[5]),
            first_states.as_ref().expect("HOT replay snapshots enabled"),
        )?;
    }
    println!(
        "Potential static coverage; procedure WASM emitted without native compilation or execution."
    );
    println!("Input game files are read-only; guest writes stay in the replay session.");
    println!(
        "Replay: requested={steps} calls={calls} completed={completed} CPU={cpu_steps} non-CPU={non_cpu_steps} unique={} capacity={capacity}",
        rows.len()
    );
    println!(
        "Final: status={:?} fault={:?} PC={:#x} emulated_ns={} IRQ_entries={}",
        session.status(),
        session.fault(),
        session.debug_pc(),
        session.emulated_nanos() - initial_nanos,
        session.debug_irq_entries() - initial_irq
    );
    println!("DPMI host statistics: {:?}", session.dpmi_stats());
    let mut io_ports: Vec<_> = io_ports.into_iter().collect();
    io_ports.sort_unstable_by_key(|&(key, count)| (Reverse(count), key));
    for ((linear, port), count) in io_ports.into_iter().take(20) {
        println!("Observed DX port: entry={linear:#x} port={port:#x} count={count}");
    }
    println!(
        "Static: nodes={} lowered_nodes={} report={:?}",
        graph.nodes().len(),
        lowered.iter().filter(|&&value| value).count(),
        prepared.report()
    );
    println!(
        "Memory/branch leaf analysis only: procedures={} source_nodes={} rejected_roots={}; no procedure WASM executed",
        memory_procedures.entries().len(),
        (0..memory_procedures.entries().len())
            .map(|index| memory_procedures.sources(index).map_or(0, <[_]>::len))
            .sum::<usize>(),
        memory_procedures.diagnostics().len()
    );
    let reached_memory_procedures = emitted
        .functions
        .iter()
        .filter(|function| {
            rows.iter()
                .any(|row| row.entry.code32 && row.entry.linear == function.entry.offset)
        })
        .count();
    let memory_entry_visits: u64 = emitted
        .functions
        .iter()
        .map(|function| {
            rows.iter()
                .filter(|row| row.entry.code32 && row.entry.linear == function.entry.offset)
                .map(|row| row.count)
                .sum::<u64>()
        })
        .sum();
    println!(
        "Memory/branch replay coverage: reached_procedures={reached_memory_procedures} entry_visits={memory_entry_visits}"
    );
    let visits: BTreeMap<ObjectAddress, u64> = rows
        .iter()
        .filter(|row| {
            matches!(
                row.category,
                Category::KnownLowered | Category::KnownUnsupported
            )
        })
        .filter_map(|row| row.address.map(|address| (address, row.count)))
        .fold(BTreeMap::new(), |mut counts, (address, count)| {
            *counts.entry(address).or_default() += count;
            counts
        });
    let mut procedure_sources = vec![false; graph.nodes().len()];
    for index in 0..memory_procedures.entries().len() {
        let id = memory_procedures.program().functions[index].id;
        if !emitted.functions.iter().any(|function| function.id == id) {
            continue;
        }
        for address in memory_procedures
            .sources(index)
            .ok_or("missing procedure sources")?
        {
            let at = graph
                .nodes()
                .binary_search_by_key(address, |node| node.address)
                .map_err(|_| "procedure source missing from CFG")?;
            procedure_sources[at] = true;
        }
    }
    let source_visits: u64 = graph
        .nodes()
        .iter()
        .zip(&procedure_sources)
        .filter(|(_, included)| **included)
        .map(|(node, _)| visits.get(&node.address).copied().unwrap_or_default())
        .sum();
    println!(
        "Memory/branch emitted-source visits={source_visits} ({:.3}% of observed CPU instructions); matching source/mode, inclusive union, not actual AOT execution",
        source_visits as f64 * 100.0 / cpu_steps.max(1) as f64
    );
    let mut reached: Vec<_> = emitted
        .functions
        .iter()
        .filter_map(|function| {
            let count: u64 = rows
                .iter()
                .filter(|row| row.entry.code32 && row.entry.linear == function.entry.offset)
                .map(|row| row.count)
                .sum();
            (count != 0).then_some((count, function))
        })
        .collect();
    reached.sort_unstable_by_key(|(count, function)| (Reverse(*count), function.entry));
    for (count, function) in reached.into_iter().take(20) {
        println!(
            "Reached memory/branch entry={:#x} visits={count} static_nodes={}",
            function.entry.offset,
            function.blocks.len()
        );
    }
    println!(
        "Memory/branch emitted procedures={} maximum_work_per_call={max_work_per_call:?}",
        emitted.functions.len()
    );
    for (reason, count) in compile_rejections {
        println!("{count:>10} memory/branch compile rejection: {reason}");
    }
    let memory_pages_unchanged = memory_versions.iter().all(|version| {
        version
            .as_ref()
            .is_some_and(|version| session.debug_code_valid(version))
    });
    println!(
        "Memory/branch leaves: single_WASM_module_bytes={} initial_source_matches={memory_source_matches} code_pages_unchanged={memory_pages_unchanged}; generated from bound EXE, not executed",
        memory_module.wasm().len()
    );
    let mut memory_rejections = BTreeMap::<String, usize>::new();
    for diagnostic in memory_procedures.diagnostics() {
        *memory_rejections
            .entry(format!("{:?}", diagnostic.reason))
            .or_default() += 1;
    }
    for (reason, count) in memory_rejections {
        println!("{count:>10} memory/branch leaf rejection: {reason}");
    }
    let mut observed_rejections: Vec<_> = memory_procedures
        .diagnostics()
        .iter()
        .filter_map(|diagnostic| {
            let entry_visits = visits.get(&diagnostic.entry).copied().unwrap_or_default();
            let stopped_visits = visits
                .get(&diagnostic.instruction)
                .copied()
                .unwrap_or_default();
            (entry_visits != 0 || stopped_visits != 0).then_some((
                stopped_visits,
                entry_visits,
                diagnostic,
            ))
        })
        .collect();
    observed_rejections.sort_unstable_by_key(|(stopped, entries, diagnostic)| {
        (Reverse(*stopped), Reverse(*entries), diagnostic.entry)
    });
    println!(
        "Observed rejected roots (top30; static first_bad counts can overlap; UnsupportedLoop records the root):"
    );
    for (stopped, entries, diagnostic) in observed_rejections.into_iter().take(30) {
        println!(
            "root={:?} entry_visits={entries} first_bad={:?} bad_visits={stopped} reason={:?}",
            diagnostic.entry, diagnostic.instruction, diagnostic.reason
        );
    }
    println!(
        "Procedure analysis only: straight_line_leaves={} instructions={} rejected_roots={}; no procedure WASM executed",
        procedures.procedures().len(),
        procedures
            .procedures()
            .iter()
            .map(|procedure| procedure.instruction_count())
            .sum::<usize>(),
        procedures.diagnostics().len(),
    );
    let mut procedure_rejections = BTreeMap::<String, usize>::new();
    for diagnostic in procedures.diagnostics() {
        *procedure_rejections
            .entry(format!("{:?}", diagnostic.reason))
            .or_default() += 1;
    }
    for (reason, count) in procedure_rejections {
        println!("{count:>10} procedure rejection: {reason}");
    }
    println!(
        "Direct-call closure analysis only: procedures={} callers={} body_instructions={} max_expanded_work={} max_call_depth={} rejected_roots={}; no procedure WASM executed",
        call_procedures.procedures().len(),
        call_procedures
            .procedures()
            .iter()
            .filter(|procedure| procedure.max_call_depth != 0)
            .count(),
        call_procedures
            .procedures()
            .iter()
            .map(|procedure| procedure.body.instruction_count())
            .sum::<usize>(),
        call_procedures
            .procedures()
            .iter()
            .map(|procedure| procedure.expanded_instruction_count)
            .max()
            .unwrap_or(0),
        call_procedures
            .procedures()
            .iter()
            .map(|procedure| procedure.max_call_depth)
            .max()
            .unwrap_or(0),
        call_procedures.diagnostics().len(),
    );
    let mut call_rejections = BTreeMap::<String, usize>::new();
    for (index, procedure) in call_procedures.procedures().iter().enumerate() {
        let unchanged = procedure_versions[index].iter().all(|version| {
            version
                .as_ref()
                .is_some_and(|version| session.debug_code_valid(version))
        });
        println!(
            "Accepted procedure: object{}+{:#x} instructions={} expanded={} depth={} local_reads={} local_writes={} return_width={} cleanup={} initial_source_matches={} code_pages_unchanged={}",
            procedure.body.entry.object_number,
            procedure.body.entry.offset,
            procedure.body.instruction_count(),
            procedure.expanded_instruction_count,
            procedure.max_call_depth,
            procedure.body.memory_read_instructions,
            procedure.body.memory_write_instructions,
            procedure.body.return_operand_width,
            procedure.body.return_pop_bytes,
            procedure_source_matches[index],
            unchanged,
        );
    }
    for diagnostic in call_procedures.diagnostics() {
        *call_rejections
            .entry(format!("{:?}", diagnostic.reason))
            .or_default() += 1;
    }
    for (reason, count) in call_rejections {
        println!("{count:>10} call closure rejection: {reason}");
    }
    let mut static_gaps = BTreeMap::<&'static str, usize>::new();
    for (node, &is_lowered) in graph.nodes().iter().zip(&lowered) {
        if is_lowered {
            continue;
        }
        let bytes = source_window(&binding, node.address, usize::from(node.byte_len))
            .ok_or("static gap source window unavailable")?;
        let entry = Entry {
            linear: binding
                .object_binding(node.address.object_number)
                .ok_or("static gap binding missing")?
                .linear_base
                .wrapping_add(node.address.offset),
            ip: node.address.offset,
            code32: node.bitness == DefaultBitness::Bits32,
            available: node.byte_len,
            bytes,
        };
        *static_gaps.entry(family(&entry)).or_default() += 1;
    }
    let mut static_gaps: Vec<_> = static_gaps.into_iter().collect();
    static_gaps.sort_unstable_by_key(|&(family, count)| (Reverse(count), family));
    println!("Static untranslated family hints (all discovered nodes, not execution counts):");
    for (family, count) in static_gaps {
        println!("{count:>10} {family}");
    }
    for category in Category::ALL {
        let mut count = 0u64;
        let mut unique = 0usize;
        for row in rows.iter().filter(|row| row.category == category) {
            count += row.count;
            unique += 1;
        }
        println!(
            "{:>10} {:>7.3}% {:>6} unique {}",
            count,
            count as f64 * 100.0 / cpu_steps.max(1) as f64,
            unique,
            category.name()
        );
    }
    let mut families = BTreeMap::<(Category, &'static str), (u64, usize)>::new();
    for row in rows
        .iter()
        .filter(|row| row.category != Category::KnownLowered)
    {
        let family = families
            .entry((row.category, family(&row.entry)))
            .or_default();
        family.0 += row.count;
        family.1 += 1;
    }
    let mut families: Vec<_> = families.into_iter().collect();
    families.sort_unstable_by_key(|&(key, (count, _))| (Reverse(count), key));
    println!("Unsupported/uncovered family hints (top30; prefixes/opcodes only):");
    for ((category, family), (count, unique)) in families.into_iter().take(30) {
        println!(
            "{count:>10} {unique:>6} unique {family:<20} {}",
            category.name()
        );
    }
    println!("Hottest observed entries (top30, including lowered instructions):");
    for row in rows.iter().take(30) {
        print_row(row);
    }
    println!("Unsupported/uncovered entries (top30; count descending, address tie-break):");
    for row in rows
        .iter()
        .filter(|row| row.category != Category::KnownLowered)
        .take(30)
    {
        print_row(row);
        if row.category == Category::ChangedBytes {
            let expected = row
                .address
                .and_then(|address| source_window(&binding, address, row.compared));
            println!(
                "           bound expected={}",
                expected.map_or_else(
                    || "unavailable".to_owned(),
                    |bytes| {
                        bytes[..row.compared]
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<Vec<_>>()
                            .join(" ")
                    }
                )
            );
        }
    }
    println!(
        "Source comparison uses exact CFG instruction lengths; outside CFG uses shared-decoder length or the complete available window. Live code/context admission and device grants are not inferred. This diagnostic is not a benchmark or differential acceptance."
    );
    if session.fault().is_some() || completed != steps || session.storage_waiting() {
        return Err("replay did not complete its requested no-fault bound".into());
    }
    Ok(())
}
