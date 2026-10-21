use super::*;
use crate::static_program::Condition;

fn at(offset: u32) -> Address {
    Address {
        offset: ENTRY.offset + offset,
        ..ENTRY
    }
}
fn instruction(offset: u32, operation: Operation) -> ProcedureInstruction {
    ProcedureInstruction {
        address: at(offset),
        operation,
    }
}
fn block(
    offset: u32,
    instructions: Vec<ProcedureInstruction>,
    end: u32,
    kind: ProcedureTerminatorKind,
) -> ProcedureBlock {
    ProcedureBlock {
        entry: at(offset),
        instructions,
        terminator: ProcedureTerminator {
            address: at(end),
            kind,
        },
    }
}
fn graph(blocks: Vec<ProcedureBlock>) -> ProcedureGraphProgram {
    ProcedureGraphProgram {
        functions: vec![ProcedureGraphFunction {
            id: ProcedureId(3),
            entry: ENTRY,
            blocks,
        }],
    }
}
fn ret(width: Width, pop_bytes: u16) -> ProcedureTerminatorKind {
    ProcedureTerminatorKind::Return { width, pop_bytes }
}
fn put(code: &mut [u8], offset: usize, bytes: &[u8]) {
    code[offset..offset + bytes.len()].copy_from_slice(bytes);
}

/// The fixture bytes are hand-encoded x86, independently of the graph emitter.
/// Both arms join; unequal lengths, AH/AX aliases and a RAM store are visible.
fn diamond(cc: Condition, produce_flags: bool) -> (ProcedureGraphProgram, Vec<u8>) {
    let mut code = vec![0xcc; 80];
    let mut prefix = vec![];
    let branch = if produce_flags {
        put(&mut code, 0, &[0x80, 0xd4, 1]); // ADC AH,1
        prefix.push(instruction(
            0,
            Operation::Alu {
                kind: Alu::Adc,
                target: Register::Ah,
                source: Source::Immediate(1),
            },
        ));
        3
    } else {
        0
    };
    let next = branch + 6;
    put(&mut code, branch as usize, &[0x0f, 0x80 | cc as u8]);
    put(
        &mut code,
        branch as usize + 2,
        &(32_i32 - next as i32).to_le_bytes(),
    );
    put(&mut code, next as usize, &[0xb4, 0x5a]); // MOV AH,5Ah
    put(&mut code, next as usize + 2, &[0xe9]);
    put(
        &mut code,
        next as usize + 3,
        &(64_i32 - (next + 7) as i32).to_le_bytes(),
    );
    put(&mut code, 32, &[0x66, 0xb8, 0xa5, 0x5a, 0xf7, 0xd8, 0xe9]); // MOV AX; NEG EAX; JMP
    put(&mut code, 39, &21_i32.to_le_bytes());
    put(&mut code, 64, &[0xa3, 0, 0x13, 0, 0, 0xc2, 8, 0]);
    (
        graph(vec![
            block(
                0,
                prefix,
                branch,
                ProcedureTerminatorKind::Branch {
                    condition: cc,
                    taken: at(32),
                    not_taken: at(next),
                },
            ),
            block(
                next,
                vec![instruction(
                    next,
                    Operation::Move {
                        target: Register::Ah,
                        source: Source::Immediate(0x5a),
                    },
                )],
                next + 2,
                ProcedureTerminatorKind::Jump(at(64)),
            ),
            block(
                32,
                vec![
                    instruction(
                        32,
                        Operation::Move {
                            target: Register::Ax,
                            source: Source::Immediate(0x5aa5),
                        },
                    ),
                    instruction(36, Operation::Neg(Register::Eax)),
                ],
                38,
                ProcedureTerminatorKind::Jump(at(64)),
            ),
            block(
                64,
                vec![instruction(
                    64,
                    Operation::StoreMemory {
                        width: Width::Dword,
                        source: Source::Register(Register::Eax),
                        address: ds(0x1300),
                    },
                )],
                69,
                ret(Width::Dword, 8),
            ),
        ]),
        code,
    )
}

fn condition_reference(cc: Condition, flags: u32) -> bool {
    let cf = flags & 1 != 0;
    let pf = flags & 4 != 0;
    let zf = flags & 0x40 != 0;
    let sf = flags & 0x80 != 0;
    let of = flags & 0x800 != 0;
    match cc {
        Condition::Overflow => of,
        Condition::NotOverflow => !of,
        Condition::Below => cf,
        Condition::AboveOrEqual => !cf,
        Condition::Equal => zf,
        Condition::NotEqual => !zf,
        Condition::BelowOrEqual => cf || zf,
        Condition::Above => !cf && !zf,
        Condition::Sign => sf,
        Condition::NotSign => !sf,
        Condition::Parity => pf,
        Condition::NotParity => !pf,
        Condition::Less => sf != of,
        Condition::GreaterOrEqual => sf == of,
        Condition::LessOrEqual => zf || sf != of,
        Condition::Greater => !zf && sf == of,
    }
}

/// Execute real x86 until the specified refused instruction or actual RET
/// target. Only the interpreter decides the path and completed instruction count.
fn compare(
    runner: &mut Runner,
    code: &[u8],
    frame: Frame,
    d: Descriptor,
    stop: Address,
    status: u32,
    setup: impl FnOnce(&mut Runner),
) -> u64 {
    compare_until(runner, code, frame, d, stop, status, None, setup)
}

