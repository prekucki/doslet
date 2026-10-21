//! Execute emitted WASM against independently encoded original x86, including
//! partial loop grants and the complete portable retained-state commit.
use super::*;
use crate::static_program::{X87Instruction, X87Operation};
use doslet_x87::{X87, pc24_boundary as wire};

fn x87(bytes: &[u8], operation: X87Operation) -> Operation {
    Operation::X87(X87Instruction {
        opcode: (u16::from(bytes[0] & 7) << 8) | u16::from(bytes[1]),
        operation,
    })
}
fn mixed_loop() -> (ProcedureGraphProgram, Vec<u8>) {
    let mut code = Vec::new();
    let mut instructions = Vec::new();
    let mut emit = |bytes: &[u8], operation| {
        instructions.push(instruction(code.len() as u32, operation));
        code.extend_from_slice(bytes);
    };
    let a = [0xd9, 0x05, 0x00, 0x11, 0, 0];
    emit(
        &a,
        x87(
            &a,
            X87Operation::LoadReal32 {
                address: ds(0x1100),
            },
        ),
    );
    let a = [0xdb, 0x05, 0x04, 0x11, 0, 0];
    emit(
        &a,
        x87(
            &a,
            X87Operation::LoadInteger32 {
                address: ds(0x1104),
            },
        ),
    );
    let a = [0xd9, 0xc9];
    emit(&a, x87(&a, X87Operation::Exchange { index: 1 }));
    let a = [0xd8, 0xc9];
    emit(
        &a,
        x87(
            &a,
            X87Operation::ArithmeticRegister {
                index: 1,
                operation: 1,
                target_top: true,
                pop: false,
            },
        ),
    );
    let a = [0xde, 0xc1];
    emit(
        &a,
        x87(
            &a,
            X87Operation::ArithmeticRegister {
                index: 1,
                operation: 0,
                target_top: false,
                pop: true,
            },
        ),
    );
    let a = [0xdb, 0x1d, 0x08, 0x11, 0, 0];
    emit(
        &a,
        x87(
            &a,
            X87Operation::StoreInteger32 {
                address: ds(0x1108),
                pop: true,
            },
        ),
    );
    emit(&[0x40], Operation::Inc(Register::Eax));
    emit(&[0x49], Operation::Dec(Register::Ecx));
    let latch = code.len() as u32;
    code.extend_from_slice(&[0x75, (-(latch as i32 + 2)) as u8]);
    let end = code.len() as u32;
    code.push(0xcc);
    (
        graph(vec![
            block(
                0,
                instructions,
                latch,
                ProcedureTerminatorKind::Branch {
                    condition: Condition::NotEqual,
                    taken: at(0),
                    not_taken: at(end),
                },
            ),
            block(end, vec![], end, ProcedureTerminatorKind::Exit),
        ]),
        code,
    )
}
fn put_fpu(runner: &mut Runner, seed: &X87, permissions: X87Procedure) -> (u8, u8) {
    let top = u32::from((seed.status() >> 11) & 7);
    let read = permissions.read_mask.rotate_left(top);
    let write = permissions.write_mask.rotate_left(top);
    let offset = FRAME + X87_BOUNDARY_OFFSET as usize;
    assert!(seed.write_pc24_boundary(
        read,
        write,
        &mut runner.bytes()[offset..offset + wire::SIZE]
    ));
    for (i, selector) in [24u16, 8, 16, 24, 0, 0].into_iter().enumerate() {
        let offset = FRAME + X87_SELECTORS_OFFSET as usize + i * 2;
        runner.bytes()[offset..offset + 2].copy_from_slice(&selector.to_le_bytes());
    }
    (read, write)
}
fn finish_fpu(runner: &mut Runner, seed: &X87, masks: (u8, u8)) -> X87 {
    let offset = FRAME + X87_BOUNDARY_OFFSET as usize;
    let mut actual = seed.clone();
    actual
        .commit_pc24_boundary(
            masks.0,
            masks.1,
            &runner.bytes()[offset..offset + wire::SIZE],
        )
        .unwrap();
    actual
}

