//! Speed-tier labels and memory patches name linear addresses. When the
//! program's LE objects or COFF sections are found at another layout than the
//! one those were recorded at, both follow their objects to the live bases.
use crate::le_layout::{Layout, ObjectSpan, locate, program_image, spans};
use crate::native_speed_tiers::Controller;
use doslet_aot::SymbolicImage;
use doslet_runtime::Session;
use std::{collections::BTreeMap, path::PathBuf};

/// Labels checked to decide whether unplaced labels already match.
const SAMPLE: usize = 2000;

pub fn follow(
    session: &mut Session,
    patches: &[PathBuf],
    tiers: Option<&mut Controller>,
    exe: &[u8],
    layout: &Layout,
) {
    let Ok(image) = program_image(exe, None) else {
        return;
    };
    if !patches.is_empty() {
        follow_patches(session, patches, exe, layout);
    }
    if let Some(tiers) = tiers {
        follow_tiers(tiers, session, &image, layout);
    }
}

/// Pending patches that name their objects move to the live bases.
fn follow_patches(session: &mut Session, paths: &[PathBuf], exe: &[u8], layout: &Layout) {
    let sizes: BTreeMap<_, _> = layout.objects.iter().map(|o| (o.number, o.size)).collect();
    let live: Vec<_> = layout
        .objects
        .iter()
        .map(|o| (o.number, o.base, o.size))
        .collect();
    match crate::native_input::patches::relocate(paths, exe, &sizes, &live) {
        Ok(plan) if plan.as_slice() == session.memory_patches() => {}
        Ok(plan) => match session.relocate_patches(plan) {
            Ok(()) => eprintln!("[INFO doslet] Patches follow the program to its live layout"),
            Err(error) => eprintln!("[WARN doslet] Cannot relocate patches: {error}"),
        },
        Err(error) => eprintln!("[WARN doslet] Cannot relocate patches: {error}"),
    }
}

/// Relocation fields of each object, as sorted `(start, end)` offsets.
struct Fixups(BTreeMap<u32, Vec<(u32, u32)>>);
impl Fixups {
    fn new(image: &SymbolicImage) -> Self {
        let mut fields = BTreeMap::<u32, Vec<(u32, u32)>>::new();
        for r in image.relocations() {
            let start = r.source.offset;
            fields
                .entry(r.source.object_number)
                .or_default()
                .push((start, start + u32::from(r.kind.width())));
        }
        for ranges in fields.values_mut() {
            ranges.sort_unstable();
        }
        Self(fields)
    }
    fn covers(&self, object: u32, offset: u32) -> bool {
        self.0.get(&object).is_some_and(|ranges| {
            let index = ranges.partition_point(|&(start, _)| start <= offset);
            index > 0 && offset < ranges[index - 1].1
        })
    }
}

fn live_code(session: &Session, address: u32) -> Option<([u8; 15], u32)> {
    let mut bytes = [0; 15];
    if !session.debug_read_memory(address, &mut bytes) {
        return None;
    }
    let length = doslet_x86::translation::instruction_length(&bytes, true)?;
    Some((bytes, u32::from(length)))
}

/// Labels follow their objects; only relocated operand bytes may differ
/// from the moved code, which then replaces the recorded bytes.
fn follow_tiers(
    tiers: &mut Controller,
    session: &mut Session,
    image: &SymbolicImage,
    layout: &Layout,
) {
    let live = &layout.objects;
    let fixups = Fixups::new(image);
    let source = match tiers.label_layout() {
        Some(bases) => match spans(image, bases) {
            Ok(source) => source,
            Err(error) => {
                eprintln!("[WARN doslet] Speed tiers name an invalid layout: {error}");
                return;
            }
        },
        None if matches_live(tiers, session, live) => live.clone(),
        None => match infer(image, &fixups, tiers) {
            Some(source) => source,
            None => {
                eprintln!(
                    "[INFO doslet] Speed tiers: learned labels do not match this program's code"
                );
                return;
            }
        },
    };
    if source == *live {
        if tiers.label_layout().is_none() {
            tiers.relocate_labels(session, layout.bases(), |address, code| {
                Some((address, *code))
            });
        }
        return;
    }
    let moves: BTreeMap<u32, Option<(u32, [u8; 15])>> = tiers
        .code_labels()
        .map(|(address, code)| {
            let moved = (|| {
                let Some((number, offset)) = locate(&source, address) else {
                    return Some((address, *code));
                };
                let span = live.iter().find(|span| span.number == number)?;
                let target = span
                    .base
                    .checked_add(offset)
                    .filter(|_| offset < span.size)?;
                let (bytes, length) = live_code(session, target)?;
                (0..length)
                    .all(|i| {
                        code[i as usize] == bytes[i as usize] || fixups.covers(number, offset + i)
                    })
                    .then_some((target, bytes))
            })();
            (address, moved)
        })
        .collect();
    let (moved, dropped) = tiers.relocate_labels(session, layout.bases(), |address, _| {
        moves.get(&address).copied().flatten()
    });
    eprintln!(
        "[INFO doslet] Speed tiers: {moved} labels follow the program to its live layout; {dropped} no longer match"
    );
}

/// Whether most 32-bit labels inside the live objects match the live code.
fn matches_live(tiers: &Controller, session: &Session, live: &[ObjectSpan]) -> bool {
    let (mut inside, mut matching) = (0, 0);
    for (address, code) in tiers.code_labels() {
        if locate(live, address).is_none() {
            continue;
        }
        inside += 1;
        matching +=
            usize::from(live_code(session, address).is_some_and(|(bytes, length)| {
                bytes[..length as usize] == code[..length as usize]
            }));
        if inside == SAMPLE {
            break;
        }
    }
    inside != 0 && matching * 2 >= inside
}