#[allow(clippy::too_many_arguments)]
fn compare_until(
    runner: &mut Runner,
    code: &[u8],
    frame: Frame,
    d: Descriptor,
    stop: Address,
    status: u32,
    reference_steps: Option<u64>,
    setup: impl FnOnce(&mut Runner),
) -> u64 {
    runner.set(frame, &d);
    let stack = frame.gpr[4]
        & if d.stack_address == StackAddress::Bits16 {
            0xffff
        } else {
            u32::MAX
        };
    let stack = RAM + (stack - RAM_BASE) as usize;
    runner.bytes()[stack..stack + 4].copy_from_slice(&0x9123_u32.to_le_bytes());
    setup(runner);
    let (mut cpu, mut bus) = reference(code, true, d.stack_address, 0);
    bus.bytes[RAM_BASE as usize..RAM_BASE as usize + RAM_LENGTH]
        .copy_from_slice(&runner.bytes()[RAM..RAM + RAM_LENGTH]);
    cpu.flags = frame.eflags;
    cpu.registers.ip = frame.address.offset;
    for (reg, value) in REGS.into_iter().zip(frame.gpr) {
        cpu.registers.set32(reg, value);
    }
    let mut generations = core::array::from_fn::<_, PAGES, _>(|page| runner.generation(page));
    let mut writes = 0;
    let mut steps = 0;
    while reference_steps.map_or(cpu.registers.ip != stop.offset, |limit| steps < limit) {
        assert!(
            steps < reference_steps.unwrap_or(300),
            "reference did not reach {stop:?}"
        );
        bus.touched.fill(false);
        cpu.step(&mut bus).unwrap();
        steps += 1;
        for (generation, touched) in generations.iter_mut().zip(bus.touched) {
            if touched {
                *generation += 1;
                writes += 1;
            }
        }
    }
    let stop = if reference_steps.is_some() {
        Address {
            offset: cpu.registers.ip,
            ..stop
        }
    } else {
        stop
    };
    assert_eq!(runner.call(), status);
    assert_eq!(
        runner.frame(),
        Frame {
            gpr: core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
            eflags: cpu.flags,
            address: stop,
            charged_work: frame.charged_work.saturating_add(steps),
            status: frame.status
                | if frame.charged_work.checked_add(steps).is_none() {
                    abi::COUNTER_OVERFLOW
                } else {
                    0
                },
            ..frame
        }
    );
    assert_eq!(
        &runner.bytes()[RAM..RAM + RAM_LENGTH],
        &bus.bytes[RAM_BASE as usize..RAM_BASE as usize + RAM_LENGTH]
    );
    for (page, generation) in generations.into_iter().enumerate() {
        assert_eq!(runner.generation(page), generation, "page {page}");
    }
    assert_eq!(
        runner.effects(),
        memory::Effects {
            ram_writes: writes,
            inline_remaining: d.inline_grant,
            ..memory::Effects::default()
        }
    );
    steps
}
fn completed_address() -> Address {
    Address {
        offset: 0x9123,
        ..ENTRY
    }
}

#[path = "procedure_memory_finite_tests.rs"]
mod finite_tests;
#[path = "procedure_memory_work_limit_tests.rs"]
mod work_limit_tests;

/// Seven real instructions per iteration, independently hand-encoded. Two
/// byte stores use the low/high halves of the word loaded through ESI.
fn counted_copy(split: bool) -> (ProcedureGraphProgram, Vec<u8>) {
    let code = vec![
        0x66, 0x8b, 0x06, // MOV AX,[ESI]
        0x88, 0x07, // MOV [EDI],AL
        0x83, 0xc6, 2, // ADD ESI,2
        0x81, 0xc7, 0xc0, 0, 0, 0, // ADD EDI,192
        0x88, 0x67, 0xa0, // MOV [EDI-96],AH
        0x4b, // DEC EBX
        0x75, 0xec, // JNZ entry
        0xc3, // RET
    ];
    let instructions = vec![
        instruction(
            0,
            Operation::LoadMemory {
                target: Register::Ax,
                address: address(Some(Register::Esi), 0, Segment::Ds),
            },
        ),
        instruction(
            3,
            Operation::StoreMemory {
                width: Width::Byte,
                source: Source::Register(Register::Al),
                address: address(Some(Register::Edi), 0, Segment::Ds),
            },
        ),
        instruction(
            5,
            Operation::Alu {
                kind: Alu::Add,
                target: Register::Esi,
                source: Source::Immediate(2),
            },
        ),
        instruction(
            8,
            Operation::Alu {
                kind: Alu::Add,
                target: Register::Edi,
                source: Source::Immediate(192),
            },
        ),
        instruction(
            14,
            Operation::StoreMemory {
                width: Width::Byte,
                source: Source::Register(Register::Ah),
                address: address(Some(Register::Edi), (-96_i32) as u32, Segment::Ds),
            },
        ),
        instruction(17, Operation::Dec(Register::Ebx)),
    ];
    let latch = ProcedureTerminatorKind::Branch {
        condition: Condition::NotEqual,
        taken: at(0),
        not_taken: at(20),
    };
    let mut blocks = if split {
        instructions
            .iter()
            .enumerate()
            .map(|(i, instruction)| {
                let next = instructions
                    .get(i + 1)
                    .map_or(18, |next| next.address.offset - ENTRY.offset);
                block(
                    instruction.address.offset - ENTRY.offset,
                    vec![*instruction],
                    next,
                    ProcedureTerminatorKind::Fallthrough(at(next)),
                )
            })
            .collect::<Vec<_>>()
    } else {
        vec![block(0, instructions, 18, latch)]
    };
    if split {
        blocks.push(block(18, vec![], 18, latch));
    }
    blocks.push(block(20, vec![], 20, ret(Width::Dword, 0)));
    (graph(blocks), code)
}

fn copy_frame(iterations: u32) -> Frame {
    let mut frame = initial();
    frame.gpr[3] = iterations;
    frame.gpr[6] = 0x1000;
    frame.gpr[7] = 0x1800;
    frame
}

