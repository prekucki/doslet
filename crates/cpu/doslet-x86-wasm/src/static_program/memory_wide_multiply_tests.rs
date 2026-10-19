use super::*;

const WIDTHS: [Width; 3] = [Width::Byte, Width::Word, Width::Dword];

fn multiply(width: Width, signed: bool, displacement: u32) -> Operation {
    Operation::WideMultiplyMemory {
        signed,
        width,
        address: address(displacement),
    }
}

#[test]
fn memory_wide_multiply_all_widths_signedness_code_sizes_boundaries_and_random_match_cpu() {
    for width in WIDTHS {
        for signed in [false, true] {
            for code32 in [false, true] {
                for inline in [false, true] {
                    differential_mode(
                        multiply(width, signed, 0x503),
                        inline,
                        descriptor(),
                        80,
                        code32,
                    );
                }
            }
        }
    }
}

#[test]
fn memory_wide_multiply_snapshots_accumulator_high_destination_base_index_and_segments() {
    for width in WIDTHS {
        for signed in [false, true] {
            // EAX/EDX as base and index must be consumed before either output.
            for base in [Register::Eax, Register::Edx] {
                for index in [Register::Eax, Register::Edx] {
                    let a = MemoryAddress {
                        base: Some(base),
                        index: Some(index),
                        scale: 4,
                        ..address(
                            0x503u32
                                .wrapping_sub(if base == index {
                                    INDEX_VALUE
                                } else {
                                    BASE_VALUE
                                })
                                .wrapping_sub(INDEX_VALUE * 4),
                        )
                    };
                    differential(
                        Operation::WideMultiplyMemory {
                            width,
                            signed,
                            address: a,
                        },
                        true,
                        descriptor(),
                        8,
                    );
                }
            }
            let a = MemoryAddress {
                base: Some(Register::Ebx),
                index: Some(Register::Esi),
                address16: true,
                ..address(0x503u32.wrapping_sub(BASE_VALUE).wrapping_sub(INDEX_VALUE))
            };
            for code32 in [false, true] {
                differential_mode(
                    Operation::WideMultiplyMemory {
                        width,
                        signed,
                        address: a,
                    },
                    false,
                    descriptor(),
                    8,
                    code32,
                );
            }
            for segment in [
                Segment::Es,
                Segment::Ss,
                Segment::Ds,
                Segment::Fs,
                Segment::Gs,
            ] {
                let mut d = descriptor();
                d.windows[0][segment as usize].base = 0x400;
                differential(
                    Operation::WideMultiplyMemory {
                        width,
                        signed,
                        address: MemoryAddress {
                            segment,
                            ..address(0x103)
                        },
                    },
                    true,
                    d,
                    8,
                );
            }
        }
    }
}

