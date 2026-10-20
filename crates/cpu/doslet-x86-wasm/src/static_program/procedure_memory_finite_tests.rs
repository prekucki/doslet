use super::*;

fn compile_finite(program: &ProcedureGraphProgram) -> CompiledProcedureProgram {
    program
        .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 1)
        .unwrap()
}

/// Independently encoded conditional transform with a word induction counter.
fn conditional_transform() -> (ProcedureGraphProgram, Vec<u8>) {
    let mut code = vec![0xcc; 26];
    put(&mut code, 0, &[0x66, 0xb9, 3, 0]); // MOV CX,3
    put(&mut code, 4, &[0x8a, 6, 0xa8, 1, 0x74, 6]); // MOV AL,[ESI]; TEST AL,1; JZ 16
    put(&mut code, 10, &[0, 0xc3, 0xeb, 6]); // ADD BL,AL; JMP 20
    put(&mut code, 16, &[0x30, 0xc3, 0xeb, 0]); // XOR BL,AL; JMP 20
    put(&mut code, 20, &[0x46, 0x66, 0x49, 0x75, 0xeb, 0xc3]); // INC ESI; DEC CX; JNZ 4; RET
    let p = graph(vec![
        block(
            0,
            vec![instruction(
                0,
                Operation::Move {
                    target: Register::Cx,
                    source: Source::Immediate(3),
                },
            )],
            4,
            ProcedureTerminatorKind::Fallthrough(at(4)),
        ),
        block(
            4,
            vec![
                instruction(
                    4,
                    Operation::LoadMemory {
                        target: Register::Al,
                        address: address(Some(Register::Esi), 0, Segment::Ds),
                    },
                ),
                instruction(
                    6,
                    Operation::Alu {
                        kind: Alu::Test,
                        target: Register::Al,
                        source: Source::Immediate(1),
                    },
                ),
            ],
            8,
            ProcedureTerminatorKind::Branch {
                condition: Condition::Equal,
                taken: at(16),
                not_taken: at(10),
            },
        ),
        block(
            10,
            vec![instruction(
                10,
                Operation::Alu {
                    kind: Alu::Add,
                    target: Register::Bl,
                    source: Source::Register(Register::Al),
                },
            )],
            12,
            ProcedureTerminatorKind::Jump(at(20)),
        ),
        block(
            16,
            vec![instruction(
                16,
                Operation::Alu {
                    kind: Alu::Xor,
                    target: Register::Bl,
                    source: Source::Register(Register::Al),
                },
            )],
            18,
            ProcedureTerminatorKind::Jump(at(20)),
        ),
        block(
            20,
            vec![
                instruction(20, Operation::Inc(Register::Esi)),
                instruction(21, Operation::Dec(Register::Cx)),
            ],
            23,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(4),
                not_taken: at(25),
            },
        ),
        block(25, vec![], 25, ret(Width::Dword, 0)),
    ]);
    (p, code)
}

fn nested_counters() -> (ProcedureGraphProgram, Vec<u8>) {
    let code = vec![
        0x66, 0xb9, 3, 0, // MOV CX,3
        0x66, 0xba, 4, 0, // outer: MOV DX,4
        0, 0xd0, // inner: ADD AL,DL
        0x66, 0x4a, 0x75, 0xfa, // DEC DX; JNZ inner
        0x66, 0x49, 0x75, 0xf2, // DEC CX; JNZ outer
        0xc3,
    ];
    let p = graph(vec![
        block(
            0,
            vec![instruction(
                0,
                Operation::Move {
                    target: Register::Cx,
                    source: Source::Immediate(3),
                },
            )],
            4,
            ProcedureTerminatorKind::Fallthrough(at(4)),
        ),
        block(
            4,
            vec![instruction(
                4,
                Operation::Move {
                    target: Register::Dx,
                    source: Source::Immediate(4),
                },
            )],
            8,
            ProcedureTerminatorKind::Fallthrough(at(8)),
        ),
        block(
            8,
            vec![
                instruction(
                    8,
                    Operation::Alu {
                        kind: Alu::Add,
                        target: Register::Al,
                        source: Source::Register(Register::Dl),
                    },
                ),
                instruction(10, Operation::Dec(Register::Dx)),
            ],
            12,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(8),
                not_taken: at(14),
            },
        ),
        block(
            14,
            vec![instruction(14, Operation::Dec(Register::Cx))],
            16,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(4),
                not_taken: at(18),
            },
        ),
        block(18, vec![], 18, ret(Width::Dword, 0)),
    ]);
    (p, code)
}

