extern crate std;

use super::*;
use std::{vec, vec::Vec};

const RAW: LeLocation = LeLocation {
    container_offset: 0,
    header_offset: 0,
};

fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

// Entirely synthetic two-page module. Its object includes an implicit BSS tail.
fn fixture() -> Vec<u8> {
    let mut bytes = vec![0; 277];
    bytes[..2].copy_from_slice(b"LE");
    put16(&mut bytes, 8, 2);
    put16(&mut bytes, 10, 1);
    for (offset, value) in [
        (20, 2),
        (24, 1),
        (28, 1),
        (32, 1),
        (36, 48),
        (40, 16),
        (44, 5),
        (48, 15),
        (56, 32),
        (64, 196),
        (68, 1),
        (72, 220),
        (104, 228),
        (108, 240),
        (112, 243),
        (120, 243),
        (128, 256),
        (196, 48),
        (200, 0x10000),
        (204, 0x2007),
        (208, 1),
        (212, 2),
        (228, 0),
        (232, 1),
        (236, 3),
    ] {
        put32(&mut bytes, offset, value);
    }
    bytes[220..228].copy_from_slice(&[0, 0, 1, 0, 0, 0, 2, 0]);
    bytes[240..243].copy_from_slice(&[0x11, 0x22, 0x33]);
    for (index, byte) in bytes[256..].iter_mut().enumerate() {
        *byte = (index as u8).wrapping_mul(17);
    }
    bytes
}

fn parse(bytes: &[u8]) -> Result<LeImage<'_>, LeError> {
    LeImage::parse_at(bytes, RAW, LeLimits::default())
}

fn mz(bytes: &mut [u8], origin: usize, legacy: bool, declared: Option<usize>) {
    let header = &mut bytes[origin..origin + 64];
    header.fill(0);
    header[..2].copy_from_slice(b"MZ");
    put16(header, 2, 64);
    put16(header, 4, 1);
    put16(header, 8, 4);
    if legacy {
        put16(header, 6, 9);
        put16(header, 24, 28);
        put32(header, 60, 0xffff_ffff); // A relocation, not e_lfanew.
    } else if let Some(offset) = declared {
        put32(header, 60, offset as u32);
    }
}

fn append_image(file: &mut Vec<u8>, origin: usize, header: usize) {
    let mut raw = fixture();
    put32(&mut raw, 128, (header + 256 - origin) as u32);
    if file.len() < header + raw.len() {
        file.resize(header + raw.len(), 0);
    }
    file[header..header + raw.len()].copy_from_slice(&raw);
}

#[test]
fn borrowed_views_and_checked_indices() {
    let bytes = fixture();
    let image = parse(&bytes).unwrap();
    assert_eq!(image.header().entry.unwrap().offset, 1);
    assert_eq!(image.header().stack.unwrap().offset, 48);
    assert_eq!(image.objects().len(), 1);
    assert_eq!(image.pages().len(), 2);
    assert_eq!(image.resources().len(), 0);
    assert_eq!(image.bytes().as_ptr(), bytes.as_ptr());
    assert_eq!(image.loader_section().as_ptr(), bytes[196..].as_ptr());
    assert_eq!(image.fixup_section().as_ptr(), bytes[228..].as_ptr());
    let first = image.page(0).unwrap();
    assert_eq!(first.fixup_records, [0x11]);
    assert_eq!(first.fixup_records.as_ptr(), bytes[240..].as_ptr());
    assert_eq!(image.page(1).unwrap().fixup_records, [0x22, 0x33]);
    assert_eq!(
        image.page(1).unwrap().data,
        LePageData::Enumerated(&bytes[272..277])
    );
    assert_eq!(image.object(usize::MAX), None);
    assert_eq!(image.page(usize::MAX), None);
    assert_eq!(image.resource(usize::MAX), None);
}

