use super::*;
use alloc::{borrow::Cow, vec};
use wasm_encoder::{
    CodeSection, CustomSection, DataSection, Encode, ExportKind, ExportSection, Function,
    FunctionSection, Instruction, Module, TypeSection, ValType,
};

fn custom(name: &str, data: &[u8]) -> Vec<u8> {
    let mut bytes = vec![0];
    CustomSection {
        name: Cow::Borrowed(name),
        data: Cow::Borrowed(data),
    }
    .encode(&mut bytes);
    bytes
}

fn tiny_module() -> Vec<u8> {
    let mut module = Module::new();
    module.section(&CustomSection {
        name: Cow::Borrowed("before"),
        data: Cow::Borrowed(b"untouched"),
    });
    let mut types = TypeSection::new();
    types.ty().function([], [ValType::I32]);
    module.section(&types);
    let mut functions = FunctionSection::new();
    functions.function(0);
    module.section(&functions);
    let mut exports = ExportSection::new();
    exports.export("answer", ExportKind::Func, 0);
    module.section(&exports);
    let mut code = CodeSection::new();
    let mut function = Function::new([]);
    function.instruction(&Instruction::I32Const(42));
    function.instruction(&Instruction::End);
    code.function(&function);
    module.section(&code);
    module.finish()
}

#[test]
fn round_trip_borrows_metadata_and_preserves_surrounding_sections() {
    let original = tiny_module();
    let metadata = b"opaque\0\xff bytes";
    let mut bundled = append(&original, metadata).unwrap();
    assert_eq!(&bundled[..original.len()], original);
    let after = custom("after", b"also untouched");
    bundled.extend_from_slice(&after);
    let allocation = allocation_counter::measure(|| {
        let found = read(&bundled).unwrap().unwrap();
        assert_eq!(found.metadata, metadata);
        assert_eq!(found.before, original);
        assert_eq!(found.after, after);
        assert_eq!(found.before.as_ptr(), bundled.as_ptr());
        assert_eq!(found.metadata.as_ptr_range().end, found.after.as_ptr());
    });
    assert_eq!(allocation.count_total, 0);

    let engine = wasmi::Engine::default();
    let module = wasmi::Module::new(&engine, bundled.as_slice()).unwrap();
    let mut store = wasmi::Store::new(&engine, ());
    let instance = wasmi::Linker::new(&engine)
        .instantiate_and_start(&mut store, &module)
        .unwrap();
    let answer = instance
        .get_typed_func::<(), i32>(&store, "answer")
        .unwrap();
    assert_eq!(answer.call(&mut store, ()).unwrap(), 42);
}

#[test]
fn metadata_can_precede_executable_sections_and_is_removed_whole() {
    let original = tiny_module();
    let section = custom(SECTION_NAME, b"metadata");
    let mut bundled = original[..8].to_vec();
    bundled.extend_from_slice(&section);
    bundled.extend_from_slice(&original[8..]);
    let found = read(&bundled).unwrap().unwrap();
    assert_eq!(found.metadata, b"metadata");
    assert_eq!(found.before, HEADER);
    assert_eq!(found.after, &original[8..]);
    let stripped: Vec<_> = found.before.iter().chain(found.after).copied().collect();
    assert_eq!(stripped, original);
}

#[test]
fn absent_and_empty_metadata_are_distinct() {
    assert_eq!(read(HEADER), Ok(None));
    let bundled = append(HEADER, b"").unwrap();
    assert_eq!(read(&bundled).unwrap().unwrap().metadata, b"");
}

#[test]
fn duplicate_sections_are_rejected_by_reader_and_writer() {
    let mut bundled = append(HEADER, b"first").unwrap();
    assert_eq!(
        append(&bundled, b"second"),
        Err(BundleError::DuplicateMetadata)
    );
    bundled.extend_from_slice(&custom(SECTION_NAME, b"second"));
    assert_eq!(read(&bundled), Err(BundleError::DuplicateMetadata));
}

#[test]
fn opaque_custom_and_data_contents_are_not_scanned_as_sections() {
    let embedded = custom(SECTION_NAME, b"not a top-level section");
    let mut module = Module::new();
    module.section(&CustomSection {
        name: Cow::Borrowed("other"),
        data: Cow::Borrowed(&embedded),
    });
    let mut data = DataSection::new();
    data.passive(embedded.iter().copied());
    module.section(&data);
    assert_eq!(read(&module.finish()), Ok(None));
}