/// Match offline decoder granularity: every instruction and real terminator
/// gets its own block, joined only by synthetic zero-work subdivisions.
fn subdivide(program: &ProcedureGraphProgram) -> ProcedureGraphProgram {
    let mut p = program.clone();
    let mut blocks = vec![];
    for block in &p.functions[0].blocks {
        for (position, instruction) in block.instructions.iter().enumerate() {
            let next = block
                .instructions
                .get(position + 1)
                .map_or(block.terminator.address, |i| i.address);
            blocks.push(ProcedureBlock {
                entry: instruction.address,
                instructions: vec![*instruction],
                terminator: ProcedureTerminator {
                    address: next,
                    kind: ProcedureTerminatorKind::Fallthrough(next),
                },
            });
        }
        if !matches!(
            block.terminator.kind,
            ProcedureTerminatorKind::Fallthrough(_)
        ) {
            blocks.push(ProcedureBlock {
                entry: block.terminator.address,
                instructions: vec![],
                terminator: block.terminator,
            });
        }
    }
    p.functions[0].blocks = blocks;
    p
}

#[test]
fn finite_conditional_computation_completes_with_budget_and_grant_one() {
    let (p, code) = conditional_transform();
    for p in [p.clone(), subdivide(&p)] {
        assert_eq!(p.functions[0].finite_maximum_work(), Ok(26));
        let compiled = compile_finite(&p);
        assert_eq!(compiled.finite_maximum_work(ProcedureId(3)), Some(26));
        assert_eq!(
            compiled.finite_loop_guards(ProcedureId(3)),
            Some([].as_slice())
        );
        let mut runner = Runner::compiled(&compiled);
        let mut frame = initial();
        frame.remaining_budget = 1;
        frame.gpr[6] = 0x1300;
        let mut d = descriptor();
        d.inline_grant = 1;
        assert_eq!(
            compare(
                &mut runner,
                &code,
                frame,
                d,
                completed_address(),
                COMPLETE,
                |runner| {
                    let data = RAM + (0x1300 - RAM_BASE) as usize;
                    runner.bytes()[data..data + 3].copy_from_slice(&[1, 2, 3]);
                }
            ),
            26
        );
    }
}

#[test]
fn finite_nested_independent_counters_match_interpreter() {
    let (p, code) = nested_counters();
    for p in [p.clone(), subdivide(&p)] {
        let compiled = compile_finite(&p);
        assert_eq!(compiled.finite_maximum_work(ProcedureId(3)), Some(47));
        let mut runner = Runner::compiled(&compiled);
        assert_eq!(
            compare(
                &mut runner,
                &code,
                initial(),
                descriptor(),
                completed_address(),
                COMPLETE,
                |_| {}
            ),
            47
        );
    }
}

#[test]
fn finite_multiple_sequential_loops_match_interpreter() {
    // Two independent byte loops, each wrapping from a live input to zero.
    let code = vec![0xfe, 0xc3, 0x75, 0xfc, 0xfe, 0xca, 0x75, 0xfc, 0xc3];
    let p = graph(vec![
        block(
            0,
            vec![instruction(0, Operation::Inc(Register::Bl))],
            2,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(0),
                not_taken: at(4),
            },
        ),
        block(
            4,
            vec![instruction(4, Operation::Dec(Register::Dl))],
            6,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(4),
                not_taken: at(8),
            },
        ),
        block(8, vec![], 8, ret(Width::Dword, 0)),
    ]);
    let compiled = compile_finite(&p);
    assert_eq!(compiled.finite_maximum_work(ProcedureId(3)), Some(1025));
    let mut runner = Runner::compiled(&compiled);
    let mut frame = initial();
    frame.gpr[3] = 253;
    frame.gpr[2] = 4;
    assert_eq!(
        compare(
            &mut runner,
            &code,
            frame,
            descriptor(),
            completed_address(),
            COMPLETE,
            |_| {}
        ),
        15
    );
}

