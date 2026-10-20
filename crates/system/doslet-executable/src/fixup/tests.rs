extern crate std;

use super::*;
use crate::LePageData;
use std::{format, vec, vec::Vec};

const PAGE_SIZE: u32 = 4096;

fn decoder(bytes: &[u8]) -> LeFixups<'_> {
    LeFixups::new(bytes, PAGE_SIZE, LeFixupLimits::default()).unwrap()
}

fn one(bytes: &[u8]) -> LeFixup<'_> {
    let mut records = decoder(bytes);
    let result = records.next().unwrap().unwrap();
    assert_eq!(records.next(), None);
    result
}

fn failure(bytes: &[u8]) -> LeFixupError {
    let mut records = decoder(bytes);
    let error = records.next().unwrap().unwrap_err();
    assert_eq!(records.next(), None);
    assert_eq!(records.next(), None);
    error
}

fn single_source(code: u8, source: i16) -> Vec<u8> {
    let mut bytes = vec![code, 0];
    bytes.extend_from_slice(&source.to_le_bytes());
    bytes.push(1);
    if code & 0x0f != 2 {
        bytes.extend_from_slice(&[0x34, 0x12]);
    }
    bytes
}

#[test]
fn every_source_type_has_the_right_write_width() {
    for (code, expected, width) in [
        (0, LeFixupSourceType::Byte, 1),
        (2, LeFixupSourceType::Selector16, 2),
        (3, LeFixupSourceType::Pointer1616, 4),
        (5, LeFixupSourceType::Offset16, 2),
        (6, LeFixupSourceType::Pointer1632, 6),
        (7, LeFixupSourceType::Offset32, 4),
        (8, LeFixupSourceType::SelfRelative32, 4),
    ] {
        let bytes = single_source(code, 0x123);
        let record = one(&bytes);
        assert_eq!(record.source_type(), expected);
        assert_eq!(record.source_type() as u8, code);
        assert_eq!(record.source_type().width(), width);
        assert_eq!(record.sources().iter().collect::<Vec<_>>(), [0x123]);
        assert_eq!(record.target().object_number(), 1);
        assert_eq!(record.target().offset(), (code != 2).then_some(0x1234));
        assert!(!record.is_alias());
        assert!(!record.is_source_list());
        assert_eq!(record.bytes().as_ptr(), bytes.as_ptr());
        assert_eq!(record.bytes(), bytes);
    }
}

