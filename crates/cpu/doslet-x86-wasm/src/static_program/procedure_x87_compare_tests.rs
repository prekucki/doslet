//! New x87 status/register forms, checked against independently encoded x86.
use super::*;

fn straight(pairs: &[(&[u8], X87Operation)]) -> (ProcedureGraphProgram, Vec<u8>) {
    let mut code = Vec::new();
    let mut instructions = Vec::new();
    for &(bytes, operation) in pairs {
        instructions.push(instruction(code.len() as u32, x87(bytes, operation)));
        code.extend_from_slice(bytes);
    }
    let end = code.len() as u32;
    code.push(0xcc);
    (
        graph(vec![block(
            0,
            instructions,
            end,
            ProcedureTerminatorKind::Exit,
        )]),
        code,
    )
}
fn seed_at(top: u8, rc: u16, values: &[f64], special_tag: bool) -> X87 {
    let mut seed = X87::default();
    seed.load_control(0x7f | (rc << 10));
    for _ in 0..((usize::from(top) + values.len()) & 7) {
        seed.rotate_top(true);
    }
    for &value in values.iter().rev() {
        seed.push(value).unwrap();
    }
    assert_eq!((seed.status() >> 11) & 7, u16::from(top));
    seed.restore_status((seed.status() & 0x3800) | 0x4720);
    seed.record_instruction(0xaabbccdd, 0x18, 0x123);
    seed.record_operand(0x11223344, 0x20);
    if special_tag && !values.is_empty() {
        let mut environment = seed.save_environment(true, true, false);
        let tag = (seed.tags() & !(3 << (top * 2))) | (2 << (top * 2));
        environment[8..10].copy_from_slice(&tag.to_le_bytes());
        seed.restore_environment(&environment, true, true, false);
    }
    seed
}

fn compare_run(
    runner: &mut Runner,
    compiled: &CompiledProcedureProgram,
    code: &[u8],
    seed: &X87,
    count: usize,
    memory_bits: u32,
) {
    let frame = initial();
    let d = descriptor();
    runner.set(frame, &d);
    let masks = put_fpu(runner, seed, compiled.x87(ProcedureId(3)).unwrap());
    let data = RAM + (0x1100 - RAM_BASE) as usize;
    runner.bytes()[data..data + 4].copy_from_slice(&memory_bits.to_le_bytes());
    let (mut cpu, mut bus) = reference(code, true, d.stack_address, 0);
    cpu.control.cr0 &= !0x0c;
    cpu.fpu = Some(seed.clone());
    cpu.flags = frame.eflags;
    cpu.registers.ip = ENTRY.offset;
    for (reg, value) in REGS.into_iter().zip(frame.gpr) {
        cpu.registers.set32(reg, value);
    }
    bus.bytes[RAM_BASE as usize..RAM_BASE as usize + RAM_LENGTH]
        .copy_from_slice(&runner.bytes()[RAM..RAM + RAM_LENGTH]);
    for _ in 0..count {
        cpu.step(&mut bus).unwrap();
    }
    assert_eq!(runner.call(), SIDE_EXIT);
    let actual = runner.frame();
    assert_eq!(actual.address.offset, cpu.registers.ip);
    assert_eq!(actual.charged_work, frame.charged_work + count as u64);
    assert_eq!(actual.eflags, cpu.flags);
    for (reg, value) in REGS.into_iter().zip(actual.gpr) {
        assert_eq!(value, cpu.registers.get32(reg));
    }
    assert_eq!(
        &runner.bytes()[RAM..RAM + RAM_LENGTH],
        &bus.bytes[RAM_BASE as usize..RAM_BASE as usize + RAM_LENGTH]
    );
    let actual = finish_fpu(runner, seed, masks);
    let expected = cpu.fpu.as_ref().unwrap();
    assert!(
        actual.bitwise_eq(expected),
        "initial {seed:?}\nactual {actual:?}\nexpected {expected:?}"
    );
}

