use super::*;
use crate::{
    ObjectAddress,
    binding::{BindingLimits, DosFlatBindingPlan},
    cfg::{CfgLimits, InstructionFlow, StaticCfg},
};
use doslet_executable::CoffLimits;

#[path = "../../../../tests/fixtures/djgpp_coff.rs"]
mod fixture;
use fixture::{STUB_BYTES, TEXT_VADDR, data_vaddr, djgpp_exe};

/// CALL +1; RET; INC EAX; RET. The entry calls a function six bytes in.
const TEXT: [u8; 8] = [0xe8, 1, 0, 0, 0, 0xc3, 0x40, 0xc3];

fn image(bytes: &[u8]) -> CoffImage<'_> {
    CoffImage::discover(bytes, CoffLimits::default()).unwrap()
}

#[test]
fn sections_become_objects_at_their_virtual_addresses() {
    let bytes = djgpp_exe(&TEXT, &[7; 9], 0x30, 0, &[]);
    let symbolic = SymbolicImage::from_coff(&image(&bytes), ImageLimits::default()).unwrap();
    assert_eq!(symbolic.format(), ImageFormat::Coff);
    assert_eq!(symbolic.relocations().len(), 0);
    let objects = symbolic.objects();
    assert_eq!(objects.len(), 3);
    let text = &objects[0];
    assert!(text.metadata().is_executable() && !text.metadata().is_writable());
    assert_eq!(text.default_bitness(), DefaultBitness::Bits32);
    assert_eq!(text.metadata().preferred_base, TEXT_VADDR);
    assert_eq!(text.bytes(), TEXT);
    assert_eq!(
        text.file_backed_spans(),
        [FileBackedSpan {
            object_offset: 0,
            file_offset: STUB_BYTES + 0xa8,
            byte_len: TEXT.len() as u32,
        }]
    );
    assert!(objects[1].metadata().is_writable() && !objects[1].metadata().is_executable());
    assert_eq!(objects[1].metadata().preferred_base, data_vaddr(TEXT.len()));
    assert_eq!(objects[1].bytes(), [7; 9]);
    // BSS is zero-filled storage, not file data.
    assert_eq!(objects[2].bytes(), [0; 0x30]);
    assert!(objects[2].file_backed_spans().is_empty());
    let entry = symbolic.header().entry.unwrap();
    assert_eq!((entry.object_number, entry.offset), (1, 0));
}

#[test]
fn live_text_replaces_patched_code_of_the_same_size() {
    let bytes = djgpp_exe(&TEXT, &[], 0, 0, &[]);
    let coff = image(&bytes);
    let mut live = TEXT;
    live[6] = 0x48; // DEC EAX, as a program patching itself would leave it.
    let symbolic =
        SymbolicImage::from_coff_with_text(&coff, Some(&live), ImageLimits::default()).unwrap();
    assert_eq!(symbolic.objects()[0].bytes(), live);
    assert_eq!(
        SymbolicImage::from_coff_with_text(&coff, Some(&live[..7]), ImageLimits::default())
            .unwrap_err(),
        ImageError::UnsupportedCoff("live text size")
    );
}

#[test]
fn relocations_and_budget_are_explicit_failures() {
    let mut bytes = djgpp_exe(&TEXT, &[], 0, 0, &[]);
    // One relocation in the text section header (s_nreloc).
    bytes[STUB_BYTES + 48 + 32] = 1;
    assert_eq!(
        SymbolicImage::from_coff(&image(&bytes), ImageLimits::default()).unwrap_err(),
        ImageError::UnsupportedCoff("section relocations")
    );
    let bytes = djgpp_exe(&TEXT, &[], 0x1000, 0, &[]);
    let limits = ImageLimits {
        max_owned_bytes: 0x800,
        ..ImageLimits::default()
    };
    assert!(matches!(
        SymbolicImage::from_coff(&image(&bytes), limits),
        Err(ImageError::OwnedByteLimit(_))
    ));
}

#[test]
fn offset_space_bindings_give_the_cfg_actual_eip_values() {
    let bytes = djgpp_exe(&TEXT, &[1, 2, 3, 4], 0x10, 0, &[]);
    let symbolic = SymbolicImage::from_coff(&image(&bytes), ImageLimits::default()).unwrap();
    let bindings = coff_offset_bindings(&symbolic, 0xaf);
    assert_eq!(bindings.len(), 3);
    assert_eq!(bindings[0].linear_base, TEXT_VADDR);
    let plan = DosFlatBindingPlan::new(&symbolic, &bindings, BindingLimits::default()).unwrap();
    assert!(plan.patches().is_empty());
    let graph = StaticCfg::analyze_bound(&plan, &[], CfgLimits::default()).unwrap();
    let nodes: Vec<_> = graph
        .nodes()
        .iter()
        .map(|node| (node.address, node.flow))
        .collect();
    let at = |offset| ObjectAddress {
        object_number: 1,
        offset,
    };
    assert_eq!(
        nodes,
        [
            (at(0), InstructionFlow::DirectCall),
            (at(5), InstructionFlow::Return),
            (at(6), InstructionFlow::Next),
            (at(7), InstructionFlow::Return),
        ]
    );
    // The callee's EIP is its section offset plus the section address.
    let callee = plan.object_binding(1).unwrap().linear_base + 6;
    assert_eq!(callee, TEXT_VADDR + 6);
}