/// A memory-controlled loop whose bounds check may leave before the latch.
/// The two branch orientations and distinct normal/early exits are intentional.
fn early_exit_loop(exit_taken: bool, conditional_latch: bool) -> (ProcedureGraphProgram, Vec<u8>) {
    let body = if exit_taken { 9 } else { 16 };
    let exit = if exit_taken { 32 } else { 9 };
    let bound = u32::from(conditional_latch) * 2;
    let mut code = vec![0xcc; 48];
    put(&mut code, 0, &[0x83, 0x3d, 0, 0x10, 0, 0, bound as u8]);
    let branch = if exit_taken { 0x74 } else { 0x75 };
    let target = if exit_taken { exit } else { body };
    put(&mut code, 7, &[branch, (target - 9) as u8]);
    put(&mut code, body as usize, &[0x89, 0x07, 0x83, 0xc7, 4]);
    put(
        &mut code,
        body as usize + 5,
        &[0x83, 0x2d, 0, 0x10, 0, 0, 1],
    );
    put(
        &mut code,
        body as usize + 12,
        &[
            if conditional_latch { 0x75 } else { 0xeb },
            (-(body as i32 + 14)) as u8,
        ],
    );
    put(&mut code, exit as usize, &[0x43, 0xc3]); // Early exit: INC EBX; RET.
    let normal = body + 14;
    if conditional_latch {
        put(&mut code, normal as usize, &[0x4a, 0xc3]); // Normal exit: DEC EDX; RET.
    }
    let mut blocks = vec![
        block(
            0,
            vec![instruction(
                0,
                Operation::CompareMemory {
                    test: false,
                    address: ds(0x1000),
                    width: Width::Dword,
                    source: Source::Immediate(bound),
                },
            )],
            7,
            ProcedureTerminatorKind::Branch {
                condition: if exit_taken {
                    Condition::Equal
                } else {
                    Condition::NotEqual
                },
                taken: at(target),
                not_taken: at(if exit_taken { body } else { exit }),
            },
        ),
        block(
            body,
            vec![
                instruction(
                    body,
                    Operation::StoreMemory {
                        width: Width::Dword,
                        source: Source::Register(Register::Eax),
                        address: address(Some(Register::Edi), 0, Segment::Ds),
                    },
                ),
                instruction(
                    body + 2,
                    Operation::Alu {
                        kind: Alu::Add,
                        target: Register::Edi,
                        source: Source::Immediate(4),
                    },
                ),
                instruction(
                    body + 5,
                    Operation::AluMemoryStore {
                        kind: Alu::Sub,
                        address: ds(0x1000),
                        width: Width::Dword,
                        source: Source::Immediate(1),
                    },
                ),
            ],
            body + 12,
            if conditional_latch {
                ProcedureTerminatorKind::Branch {
                    condition: Condition::NotEqual,
                    taken: ENTRY,
                    not_taken: at(normal),
                }
            } else {
                ProcedureTerminatorKind::Jump(ENTRY)
            },
        ),
        block(
            exit,
            vec![instruction(exit, Operation::Inc(Register::Ebx))],
            exit + 1,
            ret(Width::Dword, 0),
        ),
    ];
    if conditional_latch {
        blocks.push(block(
            normal,
            vec![instruction(normal, Operation::Dec(Register::Edx))],
            normal + 1,
            ret(Width::Dword, 0),
        ));
    }
    (graph(blocks), code)
}

#[test]
fn bounded_early_exits_match_memory_flags_epochs_and_every_grant_prefix() {
    for exit_taken in [false, true] {
        for conditional_latch in [false, true] {
            let (program, code) = early_exit_loop(exit_taken, conditional_latch);
            let options = ProcedureCompileOptions::default();
            assert!(matches!(
                program.compile(&options),
                Err(ProcedureCompileError::CyclicGraph(_))
            ));
            let compiled = program.compile_with_work_limit(&options, 31).unwrap();
            assert_eq!(compiled.maximum_work(ProcedureId(3)), Some(31));
            let mut runner = Runner::compiled(&compiled);
            for counter in [0u32, 1, 2, 3, u32::MAX] {
                let full_work = if conditional_latch {
                    match counter {
                        1 => Some(8),
                        2 => Some(4),
                        3 => Some(10),
                        _ => None,
                    }
                } else {
                    counter.checked_mul(6).and_then(|n| n.checked_add(4))
                };
                for grant in 0..=34 {
                    let mut d = descriptor();
                    d.inline_grant = grant;
                    let work = grant.min(31).min(full_work.unwrap_or(u32::MAX));
                    let mut frame = initial();
                    frame.gpr[7] = 0x1800;
                    frame.eflags = 0xa03;
                    compare_until(
                        &mut runner,
                        &code,
                        frame,
                        d,
                        ENTRY,
                        if full_work == Some(work) {
                            COMPLETE
                        } else {
                            SIDE_EXIT
                        },
                        Some(u64::from(work)),
                        |runner| {
                            let offset = RAM + (0x1000 - RAM_BASE) as usize;
                            runner.bytes()[offset..offset + 4]
                                .copy_from_slice(&counter.to_le_bytes());
                        },
                    );
                }
            }
            let mut reverse = program.clone();
            reverse.functions[0].blocks.reverse();
            assert_eq!(
                compiled.wasm(),
                reverse
                    .compile_with_work_limit(&options, 31)
                    .unwrap()
                    .wasm()
            );
        }
    }
}

#[test]
fn bounded_early_exit_refusals_preserve_prior_writes_and_stop_before_the_fault() {
    let (program, code) = early_exit_loop(true, false);
    let compiled = program
        .compile_with_work_limit(&ProcedureCompileOptions::default(), 31)
        .unwrap();
    let mut runner = Runner::compiled(&compiled);
    for refusal in 0..3 {
        let mut d = descriptor();
        d.inline_grant = 31;
        let mut frame = initial();
        frame.gpr[7] = 0x1800;
        let (counter, work) = match refusal {
            0 => {
                d.windows[0][Segment::Ds as usize].last = 0xfff;
                (3u32, 0)
            }
            1 => {
                d.windows[1][Segment::Ds as usize].last = 0x1803;
                (3, 8)
            }
            _ => {
                d.windows[0][Segment::Ss as usize].valid = false;
                (1, 9)
            }
        };
        compare_until(
            &mut runner,
            &code,
            frame,
            d,
            ENTRY,
            SIDE_EXIT,
            Some(work),
            |runner| {
                let offset = RAM + (0x1000 - RAM_BASE) as usize;
                runner.bytes()[offset..offset + 4].copy_from_slice(&counter.to_le_bytes());
            },
        );
    }
}