#[test]
fn pc24_comparisons_and_fnstsw_match_original_x86_all_top_rounding_orders_and_pops() {
    let variants: [(&[u8], X87Operation, bool); 6] = [
        (
            &[0xd8, 0x15, 0, 0x11, 0, 0],
            X87Operation::CompareReal32 {
                address: ds(0x1100),
                pop: false,
            },
            true,
        ),
        (
            &[0xd8, 0x1d, 0, 0x11, 0, 0],
            X87Operation::CompareReal32 {
                address: ds(0x1100),
                pop: true,
            },
            true,
        ),
        (
            &[0xd8, 0xd1],
            X87Operation::CompareRegister { index: 1, pops: 0 },
            false,
        ),
        (
            &[0xd8, 0xd9],
            X87Operation::CompareRegister { index: 1, pops: 1 },
            false,
        ),
        (
            &[0xde, 0xd9],
            X87Operation::CompareRegister { index: 1, pops: 2 },
            false,
        ),
        (
            &[0xd8, 0xd0],
            X87Operation::CompareRegister { index: 0, pops: 0 },
            false,
        ),
    ];
    for (bytes, operation, memory) in variants {
        let (program, code) = straight(&[
            (bytes, operation),
            (&[0xdf, 0xe0], X87Operation::StoreStatusAx),
        ]);
        let compiled = program
            .compile_with_pc24_locals_and_work_limits(
                &ProcedureCompileOptions::default(),
                128,
                4096,
            )
            .unwrap();
        let mut runner = Runner::compiled(&compiled);
        for top in 0..8 {
            for rc in 0..4 {
                for (left, right) in [
                    (-3.5, 1.5),
                    (-0.0, 0.0),
                    (0.0, -0.0),
                    (1.5, 1.5),
                    (16_777_217.0, -2.5),
                ] {
                    let seed = seed_at(top, rc, &[left, right], true);
                    compare_run(
                        &mut runner,
                        &compiled,
                        &code,
                        &seed,
                        2,
                        (right as f32).to_bits(),
                    );
                    let status = runner.frame().gpr[0] as u16;
                    let expected =
                        if matches!(operation, X87Operation::CompareRegister { index: 0, .. })
                            || left == right
                        {
                            0x4000
                        } else if left < right {
                            0x100
                        } else {
                            0
                        };
                    assert_eq!(status & 0x4700, expected);
                    assert_eq!(
                        runner.frame().gpr[0] & 0xffff0000,
                        initial().gpr[0] & 0xffff0000
                    );
                    if memory {
                        assert_eq!(compiled.x87(ProcedureId(3)).unwrap().read_mask, 1);
                    }
                }
            }
        }
    }
}

#[test]
fn pc24_fnstsw_alone_leaves_all_fpu_state_and_pointers_unchanged_even_for_unread_special_values() {
    let (program, code) = straight(&[(&[0xdf, 0xe0], X87Operation::StoreStatusAx)]);
    let compiled = program
        .compile_with_pc24_locals_and_work_limits(&ProcedureCompileOptions::default(), 128, 4096)
        .unwrap();
    assert_eq!(
        compiled.x87(ProcedureId(3)),
        Some(X87Procedure {
            read_mask: 0,
            write_mask: 0
        })
    );
    let mut runner = Runner::compiled(&compiled);
    for top in 0..8 {
        for value in [f64::NAN, f64::INFINITY, -0.0, 1.5] {
            let seed = seed_at(top, 0, &[value], false);
            compare_run(&mut runner, &compiled, &code, &seed, 1, 0);
            assert_eq!(runner.frame().gpr[0] & 0xffff, u32::from(seed.status()));
        }
    }
}

#[test]
fn pc24_fst_fstp_register_copy_full_initial_values_tags_to_empty_and_occupied_slots() {
    for index in [0, 1, 3, 7] {
        for pop in [false, true] {
            let bytes = [0xdd, (if pop { 0xd8 } else { 0xd0 }) | index];
            let (program, code) = straight(&[
                (&bytes, X87Operation::StoreRegister { index, pop }),
                (&[0xdf, 0xe0], X87Operation::StoreStatusAx),
            ]);
            let compiled = program
                .compile_with_pc24_locals_and_work_limits(
                    &ProcedureCompileOptions::default(),
                    128,
                    4096,
                )
                .unwrap();
            let mut runner = Runner::compiled(&compiled);
            for top in 0..8 {
                for value in [-0.0, 16_777_217.0, -1.234567890123456, 2f64.powi(300)] {
                    for occupied in [false, true] {
                        let mut values = vec![value];
                        if occupied {
                            values.resize(8, 19.0);
                        }
                        let seed = seed_at(top, 3, &values, true);
                        compare_run(&mut runner, &compiled, &code, &seed, 2, 0);
                        assert_eq!(runner.frame().gpr[0] & 0x200, 0);
                    }
                }
            }
        }
    }
}

