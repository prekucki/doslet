use super::*;

fn function(id: u32, kind: ProcedureTerminatorKind) -> ProcedureGraphFunction {
    let entry = Address {
        space: 0,
        offset: 0x100,
    };
    ProcedureGraphFunction {
        id: ProcedureId(id),
        entry,
        blocks: alloc::vec![ProcedureBlock {
            entry,
            instructions: Vec::new(),
            terminator: ProcedureTerminator {
                address: entry,
                kind
            },
        }],
    }
}

#[test]
fn preparation_colors_sorted_declared_boundaries_without_emission() {
    let program = ProcedureGraphProgram {
        functions: alloc::vec![
            function(7, ProcedureTerminatorKind::Exit),
            function(
                2,
                ProcedureTerminatorKind::Return {
                    width: Width::Word,
                    pop_bytes: 4
                }
            ),
        ],
    };
    // Preparation accepts a zero emission cap: output allocation is deferred.
    let options = ProcedureCompileOptions {
        max_output_bytes: 0,
        ..Default::default()
    };
    let prepared = program
        .prepare(&options, &ProcedureAnalysisOptions::default())
        .unwrap();
    assert_eq!(prepared.colors()[0].id, ProcedureId(2));
    let color = prepared.color(ProcedureId(2)).unwrap();
    assert_eq!(color.strategy, ProcedureExecutionStrategy::Acyclic);
    assert_eq!(color.maximum_work, 1);
    assert_eq!(color.returns[0].pop_bytes, 4);
    assert!(color.x87.is_none());
    assert_eq!(
        prepared
            .color(ProcedureId(7))
            .unwrap()
            .interpreter_exits
            .len(),
        1
    );
    assert!(prepared.color(ProcedureId(99)).is_none());
    assert!(prepared.compile().is_err());
}

#[test]
fn prepared_emission_matches_existing_compile_entrypoint() {
    let program = ProcedureGraphProgram {
        functions: alloc::vec![function(
            1,
            ProcedureTerminatorKind::Return {
                width: Width::Word,
                pop_bytes: 0
            }
        ),],
    };
    let options = ProcedureCompileOptions::default();
    let prepared = program
        .prepare(&options, &ProcedureAnalysisOptions::default())
        .unwrap();
    assert_eq!(
        prepared.compile().unwrap().wasm(),
        program.compile(&options).unwrap().wasm()
    );
}

#[test]
fn preparation_rejects_invalid_policy_and_duplicate_ids() {
    let options = ProcedureCompileOptions::default();
    let mut program = ProcedureGraphProgram {
        functions: alloc::vec![function(1, ProcedureTerminatorKind::Exit)],
    };
    assert_eq!(
        program
            .prepare(
                &options,
                &ProcedureAnalysisOptions {
                    ordinary_work_limit: Some(0),
                    ..Default::default()
                }
            )
            .unwrap_err(),
        ProcedureCompileError::InvalidOptions
    );
    program.functions.push(program.functions[0].clone());
    assert_eq!(
        program
            .prepare(&options, &ProcedureAnalysisOptions::default())
            .unwrap_err(),
        ProcedureCompileError::DuplicateId(ProcedureId(1))
    );
    program.functions[1].id = ProcedureId(2);
    program.functions[1].blocks.clear();
    assert_eq!(
        program
            .prepare(&options, &ProcedureAnalysisOptions::default())
            .unwrap_err(),
        ProcedureCompileError::ProcedureHasNoBlocks(ProcedureId(2))
    );
}

#[test]
fn preparation_distinguishes_bounded_and_proven_finite_countdown() {
    let entry = Address {
        space: 0,
        offset: 0x100,
    };
    let exit = Address {
        space: 0,
        offset: 0x110,
    };
    let mut program = ProcedureGraphProgram {
        functions: alloc::vec![ProcedureGraphFunction {
            id: ProcedureId(3),
            entry,
            blocks: alloc::vec![
                ProcedureBlock {
                    entry,
                    instructions: alloc::vec![ProcedureInstruction {
                        address: entry,
                        operation: Operation::Dec(crate::static_program::Register::Cl),
                    }],
                    terminator: ProcedureTerminator {
                        address: Address {
                            offset: 0x102,
                            ..entry
                        },
                        kind: ProcedureTerminatorKind::Branch {
                            condition: Condition::NotEqual,
                            taken: entry,
                            not_taken: exit
                        },
                    }
                },
                ProcedureBlock {
                    entry: exit,
                    instructions: Vec::new(),
                    terminator: ProcedureTerminator {
                        address: exit,
                        kind: ProcedureTerminatorKind::Exit,
                    }
                },
            ],
        }],
    };
    let options = ProcedureCompileOptions::default();
    let mut analysis = ProcedureAnalysisOptions {
        ordinary_work_limit: Some(32),
        ..Default::default()
    };
    let bounded = program.prepare(&options, &analysis).unwrap();
    assert_eq!(
        bounded.colors()[0].strategy,
        ProcedureExecutionStrategy::BoundedLoop
    );
    assert_eq!(bounded.colors()[0].maximum_work, 32);
    analysis.computation_kernels = true;
    let finite = program.prepare(&options, &analysis).unwrap();
    assert_eq!(
        finite.colors()[0].strategy,
        ProcedureExecutionStrategy::Finite
    );
    assert!(finite.colors()[0].maximum_work >= 512);
    // The same unknown dword counter needs a derived entry trip guard.
    drop(finite);
    program.functions[0].blocks[0].instructions[0].operation =
        Operation::Dec(crate::static_program::Register::Ecx);
    let guarded = program.prepare(&options, &analysis).unwrap();
    assert_eq!(
        guarded.colors()[0].strategy,
        ProcedureExecutionStrategy::GuardedFinite
    );
    assert!(guarded.colors()[0].maximum_work > 32);
}