#[test]
fn reordered_pages_keep_their_physical_data_and_fixup_records() {
    let mut bytes = fixture();
    bytes[220..228].copy_from_slice(&[0, 0, 2, 0, 0, 0, 1, 0]);
    let image = parse(&bytes).unwrap();
    let first = image.page(0).unwrap();
    assert_eq!(first.physical_page_number, 2);
    assert_eq!(first.data, LePageData::Enumerated(&bytes[272..277]));
    assert_eq!(first.fixup_records, [0x22, 0x33]);
    let second = image.page(1).unwrap();
    assert_eq!(second.physical_page_number, 1);
    assert_eq!(second.data, LePageData::Enumerated(&bytes[256..272]));
    assert_eq!(second.fixup_records, [0x11]);
}

#[test]
fn every_truncated_prefix_fails_without_panicking() {
    let bytes = fixture();
    for length in 0..bytes.len() {
        assert!(parse(&bytes[..length]).is_err(), "accepted prefix {length}");
    }
    assert!(matches!(
        LeImage::parse_at(
            &bytes,
            LeLocation {
                container_offset: 0,
                header_offset: usize::MAX
            },
            LeLimits::default()
        ),
        Err(LeError::Invalid(_))
    ));
}

#[test]
fn bound_nested_mz_defines_absolute_data_origin() {
    let mut bytes = vec![0; 128];
    mz(&mut bytes, 0, true, None);
    mz(&mut bytes, 64, false, Some(64));
    append_image(&mut bytes, 64, 128);
    let image = LeImage::discover(&bytes, LeLimits::default()).unwrap();
    assert_eq!(
        image.header().location,
        LeLocation {
            container_offset: 64,
            header_offset: 128
        }
    );
    assert_eq!(image.header().data_pages_offset, 384);
    assert_eq!(
        image.page(0).unwrap().data,
        LePageData::Enumerated(&bytes[384..400])
    );
    assert!(
        LeImage::parse_at(
            &bytes,
            LeLocation {
                container_offset: 0,
                header_offset: 128
            },
            LeLimits::default()
        )
        .is_err()
    );
    assert!(MzImage::parse(&bytes).unwrap().extended.is_none());
    assert_eq!(MzImage::parse(&bytes).unwrap().relocations().len(), 9);
}

#[test]
fn incidental_magic_does_not_hide_valid_old_mz_bound_image() {
    let mut bytes = vec![0; 128];
    mz(&mut bytes, 0, true, None);
    bytes[70..72].copy_from_slice(b"LE");
    bytes[75..77].copy_from_slice(b"LX");
    append_image(&mut bytes, 0, 128);
    assert_eq!(
        LeImage::discover(&bytes, LeLimits::default())
            .unwrap()
            .header()
            .location
            .header_offset,
        128
    );
}

#[test]
fn two_valid_images_are_ambiguous() {
    let mut bytes = vec![0; 128];
    mz(&mut bytes, 0, true, None);
    append_image(&mut bytes, 0, 128);
    append_image(&mut bytes, 0, 512);
    assert!(matches!(
        LeImage::discover(&bytes, LeLimits::default()),
        Err(LeError::Ambiguous {
            first: 128,
            second: 512
        })
    ));
}

#[test]
fn declarations_of_one_header_keep_only_the_valid_data_origin() {
    let mut bytes = vec![0; 128];
    mz(&mut bytes, 0, false, Some(128));
    mz(&mut bytes, 64, false, Some(64));
    append_image(&mut bytes, 0, 128);
    // Both validated MZ headers declare the same absolute LE header. Only the
    // outer origin resolves the data within this file; the nested origin fails.
    assert_eq!(
        LeImage::parse_at(
            &bytes,
            LeLocation {
                container_offset: 64,
                header_offset: 128,
            },
            LeLimits::default(),
        )
        .unwrap_err(),
        LeError::Invalid("data pages offset")
    );
    assert_eq!(
        LeImage::discover(&bytes, LeLimits::default())
            .unwrap()
            .header()
            .location,
        LeLocation {
            container_offset: 0,
            header_offset: 128,
        }
    );
}

