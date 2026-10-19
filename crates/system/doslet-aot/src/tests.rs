use super::*;
use alloc::{vec, vec::Vec};
use doslet_executable::{LeFixupErrorKind, LeFixupField, LeFixupUnsupported, LeLimits};

fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

// Entirely synthetic raw LE. Object tuples are (virtual size, flags, page count).
// Page-map values and record streams use physical page numbers independently.
fn fixture(
    objects: &[(u32, u32, u32)],
    page_map: &[u32],
    physical_records: &[Vec<u8>],
    last_page_bytes: u32,
) -> Vec<u8> {
    assert_eq!(page_map.len(), physical_records.len());
    assert_eq!(
        objects
            .iter()
            .map(|object| object.2 as usize)
            .sum::<usize>(),
        page_map.len()
    );
    let object_table = 196;
    let page_table = object_table + objects.len() * 24;
    let fixup_table = page_table + page_map.len() * 4;
    let fixup_records = fixup_table + (page_map.len() + 1) * 4;
    let data_offset = fixup_records + physical_records.iter().map(Vec::len).sum::<usize>();
    let last_length = if last_page_bytes == 0 {
        16
    } else {
        last_page_bytes as usize
    };
    let data_length =
        page_map.len().saturating_sub(1) * 16 + if page_map.is_empty() { 0 } else { last_length };
    let mut bytes = vec![0; data_offset + data_length];
    bytes[..2].copy_from_slice(b"LE");
    put16(&mut bytes, 8, 2);
    put32(&mut bytes, 20, page_map.len() as u32);
    put32(&mut bytes, 40, 16);
    put32(&mut bytes, 44, last_page_bytes);
    put32(&mut bytes, 48, (data_offset - fixup_table) as u32);
    put32(&mut bytes, 56, (fixup_table - object_table) as u32);
    put32(&mut bytes, 64, object_table as u32);
    put32(&mut bytes, 68, objects.len() as u32);
    put32(&mut bytes, 72, page_table as u32);
    put32(&mut bytes, 104, fixup_table as u32);
    put32(&mut bytes, 108, fixup_records as u32);
    put32(&mut bytes, 128, data_offset as u32);
    let mut next_page = 1;
    for (index, &(virtual_size, flags, page_count)) in objects.iter().enumerate() {
        let start = object_table + index * 24;
        put32(&mut bytes, start, virtual_size);
        put32(&mut bytes, start + 4, (index as u32 + 1) * 0x10000);
        put32(&mut bytes, start + 8, flags);
        put32(&mut bytes, start + 12, next_page);
        put32(&mut bytes, start + 16, page_count);
        next_page += page_count;
    }
    for (index, &number) in page_map.iter().enumerate() {
        let start = page_table + index * 4;
        bytes[start] = (number >> 16) as u8;
        bytes[start + 1] = (number >> 8) as u8;
        bytes[start + 2] = number as u8;
    }
    let mut record_offset = 0;
    for (index, records) in physical_records.iter().enumerate() {
        put32(&mut bytes, fixup_table + index * 4, record_offset as u32);
        let start = fixup_records + record_offset;
        bytes[start..start + records.len()].copy_from_slice(records);
        record_offset += records.len();
    }
    put32(
        &mut bytes,
        fixup_table + physical_records.len() * 4,
        record_offset as u32,
    );
    for (index, page) in bytes[data_offset..].chunks_mut(16).enumerate() {
        page.fill(index as u8 + 1);
    }
    bytes
}

fn single(kind: u8, source: i16, target_object: u8, target_offset: u32) -> Vec<u8> {
    let mut record = vec![kind, 0x10];
    record.extend_from_slice(&source.to_le_bytes());
    record.push(target_object);
    if kind & 0x0f != 2 {
        record.extend_from_slice(&target_offset.to_le_bytes());
    }
    record
}