#[test]
fn pc24_mixed_bounded_loop_all_rounding_top_and_partial_prefixes_match_original_x86() {
    let (program, code) = mixed_loop();
    for limit in [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 17, 27, 28] {
        let compiled = program
            .compile_with_pc24_locals_and_work_limits(
                &ProcedureCompileOptions::default(),
                limit,
                4096,
            )
            .unwrap();
        let permissions = compiled.x87(ProcedureId(3)).unwrap();
        assert_eq!(
            permissions,
            X87Procedure {
                read_mask: 0,
                write_mask: 0xc0
            }
        );
        assert_eq!(compiled.finite_maximum_work(ProcedureId(3)), None);
        let mut runner = Runner::compiled(&compiled);
        for rc in 0..4u16 {
            for top in 0..8 {
                let mut seed = X87::default();
                seed.load_control(0x7f | (rc << 10));
                for _ in 0..top {
                    seed.rotate_top(true);
                }
                seed.record_instruction(0x1234, 0x8, 0x123);
                seed.record_operand(0x5678, 0x18);
                let mut frame = initial();
                frame.gpr[1] = 3;
                let mut d = descriptor();
                d.inline_grant = limit;
                runner.set(frame, &d);
                let masks = put_fpu(&mut runner, &seed, permissions);
                let data = RAM + (0x1100 - RAM_BASE) as usize;
                runner.bytes()[data..data + 4]
                    .copy_from_slice(&1.5000001f32.to_bits().to_le_bytes());
                runner.bytes()[data + 4..data + 8].copy_from_slice(&16_777_217i32.to_le_bytes());
                let (mut cpu, mut bus) = reference(&code, true, d.stack_address, 0);
                cpu.fpu = Some(seed.clone());
                cpu.control.cr0 &= !0x0c;
                cpu.flags = frame.eflags;
                cpu.registers.ip = ENTRY.offset;
                for (reg, value) in REGS.into_iter().zip(frame.gpr) {
                    cpu.registers.set32(reg, value);
                }
                bus.bytes[RAM_BASE as usize..RAM_BASE as usize + RAM_LENGTH]
                    .copy_from_slice(&runner.bytes()[RAM..RAM + RAM_LENGTH]);
                let count = limit.min(27);
                for _ in 0..count {
                    cpu.step(&mut bus).unwrap();
                }
                assert_eq!(runner.call(), SIDE_EXIT);
                let actual = runner.frame();
                assert_eq!(actual.address.offset, cpu.registers.ip, "limit {limit}");
                assert_eq!(actual.charged_work, frame.charged_work + u64::from(count));
                assert_eq!(actual.eflags, cpu.flags);
                for (reg, value) in REGS.into_iter().zip(actual.gpr) {
                    assert_eq!(value, cpu.registers.get32(reg));
                }
                assert_eq!(
                    &runner.bytes()[RAM..RAM + RAM_LENGTH],
                    &bus.bytes[RAM_BASE as usize..RAM_BASE as usize + RAM_LENGTH]
                );
                let actual = finish_fpu(&mut runner, &seed, masks);
                let expected = cpu.fpu.as_ref().unwrap();
                assert!(
                    actual.bitwise_eq(expected),
                    "limit {limit}, rc {rc}, top {top}\nactual {actual:?}\nexpected {expected:?}"
                );
            }
        }
    }
}

