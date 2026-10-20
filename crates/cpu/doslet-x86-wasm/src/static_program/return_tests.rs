//! Actual emitted WASM versus independent RET encodings in the reference CPU.

use super::*;

fn program(width: Width, pop_bytes: u16, target_space: u32) -> Program {
    Program {
        regions: vec![Region {
            id: RegionId(0),
            entry: entry(0),
            operations: vec![],
            terminator: Terminator::Return {
                width,
                pop_bytes,
                target_space,
            },
            accounting: Accounting::Counted,
        }],
    }
}

fn encoding(width: Width, pop: Option<u16>, code32: bool, address_override: bool) -> Vec<u8> {
    let mut code = vec![];
    if (width == Width::Dword) != code32 {
        code.push(0x66);
    }
    if address_override {
        code.push(0x67);
    }
    if let Some(pop) = pop {
        code.push(0xc2);
        code.extend_from_slice(&pop.to_le_bytes());
    } else {
        code.push(0xc3);
    }
    code
}

#[derive(Clone, Copy, Debug)]
struct StackCase {
    mode: StackAddress,
    esp: u32,
    ram_base: u32,
    ss_base: u32,
}

fn differential(width: Width, pop: Option<u16>, code32: bool, prefix67: bool, case: StackCase) {
    let p = program(width, pop.unwrap_or(0), 0x91);
    let code = encoding(width, pop, code32, prefix67);
    let (mut cpu, mut bus) = reference(&code, code32, case.mode, case.ss_base);
    let mut d = stack_descriptor(case.mode, case.ram_base);
    for windows in &mut d.windows {
        windows[Segment::Ss as usize].base = case.ss_base;
    }
    let offset = case.esp
        & if case.mode == StackAddress::Bits16 {
            0xffff
        } else {
            u32::MAX
        };
    let linear = case.ss_base.wrapping_add(offset);
    let start = RAM + (linear - case.ram_base) as usize;
    for inline in [false, true] {
        let mut runner = Runner::new(&p, inline);
        for target in [0, 1, 0x8001, 0x1234_5678, u32::MAX] {
            let mut frame = initial();
            frame.gpr[4] = case.esp;
            frame.eflags = 0xa550_0cd5;
            frame.charged_work = 39;
            frame.status = 0x8000_0000;
            prepare(&mut runner, &p, frame, &d);
            runner.bytes()[start..start + 4].copy_from_slice(&target.to_le_bytes());
            bus.0[case.ram_base as usize..case.ram_base as usize + RAM_LENGTH]
                .copy_from_slice(&runner.bytes()[RAM..RAM + RAM_LENGTH]);
            cpu.registers.ip = 0x8000;
            cpu.flags = frame.eflags;
            for (r, v) in REGS.into_iter().zip(frame.gpr) {
                cpu.registers.set32(r, v);
            }
            cpu.step(&mut bus).unwrap();
            assert_eq!(runner.call(), abi::EXIT_BUDGET);
            assert_eq!(
                runner.frame(),
                Frame {
                    gpr: core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
                    eflags: cpu.flags,
                    address: Address {
                        space: 0x91,
                        offset: cpu.registers.ip
                    },
                    remaining_budget: 0,
                    charged_work: 40,
                    ..frame
                },
                "{width:?} pop={pop:?} code32={code32} prefix67={prefix67} {case:?}"
            );
            assert_eq!(runner.frame().address.offset, target & width.mask());
            assert_eq!(
                &runner.bytes()[RAM..RAM + RAM_LENGTH],
                &bus.0[case.ram_base as usize..case.ram_base as usize + RAM_LENGTH]
            );
            for page in 0..STACK_PAGES {
                assert_eq!(runner.generation(page), 11 + u64::from(page));
            }
            assert_eq!(
                runner.effects(),
                memory::Effects {
                    device_reads: 0,
                    latches: d.device.latches,
                    ram_writes: 0,
                    inline_reserved: u32::from(inline),
                    inline_remaining: 100 - u32::from(inline),
                    ..memory::Effects::default()
                }
            );
            assert!(runner.store.data().callbacks.is_empty());
            assert_eq!(
                runner.store.data().admissions,
                if inline { vec![] } else { vec![(0, 1)] }
            );
            runner.store.data_mut().admissions.clear();
        }
    }
}