#[test]
fn finite_disjoint_byte_aliases_and_nested_zero_restore_are_proven() {
    // The inner CH loop returns CH to zero. For the initialized ECX countdown
    // only CL can be nonzero at an outer boundary, so CH is temporary space.
    let code = vec![
        0xb9, 2, 0, 0, 0, 0x90, 0xfe, 0xc5, 0x75, 0xfc, 0x49, 0x75, 0xf8, 0xc3,
    ];
    let p = graph(vec![
        block(
            0,
            vec![instruction(
                0,
                Operation::Move {
                    target: Register::Ecx,
                    source: Source::Immediate(2),
                },
            )],
            5,
            ProcedureTerminatorKind::Fallthrough(at(5)),
        ),
        block(
            5,
            vec![instruction(5, Operation::Nop)],
            6,
            ProcedureTerminatorKind::Fallthrough(at(6)),
        ),
        block(
            6,
            vec![instruction(6, Operation::Inc(Register::Ch))],
            8,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(6),
                not_taken: at(10),
            },
        ),
        block(
            10,
            vec![instruction(10, Operation::Dec(Register::Ecx))],
            11,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(5),
                not_taken: at(13),
            },
        ),
        block(13, vec![], 13, ret(Width::Dword, 0)),
    ]);
    for p in [p.clone(), subdivide(&p)] {
        let compiled = compile_finite(&p);
        assert_eq!(compiled.finite_maximum_work(ProcedureId(3)), Some(1032));
        let mut runner = Runner::compiled(&compiled);
        assert_eq!(
            compare_until(
                &mut runner,
                &code,
                initial(),
                descriptor(),
                completed_address(),
                COMPLETE,
                Some(1032),
                |_| {}
            ),
            1032
        );
    }
}

#[test]
fn finite_counter_alias_mutation_and_read_only_wait_are_rejected() {
    let (mut p, _) = conditional_transform();
    p.functions[0].blocks[2].instructions[0].operation = Operation::Move {
        target: Register::Ch,
        source: Source::Immediate(1),
    };
    assert!(matches!(
        p.functions[0].finite_maximum_work(),
        Err(ProcedureCompileError::CyclicGraph(_))
    ));
    let p = graph(vec![
        block(
            0,
            vec![
                instruction(
                    0,
                    Operation::LoadMemory {
                        target: Register::Al,
                        address: ds(0x1300),
                    },
                ),
                instruction(
                    2,
                    Operation::Alu {
                        kind: Alu::Test,
                        target: Register::Al,
                        source: Source::Register(Register::Al),
                    },
                ),
            ],
            4,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(0),
                not_taken: at(6),
            },
        ),
        block(6, vec![], 6, ret(Width::Dword, 0)),
    ]);
    assert!(matches!(
        p.functions[0].finite_maximum_work(),
        Err(ProcedureCompileError::CyclicGraph(_))
    ));
    // Ordinary bounded loops retain their existing behavior and marker.
    assert_eq!(compile_finite(&p).finite_maximum_work(ProcedureId(3)), None);
}

#[test]
fn finite_memory_refusal_commits_exact_prefix_and_exit_has_no_work() {
    let (mut p, code) = conditional_transform();
    let compiled = compile_finite(&p);
    let mut runner = Runner::compiled(&compiled);
    let mut frame = initial();
    frame.gpr[6] = RAM_BASE + RAM_LENGTH as u32 - 1;
    let mut d = descriptor();
    d.inline_grant = 1;
    assert_eq!(
        compare_until(
            &mut runner,
            &code,
            frame,
            d,
            at(4),
            SIDE_EXIT,
            Some(9),
            |_| {}
        ),
        9
    );

    p.functions[0].blocks.last_mut().unwrap().terminator.kind = ProcedureTerminatorKind::Exit;
    let compiled = compile_finite(&p);
    assert_eq!(compiled.finite_maximum_work(ProcedureId(3)), Some(25));
    let mut runner = Runner::compiled(&compiled);
    let mut frame = initial();
    frame.gpr[6] = 0x1300;
    assert_eq!(
        compare(
            &mut runner,
            &code,
            frame,
            descriptor(),
            at(25),
            SIDE_EXIT,
            |_| {}
        ),
        25
    );
}

