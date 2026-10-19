//! Offline compiler diagnostic. The mandatory flag declares test-only bindings;
//! it does not inspect a live loader or claim that the emitted module runs a game.
use doslet_aot::{
    DefaultBitness, ImageLimits, SymbolicImage,
    binding::{BindingLimits, DosFlatBindingPlan, DosFlatObjectBinding, DosFlatSelector},
    cfg::{CfgLimits, StaticCfg},
    profile::{BoundGuidance, GuidanceLimits, Profile, ProfileLimits, Tier},
    wasm::{LoweringLimits, PreparedProgram},
};
use doslet_executable::{LeImage, LeLimits};
use doslet_x86_wasm::static_program::CompileOptions;
use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fs::{File, OpenOptions},
    io::{Read, Write},
};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let usage = "usage: translate-exe --synthetic-layout EXE [NEW-OUTPUT.wasm] [--object-base NUMBER:ADDRESS] [--profile JSON] [--expected-identity ID] [--uncounted-hot]";
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("--synthetic-layout")) {
        return Err(usage.into());
    }
    let path = arguments.next().ok_or(usage)?;
    let mut output = None;
    let mut profile_path = None;
    let mut expected_identity = None;
    let mut uncounted_hot = false;
    let mut object_bases = BTreeMap::new();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--object-base") => {
                let value = arguments
                    .next()
                    .ok_or(usage)?
                    .into_string()
                    .map_err(|_| usage)?;
                let (number, base) = value.split_once(':').ok_or(usage)?;
                let number: u32 = number.parse()?;
                let base = if let Some(hex) = base.strip_prefix("0x") {
                    u32::from_str_radix(hex, 16)?
                } else {
                    base.parse()?
                };
                if number == 0 || object_bases.insert(number, base).is_some() {
                    return Err("duplicate/zero object binding override".into());
                }
            }
            Some("--profile") if profile_path.is_none() => {
                profile_path = Some(arguments.next().ok_or(usage)?)
            }
            Some("--expected-identity") if expected_identity.is_none() => {
                expected_identity = Some(
                    arguments
                        .next()
                        .ok_or(usage)?
                        .into_string()
                        .map_err(|_| "identity must be UTF-8")?,
                )
            }
            Some("--uncounted-hot") if !uncounted_hot => uncounted_hot = true,
            Some(value) if value.starts_with("--") => return Err(usage.into()),
            _ if output.is_none() => output = Some(argument),
            _ => return Err(usage.into()),
        }
    }
    if profile_path.is_none() && (expected_identity.is_some() || uncounted_hot) {
        return Err("identity/HOT options require --profile".into());
    }
    let limits = LeLimits::default();
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limits.max_file_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    let le = LeImage::discover(&bytes, limits)?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    for &number in object_bases.keys() {
        if image.object(number).is_none() {
            return Err("unknown object binding override".into());
        }
    }
    let mut bindings = Vec::new();
    for object in image.objects() {
        // These are explicit diagnostic choices, never observed live values.
        let bitness = object.default_bitness();
        let base = object_bases
            .get(&object.object_number())
            .copied()
            .unwrap_or(object.metadata().preferred_base);
        let value = if bitness == DefaultBitness::Bits32 {
            0x28
        } else {
            u16::try_from(object.object_number())?
                .checked_mul(16)
                .and_then(|v| v.checked_add(0x40))
                .ok_or("test selector space exhausted")?
        };
        bindings.push(DosFlatObjectBinding {
            object_number: object.object_number(),
            linear_base: base,
            normal: Some(DosFlatSelector {
                value,
                base: if bitness == DefaultBitness::Bits32 {
                    0
                } else {
                    base
                },
                limit: if bitness == DefaultBitness::Bits32 {
                    u32::MAX
                } else {
                    0xffff
                },
                default_bitness: bitness,
            }),
            alias: None,
        });
    }
    let binding = DosFlatBindingPlan::new(&image, &bindings, BindingLimits::default())?;
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default())?;
    let prepared = if let Some(path) = profile_path {
        let limits = ProfileLimits::default();
        let mut json = Vec::new();
        File::open(path)?
            .take(limits.max_input_bytes as u64 + 1)
            .read_to_end(&mut json)?;
        let profile = Profile::parse(&json, limits)?;
        let guidance = BoundGuidance::bind(
            &profile,
            &graph,
            &binding,
            expected_identity.as_deref(),
            GuidanceLimits::default(),
        )?;
        println!(
            "profile identity: {:?}; check: {:?}; binding report: {:?}",
            profile.identity(),
            guidance.identity_check(),
            guidance.report()
        );
        PreparedProgram::from_guided_cfg(
            &graph,
            &binding,
            &guidance,
            uncounted_hot,
            LoweringLimits::default(),
        )?
    } else {
        PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default())?
    };
    let compiled = prepared.program().compile(&CompileOptions::default())?;
    let engine = wasmi::Engine::default();
    let _validated = wasmi::Module::new(&engine, compiled.wasm())?;
    let report = prepared.report();
    println!(
        "SYNTHETIC layout; original EXE bytes + explicit relocation bindings; profiles only guide verified instructions"
    );
    let mut tiers = [0usize; 4];
    for instruction in prepared.guidance() {
        tiers[match instruction.tier {
            Tier::Hot => 0,
            Tier::Cold => 1,
            Tier::Grey => 2,
            Tier::Irq => 3,
        }] += 1;
    }
    let uncounted_regions = prepared
        .program()
        .regions
        .iter()
        .filter(|region| {
            region.accounting == doslet_x86_wasm::static_program::Accounting::UncountedHot
        })
        .count();
    println!(
        "instruction guidance HOT/COLD/GREY/IRQ: {tiers:?}; uncounted HOT regions: {uncounted_regions}"
    );
    println!(
        "encoded address-dispatch body bytes: {}",
        compiled.dispatcher_bytes()
    );
    println!(
        "discovered: {}; register operations: {}; direct transfers: {}; fallback instructions: {}; regions: {}; external fallback stubs: {}; WASM bytes: {}",
        report.discovered_instructions,
        report.register_instructions,
        report.direct_transfers,
        report.fallback_instructions,
        report.generated_regions,
        report.external_stubs,
        compiled.wasm().len()
    );
    println!(
        "One module validated with Wasmi; memory/runtime imports still require a compatible adapter. No game execution verified."
    );
    if let Some(output) = output {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)?;
        file.write_all(compiled.wasm())?;
        println!("Wrote new local diagnostic module.");
    }
    Ok(())
}