#[test]
fn ret_wasm_matches_cpu_operand_ss_code_and_address_widths_and_unsigned_extra_pop() {
    for width in [Width::Word, Width::Dword] {
        for pop in [None, Some(0), Some(1), Some(u16::MAX)] {
            for code32 in [false, true] {
                for prefix67 in [false, true] {
                    for case in [
                        StackCase {
                            mode: StackAddress::Bits16,
                            esp: 0xdead_0fff,
                            ram_base: RAM_BASE,
                            ss_base: 0,
                        },
                        StackCase {
                            mode: StackAddress::Bits32,
                            esp: 0x1_0fff,
                            ram_base: 0x1_0000,
                            ss_base: 0,
                        },
                    ] {
                        differential(width, pop, code32, prefix67, case);
                    }
                }
            }
        }
    }
}

#[test]
fn ret_pointer_wrap_and_nonzero_ss_bases_match_cpu_without_wrapping_operand_bytes() {
    for width in [Width::Word, Width::Dword] {
        for pop in [None, Some(u16::MAX)] {
            for case in [
                StackCase {
                    mode: StackAddress::Bits16,
                    esp: 0xdead_ffff,
                    ram_base: 0xf000,
                    ss_base: 0,
                },
                StackCase {
                    mode: StackAddress::Bits16,
                    esp: 0xdead_fffe,
                    ram_base: 0xf000,
                    ss_base: 0,
                },
                StackCase {
                    mode: StackAddress::Bits32,
                    esp: 0xffff_fffc,
                    ram_base: RAM_BASE,
                    ss_base: 0x1000,
                },
                StackCase {
                    mode: StackAddress::Bits32,
                    esp: 0x1_0fff,
                    ram_base: RAM_BASE,
                    ss_base: 0xffff_0000,
                },
            ] {
                differential(width, pop, true, true, case);
            }
        }
    }
}

fn with_target(width: Width, target: u32) -> (Program, EntryAlias) {
    let mut p = program(width, 3, 0x91);
    p.regions.push(Region {
        id: RegionId(91),
        entry: Address {
            space: 0x17,
            offset: 0x1234,
        },
        operations: vec![Operation::Move {
            target: Register::Edx,
            source: Source::Immediate(0x7788_9900),
        }],
        terminator: Terminator::Fallback(Address {
            space: 0x81,
            offset: 0x8182_8384,
        }),
        accounting: Accounting::Counted,
    });
    let alias = EntryAlias {
        address: Address {
            space: 0x91,
            offset: target,
        },
        target: RegionId(91),
    };
    (p, alias)
}

fn setup(runner: &mut Runner, p: &Program, frame: Frame, target: u32) {
    let mut d = stack_descriptor(StackAddress::Bits32, RAM_BASE);
    d.region_guards_length = p.regions.iter().map(|r| r.id.0).max().unwrap() + 1;
    prepare(runner, p, frame, &d);
    let start = RAM + (frame.gpr[4] - RAM_BASE) as usize;
    runner.bytes()[start..start + 4].copy_from_slice(&target.to_le_bytes());
}

