use super::*;
use crate::static_program::procedure::*;

type Inputs = (u32, u32, u32, u32, u32, u32, u32, u32, u32);
type Outputs = (u32, u32, u32, u32, u32, u32, u32, u32, u32, u64);

struct ProcedureRunner {
    store: Store<()>,
    instance: wasmi::Instance,
}
impl ProcedureRunner {
    fn new(compiled: &CompiledProcedureProgram) -> Self {
        let engine = Engine::default();
        let module = Module::new(&engine, compiled.wasm()).unwrap();
        assert_eq!(module.imports().count(), 0);
        assert_eq!(module.exports().count(), compiled.procedure_ids().len());
        let mut store = Store::new(&engine, ());
        let linker = Linker::new(&engine);
        let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
        Self { store, instance }
    }
    fn function(
        &self,
        compiled: &CompiledProcedureProgram,
        id: ProcedureId,
    ) -> TypedFunc<Inputs, Outputs> {
        self.instance
            .get_typed_func(&self.store, compiled.export_name(id).unwrap())
            .unwrap()
    }
    fn call(
        &mut self,
        function: &TypedFunc<Inputs, Outputs>,
        gpr: [u32; 8],
        flags: u32,
    ) -> ([u32; 8], u32, u64) {
        let (a, c, d, b, sp, bp, si, di, flags, work) = function
            .call(
                &mut self.store,
                (
                    gpr[0], gpr[1], gpr[2], gpr[3], gpr[4], gpr[5], gpr[6], gpr[7], flags,
                ),
            )
            .unwrap();
        ([a, c, d, b, sp, bp, si, di], flags, work)
    }
}

fn function(id: u32, operations: Vec<ProcedureOperation>) -> ProcedureFunction {
    ProcedureFunction {
        id: ProcedureId(id),
        operations,
    }
}
fn execute(op: Operation) -> ProcedureOperation {
    ProcedureOperation::Execute(op)
}
fn call(id: u32) -> ProcedureOperation {
    ProcedureOperation::Call(ProcedureId(id))
}
fn alu(kind: Alu, target: Register, source: Register) -> ProcedureOperation {
    execute(Operation::Alu {
        kind,
        target,
        source: Source::Register(source),
    })
}
fn compile(program: &ProcedureProgram) -> CompiledProcedureProgram {
    program
        .compile(&ProcedureCompileOptions::default())
        .unwrap()
}
fn random(seed: &mut u32) -> u32 {
    *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    *seed
}