#[test]
fn target_integer_widths_are_independent_of_the_source_width() {
    for (bytes, object, offset, flags) in [
        (&[0, 0, 0, 0, 0xff, 0xff, 0xff][..], 0xff, 0xffff, 0),
        (
            &[7, 0x40, 0, 0, 0x34, 0x12, 0xcd, 0xab],
            0x1234,
            0xabcd,
            0x40,
        ),
        (
            &[5, 0x10, 0, 0, 2, 0x78, 0x56, 0x34, 0x12],
            2,
            0x1234_5678,
            0x10,
        ),
        (
            &[6, 0x50, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
            0xffff,
            0xffff_ffff,
            0x50,
        ),
    ] {
        let record = one(bytes);
        assert_eq!(record.target().object_number(), object);
        assert_eq!(record.target().offset(), Some(offset));
        assert_eq!(record.target_flags(), flags);
    }
}

#[test]
fn source_list_follows_target_data_and_borrows_offsets() {
    let bytes = [
        0x27, 0x50, 3, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12, 0xfd, 0xff, 0, 0, 0xff, 0x0f,
    ];
    let record = one(&bytes);
    assert!(record.is_source_list());
    assert_eq!(record.target().object_number(), 0x1234);
    assert_eq!(record.target().offset(), Some(0x1234_5678));
    assert_eq!(record.sources().bytes.as_ptr(), bytes[9..].as_ptr());
    assert_eq!(record.sources().len(), 3);
    assert!(!record.sources().is_empty());
    let mut offsets = record.sources().iter();
    assert_eq!(offsets.len(), 3);
    assert_eq!(offsets.next(), Some(-3));
    assert_eq!(offsets.next_back(), Some(4095));
    assert_eq!(offsets.next(), Some(0));
    assert_eq!(offsets.len(), 0);
    assert_eq!(offsets.next(), None);
    assert_eq!(offsets.next_back(), None);
}

#[test]
fn selectors_omit_target_offset_even_when_width_flag_is_set() {
    let bytes = [
        0x32, 0x50, 2, 0x34, 0x12, 0xff, 0xff, 8, 0, // Alias selector list.
        2, 0x10, 3, 0, 1, // Single selector, no target offset.
        7, 0, 4, 0, 2, 0x78, 0x56, // Offset32 source, 16-bit target encoding.
    ];
    let mut records = decoder(&bytes);
    let first = records.next().unwrap().unwrap();
    assert_eq!(first.record_offset(), 0);
    assert_eq!(first.bytes(), &bytes[..9]);
    assert!(first.is_alias());
    assert_eq!(first.target().offset(), None);
    assert_eq!(first.sources().iter().collect::<Vec<_>>(), [-1, 8]);
    let second = records.next().unwrap().unwrap();
    assert_eq!(second.record_offset(), 9);
    assert_eq!(second.target().offset(), None);
    assert_eq!(second.sources().iter().next(), Some(3));
    let third = records.next().unwrap().unwrap();
    assert_eq!(third.record_offset(), 14);
    assert_eq!(third.target().offset(), Some(0x5678));
    assert_eq!(records.next(), None);
}

#[test]
fn signed_sources_must_touch_this_page_for_each_write_width() {
    for (code, width) in [(0, 1), (2, 2), (3, 4), (5, 2), (6, 6), (7, 4), (8, 4)] {
        for source in [1 - width, 0, 4095] {
            let bytes = single_source(code, source);
            assert_eq!(one(&bytes).sources().iter().next(), Some(source));
        }
        for source in [-width, i16::MIN, 4096, i16::MAX] {
            let bytes = single_source(code, source);
            assert_eq!(
                failure(&bytes),
                LeFixupError {
                    record_offset: 0,
                    byte_offset: 2,
                    kind: LeFixupErrorKind::SourceOutsidePage {
                        source_offset: source,
                        width: width as u8,
                        page_size: PAGE_SIZE,
                    },
                }
            );
        }
    }
}

#[test]
fn supported_page_size_boundaries_are_explicit() {
    let bytes = single_source(6, -5);
    let mut small = LeFixups::new(&bytes, 1, LeFixupLimits::default()).unwrap();
    assert!(small.next().unwrap().is_ok());
    let bytes = single_source(7, i16::MAX);
    let mut largest = LeFixups::new(&bytes, 32768, LeFixupLimits::default()).unwrap();
    assert!(largest.next().unwrap().is_ok());
    for page_size in [0, 32769, 65536, u32::MAX] {
        let error = LeFixups::new(&[], page_size, LeFixupLimits::default()).unwrap_err();
        assert_eq!(error.record_offset, 0);
        assert_eq!(error.byte_offset, 0);
        assert_eq!(
            error.kind,
            LeFixupErrorKind::Unsupported(LeFixupUnsupported::PageSize(page_size))
        );
    }
}

#[test]
fn alias_flag_is_metadata_only_and_is_limited_to_selector_pointer_types() {
    for code in [0, 2, 3, 5, 6, 7, 8] {
        let bytes = single_source(code | 0x10, 0);
        if matches!(code, 2 | 3 | 6) {
            let record = one(&bytes);
            assert!(record.is_alias());
            assert_eq!(record.target().offset(), (code != 2).then_some(0x1234));
        } else {
            let error = failure(&bytes);
            assert_eq!(error.byte_offset, 0);
            assert!(matches!(error.kind, LeFixupErrorKind::InvalidAlias(_)));
        }
    }
    // Alias suitability (including a 64 KiB target limit) belongs to application.
    assert!(one(&[0x13, 0x10, 0, 0, 1, 0, 0, 1, 0]).is_alias());
}

#[test]
fn unknown_types_and_reserved_source_flags_are_precise_errors() {
    for code in [1, 4, 9, 10, 11, 12, 13, 14, 15] {
        for modifier in [0, 0x10, 0x20, 0x30] {
            assert_eq!(
                failure(&[code | modifier]).kind,
                LeFixupErrorKind::UnknownSourceType(code)
            );
        }
    }
    for reserved in [0x40, 0x80, 0xc0] {
        let source = 7 | reserved;
        assert_eq!(
            failure(&[source]).kind,
            LeFixupErrorKind::ReservedSourceFlags(source)
        );
    }
}

#[test]
fn unsupported_targets_fail_before_consuming_an_unknown_layout() {
    for (flags, feature) in [
        (1, LeFixupUnsupported::ImportByOrdinal),
        (2, LeFixupUnsupported::ImportByName),
        (3, LeFixupUnsupported::InternalViaEntry),
        (8, LeFixupUnsupported::InternalChain),
        (4, LeFixupUnsupported::Additive),
        (0x20, LeFixupUnsupported::Additive),
        (0x24, LeFixupUnsupported::Additive),
        (0x80, LeFixupUnsupported::Ordinal8),
    ] {
        assert_eq!(
            failure(&[7, flags]),
            LeFixupError {
                record_offset: 0,
                byte_offset: 1,
                kind: LeFixupErrorKind::Unsupported(feature),
            }
        );
    }
    // Every target flag combination outside 0x00/0x10/0x40/0x50 is rejected.
    for flags in 0..=u8::MAX {
        let kind = failure(&[7, flags]).kind;
        if flags & !0x50 == 0 {
            assert_eq!(
                kind,
                LeFixupErrorKind::Truncated(LeFixupField::SourceOffset)
            );
        } else {
            assert!(
                matches!(kind, LeFixupErrorKind::Unsupported(_)),
                "{flags:#x}"
            );
        }
    }
}

#[test]
fn zero_object_numbers_are_rejected_for_both_encodings() {
    for bytes in [&[7, 0, 0, 0, 0][..], &[7, 0x40, 0, 0, 0, 0]] {
        let error = failure(bytes);
        assert_eq!(error.byte_offset, 4);
        assert_eq!(error.kind, LeFixupErrorKind::ZeroObjectNumber);
    }
    let error = failure(&[0x27, 0x40, 0, 0, 0]);
    assert_eq!(error.byte_offset, 3);
    assert_eq!(error.kind, LeFixupErrorKind::ZeroObjectNumber);
}

#[test]
fn all_nonempty_truncated_prefixes_fail_without_publishing_a_partial_record() {
    let records: &[&[u8]] = &[
        &[7, 0, 0, 0, 1, 0x34, 0x12],
        &[7, 0x50, 0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        &[2, 0x40, 0, 0, 0x34, 0x12],
        &[
            0x27, 0x50, 3, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12, 0xfd, 0xff, 0, 0, 0xff, 0x0f,
        ],
        &[0x32, 0x40, 2, 0x34, 0x12, 0xff, 0xff, 0, 0],
    ];
    for bytes in records {
        assert!(decoder(&[]).next().is_none());
        for length in 1..bytes.len() {
            let error = failure(&bytes[..length]);
            assert!(
                matches!(error.kind, LeFixupErrorKind::Truncated(_)),
                "prefix {length} of {bytes:x?}: {error}"
            );
            assert!(error.byte_offset <= length);
            assert_eq!(error.record_offset, 0);
        }
        one(bytes);
    }
}

#[test]
fn truncation_identifies_each_field_start() {
    let bytes = [
        0x27, 0x50, 3, 0x34, 0x12, 0x78, 0x56, 0x34, 0x12, 0xfd, 0xff, 0, 0, 0xff, 0x0f,
    ];
    for (length, field_offset, field) in [
        (1, 1, LeFixupField::TargetFlags),
        (2, 2, LeFixupField::SourceCount),
        (3, 3, LeFixupField::ObjectNumber),
        (4, 3, LeFixupField::ObjectNumber),
        (5, 5, LeFixupField::TargetOffset),
        (8, 5, LeFixupField::TargetOffset),
        (9, 9, LeFixupField::SourceList),
        (14, 9, LeFixupField::SourceList),
    ] {
        assert_eq!(
            failure(&bytes[..length]),
            LeFixupError {
                record_offset: 0,
                byte_offset: field_offset,
                kind: LeFixupErrorKind::Truncated(field),
            }
        );
    }
    assert_eq!(failure(&[7, 0, 1]).byte_offset, 2);
}

#[test]
fn an_invalid_last_list_entry_prevents_the_whole_record_from_being_published() {
    let bytes = [0x27, 0, 3, 1, 0x34, 0x12, 0, 0, 1, 0, 0, 0x10];
    let error = failure(&bytes);
    assert_eq!(error.record_offset, 0);
    assert_eq!(error.byte_offset, 10);
    assert!(matches!(
        error.kind,
        LeFixupErrorKind::SourceOutsidePage {
            source_offset: 4096,
            ..
        }
    ));
}

#[test]
fn empty_source_lists_consume_their_target_and_one_record_step() {
    let bytes = [0x27, 0, 0, 1, 0x34, 0x12];
    let mut records = LeFixups::new(
        &bytes,
        PAGE_SIZE,
        LeFixupLimits {
            max_decode_steps: 1,
        },
    )
    .unwrap();
    let record = records.next().unwrap().unwrap();
    assert!(record.is_source_list());
    assert_eq!(record.target().offset(), Some(0x1234));
    assert!(record.sources().is_empty());
    assert_eq!(record.sources().len(), 0);
    assert_eq!(record.sources().iter().next(), None);
    assert_eq!(records.remaining_decode_steps(), 0);
    assert_eq!(records.next(), None);
}

#[test]
fn maximum_source_list_count_is_decoded_with_a_finite_budget() {
    let mut bytes = vec![0x27, 0, 255, 1, 0x34, 0x12];
    for offset in 0i16..255 {
        bytes.extend_from_slice(&offset.to_le_bytes());
    }
    let mut records = LeFixups::new(
        &bytes,
        PAGE_SIZE,
        LeFixupLimits {
            max_decode_steps: 256,
        },
    )
    .unwrap();
    let record = records.next().unwrap().unwrap();
    assert_eq!(record.sources().len(), 255);
    assert_eq!(record.sources().iter().last(), Some(254));
    assert_eq!(records.remaining_decode_steps(), 0);
    assert_eq!(records.next(), None);
}

#[test]
fn budget_exhaustion_is_precise_and_never_publishes_a_partial_record() {
    let bytes = [0x27, 0, 3, 1, 0x34, 0x12, 0, 0, 1, 0, 2, 0];
    for (budget, expected_offset) in [(0, 0), (1, 6), (2, 8), (3, 10)] {
        let mut records = LeFixups::new(
            &bytes,
            PAGE_SIZE,
            LeFixupLimits {
                max_decode_steps: budget,
            },
        )
        .unwrap();
        assert_eq!(
            records.next(),
            Some(Err(LeFixupError {
                record_offset: 0,
                byte_offset: expected_offset,
                kind: LeFixupErrorKind::DecodeBudget,
            }))
        );
        assert_eq!(records.remaining_decode_steps(), 0);
        assert_eq!(records.next(), None);
        assert_eq!(records.next(), None);
    }
    let mut records = LeFixups::new(
        &bytes,
        PAGE_SIZE,
        LeFixupLimits {
            max_decode_steps: 4,
        },
    )
    .unwrap();
    assert!(records.next().unwrap().is_ok());
    assert_eq!(records.remaining_decode_steps(), 0);
    assert_eq!(records.next(), None);
    assert_eq!(
        LeFixups::new(
            &[],
            PAGE_SIZE,
            LeFixupLimits {
                max_decode_steps: 0
            }
        )
        .unwrap()
        .next(),
        None
    );
}

#[test]
fn concatenated_errors_use_stream_offsets_and_fuse_without_resynchronizing() {
    let bytes = [7, 0, 0, 0, 1, 0x34, 0x12, 7, 8, 7, 0, 1, 0, 1, 0, 0];
    let mut records = decoder(&bytes);
    assert_eq!(records.next().unwrap().unwrap().record_offset(), 0);
    let error = records.next().unwrap().unwrap_err();
    assert_eq!(error.record_offset, 7);
    assert_eq!(error.byte_offset, 8);
    assert_eq!(
        error.kind,
        LeFixupErrorKind::Unsupported(LeFixupUnsupported::InternalChain)
    );
    assert_eq!(records.next(), None);
    assert_eq!(records.next(), None);
    assert_eq!(
        format!("{error}"),
        "LE fixup record at 0x7, field at 0x8: unsupported InternalChain"
    );
}

#[test]
fn budget_is_shared_across_records_and_can_continue_across_pages() {
    let bytes = [7, 0, 0, 0, 1, 0x34, 0x12, 7, 0, 1, 0, 1, 0x78, 0x56];
    let mut records = LeFixups::new(
        &bytes,
        PAGE_SIZE,
        LeFixupLimits {
            max_decode_steps: 3,
        },
    )
    .unwrap();
    assert!(records.next().unwrap().is_ok());
    assert_eq!(
        records.next().unwrap().unwrap_err(),
        LeFixupError {
            record_offset: 7,
            byte_offset: 9,
            kind: LeFixupErrorKind::DecodeBudget,
        }
    );
    assert_eq!(records.next(), None);
    let mut first = LeFixups::new(
        &bytes[..7],
        PAGE_SIZE,
        LeFixupLimits {
            max_decode_steps: 4,
        },
    )
    .unwrap();
    assert!(first.next().unwrap().is_ok());
    assert_eq!(first.next(), None);
    let mut second = LeFixups::new(
        &bytes[7..],
        PAGE_SIZE,
        LeFixupLimits {
            max_decode_steps: first.remaining_decode_steps(),
        },
    )
    .unwrap();
    assert!(second.next().unwrap().is_ok());
    assert_eq!(second.next(), None);
    assert_eq!(second.remaining_decode_steps(), 0);
}

#[test]
fn caller_constructed_page_bytes_do_not_bypass_validation() {
    let page = LePage {
        physical_page_number: 1,
        data: LePageData::Invalid,
        fixup_records: &[7],
    };
    let mut records = page.fixups(PAGE_SIZE, LeFixupLimits::default()).unwrap();
    assert_eq!(
        records.next().unwrap().unwrap_err().kind,
        LeFixupErrorKind::Truncated(LeFixupField::TargetFlags)
    );
    assert_eq!(records.next(), None);
}

#[test]
fn record_and_source_iterators_implement_fused_iterator() {
    fn fused(_: impl FusedIterator) {}
    let bytes = single_source(7, 0);
    fused(decoder(&bytes));
    fused(one(&bytes).sources().iter());
    let mut records = decoder(&bytes);
    assert!(records.next().unwrap().is_ok());
    assert_eq!(records.next(), None);
    assert_eq!(records.next(), None);
}
