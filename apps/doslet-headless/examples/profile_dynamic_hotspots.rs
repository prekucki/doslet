//! Bounded guest-PC sampling by execution phase, with sparse short edge traces.
//! This is a workload profile, not a host-time benchmark or a full x86 decoder.
//! Only addresses, coarse operation families and counts are printed. Guest code
//! bytes stay in borrowed RAM; original game files are never modified.
#[path = "../../common/native_input.rs"]
mod native_input;

use doslet_runtime::{Session, SessionStatus};
use doslet_x86::{Access, SegmentRegister};
use std::{collections::BTreeMap, error::Error, path::Path};

const WINDOW: u64 = 100_000_000;
const MAX_STEPS: u64 = 1_000_000_000;
const CAPACITY: usize = 16_384;
const TRACE_STEPS: u32 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Pc {
    cs: u16,
    ip: u32,
    linear: u32,
    default32: bool,
    family: &'static str,
}

fn observe(session: &Session) -> Option<Pc> {
    let r = session.registers();
    // One byte suffices for the coarse family. Do not copy a guest code range
    // or mistake data beyond a segment/A20 boundary for instruction bytes.
    let mut linear = r
        .address(SegmentRegister::Cs, r.ip, 1, Access::Execute)
        .ok()?;
    if !session.a20_enabled() {
        linear &= !(1 << 20);
    }
    let byte = *session
        .ram_view(linear as usize..linear as usize + 1)?
        .first()?;
    let family = match byte {
        0x00..=0x05
        | 0x08..=0x0D
        | 0x10..=0x15
        | 0x18..=0x1D
        | 0x20..=0x25
        | 0x28..=0x2D
        | 0x30..=0x35
        | 0x38..=0x3D
        | 0x80..=0x85
        | 0xA8..=0xA9 => "alu",
        0x40..=0x4F => "inc-dec",
        0x50..=0x61 | 0x68 | 0x6A => "stack",
        0x70..=0x7F | 0xE0..=0xE3 => "conditional-branch",
        0x86..=0x87 | 0x90..=0x97 => "exchange",
        0x88..=0x8B | 0xA0..=0xA3 | 0xB0..=0xBF | 0xC6..=0xC7 => "move",
        0x8D => "lea",
        0xA4..=0xA7 | 0xAA..=0xAF => "string",
        0xC0..=0xC1 | 0xD0..=0xD3 => "shift",
        0xC2..=0xC3 | 0xCA..=0xCB => "return",
        0xD8..=0xDF => "x87",
        0xE4..=0xE7 | 0xEC..=0xEF => "port-io",
        0xE8 => "direct-call",
        0xE9..=0xEB => "jump",
        0xF6..=0xF7 => "test-unary-mul-div-group",
        0xFE..=0xFF => "inc-dec-call-jump-push-group",
        0x26 | 0x2E | 0x36 | 0x3E | 0x64..=0x67 | 0xF0 | 0xF2..=0xF3 => "prefixed",
        0x0F => "extended",
        _ => "other",
    };
    Some(Pc {
        cs: r.cs,
        ip: r.ip,
        linear,
        default32: r.segment_cache(SegmentRegister::Cs).default32,
        family,
    })
}

fn count<K: Ord>(table: &mut BTreeMap<K, u64>, key: K, dropped: &mut u64) {
    if let Some(count) = table.get_mut(&key) {
        *count += 1;
    } else if table.len() < CAPACITY {
        table.insert(key, 1);
    } else {
        *dropped += 1;
    }
}

fn drain(session: &mut Session) {
    let mut midi = [0; 1024];
    while session.drain_midi(&mut midi) != 0 {}
    let mut pcm = [[0; 2]; 128];
    while session.drain_pcm(&mut pcm) != 0 {}
}

fn advance(session: &mut Session, budget: u32) -> Result<(), Box<dyn Error>> {
    let done = session.advance(budget);
    if done != budget || session.status() != SessionStatus::Running {
        return Err(format!(
            "stopped at {} steps: {:?}, {:?}",
            session.steps(),
            session.status(),
            session.fault()
        )
        .into());
    }
    Ok(())
}

