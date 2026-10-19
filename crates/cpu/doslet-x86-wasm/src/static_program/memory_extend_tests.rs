use super::*;

fn extension(target: Register, width: Width, signed: bool, displacement: u32) -> Operation {
    Operation::ExtendMemory {
        target,
        width,
        signed,
        address: address(displacement),
    }
}

#[test]
fn memory_extensions_all_targets_widths_signedness_code_sizes_and_values_match_cpu() {
    for target in ALIASES.into_iter().take(16) {
        for width in [Width::Byte, Width::Word] {
            for signed in [false, true] {
                for code32 in [false, true] {
                    differential_mode(
                        extension(target, width, signed, 0x503),
                        code32,
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
fn memory_extensions_snapshot_aliased_base_index_and_address16_before_target_write() {
    for target in ALIASES.into_iter().take(16) {
        let base = ALIASES[target.index() as usize];
        for width in [Width::Byte, Width::Word] {
            for signed in [false, true] {
                let a = MemoryAddress {
                    base: Some(base),
                    index: Some(Register::Edx),
                    scale: 4,
                    ..address(
                        0x503u32
                            .wrapping_sub(if base == Register::Edx {
                                INDEX_VALUE
                            } else {
                                BASE_VALUE
                            })
                            .wrapping_sub(INDEX_VALUE * 4),
                    )
                };
                differential(
                    Operation::ExtendMemory {
                        target,
                        width,
                        signed,
                        address: a,
                    },
                    true,
                    descriptor(),
                    8,
                );
                let a = MemoryAddress {
                    base: Some(Register::Ebx),
                    index: Some(Register::Esi),
                    address16: true,
                    ..address(0x503u32.wrapping_sub(BASE_VALUE).wrapping_sub(INDEX_VALUE))
                };
                for code32 in [false, true] {
                    differential_mode(
                        Operation::ExtendMemory {
                            target,
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
            }
        }
    }
}

#[test]
fn memory_extensions_linear_planar_chain4_compare_have_exact_source_read_effects() {
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
                    for target in [Register::Ax, Register::Eax, Register::Di, Register::Edi] {
                        for width in [Width::Byte, Width::Word] {
                            for signed in [false, true] {
                                differential(extension(target, width, signed, 0xa0003), true, d, 8);
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn extension_preflight_accepts_exact_source_span_even_when_target_would_cross() {
    for width in [Width::Byte, Width::Word] {
        let bytes = width.bits() / 8;
        for device_read in [false, true] {
            let offset = if device_read { 0xa0003 } else { 0x503 };
            let op = extension(Register::Eax, width, true, offset);
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
                    &single(op),
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
                runner.bytes()[pointer..pointer + bytes as usize].fill(0x80);
                assert_eq!(runner.call(), abi::EXIT_BUDGET);
                assert_eq!(
                    runner.frame().gpr[0],
                    if width == Width::Byte {
                        0xffff_ff80
                    } else {
                        0xffff_8080
                    }
                );
                assert_eq!(runner.frame().eflags, initial().eflags);
                assert_eq!(
                    runner.effects().device_reads,
                    if device_read { u64::from(bytes) } else { 0 }
                );
            }
        }
    }
}

#[test]
fn extension_bad_complete_spans_decline_before_admission_or_effects() {
    for width in [Width::Byte, Width::Word] {
        let bytes = width.bits() / 8;
        for case in 0..10 {
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
                6 => {
                    offset = 0x2500 - bytes + 1;
                }
                7 => {
                    offset = u32::MAX;
                }
                8 => d.device.kind = DeviceReadKind::Disabled,
                9 => d.device.read_plane = 4,
                _ => unreachable!(),
            }
            for inline in [false, true] {
                let mut runner = Runner::with_options(
                    &single(extension(Register::Eax, width, true, offset)),
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
                    "width={width:?} case={case} inline={inline}"
                );
            }
        }
    }
}

#[test]
fn extension_admission_horizon_and_source_proofs_decline_before_device_effects() {
    for signed in [false, true] {
        for width in [Width::Byte, Width::Word] {
            for case in 0..5 {
                let inline = case != 0;
                let mut runner = Runner::with_options(
                    &single(extension(Register::Eax, width, signed, 0xa0003)),
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
                    _ => unreachable!(),
                }
                runner.set(initial(), &d);
                runner.bytes()[VRAM..VRAM + 64].fill(0xa5);
                let before = runner.bytes().to_vec();
                refused(&mut runner, initial(), usize::from(!inline));
                assert_eq!(runner.bytes(), before, "case={case}");
            }
        }
    }
}

#[test]
fn extension_memory_validation_rejects_disabled_mixed_hot_widths_and_addresses() {
    let valid = extension(Register::Eax, Width::Byte, true, 0x503);
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
    for target in ALIASES {
        for width in [Width::Byte, Width::Word, Width::Dword] {
            if target.width() != Width::Byte && width != Width::Dword {
                continue;
            }
            for signed in [false, true] {
                assert_eq!(
                    single(extension(target, width, signed, 0))
                        .compile(&options(false))
                        .unwrap_err(),
                    CompileError::WidthMismatch(RegionId(1))
                );
            }
        }
    }
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
            single(Operation::ExtendMemory {
                target: Register::Eax,
                width: Width::Byte,
                address: a,
                signed: true
            })
            .compile(&options(false))
            .unwrap_err(),
            CompileError::InvalidMemoryAddress(RegionId(1))
        );
    }
}

#[test]
fn memory_extension_execution_allocates_nothing_after_setup() {
    for width in [Width::Byte, Width::Word] {
        for signed in [false, true] {
            let mut runner = Runner::new(
                &single(extension(Register::Ax, width, signed, 0xa0003)),
                false,
            );
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
