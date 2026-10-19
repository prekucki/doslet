//! Reconstruct a local image without relocating, loading or executing it.
use std::{env, error::Error, fs::File, io::Read};

use doslet_aot::{ImageLimits, SymbolicImage};
use doslet_executable::{LeImage, LeLimits};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let path = arguments.next().ok_or("usage: inspect-image EXE")?;
    if arguments.next().is_some() {
        return Err("usage: inspect-image EXE".into());
    }
    let limits = LeLimits::default();
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limits.max_file_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    let le = LeImage::discover(&bytes, limits)?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    let virtual_bytes: u64 = image
        .objects()
        .iter()
        .map(|object| u64::from(object.metadata().virtual_size))
        .sum();
    let file_backed_bytes: u64 = image
        .objects()
        .iter()
        .flat_map(|object| object.file_backed_spans())
        .map(|span| u64::from(span.byte_len))
        .sum();
    println!(
        "objects: {}; virtual bytes: {virtual_bytes}; file-backed bytes: {file_backed_bytes}; canonical relocations: {}",
        image.objects().len(),
        image.relocations().len()
    );
    Ok(())
}