#[test]
fn pc24_legacy_apis_refuse_and_unbalanced_stack_cycles_or_joins_decline() {
    let (program, _) = mixed_loop();
    let options = ProcedureCompileOptions::default();
    assert!(
        program
            .compile_with_computation_kernels(&options, 128)
            .is_err()
    );
    assert!(
        program.functions[0]
            .validate_with_pc24_work_limit(128)
            .is_ok()
    );
    let a = [0xd9, 0xc0];
    let unbalanced = graph(vec![block(
        0,
        vec![instruction(
            0,
            x87(&a, X87Operation::Duplicate { index: 0 }),
        )],
        2,
        ProcedureTerminatorKind::Jump(at(0)),
    )]);
    assert!(
        unbalanced.functions[0]
            .validate_with_pc24_work_limit(128)
            .is_err()
    );
    let join = graph(vec![
        block(
            0,
            vec![],
            0,
            ProcedureTerminatorKind::Branch {
                condition: Condition::Equal,
                taken: at(2),
                not_taken: at(4),
            },
        ),
        block(
            2,
            vec![instruction(
                2,
                x87(&a, X87Operation::Duplicate { index: 0 }),
            )],
            3,
            ProcedureTerminatorKind::Jump(at(6)),
        ),
        block(
            4,
            vec![instruction(4, Operation::Nop)],
            5,
            ProcedureTerminatorKind::Jump(at(6)),
        ),
        block(6, vec![], 6, ProcedureTerminatorKind::Exit),
    ]);
    assert!(
        join.functions[0]
            .validate_with_pc24_work_limit(128)
            .is_err()
    );
}

#[test]
fn pc24_numeric_and_store_refusals_commit_only_the_completed_prefix() {
    let (program, _) = mixed_loop();
    let compiled = program
        .compile_with_pc24_locals_and_work_limits(&ProcedureCompileOptions::default(), 128, 4096)
        .unwrap();
    let permissions = compiled.x87(ProcedureId(3)).unwrap();
    let mut runner = Runner::compiled(&compiled);
    for case in 0..5 {
        let mut seed = X87::default();
        seed.load_control(0x7f);
        let mut d = descriptor();
        if case == 2 {
            d.windows[1][Segment::Ds as usize].last = 0x1107;
        }
        let mut frame = initial();
        frame.gpr[1] = 1;
        runner.set(frame, &d);
        let masks = put_fpu(&mut runner, &seed, permissions);
        let data = RAM + (0x1100 - RAM_BASE) as usize;
        let value = match case {
            0 => f32::NAN,
            1 => f32::from_bits(1),
            4 => f32::MAX,
            _ => 1.5,
        };
        runner.bytes()[data..data + 4].copy_from_slice(&value.to_bits().to_le_bytes());
        runner.bytes()[data + 4..data + 8].copy_from_slice(&16_777_217i32.to_le_bytes());
        if case == 3 {
            runner.bytes()[PROTECTED + 1] = 1;
        }
        let before = runner.bytes()[RAM..RAM + RAM_LENGTH].to_vec();
        assert_eq!(runner.call(), SIDE_EXIT);
        let count = if case <= 1 { 0 } else { 5 };
        assert_eq!(runner.frame().charged_work, frame.charged_work + count);
        assert_eq!(&runner.bytes()[RAM..RAM + RAM_LENGTH], before);
        let mut expected = seed.clone();
        if count != 0 {
            let mut boundary = expected.pc24_region(0, 0xc0).unwrap();
            assert!(boundary.load_real32(value.to_bits()));
            assert!(boundary.load_integer32(16_777_217));
            assert!(boundary.exchange(1));
            assert!(boundary.arithmetic_top(1, 1));
            assert!(boundary.arithmetic_into(1, 0, true));
            boundary.record_instruction(ENTRY.offset + 16, 8, 0x6c1);
            boundary.record_operand(0x1104, 24);
            boundary.commit();
        }
        let actual = finish_fpu(&mut runner, &seed, masks);
        assert!(
            actual.bitwise_eq(&expected),
            "case {case}\n{actual:?}\n{expected:?}"
        );
    }
}

