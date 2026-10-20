//! Read-only aggregate inspection; no guest bytes or relocated image are emitted.
use doslet_aot::{
    DefaultBitness, ImageLimits, SymbolicImage,
    cfg::{CfgLimits, EdgeTarget, StaticCfg},
    regions::{RegionLimits, RegionPlan},
};
use doslet_executable::{LeImage, LeLimits};
use std::{env, error::Error, fs::File, io::Read};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let path = arguments.next().ok_or("usage: inspect-cfg EXE")?;
    if arguments.next().is_some() {
        return Err("usage: inspect-cfg EXE".into());
    }
    let limits = LeLimits::default();
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limits.max_file_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    let le = LeImage::discover(&bytes, limits)?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default())?;
    let regions = RegionPlan::build(&graph, &[], RegionLimits::default())?;
    let decoded_bytes: u64 = graph
        .nodes()
        .iter()
        .map(|node| u64::from(node.byte_len))
        .sum();
    let code16 = graph
        .nodes()
        .iter()
        .filter(|node| node.bitness == DefaultBitness::Bits16)
        .count();
    let unresolved = graph
        .edges()
        .iter()
        .filter(|edge| matches!(edge.target, EdgeTarget::Unresolved(_)))
        .count();
    println!(
        "instructions: {}; decoded bytes (counting overlaps): {decoded_bytes}; code16: {code16}; code32: {}; edges: {}; unresolved: {unresolved}; stopped paths: {}; overlapping pairs: {}",
        graph.nodes().len(),
        graph.nodes().len() - code16,
        graph.edges().len(),
        graph.diagnostics().len(),
        graph.overlaps().len()
    );
    println!(
        "straight-line regions: {}; maximum instructions per region: {}",
        regions.regions().len(),
        regions
            .regions()
            .iter()
            .map(|region| region.instruction_count())
            .max()
            .unwrap_or(0)
    );
    for diagnostic in graph.diagnostics().iter().take(8) {
        println!(
            "stopped: object {}+{:#x}: {:?}",
            diagnostic.address.object_number, diagnostic.address.offset, diagnostic.reason
        );
    }
    for overlap in graph.overlaps().iter().take(8) {
        println!(
            "overlap: object {} starts {:#x} and {:#x}; shared {:#x}..{:#x}",
            overlap.first.object_number,
            overlap.first.offset,
            overlap.second.offset,
            overlap.start.offset,
            overlap.start.offset + u32::from(overlap.byte_len),
        );
    }
    Ok(())
}
