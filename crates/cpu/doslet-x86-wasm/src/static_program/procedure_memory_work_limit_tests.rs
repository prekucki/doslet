use super::*;
use crate::static_program as rle_api;

#[allow(
    dead_code,
    reason = "shared fixture also carries original x86 bytes for execution tests"
)]
mod rle_fixture {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/fixtures/rle_procedure.rs"
    ));
}

/// Minimal complete copy idiom, with no optional padding. This has the same
/// 15 canonical guest sites as the padded copy acceptance fixture.
fn semantic_copy() -> ProcedureGraphProgram {
    let mem = |base, displacement| address(Some(base), displacement, Segment::Ds);
    let add = |target, value| Operation::Alu {
        kind: Alu::Add,
        target,
        source: Source::Immediate(value),
    };
    graph(vec![
        block(
            0,
            vec![
                instruction(
                    0,
                    Operation::Move {
                        target: Register::Ebx,
                        source: Source::Register(Register::Ecx),
                    },
                ),
                instruction(
                    1,
                    Operation::Shift {
                        target: Register::Ebx,
                        kind: crate::static_program::Shift::Shr,
                        count: crate::static_program::ShiftCount::One,
                    },
                ),
            ],
            2,
            ProcedureTerminatorKind::Branch {
                condition: Condition::Equal,
                taken: at(40),
                not_taken: at(3),
            },
        ),
        block(
            3,
            vec![
                instruction(
                    3,
                    Operation::LoadMemory {
                        target: Register::Ax,
                        address: mem(Register::Esi, 0),
                    },
                ),
                instruction(
                    4,
                    Operation::StoreMemory {
                        width: Width::Byte,
                        source: Source::Register(Register::Al),
                        address: mem(Register::Edi, 0),
                    },
                ),
                instruction(5, add(Register::Esi, 2)),
                instruction(6, add(Register::Edi, 192)),
                instruction(
                    7,
                    Operation::StoreMemory {
                        width: Width::Byte,
                        source: Source::Register(Register::Ah),
                        address: mem(Register::Edi, (-96i32) as u32),
                    },
                ),
                instruction(8, Operation::Dec(Register::Ebx)),
            ],
            9,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(3),
                not_taken: at(10),
            },
        ),
        block(
            10,
            vec![instruction(
                10,
                Operation::Alu {
                    kind: Alu::And,
                    target: Register::Ecx,
                    source: Source::Immediate(1),
                },
            )],
            11,
            ProcedureTerminatorKind::Branch {
                condition: Condition::Equal,
                taken: at(42),
                not_taken: at(40),
            },
        ),
        block(
            40,
            vec![
                instruction(
                    40,
                    Operation::LoadMemory {
                        target: Register::Al,
                        address: mem(Register::Esi, 0),
                    },
                ),
                instruction(
                    41,
                    Operation::StoreMemory {
                        width: Width::Byte,
                        source: Source::Register(Register::Al),
                        address: mem(Register::Edi, 0),
                    },
                ),
            ],
            42,
            ProcedureTerminatorKind::Fallthrough(at(42)),
        ),
        block(42, vec![], 42, ret(Width::Dword, 0)),
    ])
}

fn finite_counter() -> ProcedureGraphProgram {
    graph(vec![
        block(
            0,
            vec![instruction(
                0,
                Operation::Move {
                    target: Register::Cx,
                    source: Source::Immediate(3),
                },
            )],
            1,
            ProcedureTerminatorKind::Fallthrough(at(1)),
        ),
        block(
            1,
            vec![
                instruction(1, Operation::Nop),
                instruction(2, Operation::Dec(Register::Cx)),
            ],
            3,
            ProcedureTerminatorKind::Branch {
                condition: Condition::NotEqual,
                taken: at(1),
                not_taken: at(4),
            },
        ),
        block(4, vec![], 4, ret(Width::Dword, 0)),
    ])
}

fn relocate(function: &mut ProcedureGraphFunction, id: u32, delta: u32) {
    let shift = |address: &mut Address| address.offset += delta;
    function.id = ProcedureId(id);
    shift(&mut function.entry);
    for block in &mut function.blocks {
        shift(&mut block.entry);
        for instruction in &mut block.instructions {
            shift(&mut instruction.address);
        }
        shift(&mut block.terminator.address);
        match &mut block.terminator.kind {
            ProcedureTerminatorKind::Fallthrough(target)
            | ProcedureTerminatorKind::Jump(target) => shift(target),
            ProcedureTerminatorKind::Branch {
                taken, not_taken, ..
            } => {
                shift(taken);
                shift(not_taken);
            }
            _ => {}
        }
    }
}

#[test]
fn distinct_work_limits_keep_ordinary_loop_reservation_and_execution_at_128() {
    let (program, code) = early_exit_loop(true, false);
    let options = ProcedureCompileOptions::default();
    let compiled = program
        .compile_with_computation_kernels_and_work_limits(&options, 128, 4096)
        .unwrap();
    assert_eq!(compiled.maximum_work(ProcedureId(3)), Some(128));
    assert_eq!(compiled.semantic_kernel(ProcedureId(3)), None);
    assert_eq!(compiled.finite_maximum_work(ProcedureId(3)), None);
    assert_eq!(
        compiled.wasm(),
        program
            .compile_with_computation_kernels(&options, 128)
            .unwrap()
            .wasm()
    );
    let mut runner = Runner::compiled(&compiled);
    let mut frame = initial();
    frame.remaining_budget = 4096;
    frame.gpr[7] = 0x1800;
    let mut descriptor = descriptor();
    descriptor.inline_grant = 4096;
    assert_eq!(
        compare_until(
            &mut runner,
            &code,
            frame,
            descriptor,
            ENTRY,
            SIDE_EXIT,
            Some(128),
            |runner| {
                let offset = RAM + (0x1000 - RAM_BASE) as usize;
                runner.bytes()[offset..offset + 4].copy_from_slice(&100u32.to_le_bytes());
            }
        ),
        128
    );
}

