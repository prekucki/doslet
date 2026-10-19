//! Engine validation and execution of dispatch tables beyond one WASM body's
//! size limit. All sampled entries share one instantiated module per fixture.

use super::*;

const CHUNK_ENTRIES: usize = 8192;
const ENGINE_BODY_LIMIT: usize = 7_654_321;
const DISPATCH_BODY_BOUND: usize = 2 * 1024 * 1024;

fn unsigned(bytes: &[u8], cursor: &mut usize) -> usize {
    let mut value = 0;
    for shift in (0..35).step_by(7) {
        let byte = bytes[*cursor];
        *cursor += 1;
        value |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return value;
        }
    }
    panic!("invalid u32 LEB128 in compiler output");
}

// Read only section/body lengths here; Module::new independently validates the
// complete emitted WASM, including instructions, types and function indices.
fn body_sizes(bytes: &[u8]) -> Vec<usize> {
    assert_eq!(&bytes[..8], b"\0asm\x01\0\0\0");
    let mut cursor = 8;
    while cursor < bytes.len() {
        let id = bytes[cursor];
        cursor += 1;
        let section_size = unsigned(bytes, &mut cursor);
        let end = cursor + section_size;
        if id == 10 {
            let count = unsigned(bytes, &mut cursor);
            let mut sizes = Vec::with_capacity(count);
            for _ in 0..count {
                let size = unsigned(bytes, &mut cursor);
                sizes.push(size);
                cursor += size;
            }
            assert_eq!(cursor, end);
            return sizes;
        }
        cursor = end;
    }
    panic!("missing code section");
}

fn marker(id: u32) -> u32 {
    id ^ 0x9234_5678
}

fn fallback(id: u32) -> Address {
    address(0x7654_3210, id)
}

fn canonical(id: u32) -> Address {
    address(0x8000_0000, 0x8000_0000 + 2 * id)
}

fn alias_address(id: u32) -> Address {
    address(u32::MAX, 0x8000_0000 + 2 * id)
}

fn initial(entry: Address) -> Frame {
    Frame {
        gpr: [0x1234_5678, 2, 3, 4, 0x800, 6, 7, 8],
        eflags: 0xa55a_0247,
        address: entry,
        remaining_budget: 100,
        charged_work: 31,
        status: 0x8000_0000,
    }
}

fn reset(runner: &mut Runner, frame: Frame) {
    runner.store.data_mut().events.clear();
    runner.store.data_mut().admissions.clear();
    runner.store.data_mut().horizon = None;
    runner.store.data_mut().stop_after = 1;
    runner.set(frame);
}

fn stack(runner: &mut Runner, target: u32) {
    const RAM: usize = 4096;
    let mut descriptor = memory::Descriptor {
        ram: memory::RamDescriptor {
            pointer: RAM as u32,
            length: 4096,
            ..memory::RamDescriptor::default()
        },
        stack_address: memory::StackAddress::Bits32,
        ..memory::Descriptor::default()
    };
    descriptor.windows[0][memory::Segment::Ss as usize] = memory::SegmentBounds {
        valid: true,
        base: 0,
        first: 0,
        last: u32::MAX,
    };
    let bytes = runner.memory.data_mut(&mut runner.store);
    descriptor
        .write(&mut bytes[PTR + memory::OFFSET as usize..])
        .unwrap();
    bytes[RAM + 0x800..RAM + 0x804].copy_from_slice(&target.to_le_bytes());
}