fn list(kind: u8, sources: &[i16], target_object: u8, target_offset: u32) -> Vec<u8> {
    let mut record = vec![kind | 0x20, 0x10, sources.len() as u8, target_object];
    if kind & 0x0f != 2 {
        record.extend_from_slice(&target_offset.to_le_bytes());
    }
    for source in sources {
        record.extend_from_slice(&source.to_le_bytes());
    }
    record
}

fn prepare(bytes: &[u8]) -> Result<SymbolicImage, ImageError> {
    prepare_with(bytes, ImageLimits::default())
}

fn prepare_with(bytes: &[u8], limits: ImageLimits) -> Result<SymbolicImage, ImageError> {
    let le = LeImage::discover(bytes, LeLimits::default()).unwrap();
    SymbolicImage::from_le(&le, limits)
}

#[test]
fn reconstructs_partial_last_page_and_bss_without_relocating() {
    let records = single(7, 2, 1, 0x1234_5678);
    let bytes = fixture(&[(48, 0x2007, 2)], &[1, 2], &[records, vec![]], 5);
    let image = prepare(&bytes).unwrap();
    let object = image.object(1).unwrap();
    assert_eq!(object.object_number(), 1);
    assert_eq!(object.metadata().preferred_base, 0x10000);
    assert!(object.metadata().is_executable());
    assert!(object.metadata().is_writable());
    assert_eq!(object.default_bitness(), DefaultBitness::Bits32);
    assert_eq!(&object.bytes()[..16], &[1; 16]);
    assert_eq!(&object.bytes()[16..21], &[2; 5]);
    assert_eq!(&object.bytes()[21..], &[0; 27]);
    let spans = object.file_backed_spans();
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0].object_offset, 0);
    assert_eq!(spans[0].byte_len, 16);
    assert_eq!(spans[1].object_offset, 16);
    assert_eq!(spans[1].byte_len, 5);
    for span in spans {
        let length = span.byte_len as usize;
        assert_eq!(
            &object.bytes()[span.object_offset as usize..][..length],
            &bytes[span.file_offset..][..length]
        );
    }
    let relocation = image.relocations().next().unwrap();
    assert_eq!(relocation.target.offset, Some(0x1234_5678));
    assert_eq!(relocation.source.offset, 2);
    assert_eq!(&object.bytes()[2..6], &[1; 4]);
    assert_eq!(image.object(0).map(SymbolicObject::object_number), None);
    assert!(image.object(2).is_none());
    assert!(image.object(u32::MAX).is_none());
}

#[test]
fn clips_file_pages_to_virtual_size_and_keeps_bss_only_objects() {
    let bytes = fixture(
        &[(19, 4, 2), (23, 2, 0), (0, 0x2000, 0)],
        &[1, 2],
        &[vec![], vec![]],
        0,
    );
    let image = prepare(&bytes).unwrap();
    let object = image.object(1).unwrap();
    assert_eq!(object.bytes().len(), 19);
    assert_eq!(&object.bytes()[16..], &[2; 3]);
    assert_eq!(object.file_backed_spans()[1].byte_len, 3);
    assert_eq!(object.default_bitness(), DefaultBitness::Bits16);
    assert_eq!(image.object(2).unwrap().bytes(), &[0; 23]);
    assert!(image.object(2).unwrap().file_backed_spans().is_empty());
    assert!(image.object(3).unwrap().bytes().is_empty());
    assert_eq!(image.relocations().len(), 0);
}