#[test]
fn distinct_work_limits_preserve_single_copy_bytes_and_large_copy_contract() {
    let program = semantic_copy();
    let options = ProcedureCompileOptions::default();
    let legacy = program
        .compile_with_computation_kernels(&options, 4096)
        .unwrap();
    let compiled = program
        .compile_with_computation_kernels_and_work_limits(&options, 128, 4096)
        .unwrap();
    let id = ProcedureId(3);
    let kernel = compiled
        .semantic_kernel(id)
        .expect("complete paired copy recognized");
    assert_eq!(kernel.max_work_per_call, 4096);
    assert_eq!(kernel, legacy.semantic_kernel(id).unwrap());
    assert_eq!(kernel.actual_work(100), Some(356));
    assert_eq!(compiled.maximum_work(id), Some(4096));
    assert_eq!(compiled.wasm(), legacy.wasm());
    assert_eq!(
        program
            .compile_with_computation_kernels(&options, 128)
            .unwrap()
            .semantic_kernel(id)
            .unwrap()
            .actual_work(100),
        None
    );
    Runner::compiled(&compiled); // Validate emitted module and export signature.
}

#[test]
fn distinct_work_limits_preserve_finite_and_rle_single_function_bytes() {
    let finite = finite_counter();
    let rle = ProcedureGraphProgram {
        functions: vec![
            rle_fixture::rle_fixture(ProcedureId(3), ENTRY.space, ENTRY.offset).function,
        ],
    };
    let options = ProcedureCompileOptions::default();
    for program in [finite, rle] {
        let legacy = program
            .compile_with_computation_kernels(&options, 4096)
            .unwrap();
        let compiled = program
            .compile_with_computation_kernels_and_work_limits(&options, 128, 4096)
            .unwrap();
        let id = ProcedureId(3);
        assert_eq!(compiled.maximum_work(id), legacy.maximum_work(id));
        assert_eq!(
            compiled.finite_maximum_work(id),
            legacy.finite_maximum_work(id)
        );
        assert_eq!(
            compiled.finite_loop_guards(id),
            legacy.finite_loop_guards(id)
        );
        assert_eq!(compiled.rle_kernel(id), legacy.rle_kernel(id));
        assert_eq!(compiled.wasm(), legacy.wasm());
    }
}

#[test]
fn distinct_work_limits_apply_per_function_in_a_mixed_module() {
    let mut ordinary = early_exit_loop(true, false).0.functions.remove(0);
    relocate(&mut ordinary, 3, 0);
    let mut copy = semantic_copy().functions.remove(0);
    relocate(&mut copy, 4, 0x200);
    let mut finite = finite_counter().functions.remove(0);
    relocate(&mut finite, 5, 0x400);
    let rle = rle_fixture::rle_fixture(ProcedureId(6), ENTRY.space, ENTRY.offset + 0x600).function;
    let program = ProcedureGraphProgram {
        functions: vec![rle, copy, ordinary, finite],
    };
    let options = ProcedureCompileOptions::default();
    let compiled = program
        .compile_with_computation_kernels_and_work_limits(&options, 128, 4096)
        .unwrap();
    assert_eq!(compiled.maximum_work(ProcedureId(3)), Some(128));
    assert_eq!(compiled.maximum_work(ProcedureId(4)), Some(4096));
    assert_eq!(
        compiled
            .semantic_kernel(ProcedureId(4))
            .unwrap()
            .max_work_per_call,
        4096
    );
    assert_eq!(compiled.finite_maximum_work(ProcedureId(5)), Some(11));
    assert!(compiled.rle_kernel(ProcedureId(6)).is_some());
    let legacy = program
        .compile_with_computation_kernels(&options, 4096)
        .unwrap();
    for id in [ProcedureId(4), ProcedureId(5), ProcedureId(6)] {
        assert_eq!(compiled.maximum_work(id), legacy.maximum_work(id));
        assert_eq!(compiled.semantic_kernel(id), legacy.semantic_kernel(id));
        assert_eq!(
            compiled.finite_maximum_work(id),
            legacy.finite_maximum_work(id)
        );
        assert_eq!(compiled.rle_kernel(id), legacy.rle_kernel(id));
    }
    for limit in [1, 128, 4096] {
        assert_eq!(
            program
                .compile_with_computation_kernels_and_work_limits(&options, limit, limit)
                .unwrap()
                .wasm(),
            program
                .compile_with_computation_kernels(&options, limit)
                .unwrap()
                .wasm()
        );
    }
}

#[test]
fn distinct_work_limits_validate_both_bounds_even_for_empty_programs() {
    let program = ProcedureGraphProgram::default();
    let options = ProcedureCompileOptions::default();
    for (ordinary, copy) in [
        (0, 4096),
        (128, 0),
        (MAX_WORK_PER_CALL + 1, 4096),
        (128, MAX_WORK_PER_CALL + 1),
    ] {
        assert!(matches!(
            program.compile_with_computation_kernels_and_work_limits(&options, ordinary, copy),
            Err(ProcedureCompileError::InvalidOptions)
        ));
    }
}