#[test]
fn finite_block_order_and_relocation_do_not_change_proof() {
    let (p, _) = nested_counters();
    let compiled = compile_finite(&p);
    let mut reordered = p.clone();
    reordered.functions[0].blocks.reverse();
    assert_eq!(compiled.wasm(), compile_finite(&reordered).wasm());
    let mut relocated = p;
    let shift = |mut address: Address| {
        address.space += 7;
        address.offset += 0x11000;
        address
    };
    let function = &mut relocated.functions[0];
    function.entry = shift(function.entry);
    for block in &mut function.blocks {
        block.entry = shift(block.entry);
        for instruction in &mut block.instructions {
            instruction.address = shift(instruction.address);
        }
        block.terminator.address = shift(block.terminator.address);
        block.terminator.kind = match block.terminator.kind {
            ProcedureTerminatorKind::Fallthrough(target) => {
                ProcedureTerminatorKind::Fallthrough(shift(target))
            }
            ProcedureTerminatorKind::Jump(target) => ProcedureTerminatorKind::Jump(shift(target)),
            ProcedureTerminatorKind::Branch {
                condition,
                taken,
                not_taken,
            } => ProcedureTerminatorKind::Branch {
                condition,
                taken: shift(taken),
                not_taken: shift(not_taken),
            },
            terminal => terminal,
        };
    }
    assert_eq!(function.finite_maximum_work(), Ok(47));
    assert_eq!(
        compile_finite(&relocated).finite_maximum_work(ProcedureId(3)),
        Some(47)
    );
}

#[test]
fn finite_acyclic_exit_preserves_ordinary_compilation() {
    let p = graph(vec![block(
        0,
        vec![instruction(0, Operation::Inc(Register::Eax))],
        1,
        ProcedureTerminatorKind::Exit,
    )]);
    assert_eq!(p.functions[0].finite_maximum_work(), Ok(1));
    let ordinary = p.compile(&ProcedureCompileOptions::default()).unwrap();
    let computation = compile_finite(&p);
    assert_eq!(ordinary.wasm(), computation.wasm());
    assert_eq!(computation.finite_maximum_work(ProcedureId(3)), None);
    assert_eq!(computation.finite_loop_guards(ProcedureId(3)), None);
    assert_eq!(computation.finite_loop_guards(ProcedureId(99)), None);
    let mut runner = Runner::compiled(&ordinary);
    assert_eq!(
        compare(
            &mut runner,
            &[0x40, 0xcc],
            initial(),
            descriptor(),
            at(1),
            SIDE_EXIT,
            |_| {}
        ),
        1
    );
}

#[test]
fn finite_live_narrow_zero_wraps_once_and_preserves_upper_alias_bits() {
    for (counter, bytes, maximum) in [
        (Register::Bl, [0xfe, 0xcb], 513),
        (Register::Cx, [0x66, 0x49], 131_073),
    ] {
        let code = vec![bytes[0], bytes[1], 0x75, 0xfc, 0xc3];
        let p = graph(vec![
            block(
                0,
                vec![instruction(0, Operation::Dec(counter))],
                2,
                ProcedureTerminatorKind::Branch {
                    condition: Condition::NotEqual,
                    taken: at(0),
                    not_taken: at(4),
                },
            ),
            block(4, vec![], 4, ret(Width::Dword, 0)),
        ]);
        let compiled = compile_finite(&p);
        assert_eq!(compiled.finite_maximum_work(ProcedureId(3)), Some(maximum));
        let mut runner = Runner::compiled(&compiled);
        let mut frame = initial();
        frame.remaining_budget = 1;
        frame.gpr[counter.index() as usize] &= u32::MAX << counter.width().bits();
        let mut d = descriptor();
        d.inline_grant = 1;
        assert_eq!(
            compare_until(
                &mut runner,
                &code,
                frame,
                d,
                completed_address(),
                COMPLETE,
                Some(u64::from(maximum)),
                |_| {}
            ),
            u64::from(maximum)
        );
    }
}