#[test]
fn module_header_rejects_truncation_other_versions_and_components() {
    for length in 0..HEADER.len() {
        assert_eq!(read(&HEADER[..length]), Err(BundleError::InvalidHeader));
    }
    for header in [b"\0bad\x01\0\0\0", b"\0asm\x02\0\0\0", b"\0asm\x0d\0\x01\0"] {
        assert_eq!(read(header), Err(BundleError::InvalidHeader));
        assert_eq!(append(header, b"metadata"), Err(BundleError::InvalidHeader));
    }
}

#[test]
fn malformed_lengths_names_and_trailing_sections_are_rejected() {
    let cases: &[(&[u8], BundleError)] = &[
        (&[0], BundleError::TruncatedLeb),
        (&[0, 0x80], BundleError::TruncatedLeb),
        (&[1, 0xff, 0xff, 0xff, 0xff, 0x10], BundleError::InvalidLeb),
        (
            &[1, 0x80, 0x80, 0x80, 0x80, 0x80, 0],
            BundleError::InvalidLeb,
        ),
        (
            &[1, 0xff, 0xff, 0xff, 0xff, 0x0f],
            BundleError::TruncatedSection,
        ),
        (&[1, 1], BundleError::TruncatedSection),
        (&[0, 0], BundleError::TruncatedLeb),
        (&[0, 1, 0x80], BundleError::TruncatedLeb),
        (&[0, 1, 1], BundleError::TruncatedCustomName),
        (&[0, 2, 1, 0xff], BundleError::InvalidCustomName),
        (
            &[0, 6, 0xff, 0xff, 0xff, 0xff, 0x10, 0],
            BundleError::InvalidLeb,
        ),
        (
            &[0, 5, 0xff, 0xff, 0xff, 0xff, 0x0f],
            BundleError::TruncatedCustomName,
        ),
    ];
    for &(suffix, error) in cases {
        let mut module = HEADER.to_vec();
        module.extend_from_slice(suffix);
        assert_eq!(read(&module), Err(error), "suffix {suffix:?}");
        assert_eq!(append(&module, b"metadata"), Err(error));
    }
    let mut valid_then_bad = append(HEADER, b"metadata").unwrap();
    valid_then_bad.extend_from_slice(&[0, 1, 1]);
    assert_eq!(read(&valid_then_bad), Err(BundleError::TruncatedCustomName));
}

#[test]
fn padded_u32_lengths_are_valid_but_remain_bounded() {
    let mut module = HEADER.to_vec();
    module.extend_from_slice(&[0, 0x85, 0x80, 0x80, 0x80, 0, 0x80, 0x80, 0x80, 0x80, 0]);
    assert_eq!(read(&module), Ok(None));
    assert!(append(&module, b"metadata").is_ok());
}

#[test]
fn size_limits_exclude_the_whole_metadata_section_but_include_other_custom_sections() {
    // One bounded allocation exercises the actual production limits without
    // making copies of either large payload.
    let mut module = Vec::with_capacity(MAX_BUNDLE_BYTES + 1);
    module.extend_from_slice(HEADER);
    module.push(0);
    let payload_length = MAX_CODE_BYTES - HEADER.len() - 1 - 4;
    write_u32(payload_length as u32, &mut module);
    module.push(0); // Empty unrelated custom name.
    module.resize(MAX_CODE_BYTES, 0);
    assert_eq!(read(&module), Ok(None));
    module.push(0);
    let metadata_payload_length = 1 + SECTION_NAME.len() + MAX_METADATA_BYTES;
    write_u32(metadata_payload_length as u32, &mut module);
    module.push(SECTION_NAME.len() as u8);
    module.extend_from_slice(SECTION_NAME.as_bytes());
    let metadata_start = module.len();
    module.resize(metadata_start + MAX_METADATA_BYTES, 0);
    let found = read(&module).unwrap().unwrap();
    assert_eq!(found.before.len(), MAX_CODE_BYTES);
    assert_eq!(found.metadata.len(), MAX_METADATA_BYTES);

    module.extend_from_slice(&[0, 1, 0]);
    assert_eq!(read(&module), Err(BundleError::ModuleTooLarge));
    module.extend_from_slice(&[0, 1, 0]);
    assert!(module.len() > MAX_BUNDLE_BYTES);
    assert_eq!(read(&module), Err(BundleError::ModuleTooLarge));

    module.truncate(metadata_start + MAX_METADATA_BYTES);
    let mut encoded_length = Vec::new();
    write_u32((metadata_payload_length + 1) as u32, &mut encoded_length);
    module[MAX_CODE_BYTES + 1..MAX_CODE_BYTES + 5].copy_from_slice(&encoded_length);
    module.push(0);
    assert_eq!(read(&module), Err(BundleError::MetadataTooLarge));
    assert_eq!(
        append(HEADER, &module[metadata_start..]),
        Err(BundleError::MetadataTooLarge)
    );
}