fn print_pc(kind: &str, begin: u64, end: u64, pc: Pc, count: u64, samples: u64) {
    println!(
        "{kind},{begin},{end},{:04X},{:08X},{:08X},{},{},{count},{:.3}",
        pc.cs,
        pc.ip,
        pc.linear,
        u8::from(pc.default32),
        pc.family,
        100.0 * count as f64 / samples.max(1) as f64
    );
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() || args.len() > 3 {
        return Err("usage: profile_dynamic_hotspots EXE [STEPS <= 1000000000] [SEED != 0]".into());
    }
    let steps = args
        .get(1)
        .map_or(Ok(MAX_STEPS), |v| v.to_string_lossy().parse::<u64>())?;
    let mut seed = args
        .get(2)
        .map_or(Ok(0x12345678), |v| v.to_string_lossy().parse::<u32>())?;
    if steps == 0 || steps > MAX_STEPS || seed == 0 {
        return Err("steps must be 1..=1000000000 and seed must be nonzero".into());
    }
    let mut session = native_input::load_game(Path::new(&args[0]))?;
    // The ordinary interpreter exposes every requested boundary. This measures
    // the guest workload without preferentially sampling native block exits.
    session.set_dynamic_core(false);
    session.start();
    let mut edges = BTreeMap::new();
    let mut edge_drops = 0;
    let mut edge_samples = 0_u64;
    let mut trace_unavailable = 0_u64;
    let mut sample_index = 0_u64;
    println!(
        "# Randomized PC frequency by logical steps, not host CPU cost or exact instruction counts"
    );
    println!(
        "# Coarse families inspect one opcode byte; groups and prefixes are intentionally unresolved"
    );
    println!("kind,begin_step,end_step,cs,ip,linear,default32,family,count,percent_of_all_samples");
    while session.steps() < steps {
        let begin = session.steps();
        let end = (begin + WINDOW).min(steps);
        let mut pcs = BTreeMap::new();
        let mut regions = BTreeMap::new();
        let mut dropped = 0;
        let mut region_drops = 0;
        let mut samples = 0_u64;
        let mut unavailable = 0_u64;
        while session.steps() < end {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let interval = u64::from(1000 + seed % 9001).min(end - session.steps());
            advance(&mut session, interval as u32)?;
            samples += 1;
            sample_index += 1;
            if let Some(pc) = observe(&session) {
                count(&mut pcs, pc, &mut dropped);
                count(&mut regions, (pc.cs, pc.linear & !255), &mut region_drops);
            } else {
                unavailable += 1;
            }
            // Sparse bounded traces expose actual successor PCs. IRQ/service
            // entries are possible; these are step edges, not a decoded CFG.
            if sample_index.is_multiple_of(64) {
                for _ in 0..u64::from(TRACE_STEPS).min(end - session.steps()) {
                    let from = observe(&session);
                    advance(&mut session, 1)?;
                    let to = observe(&session);
                    edge_samples += 1;
                    if let (Some(from), Some(to)) = (from, to) {
                        count(&mut edges, (from, to), &mut edge_drops);
                    } else {
                        trace_unavailable += 1;
                    }
                }
            }
            drain(&mut session);
        }
        let mut ranked: Vec<_> = pcs.into_iter().collect();
        ranked.sort_by_key(|(pc, count)| (std::cmp::Reverse(*count), *pc));
        for (pc, count) in ranked.into_iter().take(32) {
            print_pc("pc", begin, end, pc, count, samples);
        }
        let mut ranked: Vec<_> = regions.into_iter().collect();
        ranked.sort_by_key(|(region, count)| (std::cmp::Reverse(*count), *region));
        for ((cs, linear), count) in ranked.into_iter().take(16) {
            println!(
                "# region,{begin},{end},{cs:04X},{linear:08X},{count},{:.3}",
                100.0 * count as f64 / samples.max(1) as f64
            );
        }
        println!(
            "# window_samples,{begin},{end},{samples},unavailable={unavailable},pc_capacity_drops={dropped},region_capacity_drops={region_drops}"
        );
    }
    println!(
        "# Sparse trace edges: counts have a separate denominator and are not execution counts"
    );
    println!(
        "# edge,from_cs,from_ip,from_family,to_cs,to_ip,to_family,count,percent_of_trace_steps"
    );
    let mut ranked: Vec<_> = edges.into_iter().collect();
    ranked.sort_by_key(|(edge, count)| (std::cmp::Reverse(*count), *edge));
    for ((from, to), count) in ranked.into_iter().take(64) {
        println!(
            "# edge,{:04X},{:08X},{},{:04X},{:08X},{},{count},{:.3}",
            from.cs,
            from.ip,
            from.family,
            to.cs,
            to.ip,
            to.family,
            100.0 * count as f64 / edge_samples.max(1) as f64
        );
    }
    println!(
        "# trace_steps={edge_samples},unavailable={trace_unavailable},capacity_drops={edge_drops}"
    );
    println!("# completed_steps={}", session.steps());
    Ok(())
}