#[test]
fn large_ret_alias_dispatch_uses_bounded_bodies_and_preserves_exact_lookup() {
    const REGIONS: u32 = 40_000;
    const RET: u32 = REGIONS / 3;
    let program = Program {
        regions: (0..REGIONS)
            .map(|id| Region {
                id: RegionId(id),
                entry: canonical(id),
                operations: if id == RET {
                    vec![]
                } else {
                    vec![Operation::Move {
                        target: Register::Eax,
                        source: Source::Immediate(marker(id)),
                    }]
                },
                terminator: if id == RET {
                    Terminator::Return {
                        width: Width::Dword,
                        pop_bytes: 7,
                        target_space: u32::MAX,
                    }
                } else {
                    Terminator::Fallback(fallback(id))
                },
                accounting: Accounting::Counted,
            })
            .collect(),
    };
    let aliases: Vec<_> = (0..REGIONS)
        .map(|id| EntryAlias {
            address: alias_address(id),
            target: RegionId(REGIONS - 1 - id),
        })
        .collect();
    let options = CompileOptions {
        direct_memory: true,
        runtime_admission: true,
        service_interval_regions: 1,
        max_output_bytes: 64 * 1024 * 1024,
        ..CompileOptions::default()
    };
    let compiled = program.compile_with_aliases(&options, &aliases).unwrap();
    assert!(compiled.wasm().len() <= options.max_output_bytes);
    // The old monolithic table for this fixture exceeded the engine body cap.
    assert!(compiled.dispatcher_bytes() > ENGINE_BODY_LIMIT);
    let sizes = body_sizes(compiled.wasm());
    let helpers = (2 * REGIONS as usize).div_ceil(CHUNK_ENTRIES);
    assert_eq!(sizes.len(), 3 + REGIONS as usize + helpers);
    assert!(sizes.iter().all(|&size| size < DISPATCH_BODY_BOUND));
    assert_eq!(
        compiled.dispatcher_bytes(),
        sizes[1] + sizes[3 + REGIONS as usize..].iter().sum::<usize>()
    );
    std::println!(
        "large static dispatch: module={} dispatcher={} max_body={} helpers={helpers}",
        compiled.wasm().len(),
        compiled.dispatcher_bytes(),
        sizes.iter().max().unwrap()
    );
    // This validates all function bodies with the actual engine once. Reuse
    // the instance for all entry, gap, return, budget and admission samples.
    let mut runner = Runner::with_compiled(&compiled);
    let mut sampled = vec![0, REGIONS / 2, REGIONS - 1];
    // Check both sides of every chunk boundary, including the boundary that
    // falls inside the aliases after the canonical address range.
    for split in (CHUNK_ENTRIES..2 * REGIONS as usize).step_by(CHUNK_ENTRIES) {
        for index in [split - 1, split] {
            sampled.push((index % REGIONS as usize) as u32);
        }
    }
    for index in sampled {
        for (entry, id) in [
            (canonical(index), index),
            (alias_address(index), REGIONS - 1 - index),
        ] {
            assert_ne!(id, RET);
            let frame = initial(entry);
            reset(&mut runner, frame);
            assert_eq!(runner.call(), abi::EXIT_RUNTIME);
            let mut expected = Frame {
                address: fallback(id),
                remaining_budget: 99,
                charged_work: 32,
                ..frame
            };
            expected.gpr[0] = marker(id);
            assert_eq!(runner.frame(), expected);
            assert_eq!(runner.store.data().admissions, [(id, 1)]);
            assert_eq!(
                runner.store.data().events,
                [(abi::REASON_FALLBACK, expected)]
            );
            // A missing exact address must not inherit the selected chunk's
            // target, service reason or admission.
            let unknown = Frame {
                address: address(entry.space, entry.offset + 1),
                ..frame
            };
            reset(&mut runner, unknown);
            assert_eq!(runner.call(), abi::EXIT_RUNTIME);
            assert_eq!(runner.frame(), unknown);
            assert!(runner.store.data().admissions.is_empty());
            assert_eq!(
                runner.store.data().events,
                [(abi::REASON_FALLBACK, unknown)]
            );
        }
    }
    for entry in [
        address(0, 0),
        address(0x8000_0000, 0x7fff_ffff),
        address(0xffff_fffe, u32::MAX),
        address(u32::MAX, u32::MAX),
    ] {
        let frame = initial(entry);
        reset(&mut runner, frame);
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        assert_eq!(runner.frame(), frame);
        assert_eq!(runner.store.data().events, [(abi::REASON_FALLBACK, frame)]);
        assert!(runner.store.data().admissions.is_empty());
    }

    // RET exhausts the structural quota. Known targets service before their
    // own admission; unknown targets still fall back after the completed RET.
    for target in [
        alias_address(0).offset,
        alias_address(REGIONS - 1).offset,
        1,
    ] {
        let frame = initial(canonical(RET));
        reset(&mut runner, frame);
        stack(&mut runner, target);
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        let mut expected = Frame {
            address: address(u32::MAX, target),
            remaining_budget: 99,
            charged_work: 32,
            ..frame
        };
        expected.gpr[4] += 11;
        assert_eq!(runner.frame(), expected);
        assert_eq!(runner.store.data().admissions, [(RET, 1)]);
        assert_eq!(
            runner.store.data().events,
            [(
                if target == 1 {
                    abi::REASON_FALLBACK
                } else {
                    abi::REASON_SERVICE
                },
                expected
            )]
        );
    }
    for index in [0, REGIONS - 1] {
        let frame = initial(canonical(RET));
        reset(&mut runner, frame);
        runner.store.data_mut().stop_after = 2;
        stack(&mut runner, alias_address(index).offset);
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        let mut returned = Frame {
            address: alias_address(index),
            remaining_budget: 99,
            charged_work: 32,
            ..frame
        };
        returned.gpr[4] += 11;
        let id = REGIONS - 1 - index;
        let mut expected = Frame {
            address: fallback(id),
            remaining_budget: 98,
            charged_work: 33,
            ..returned
        };
        expected.gpr[0] = marker(id);
        assert_eq!(runner.frame(), expected);
        assert_eq!(runner.store.data().admissions, [(RET, 1), (id, 1)]);
        assert_eq!(
            runner.store.data().events,
            [
                (abi::REASON_SERVICE, returned),
                (abi::REASON_FALLBACK, expected)
            ]
        );
    }
    // An exhausted budget precedes lookup, service and target effects.
    for budget in [0, 1] {
        let frame = Frame {
            remaining_budget: budget,
            ..initial(canonical(RET))
        };
        reset(&mut runner, frame);
        stack(&mut runner, alias_address(0).offset);
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        let mut expected = frame;
        if budget == 1 {
            expected.gpr[4] += 11;
            expected.address = alias_address(0);
            expected.remaining_budget = 0;
            expected.charged_work += 1;
        }
        assert_eq!(runner.frame(), expected);
        assert!(runner.store.data().events.is_empty());
        assert_eq!(runner.store.data().admissions.len(), budget as usize);
    }
    // Refusal still belongs to the selected original region before its effects.
    let frame = initial(alias_address(REGIONS / 2));
    reset(&mut runner, frame);
    runner.store.data_mut().horizon = Some(0);
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    let refused = Frame {
        address: canonical(REGIONS - 1 - REGIONS / 2),
        ..frame
    };
    assert_eq!(runner.frame(), refused);
    assert_eq!(
        runner.store.data().admissions,
        [(REGIONS - 1 - REGIONS / 2, 1)]
    );
    assert_eq!(
        runner.store.data().events,
        [(abi::REASON_FALLBACK, refused)]
    );

    // Warmed helper lookup retains the allocation-free execution contract.
    let frame = Frame {
        remaining_budget: 1,
        ..initial(alias_address(0))
    };
    reset(&mut runner, frame);
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    let info = allocation_counter::measure(|| {
        for _ in 0..8 {
            reset(&mut runner, frame);
            assert_eq!(runner.call(), abi::EXIT_BUDGET);
        }
    });
    assert_eq!(info.count_total, 0);
}