#[test]
fn reordered_pages_preserve_data_gaps_and_physical_fixup_association() {
    let bytes = fixture(
        &[(48, 0x2007, 3)],
        &[3, 1, 2],
        &[
            single(7, 0, 1, 101),
            single(7, 4, 1, 202),
            single(0, 0, 1, 303),
        ],
        5,
    );
    let image = prepare(&bytes).unwrap();
    let object = image.object(1).unwrap();
    assert_eq!(&object.bytes()[..5], &[3; 5]);
    assert_eq!(&object.bytes()[5..16], &[0; 11]);
    assert_eq!(&object.bytes()[16..32], &[1; 16]);
    assert_eq!(&object.bytes()[32..], &[2; 16]);
    let relocations: Vec<_> = image.relocations().collect();
    assert_eq!(relocations.len(), 3);
    for (index, (offset, target, physical)) in [(0, 303, 3), (16, 101, 1), (36, 202, 2)]
        .into_iter()
        .enumerate()
    {
        let relocation = relocations[index];
        assert_eq!(relocation.source.offset, offset);
        assert_eq!(relocation.target.offset, Some(target));
        assert_eq!(
            relocation.provenance[0].context.logical_page_number,
            index as u32 + 1
        );
        assert_eq!(
            relocation.provenance[0].context.physical_page_number,
            physical
        );
    }
}

#[test]
fn all_kinds_and_alias_metadata_remain_symbolic() {
    let first = [
        single(0, 0, 1, u32::MAX),
        single(0x12, 2, 1, 0),
        single(0x13, 4, 1, 48),
        single(5, 8, 1, 49),
    ]
    .concat();
    let second = [
        single(0x16, 0, 1, u32::MAX),
        single(7, 6, 1, 0),
        single(8, 10, 1, 0),
    ]
    .concat();
    let bytes = fixture(&[(48, 0x2007, 2)], &[1, 2], &[first, second], 0);
    let image = prepare(&bytes).unwrap();
    let relocations: Vec<_> = image.relocations().collect();
    assert_eq!(relocations.len(), 7);
    assert_eq!(relocations[0].kind, LeFixupSourceType::Byte);
    assert_eq!(relocations[0].target.offset, Some(u32::MAX));
    assert_eq!(relocations[1].kind, LeFixupSourceType::Selector16);
    assert_eq!(relocations[1].target.offset, None);
    assert!(relocations[1].is_alias);
    assert_eq!(relocations[2].kind, LeFixupSourceType::Pointer1616);
    assert!(relocations[2].is_alias);
    assert_eq!(relocations[2].target.offset, Some(48));
    assert_eq!(relocations[3].kind, LeFixupSourceType::Offset16);
    assert_eq!(relocations[3].target.offset, Some(49));
    assert_eq!(relocations[4].kind, LeFixupSourceType::Pointer1632);
    assert!(relocations[4].is_alias);
    assert_eq!(relocations[5].kind, LeFixupSourceType::Offset32);
    assert_eq!(relocations[6].kind, LeFixupSourceType::SelfRelative32);
}

#[test]
fn deduplicates_cross_page_writes_and_retains_both_provenances() {
    let bytes = fixture(
        &[(32, 0x2007, 2)],
        &[1, 2],
        &[single(7, 14, 1, 23), single(7, -2, 1, 23)],
        0,
    );
    let image = prepare(&bytes).unwrap();
    assert_eq!(image.relocations().len(), 1);
    let relocation = image.relocations().next().unwrap();
    assert_eq!(
        relocation.source,
        ObjectAddress {
            object_number: 1,
            offset: 14
        }
    );
    assert_eq!(relocation.provenance.len(), 2);
    assert_eq!(relocation.provenance[0].page_relative_offset, 14);
    assert_eq!(relocation.provenance[0].context.object_page_index, 0);
    assert_eq!(relocation.provenance[1].page_relative_offset, -2);
    assert_eq!(relocation.provenance[1].context.object_page_index, 1);
}

#[test]
fn source_lists_sort_deduplicate_and_keep_entry_indices() {
    let bytes = fixture(&[(16, 4, 1)], &[1], &[list(7, &[8, 0, 8], 1, 16)], 0);
    let image = prepare(&bytes).unwrap();
    let relocations: Vec<_> = image.relocations().collect();
    assert_eq!(relocations.len(), 2);
    assert_eq!(relocations[0].source.offset, 0);
    assert_eq!(relocations[0].provenance[0].source_index, 1);
    assert_eq!(relocations[1].source.offset, 8);
    assert_eq!(relocations[1].provenance.len(), 2);
    assert_eq!(relocations[1].provenance[0].source_index, 0);
    assert_eq!(relocations[1].provenance[1].source_index, 2);
}