#[test]
fn procedure_executes_original_asm_three_level_calls_and_repeated_callee() {
    // A: ADD EAX,EDX; CALL B; XCHG AH,BL; CALL B; ADC AX,BX; RET.
    // B: INC AX; CALL C; SBB BH,DH; RET.
    // C: RCL AH,1; XOR ESI,EDI; NEG DL; RET.
    // Original test code: no external assembler or proprietary guest bytes.
    let program = ProcedureProgram {
        functions: vec![
            function(
                10,
                vec![
                    alu(Alu::Add, Register::Eax, Register::Edx),
                    call(20),
                    execute(Operation::Exchange {
                        left: Register::Ah,
                        right: Register::Bl,
                    }),
                    call(20),
                    alu(Alu::Adc, Register::Ax, Register::Bx),
                ],
            ),
            function(
                20,
                vec![
                    execute(Operation::Inc(Register::Ax)),
                    call(30),
                    alu(Alu::Sbb, Register::Bh, Register::Dh),
                ],
            ),
            function(
                30,
                vec![
                    execute(Operation::Shift {
                        kind: Shift::Rcl,
                        target: Register::Ah,
                        count: ShiftCount::One,
                    }),
                    alu(Alu::Xor, Register::Esi, Register::Edi),
                    execute(Operation::Neg(Register::Dl)),
                ],
            ),
        ],
    };
    let code = [
        0x01, 0xd0, 0xe8, 11, 0, 0, 0, 0x86, 0xdc, 0xe8, 4, 0, 0, 0, 0x66, 0x11, 0xd8, 0xc3, 0x66,
        0x40, 0xe8, 3, 0, 0, 0, 0x18, 0xf7, 0xc3, 0xd0, 0xd4, 0x31, 0xfe, 0xf6, 0xda, 0xc3,
    ];
    let compiled = compile(&program);
    assert_eq!(compiled.expanded_work(ProcedureId(10)), Some(22));
    assert_eq!(compiled.expanded_work(ProcedureId(20)), Some(8));
    assert_eq!(compiled.expanded_work(ProcedureId(30)), Some(4));
    assert_eq!(compiled.max_call_depth(ProcedureId(10)), Some(3));
    let mut runner = ProcedureRunner::new(&compiled);
    let entry = runner.function(&compiled, ProcedureId(10));
    let (mut cpu, mut bus) = reference(&code);
    let mut seed = 0x71ca_1103;
    for sample in 0..512 {
        let mut initial = core::array::from_fn(|_| random(&mut seed));
        initial[4] = 0x8000;
        let flags = 0x402 | (random(&mut seed) & 0xad5);
        cpu.registers.ip = 0x1000;
        cpu.flags = flags;
        for (reg, value) in REGS.into_iter().zip(initial) {
            cpu.registers.set32(reg, value);
        }
        bus.0[0x8000..0x8004].copy_from_slice(&0x1800_u32.to_le_bytes());
        for _ in 0..22 {
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(cpu.registers.ip, 0x1800);
        assert_eq!(cpu.registers.get32(Gpr32::Esp), 0x8004);
        let (gpr, actual_flags, work) = runner.call(&entry, initial, flags);
        let mut expected = core::array::from_fn(|i| cpu.registers.get32(REGS[i]));
        // The CPU-only IR deliberately has no real guest CALL/RET stack
        // effects. Normalize the outer RET's ESP increment explicitly; this
        // comparison does not establish equivalence of guest stack memory.
        expected[4] = initial[4];
        assert_eq!(gpr, expected, "sample={sample}");
        assert_eq!(actual_flags, cpu.flags, "sample={sample}");
        assert_eq!(work, 22);
    }
}

fn scalar_encoding(op: Operation) -> Vec<u8> {
    let mut bytes = Vec::new();
    match op {
        Operation::Extend {
            target,
            source,
            signed,
        } => {
            if target.width() == Width::Word {
                bytes.push(0x66);
            }
            bytes.extend_from_slice(&[
                0x0f,
                0xb6 | u8::from(source.width() == Width::Word) | (u8::from(signed) << 3),
                0xc0 | (register_code(target) << 3) | register_code(source),
            ]);
        }
        Operation::SignedMultiply {
            target,
            left,
            right: Source::Immediate(value),
        } => {
            if target.width() == Width::Word {
                bytes.push(0x66);
            }
            let Source::Register(left) = left else {
                unreachable!()
            };
            bytes.extend_from_slice(&[
                0x69,
                0xc0 | (register_code(target) << 3) | register_code(left),
            ]);
            bytes.extend_from_slice(&value.to_le_bytes()[..target.width().bits() as usize / 8]);
        }
        Operation::SignedMultiply {
            target,
            left,
            right: Source::Register(right),
        } => {
            assert_eq!(left, Source::Register(target));
            if target.width() == Width::Word {
                bytes.push(0x66);
            }
            bytes.extend_from_slice(&[
                0x0f,
                0xaf,
                0xc0 | (register_code(target) << 3) | register_code(right),
            ]);
        }
        Operation::WideMultiply {
            signed,
            width,
            source: Source::Register(source),
        } => {
            if width == Width::Word {
                bytes.push(0x66);
            }
            bytes.extend_from_slice(&[
                0xf6 | u8::from(width != Width::Byte),
                0xc0 | ((4 | u8::from(signed)) << 3) | register_code(source),
            ]);
        }
        _ => return encoding(op),
    }
    bytes
}

#[test]
fn procedure_scalar_subset_matches_interpreter_aliases_flags_and_boundaries() {
    let registers = [
        Register::Eax,
        Register::Ecx,
        Register::Edx,
        Register::Ebx,
        Register::Ebp,
        Register::Esi,
        Register::Edi,
        Register::Ax,
        Register::Cx,
        Register::Dx,
        Register::Bx,
        Register::Bp,
        Register::Si,
        Register::Di,
        Register::Al,
        Register::Cl,
        Register::Dl,
        Register::Bl,
        Register::Ah,
        Register::Ch,
        Register::Dh,
        Register::Bh,
    ];
    let mut operations = vec![Operation::Nop];
    for r in registers {
        let other = match r.width() {
            Width::Byte => Register::Ah,
            Width::Word => Register::Ax,
            Width::Dword => Register::Eax,
        };
        operations.extend([
            Operation::Move {
                target: r,
                source: Source::Register(other),
            },
            Operation::Move {
                target: r,
                source: Source::Immediate(r.width().mask()),
            },
            Operation::Inc(r),
            Operation::Dec(r),
            Operation::Neg(r),
            Operation::Not(r),
            Operation::Exchange {
                left: r,
                right: other,
            },
        ]);
        for kind in [
            Alu::Add,
            Alu::Adc,
            Alu::Sub,
            Alu::Sbb,
            Alu::And,
            Alu::Or,
            Alu::Xor,
            Alu::Cmp,
            Alu::Test,
        ] {
            operations.push(Operation::Alu {
                kind,
                target: r,
                source: Source::Register(other),
            });
            operations.push(Operation::Alu {
                kind,
                target: r,
                source: Source::Immediate(r.width().mask() >> 1),
            });
        }
        for kind in [
            Shift::Shl,
            Shift::Shr,
            Shift::Sar,
            Shift::Rol,
            Shift::Ror,
            Shift::Rcl,
            Shift::Rcr,
        ] {
            for count in [
                ShiftCount::One,
                ShiftCount::Immediate(0),
                ShiftCount::Immediate(16),
                ShiftCount::Immediate(31),
                ShiftCount::Cl,
            ] {
                operations.push(Operation::Shift {
                    kind,
                    target: r,
                    count,
                });
            }
        }
        if r.width() != Width::Dword {
            for signed in [false, true] {
                operations.push(Operation::Extend {
                    target: Register::Eax,
                    source: r,
                    signed,
                });
            }
        }
        if r.width() != Width::Byte {
            operations.extend([
                Operation::SignedMultiply {
                    target: r,
                    left: Source::Register(r),
                    right: Source::Register(other),
                },
                Operation::SignedMultiply {
                    target: r,
                    left: Source::Register(other),
                    right: Source::Immediate(r.width().mask() >> 1),
                },
            ]);
            for kind in [DoubleShift::Left, DoubleShift::Right] {
                for count in [
                    ShiftCount::One,
                    ShiftCount::Immediate(0),
                    ShiftCount::Immediate(16),
                ] {
                    operations.push(Operation::DoubleShift {
                        kind,
                        target: r,
                        source: other,
                        count,
                    });
                }
                if r.width() == Width::Dword {
                    operations.push(Operation::DoubleShift {
                        kind,
                        target: r,
                        source: other,
                        count: ShiftCount::Cl,
                    });
                }
            }
        }
        for signed in [false, true] {
            operations.push(Operation::WideMultiply {
                signed,
                width: r.width(),
                source: Source::Register(r),
            });
        }
    }
    let program = ProcedureProgram {
        functions: operations
            .iter()
            .enumerate()
            .map(|(i, &op)| function(i as u32, vec![execute(op)]))
            .collect(),
    };
    let compiled = compile(&program);
    let mut runner = ProcedureRunner::new(&compiled);
    let mut seed = 0xfac1_7103;
    let boundaries = [
        0,
        1,
        0xf,
        0x10,
        0x7f,
        0x80,
        0xff,
        0x7fff,
        0x8000,
        0xffff,
        0x7fff_ffff,
        0x8000_0000,
        u32::MAX,
    ];
    for (index, &op) in operations.iter().enumerate() {
        let entry = runner.function(&compiled, ProcedureId(index as u32));
        let (mut cpu, mut bus) = reference(&scalar_encoding(op));
        for sample in 0..17 {
            let mut gpr = core::array::from_fn(|_| random(&mut seed));
            // Broadcast boundary values to all aliases; remaining cases are
            // independent random registers, including CL counts and high bytes.
            if let Some(&value) = boundaries.get(sample) {
                gpr.fill(value);
            }
            for flags in [0xa550_0400, 0xa550_0cd7] {
                cpu.registers.ip = 0x1000;
                cpu.flags = flags;
                for (reg, value) in REGS.into_iter().zip(gpr) {
                    cpu.registers.set32(reg, value);
                }
                cpu.step(&mut bus).unwrap();
                let (result, actual_flags, work) = runner.call(&entry, gpr, flags);
                assert_eq!(
                    result,
                    core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
                    "op={op:?} sample={sample}"
                );
                assert_eq!(actual_flags, cpu.flags, "op={op:?} sample={sample}");
                assert_eq!(result[4], gpr[4]);
                assert_eq!(work, 2);
            }
        }
    }
}

fn uleb(bytes: &mut &[u8]) -> usize {
    let mut result = 0;
    let mut shift = 0;
    loop {
        let (&byte, rest) = bytes.split_first().unwrap();
        *bytes = rest;
        result |= usize::from(byte & 0x7f) << shift;
        if byte < 128 {
            return result;
        }
        shift += 7;
        assert!(shift < 64);
    }
}

#[test]
fn procedure_module_has_only_direct_calls_and_no_memory_table_import_or_guest_dispatch() {
    let compiled = compile(&ProcedureProgram {
        functions: vec![
            function(9, vec![call(20), call(20)]),
            function(20, vec![call(30)]),
            function(30, vec![execute(Operation::Inc(Register::Ah))]),
        ],
    });
    // Engine validation plus absence of memory/table declarations and imports
    // excludes valid load/store, indirect-call and callback-frame instructions.
    let _runner = ProcedureRunner::new(&compiled);
    let mut bytes = &compiled.wasm()[8..];
    let mut sections = Vec::new();
    let mut calls = Vec::new();
    let mut work_constants = 0;
    while !bytes.is_empty() {
        let id = bytes[0];
        bytes = &bytes[1..];
        let length = uleb(&mut bytes);
        let (mut payload, rest) = bytes.split_at(length);
        bytes = rest;
        sections.push(id);
        if id != 10 {
            continue;
        }
        let count = uleb(&mut payload);
        assert_eq!(count, 6); // Three internal functions and three exports.
        for function_index in 0..count {
            let length = uleb(&mut payload);
            let (mut code, rest) = payload.split_at(length);
            payload = rest;
            let locals = uleb(&mut code);
            for _ in 0..locals {
                uleb(&mut code);
                code = &code[1..];
            }
            while !code.is_empty() {
                let opcode = code[0];
                code = &code[1..];
                match opcode {
                    0x10 => {
                        calls.push(uleb(&mut code));
                    }
                    0x20..=0x22 | 0x41 => {
                        uleb(&mut code);
                    }
                    0x42 => {
                        uleb(&mut code);
                        // Internal arithmetic uses i64 constants for flags;
                        // only exported wrappers carry a work result.
                        if function_index >= count / 2 {
                            work_constants += 1;
                        }
                    }
                    0x04 => {
                        assert_eq!(code[0], 0x40);
                        code = &code[1..];
                    }
                    0x0b | 0x45..=0xc4 => {}
                    _ => panic!("unexpected generated opcode {opcode:x}"),
                }
            }
        }
        assert!(payload.is_empty());
    }
    assert_eq!(sections, [1, 3, 7, 10]);
    assert_eq!(calls, [1, 1, 2, 0, 1, 2]);
    assert_eq!(work_constants, 3); // One per exported boundary, none per call.
}

#[test]
fn procedure_empty_leaf_passthrough_and_compilation_order_are_deterministic() {
    let mut program = ProcedureProgram {
        functions: vec![
            function(u32::MAX, vec![]),
            function(7, vec![call(u32::MAX)]),
        ],
    };
    let compiled = compile(&program);
    assert_eq!(
        compiled.procedure_ids().collect::<Vec<_>>(),
        [ProcedureId(7), ProcedureId(u32::MAX)]
    );
    assert_eq!(
        compiled.export_name(ProcedureId(u32::MAX)),
        Some("procedure_4294967295")
    );
    assert_eq!(compiled.expanded_work(ProcedureId(7)), Some(3));
    assert_eq!(compiled.expanded_work(ProcedureId(u32::MAX)), Some(1));
    assert_eq!(compiled.expanded_work(ProcedureId(42)), None);
    assert_eq!(compiled.export_name(ProcedureId(42)), None);
    assert_eq!(compiled.max_call_depth(ProcedureId(42)), None);
    program.functions.reverse();
    assert_eq!(compiled.wasm(), compile(&program).wasm());
    let mut runner = ProcedureRunner::new(&compiled);
    for id in compiled.procedure_ids() {
        let entry = runner.function(&compiled, id);
        let initial = [
            0,
            1,
            0x1234_5678,
            u32::MAX,
            0x8000_ffff,
            99,
            0x5555_aaaa,
            0xaaaa_5555,
        ];
        assert_eq!(
            runner.call(&entry, initial, 0xa55a_c583),
            (initial, 0xa55a_c583, compiled.expanded_work(id).unwrap())
        );
    }
    let empty = compile(&ProcedureProgram::default());
    assert!(empty.procedure_ids().next().is_none());
    ProcedureRunner::new(&empty);
}

#[test]
fn procedure_rejects_cycles_unknown_calls_duplicate_ids_and_all_limits() {
    let options = ProcedureCompileOptions::default();
    let error = |functions| {
        ProcedureProgram { functions }
            .compile(&options)
            .unwrap_err()
    };
    assert_eq!(
        error(vec![function(7, vec![call(7)])]),
        ProcedureCompileError::Cycle {
            from: ProcedureId(7),
            target: ProcedureId(7)
        }
    );
    assert_eq!(
        error(vec![function(7, vec![call(8)]), function(8, vec![call(7)])]),
        ProcedureCompileError::Cycle {
            from: ProcedureId(8),
            target: ProcedureId(7)
        }
    );
    assert_eq!(
        error(vec![function(7, vec![call(9)])]),
        ProcedureCompileError::UnknownCallee {
            from: ProcedureId(7),
            target: ProcedureId(9)
        }
    );
    assert_eq!(
        error(vec![function(7, vec![]), function(7, vec![])]),
        ProcedureCompileError::DuplicateId(ProcedureId(7))
    );
    let chain = |count| ProcedureProgram {
        functions: (0..count)
            .map(|id| {
                function(
                    id,
                    if id + 1 < count {
                        vec![call(id + 1)]
                    } else {
                        vec![]
                    },
                )
            })
            .collect(),
    };
    assert_eq!(
        compile(&chain(MAX_CALL_DEPTH as u32)).max_call_depth(ProcedureId(0)),
        Some(MAX_CALL_DEPTH)
    );
    assert!(matches!(
        chain(MAX_CALL_DEPTH as u32 + 1).compile(&options),
        Err(ProcedureCompileError::CallDepthExceeded(_))
    ));
    // The callee's depth was memoized under an earlier sorted root. It must
    // still count in a later caller's complete depth.
    let memoized = ProcedureProgram {
        functions: vec![
            function(1, vec![call(2)]),
            function(2, vec![call(3)]),
            function(3, vec![]),
            function(4, vec![call(1)]),
        ],
    };
    assert_eq!(
        memoized
            .compile(&ProcedureCompileOptions {
                max_call_depth: 3,
                ..options
            })
            .unwrap_err(),
        ProcedureCompileError::CallDepthExceeded(ProcedureId(4))
    );
    let exponential = ProcedureProgram {
        functions: (0..32)
            .map(|id| {
                function(
                    id,
                    if id == 31 {
                        vec![]
                    } else {
                        vec![call(id + 1), call(id + 1)]
                    },
                )
            })
            .collect(),
    };
    assert!(matches!(
        exponential.compile(&options),
        Err(ProcedureCompileError::ExpandedWorkExceeded(_))
    ));
    let program = ProcedureProgram {
        functions: vec![
            function(0, vec![call(1), call(1)]),
            function(1, vec![execute(Operation::Nop)]),
        ],
    };
    assert_eq!(
        program
            .compile(&ProcedureCompileOptions {
                max_expanded_work: 7,
                ..options
            })
            .unwrap()
            .expanded_work(ProcedureId(0)),
        Some(7)
    );
    assert_eq!(
        program
            .compile(&ProcedureCompileOptions {
                max_expanded_work: 6,
                ..options
            })
            .unwrap_err(),
        ProcedureCompileError::ExpandedWorkExceeded(ProcedureId(0))
    );
    for (limited, expected) in [
        (
            ProcedureCompileOptions {
                max_procedures: 1,
                ..options
            },
            ProcedureCompileError::TooManyProcedures,
        ),
        (
            ProcedureCompileOptions {
                max_procedure_operations: 1,
                ..options
            },
            ProcedureCompileError::ProcedureTooLong(ProcedureId(0)),
        ),
        (
            ProcedureCompileOptions {
                max_total_operations: 2,
                ..options
            },
            ProcedureCompileError::TooManyOperations,
        ),
        (
            ProcedureCompileOptions {
                max_output_bytes: 0,
                ..options
            },
            ProcedureCompileError::OutputTooLarge,
        ),
    ] {
        assert_eq!(program.compile(&limited).unwrap_err(), expected);
    }
    let exact = compile(&program).wasm().len();
    assert!(
        program
            .compile(&ProcedureCompileOptions {
                max_output_bytes: exact,
                ..options
            })
            .is_ok()
    );
    assert_eq!(
        program
            .compile(&ProcedureCompileOptions {
                max_output_bytes: exact - 1,
                ..options
            })
            .unwrap_err(),
        ProcedureCompileError::OutputTooLarge
    );
    let invalid = [
        ProcedureCompileOptions {
            max_procedures: MAX_PROCEDURES + 1,
            ..options
        },
        ProcedureCompileOptions {
            max_procedure_operations: MAX_PROCEDURE_OPERATIONS + 1,
            ..options
        },
        ProcedureCompileOptions {
            max_total_operations: MAX_TOTAL_OPERATIONS + 1,
            ..options
        },
        ProcedureCompileOptions {
            max_call_depth: MAX_CALL_DEPTH + 1,
            ..options
        },
        ProcedureCompileOptions {
            max_call_depth: 0,
            ..options
        },
        ProcedureCompileOptions {
            max_expanded_work: MAX_EXPANDED_WORK + 1,
            ..options
        },
        ProcedureCompileOptions {
            max_expanded_work: 0,
            ..options
        },
        ProcedureCompileOptions {
            max_output_bytes: crate::static_program::procedure::MAX_OUTPUT_BYTES + 1,
            ..options
        },
    ];
    for invalid in invalid {
        assert_eq!(
            program.compile(&invalid).unwrap_err(),
            ProcedureCompileError::InvalidOptions
        );
    }
}

fn invalid_operation(op: Operation, reason: ProcedureOperationError) {
    let program = ProcedureProgram {
        functions: vec![function(
            7,
            vec![execute(Operation::Inc(Register::Eax)), execute(op)],
        )],
    };
    assert_eq!(
        program
            .compile(&ProcedureCompileOptions::default())
            .unwrap_err(),
        ProcedureCompileError::InvalidOperation {
            procedure: ProcedureId(7),
            operation: 1,
            reason
        }
    );
}

#[test]
fn procedure_rejects_every_memory_stack_and_lea_form_before_execution() {
    let address = memory::MemoryAddress {
        base: Some(Register::Eax),
        index: None,
        scale: 1,
        displacement: 0,
        address16: false,
        segment: memory::Segment::Ds,
    };
    for op in [
        Operation::Lea {
            target: Register::Eax,
            address,
        },
        Operation::LoadMemory {
            target: Register::Eax,
            address,
        },
        Operation::ExtendMemory {
            target: Register::Eax,
            address,
            width: Width::Byte,
            signed: false,
        },
        Operation::SignedMultiplyMemory {
            target: Register::Eax,
            source: Source::Register(Register::Eax),
            address,
        },
        Operation::WideMultiplyMemory {
            signed: true,
            width: Width::Dword,
            address,
        },
        Operation::StoreMemory {
            address,
            width: Width::Dword,
            source: Source::Register(Register::Eax),
        },
        Operation::Push {
            width: Width::Dword,
            source: Source::Register(Register::Eax),
        },
        Operation::Pop {
            target: Register::Eax,
        },
        Operation::AluMemory {
            kind: Alu::Add,
            target: Register::Eax,
            address,
        },
        Operation::CompareMemory {
            test: false,
            address,
            width: Width::Dword,
            source: Source::Register(Register::Eax),
        },
        Operation::AluMemoryStore {
            kind: Alu::Add,
            address,
            width: Width::Dword,
            source: Source::Register(Register::Eax),
        },
        Operation::UnaryMemory {
            kind: MemoryUnary::Inc,
            address,
            width: Width::Dword,
        },
    ] {
        invalid_operation(op, ProcedureOperationError::Unsupported);
    }
}

#[test]
fn procedure_rejects_explicit_sp_esp_reads_writes_and_invalid_scalar_operands() {
    for stack in [Register::Sp, Register::Esp] {
        let ordinary = if stack == Register::Sp {
            Register::Ax
        } else {
            Register::Eax
        };
        for op in [
            Operation::Move {
                target: stack,
                source: Source::Register(ordinary),
            },
            Operation::Move {
                target: ordinary,
                source: Source::Register(stack),
            },
            Operation::Alu {
                kind: Alu::Cmp,
                target: stack,
                source: Source::Register(ordinary),
            },
            Operation::Extend {
                target: Register::Eax,
                source: stack,
                signed: false,
            },
            Operation::Extend {
                target: stack,
                source: Register::Al,
                signed: false,
            },
            Operation::SignedMultiply {
                target: stack,
                left: Source::Register(ordinary),
                right: Source::Register(ordinary),
            },
            Operation::SignedMultiply {
                target: ordinary,
                left: Source::Register(stack),
                right: Source::Register(ordinary),
            },
            Operation::SignedMultiply {
                target: ordinary,
                left: Source::Register(ordinary),
                right: Source::Register(stack),
            },
            Operation::WideMultiply {
                signed: false,
                width: stack.width(),
                source: Source::Register(stack),
            },
            Operation::Shift {
                kind: Shift::Shl,
                target: stack,
                count: ShiftCount::One,
            },
            Operation::DoubleShift {
                kind: DoubleShift::Left,
                target: ordinary,
                source: stack,
                count: ShiftCount::One,
            },
            Operation::DoubleShift {
                kind: DoubleShift::Left,
                target: stack,
                source: ordinary,
                count: ShiftCount::One,
            },
            Operation::Inc(stack),
            Operation::Dec(stack),
            Operation::Neg(stack),
            Operation::Not(stack),
            Operation::Exchange {
                left: ordinary,
                right: stack,
            },
            Operation::Exchange {
                left: stack,
                right: ordinary,
            },
        ] {
            invalid_operation(op, ProcedureOperationError::StackRegister);
        }
    }
    for op in [
        Operation::Move {
            target: Register::Al,
            source: Source::Register(Register::Ax),
        },
        Operation::Alu {
            kind: Alu::Add,
            target: Register::Ax,
            source: Source::Register(Register::Eax),
        },
        Operation::Extend {
            target: Register::Al,
            source: Register::Ah,
            signed: false,
        },
        Operation::Extend {
            target: Register::Ax,
            source: Register::Eax,
            signed: true,
        },
        Operation::SignedMultiply {
            target: Register::Al,
            left: Source::Register(Register::Al),
            right: Source::Register(Register::Bl),
        },
        Operation::SignedMultiply {
            target: Register::Ax,
            left: Source::Register(Register::Eax),
            right: Source::Register(Register::Bx),
        },
        Operation::WideMultiply {
            signed: true,
            width: Width::Byte,
            source: Source::Register(Register::Ax),
        },
        Operation::DoubleShift {
            kind: DoubleShift::Left,
            target: Register::Al,
            source: Register::Bl,
            count: ShiftCount::One,
        },
        Operation::DoubleShift {
            kind: DoubleShift::Right,
            target: Register::Ax,
            source: Register::Ebx,
            count: ShiftCount::One,
        },
        Operation::Exchange {
            left: Register::Ax,
            right: Register::Eax,
        },
    ] {
        invalid_operation(op, ProcedureOperationError::WidthMismatch);
    }
    for op in [
        Operation::Move {
            target: Register::Al,
            source: Source::Immediate(256),
        },
        Operation::Alu {
            kind: Alu::Add,
            target: Register::Ax,
            source: Source::Immediate(65536),
        },
        Operation::SignedMultiply {
            target: Register::Ax,
            left: Source::Immediate(65536),
            right: Source::Register(Register::Bx),
        },
        Operation::WideMultiply {
            signed: true,
            width: Width::Byte,
            source: Source::Immediate(256),
        },
    ] {
        invalid_operation(op, ProcedureOperationError::ImmediateOutOfRange);
    }
    for count in [
        ShiftCount::Cl,
        ShiftCount::Immediate(17),
        ShiftCount::Immediate(31),
    ] {
        invalid_operation(
            Operation::DoubleShift {
                kind: DoubleShift::Left,
                target: Register::Ax,
                source: Register::Bx,
                count,
            },
            ProcedureOperationError::InvalidShiftCount,
        );
    }
}
