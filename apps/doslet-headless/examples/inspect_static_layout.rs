//! Read-only layout evidence from an existing state. Candidates are diagnostics,
//! not automatic loader bindings or evidence that generated code can execute.
#[allow(dead_code)]
#[path = "../../doslet/src/archive.rs"]
mod archive;
#[allow(dead_code)]
#[path = "../../common/machine_profile.rs"]
mod machine_profile;

use doslet_aot::{
    DefaultBitness, ImageLimits, ObjectAddress, SymbolicImage,
    cfg::{CfgLimits, StaticCfg},
    profile::{Profile, ProfileLimits, Reason},
};
use doslet_config::{SourceKind, WritePolicy};
use doslet_executable::{LeFixupSourceType, LeImage, LeLimits};
use doslet_runtime::Session;
use doslet_x86::SegmentRegister;
use std::{collections::BTreeMap, error::Error, fs::File, io::Read, path::Path, sync::Arc};

struct Window {
    address: ObjectAddress,
    bits: DefaultBitness,
    bytes: [u8; 15],
    fixed: [bool; 15],
}

fn read(path: &Path, limit: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("diagnostic input exceeds limit".into());
    }
    Ok(bytes)
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 3 {
        return Err(
            "usage: inspect_static_layout CONFIG.toml INITIAL.state SPEED-TIERS.json".into(),
        );
    }
    let config = Path::new(&args[0]);
    let mut spec =
        machine_profile::parse_machine_profile(std::str::from_utf8(&read(config, 1024 * 1024)?)?)?;
    let mount = spec
        .mounts
        .iter()
        .find(|m| m.letter == b'C' && m.kind == SourceKind::BrowserZip)
        .ok_or("ZIP C: required")?;
    let source = config
        .parent()
        .unwrap_or(Path::new("."))
        .join(spec.binding(&mount.source));
    for mount in &mut spec.mounts {
        mount.policy = WritePolicy::Temporary;
    }
    let archive = archive::GameArchive::new(Arc::new(read(&source, archive::MAX_ZIP_BYTES)?))?;
    let wanted = spec.game.executable.replace('\\', "/").to_ascii_uppercase();
    let selected = archive
        .programs
        .iter()
        .find(|p| **p == wanted || p.ends_with(&format!("/{wanted}")))
        .ok_or("profile EXE missing")?;
    let exe = archive.program_bytes(selected)?;
    let media = archive.load_configured(selected, &spec)?;
    let state = read(Path::new(&args[1]), 256 * 1024 * 1024)?;
    let session = Session::load_state_with_media(&state, &media)?;
    let profile = Profile::parse(
        &read(
            Path::new(&args[2]),
            ProfileLimits::default().max_input_bytes,
        )?,
        ProfileLimits::default(),
    )?;
    let le = LeImage::discover(&exe, LeLimits::default())?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default())?;
    let relocations: Vec<_> = image.relocations().collect();
    let mut windows = Vec::new();
    for node in graph.nodes() {
        let object = image
            .object(node.address.object_number)
            .ok_or("CFG object missing")?;
        let start = node.address.offset as usize;
        let Some(source) = object.bytes().get(start..start + 15) else {
            continue;
        };
        // Require all anchor bytes to originate in the file, including when
        // the window crosses adjacent physical-page spans.
        let mut backed = node.address.offset;
        for span in object
            .file_backed_spans()
            .iter()
            .skip_while(|span| span.object_offset + span.byte_len <= node.address.offset)
        {
            if span.object_offset > backed {
                break;
            }
            backed = span.object_offset + span.byte_len;
            if u64::from(backed) >= u64::from(node.address.offset) + 15 {
                break;
            }
        }
        if u64::from(backed) < u64::from(node.address.offset) + 15 {
            continue;
        }
        let mut fixed = [true; 15];
        let first = relocations.partition_point(|r| {
            r.source.object_number < node.address.object_number
                || (r.source.object_number == node.address.object_number
                    && u64::from(r.source.offset) + u64::from(r.kind.width())
                        <= u64::from(node.address.offset))
        });
        for relocation in &relocations[first..] {
            if relocation.source.object_number != node.address.object_number
                || u64::from(relocation.source.offset) >= u64::from(node.address.offset) + 15
            {
                break;
            }
            for byte in 0..relocation.kind.width() {
                let offset = u64::from(relocation.source.offset) + u64::from(byte);
                if offset >= u64::from(node.address.offset)
                    && offset < u64::from(node.address.offset) + 15
                {
                    fixed[(offset - u64::from(node.address.offset)) as usize] = false;
                }
            }
        }
        if fixed.iter().filter(|&&fixed| fixed).count() < 10 {
            continue;
        }
        windows.push(Window {
            address: node.address,
            bits: node.bitness,
            bytes: source.try_into()?,
            fixed,
        });
    }
    let mut votes = BTreeMap::<(u32, u32), usize>::new();
    let mut comparisons = 0u64;
    let eligible: Vec<_> = profile
        .nodes()
        .iter()
        .filter(|row| {
            row.reason != Reason::CodeConflict
                && windows.iter().any(|window| window.bits == row.bits)
        })
        .collect();
    let stride = eligible.len().div_ceil(1024).max(1);
    for row in eligible.iter().step_by(stride) {
        let mut matched = None;
        let mut ambiguous = false;
        for window in &windows {
            comparisons += 1;
            if comparisons > 100_000_000 {
                return Err("anchor comparison work limit".into());
            }
            if window.bits != row.bits {
                continue;
            }
            if window
                .bytes
                .iter()
                .zip(row.bytes)
                .zip(window.fixed)
                .all(|((&expected, actual), fixed)| !fixed || expected == actual)
            {
                if matched.is_some() {
                    ambiguous = true;
                    break;
                }
                matched = Some(window.address);
            }
        }
        if !ambiguous
            && let Some(address) = matched
            && let Some(base) = row.pc.checked_sub(address.offset)
        {
            *votes.entry((address.object_number, base)).or_default() += 1;
        }
    }
    println!(
        "Read-only snapshot at step{} PC{:#x}; unique masked EXE anchors; candidates require full live validation",
        session.steps(),
        session.debug_pc()
    );
    let mut code_bases = BTreeMap::new();
    let mut historical_bases = BTreeMap::new();
    for object in image.objects() {
        let mut candidates: Vec<_> = votes
            .iter()
            .filter(|((number, _), _)| *number == object.object_number())
            .map(|(&(number, base), &count)| (count, number, base))
            .collect();
        candidates.sort_unstable_by_key(|&(count, _, base)| (std::cmp::Reverse(count), base));
        for &(count, number, base) in candidates.iter().take(3) {
            println!("object{number} anchor candidatebase{base:#x}: {count} unique windows");
        }
        if let Some(&(count, number, base)) = candidates.first()
            && count >= 2
        {
            historical_bases.insert(number, base);
        }
        if candidates.len() == 1 && candidates[0].0 >= 2 {
            code_bases.insert(object.object_number(), candidates[0].2);
        }
    }
    // Profile field evidence belongs to its historical load layout. Never use
    // such addresses to read the current snapshot, which may have another base.
    let mut historical_targets = BTreeMap::<(u32, u32), usize>::new();
    for (&number, &base) in &historical_bases {
        for row in profile.nodes() {
            let Some(offset) = row.pc.checked_sub(base) else {
                continue;
            };
            let address = ObjectAddress {
                object_number: number,
                offset,
            };
            let Ok(index) = windows.binary_search_by_key(&address, |window| window.address) else {
                continue;
            };
            let window = &windows[index];
            if window.bits != row.bits
                || !window
                    .bytes
                    .iter()
                    .zip(row.bytes)
                    .zip(window.fixed)
                    .all(|((&expected, actual), fixed)| !fixed || expected == actual)
            {
                continue;
            }
            let first = relocations.partition_point(|r| r.source < address);
            for relocation in &relocations[first..] {
                if relocation.source.object_number != number
                    || u64::from(relocation.source.offset) >= u64::from(offset) + 15
                {
                    break;
                }
                if !matches!(
                    relocation.kind,
                    LeFixupSourceType::Offset32 | LeFixupSourceType::SelfRelative32
                ) {
                    continue;
                }
                let at = (relocation.source.offset - offset) as usize;
                let Some(field) = row.bytes.get(at..at + 4) else {
                    continue;
                };
                let value = u32::from_le_bytes(field.try_into()?);
                let Some(addend) = relocation.target.offset else {
                    continue;
                };
                let target = if relocation.kind == LeFixupSourceType::Offset32 {
                    value.wrapping_sub(addend)
                } else {
                    value
                        .wrapping_add(base)
                        .wrapping_add(relocation.source.offset)
                        .wrapping_add(4)
                        .wrapping_sub(addend)
                };
                *historical_targets
                    .entry((relocation.target.object_number, target))
                    .or_default() += 1;
            }
        }
    }
    for object in image.objects() {
        let mut candidates: Vec<_> = historical_targets
            .iter()
            .filter(|((number, _), _)| *number == object.object_number())
            .map(|(&(number, base), &count)| (count, number, base))
            .collect();
        candidates.sort_unstable_by_key(|&(count, _, base)| (std::cmp::Reverse(count), base));
        for &(count, number, base) in candidates.iter().take(3) {
            println!(
                "historical profile object{number} fixup candidatebase{base:#x}: {count} observed fields"
            );
        }
    }
    let mut target_votes = BTreeMap::<(u32, u32), usize>::new();
    for relocation in image.relocations() {
        let Some(&base) = code_bases.get(&relocation.source.object_number) else {
            continue;
        };
        let Some(addend) = relocation.target.offset else {
            continue;
        };
        let mut bytes = [0; 4];
        if !matches!(
            relocation.kind,
            LeFixupSourceType::Offset32 | LeFixupSourceType::SelfRelative32
        ) || !session.debug_read_memory(base.wrapping_add(relocation.source.offset), &mut bytes)
        {
            continue;
        }
        let value = u32::from_le_bytes(bytes);
        let target = if relocation.kind == LeFixupSourceType::Offset32 {
            value.wrapping_sub(addend)
        } else {
            value
                .wrapping_add(base)
                .wrapping_add(relocation.source.offset)
                .wrapping_add(4)
                .wrapping_sub(addend)
        };
        *target_votes
            .entry((relocation.target.object_number, target))
            .or_default() += 1;
    }
    for object in image.objects() {
        let mut candidates: Vec<_> = target_votes
            .iter()
            .filter(|((number, _), _)| *number == object.object_number())
            .map(|(&(number, base), &count)| (count, number, base))
            .collect();
        candidates.sort_unstable_by_key(|&(count, _, base)| (std::cmp::Reverse(count), base));
        for &(count, number, base) in candidates.iter().take(3) {
            println!("object{number} fixup candidatebase{base:#x}: {count} fields");
        }
    }
    for segment in SegmentRegister::ALL {
        println!(
            "{segment:?}: {:?}",
            session.registers().segment_cache(segment)
        );
    }
    let control = &session.debug_cpu().control;
    for (table, ti) in [(Some(control.gdtr), 0u16), (control.ldtr, 4)] {
        let Some(table) = table else { continue };
        for index in 0..(u32::from(table.limit) + 1) / 8 {
            let mut raw = [0; 8];
            if !session.debug_read_memory(table.base.wrapping_add(index * 8), &mut raw) {
                continue;
            }
            let base = u32::from(raw[2])
                | (u32::from(raw[3]) << 8)
                | (u32::from(raw[4]) << 16)
                | (u32::from(raw[7]) << 24);
            if raw[5] & 0x80 == 0
                || raw[6] & 0x40 != 0
                || !target_votes.keys().any(|&(_, candidate)| candidate == base)
            {
                continue;
            }
            let mut limit =
                u32::from(raw[0]) | (u32::from(raw[1]) << 8) | (u32::from(raw[6] & 15) << 16);
            if raw[6] & 0x80 != 0 {
                limit = (limit << 12) | 0xfff;
            }
            println!(
                "narrow descriptor candidate selector{:#x}: base{base:#x} limit{limit:#x} access{:#x}",
                (index as u16 * 8) | ti,
                raw[5]
            );
        }
    }
    println!("No state advances, bindings published or guest bytes modified.");
    Ok(())
}