#[test]
fn one_header_with_two_valid_data_origins_is_ambiguous() {
    let mut bytes = vec![0; 128];
    mz(&mut bytes, 0, false, Some(128));
    mz(&mut bytes, 64, false, Some(64));
    append_image(&mut bytes, 0, 128);
    bytes.resize(bytes.len() + 64, 0);
    for origin in [0, 64] {
        let image = LeImage::parse_at(
            &bytes,
            LeLocation {
                container_offset: origin,
                header_offset: 128,
            },
            LeLimits::default(),
        )
        .unwrap();
        assert_eq!(image.header().data_pages_offset, 384 + origin);
        assert_eq!(
            image.page(0).unwrap().data,
            LePageData::Enumerated(&bytes[384 + origin..400 + origin])
        );
    }
    assert_ne!(&bytes[384..400], &bytes[448..464]);
    assert_eq!(
        LeImage::discover(&bytes, LeLimits::default()).unwrap_err(),
        LeError::Ambiguous {
            first: 128,
            second: 128,
        }
    );
}

#[test]
fn declared_header_is_not_counted_twice_by_discovery() {
    let mut bytes = vec![0; 128];
    mz(&mut bytes, 0, false, Some(128));
    append_image(&mut bytes, 0, 128);
    let mut work = Work(LeLimits::default().max_validation_steps);
    LeImage::parse(
        &bytes,
        LeLocation {
            container_offset: 0,
            header_offset: 128,
        },
        LeLimits::default(),
        &mut work,
    )
    .unwrap();
    let limits = LeLimits {
        max_validation_steps: LeLimits::default().max_validation_steps - work.0,
        ..LeLimits::default()
    };
    // A budget sufficient for precisely one validation also proves that the
    // declaration and the signature scan do not revalidate the same location.
    assert!(LeImage::discover(&bytes, limits).is_ok());
}

#[test]
fn sixty_four_declared_containers_are_allowed_but_sixty_five_exceed_limit() {
    for count in [MAX_CONTAINERS, MAX_CONTAINERS + 1] {
        let header = count * 64;
        let mut bytes = vec![0; header];
        for origin in (0..header).step_by(64) {
            mz(&mut bytes, origin, false, Some(header - origin));
        }
        append_image(&mut bytes, 0, header);
        if count == MAX_CONTAINERS {
            assert_eq!(
                LeImage::discover(&bytes, LeLimits::default())
                    .unwrap()
                    .header()
                    .location,
                LeLocation {
                    container_offset: 0,
                    header_offset: header,
                }
            );
        } else {
            assert_eq!(
                LeImage::discover(&bytes, LeLimits::default()).unwrap_err(),
                LeError::Limit(LeLimit::Containers)
            );
        }
    }
}

#[test]
fn shared_validation_budget_exhaustion_prevents_a_uniqueness_claim() {
    let mut bytes = vec![0; 128];
    mz(&mut bytes, 0, true, None);
    append_image(&mut bytes, 0, 128);
    let mut work = Work(LeLimits::default().max_validation_steps);
    LeImage::parse(
        &bytes,
        LeLocation {
            container_offset: 0,
            header_offset: 128,
        },
        LeLimits::default(),
        &mut work,
    )
    .unwrap();
    let limits = LeLimits {
        max_validation_steps: LeLimits::default().max_validation_steps - work.0,
        ..LeLimits::default()
    };
    assert!(LeImage::discover(&bytes, limits).is_ok());
    // The first valid image consumes the budget. A later truncated candidate
    // must still be checked before discovery can establish uniqueness.
    bytes.resize(514, 0);
    bytes[512..].copy_from_slice(b"LE");
    assert_eq!(
        LeImage::discover(&bytes, limits).unwrap_err(),
        LeError::Limit(LeLimit::ValidationSteps)
    );
    assert_eq!(
        LeImage::discover(
            &bytes,
            LeLimits {
                max_validation_steps: limits.max_validation_steps + 1,
                ..limits
            },
        )
        .unwrap()
        .header()
        .location
        .header_offset,
        128
    );
}