#[test]
fn empty_source_lists_do_not_create_sites_or_consume_source_limit() {
    let bytes = fixture(&[(16, 4, 1)], &[1], &[list(7, &[], 1, u32::MAX)], 0);
    let image = prepare_with(
        &bytes,
        ImageLimits {
            max_relocation_sources: 0,
            ..ImageLimits::default()
        },
    )
    .unwrap();
    assert_eq!(image.relocations().len(), 0);
}

#[test]
fn conflicting_cross_page_targets_are_errors() {
    let bytes = fixture(
        &[(32, 4, 2)],
        &[1, 2],
        &[single(7, 14, 1, 23), single(7, -2, 1, 24)],
        0,
    );
    let ImageError::ConflictingRelocations {
        source,
        first,
        second,
    } = prepare(&bytes).unwrap_err()
    else {
        panic!("expected conflicting sites");
    };
    assert_eq!(source.offset, 14);
    assert_eq!(first.context.logical_page_number, 1);
    assert_eq!(second.context.logical_page_number, 2);
}

#[test]
fn source_kind_target_object_and_alias_participate_in_equivalence() {
    for second in [single(8, 0, 1, 3), single(7, 0, 2, 3)] {
        let records = [single(7, 0, 1, 3), second].concat();
        let bytes = fixture(&[(16, 4, 1), (16, 2, 0)], &[1], &[records], 0);
        assert!(matches!(
            prepare(&bytes),
            Err(ImageError::ConflictingRelocations { .. })
        ));
    }
    let records = [single(2, 0, 1, 0), single(0x12, 0, 1, 0)].concat();
    let bytes = fixture(&[(16, 4, 1)], &[1], &[records], 0);
    assert!(matches!(
        prepare(&bytes),
        Err(ImageError::ConflictingRelocations { .. })
    ));
}

#[test]
fn overlapping_writes_fail_but_adjacent_writes_are_valid() {
    for second_offset in [1, 3] {
        let records = [single(7, 0, 1, 0), single(0, second_offset, 1, 0)].concat();
        let bytes = fixture(&[(16, 4, 1)], &[1], &[records], 0);
        let ImageError::OverlappingRelocations {
            first_source,
            second_source,
            ..
        } = prepare(&bytes).unwrap_err()
        else {
            panic!("expected overlapping writes");
        };
        assert_eq!(first_source.offset, 0);
        assert_eq!(second_source.offset, second_offset as u32);
    }
    let records = [single(7, 0, 1, 0), single(7, 4, 1, 0)].concat();
    let bytes = fixture(&[(16, 4, 1)], &[1], &[records], 0);
    assert_eq!(prepare(&bytes).unwrap().relocations().len(), 2);
}

#[test]
fn object_boundaries_separate_identical_offsets() {
    let bytes = fixture(
        &[(16, 4, 1), (16, 0x2002, 1)],
        &[1, 2],
        &[single(7, 0, 2, 0), single(7, 0, 1, 0)],
        0,
    );
    let image = prepare(&bytes).unwrap();
    let sites: Vec<_> = image.relocations().collect();
    assert_eq!(sites.len(), 2);
    assert_eq!(sites[0].source.object_number, 1);
    assert_eq!(sites[1].source.object_number, 2);
    assert_eq!(sites[1].provenance[0].context.object_page_index, 0);
    assert_eq!(sites[1].provenance[0].context.logical_page_number, 2);
}

