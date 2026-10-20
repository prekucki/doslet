//! Temporary bounded read-only binary contract diagnostic.
#[allow(dead_code)]
#[path = "../../doslet/src/archive.rs"]
mod archive;
use doslet_aot::{
    ImageLimits, ObjectAddress, SymbolicImage,
    binding::{BindingLimits, DosFlatBindingPlan},
    cfg::{CfgLimits, EdgeTarget, StaticCfg},
};
use doslet_executable::{LeImage, LeLimits};
use doslet_static_game_probe::prepare::bindings_from_json;
use std::{error::Error, fs, sync::Arc};
fn main() -> Result<(), Box<dyn Error>> {
    let zip =
        archive::GameArchive::new(Arc::new(fs::read("/home/reqc/games/retro/dos/ROTT.zip")?))?;
    let selected = zip
        .programs
        .iter()
        .find(|p| p.ends_with("ROTT.EXE"))
        .ok_or("no EXE")?;
    let exe = zip.program_bytes(selected)?;
    let le = LeImage::discover(&exe, LeLimits::default())?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    let decl = bindings_from_json(
        &exe,
        &fs::read("target/static-rott-layout/current-bindings.json")?,
    )?;
    let binding = DosFlatBindingPlan::new(&image, &decl, BindingLimits::default())?;
    let roots: Vec<_> = image
        .relocations()
        .filter(|r| r.target.object_number == 3 && r.target.offset == Some(0xc83))
        .map(|r| ObjectAddress {
            object_number: r.source.object_number,
            offset: r.source.offset - 1,
        })
        .collect();
    let graph = StaticCfg::analyze_bound(&binding, &roots, CfgLimits::default())?;
    let target = ObjectAddress {
        object_number: 3,
        offset: 0xc83,
    };
    let mut starts = vec![(3, 0xc70, 0xcca)];
    for edge in graph
        .edges()
        .iter()
        .filter(|e| e.target == EdgeTarget::Address(target))
    {
        println!("INCOMING {edge:?}");
        starts.push((
            edge.source.object_number,
            edge.source.offset.saturating_sub(0x60),
            edge.source.offset + 0x80,
        ));
    }
    for rel in image.relocations().filter(|r| {
        r.target.object_number == 3 && r.target.offset.is_some_and(|o| (0xc70..0xcca).contains(&o))
    }) {
        println!("TARGET_RELOC {rel:?}");
    }
    for (object, begin, end) in starts {
        println!("WINDOW object{object}+{begin:#x}..{end:#x}");
        for node in graph.nodes().iter().filter(|n| {
            n.address.object_number == object && (begin..end).contains(&n.address.offset)
        }) {
            let start = node.address.offset as usize;
            let mut bytes = image.object(object).unwrap().bytes()
                [start..start + usize::from(node.byte_len)]
                .to_vec();
            for patch in binding.object_patches(object).unwrap() {
                for (i, b) in patch.bytes().iter().enumerate() {
                    let at = patch.source.offset as usize + i;
                    if (start..start + bytes.len()).contains(&at) {
                        bytes[at - start] = *b;
                    }
                }
            }
            let linear = binding.object_binding(object).unwrap().linear_base + node.address.offset;
            println!(
                "{linear:08x} object{object}+{:x} {:02x?} {:?} {:?}",
                node.address.offset,
                bytes,
                node.flow,
                doslet_x86::analysis::decode(&bytes, linear)
            );
        }
    }
    Ok(())
}
