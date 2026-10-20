use super::*;

fn baseline_program() -> Program {
    Program {
        regions: vec![
            region(
                91,
                vec![Operation::Inc(Register::Al)],
                Terminator::Jump(RegionId(7)),
            ),
            region(
                7,
                vec![Operation::Move {
                    target: Register::Edx,
                    source: Source::Register(Register::Eax),
                }],
                Terminator::Fallback(address(8, 17)),
            ),
        ],
    }
}

fn byte_fingerprint(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
    })
}

#[test]
fn empty_aliases_preserve_existing_module_bytes() {
    let program = baseline_program();
    let options = [
        CompileOptions::default(),
        admission_options(),
        CompileOptions {
            direct_memory: true,
            inline_admission: true,
            ..CompileOptions::default()
        },
    ];
    // The first two fingerprints predate entry aliases; the direct-memory
    // fingerprint reflects descriptor ABI v2 (384 bytes, explicit VGA writes).
    let expected = [
        (1167, 0x4580_c528_675e_fef7),
        (1457, 0x57e7_9fc6_e006_af19),
        (1747, 0xbf51_1041_3add_d0f5),
    ];
    for (options, expected) in options.into_iter().zip(expected) {
        let module = program.compile(&options).unwrap();
        assert_eq!(
            (module.wasm().len(), byte_fingerprint(module.wasm())),
            expected
        );
        let explicit = program.compile_with_aliases(&options, &[]).unwrap();
        assert_eq!(explicit.wasm(), module.wasm());
        assert_eq!(explicit.dispatcher_bytes(), module.dispatcher_bytes());
    }
}

fn aliases() -> [EntryAlias; 2] {
    [
        EntryAlias {
            address: address(u32::MAX, 0x8000_0001),
            target: RegionId(7),
        },
        EntryAlias {
            address: address(0, u32::MAX),
            target: RegionId(91),
        },
    ]
}

const GENERATIONS: usize = 4096;
const GUARDS: usize = 8192;

fn inline_options() -> CompileOptions {
    CompileOptions {
        direct_memory: true,
        inline_admission: true,
        ..CompileOptions::default()
    }
}

fn configure_inline(runner: &mut Runner, program: &Program, grant: u32) {
    let descriptor = memory::Descriptor {
        ram: memory::RamDescriptor {
            generations_pointer: GENERATIONS as u32,
            generations_length: 1,
            ..memory::RamDescriptor::default()
        },
        inline_grant: grant,
        region_guards_pointer: GUARDS as u32,
        region_guards_length: program.regions.iter().map(|r| r.id.0).max().unwrap() + 1,
        ..memory::Descriptor::default()
    };
    let bytes = runner.memory.data_mut(&mut runner.store);
    descriptor
        .write(&mut bytes[PTR + memory::OFFSET as usize..])
        .unwrap();
    bytes[GENERATIONS..GENERATIONS + 8].copy_from_slice(&12u64.to_le_bytes());
    for region in &program.regions {
        memory::RegionGuard {
            valid: true,
            pages: [0, 0],
            generations: [12, 12],
        }
        .write(&mut bytes[GUARDS + region.id.0 as usize * memory::REGION_GUARD_BYTES..])
        .unwrap();
    }
}

#[test]
fn canonical_and_alias_entries_share_sparse_region_effects_and_accounting() {
    let program = baseline_program();
    for options in [
        CompileOptions::default(),
        admission_options(),
        inline_options(),
    ] {
        for alias in aliases() {
            let canonical = program
                .regions
                .iter()
                .find(|r| r.id == alias.target)
                .unwrap()
                .entry;
            let initial = Frame {
                gpr: [0x1234_56ff, 2, 3, 4, 5, 6, 7, 8],
                eflags: 0xa55a_0247,
                remaining_budget: 100,
                charged_work: 31,
                status: 0x8000_0000,
                ..Frame::default()
            };
            let mut results = Vec::new();
            for entry in [canonical, alias.address] {
                let mut runner = Runner::with_aliases(&program, options, &aliases());
                if options.inline_admission {
                    configure_inline(&mut runner, &program, 10);
                }
                runner.set(Frame {
                    address: entry,
                    ..initial
                });
                assert_eq!(runner.call(), abi::EXIT_RUNTIME);
                let effects = options.inline_admission.then(|| {
                    memory::Effects::read(
                        &runner.memory.data(&runner.store)[PTR + memory::OFFSET as usize..],
                    )
                    .unwrap()
                });
                results.push((
                    runner.frame(),
                    runner.store.data().admissions.clone(),
                    runner.store.data().events.clone(),
                    effects,
                ));
            }
            assert_eq!(results[0], results[1]);
            let cost = if alias.target == RegionId(91) { 3 } else { 1 };
            assert_eq!(results[1].0.charged_work, 31 + cost);
            assert_eq!(results[1].0.remaining_budget, 100 - cost);
            assert_eq!(results[1].0.address, address(8, 17));
            if options.runtime_admission {
                let expected = if alias.target == RegionId(91) {
                    vec![(91, 2), (7, 1)]
                } else {
                    vec![(7, 1)]
                };
                assert_eq!(results[1].1, expected);
            }
            if let Some(effects) = results[1].3 {
                assert_eq!(effects.inline_reserved, cost as u32);
                assert_eq!(effects.inline_remaining, 10 - cost as u32);
            }
        }
    }
}