#[test]
fn ret_known_sparse_aliases_execute_without_fallback_and_soft_exit_keeps_exact_target() {
    for inline in [false, true] {
        for width in [Width::Word, Width::Dword] {
            let target = 0x9234_5678 & width.mask();
            let (p, alias) = with_target(width, target);
            for budget in [1, 2] {
                let mut runner = Runner::with_aliases(&p, options(inline), &[alias]);
                let frame = Frame {
                    remaining_budget: budget,
                    ..initial()
                };
                setup(&mut runner, &p, frame, 0x9234_5678);
                let before_ram = runner.bytes()[RAM..RAM + RAM_LENGTH].to_vec();
                assert_eq!(runner.call(), abi::EXIT_BUDGET);
                let mut expected = frame;
                expected.gpr[4] += width.bits() / 8 + 3;
                expected.address = alias.address;
                if budget == 2 {
                    expected.gpr[2] = 0x7788_9900;
                    expected.address = Address {
                        space: 0x81,
                        offset: 0x8182_8384,
                    };
                }
                expected.remaining_budget = 0;
                expected.charged_work = budget;
                assert_eq!(runner.frame(), expected);
                assert_eq!(&runner.bytes()[RAM..RAM + RAM_LENGTH], before_ram);
                assert!(runner.store.data().callbacks.is_empty());
                assert_eq!(
                    runner.store.data().admissions,
                    if inline {
                        vec![]
                    } else if budget == 1 {
                        vec![(0, 1)]
                    } else {
                        vec![(0, 1), (91, 1)]
                    }
                );
                assert_eq!(
                    runner.effects().inline_reserved,
                    if inline { budget as u32 } else { 0 }
                );
            }
        }
    }
}

#[test]
fn ret_unknown_target_falls_back_after_effects_even_when_structural_quota_expires() {
    for inline in [false, true] {
        let p = program(Width::Dword, u16::MAX, 0x91);
        let mut opts = options(inline);
        opts.service_interval_regions = 1;
        let mut runner = Runner::with_aliases(&p, opts, &[]);
        let frame = Frame {
            remaining_budget: 100,
            ..initial()
        };
        setup(&mut runner, &p, frame, 0xf123_4567);
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        let expected = Frame {
            gpr: {
                let mut gpr = frame.gpr;
                gpr[4] += 4 + u32::from(u16::MAX);
                gpr
            },
            address: Address {
                space: 0x91,
                offset: 0xf123_4567,
            },
            charged_work: 1,
            remaining_budget: 99,
            ..frame
        };
        assert_eq!(runner.frame(), expected);
        assert_eq!(
            runner.store.data().callbacks,
            vec![(abi::REASON_FALLBACK, expected)]
        );
    }
}

#[test]
fn ret_alias_target_retains_its_own_source_and_horizon_admission() {
    let (p, alias) = with_target(Width::Dword, 0x1234_5678);
    for inline in [false, true] {
        let mut runner = Runner::with_aliases(&p, options(inline), &[alias]);
        let frame = Frame {
            remaining_budget: 100,
            ..initial()
        };
        setup(&mut runner, &p, frame, alias.address.offset);
        if inline {
            runner.guard(91, RegionGuard::default());
        } else {
            runner.store.data_mut().refuse_region = Some(91);
        }
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        let expected = Frame {
            gpr: {
                let mut gpr = frame.gpr;
                gpr[4] += 7;
                gpr
            },
            address: p.regions[1].entry,
            remaining_budget: 99,
            charged_work: 1,
            ..frame
        };
        assert_eq!(runner.frame(), expected);
        assert_eq!(
            runner.store.data().callbacks,
            vec![(abi::REASON_FALLBACK, expected)]
        );
        assert_eq!(
            runner.store.data().admissions,
            if inline {
                vec![]
            } else {
                vec![(0, 1), (91, 1)]
            }
        );
        assert_eq!(runner.effects().inline_reserved, u32::from(inline));
    }
}

#[test]
fn ret_self_loops_consume_structural_quota_and_continue_with_refreshed_quota() {
    for inline in [false, true] {
        for interval in [1, 3] {
            let p = program(Width::Word, 0xfffe, 0x91); // SP adds 65536, retaining the same stack slot.
            let alias = EntryAlias {
                address: Address {
                    space: 0x91,
                    offset: 0x1234,
                },
                target: RegionId(0),
            };
            let mut opts = options(inline);
            opts.service_interval_regions = interval;
            let mut runner = Runner::with_aliases(&p, opts, &[alias]);
            let frame = Frame {
                remaining_budget: 100,
                ..initial()
            };
            let mut d = stack_descriptor(StackAddress::Bits16, RAM_BASE);
            d.region_guards_length = 1;
            prepare(&mut runner, &p, frame, &d);
            runner.bytes()[RAM..RAM + 2].copy_from_slice(&0x1234u16.to_le_bytes());
            runner.store.data_mut().continue_services = 2;
            assert_eq!(runner.call(), abi::EXIT_RUNTIME);
            assert_eq!(runner.store.data().callbacks.len(), 3);
            for (i, (reason, state)) in runner.store.data().callbacks.iter().enumerate() {
                assert_eq!(*reason, abi::REASON_SERVICE);
                assert_eq!(state.address, alias.address);
                assert_eq!(state.gpr, frame.gpr);
                assert_eq!(state.eflags, frame.eflags);
                assert_eq!(state.charged_work, (i as u64 + 1) * u64::from(interval));
                assert_eq!(state.remaining_budget, 100 - state.charged_work);
            }
            assert_eq!(
                runner.effects().inline_reserved,
                if inline { interval * 3 } else { 0 }
            );
        }
    }
}