#[test]
fn finite_guarded_unknown_dword_and_oversized_constant_countdowns() {
    let p = graph(vec![
        block(
            0,
            vec![instruction(0, Operation::Dec(Register::Ecx))],
            1,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(0),
                not_taken: at(3),
            },
        ),
        block(3, vec![], 3, ret(Width::Dword, 0)),
    ]);
    let maximum = 469_762_071;
    assert_eq!(p.functions[0].finite_maximum_work(), Ok(maximum));
    let compiled = compile_finite(&p);
    assert_eq!(compiled.finite_maximum_work(ProcedureId(3)), Some(maximum));
    assert_eq!(
        compiled.finite_loop_guards(ProcedureId(3)),
        Some(
            [FiniteLoopGuard {
                entry: ENTRY,
                counter: Register::Ecx,
                decrement: true,
                maximum_trips: 234_881_035
            },]
            .as_slice()
        )
    );
    for count in [1, 3, 40] {
        let mut runner = Runner::compiled(&compiled);
        let mut frame = initial();
        frame.gpr[1] = count;
        frame.remaining_budget = 1;
        let mut d = descriptor();
        d.inline_grant = 1;
        assert_eq!(
            compare(
                &mut runner,
                &[0x49, 0x75, 0xfd, 0xc3],
                frame,
                d,
                completed_address(),
                COMPLETE,
                |_| {}
            ),
            u64::from(2 * count + 1)
        );
    }
    for count in [0, maximum, u32::MAX] {
        let mut runner = Runner::compiled(&compiled);
        let mut frame = initial();
        frame.gpr[1] = count;
        assert_eq!(
            compare(
                &mut runner,
                &[0x49, 0x75, 0xfd, 0xc3],
                frame,
                descriptor(),
                ENTRY,
                SIDE_EXIT,
                |_| {}
            ),
            0
        );
    }
    // A constant initialization proves termination, but it must also establish
    // a work reservation which fits the owner's finite atomic ceiling.
    for constant in [0, u32::MAX, 300_000_000] {
        let mut p = p.clone();
        for block in &mut p.functions[0].blocks {
            block.entry.offset += 5;
            block.terminator.address.offset += 5;
            for instruction in &mut block.instructions {
                instruction.address.offset += 5;
            }
            if let ProcedureTerminatorKind::Branch {
                taken, not_taken, ..
            } = &mut block.terminator.kind
            {
                taken.offset += 5;
                not_taken.offset += 5;
            }
        }
        p.functions[0].blocks.push(block(
            0,
            vec![instruction(
                0,
                Operation::Move {
                    target: Register::Ecx,
                    source: Source::Immediate(constant),
                },
            )],
            5,
            ProcedureTerminatorKind::Fallthrough(at(5)),
        ));
        assert!(matches!(
            p.functions[0].finite_maximum_work(),
            Err(ProcedureCompileError::CyclicGraph(_))
        ));
    }
}

#[test]
fn finite_nested_unknown_dword_guard_refuses_second_zero_entry() {
    // ECX is initialized before the outer loop. The first inner countdown may
    // be short, but its second entry starts at zero and would take 2^32 trips.
    let p = graph(vec![
        block(
            0,
            vec![instruction(
                0,
                Operation::Move {
                    target: Register::Ecx,
                    source: Source::Immediate(3),
                },
            )],
            5,
            ProcedureTerminatorKind::Fallthrough(at(5)),
        ),
        block(
            5,
            vec![instruction(
                5,
                Operation::Move {
                    target: Register::Bl,
                    source: Source::Immediate(2),
                },
            )],
            7,
            ProcedureTerminatorKind::Fallthrough(at(7)),
        ),
        block(
            7,
            vec![instruction(7, Operation::Nop)],
            8,
            ProcedureTerminatorKind::Fallthrough(at(8)),
        ),
        block(
            8,
            vec![instruction(8, Operation::Dec(Register::Ecx))],
            9,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(8),
                not_taken: at(11),
            },
        ),
        block(
            11,
            vec![instruction(11, Operation::Dec(Register::Bl))],
            13,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(7),
                not_taken: at(15),
            },
        ),
        block(15, vec![], 15, ret(Width::Dword, 0)),
    ]);
    let compiled = compile_finite(&p);
    assert_eq!(
        compiled.finite_maximum_work(ProcedureId(3)),
        Some(469_762_069)
    );
    assert_eq!(
        compiled.finite_loop_guards(ProcedureId(3)),
        Some(
            [FiniteLoopGuard {
                entry: at(8),
                counter: Register::Ecx,
                decrement: true,
                maximum_trips: 117_440_515
            },]
            .as_slice()
        )
    );
    let code = vec![
        0xb9, 3, 0, 0, 0, 0xb3, 2, 0x90, 0x49, 0x75, 0xfd, 0xfe, 0xcb, 0x75, 0xf8, 0xc3,
    ];
    let mut runner = Runner::compiled(&compiled);
    assert_eq!(
        compare_until(
            &mut runner,
            &code,
            initial(),
            descriptor(),
            at(8),
            SIDE_EXIT,
            Some(12),
            |_| {}
        ),
        12
    );
}

