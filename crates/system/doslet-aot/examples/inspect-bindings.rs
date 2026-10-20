//! Validate patches under a declared SYNTHETIC layout; never loads or writes an
//! executable image. Preferred bases are chosen only for this explicit test.
use std::{env, error::Error, fs::File, io::Read};

use doslet_aot::{
    DefaultBitness, ImageLimits, SymbolicImage,
    binding::{BindingLimits, DosFlatBindingPlan, DosFlatObjectBinding, DosFlatSelector},
};
use doslet_executable::{LeFixupSourceType, LeImage, LeLimits};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let path = arguments.next().ok_or("usage: inspect-bindings EXE")?;
    if arguments.next().is_some() {
        return Err("usage: inspect-bindings EXE".into());
    }
    let limits = LeLimits::default();
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limits.max_file_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    let le = LeImage::discover(&bytes, limits)?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    let mut needs_alias = vec![false; image.objects().len()];
    for relocation in image.relocations().filter(|relocation| relocation.is_alias) {
        needs_alias[usize::try_from(relocation.target.object_number - 1)?] = true;
    }
    let mut bindings = Vec::new();
    for (object, &needs_alias) in image.objects().iter().zip(&needs_alias) {
        let base = object.metadata().preferred_base;
        // Explicit test selector values; these are not observed guest selectors.
        let test_selector = u16::try_from(object.object_number())?
            .checked_mul(16)
            .and_then(|value| value.checked_add(0x40))
            .ok_or("synthetic selector space exhausted")?;
        let normal = match object.default_bitness() {
            DefaultBitness::Bits32 => DosFlatSelector {
                value: 0x28,
                base: 0,
                limit: u32::MAX,
                default_bitness: DefaultBitness::Bits32,
            },
            DefaultBitness::Bits16 => DosFlatSelector {
                value: test_selector,
                base,
                limit: 0xffff,
                default_bitness: DefaultBitness::Bits16,
            },
        };
        bindings.push(DosFlatObjectBinding {
            object_number: object.object_number(),
            linear_base: base,
            normal: Some(normal),
            alias: needs_alias.then_some(DosFlatSelector {
                value: test_selector
                    .checked_add(8)
                    .ok_or("synthetic selector space exhausted")?,
                base,
                limit: 0xffff,
                default_bitness: DefaultBitness::Bits16,
            }),
        });
    }
    let plan = DosFlatBindingPlan::new(&image, &bindings, BindingLimits::default())?;
    let mut counts = [0usize; 3];
    for patch in plan.patches() {
        counts[match patch.kind {
            LeFixupSourceType::Offset32 => 0,
            LeFixupSourceType::SelfRelative32 => 1,
            LeFixupSourceType::Selector16 => 2,
            _ => unreachable!("plan only publishes supported kinds"),
        }] += 1;
    }
    println!(
        "SYNTHETIC DOS-flat layout: preferred bases explicitly selected; declared test selectors, not live guest bindings"
    );
    println!(
        "objects: {}; patches: {}; Offset32: {}; SelfRelative32: {}; Selector16: {}",
        image.objects().len(),
        plan.patches().len(),
        counts[0],
        counts[1],
        counts[2]
    );
    println!(
        "Offline plan only; no encoded image emitted and no guest load or execution verified."
    );
    Ok(())
}