#[test]
fn runtime_callback_can_resume_through_an_alias() {
    let program = baseline_program();
    let alias = aliases()[1];
    let initial = Frame {
        address: address(4, 123),
        remaining_budget: 100,
        ..Frame::default()
    };
    let mut runner = Runner::with_aliases(&program, admission_options(), &aliases());
    runner.store.data_mut().stop_after = 2;
    runner.store.data_mut().redirect = Some(Frame {
        address: alias.address,
        ..initial
    });
    runner.set(initial);
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(
        runner.store.data().events[0],
        (abi::REASON_FALLBACK, initial)
    );
    assert_eq!(runner.store.data().admissions, vec![(91, 2), (7, 1)]);
    assert_eq!(runner.frame().gpr[0], 1);
    assert_eq!(runner.frame().gpr[2], 1);
    assert_eq!(runner.frame().charged_work, 3);
}

#[test]
fn aliases_preserve_target_admission_refusal_before_effects() {
    let program = baseline_program();
    let alias = aliases()[1];
    let initial = Frame {
        gpr: [0x1234_5678; 8],
        eflags: 0xa55a_0247,
        address: alias.address,
        remaining_budget: 100,
        charged_work: 37,
        status: 0x8000_0000,
    };
    let expected = Frame {
        address: program.regions[0].entry,
        ..initial
    };
    let mut runner = Runner::with_aliases(&program, admission_options(), &aliases());
    runner.store.data_mut().horizon = Some(1);
    runner.set(initial);
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.frame(), expected);
    assert_eq!(runner.store.data().admissions, vec![(91, 2)]);
    assert_eq!(
        runner.store.data().events,
        vec![(abi::REASON_FALLBACK, expected)]
    );

    for refusal in 0..5 {
        let mut runner = Runner::with_aliases(&program, inline_options(), &aliases());
        configure_inline(&mut runner, &program, if refusal == 0 { 1 } else { 10 });
        runner.set(initial);
        let bytes = runner.memory.data_mut(&mut runner.store);
        match refusal {
            1 => bytes[GENERATIONS] = 13,
            2 => bytes[GUARDS + 91 * memory::REGION_GUARD_BYTES] = 0,
            3 => bytes[PTR + memory::OFFSET as usize + memory::VERSION_OFFSET as usize] = 0,
            4 => {
                let start = PTR + memory::OFFSET as usize + memory::REGION_GUARDS_LENGTH as usize;
                bytes[start..start + 4].copy_from_slice(&91u32.to_le_bytes());
            }
            _ => {}
        }
        let mut expected_bytes = bytes.to_vec();
        expected.write(&mut expected_bytes[PTR..]).unwrap();
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        assert_eq!(runner.frame(), expected);
        assert_eq!(runner.memory.data(&runner.store), expected_bytes);
        assert_eq!(
            runner.store.data().events,
            vec![(abi::REASON_FALLBACK, expected)]
        );
        assert!(runner.store.data().admissions.is_empty());
    }
}

#[test]
fn unknown_addresses_preserve_the_complete_input_frame_with_aliases() {
    let program = baseline_program();
    for unknown in [
        address(0, u32::MAX - 1),
        address(4, 0),
        address(u32::MAX, u32::MAX),
    ] {
        let initial = Frame {
            gpr: [0xdead_beef; 8],
            eflags: 0x845,
            address: unknown,
            remaining_budget: 17,
            charged_work: 23,
            status: 0x8000_0000,
        };
        let mut runner = Runner::with_aliases(&program, admission_options(), &aliases());
        runner.set(initial);
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        assert_eq!(runner.frame(), initial);
        assert_eq!(
            runner.store.data().events,
            vec![(abi::REASON_FALLBACK, initial)]
        );
        assert!(runner.store.data().admissions.is_empty());
    }
}

#[test]
fn region_and_alias_order_do_not_change_modules_or_unknown_target_errors() {
    let mut program = baseline_program();
    let mut entries = aliases();
    let compiled = program
        .compile_with_aliases(&CompileOptions::default(), &entries)
        .unwrap();
    program.regions.reverse();
    entries.reverse();
    let reordered = program
        .compile_with_aliases(&CompileOptions::default(), &entries)
        .unwrap();
    assert_eq!(compiled.wasm(), reordered.wasm());
    for entry in &mut entries {
        entry.target = RegionId(99);
    }
    let expected = CompileError::UnknownAliasTarget {
        address: aliases()[1].address,
        target: RegionId(99),
    };
    for _ in 0..2 {
        assert_eq!(
            program
                .compile_with_aliases(&CompileOptions::default(), &entries)
                .unwrap_err(),
            expected
        );
        entries.reverse();
        program.regions.reverse();
    }
    assert_eq!(
        Program::default()
            .compile_with_aliases(&CompileOptions::default(), &entries)
            .unwrap_err(),
        expected
    );
}

