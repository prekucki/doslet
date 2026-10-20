use super::*;

fn multiply(target: Register, immediate: bool, displacement: u32) -> Operation {
    Operation::SignedMultiplyMemory {
        target,
        address: address(displacement),
        source: if immediate {
            Source::Immediate(target.width().mask())
        } else {
            Source::Register(target)
        },
    }
}

#[test]
fn memory_signed_multiply_all_targets_code_sizes_boundaries_and_random_match_cpu() {
    for target in ALIASES.into_iter().take(16) {
        for immediate in [false, true] {
            for code32 in [false, true] {
                differential_mode(
                    multiply(target, immediate, 0x503),
                    code32,
                    descriptor(),
                    80,
                    code32,
                );
            }
        }
    }
}

#[test]
fn memory_signed_multiply_snapshots_aliased_base_index_address16_and_segment_sources() {
    for target in ALIASES.into_iter().take(16) {
        let base = ALIASES[target.index() as usize];
        for immediate in [false, true] {
            let source = if immediate {
                Source::Immediate(target.width().mask() - 1)
            } else {
                Source::Register(target)
            };
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
                Operation::SignedMultiplyMemory {
                    target,
                    source,
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
                    Operation::SignedMultiplyMemory {
                        target,
                        source,
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
                let a = MemoryAddress {
                    segment,
                    ..address(0x103)
                };
                differential(
                    Operation::SignedMultiplyMemory {
                        target,
                        source,
                        address: a,
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
fn memory_signed_multiply_vga_linear_planar_chain4_compare_has_exact_read_effects() {
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
                        for immediate in [false, true] {
                            differential(multiply(target, immediate, 0xa0003), true, d, 8);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn signed_multiply_bad_complete_spans_decline_before_admission_or_effects() {
    for target in [Register::Ax, Register::Eax] {
        let bytes = target.width().bits() / 8;
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
                    &single(multiply(target, false, offset)),
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
                    "target={target:?} case={case} inline={inline}"
                );
            }
        }
    }
}

#[test]
fn signed_multiply_admission_horizon_and_source_guards_refuse_before_device_effects() {
    for target in [Register::Ax, Register::Eax] {
        for immediate in [false, true] {
            for case in 0..6 {
                let inline = case != 0;
                let mut runner = Runner::with_options(
                    &single(multiply(target, immediate, 0xa0003)),
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
                assert_eq!(runner.bytes(), before, "target={target:?} case={case}");
            }
        }
    }
}

#[test]
fn memory_signed_multiply_validation_rejects_disabled_mixed_hot_widths_and_addresses() {
    let valid = multiply(Register::Eax, false, 0x503);
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
        for source in ALIASES {
            if target.width() != Width::Byte && target.width() == source.width() {
                continue;
            }
            assert_eq!(
                single(Operation::SignedMultiplyMemory {
                    target,
                    address: address(0),
                    source: Source::Register(source)
                })
                .compile(&options(false))
                .unwrap_err(),
                CompileError::WidthMismatch(RegionId(1))
            );
        }
    }
    assert_eq!(
        single(Operation::SignedMultiplyMemory {
            target: Register::Ax,
            address: address(0),
            source: Source::Immediate(0xffff_ffff)
        })
        .compile(&options(false))
        .unwrap_err(),
        CompileError::ImmediateOutOfRange(RegionId(1))
    );
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
            single(Operation::SignedMultiplyMemory {
                target: Register::Eax,
                address: a,
                source: Source::Immediate(1)
            })
            .compile(&options(false))
            .unwrap_err(),
            CompileError::InvalidMemoryAddress(RegionId(1))
        );
    }
}

#[test]
fn memory_signed_multiply_execution_allocates_nothing_after_setup() {
    for target in [Register::Ax, Register::Eax] {
        for immediate in [false, true] {
            let mut runner = Runner::new(&single(multiply(target, immediate, 0xa0003)), false);
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