#[test]
fn bounded_early_exit_does_not_admit_an_internal_diamond_or_second_cycle() {
    let (mut program, _) = early_exit_loop(true, false);
    // The apparent early exit now returns to the header: two continuing paths.
    program.functions[0].blocks[2].terminator.kind = ProcedureTerminatorKind::Jump(ENTRY);
    assert!(matches!(
        program.compile_with_work_limit(&ProcedureCompileOptions::default(), 31),
        Err(ProcedureCompileError::CyclicGraph(_))
    ));
    // A separate self-loop outside the body also remains unsupported.
    program.functions[0].blocks[2].terminator.kind = ProcedureTerminatorKind::Jump(at(32));
    assert!(matches!(
        program.compile_with_work_limit(&ProcedureCompileOptions::default(), 31),
        Err(ProcedureCompileError::CyclicGraph(_))
    ));
}

#[test]
fn bounded_counted_copy_all_prefixes_and_real_return_match_original_x86() {
    for split in [false, true] {
        let (program, code) = counted_copy(split);
        assert!(matches!(
            program.compile(&ProcedureCompileOptions::default()),
            Err(ProcedureCompileError::CyclicGraph(_))
        ));
        let compiled = program
            .compile_with_work_limit(&ProcedureCompileOptions::default(), 25)
            .unwrap();
        assert_eq!(compiled.maximum_work(ProcedureId(3)), Some(25));
        let mut runner = Runner::compiled(&compiled);
        for grant in 0..=30 {
            let mut d = descriptor();
            d.inline_grant = grant;
            let steps = u64::from(grant.min(22));
            compare_until(
                &mut runner,
                &code,
                copy_frame(3),
                d,
                ENTRY,
                if steps == 22 { COMPLETE } else { SIDE_EXIT },
                Some(steps),
                |runner| {
                    let source = RAM + (0x1000 - RAM_BASE) as usize;
                    runner.bytes()[source..source + 6].copy_from_slice(&[1, 2, 3, 4, 5, 6]);
                },
            );
        }
        // A caller cannot extend the compiled maximum by advertising a larger grant.
        let mut d = descriptor();
        d.inline_grant = u32::MAX;
        compare_until(
            &mut runner,
            &code,
            copy_frame(10),
            d,
            ENTRY,
            SIDE_EXIT,
            Some(25),
            |_| {},
        );
        let mut reverse = program.clone();
        reverse.functions[0].blocks.reverse();
        assert_eq!(
            compiled.wasm(),
            reverse
                .compile_with_work_limit(&ProcedureCompileOptions::default(), 25)
                .unwrap()
                .wasm()
        );
    }
}

#[test]
fn bounded_copy_refused_access_charges_prior_iterations_and_ret_stays_precise() {
    let (program, code) = counted_copy(true);
    let compiled = program
        .compile_with_work_limit(&ProcedureCompileOptions::default(), 64)
        .unwrap();
    let mut runner = Runner::compiled(&compiled);
    let mut d = descriptor();
    d.windows[0][Segment::Ds as usize].last = 0x1001;
    // Write windows remain broad; only the second iteration's read refuses.
    compare_until(
        &mut runner,
        &code,
        copy_frame(4),
        d,
        ENTRY,
        SIDE_EXIT,
        Some(7),
        |_| {},
    );
    let mut d = descriptor();
    d.windows[1][Segment::Ds as usize].last = 0x1800;
    // The high-byte store fails after the low-byte store and two ADDs commit.
    compare_until(
        &mut runner,
        &code,
        copy_frame(4),
        d,
        ENTRY,
        SIDE_EXIT,
        Some(4),
        |_| {},
    );
    let mut d = descriptor();
    d.windows[0][Segment::Ss as usize].valid = false;
    compare_until(
        &mut runner,
        &code,
        copy_frame(1),
        d,
        ENTRY,
        SIDE_EXIT,
        Some(7),
        |_| {},
    );
    let mut frame = copy_frame(4);
    frame.charged_work = u64::MAX - 3;
    let mut d = descriptor();
    d.inline_grant = 8;
    compare_until(
        &mut runner,
        &code,
        frame,
        d,
        ENTRY,
        SIDE_EXIT,
        Some(8),
        |_| {},
    );
}

