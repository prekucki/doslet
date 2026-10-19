//! Capture live DOS-flat LE bindings and a matching portable starting state.
//! Guest media is read-only. Preferred image addresses never select bindings.
#[allow(dead_code)]
#[path = "../../common/procedure_input.rs"]
mod procedure_input;

use doslet_aot::{
    DefaultBitness, ImageLimits, SymbolicImage,
    binding::{BindingLimits, DosFlatBindingPlan, DosFlatObjectBinding, DosFlatSelector},
};
use doslet_executable::{LeFixupSourceType, LeImage, LeLimits};
use doslet_runtime::{Session, SessionStatus};
use doslet_x86::SegmentRegister;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, error::Error, fs, path::Path, time::Instant};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Anchor {
    object: u32,
    offset: u32,
    bytes: [u8; 32],
}

fn anchors(image: &SymbolicImage) -> Vec<Anchor> {
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
        // Spread distinct, relocation-free anchors over the file-backed code.
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

fn discover(
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

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(2..=3).contains(&args.len()) {
        return Err("usage: capture_procedure_layout CONFIG.toml OUTPUT_DIR [MAX_STEPS]".into());
    }
    let limit: u64 = args
        .get(2)
        .map(|v| -> Result<u64> {
            v.to_str()
                .ok_or("invalid steps")?
                .parse::<u64>()
                .map_err(Into::into)
        })
        .transpose()?
        .unwrap_or(20_000_000);
    if limit == 0 || limit > 100_000_000 {
        return Err("max steps must be 1..100000000".into());
    }
    let input = procedure_input::load(Path::new(&args[0]))?;
    let le = LeImage::discover(&input.exe, LeLimits::default())?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    for object in image.objects() {
        eprintln!(
            "object{}: {:?}, {} bytes, executable={}, preferred={:#x}",
            object.object_number(),
            object.default_bitness(),
            object.bytes().len(),
            object.metadata().is_executable(),
            object.metadata().preferred_base,
        );
    }
    let anchors = anchors(&image);
    let mut session = input.media;
    session.set_dynamic_core(false);
    session.set_decode_cache(true);
    session.set_idle_loops(false);
    session.set_vga_wait_acceleration(false);
    session.set_adaptive_cpu(false);
    session.set_adaptive_idle(false);
    session.start();
    let start = Instant::now();
    let mut next_probe = 0;
    let mut last_reason = String::new();
    let mut saved_diagnostic = false;
    let mut pcm = [doslet_pcm::EMPTY; 4096];
    let mut midi = [0; 4096];
    while session.steps() < limit && start.elapsed().as_secs() < 180 {
        session.advance(4096.min((limit - session.steps()) as u32));
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
        match discover(&session, &image, &anchors) {
            Ok(bindings) => {
                let directory = Path::new(&args[1]);
                fs::create_dir_all(directory)?;
                let rows: Vec<_> = bindings
                    .iter()
                    .map(|binding| {
                        let mut row = json!({
                            "object_number": binding.object_number,
                            "linear_base": binding.linear_base,
                            "selector": binding.normal.map(|selector| selector.value),
                        });
                        if let Some(alias) = binding.alias {
                            row["alias_selector"] = json!(alias.value);
                        }
                        row
                    })
                    .collect();
                fs::write(
                    directory.join("bindings.json"),
                    serde_json::to_vec_pretty(&json!({"objects":rows}))?,
                )?;
                // Capture ran with raw polling for reproducible layout probes.
                // The exported native test starts with normal bounded VGA waits.
                session.set_vga_wait_acceleration(true);
                fs::write(directory.join("start.state"), session.save_state()?)?;
                let frame = session.presentation_frame();
                let mut rgba = vec![0; frame.width() * frame.height() * 4];
                frame.write_rgba(&mut rgba).map_err(|e| format!("{e:?}"))?;
                let mut png = png::Encoder::new(
                    fs::File::create(directory.join("start.png"))?,
                    frame.width() as u32,
                    frame.height() as u32,
                );
                png.set_color(png::ColorType::Rgba);
                png.set_depth(png::BitDepth::Eight);
                png.write_header()?.write_image_data(&rgba)?;
                let code_fixups = image
                    .relocations()
                    .filter(|r| {
                        image
                            .object(r.source.object_number)
                            .is_some_and(|object| object.metadata().is_executable())
                    })
                    .count();
                fs::write(
                    directory.join("layout-evidence.json"),
                    serde_json::to_vec_pretty(&json!({
                        "exe_sha256": format!("{:x}", Sha256::digest(&input.exe)),
                        "steps": session.steps(),
                        "pc": session.debug_pc(),
                        "vga_wait_acceleration": true,
                        "anchors": anchors.len(),
                        "code_fixups_verified": code_fixups,
                        "bindings": rows,
                    }))?,
                )?;
                println!(
                    "Captured {} live object bindings at step{} PC{:#x} in {}",
                    bindings.len(),
                    session.steps(),
                    session.debug_pc(),
                    directory.display()
                );
                return Ok(());
            }
            Err(error) => {
                if !saved_diagnostic {
                    let directory = Path::new(&args[1]);
                    fs::create_dir_all(directory)?;
                    fs::write(directory.join("diagnostic.state"), session.save_state()?)?;
                    saved_diagnostic = true;
                }
                let reason = error.to_string();
                if reason != last_reason {
                    eprintln!(
                        "step{} PC{:#x}: {reason}",
                        session.steps(),
                        session.debug_pc()
                    );
                    last_reason = reason;
                }
            }
        }
    }
    Err(format!(
        "live bindings not established after {} steps: {last_reason}",
        session.steps()
    )
    .into())
}