#[test]
fn malformed_declared_image_does_not_hide_a_valid_candidate() {
    let mut bytes = vec![0; 128];
    mz(&mut bytes, 0, false, Some(64));
    bytes[64..66].copy_from_slice(b"LE");
    bytes[66] = 1;
    append_image(&mut bytes, 0, 128);
    assert_eq!(
        LeImage::discover(&bytes, LeLimits::default())
            .unwrap()
            .header()
            .location
            .header_offset,
        128
    );
}

#[test]
fn declared_lx_is_explicitly_unsupported() {
    let mut bytes = vec![0; 128];
    mz(&mut bytes, 0, false, Some(64));
    bytes[64..66].copy_from_slice(b"LX");
    assert_eq!(
        LeImage::discover(&bytes, LeLimits::default()).unwrap_err(),
        LeError::UnsupportedLx
    );
    assert_eq!(
        LeImage::parse_at(b"LX", RAW, LeLimits::default()).unwrap_err(),
        LeError::UnsupportedLx
    );
}

#[test]
fn unsupported_orders_levels_cpus_and_page_encodings_are_precise() {
    for (field, value, error) in [
        (2, 1, LeError::UnsupportedByteOrder),
        (3, 1, LeError::UnsupportedByteOrder),
        (4, 1, LeError::UnsupportedLevel(1)),
        (8, 1, LeError::UnsupportedCpu(1)),
        (223, 1, LeError::UnsupportedPageEncoding(1)),
        (223, 2, LeError::UnsupportedPageEncoding(2)),
        (223, 3, LeError::UnsupportedPageEncoding(3)),
        (223, 4, LeError::UnsupportedPageEncoding(4)),
        (223, 255, LeError::UnsupportedPageEncoding(255)),
    ] {
        let mut bytes = fixture();
        bytes[field] = value;
        assert_eq!(parse(&bytes).unwrap_err(), error);
    }
}

#[test]
fn zero_and_invalid_page_encodings_are_unsupported_with_any_page_number() {
    for encoding in [2, 3] {
        for number in [0, 1, 2] {
            let mut bytes = fixture();
            bytes[224..228].copy_from_slice(&[0, 0, number, encoding]);
            assert_eq!(
                parse(&bytes).unwrap_err(),
                LeError::UnsupportedPageEncoding(encoding)
            );
        }
    }
}

#[test]
fn entry_requires_backed_executable_byte_stack_allows_one_past_end() {
    for (field, value, error) in [
        (24, 2, "object reference"),
        (28, 21, "entry point has no file-backed byte"),
        (28, 32, "entry point has no file-backed page"),
        (28, 48, "entry point"),
        (36, 49, "stack pointer"),
        (204, 0x2003, "entry point"),
    ] {
        let mut bytes = fixture();
        put32(&mut bytes, field, value);
        assert_eq!(
            parse(&bytes).unwrap_err(),
            LeError::Invalid(error),
            "field {field} value {value}"
        );
    }
    let mut bytes = fixture();
    put32(&mut bytes, 28, 20);
    assert!(parse(&bytes).is_ok());
    put32(&mut bytes, 204, 0x2005);
    assert_eq!(
        parse(&bytes).unwrap_err(),
        LeError::Invalid("stack pointer")
    );
}

#[test]
fn malformed_tables_counts_and_checked_offsets_are_rejected() {
    for (field, value) in [
        (40, 0),
        (40, 17),
        (44, 17),
        (64, u32::MAX),
        (68, 0),
        (68, u32::MAX),
        (72, u32::MAX),
        (72, 196),
        (84, u32::MAX),
        (104, u32::MAX),
        (108, 228),
        (108, u32::MAX),
        (112, 228),
        (128, u32::MAX),
        (132, 3),
        (148, 2),
        (196, 16),
        (200, u32::MAX),
        (208, 0),
        (208, u32::MAX),
        (212, 3),
        (216, 1),
        (228, 1),
        (232, 4),
        (236, 0),
        (236, u32::MAX),
    ] {
        let mut bytes = fixture();
        put32(&mut bytes, field, value);
        assert!(parse(&bytes).is_err(), "field {field} value {value}");
    }
}