#[test]
fn pc24_compare_memory_refusal_preserves_status_pointers_and_pop_after_scalar_prefix() {
    for pop in [false, true] {
        let opcode = [0xd8, if pop { 0x1d } else { 0x15 }, 0, 0x11, 0, 0];
        let mut compare = straight(&[(
            &opcode,
            X87Operation::CompareReal32 {
                address: ds(0x1100),
                pop,
            },
        )]);
        // Add a real INC EAX prefix independently of the x87 lowering.
        compare.0.functions[0].blocks[0]
            .instructions
            .insert(0, instruction(0, Operation::Inc(Register::Eax)));
        compare.0.functions[0].blocks[0].instructions[1].address = at(1);
        compare.0.functions[0].blocks[0].terminator.address = at(7);
        compare.1.insert(0, 0x40);
        let compiled = compare
            .0
            .compile_with_pc24_locals_and_work_limits(
                &ProcedureCompileOptions::default(),
                128,
                4096,
            )
            .unwrap();
        let mut runner = Runner::compiled(&compiled);
        for bits in [f32::NAN.to_bits(), f32::INFINITY.to_bits(), 1, 0x80000001] {
            let seed = seed_at(3, 0, &[1.5], false);
            let frame = initial();
            runner.set(frame, &descriptor());
            let masks = put_fpu(&mut runner, &seed, compiled.x87(ProcedureId(3)).unwrap());
            let data = RAM + (0x1100 - RAM_BASE) as usize;
            runner.bytes()[data..data + 4].copy_from_slice(&bits.to_le_bytes());
            assert_eq!(runner.call(), SIDE_EXIT);
            assert_eq!(runner.frame().address, at(1));
            assert_eq!(runner.frame().charged_work, frame.charged_work + 1);
            assert_eq!(runner.frame().gpr[0], frame.gpr[0] + 1);
            assert!(finish_fpu(&mut runner, &seed, masks).bitwise_eq(&seed));
        }
    }
}

#[test]
fn pc24_invalid_compare_or_store_register_operands_decline_before_emission() {
    for operation in [
        X87Operation::CompareRegister { index: 8, pops: 0 },
        X87Operation::CompareRegister { index: 1, pops: 3 },
        X87Operation::CompareRegister { index: 0, pops: 2 },
        X87Operation::StoreRegister {
            index: 8,
            pop: true,
        },
        X87Operation::Duplicate { index: 255 },
        X87Operation::Exchange { index: 255 },
        X87Operation::ArithmeticRegister {
            index: 255,
            operation: 0,
            target_top: true,
            pop: false,
        },
        X87Operation::ArithmeticRegister {
            index: 0,
            operation: 3,
            target_top: true,
            pop: false,
        },
        X87Operation::ArithmeticReal32 {
            address: ds(0x1100),
            operation: 255,
        },
        X87Operation::CompareRegister {
            index: 1,
            pops: 255,
        },
    ] {
        let (program, _) = straight(&[(&[0xde, 0xd9], operation)]);
        assert!(matches!(
            program.compile_with_pc24_locals_and_work_limits(
                &ProcedureCompileOptions::default(),
                128,
                4096
            ),
            Err(ProcedureCompileError::InvalidOperation { .. })
        ));
        assert!(matches!(
            program.functions[0].validate_with_pc24_work_limit(128),
            Err(ProcedureCompileError::InvalidOperation { .. })
        ));
    }
}

#[test]
fn pc24_invalid_fop_declines_before_stack_planning() {
    let (mut program, _) = straight(&[(&[0xdf, 0xe0], X87Operation::StoreStatusAx)]);
    program.functions[0].blocks[0].instructions[0].operation = Operation::X87(X87Instruction {
        opcode: 0x800,
        operation: X87Operation::StoreStatusAx,
    });
    assert!(matches!(
        program.compile_with_pc24_locals_and_work_limits(
            &ProcedureCompileOptions::default(),
            128,
            4096
        ),
        Err(ProcedureCompileError::InvalidOperation { .. })
    ));
    assert!(matches!(
        program.functions[0].validate_with_pc24_work_limit(128),
        Err(ProcedureCompileError::InvalidOperation { .. })
    ));
}