#[test]
fn bounded_loop_options_acyclic_bytes_and_unsupported_shapes_are_explicit() {
    let options = ProcedureCompileOptions::default();
    let (acyclic, _) = diamond(Condition::Equal, false);
    assert_eq!(
        acyclic.compile(&options).unwrap().wasm(),
        acyclic.compile_with_work_limit(&options, 1).unwrap().wasm()
    );
    let (counted, _) = counted_copy(true);
    for limit in [0, MAX_WORK_PER_CALL + 1] {
        assert!(matches!(
            counted.compile_with_work_limit(&options, limit),
            Err(ProcedureCompileError::InvalidOptions)
        ));
    }
    let two_loops = graph(vec![
        block(
            0,
            vec![],
            0,
            ProcedureTerminatorKind::Branch {
                condition: Condition::Equal,
                taken: at(16),
                not_taken: at(32),
            },
        ),
        block(16, vec![], 16, ProcedureTerminatorKind::Jump(at(16))),
        block(32, vec![], 32, ProcedureTerminatorKind::Jump(at(32))),
    ]);
    assert!(matches!(
        two_loops.compile_with_work_limit(&options, 32),
        Err(ProcedureCompileError::CyclicGraph(_))
    ));
    let forever = graph(vec![block(
        0,
        vec![],
        0,
        ProcedureTerminatorKind::Jump(ENTRY),
    )]);
    let compiled = forever
        .compile_with_work_limit(&options, MAX_WORK_PER_CALL)
        .unwrap();
    assert_eq!(
        compiled.maximum_work(ProcedureId(3)),
        Some(u64::from(MAX_WORK_PER_CALL))
    );
    let mut runner = Runner::compiled(&compiled);
    let mut d = descriptor();
    d.inline_grant = 17;
    compare_until(
        &mut runner,
        &[0xeb, 0xfe],
        initial(),
        d,
        ENTRY,
        SIDE_EXIT,
        Some(17),
        |_| {},
    );
    // The loop edge may be the not-taken Jcc arm, and guest address order
    // need not match execution: start at +2; DEC EBX; JMP +0; JZ +8.
    let mut inverse = graph(vec![
        block(
            0,
            vec![],
            0,
            ProcedureTerminatorKind::Branch {
                condition: Condition::Equal,
                taken: at(8),
                not_taken: at(2),
            },
        ),
        block(
            2,
            vec![instruction(2, Operation::Dec(Register::Ebx))],
            3,
            ProcedureTerminatorKind::Jump(at(0)),
        ),
        block(8, vec![], 8, ret(Width::Dword, 0)),
    ]);
    inverse.functions[0].entry = at(2);
    let compiled = inverse.compile_with_work_limit(&options, 64).unwrap();
    let mut runner = Runner::compiled(&compiled);
    let mut frame = copy_frame(3);
    frame.address = at(2);
    compare_until(
        &mut runner,
        &[0x74, 6, 0x4b, 0xeb, 0xfb, 0xcc, 0xcc, 0xcc, 0xc3],
        frame,
        descriptor(),
        ENTRY,
        COMPLETE,
        Some(10),
        |_| {},
    );
}

#[test]
fn bounded_rotated_fallthrough_latch_is_declined_without_charging_synthetic_edges() {
    // +0: MOV EAX,1; +5: DEC EDX; +6: JMP +0.
    // Entry at +5 rotates the cycle so the last graph edge is the synthetic
    // fallthrough from +0 to +5. It is not a one-instruction guest branch.
    let code = [0xb8, 1, 0, 0, 0, 0x4a, 0xeb, 0xf8];
    let mut program = graph(vec![
        block(
            0,
            vec![instruction(
                0,
                Operation::Move {
                    target: Register::Eax,
                    source: Source::Immediate(1),
                },
            )],
            5,
            ProcedureTerminatorKind::Fallthrough(at(5)),
        ),
        block(
            5,
            vec![instruction(5, Operation::Dec(Register::Edx))],
            6,
            ProcedureTerminatorKind::Jump(at(0)),
        ),
    ]);
    let options = ProcedureCompileOptions::default();
    for reverse in [false, true] {
        if reverse {
            program.functions[0].blocks.reverse();
        }
        program.functions[0].entry = at(5);
        assert!(matches!(
            program.functions[0].validate_with_work_limit(17),
            Err(ProcedureCompileError::CyclicGraph(_))
        ));
        assert!(matches!(
            program.compile_with_work_limit(&options, 17),
            Err(ProcedureCompileError::CyclicGraph(_))
        ));
        // At the original header the same graph has a real JMP latch and is
        // supported. Every budget prefix must match exactly three guest
        // instructions per full iteration, without charging the fallthrough.
        program.functions[0].entry = at(0);
        let compiled = program.compile_with_work_limit(&options, 17).unwrap();
        let mut runner = Runner::compiled(&compiled);
        for grant in 0..=20 {
            let mut d = descriptor();
            d.inline_grant = grant;
            compare_until(
                &mut runner,
                &code,
                initial(),
                d,
                ENTRY,
                SIDE_EXIT,
                Some(u64::from(grant.min(17))),
                |_| {},
            );
        }
    }
}

#[test]
fn all_sixteen_conditions_flags_aliases_memory_and_unequal_paths_match_original_x86() {
    let conditions = [
        Condition::Overflow,
        Condition::NotOverflow,
        Condition::Below,
        Condition::AboveOrEqual,
        Condition::Equal,
        Condition::NotEqual,
        Condition::BelowOrEqual,
        Condition::Above,
        Condition::Sign,
        Condition::NotSign,
        Condition::Parity,
        Condition::NotParity,
        Condition::Less,
        Condition::GreaterOrEqual,
        Condition::LessOrEqual,
        Condition::Greater,
    ];
    for cc in conditions {
        for produce_flags in [false, true] {
            let (p, code) = diamond(cc, produce_flags);
            let compiled = p.compile(&ProcedureCompileOptions::default()).unwrap();
            assert_eq!(
                compiled.maximum_work(ProcedureId(3)),
                Some(6 + u64::from(produce_flags))
            );
            let mut runner = Runner::compiled(&compiled);
            for bits in 0..32 {
                let mut frame = initial();
                frame.eflags = 0x202
                    | (bits & 1)
                    | ((bits & 2) << 1)
                    | ((bits & 4) << 4)
                    | ((bits & 8) << 4)
                    | ((bits & 16) << 7);
                frame.gpr[0] = [0x1234_7fff, 0x8765_ffff, 0x8000_0000, u32::MAX][bits as usize % 4];
                let steps = compare(
                    &mut runner,
                    &code,
                    frame,
                    descriptor(),
                    completed_address(),
                    COMPLETE,
                    |_| {},
                );
                if !produce_flags {
                    assert_eq!(
                        steps,
                        if condition_reference(cc, frame.eflags) {
                            6
                        } else {
                            5
                        }
                    );
                }
            }
        }
    }
}