#[test]
fn resource_ranges_are_checked_against_objects() {
    let mut bytes = fixture();
    // Add a resource between the page map and fixup section.
    bytes.splice(228..228, [0u8; 14]);
    for (field, value) in [
        (56, 46),
        (80, 228),
        (84, 1),
        (104, 242),
        (108, 254),
        (112, 257),
        (120, 257),
        (128, 270),
    ] {
        put32(&mut bytes, field, value);
    }
    put16(&mut bytes, 228, 10);
    put16(&mut bytes, 230, 20);
    put32(&mut bytes, 232, 4);
    put16(&mut bytes, 236, 1);
    put32(&mut bytes, 238, 44);
    assert_eq!(parse(&bytes).unwrap().resource(0).unwrap().byte_length, 4);
    put32(&mut bytes, 238, 45);
    assert_eq!(
        parse(&bytes).unwrap_err(),
        LeError::Invalid("resource object range")
    );
    put16(&mut bytes, 236, 0);
    assert_eq!(
        parse(&bytes).unwrap_err(),
        LeError::Invalid("object reference")
    );
}

#[test]
fn page_number_is_big_endian_above_255() {
    let page_count = 258;
    let map = 220;
    let fixpage = map + page_count * 4;
    let fixrecord = fixpage + (page_count + 1) * 4;
    let data = fixrecord;
    let mut bytes = fixture();
    bytes.resize(data + page_count, 0);
    put32(&mut bytes, 20, page_count as u32);
    put32(&mut bytes, 28, 0);
    put32(&mut bytes, 36, page_count as u32);
    put32(&mut bytes, 40, 1);
    put32(&mut bytes, 44, 1);
    put32(&mut bytes, 48, ((page_count + 1) * 4) as u32);
    put32(&mut bytes, 56, (24 + page_count * 4) as u32);
    put32(&mut bytes, 104, fixpage as u32);
    put32(&mut bytes, 108, fixrecord as u32);
    put32(&mut bytes, 112, fixrecord as u32);
    put32(&mut bytes, 120, fixrecord as u32);
    put32(&mut bytes, 128, data as u32);
    put32(&mut bytes, 196, page_count as u32);
    put32(&mut bytes, 212, page_count as u32);
    bytes[map..].fill(0);
    for index in 0..page_count {
        let number = ((index + 1) as u32).to_be_bytes();
        bytes[map + index * 4..map + index * 4 + 3].copy_from_slice(&number[1..]);
        bytes[data + index] = index as u8;
    }
    let image = parse(&bytes).unwrap();
    assert_eq!(image.page(256).unwrap().physical_page_number, 257);
    assert_eq!(image.page(256).unwrap().data, LePageData::Enumerated(&[0]));
}

#[test]
fn each_resource_budget_is_enforced() {
    let bytes = fixture();
    for (limits, limit) in [
        (
            LeLimits {
                max_file_bytes: 276,
                ..LeLimits::default()
            },
            LeLimit::FileBytes,
        ),
        (
            LeLimits {
                max_objects: 0,
                ..LeLimits::default()
            },
            LeLimit::Objects,
        ),
        (
            LeLimits {
                max_pages: 1,
                ..LeLimits::default()
            },
            LeLimit::Pages,
        ),
        (
            LeLimits {
                max_page_size: 8,
                ..LeLimits::default()
            },
            LeLimit::PageSize,
        ),
        (
            LeLimits {
                max_virtual_bytes: 47,
                ..LeLimits::default()
            },
            LeLimit::VirtualBytes,
        ),
        (
            LeLimits {
                max_validation_steps: 3,
                ..LeLimits::default()
            },
            LeLimit::ValidationSteps,
        ),
    ] {
        assert_eq!(
            LeImage::parse_at(&bytes, RAW, limits).unwrap_err(),
            LeError::Limit(limit)
        );
    }
    let mut bytes = vec![0; 128];
    mz(&mut bytes, 0, true, None);
    append_image(&mut bytes, 0, 128);
    assert_eq!(
        LeImage::discover(
            &bytes,
            LeLimits {
                max_scan_bytes: 1,
                ..LeLimits::default()
            }
        )
        .unwrap_err(),
        LeError::Limit(LeLimit::ScanBytes)
    );
    assert_eq!(
        LeImage::discover(
            &bytes,
            LeLimits {
                max_candidates: 0,
                ..LeLimits::default()
            }
        )
        .unwrap_err(),
        LeError::Limit(LeLimit::Candidates)
    );
}

