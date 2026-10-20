//! Bounded read-only native profile census; code byte windows are not printed.
use doslet_aot::profile::{Profile, ProfileLimits, Reason, Tier};
use std::{env, error::Error, fs::File, io::Read};

fn main() -> Result<(), Box<dyn Error>> {
    let mut paths = env::args_os().skip(1).peekable();
    if paths.peek().is_none() {
        return Err("usage: inspect-profile JSON [JSON ...]".into());
    }
    for path in paths {
        let limits = ProfileLimits::default();
        let mut bytes = Vec::new();
        File::open(&path)?
            .take(limits.max_input_bytes as u64 + 1)
            .read_to_end(&mut bytes)?;
        let profile = Profile::parse(&bytes, limits)?;
        let mut tiers = [0usize; 4];
        let mut bits = [0usize; 2];
        let mut conflicts = 0;
        for node in profile.nodes() {
            tiers[match node.tier {
                Tier::Hot => 0,
                Tier::Cold => 1,
                Tier::Grey => 2,
                Tier::Irq => 3,
            }] += 1;
            bits[usize::from(node.bits == doslet_aot::DefaultBitness::Bits32)] += 1;
            conflicts += usize::from(node.reason == Reason::CodeConflict);
        }
        println!(
            "{}: {} bytes; {} rows; HOT/COLD/GREY/IRQ={tiers:?}; code16/code32={bits:?}; conflicts={conflicts}; identity={:?}; rates={:?}; learning={:?}",
            std::path::Path::new(&path).display(),
            bytes.len(),
            profile.nodes().len(),
            profile.identity(),
            profile.rates,
            profile.learning
        );
    }
    Ok(())
}