#[test]
fn finite_guarded_increment_and_exact_cap_acceptance() {
    let p = graph(vec![
        block(
            0,
            vec![instruction(0, Operation::Inc(Register::Ecx))],
            1,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(0),
                not_taken: at(3),
            },
        ),
        block(3, vec![], 3, ret(Width::Dword, 0)),
    ]);
    let compiled = compile_finite(&p);
    assert_eq!(
        compiled.finite_loop_guards(ProcedureId(3)),
        Some(
            [FiniteLoopGuard {
                entry: ENTRY,
                counter: Register::Ecx,
                decrement: false,
                maximum_trips: 234_881_035
            },]
            .as_slice()
        )
    );
    let mut runner = Runner::compiled(&compiled);
    for trips in [1, 3, 40] {
        let mut frame = initial();
        frame.gpr[1] = 0u32.wrapping_sub(trips);
        assert_eq!(
            compare(
                &mut runner,
                &[0x41, 0x75, 0xfd, 0xc3],
                frame,
                descriptor(),
                completed_address(),
                COMPLETE,
                |_| {}
            ),
            u64::from(2 * trips + 1)
        );
    }
    for value in [0, 1, 0u32.wrapping_sub(234_881_036)] {
        let mut frame = initial();
        frame.gpr[1] = value;
        assert_eq!(
            compare(
                &mut runner,
                &[0x41, 0x75, 0xfd, 0xc3],
                frame,
                descriptor(),
                ENTRY,
                SIDE_EXIT,
                |_| {}
            ),
            0
        );
    }
    // At the automatically derived cap, admission succeeds and the ordinary
    // load guard refuses the first instruction. One beyond the cap declines
    // at the loop header before even the independent prefix INC.
    let p = graph(vec![
        block(
            0,
            vec![
                instruction(0, Operation::Inc(Register::Eax)),
                instruction(
                    1,
                    Operation::LoadMemory {
                        target: Register::Bl,
                        address: ds(0x7fff),
                    },
                ),
                instruction(7, Operation::Dec(Register::Ecx)),
            ],
            8,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(0),
                not_taken: at(10),
            },
        ),
        block(10, vec![], 10, ret(Width::Dword, 0)),
    ]);
    let compiled = compile_finite(&p);
    let cap = (469_762_071 - 1) / 4;
    let code = vec![0x40, 0x8a, 0x1d, 0xff, 0x7f, 0, 0, 0x49, 0x75, 0xf6, 0xc3];
    let mut runner = Runner::compiled(&compiled);
    for count in [cap, cap + 1] {
        let mut frame = initial();
        frame.gpr[1] = count;
        let accepted = count == cap;
        assert_eq!(
            compare(
                &mut runner,
                &code,
                frame,
                descriptor(),
                if accepted { at(1) } else { ENTRY },
                SIDE_EXIT,
                |_| {}
            ),
            u64::from(accepted)
        );
    }
}