#[test]
fn dispatcher_chunk_threshold_preserves_helper_indices_limits_and_determinism() {
    let mut program = Program {
        regions: [7, 91]
            .into_iter()
            .map(|id| Region {
                id: RegionId(id),
                entry: canonical(id),
                operations: vec![Operation::Move {
                    target: Register::Eax,
                    source: Source::Immediate(marker(id)),
                }],
                terminator: Terminator::Fallback(fallback(id)),
                accounting: Accounting::Counted,
            })
            .collect(),
    };
    let mut aliases: Vec<_> = (0..CHUNK_ENTRIES - 1)
        .map(|index| EntryAlias {
            address: alias_address(index as u32),
            target: RegionId(if index % 2 == 0 { 7 } else { 91 }),
        })
        .collect();
    for options in [
        CompileOptions::default(),
        admission_options(),
        CompileOptions {
            direct_memory: true,
            inline_admission: true,
            ..CompileOptions::default()
        },
    ] {
        let inline_helpers = usize::from(options.inline_admission);
        let before = program
            .compile_with_aliases(&options, &aliases[..aliases.len() - 1])
            .unwrap();
        assert_eq!(body_sizes(before.wasm()).len(), 5 + inline_helpers);
        let compiled = program.compile_with_aliases(&options, &aliases).unwrap();
        let sizes = body_sizes(compiled.wasm());
        assert_eq!(sizes.len(), 7 + inline_helpers);
        assert!(sizes.iter().all(|&size| size < DISPATCH_BODY_BOUND));
        assert_eq!(
            compiled.dispatcher_bytes(),
            sizes[1] + sizes[5 + inline_helpers..].iter().sum::<usize>()
        );
        let mut runner = Runner::with_compiled(&compiled);
        for entry in [
            aliases[0],
            aliases[CHUNK_ENTRIES - 3],
            aliases[CHUNK_ENTRIES - 2],
        ] {
            let frame = Frame {
                remaining_budget: 1,
                ..initial(entry.address)
            };
            reset(&mut runner, frame);
            if options.inline_admission {
                const GENERATIONS: usize = 4096;
                const GUARDS: usize = 8192;
                let descriptor = memory::Descriptor {
                    ram: memory::RamDescriptor {
                        generations_pointer: GENERATIONS as u32,
                        generations_length: 1,
                        ..memory::RamDescriptor::default()
                    },
                    inline_grant: 1,
                    region_guards_pointer: GUARDS as u32,
                    region_guards_length: 92,
                    ..memory::Descriptor::default()
                };
                let bytes = runner.memory.data_mut(&mut runner.store);
                descriptor
                    .write(&mut bytes[PTR + memory::OFFSET as usize..])
                    .unwrap();
                bytes[GENERATIONS..GENERATIONS + 8].copy_from_slice(&12u64.to_le_bytes());
                memory::RegionGuard {
                    valid: true,
                    pages: [0, 0],
                    generations: [12, 12],
                }
                .write(&mut bytes[GUARDS + entry.target.0 as usize * memory::REGION_GUARD_BYTES..])
                .unwrap();
            }
            assert_eq!(runner.call(), abi::EXIT_BUDGET);
            let mut expected = Frame {
                address: fallback(entry.target.0),
                remaining_budget: 0,
                charged_work: 32,
                ..frame
            };
            expected.gpr[0] = marker(entry.target.0);
            assert_eq!(runner.frame(), expected);
            assert!(runner.store.data().events.is_empty());
            assert_eq!(
                runner.store.data().admissions,
                if options.runtime_admission {
                    vec![(entry.target.0, 1)]
                } else {
                    vec![]
                }
            );
            if options.inline_admission {
                let effects = memory::Effects::read(
                    &runner.memory.data(&runner.store)[PTR + memory::OFFSET as usize..],
                )
                .unwrap();
                assert_eq!(effects.inline_remaining, 0);
                assert_eq!(effects.inline_reserved, 1);
            }
        }
        for max_output_bytes in [256, compiled.wasm().len() - 1] {
            assert_eq!(
                program
                    .compile_with_aliases(
                        &CompileOptions {
                            max_output_bytes,
                            ..options
                        },
                        &aliases
                    )
                    .unwrap_err(),
                CompileError::OutputTooLarge
            );
        }
        let exact = program
            .compile_with_aliases(
                &CompileOptions {
                    max_output_bytes: compiled.wasm().len(),
                    ..options
                },
                &aliases,
            )
            .unwrap();
        assert_eq!(exact.wasm(), compiled.wasm());
        program.regions.reverse();
        aliases.reverse();
        let reordered = program.compile_with_aliases(&options, &aliases).unwrap();
        assert_eq!(reordered.wasm(), compiled.wasm());
        program.regions.reverse();
        aliases.reverse();
    }
}