#[test]
fn multiple_return_widths_and_pop_adjustments_match_independent_stack_widths() {
    let (mut p, mut code) = diamond(Condition::Equal, false);
    p.functions[0].blocks[2].terminator.kind = ret(Width::Word, u16::MAX);
    put(&mut code, 38, &[0x66, 0xc2, 0xff, 0xff]);
    for stack in [StackAddress::Bits16, StackAddress::Bits32] {
        let mut d = descriptor();
        d.stack_address = stack;
        let compiled = p.compile(&ProcedureCompileOptions::default()).unwrap();
        assert_eq!(compiled.maximum_work(ProcedureId(3)), Some(5));
        let mut runner = Runner::compiled(&compiled);
        for zf in [0, 0x40] {
            let mut frame = initial();
            frame.eflags |= zf;
            if stack == StackAddress::Bits16 {
                frame.gpr[4] |= 0xdead_0000;
            }
            compare(
                &mut runner,
                &code,
                frame,
                d,
                completed_address(),
                COMPLETE,
                |_| {},
            );
        }
    }
}

#[test]
fn topological_order_is_independent_of_guest_address_order_and_input_order() {
    let mut code = vec![0xcc; 64];
    put(&mut code, 0, &[0xe9, 43, 0, 0, 0]);
    put(&mut code, 48, &[0xbb, 0x78, 0x56, 0x34, 0x12, 0xe9]);
    put(&mut code, 54, &(-42_i32).to_le_bytes());
    put(&mut code, 16, &[0x89, 0x1d, 0, 0x14, 0, 0, 0xc3]);
    let mut p = graph(vec![
        block(0, vec![], 0, ProcedureTerminatorKind::Jump(at(48))),
        block(
            16,
            vec![instruction(
                16,
                Operation::StoreMemory {
                    width: Width::Dword,
                    source: Source::Register(Register::Ebx),
                    address: ds(0x1400),
                },
            )],
            22,
            ret(Width::Dword, 0),
        ),
        block(
            48,
            vec![instruction(
                48,
                Operation::Move {
                    target: Register::Ebx,
                    source: Source::Immediate(0x1234_5678),
                },
            )],
            53,
            ProcedureTerminatorKind::Jump(at(16)),
        ),
    ]);
    let compiled = p.compile(&ProcedureCompileOptions::default()).unwrap();
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
        5
    );
    p.functions[0].blocks.reverse();
    assert_eq!(
        compiled.wasm(),
        p.compile(&ProcedureCompileOptions::default())
            .unwrap()
            .wasm()
    );
}

#[test]
fn taken_next_block_and_early_return_skip_other_arm_without_dispatch() {
    let mut code = vec![0xcc; 64];
    put(&mut code, 0, &[0xe9, 43, 0, 0, 0]);
    put(&mut code, 48, &[0x0f, 0x84]);
    put(&mut code, 50, &(-38_i32).to_le_bytes());
    put(&mut code, 16, &[0xf7, 0xd8, 0xc3]);
    put(&mut code, 54, &[0x43, 0xc3]);
    let p = graph(vec![
        block(0, vec![], 0, ProcedureTerminatorKind::Jump(at(48))),
        block(
            48,
            vec![],
            48,
            ProcedureTerminatorKind::Branch {
                condition: Condition::Equal,
                taken: at(16),
                not_taken: at(54),
            },
        ),
        block(
            16,
            vec![instruction(16, Operation::Neg(Register::Eax))],
            18,
            ret(Width::Dword, 0),
        ),
        block(
            54,
            vec![instruction(54, Operation::Inc(Register::Ebx))],
            55,
            ret(Width::Dword, 0),
        ),
    ]);
    let compiled = p.compile(&ProcedureCompileOptions::default()).unwrap();
    let mut runner = Runner::compiled(&compiled);
    for zf in [0, 0x40] {
        let mut frame = initial();
        frame.eflags |= zf;
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
            4
        );
    }
}

#[test]
fn synthetic_fallthrough_has_no_charge_and_same_target_branch_costs_one() {
    let mut code = vec![0xcc; 16];
    put(&mut code, 0, &[0xb4, 0x12, 0x74, 0, 0xc3]);
    let p = graph(vec![
        block(
            0,
            vec![instruction(
                0,
                Operation::Move {
                    target: Register::Ah,
                    source: Source::Immediate(0x12),
                },
            )],
            2,
            ProcedureTerminatorKind::Fallthrough(at(2)),
        ),
        block(
            2,
            vec![],
            2,
            ProcedureTerminatorKind::Branch {
                condition: Condition::Equal,
                taken: at(4),
                not_taken: at(4),
            },
        ),
        block(4, vec![], 4, ret(Width::Dword, 0)),
    ]);
    let compiled = p.compile(&ProcedureCompileOptions::default()).unwrap();
    assert_eq!(compiled.maximum_work(ProcedureId(3)), Some(3));
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
        3
    );
}

fn prefix_graph() -> (ProcedureGraphProgram, Vec<u8>) {
    let mut code = vec![0xcc; 112];
    put(
        &mut code,
        0,
        &[
            0xb8, 0x78, 0x56, 0x34, 0x12, 0xa3, 0, 0x13, 0, 0, 0x0f, 0x84, 48, 0, 0, 0,
        ],
    );
    put(&mut code, 16, &[0xb4, 0x9a, 0xe9, 73, 0, 0, 0]);
    put(
        &mut code,
        64,
        &[0xf7, 0xd8, 0xa3, 0, 0x14, 0, 0, 0xe9, 20, 0, 0, 0],
    );
    put(&mut code, 96, &[0xa3, 0xff, 0x1f, 0, 0, 0xc3]);
    (
        graph(vec![
            block(
                0,
                vec![
                    instruction(
                        0,
                        Operation::Move {
                            target: Register::Eax,
                            source: Source::Immediate(0x1234_5678),
                        },
                    ),
                    instruction(
                        5,
                        Operation::StoreMemory {
                            width: Width::Dword,
                            source: Source::Register(Register::Eax),
                            address: ds(0x1300),
                        },
                    ),
                ],
                10,
                ProcedureTerminatorKind::Branch {
                    condition: Condition::Equal,
                    taken: at(64),
                    not_taken: at(16),
                },
            ),
            block(
                16,
                vec![instruction(
                    16,
                    Operation::Move {
                        target: Register::Ah,
                        source: Source::Immediate(0x9a),
                    },
                )],
                18,
                ProcedureTerminatorKind::Jump(at(96)),
            ),
            block(
                64,
                vec![
                    instruction(64, Operation::Neg(Register::Eax)),
                    instruction(
                        66,
                        Operation::StoreMemory {
                            width: Width::Dword,
                            source: Source::Register(Register::Eax),
                            address: ds(0x1400),
                        },
                    ),
                ],
                71,
                ProcedureTerminatorKind::Jump(at(96)),
            ),
            block(
                96,
                vec![instruction(
                    96,
                    Operation::StoreMemory {
                        width: Width::Dword,
                        source: Source::Register(Register::Eax),
                        address: ds(0x1fff),
                    },
                )],
                101,
                ret(Width::Dword, 0),
            ),
        ]),
        code,
    )
}

