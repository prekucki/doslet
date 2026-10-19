//! Inspect a local EXE without loading, relocating or executing it.
use std::{env, error::Error, fs::File, io::Read};

use doslet_executable::{LeFixupLimits, LeImage, LeLimits, LePageData};

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os().skip(1);
    let path = arguments.next().ok_or("usage: inspect-le EXE")?;
    if arguments.next().is_some() {
        return Err("usage: inspect-le EXE".into());
    }
    let limits = LeLimits::default();
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limits.max_file_bytes as u64 + 1)
        .read_to_end(&mut bytes)?;
    let image = LeImage::discover(&bytes, limits)?;
    let header = image.header();
    println!(
        "LE: container {:#x}, header {:#x}, data {:#x}, page size {}",
        header.location.container_offset,
        header.location.header_offset,
        header.data_pages_offset,
        header.page_size
    );
    println!("entry: {:?}; stack: {:?}", header.entry, header.stack);
    for (index, object) in image.objects().enumerate() {
        println!(
            "object {}: base {:#x}, virtual {:#x}, flags {:#x}, {} pages from {}",
            index + 1,
            object.preferred_base,
            object.virtual_size,
            object.flags,
            object.page_count,
            object.first_page_number
        );
    }
    let mut enumerated = 0;
    let mut zero = 0;
    let mut invalid = 0;
    let mut fixup_bytes = 0;
    let mut fixup_records = 0;
    let mut fixup_sources = 0;
    let mut source_types = [0usize; 9];
    let mut cross_page_sources = 0;
    let mut source_lists = 0;
    let mut aliases = 0;
    let mut remaining_steps = LeFixupLimits::default().max_decode_steps;
    for (page_index, page) in image.pages().enumerate() {
        match page.data {
            LePageData::Enumerated(_) => enumerated += 1,
            LePageData::ZeroFill => zero += 1,
            LePageData::Invalid => invalid += 1,
        }
        fixup_bytes += page.fixup_records.len();
        let mut records = page.fixups(
            header.page_size,
            LeFixupLimits {
                max_decode_steps: remaining_steps,
            },
        )?;
        for result in records.by_ref() {
            let record = result.map_err(|error| {
                format!(
                    "page {} (physical {}): {error}",
                    page_index + 1,
                    page.physical_page_number
                )
            })?;
            fixup_records += 1;
            fixup_sources += record.sources().len();
            source_types[record.source_type() as usize] += 1;
            source_lists += usize::from(record.is_source_list());
            aliases += usize::from(record.is_alias());
            cross_page_sources += record.sources().iter().filter(|offset| *offset < 0).count();
        }
        remaining_steps = records.remaining_decode_steps();
    }
    println!("pages: {enumerated} enumerated, {zero} zero-fill, {invalid} invalid");
    println!(
        "fixups: {fixup_bytes} bytes, {fixup_records} direct internal records, {fixup_sources} sources"
    );
    println!(
        "source types [0..8]: {source_types:?}; lists {source_lists}, aliases {aliases}, negative sources {cross_page_sources}"
    );
    println!("Inspection only; no relocation, loading or execution.");
    Ok(())
}
