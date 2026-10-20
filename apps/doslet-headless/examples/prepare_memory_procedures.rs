//! Offline original-EXE conversion for a complete native procedure test bundle.
#[allow(dead_code)]
#[path = "../../common/host_dirs.rs"]
mod host_dirs;
#[allow(dead_code)]
#[path = "../../common/le_layout.rs"]
mod le_layout;
#[allow(dead_code)]
#[path = "../../common/procedure_cache.rs"]
mod procedure_cache;
#[allow(dead_code)]
#[path = "../../common/procedure_input.rs"]
mod procedure_input;
use doslet_aot::{
    SymbolicImage,
    binding::{BindingLimits, DosFlatBindingPlan},
    profile::{BoundGuidance, GuidanceLimits, Profile, ProfileLimits},
};
use doslet_static_game_probe::prepare::{bindings_dpmi, bindings_from_json};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    io::Read,
    path::{Path, PathBuf},
};
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
    if args.len() < 2 {
        return Err("usage: prepare_memory_procedures CONFIG.toml BINDINGS.json [OUTPUT_DIR] [--max-work-per-call N] [--hot-profile JSON --profile-bindings JSON] [--copy-kernels | --computation-kernels] [--kernels-only] [--kernel-interrupts defer|before-irq]".into());
    }
    let mut max_work_per_call = None;
    let mut profile_path = None;
    let mut profile_bindings = None;
    let mut copy_kernels = false;
    let mut computation_kernels = false;
    let mut kernels_only = false;
    let mut kernel_interrupts = None;
    let explicit_output = args
        .get(2)
        .filter(|arg| !arg.to_string_lossy().starts_with("--"));
    let mut options = args[2 + usize::from(explicit_output.is_some())..].iter();
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
    let live_bindings = read(Path::new(&args[1]), 64 * 1024)?;
    let declarations = bindings_from_json(&exe, &live_bindings)?;
    // Bound code is valid only for the DPMI mode that placed it.
    let dpmi = bindings_dpmi(&live_bindings)?;
    // HOT guidance only narrows the selection; it never relaxes code proofs.
    let mut verified_hot: Option<Vec<bool>> = None;
    let mut keep = |graph: &doslet_aot::cfg::StaticCfg<'_>,
                    image: &SymbolicImage,
                    sources: &[doslet_aot::ObjectAddress]|
     -> Result<bool> {
        let (Some(path), Some(layout)) = (profile_path, profile_bindings) else {
            return Ok(true);
        };
        if verified_hot.is_none() {
            let profile = Profile::parse(&read(path, 64 * 1024 * 1024)?, ProfileLimits::default())?;
            let declarations = bindings_from_json(&exe, &read(layout, 64 * 1024)?)?;
            let historical =
                DosFlatBindingPlan::new(image, &declarations, BindingLimits::default())?;
            let guidance = BoundGuidance::bind(
                &profile,
                graph,
                &historical,
                None,
                GuidanceLimits::default(),
            )?;
            println!("HOT source verification: {:?}", guidance.report());
            verified_hot = Some(
                guidance
                    .instructions()
                    .iter()
                    .map(|instruction| instruction.verified_hot())
                    .collect(),
            );
        }
        let hot = verified_hot.as_ref().expect("HOT labels");
        let mut hot_nodes = 0usize;
        let mut relevant_nodes = 0usize;
        for address in sources {
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
        Ok(hot_nodes != 0 && hot_nodes * 2 >= relevant_nodes)
    };
    let recipe = procedure_cache::Recipe {
        max_work_per_call,
        copy_kernels,
        computation_kernels,
        kernels_only,
        kernel_interrupts,
        hot_profile: profile_path.is_some(),
    };
    // The desktop finds a cached module by the key of its live layout.
    let layout = {
        let le =
            doslet_executable::LeImage::discover(&exe, doslet_executable::LeLimits::default())?;
        let image = SymbolicImage::from_le(&le, doslet_aot::ImageLimits::default())?;
        le_layout::Layout::new(&image, declarations.clone())?.key()
    };
    let module = procedure_cache::build(
        &exe,
        &declarations,
        dpmi,
        &layout,
        &recipe,
        &mut keep,
        &mut |line| println!("{line}"),
    )?;
    let output = match explicit_output {
        Some(path) => PathBuf::from(path),
        None => host_dirs::HostDirs::detect()?
            .aot_module(&format!("{:x}", Sha256::digest(&exe)), &layout)
            .parent()
            .ok_or("invalid AOT cache path")?
            .to_path_buf(),
    };
    let output = output.as_path();
    fs::create_dir_all(output)?;
    fs::write(output.join("procedures.wasm"), &module.bytes)?;
    println!(
        "Prepared {} original-EXE procedure/region entries for DPMI {dpmi} in one {}-byte WASM module: {}",
        module.entries,
        module.bytes.len(),
        output.join("procedures.wasm").display()
    );
    Ok(())
}