#[test]
fn empty_module_page_map_retains_a_bss_object_and_stack() {
    let mut bytes = fixture();
    for (field, value) in [
        (20, 0),
        (24, 0),
        (28, 0),
        (48, 4),
        (56, 24),
        (72, 0),
        (104, 220),
        (108, 224),
        (112, 224),
        (120, 224),
        (128, 0),
        (208, 0),
        (212, 0),
        (220, 0),
    ] {
        put32(&mut bytes, field, value);
    }
    bytes.truncate(224);
    let image = parse(&bytes).unwrap();
    assert_eq!(image.pages().len(), 0);
    assert!(image.header().entry.is_none());
    assert_eq!(image.header().stack.unwrap().offset, 48);
    assert_eq!(image.objects().len(), 1);
    put32(&mut bytes, 208, u32::MAX);
    assert_eq!(
        parse(&bytes).unwrap_err(),
        LeError::Invalid("empty object page range")
    );
}

#[test]
fn loader_can_contain_complete_trailing_fixups_but_not_partial_overlap() {
    let mut bytes = fixture();
    put32(&mut bytes, 56, 47); // loader [196,243), fixups [228,243).
    assert!(parse(&bytes).is_ok());
    for length in [33, 46, 48, 64] {
        put32(&mut bytes, 56, length);
        assert!(parse(&bytes).is_err(), "loader length {length}");
    }
    put32(&mut bytes, 56, 47);
    put32(&mut bytes, 72, 228);
    assert!(parse(&bytes).is_err());
}

#[test]
fn object_page_ranges_are_disjoint_complete_and_capacity_checked() {
    let mut bytes = fixture();
    bytes.splice(220..220, [0u8; 24]);
    for (field, value) in [
        (56, 56),
        (68, 2),
        (72, 244),
        (104, 252),
        (108, 264),
        (112, 267),
        (120, 267),
        (128, 280),
        (212, 1),
        (220, 16),
        (224, 0x20000),
        (228, 0x2003),
        (232, 2),
        (236, 1),
    ] {
        put32(&mut bytes, field, value);
    }
    assert!(parse(&bytes).is_ok());
    put32(&mut bytes, 232, 1);
    assert_eq!(
        parse(&bytes).unwrap_err(),
        LeError::Invalid("overlapping object page ranges")
    );
    put32(&mut bytes, 232, 2);
    put32(&mut bytes, 236, 0);
    assert_eq!(
        parse(&bytes).unwrap_err(),
        LeError::Invalid("unowned logical pages")
    );
}

#[test]
fn single_byte_mutations_never_create_a_panicking_public_view() {
    let original = fixture();
    for position in 0..original.len() {
        for value in [0, 1, 0x7f, 0x80, 0xff] {
            let mut bytes = original.clone();
            bytes[position] = value;
            if let Ok(image) = parse(&bytes) {
                assert_eq!(
                    image.objects().count(),
                    image.header().object_count as usize
                );
                assert_eq!(image.pages().count(), image.header().page_count as usize);
                for page in image.pages() {
                    if let LePageData::Enumerated(data) = page.data {
                        assert!(!data.is_empty());
                    }
                }
                let _ = image.resources().count();
            }
        }
    }
}