#[test]
fn missing_target_object_has_object_page_and_record_context() {
    let good = single(7, 0, 1, 0);
    let records = [good.clone(), single(7, 8, 2, 0)].concat();
    let bytes = fixture(&[(16, 4, 1)], &[1], &[records], 0);
    let ImageError::MissingTargetObject {
        context,
        record_offset,
        target_object_number,
    } = prepare(&bytes).unwrap_err()
    else {
        panic!("expected missing target");
    };
    assert_eq!(context.object_number, 1);
    assert_eq!(context.logical_page_number, 1);
    assert_eq!(context.physical_page_number, 1);
    assert_eq!(record_offset, good.len());
    assert_eq!(target_object_number, 2);
}

#[test]
fn negative_source_on_first_object_page_cannot_write_previous_object() {
    let bytes = fixture(
        &[(16, 4, 1), (16, 2, 1)],
        &[1, 2],
        &[vec![], single(7, -2, 1, 0)],
        0,
    );
    let ImageError::SourceOutsideObject {
        provenance,
        object_offset,
        width,
        virtual_size,
    } = prepare(&bytes).unwrap_err()
    else {
        panic!("expected out-of-object source");
    };
    assert_eq!(object_offset, -2);
    assert_eq!(width, 4);
    assert_eq!(virtual_size, 16);
    assert_eq!(provenance.context.object_number, 2);
    assert_eq!(provenance.context.object_page_index, 0);
}

#[test]
fn full_write_width_must_fit_source_object_including_cross_page_tails() {
    for (size, source) in [(16, 14), (18, 15)] {
        let bytes = fixture(
            &[(size, 4, 1), (16, 2, 0)],
            &[1],
            &[single(7, source, 2, 0)],
            0,
        );
        assert!(
            matches!(prepare(&bytes), Err(ImageError::SourceOutsideObject { object_offset, width: 4, virtual_size, .. }) if object_offset == i64::from(source) && virtual_size == size)
        );
    }
    // A relocation may extend from input bytes into the object's own BSS.
    let bytes = fixture(&[(18, 4, 1)], &[1], &[single(7, 14, 1, 0)], 0);
    assert_eq!(prepare(&bytes).unwrap().relocations().len(), 1);
}

#[test]
fn complete_stream_is_validated_and_fixup_error_is_preserved_exactly() {
    let good = single(7, 0, 1, 0);
    let records = [good.clone(), vec![7, 0x10, 4, 0, 1, 9]].concat();
    let bytes = fixture(&[(16, 4, 1)], &[1], &[records], 0);
    let ImageError::Fixup { context, error } = prepare(&bytes).unwrap_err() else {
        panic!("expected fixup error");
    };
    assert_eq!(context.object_number, 1);
    assert_eq!(context.logical_page_number, 1);
    assert_eq!(
        error,
        LeFixupError {
            record_offset: good.len(),
            byte_offset: good.len() + 5,
            kind: LeFixupErrorKind::Truncated(LeFixupField::TargetOffset)
        }
    );
}

#[test]
fn unsupported_record_after_good_page_is_never_skipped() {
    let bytes = fixture(&[(32, 4, 2)], &[1, 2], &[single(7, 0, 1, 0), vec![7, 1]], 0);
    let ImageError::Fixup { context, error } = prepare(&bytes).unwrap_err() else {
        panic!("expected unsupported record");
    };
    assert_eq!(context.logical_page_number, 2);
    assert_eq!(
        error.kind,
        LeFixupErrorKind::Unsupported(LeFixupUnsupported::ImportByOrdinal)
    );
    assert_eq!(error.byte_offset, 1);
}

#[test]
fn fixup_decode_budget_is_shared_across_pages_and_objects() {
    let bytes = fixture(
        &[(16, 4, 1), (16, 4, 1)],
        &[1, 2],
        &[single(7, 0, 1, 0), single(7, 0, 1, 0)],
        0,
    );
    let limits = ImageLimits {
        max_fixup_decode_steps: 3,
        ..ImageLimits::default()
    };
    let ImageError::Fixup { context, error } = prepare_with(&bytes, limits).unwrap_err() else {
        panic!("expected shared decode limit");
    };
    assert_eq!(context.object_number, 2);
    assert_eq!(context.logical_page_number, 2);
    assert_eq!(error.kind, LeFixupErrorKind::DecodeBudget);
    assert_eq!(error.byte_offset, 2);
    let limits = ImageLimits {
        max_fixup_decode_steps: 4,
        ..limits
    };
    assert_eq!(prepare_with(&bytes, limits).unwrap().relocations().len(), 2);
}