#[test]
fn pc24_duplicate_and_exchange_preserve_full_initial_provenance_and_noncanonical_tags() {
    let pairs = [
        (&[0xd9, 0xc0][..], X87Operation::Duplicate { index: 0 }),
        (&[0xd9, 0xc9][..], X87Operation::Exchange { index: 1 }),
        (
            &[0xdb, 0x1d, 0x00, 0x11, 0, 0][..],
            X87Operation::StoreInteger32 {
                address: ds(0x1100),
                pop: true,
            },
        ),
        (
            &[0xdb, 0x1d, 0x04, 0x11, 0, 0][..],
            X87Operation::StoreInteger32 {
                address: ds(0x1104),
                pop: true,
            },
        ),
    ];
    let mut code = Vec::new();
    let mut instructions = Vec::new();
    for (bytes, operation) in pairs {
        instructions.push(instruction(code.len() as u32, x87(bytes, operation)));
        code.extend_from_slice(bytes);
    }
    let end = code.len() as u32;
    code.push(0xcc);
    let program = graph(vec![block(
        0,
        instructions,
        end,
        ProcedureTerminatorKind::Exit,
    )]);
    let compiled = program
        .compile_with_pc24_locals_and_work_limits(&ProcedureCompileOptions::default(), 128, 4096)
        .unwrap();
    let permissions = compiled.x87(ProcedureId(3)).unwrap();
    assert_eq!(permissions.read_mask, 1);
    let mut seed = X87::default();
    seed.load_control(0x7f);
    seed.push_integer(16_777_217).unwrap();
    let mut environment = seed.save_environment(true, true, false);
    let tags = (seed.tags() & !(3 << 14)) | (2 << 14);
    environment[8..10].copy_from_slice(&tags.to_le_bytes());
    seed.restore_environment(&environment, true, true, false);
    let mut runner = Runner::compiled(&compiled);
    let frame = initial();
    let d = descriptor();
    runner.set(frame, &d);
    let masks = put_fpu(&mut runner, &seed, permissions);
    let (mut cpu, mut bus) = reference(&code, true, d.stack_address, 0);
    cpu.fpu = Some(seed.clone());
    cpu.control.cr0 &= !0x0c;
    cpu.registers.ip = ENTRY.offset;
    for _ in 0..4 {
        cpu.step(&mut bus).unwrap();
    }
    assert_eq!(runner.call(), SIDE_EXIT);
    assert_eq!(runner.frame().charged_work, frame.charged_work + 4);
    let actual = finish_fpu(&mut runner, &seed, masks);
    assert!(
        actual.bitwise_eq(cpu.fpu.as_ref().unwrap()),
        "{actual:?}\n{:?}",
        cpu.fpu
    );
    let offset = RAM + (0x1100 - RAM_BASE) as usize;
    assert_eq!(
        &runner.bytes()[offset..offset + 8],
        &bus.bytes[0x1100..0x1108]
    );
}

#[test]
fn pc24_extended_frame_alias_refuses_before_scalar_or_fpu_effects() {
    let (program, _) = mixed_loop();
    let compiled = program
        .compile_with_pc24_locals_and_work_limits(&ProcedureCompileOptions::default(), 128, 4096)
        .unwrap();
    let permissions = compiled.x87(ProcedureId(3)).unwrap();
    let mut runner = Runner::compiled(&compiled);
    for case in 0..4 {
        let mut d = descriptor();
        let overlap = FRAME as u32 + 600;
        match case {
            0 => d.ram.pointer = overlap,
            1 => d.ram.generations_pointer = overlap,
            2 => d.ram.code_pages_pointer = overlap,
            _ => {
                d.device.pointer = overlap;
                d.device.backing_length = 1;
            }
        }
        let frame = initial();
        runner.set(frame, &d);
        let mut seed = X87::default();
        seed.load_control(0x7f);
        let masks = put_fpu(&mut runner, &seed, permissions);
        let before = runner.bytes()
            [FRAME + X87_BOUNDARY_OFFSET as usize..FRAME + X87_SELECTORS_OFFSET as usize]
            .to_vec();
        assert_eq!(runner.call(), SIDE_EXIT);
        assert_eq!(runner.frame().charged_work, frame.charged_work);
        assert_eq!(runner.frame().address, frame.address);
        assert_eq!(
            &runner.bytes()
                [FRAME + X87_BOUNDARY_OFFSET as usize..FRAME + X87_SELECTORS_OFFSET as usize],
            before
        );
        assert!(finish_fpu(&mut runner, &seed, masks).bitwise_eq(&seed));
    }
}

#[path = "procedure_x87_compare_tests.rs"]
mod compare_tests;