#[test]
fn branch_arm_side_exits_preserve_exact_prefix_and_decline_all_cross_page_store_effects() {
    let (p, code) = prefix_graph();
    let compiled = p.compile(&ProcedureCompileOptions::default()).unwrap();
    assert_eq!(compiled.maximum_work(ProcedureId(3)), Some(8));
    let mut runner = Runner::compiled(&compiled);
    for zf in [0, 0x40] {
        for refusal in 0..4 {
            let mut frame = initial();
            frame.eflags |= zf;
            let mut d = descriptor();
            if refusal == 0 {
                d.ram.excluded_start = 0x2000;
                d.ram.excluded_length = 1;
            }
            let steps = compare(
                &mut runner,
                &code,
                frame,
                d,
                at(96),
                SIDE_EXIT,
                |r| match refusal {
                    1 => r.bytes()[PROTECTED + 2] = 1,
                    2 => r.set_generation(2, u64::MAX),
                    3 => r.set_generation(1, u64::MAX - if zf == 0 { 1 } else { 2 }),
                    _ => {}
                },
            );
            assert_eq!(steps, if zf == 0 { 5 } else { 6 });
        }
    }
    // A failed RET after all CFG stores preserves the full path, without RET charge.
    let mut d = descriptor();
    d.windows[0][Segment::Ss as usize].last = initial().gpr[4] - 1;
    compare(&mut runner, &code, initial(), d, at(101), SIDE_EXIT, |_| {});
}

#[test]
fn refusals_inside_either_arm_escape_all_pending_cfg_labels() {
    let (mut p, mut code) = prefix_graph();
    // The not-taken arm refuses a read, while the taken arm refuses a store.
    // Both must bypass the later merge and preserve the root's committed store.
    p.functions[0].blocks[1].instructions[0].operation = Operation::LoadMemory {
        target: Register::Ah,
        address: ds(0x3000),
    };
    p.functions[0].blocks[1].terminator.address = at(22);
    put(
        &mut code,
        16,
        &[0x8a, 0x25, 0, 0x30, 0, 0, 0xe9, 69, 0, 0, 0],
    );
    p.functions[0].blocks[2].instructions[1].operation = Operation::StoreMemory {
        width: Width::Dword,
        source: Source::Register(Register::Eax),
        address: ds(0x2fff),
    };
    put(&mut code, 66, &[0xa3, 0xff, 0x2f, 0, 0]);
    let compiled = p.compile(&ProcedureCompileOptions::default()).unwrap();
    let mut runner = Runner::compiled(&compiled);
    let mut d = descriptor();
    d.ram.excluded_start = 0x3000;
    d.ram.excluded_length = 1;
    for zf in [0, 0x40] {
        let mut frame = initial();
        frame.eflags |= zf;
        let steps = compare(
            &mut runner,
            &code,
            frame,
            d,
            at(if zf == 0 { 16 } else { 66 }),
            SIDE_EXIT,
            |_| {},
        );
        assert_eq!(steps, if zf == 0 { 3 } else { 4 });
    }
    // An early RET failure likewise exits while another arm label is pending.
    p.functions[0].blocks[2].instructions.truncate(1);
    p.functions[0].blocks[2].terminator.address = at(66);
    p.functions[0].blocks[2].terminator.kind = ret(Width::Dword, 0);
    let compiled = p.compile(&ProcedureCompileOptions::default()).unwrap();
    let mut runner = Runner::compiled(&compiled);
    let mut frame = initial();
    frame.eflags |= 0x40;
    d.windows[0][Segment::Ss as usize].last = frame.gpr[4] - 1;
    compare(&mut runner, &code, frame, d, at(66), SIDE_EXIT, |_| {});
}

#[test]
fn graph_execution_allocates_nothing_and_uses_the_shared_saturating_counter() {
    let (p, code) = diamond(Condition::Equal, false);
    let compiled = p.compile(&ProcedureCompileOptions::default()).unwrap();
    let mut runner = Runner::compiled(&compiled);
    let mut frame = initial();
    frame.charged_work = u64::MAX - 1;
    compare(
        &mut runner,
        &code,
        frame,
        descriptor(),
        completed_address(),
        COMPLETE,
        |_| {},
    );
    runner.set(frame, &descriptor());
    let stack = RAM + (frame.gpr[4] - RAM_BASE) as usize;
    runner.bytes()[stack..stack + 4].copy_from_slice(&0x9123_u32.to_le_bytes());
    let measured = allocation_counter::measure(|| assert_eq!(runner.call(), COMPLETE));
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
    assert_eq!(runner.frame().charged_work, u64::MAX);
    assert_eq!(runner.frame().status, frame.status | abi::COUNTER_OVERFLOW);
}

