extern crate std;

use super::*;
use std::vec::Vec;

#[path = "../../../../../tests/fixtures/djgpp_coff.rs"]
mod fixture;
use fixture::{STUB_BYTES, TEXT_VADDR, data_vaddr, djgpp_exe};

const TEXT: [u8; 6] = [0x55, 0x89, 0xe5, 0x31, 0xc0, 0xc3];

fn sample() -> Vec<u8> {
    djgpp_exe(
        &TEXT,
        &[1, 2, 3, 4, 5],
        0x100,
        3,
        &[
            (b"start", TEXT_VADDR, 1, 0x20, 2),
            (b"_a_long_function_name", TEXT_VADDR + 3, 1, 0x24, 3),
            (b"_value", data_vaddr(TEXT.len()), 2, 4, 2),
        ],
    )
}

fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[test]
fn stub_and_sections_borrow_the_file() {
    let bytes = sample();
    let image = CoffImage::discover(&bytes, CoffLimits::default()).unwrap();
    let header = image.header();
    assert_eq!(header.offset, STUB_BYTES);
    assert_eq!(header.section_count, 3);
    assert_eq!(header.entry, TEXT_VADDR + 3);
    assert_eq!(header.text_start, TEXT_VADDR);
    assert_eq!(header.data_start, data_vaddr(TEXT.len()));
    assert_eq!(
        (header.text_size, header.data_size, header.bss_size),
        (6, 5, 0x100)
    );
    let text = image.text();
    assert_eq!(text.name(), b".text");
    assert_eq!(text.virtual_address, TEXT_VADDR);
    assert_eq!(text.data, TEXT);
    // Section offsets are relative to the COFF header.
    assert_eq!(text.file_offset, Some(STUB_BYTES + 0xa8));
    assert_eq!(text.data.as_ptr(), bytes[STUB_BYTES + 0xa8..].as_ptr());
    let sections: Vec<_> = image.sections().collect();
    assert!(sections[1].is_data() && sections[1].data == [1, 2, 3, 4, 5]);
    assert!(sections[2].is_bss() && sections[2].data.is_empty());
    assert_eq!(sections[2].file_offset, None);
    assert_eq!(sections[2].size, 0x100);
}

#[test]
fn symbols_resolve_short_and_long_names_and_skip_auxiliary_entries() {
    let bytes = sample();
    let image = CoffImage::discover(&bytes, CoffLimits::default()).unwrap();
    assert_eq!(image.header().symbol_count, 6);
    let symbols: Vec<_> = image.symbols().collect();
    assert_eq!(symbols.len(), 3);
    assert_eq!(symbols[0].name, b"start");
    assert_eq!(symbols[1].name, b"_a_long_function_name");
    assert_eq!(symbols[1].value, TEXT_VADDR + 3);
    assert!(symbols[1].is_function() && symbols[1].is_defined_global_or_static());
    assert!(!symbols[2].is_function());
    assert_eq!(symbols[2].section, 2);
}

#[test]
fn raw_coff_and_stripped_files_parse() {
    let bytes = djgpp_exe(&TEXT, &[], 0, 0, &[]);
    let raw = &bytes[STUB_BYTES..];
    let image = CoffImage::discover(raw, CoffLimits::default()).unwrap();
    assert_eq!(image.header().offset, 0);
    assert_eq!(image.header().symbols_offset, None);
    assert_eq!(image.symbols().count(), 0);
    assert_eq!(image.text().data, TEXT);
}

#[test]
fn other_executables_are_not_coff() {
    // A plain MZ program and an LE-bound one have no COFF after the stub.
    let mut mz = [0u8; 128];
    mz[..2].copy_from_slice(b"MZ");
    mz[2] = 128;
    mz[4] = 1;
    mz[8] = 4;
    assert_eq!(
        CoffImage::discover(&mz, CoffLimits::default()).unwrap_err(),
        CoffError::NotFound
    );
    let mut le = mz;
    le[60] = 64;
    le[64..66].copy_from_slice(b"LE");
    assert_eq!(
        CoffImage::discover(&le, CoffLimits::default()).unwrap_err(),
        CoffError::NotFound
    );
    assert!(matches!(
        CoffImage::discover(b"PK", CoffLimits::default()),
        Err(CoffError::Mz(_))
    ));
}

#[test]
fn malformed_headers_are_rejected_before_use() {
    let good = sample();
    let coff = STUB_BYTES;
    let check = |edit: &dyn Fn(&mut Vec<u8>), expected: CoffError| {
        let mut bytes = good.clone();
        edit(&mut bytes);
        assert_eq!(
            CoffImage::discover(&bytes, CoffLimits::default()).unwrap_err(),
            expected
        );
    };
    check(
        &|b| b[coff + 18] &= !(F_EXEC as u8),
        CoffError::Invalid("non-executable object file"),
    );
    check(
        &|b| b[coff + 20] = 0x07,
        CoffError::UnsupportedOptionalHeader(0x0107),
    );
    check(
        &|b| b[coff + 16] = 30,
        CoffError::Invalid("optional header size"),
    );
    // Text data beyond the end of the file.
    check(
        &|b| put32(b, coff + 48 + 16, 0x10_0000),
        CoffError::Truncated("section data"),
    );
    // Data placed over the text section.
    check(
        &|b| put32(b, coff + 88 + 12, TEXT_VADDR + 2),
        CoffError::Invalid("overlapping sections"),
    );
    check(
        &|b| put32(b, coff + 20 + 16, TEXT_VADDR + 6),
        CoffError::Invalid("entry outside the text section"),
    );
    // A second text section.
    check(
        &|b| put32(b, coff + 88 + 36, STYP_TEXT),
        CoffError::Invalid("text section count"),
    );
    check(&|b| b[coff + 2] = 0, CoffError::Invalid("section count"));
    let limits = CoffLimits {
        max_sections: 2,
        ..CoffLimits::default()
    };
    assert_eq!(
        CoffImage::discover(&good, limits).unwrap_err(),
        CoffError::Limit(CoffLimit::Sections)
    );
    let limits = CoffLimits {
        max_virtual_bytes: u64::from(TEXT_VADDR),
        ..CoffLimits::default()
    };
    assert_eq!(
        CoffImage::discover(&good, limits).unwrap_err(),
        CoffError::Limit(CoffLimit::VirtualBytes)
    );
    // Truncation inside the section table.
    assert_eq!(
        CoffImage::discover(&good[..coff + 100], CoffLimits::default()).unwrap_err(),
        CoffError::Truncated("section table")
    );
}