#[test]
fn relocation_source_limit_counts_occurrences_before_deduplication() {
    let bytes = fixture(&[(16, 4, 1)], &[1], &[list(7, &[0, 0], 1, 0)], 0);
    let limits = ImageLimits {
        max_relocation_sources: 1,
        ..ImageLimits::default()
    };
    assert_eq!(
        prepare_with(&bytes, limits).unwrap_err(),
        ImageError::RelocationSourceLimit
    );
    let limits = ImageLimits {
        max_relocation_sources: 2,
        ..limits
    };
    assert_eq!(prepare_with(&bytes, limits).unwrap().relocations().len(), 1);
}

#[test]
fn owned_storage_budget_includes_objects_bytes_spans_and_final_metadata() {
    let bytes = fixture(&[(16, 4, 1)], &[1], &[single(7, 0, 1, 0)], 0);
    let before_sources = size_of::<SymbolicObject>() + 16 + size_of::<FileBackedSpan>();
    for (max_owned_bytes, expected) in [
        (0, ImageStorage::Objects),
        (size_of::<SymbolicObject>(), ImageStorage::ObjectBytes),
        (
            size_of::<SymbolicObject>() + 16,
            ImageStorage::FileBackedSpans,
        ),
        (before_sources, ImageStorage::RelocationSources),
        (
            before_sources + size_of::<PendingRelocation>(),
            ImageStorage::Relocations,
        ),
        (
            before_sources + size_of::<PendingRelocation>() + size_of::<Site>(),
            ImageStorage::Provenance,
        ),
    ] {
        let limits = ImageLimits {
            max_owned_bytes,
            max_relocation_sources: 1,
            ..ImageLimits::default()
        };
        assert_eq!(
            prepare_with(&bytes, limits).unwrap_err(),
            ImageError::OwnedByteLimit(expected)
        );
    }
    let exact = before_sources
        + size_of::<PendingRelocation>()
        + size_of::<Site>()
        + size_of::<SourceProvenance>();
    let limits = ImageLimits {
        max_owned_bytes: exact,
        max_relocation_sources: 1,
        ..ImageLimits::default()
    };
    assert_eq!(prepare_with(&bytes, limits).unwrap().relocations().len(), 1);
}

#[test]
fn source_growth_remains_bounded_beyond_initial_capacity() {
    let sources: Vec<_> = (0..16).collect();
    let bytes = fixture(
        &[(32, 4, 2)],
        &[1, 2],
        &[list(0, &sources, 1, 0), single(0, 0, 1, 0)],
        0,
    );
    let limits = ImageLimits {
        max_relocation_sources: 17,
        ..ImageLimits::default()
    };
    assert_eq!(
        prepare_with(&bytes, limits).unwrap().relocations().len(),
        17
    );
    let limits = ImageLimits {
        max_relocation_sources: 16,
        ..limits
    };
    assert_eq!(
        prepare_with(&bytes, limits).unwrap_err(),
        ImageError::RelocationSourceLimit
    );
}

#[test]
fn storage_size_multiplication_is_checked_before_allocation() {
    let mut budget = StorageBudget(usize::MAX);
    let mut values: Vec<SourceProvenance> = Vec::new();
    assert_eq!(
        budget.reserve(&mut values, usize::MAX, ImageStorage::Provenance),
        Err(ImageError::OwnedByteLimit(ImageStorage::Provenance))
    );
    assert_eq!(values.capacity(), 0);
}