#[test]
fn ret_preflight_refuses_stack_device_permission_and_backing_gaps_before_admission() {
    for inline in [false, true] {
        for width in [Width::Word, Width::Dword] {
            let p = program(width, u16::MAX, 0x91);
            for case in 0..14 {
                let mut runner = Runner::new(&p, inline);
                let mut frame = initial();
                frame.gpr[4] = 0xfff;
                let mut d = stack_descriptor(StackAddress::Bits32, RAM_BASE);
                let ss = &mut d.windows[0][Segment::Ss as usize];
                match case {
                    0 => d.stack_address = StackAddress::Disabled,
                    1 => ss.valid = false,
                    2 => ss.first = 0x1000,
                    3 => ss.last = 0xfff + width.bits() / 8 - 2,
                    4 => d.ram.excluded_start = 0x1000,
                    5 => d.ram.pointer = u32::MAX,
                    6 => d.ram.pointer = 65535,
                    7 => d.ram.linear_base = 0x1000,
                    8 => d.ram.length = 0xfff - RAM_BASE + width.bits() / 8 - 1,
                    9 => {
                        d.device = DeviceReadDescriptor {
                            guest_start: 0x1000,
                            guest_length: 1,
                            pointer: RAM as u32,
                            backing_length: RAM_LENGTH as u32,
                            kind: DeviceReadKind::Linear,
                            ..d.device
                        }
                    }
                    10 => ss.base = u32::MAX - 0xfff,
                    _ => {}
                }
                if case == 4 {
                    d.ram.excluded_length = 1;
                }
                prepare(&mut runner, &p, frame, &d);
                match case {
                    11 => runner.word(memory::STACK_ADDRESS, u32::MAX),
                    12 => runner.word(memory::VERSION_OFFSET, memory::VERSION + 1),
                    13 => runner.word(
                        memory::RAM_POINTER,
                        65536 - (frame.gpr[4] - RAM_BASE) - width.bits() / 8 + 1,
                    ),
                    _ => {}
                }
                let before = runner.bytes().to_vec();
                assert_eq!(
                    runner.call(),
                    abi::EXIT_RUNTIME,
                    "{width:?} inline={inline} case={case}"
                );
                assert_eq!(
                    runner.frame(),
                    frame,
                    "{width:?} inline={inline} case={case}"
                );
                assert_eq!(runner.bytes(), before);
                assert!(runner.store.data().admissions.is_empty());
                assert_eq!(
                    runner.store.data().callbacks,
                    vec![(abi::REASON_FALLBACK, frame)]
                );
            }
        }
    }
}