/// The bases labels without a recorded layout were learned at. Each label
/// votes for the base its first four bytes give, where those occur once,
/// relocation-free, in an executable object of the image.
fn infer(image: &SymbolicImage, fixups: &Fixups, tiers: &Controller) -> Option<Vec<ObjectSpan>> {
    let mut index = Vec::new();
    for object in image
        .objects()
        .iter()
        .filter(|o| o.metadata().is_executable())
    {
        let number = object.object_number();
        let bytes = object.bytes();
        for offset in 0..bytes.len().saturating_sub(3) as u32 {
            if (offset..offset + 4).any(|i| fixups.covers(number, i)) {
                continue;
            }
            let at = offset as usize;
            let key = u32::from_le_bytes(bytes[at..at + 4].try_into().expect("window"));
            index.push((key, number, offset));
        }
    }
    index.sort_unstable();
    let mut votes = BTreeMap::<u32, BTreeMap<u32, usize>>::new();
    for (address, code) in tiers.code_labels() {
        let key = u32::from_le_bytes(code[..4].try_into().expect("label code"));
        let start = index.partition_point(|&(k, _, _)| k < key);
        let end = index.partition_point(|&(k, _, _)| k <= key);
        if end != start + 1 {
            continue;
        }
        let (_, number, offset) = index[start];
        if let Some(base) = address.checked_sub(offset) {
            *votes.entry(number).or_default().entry(base).or_default() += 1;
        }
    }
    let mut bases = Vec::new();
    for (number, candidates) in votes {
        let mut ranked: Vec<_> = candidates.into_iter().map(|(b, n)| (n, b)).collect();
        ranked.sort_unstable_by(|a, b| b.cmp(a));
        let (count, base) = ranked[0];
        if count >= 8 && ranked.get(1).is_none_or(|&(next, _)| count > next * 2) {
            bases.push((number, base));
        }
    }
    (!bases.is_empty())
        .then(|| spans(image, &bases).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::Path, sync::Arc, time::Instant};

    /// `cargo test -p doslet --release --bin doslet heroes_patch -- --ignored --nocapture`
    #[test]
    #[ignore = "requires the local Heroes profile and its learned speed tiers"]
    fn heroes_patch_and_tiers_follow_every_dpmi_layout() {
        use clap::Parser;
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        // Tests never touch the user's cache; work on a copy of the labels.
        let cache = Path::new(&std::env::var_os("HOME").unwrap()).join(".cache/doslet/speed-tiers");
        let copy = std::env::temp_dir().join(format!("doslet-layout-tiers-{}", std::process::id()));
        for dpmi in ["off", "reflect", "direct"] {
            std::fs::create_dir_all(&copy).unwrap();
            let name = "0e87d3025f437158dfb9099ca2921c83bac5bf9e4708bad04e4d5fc8297218a2.json";
            std::fs::copy(cache.join(name), copy.join(name)).unwrap();
            let cli = crate::cli::Cli::try_parse_from([
                "doslet",
                "--config",
                root.join("games/HoMnM.toml").to_str().unwrap(),
                "--dpmi",
                dpmi,
                "--no-aot",
            ])
            .unwrap()
            .resolve()
            .unwrap();
            let game = cli.game.clone().unwrap();
            let mut launch = cli.launch;
            launch.persist = false;
            launch.preload_local_media = true;
            let exe: Arc<[u8]> = crate::native_zip::program_bytes(&game, &launch)
                .unwrap()
                .into();
            let mut session = crate::native_zip::load_game_with_settings(&game, &launch).unwrap();
            let mut tiers = Controller::cached_game(&game, Some(&copy))
                .unwrap()
                .unwrap();
            assert!(
                tiers.label_layout().is_none(),
                "legacy labels name no layout"
            );
            tiers.enable(&mut session);
            session.start();
            let mut watcher = crate::le_layout::Watcher::new(exe.clone()).unwrap();
            let mut found = None;
            let mut pcm = [doslet_pcm::EMPTY; 4096];
            let mut midi = [0; 4096];
            let end = 40_000_000_000;
            let started = Instant::now();
            while session.emulated_nanos() < end
                && session.status() == doslet_runtime::SessionStatus::Running
            {
                let target = (session.emulated_nanos() + 10_000_000).min(end);
                while session.emulated_nanos() < target {
                    let done = tiers.advance(&mut session, 10_000, target).unwrap();
                    while session.drain_pcm_timed(&mut pcm) != 0 {}
                    while session.drain_midi(&mut midi) != 0 {}
                    if done == 0 && !tiers.has_pending_time() {
                        break;
                    }
                }
                if let Some(layout) = watcher.poll(&session) {
                    follow(
                        &mut session,
                        &launch.patches,
                        Some(&mut tiers),
                        &exe,
                        &layout,
                    );
                    found = Some(layout);
                }
            }
            let layout = found.expect("live layout");
            let steps = tiers.stats.steps;
            let labelled = 100.0 * (steps[0] + steps[1] + steps[3]) as f64
                / steps.iter().sum::<u64>().max(1) as f64;
            eprintln!(
                "DPMI {dpmi}: objects {:x?}, {:?} host, {:?}, patches {}, labelled {labelled:.1}%",
                layout.bases(),
                started.elapsed(),
                session.status(),
                session.applied_patch_count()
            );
            assert_eq!(session.status(), doslet_runtime::SessionStatus::Running);
            assert_eq!(session.applied_patch_count(), 1);
            assert!(labelled > 80.0, "{labelled}");
            assert_eq!(tiers.label_layout(), Some(layout.bases().as_slice()));
            std::fs::remove_dir_all(&copy).unwrap();
        }
    }
}