#[test]
fn conflicting_and_duplicate_alias_addresses_are_explicit() {
    let program = baseline_program();
    let options = CompileOptions::default();
    for target in [RegionId(91), RegionId(7), RegionId(99)] {
        let collision = EntryAlias {
            address: program.regions[0].entry,
            target,
        };
        assert_eq!(
            program
                .compile_with_aliases(&options, &[collision])
                .unwrap_err(),
            CompileError::DuplicateAddress(collision.address)
        );
        let duplicate = EntryAlias {
            address: aliases()[0].address,
            target,
        };
        assert_eq!(
            program
                .compile_with_aliases(&options, &[duplicate, aliases()[0]])
                .unwrap_err(),
            CompileError::DuplicateAddress(duplicate.address)
        );
    }
    let mut duplicate_regions = program;
    duplicate_regions.regions[1].entry = duplicate_regions.regions[0].entry;
    assert_eq!(
        duplicate_regions
            .compile_with_aliases(&options, &aliases())
            .unwrap_err(),
        CompileError::DuplicateAddress(duplicate_regions.regions[0].entry)
    );
    duplicate_regions.regions[1].id = duplicate_regions.regions[0].id;
    assert_eq!(
        duplicate_regions
            .compile_with_aliases(&options, &aliases())
            .unwrap_err(),
        CompileError::DuplicateId(RegionId(91))
    );
}

#[test]
fn aliases_do_not_supply_missing_regions_for_direct_edges() {
    let mut program = baseline_program();
    program.regions[1].terminator = Terminator::Jump(RegionId(99));
    assert_eq!(
        program
            .compile_with_aliases(&CompileOptions::default(), &aliases())
            .unwrap_err(),
        CompileError::UnknownTarget {
            from: RegionId(7),
            target: RegionId(99)
        }
    );
}

#[test]
fn alias_and_combined_entry_limits_are_bounded_before_allocation() {
    let program = baseline_program();
    let options = CompileOptions {
        max_regions: 2,
        ..CompileOptions::default()
    };
    let checked = Checked::new(&program, &options, &aliases()).unwrap();
    assert_eq!(checked.by_address.len(), 2 * options.max_regions);
    assert_eq!(
        checked.by_id,
        Checked::new(&program, &options, &[]).unwrap().by_id
    );
    program.compile_with_aliases(&options, &aliases()).unwrap();
    let oversized = [aliases()[0]; 3];
    let info = allocation_counter::measure(|| {
        assert_eq!(
            program
                .compile_with_aliases(&options, &oversized)
                .unwrap_err(),
            CompileError::TooManyAliases
        );
        assert_eq!(
            program
                .compile_with_aliases(
                    &CompileOptions {
                        max_regions: 1,
                        ..options
                    },
                    &[]
                )
                .unwrap_err(),
            CompileError::TooManyRegions
        );
        assert_eq!(
            Program::default()
                .compile_with_aliases(
                    &CompileOptions {
                        max_regions: 0,
                        ..options
                    },
                    &aliases()[..1]
                )
                .unwrap_err(),
            CompileError::TooManyAliases
        );
    });
    assert_eq!(info.count_total, 0);
}

#[test]
fn alias_dispatcher_obeys_the_output_byte_limit() {
    let program = baseline_program();
    let options = CompileOptions::default();
    let canonical = program.compile(&options).unwrap();
    let aliased = program.compile_with_aliases(&options, &aliases()).unwrap();
    assert!(aliased.dispatcher_bytes() > canonical.dispatcher_bytes());
    for limit in [canonical.wasm().len(), aliased.wasm().len() - 1] {
        assert_eq!(
            program
                .compile_with_aliases(
                    &CompileOptions {
                        max_output_bytes: limit,
                        ..options
                    },
                    &aliases()
                )
                .unwrap_err(),
            CompileError::OutputTooLarge
        );
    }
    let exact = program
        .compile_with_aliases(
            &CompileOptions {
                max_output_bytes: aliased.wasm().len(),
                ..options
            },
            &aliases(),
        )
        .unwrap();
    assert_eq!(exact.wasm(), aliased.wasm());
}

#[test]
fn warmed_alias_dispatch_does_not_allocate_at_runtime() {
    let program = single(Operation::Inc(Register::Eax));
    let entries = &aliases()[..1];
    for options in [CompileOptions::default(), inline_options()] {
        let mut runner = Runner::with_aliases(&program, options, entries);
        let initial = Frame {
            address: entries[0].address,
            remaining_budget: 1,
            ..Frame::default()
        };
        if options.inline_admission {
            configure_inline(&mut runner, &program, 100);
        }
        runner.set(initial);
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        let info = allocation_counter::measure(|| {
            for _ in 0..8 {
                runner.set(initial);
                assert_eq!(runner.call(), abi::EXIT_BUDGET);
            }
        });
        assert_eq!(info.count_total, 0);
        assert_eq!(runner.frame().gpr[0], 1);
        assert!(runner.store.data().events.is_empty());
    }
}