#[test]
fn finite_guard_symbolic_envelope_handles_non_affine_longest_paths() {
    // For trips1 and2 the unrelated long arm wins. Sampling those longest-path
    // values would incorrectly infer zero slope for the other arm's countdown.
    let mut nops = vec![];
    for offset in 4..104 {
        nops.push(instruction(offset, Operation::Nop));
    }
    let p = graph(vec![
        block(
            0,
            vec![],
            0,
            ProcedureTerminatorKind::Branch {
                condition: Condition::Equal,
                taken: at(112),
                not_taken: at(4),
            },
        ),
        block(4, nops, 104, ProcedureTerminatorKind::Jump(at(128))),
        block(
            112,
            vec![instruction(112, Operation::Dec(Register::Ecx))],
            113,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(112),
                not_taken: at(128),
            },
        ),
        block(128, vec![], 128, ret(Width::Dword, 0)),
    ]);
    let compiled = compile_finite(&p);
    // Envelope 2*trips+103. It includes the longest constant arm, even though
    // that arm and the dynamic countdown are mutually exclusive.
    assert_eq!(
        compiled.finite_maximum_work(ProcedureId(3)),
        Some(469_762_071)
    );
    let mut code = vec![0x90; 129];
    put(&mut code, 0, &[0x74, 110]);
    put(&mut code, 104, &[0xeb, 22]);
    put(&mut code, 112, &[0x49, 0x75, 0xfd]);
    code[128] = 0xc3;
    // A natural fallthrough after the not-taken JNZ goes to128 in IR. Encode
    // the gap as JMP128, accounting it with a separate addressed block.
    let mut p = p;
    p.functions[0].blocks[2].terminator.kind = ProcedureTerminatorKind::Branch {
        condition: Condition::NotEqual,
        taken: at(112),
        not_taken: at(115),
    };
    p.functions[0].blocks.push(block(
        115,
        vec![],
        115,
        ProcedureTerminatorKind::Jump(at(128)),
    ));
    put(&mut code, 115, &[0xeb, 11]);
    let compiled = compile_finite(&p);
    let mut frame = initial();
    frame.eflags |= 0x40;
    frame.gpr[1] = u32::MAX;
    let mut runner = Runner::compiled(&compiled);
    assert_eq!(
        compare(
            &mut runner,
            &code,
            frame,
            descriptor(),
            at(112),
            SIDE_EXIT,
            |_| {}
        ),
        1
    );
}

#[test]
fn finite_two_unknown_dword_inductions_are_rejected() {
    let p = graph(vec![
        block(
            0,
            vec![instruction(0, Operation::Dec(Register::Ecx))],
            1,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(0),
                not_taken: at(3),
            },
        ),
        block(
            3,
            vec![instruction(3, Operation::Dec(Register::Edx))],
            4,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(3),
                not_taken: at(6),
            },
        ),
        block(6, vec![], 6, ret(Width::Dword, 0)),
    ]);
    assert!(matches!(
        p.functions[0].finite_maximum_work(),
        Err(ProcedureCompileError::CyclicGraph(_))
    ));
}

#[test]
fn finite_partial_byte_writes_preserve_independent_high_byte_induction() {
    // CL changes on every pass; its disjoint CH induction still wraps once.
    let code = vec![0xb1, 0x7a, 0xfe, 0xc5, 0x75, 0xfa, 0xc3];
    let p = graph(vec![
        block(
            0,
            vec![
                instruction(
                    0,
                    Operation::Move {
                        target: Register::Cl,
                        source: Source::Immediate(0x7a),
                    },
                ),
                instruction(2, Operation::Inc(Register::Ch)),
            ],
            4,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(0),
                not_taken: at(6),
            },
        ),
        block(6, vec![], 6, ret(Width::Dword, 0)),
    ]);
    let compiled = compile_finite(&p);
    assert_eq!(compiled.finite_maximum_work(ProcedureId(3)), Some(769));
    let mut frame = initial();
    frame.gpr[1] = (frame.gpr[1] & !0xff00) | 0xfd00;
    let mut runner = Runner::compiled(&compiled);
    assert_eq!(
        compare(
            &mut runner,
            &code,
            frame,
            descriptor(),
            completed_address(),
            COMPLETE,
            |_| {}
        ),
        10
    );
    let mut invalid = p;
    invalid.functions[0].blocks[0].instructions[0].operation = Operation::Move {
        target: Register::Cx,
        source: Source::Immediate(0x7a),
    };
    assert!(matches!(
        invalid.functions[0].finite_maximum_work(),
        Err(ProcedureCompileError::CyclicGraph(_))
    ));
}