#[test]
fn graph_rejects_bad_addresses_edges_cycles_overlap_and_unreachable_blocks() {
    let (p, _) = diamond(Condition::Equal, false);
    let options = ProcedureCompileOptions::default();
    let mut bad = p.clone();
    bad.functions[0].blocks.clear();
    assert!(matches!(
        bad.compile(&options),
        Err(ProcedureCompileError::ProcedureHasNoBlocks(_))
    ));
    let mut bad = p.clone();
    let duplicate = bad.functions[0].blocks[0].clone();
    bad.functions[0].blocks.push(duplicate);
    assert!(matches!(
        bad.compile(&options),
        Err(ProcedureCompileError::DuplicateBlock { .. })
    ));
    let mut bad = p.clone();
    bad.functions[0].blocks[0].terminator.kind = ProcedureTerminatorKind::Jump(at(31));
    assert!(matches!(
        bad.compile(&options),
        Err(ProcedureCompileError::UnresolvedTarget { .. })
    ));
    let mut bad = p.clone();
    bad.functions[0].blocks[3].terminator.kind = ProcedureTerminatorKind::Jump(ENTRY);
    assert!(matches!(
        bad.compile(&options),
        Err(ProcedureCompileError::CyclicGraph(_))
    ));
    let mut bad = p.clone();
    bad.functions[0].blocks[3].terminator.kind = ProcedureTerminatorKind::Jump(at(64));
    assert!(matches!(
        bad.compile(&options),
        Err(ProcedureCompileError::CyclicGraph(_))
    ));
    let mut bad = p.clone();
    bad.functions[0]
        .blocks
        .push(block(100, vec![], 100, ret(Width::Dword, 0)));
    assert!(matches!(
        bad.compile(&options),
        Err(ProcedureCompileError::UnreachableBlock { .. })
    ));
    let mut bad = p.clone();
    bad.functions[0].blocks[1].terminator.address = at(33);
    assert!(matches!(
        bad.compile(&options),
        Err(ProcedureCompileError::InvalidAddresses(_))
    ));
    let mut bad = p.clone();
    bad.functions[0].blocks[1].instructions[0].address = at(7);
    assert!(matches!(
        bad.compile(&options),
        Err(ProcedureCompileError::InvalidAddresses(_))
    ));
    let mut bad = p.clone();
    bad.functions[0].blocks[3].terminator.kind = ret(Width::Byte, 0);
    assert!(matches!(
        bad.compile(&options),
        Err(ProcedureCompileError::InvalidReturn(_))
    ));
    let mut bad = p.clone();
    bad.functions[0].blocks[1].instructions[0].operation = Operation::Move {
        target: Register::Al,
        source: Source::Register(Register::Ax),
    };
    assert!(matches!(
        bad.compile(&options),
        Err(ProcedureCompileError::InvalidOperation { .. })
    ));
    let mut bad = p.clone();
    bad.functions[0].blocks[1].terminator.kind = ProcedureTerminatorKind::Fallthrough(at(64));
    assert!(matches!(
        bad.compile(&options),
        Err(ProcedureCompileError::InvalidAddresses(_))
    ));
    let bad = graph(vec![block(
        0,
        vec![],
        0,
        ProcedureTerminatorKind::Fallthrough(ENTRY),
    )]);
    assert!(matches!(
        bad.compile(&options),
        Err(ProcedureCompileError::InvalidAddresses(_))
    ));
}

#[test]
fn bounded_blocks_operations_output_and_deterministic_function_order() {
    let (p, _) = diamond(Condition::Equal, false);
    for options in [
        ProcedureCompileOptions {
            max_procedures: 0,
            ..ProcedureCompileOptions::default()
        },
        ProcedureCompileOptions {
            max_procedure_operations: 7,
            ..ProcedureCompileOptions::default()
        },
        ProcedureCompileOptions {
            max_total_operations: 7,
            ..ProcedureCompileOptions::default()
        },
        ProcedureCompileOptions {
            max_output_bytes: 8,
            ..ProcedureCompileOptions::default()
        },
        ProcedureCompileOptions {
            max_total_operations: MAX_TOTAL_OPERATIONS + 1,
            ..ProcedureCompileOptions::default()
        },
    ] {
        assert!(p.compile(&options).is_err());
    }
    let options = ProcedureCompileOptions::default();
    let mut chain = graph(
        (0..MAX_PROCEDURE_BLOCKS as u32)
            .map(|i| {
                block(
                    i * 5,
                    vec![],
                    i * 5,
                    if i + 1 == MAX_PROCEDURE_BLOCKS as u32 {
                        ret(Width::Dword, 0)
                    } else {
                        ProcedureTerminatorKind::Jump(at((i + 1) * 5))
                    },
                )
            })
            .collect(),
    );
    let compiled = chain.compile(&options).unwrap();
    assert_eq!(
        compiled.maximum_work(ProcedureId(3)),
        Some(MAX_PROCEDURE_BLOCKS as u64)
    );
    let mut code = vec![0xe9; MAX_PROCEDURE_BLOCKS * 5];
    for i in 0..MAX_PROCEDURE_BLOCKS - 1 {
        put(&mut code, i * 5 + 1, &0_i32.to_le_bytes());
    }
    code[(MAX_PROCEDURE_BLOCKS - 1) * 5] = 0xc3;
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
        MAX_PROCEDURE_BLOCKS as u64
    );
    chain.functions[0]
        .blocks
        .push(block(2000, vec![], 2000, ret(Width::Dword, 0)));
    assert!(matches!(
        chain.compile(&options),
        Err(ProcedureCompileError::ProcedureHasTooManyBlocks(_))
    ));
    let mut functions = p.clone();
    let mut other = functions.functions[0].clone();
    other.id = ProcedureId(1);
    functions.functions.push(other);
    let compiled = functions.compile(&options).unwrap();
    functions.functions.reverse();
    assert_eq!(compiled.wasm(), functions.compile(&options).unwrap().wasm());
    functions.functions[1].id = ProcedureId(1);
    assert!(matches!(
        functions.compile(&options),
        Err(ProcedureCompileError::DuplicateId(_))
    ));
}

#[path = "procedure_x87_tests.rs"]
mod pc24_tests;