#[test]
fn ret_source_horizon_refusal_precedes_effects_and_extra_pop_needs_no_memory_proof() {
    for inline in [false, true] {
        let p = program(Width::Dword, u16::MAX, 0x91);
        for refusal in 0..if inline { 5 } else { 1 } {
            let mut runner = Runner::new(&p, inline);
            let frame = initial();
            setup(&mut runner, &p, frame, 0x1234_5678);
            if inline {
                match refusal {
                    0 => runner.word(memory::INLINE_GRANT, 0),
                    1 => runner.guard(0, RegionGuard::default()),
                    2 => runner.set_generation(4, 16),
                    3 => runner.word(memory::REGION_GUARDS_POINTER, u32::MAX),
                    4 => runner.word(memory::INLINE_RESERVED, u32::MAX),
                    _ => unreachable!(),
                }
                runner.assert_refused(frame);
            } else {
                runner.store.data_mut().refuse = true;
                let before = runner.bytes().to_vec();
                assert_eq!(runner.call(), abi::EXIT_RUNTIME);
                assert_eq!(runner.bytes(), before);
                assert_eq!(runner.store.data().admissions, vec![(0, 1)]);
                assert_eq!(
                    runner.store.data().callbacks,
                    vec![(abi::REASON_FALLBACK, frame)]
                );
            }
        }
        // A read-only, exact four-byte window suffices; stack/code-page write
        // guards and every byte skipped by imm16 are deliberately unavailable.
        let mut runner = Runner::new(&p, inline);
        let frame = initial();
        setup(&mut runner, &p, frame, 0x1234_5678);
        runner.word(memory::RAM_LENGTH, 4);
        let offset = FRAME + memory::OFFSET as usize + memory::SEGMENTS as usize;
        let ss = offset + Segment::Ss as usize * memory::SEGMENT_STRIDE as usize;
        runner.bytes()[ss + 12..ss + 16].copy_from_slice(&(RAM_BASE + 3).to_le_bytes());
        let write_ss = ss + 6 * memory::SEGMENT_STRIDE as usize;
        runner.bytes()[write_ss..write_ss + 4].fill(0);
        runner.word(memory::CODE_PAGES_POINTER, u32::MAX);
        runner.word(memory::CODE_PAGES_LENGTH, 0);
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        assert_eq!(
            runner.frame().gpr[4],
            frame.gpr[4] + 4 + u32::from(u16::MAX)
        );
        assert_eq!(runner.frame().address.offset, 0x1234_5678);
    }
}

#[test]
fn ret_ir_requires_isolated_counted_word_dword_direct_memory() {
    let p = program(Width::Dword, 0, 0);
    assert_eq!(p.regions[0].completed_instructions(), 1);
    assert_eq!(
        p.compile(&CompileOptions::default()).unwrap_err(),
        CompileError::DirectMemoryDisabled(RegionId(0))
    );
    let mut invalid = p.clone();
    invalid.regions[0].operations.push(Operation::Nop);
    assert_eq!(
        invalid.compile(&options(false)).unwrap_err(),
        CompileError::MixedMemoryRegion(RegionId(0))
    );
    invalid = p;
    invalid.regions[0].accounting = Accounting::UncountedHot;
    assert_eq!(
        invalid
            .compile(&CompileOptions {
                direct_memory: true,
                ..CompileOptions::default()
            })
            .unwrap_err(),
        CompileError::MemoryUncountedHot(RegionId(0))
    );
    assert_eq!(
        program(Width::Byte, 0, 0)
            .compile(&options(true))
            .unwrap_err(),
        CompileError::WidthMismatch(RegionId(0))
    );
}

#[test]
fn warmed_ret_alias_execution_allocates_nothing() {
    let p = program(Width::Word, 0xfffe, 0x91);
    let alias = EntryAlias {
        address: Address {
            space: 0x91,
            offset: 0x1234,
        },
        target: RegionId(0),
    };
    let mut runner = Runner::with_aliases(&p, options(true), &[alias]);
    let frame = Frame {
        remaining_budget: 8,
        ..initial()
    };
    let d = stack_descriptor(StackAddress::Bits16, RAM_BASE);
    prepare(&mut runner, &p, frame, &d);
    runner.bytes()[RAM..RAM + 2].copy_from_slice(&0x1234u16.to_le_bytes());
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    let info = allocation_counter::measure(|| {
        for _ in 0..8 {
            frame.write(&mut runner.bytes()[FRAME..]).unwrap();
            runner.word(memory::INLINE_GRANT, 100);
            runner.word(memory::INLINE_RESERVED, 0);
            assert_eq!(runner.call(), abi::EXIT_BUDGET);
        }
    });
    assert_eq!(info.count_total, 0);
    assert!(runner.store.data().callbacks.is_empty());
}