use crate::static_program as rle_api;
mod rle_fixture {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/fixtures/rle_procedure.rs"
    ));
}

#[test]
fn preparation_recognizes_rle_before_wasm_emission() {
    let fixture = rle_fixture::rle_fixture(ProcedureId(19), 7, 0x8000);
    assert!(!fixture.code.is_empty());
    let program = ProcedureGraphProgram {
        functions: alloc::vec![fixture.function],
    };
    let options = ProcedureCompileOptions {
        max_output_bytes: 0,
        ..Default::default()
    };
    let prepared = program
        .prepare(
            &options,
            &ProcedureAnalysisOptions {
                ordinary_work_limit: Some(32),
                copy_work_limit: Some(32),
                computation_kernels: true,
                pc24_locals: false,
            },
        )
        .unwrap();
    assert_eq!(
        prepared.colors()[0].strategy,
        ProcedureExecutionStrategy::Rle
    );
    assert_eq!(prepared.colors()[0].maximum_work, u64::from(u32::MAX));
}

#[test]
fn prepared_pc24_color_reports_balanced_independent_stack_and_compiles() {
    use crate::static_program::{
        X87Instruction, X87Operation,
        memory::{MemoryAddress, Segment},
    };
    let entry = Address {
        space: 7,
        offset: 0x8000,
    };
    let memory = |displacement| MemoryAddress {
        base: None,
        index: None,
        scale: 1,
        displacement,
        address16: false,
        segment: Segment::Ds,
    };
    let program = ProcedureGraphProgram {
        functions: alloc::vec![ProcedureGraphFunction {
            id: ProcedureId(24),
            entry,
            blocks: alloc::vec![ProcedureBlock {
                entry,
                instructions: alloc::vec![
                    ProcedureInstruction {
                        address: entry,
                        operation: Operation::X87(X87Instruction {
                            // D9 /0: FLD m32real.
                            opcode: 0x105,
                            operation: X87Operation::LoadReal32 {
                                address: memory(0x1100)
                            },
                        })
                    },
                    ProcedureInstruction {
                        address: Address {
                            offset: 0x8006,
                            ..entry
                        },
                        operation: Operation::X87(X87Instruction {
                            // DB /3: FISTP m32int.
                            opcode: 0x31d,
                            operation: X87Operation::StoreInteger32 {
                                address: memory(0x1104),
                                pop: true
                            },
                        })
                    },
                ],
                terminator: ProcedureTerminator {
                    address: Address {
                        offset: 0x800c,
                        ..entry
                    },
                    kind: ProcedureTerminatorKind::Exit,
                },
            }],
        }],
    };
    let prepared = program
        .prepare(
            &ProcedureCompileOptions::default(),
            &ProcedureAnalysisOptions {
                pc24_locals: true,
                ..Default::default()
            },
        )
        .unwrap();
    let effects = prepared
        .color(ProcedureId(24))
        .unwrap()
        .x87
        .as_ref()
        .unwrap();
    assert_eq!(effects.class, X87EffectClass::IndependentBalanced);
    assert_eq!(effects.stack_min, Some(0));
    assert_eq!(effects.stack_max, Some(1));
    assert!(!effects.initial_values_required);
    assert_eq!(effects.exit_balances.len(), 1);
    assert_eq!(effects.exit_balances[0].delta, Some(0));
    assert!(effects.declared_exits_balanced);
    assert!(!effects.all_boundaries_balanced);
    assert!(!prepared.compile().unwrap().wasm().is_empty());
}
