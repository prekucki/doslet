use super::*;
use doslet_x86_wasm::static_program::memory::{Descriptor, Effects, RamDescriptor, SegmentBounds};

#[test]
fn original_truncated_imul_encodings_keep_operand_order_width_and_overflow() {
    for (bytes, memory, target, value, overflow) in [
        (vec![0x0f, 0xaf, 0xc1], false, 0, 0x8000_0000, true),
        (vec![0x0f, 0xaf, 0xc3], false, 0, 0x8000_0000, true),
        (vec![0x0f, 0xaf, 0xd9], false, 3, 0xffff_fffd, false),
        (vec![0x66, 0x0f, 0xaf, 0xd1], false, 2, 0x1111_8000, true),
        (vec![0x69, 0xd9, 2, 0, 0, 0], false, 3, 0xffff_fffe, false),
        (vec![0x6b, 0xd9, 0x80], false, 3, 0x80, false),
        (vec![0x66, 0x6b, 0xd2, 0xff], false, 2, 0x1111_8000, true),
        (
            vec![0x66, 0x69, 0xd3, 0xff, 0x7f],
            false,
            2,
            0x1111_7ffd,
            true,
        ),
        (
            vec![0x0f, 0xaf, 0x2d, 0, 5, 0, 0],
            true,
            5,
            0xffff_fb00,
            false,
        ),
        (vec![0x0f, 0xaf, 0x6d, 0], true, 5, 0xffff_fb00, false),
        (
            vec![0x69, 0x1d, 0, 5, 0, 0, 0, 0, 0, 0x80],
            true,
            3,
            0x8000_0000,
            true,
        ),
        (
            vec![0x66, 0x6b, 0x15, 0, 5, 0, 0, 0xff],
            true,
            2,
            0x1111_0001,
            false,
        ),
        (
            vec![0x66, 0x67, 0x6b, 0x16, 0, 5, 0x80],
            true,
            2,
            0x1111_0080,
            false,
        ),
    ] {
        let mut code = bytes.clone();
        code.push(0xf4);
        let image = image(&code, DefaultBitness::Bits32);
        let binding = binding(&image, 0x1000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let prepared = PreparedProgram::from_bound_cfg(
            &graph,
            &binding,
            LoweringLimits {
                direct_memory: memory,
                ..LoweringLimits::default()
            },
        )
        .unwrap();
        assert_eq!(
            prepared.report().memory_multiply,
            usize::from(memory),
            "{bytes:02x?}"
        );
        assert_eq!(
            prepared.report().register_instructions,
            usize::from(!memory),
            "{bytes:02x?}"
        );
        let mut gpr = [0x8000_0000, u32::MAX, 0x1111_8000, 3, 0, 0x500, 0, 0];
        let (frame, calls) = execute_setup(
            &prepared,
            Frame {
                gpr,
                eflags: 0xad7,
                address: address(at(0)),
                remaining_budget: 1,
                ..Frame::default()
            },
            CompileOptions {
                direct_memory: memory,
                ..CompileOptions::default()
            },
            |bytes| {
                bytes[4096..4100].copy_from_slice(&u32::MAX.to_le_bytes());
                Descriptor {
                    ram: RamDescriptor {
                        pointer: 4096,
                        linear_base: 0x500,
                        length: 4,
                        ..RamDescriptor::default()
                    },
                    windows: [[SegmentBounds {
                        valid: true,
                        base: 0,
                        first: 0,
                        last: u32::MAX,
                    }; 6]; 2],
                    ..Descriptor::default()
                }
                .write(&mut bytes[72..])
                .unwrap();
            },
            |bytes| {
                assert_eq!(&bytes[4096..4100], &u32::MAX.to_le_bytes());
                assert_eq!(Effects::read(&bytes[72..]).unwrap().ram_writes, 0);
                assert_eq!(Effects::read(&bytes[72..]).unwrap().device_reads, 0);
            },
        );
        gpr[target] = value;
        assert_eq!(frame.gpr, gpr, "{bytes:02x?}");
        assert_eq!(
            frame.eflags,
            if overflow { 0xad7 } else { 0x2d6 },
            "{bytes:02x?}"
        );
        assert_eq!(frame.charged_work, 1);
        assert_eq!(frame.address, address(at(bytes.len() as u32)));
        assert!(calls.is_empty());
    }
}

#[test]
fn unselected_memory_multiply_operands_still_fall_back_before_effects() {
    for (bytes, direct) in [
        (vec![0xf7, 0x28], false),
        (vec![0xf7, 0x20], false),
        (vec![0x0f, 0xaf, 0x00], false),
    ] {
        let image = image(&bytes, DefaultBitness::Bits32);
        let binding = binding(&image, 0x1000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let prepared = PreparedProgram::from_bound_cfg(
            &graph,
            &binding,
            LoweringLimits {
                direct_memory: direct,
                ..LoweringLimits::default()
            },
        )
        .unwrap();
        let original = Frame {
            gpr: [0x8123_4567; 8],
            eflags: 0xad7,
            address: address(at(0)),
            remaining_budget: 10,
            ..Frame::default()
        };
        let (frame, calls) = execute(&prepared, original);
        assert_eq!(frame, original);
        assert_eq!(calls, [(abi::REASON_FALLBACK, original.address)]);
    }
}

#[test]
fn original_implicit_multiply_encodings_write_full_products_and_preserve_aliases() {
    for bitness in [DefaultBitness::Bits16, DefaultBitness::Bits32] {
        for bits in [8, 16, 32] {
            for signed in [false, true] {
                // AL/AX/EAX, CL/CX/ECX and DL/DX/EDX exercise captured aliases.
                // AH is also an input alias of the combined byte product AX.
                for source in [Some(0u8), Some(1), Some(2), Some(4), None] {
                    if source == Some(4) && bits != 8 {
                        continue;
                    }
                    let mut bytes = Vec::new();
                    if bits != 8 && (bits == 32) != (bitness == DefaultBitness::Bits32) {
                        bytes.push(0x66);
                    }
                    bytes.push(if bits == 8 { 0xf6 } else { 0xf7 });
                    let extension = if signed { 5 } else { 4 } << 3;
                    if let Some(source) = source {
                        bytes.push(0xc0 | extension | source);
                    } else if bitness == DefaultBitness::Bits32 {
                        bytes.push(extension | 5);
                        bytes.extend_from_slice(&0x500u32.to_le_bytes());
                    } else {
                        bytes.push(extension | 6);
                        bytes.extend_from_slice(&0x500u16.to_le_bytes());
                    }
                    let length = bytes.len() as u32;
                    let instruction = bytes.clone();
                    bytes.push(0xf4);
                    let image = image(&bytes, bitness);
                    let binding = binding(&image, 0x1000);
                    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
                    let memory = source.is_none();
                    let prepared = PreparedProgram::from_bound_cfg(
                        &graph,
                        &binding,
                        LoweringLimits {
                            direct_memory: memory,
                            ..LoweringLimits::default()
                        },
                    )
                    .unwrap();
                    assert_eq!(prepared.report().memory_multiply, usize::from(memory));
                    assert_eq!(
                        prepared.report().register_instructions,
                        usize::from(!memory)
                    );
                    let mut gpr = [0x89ab_80f3, 0xffff_fffd, 0x4567_8001, 0, 0, 0, 0, 0];
                    let mask = (1u64 << bits) - 1;
                    let left = u64::from(gpr[0]) & mask;
                    let right = u64::from(match source {
                        Some(4) => gpr[0] >> 8,
                        Some(index) => gpr[usize::from(index)],
                        None => 0xffff_8003,
                    }) & mask;
                    let sign_extend = |value: u64| {
                        let shift = 128 - bits;
                        (i128::from(value) << shift) >> shift
                    };
                    let product = if signed {
                        sign_extend(left) * sign_extend(right)
                    } else {
                        i128::from(left) * i128::from(right)
                    };
                    let overflow = if signed {
                        product != sign_extend((product as u64) & mask)
                    } else {
                        (product as u64) >> bits != 0
                    };
                    let low = (product as u64 & mask) as u32;
                    let high = ((product as u64 >> bits) & mask) as u32;
                    let (frame, calls) = execute_setup(
                        &prepared,
                        Frame {
                            gpr,
                            eflags: 0xad7,
                            address: address(at(0)),
                            remaining_budget: 1,
                            ..Frame::default()
                        },
                        CompileOptions {
                            direct_memory: memory,
                            ..CompileOptions::default()
                        },
                        |bytes| {
                            bytes[4096..4100].copy_from_slice(&0xffff_8003u32.to_le_bytes());
                            Descriptor {
                                ram: RamDescriptor {
                                    pointer: 4096,
                                    linear_base: 0x500,
                                    length: 4,
                                    ..RamDescriptor::default()
                                },
                                windows: [[SegmentBounds {
                                    valid: true,
                                    base: 0,
                                    first: 0,
                                    last: u32::MAX,
                                }; 6]; 2],
                                ..Descriptor::default()
                            }
                            .write(&mut bytes[72..])
                            .unwrap();
                        },
                        |bytes| {
                            assert_eq!(Effects::read(&bytes[72..]).unwrap().ram_writes, 0);
                            assert_eq!(&bytes[4096..4100], &0xffff_8003u32.to_le_bytes());
                        },
                    );
                    match bits {
                        8 => gpr[0] = (gpr[0] & 0xffff_0000) | low | high << 8,
                        16 => {
                            gpr[0] = (gpr[0] & 0xffff_0000) | low;
                            gpr[2] = (gpr[2] & 0xffff_0000) | high;
                        }
                        32 => {
                            gpr[0] = low;
                            gpr[2] = high;
                        }
                        _ => unreachable!(),
                    }
                    assert_eq!(frame.gpr, gpr, "{instruction:02x?}");
                    assert_eq!(frame.eflags, if overflow { 0xad7 } else { 0x2d6 });
                    assert_eq!(frame.charged_work, 1);
                    assert_eq!(frame.address, address(at(length)));
                    assert!(calls.is_empty());
                }
            }
        }
    }
}