#[test]
fn memory_wide_multiply_vga_linear_planar_chain4_compare_has_exact_operand_read_effects() {
    for kind in [DeviceReadKind::Linear, DeviceReadKind::FourLanes] {
        for chain4 in [false, true] {
            for compare in [false, true] {
                let mut d = device(kind);
                d.device.chain4 = chain4;
                d.device.compare_enabled = compare;
                d.device.compare = 5;
                d.device.care = 13;
                d.device.base_offset = if kind == DeviceReadKind::Linear {
                    17
                } else {
                    0
                };
                for plane in 0..4 {
                    d.device.read_plane = plane;
                    for width in WIDTHS {
                        for signed in [false, true] {
                            differential(multiply(width, signed, 0xa0003), true, d, 8);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn wide_multiply_preflight_accepts_exact_source_span_independently_of_product_width() {
    for width in WIDTHS {
        let bytes = width.bits() / 8;
        for signed in [false, true] {
            for device_read in [false, true] {
                let offset = if device_read { 0xa0003 } else { 0x503 };
                let mut d = if device_read {
                    device(DeviceReadKind::Linear)
                } else {
                    descriptor()
                };
                d.windows[0][Segment::Ds as usize].last = offset + bytes - 1;
                if device_read {
                    d.device.guest_length = 3 + bytes;
                    d.device.backing_length = 3 + bytes;
                } else {
                    d.ram.length = 3 + bytes;
                }
                for inline in [false, true] {
                    let mut runner = Runner::with_options(
                        &single(multiply(width, signed, offset)),
                        CompileOptions {
                            direct_memory: true,
                            runtime_admission: !inline,
                            inline_admission: inline,
                            ..CompileOptions::default()
                        },
                    );
                    read_proofs(&mut runner, &mut d);
                    runner.set(initial(), &d);
                    let pointer = if device_read { VRAM + 3 } else { RAM + 3 };
                    runner.bytes()[pointer..pointer + bytes as usize].fill(0);
                    assert_eq!(runner.call(), abi::EXIT_BUDGET);
                    assert_eq!(
                        runner.frame().gpr[0],
                        if width == Width::Dword {
                            0
                        } else {
                            0xaabb_0000
                        }
                    );
                    assert_eq!(
                        runner.frame().gpr[2],
                        match width {
                            Width::Byte => 0xaabb_ccdd,
                            Width::Word => 0xaabb_0000,
                            Width::Dword => 0,
                        }
                    );
                    assert_eq!(runner.frame().eflags, initial().eflags & !0x801);
                    assert_eq!(
                        runner.effects().device_reads,
                        if device_read { u64::from(bytes) } else { 0 }
                    );
                    assert_eq!(runner.effects().ram_writes, 0);
                }
            }
        }
    }
}

#[test]
fn wide_multiply_bad_complete_spans_decline_before_admission_or_effects() {
    for width in WIDTHS {
        let bytes = width.bits() / 8;
        for signed in [false, true] {
            for case in 0..13 {
                let mut d = device(DeviceReadKind::FourLanes);
                let mut offset = 0xa0003;
                match case {
                    0 => d.windows[0][Segment::Ds as usize].valid = false,
                    1 => d.windows[0][Segment::Ds as usize].last = offset + bytes - 2,
                    2 => d.device.guest_length = 3 + bytes - 1,
                    3 => d.device.backing_length = (3 + bytes) * 4 - 1,
                    4 => d.device.pointer = 65535,
                    5 => {
                        offset = 0x503;
                        d.ram.pointer = 65534;
                    }
                    6 => offset = 0x2500 - bytes + 1,
                    7 => offset = u32::MAX,
                    8 => d.device.kind = DeviceReadKind::Disabled,
                    9 => d.device.read_plane = 4,
                    10 => d.windows[0][Segment::Ds as usize].first = offset + 1,
                    11 => {
                        offset = 0x503;
                        d.ram.length = 3 + bytes - 1;
                    }
                    12 => {
                        offset = 0x503;
                        d.ram.excluded_start = offset + bytes - 1;
                        d.ram.excluded_length = 1;
                    }
                    _ => unreachable!(),
                }
                for inline in [false, true] {
                    let mut runner = Runner::with_options(
                        &single(multiply(width, signed, offset)),
                        CompileOptions {
                            direct_memory: true,
                            runtime_admission: !inline,
                            inline_admission: inline,
                            ..CompileOptions::default()
                        },
                    );
                    read_proofs(&mut runner, &mut d);
                    runner.set(initial(), &d);
                    runner.bytes()[VRAM..VRAM + 64].fill(0xa5);
                    let before = runner.bytes().to_vec();
                    refused(&mut runner, initial(), 0);
                    assert_eq!(
                        runner.bytes(),
                        before,
                        "width={width:?} signed={signed} case={case} inline={inline}"
                    );
                }
            }
        }
    }
}

#[test]
fn wide_multiply_admission_horizon_and_source_guards_refuse_before_device_effects() {
    for width in WIDTHS {
        for signed in [false, true] {
            for case in 0..6 {
                let inline = case != 0;
                let mut runner = Runner::with_options(
                    &single(multiply(width, signed, 0xa0003)),
                    CompileOptions {
                        direct_memory: true,
                        runtime_admission: !inline,
                        inline_admission: inline,
                        ..CompileOptions::default()
                    },
                );
                let mut d = device(DeviceReadKind::FourLanes);
                read_proofs(&mut runner, &mut d);
                match case {
                    0 => runner.store.data_mut().refuse = true,
                    1 => d.inline_grant = 0,
                    2 => d.region_guards_length = 1,
                    3 => runner.bytes()[GENERATIONS] ^= 1,
                    4 => runner.bytes()[GUARDS + memory::REGION_GUARD_BYTES] = 0,
                    5 => d.ram.generations_pointer = 65535,
                    _ => unreachable!(),
                }
                runner.set(initial(), &d);
                runner.bytes()[VRAM..VRAM + 64].fill(0xa5);
                let before = runner.bytes().to_vec();
                refused(&mut runner, initial(), usize::from(!inline));
                assert_eq!(
                    runner.bytes(),
                    before,
                    "width={width:?} signed={signed} case={case}"
                );
            }
        }
    }
}

#[test]
fn memory_wide_multiply_validation_rejects_disabled_mixed_hot_and_invalid_addresses() {
    let valid = multiply(Width::Dword, false, 0x503);
    let mut p = single(valid);
    assert_eq!(
        p.compile(&CompileOptions::default()).unwrap_err(),
        CompileError::DirectMemoryDisabled(RegionId(1))
    );
    p.regions[0].operations.push(Operation::Nop);
    assert_eq!(
        p.compile(&options(false)).unwrap_err(),
        CompileError::MixedMemoryRegion(RegionId(1))
    );
    p.regions[0].operations.pop();
    for terminator in [
        Terminator::Jump(RegionId(1)),
        Terminator::Branch {
            condition: Condition::Equal,
            taken: RegionId(1),
            not_taken: RegionId(1),
        },
    ] {
        p.regions[0].terminator = terminator;
        assert_eq!(
            p.compile(&options(false)).unwrap_err(),
            CompileError::MixedMemoryRegion(RegionId(1))
        );
    }
    p.regions[0].terminator = Terminator::Fallthrough(RegionId(1));
    p.regions[0].accounting = Accounting::UncountedHot;
    assert_eq!(
        p.compile(&options(false)).unwrap_err(),
        CompileError::MemoryUncountedHot(RegionId(1))
    );
    for width in WIDTHS {
        for signed in [false, true] {
            for a in [
                MemoryAddress {
                    scale: 3,
                    ..address(0)
                },
                MemoryAddress {
                    base: Some(Register::Ax),
                    ..address(0)
                },
                MemoryAddress {
                    index: Some(Register::Ah),
                    ..address(0)
                },
                MemoryAddress {
                    address16: true,
                    scale: 2,
                    ..address(0)
                },
            ] {
                assert_eq!(
                    single(Operation::WideMultiplyMemory {
                        width,
                        signed,
                        address: a
                    })
                    .compile(&options(false))
                    .unwrap_err(),
                    CompileError::InvalidMemoryAddress(RegionId(1))
                );
            }
        }
    }
}

#[test]
fn memory_wide_multiply_execution_allocates_nothing_after_setup() {
    for width in WIDTHS {
        for signed in [false, true] {
            let mut runner = Runner::new(&single(multiply(width, signed, 0xa0003)), false);
            let d = device(DeviceReadKind::FourLanes);
            runner.bytes()[VRAM..VRAM + 64].fill(0x80);
            runner.set(initial(), &d);
            assert_eq!(runner.call(), abi::EXIT_BUDGET);
            let allocation = allocation_counter::measure(|| {
                for _ in 0..1000 {
                    runner.set(initial(), &d);
                    assert_eq!(runner.call(), abi::EXIT_BUDGET);
                }
            });
            assert_eq!(allocation.count_total, 0);
            assert_eq!(allocation.bytes_total, 0);
        }
    }
}
