use super::*;

const DWORDS: [Register; 8] = [
    Register::Eax,
    Register::Ecx,
    Register::Edx,
    Register::Ebx,
    Register::Esp,
    Register::Ebp,
    Register::Esi,
    Register::Edi,
];
const WORDS: [Register; 8] = [
    Register::Ax,
    Register::Cx,
    Register::Dx,
    Register::Bx,
    Register::Sp,
    Register::Bp,
    Register::Si,
    Register::Di,
];

fn encoding(
    target: Register,
    left: Register,
    immediate: Option<(u32, bool)>,
    code32: bool,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    if (target.width() == Width::Dword) != code32 {
        bytes.push(0x66);
    }
    if let Some((value, short)) = immediate {
        bytes.push(if short { 0x6b } else { 0x69 });
        bytes.push(0xc0 | register_code(target) << 3 | register_code(left));
        bytes.extend_from_slice(
            &value.to_le_bytes()[..if short {
                1
            } else {
                target.width().bits() as usize / 8
            }],
        );
    } else {
        bytes.extend_from_slice(&[
            0x0f,
            0xaf,
            0xc0 | register_code(target) << 3 | register_code(left),
        ]);
    }
    bytes
}

fn put(gpr: &mut [u32; 8], r: Register, value: u32) {
    let old = &mut gpr[r.index() as usize];
    *old = (*old & !r.width().mask()) | (value & r.width().mask());
}

fn set_cpu(cpu: &mut Cpu, frame: Frame) {
    cpu.registers.ip = 0x1000;
    cpu.flags = frame.eflags;
    for (r, value) in REGS.into_iter().zip(frame.gpr) {
        cpu.registers.set32(r, value);
    }
}

type MultiplyCase = (Register, Register, Option<(u32, bool)>);

fn differential(cases: &[MultiplyCase], samples: usize) {
    let program = Program {
        regions: cases
            .iter()
            .enumerate()
            .map(|(i, &(target, register, immediate))| {
                let id = i as u32 + 1;
                region(
                    id,
                    vec![Operation::SignedMultiply {
                        target,
                        left: Source::Register(if immediate.is_some() {
                            register
                        } else {
                            target
                        }),
                        right: immediate
                            .map_or(Source::Register(register), |(v, _)| Source::Immediate(v)),
                    }],
                    Terminator::Fallthrough(RegionId(id)),
                )
            })
            .collect(),
    };
    let mut runner = Runner::new(&program, CompileOptions::default());
    let mut seed = 0x8b27_9c13u32;
    for code32 in [false, true] {
        for (i, &(target, register, immediate)) in cases.iter().enumerate() {
            let (mut cpu, mut bus) = reference(&encoding(target, register, immediate, code32));
            bus.0[0x20e] = if code32 { 0xcf } else { 0x8f };
            cpu.load_segment(&mut bus, SegmentRegister::Cs, 8).unwrap();
            let sign = 1 << (target.width().bits() - 1);
            let boundaries = [
                0,
                1,
                2,
                0x7f,
                sign - 1,
                sign,
                target.width().mask(),
                target.width().mask() - 1,
            ];
            for sample in 0..samples {
                let mut random = || {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    seed
                };
                let mut gpr = core::array::from_fn(|_| random());
                let left = if sample < 64 {
                    boundaries[sample / 8]
                } else {
                    random()
                };
                let right = if sample < 64 {
                    boundaries[sample % 8]
                } else {
                    random()
                };
                put(&mut gpr, target, left);
                put(&mut gpr, register, right);
                for flags in [0, 1, 0x8d4, 0x8d7] {
                    let initial = Frame {
                        gpr,
                        eflags: 0xa550_0400 | flags,
                        address: program.regions[i].entry,
                        remaining_budget: 1,
                        charged_work: 39,
                        status: 0x8000_0000,
                    };
                    runner.set(initial);
                    set_cpu(&mut cpu, initial);
                    cpu.step(&mut bus).unwrap();
                    assert_eq!(runner.call(), abi::EXIT_BUDGET);
                    assert_eq!(
                        runner.frame(),
                        Frame {
                            gpr: core::array::from_fn(|j| cpu.registers.get32(REGS[j])),
                            eflags: cpu.flags,
                            remaining_budget: 0,
                            charged_work: 40,
                            ..initial
                        },
                        "target={target:?} register={register:?} immediate={immediate:?} code32={code32} sample={sample}"
                    );
                    assert_eq!(runner.frame().eflags & !0x801, initial.eflags & !0x801);
                    assert_eq!(runner.frame().eflags & 1, (runner.frame().eflags >> 11) & 1);
                }
            }
        }
    }
    assert!(runner.store.data().events.is_empty());
    assert!(runner.store.data().admissions.is_empty());
    for (i, &byte) in runner.memory.data(&runner.store).iter().enumerate() {
        if !(PTR..PTR + abi::FRAME_BYTES).contains(&i)
            || (PTR + 44..PTR + 48).contains(&i)
            || (PTR + 68..PTR + 72).contains(&i)
        {
            assert_eq!(byte, 0x5a, "signed multiply wrote outside its frame at {i}");
        }
    }
}

#[test]
fn signed_multiply_two_operand_all_aliases_code_sizes_boundary_products_and_random_match_cpu() {
    let mut cases = Vec::new();
    for aliases in [DWORDS, WORDS] {
        for target in aliases {
            for source in aliases {
                cases.push((target, source, None));
            }
        }
    }
    differential(&cases, 80);
}

#[test]
fn signed_multiply_three_operand_all_aliases_69_and_sign_extended_6b_match_cpu() {
    let mut cases = Vec::new();
    for aliases in [DWORDS, WORDS] {
        for target in aliases {
            let sign = 1 << (target.width().bits() - 1);
            let mask = target.width().mask();
            for source in aliases {
                for immediate in [0, 1, 2, 0x7f, sign - 1, sign, mask, mask - 1] {
                    cases.push((target, source, Some((immediate, false))));
                }
                for immediate in [0u8, 1, 0x7f, 0x80, 0xff] {
                    cases.push((
                        target,
                        source,
                        Some(((immediate as i8 as i32 as u32) & mask, true)),
                    ));
                }
            }
        }
    }
    differential(&cases, 72);
}

#[test]
fn signed_multiply_captures_arbitrary_register_and_immediate_sources_before_alias_write() {
    for aliases in [DWORDS, WORDS] {
        let mask = aliases[0].width().mask();
        for target in aliases {
            for left in [
                Source::Register(aliases[0]),
                Source::Register(target),
                Source::Immediate(mask),
            ] {
                for right in [
                    Source::Register(aliases[1]),
                    Source::Register(target),
                    Source::Immediate(mask - 1),
                ] {
                    let p = single(Operation::SignedMultiply {
                        target,
                        left,
                        right,
                    });
                    let mut runner = Runner::new(&p, CompileOptions::default());
                    let initial = Frame {
                        gpr: [0x8765_8001, 0x1234_ffff, 3, 4, 5, 6, 7, 8],
                        eflags: 0xa550_0cd7,
                        address: p.regions[0].entry,
                        remaining_budget: 1,
                        ..Frame::default()
                    };
                    let signed = |source| {
                        let value = match source {
                            Source::Register(r) => initial.gpr[r.index() as usize] & mask,
                            Source::Immediate(v) => v,
                            Source::VirtualRegister(_) => unreachable!("test source"),
                        };
                        if target.width() == Width::Word {
                            i64::from(value as i16)
                        } else {
                            i64::from(value as i32)
                        }
                    };
                    let product = signed(left) * signed(right);
                    let low = product as u32 & mask;
                    let overflow = product
                        != if target.width() == Width::Word {
                            i64::from(low as i16)
                        } else {
                            i64::from(low as i32)
                        };
                    let mut expected = initial;
                    put(&mut expected.gpr, target, low);
                    expected.eflags = (initial.eflags & !0x801) | if overflow { 0x801 } else { 0 };
                    expected.remaining_budget = 0;
                    expected.charged_work = 1;
                    runner.set(initial);
                    assert_eq!(runner.call(), abi::EXIT_BUDGET);
                    assert_eq!(runner.frame(), expected, "{target:?} {left:?} {right:?}");
                }
            }
        }
    }
}

#[test]
fn signed_multiply_mixes_with_alu_and_hot_structural_services() {
    let operations = vec![
        Operation::SignedMultiply {
            target: Register::Ax,
            left: Source::Register(Register::Dx),
            right: Source::Immediate(0xfffe),
        },
        Operation::Alu {
            kind: Alu::Adc,
            target: Register::Eax,
            source: Source::Immediate(0x1234_0080),
        },
        Operation::SignedMultiply {
            target: Register::Edx,
            left: Source::Register(Register::Edx),
            right: Source::Register(Register::Eax),
        },
        Operation::Alu {
            kind: Alu::Sbb,
            target: Register::Cx,
            source: Source::Register(Register::Ax),
        },
    ];
    let mut code = encoding(Register::Ax, Register::Dx, Some((0xfffe, false)), true);
    code.extend(super::encoding(operations[1]));
    code.extend(encoding(Register::Edx, Register::Eax, None, true));
    code.extend(super::encoding(operations[3]));
    for hot in [false, true] {
        let mut r = region(1, operations.clone(), Terminator::Fallthrough(RegionId(1)));
        if hot {
            r.accounting = Accounting::UncountedHot;
        }
        let program = Program { regions: vec![r] };
        let mut runner = Runner::new(
            &program,
            CompileOptions {
                service_interval_regions: 3,
                ..CompileOptions::default()
            },
        );
        let initial = Frame {
            gpr: [0x9234_80ff; 8],
            eflags: 0xa550_0cd5,
            address: program.regions[0].entry,
            remaining_budget: 1,
            charged_work: 71,
            status: 0x8000_0000,
        };
        let (mut cpu, mut bus) = reference(&code.repeat(3));
        set_cpu(&mut cpu, initial);
        for _ in 0..operations.len() * if hot { 3 } else { 1 } {
            cpu.step(&mut bus).unwrap();
        }
        runner.set(initial);
        assert_eq!(
            runner.call(),
            if hot {
                abi::EXIT_RUNTIME
            } else {
                abi::EXIT_BUDGET
            }
        );
        let expected = Frame {
            gpr: core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
            eflags: cpu.flags,
            remaining_budget: if hot { 1 } else { 0 },
            charged_work: if hot { 71 } else { 75 },
            status: initial.status | if hot { abi::UNCOUNTED_SEEN } else { 0 },
            ..initial
        };
        assert_eq!(runner.frame(), expected);
        assert_eq!(runner.store.data().events.len(), usize::from(hot));
        assert_eq!(
            runner.frame().exact_total(),
            if hot { None } else { Some(75) }
        );
    }
}

#[test]
fn signed_multiply_needs_only_original_frame_and_allocates_nothing() {
    for target in [Register::Ax, Register::Eax] {
        let p = single(Operation::SignedMultiply {
            target,
            left: Source::Register(target),
            right: Source::Immediate(target.width().mask()),
        });
        let compiled = p.compile(&CompileOptions::default()).unwrap();
        assert_eq!(
            compiled.wasm(),
            p.compile(&CompileOptions {
                direct_memory: true,
                ..CompileOptions::default()
            })
            .unwrap()
            .wasm()
        );
        let mut config = Config::default();
        config.wasm_tail_call(true);
        let module = Module::new(&Engine::new(&config), compiled.wasm()).unwrap();
        assert_eq!(module.imports().count(), 2);
        let mut runner = Runner::with_compiled(&compiled);
        let ptr = runner.memory.data(&runner.store).len() - abi::FRAME_BYTES;
        let initial = Frame {
            gpr: [0x8000_8000; 8],
            eflags: 0xa550_0cd5,
            address: p.regions[0].entry,
            remaining_budget: 1,
            ..Frame::default()
        };
        let mut execute = || {
            initial
                .write(&mut runner.memory.data_mut(&mut runner.store)[ptr..])
                .unwrap();
            assert_eq!(
                runner.run.call(&mut runner.store, ptr as u32).unwrap(),
                abi::EXIT_BUDGET
            );
        };
        execute();
        let allocation = allocation_counter::measure(|| {
            for _ in 0..1000 {
                execute();
            }
        });
        assert_eq!(allocation.count_total, 0);
        assert_eq!(allocation.bytes_total, 0);
    }
}

#[test]
fn signed_multiply_rejects_byte_targets_mismatched_sources_and_unextended_immediates() {
    for target in [Register::Al, Register::Ah] {
        assert_eq!(
            single(Operation::SignedMultiply {
                target,
                left: Source::Register(target),
                right: Source::Immediate(1)
            })
            .compile(&CompileOptions::default())
            .unwrap_err(),
            CompileError::WidthMismatch(RegionId(7))
        );
    }
    for target in DWORDS.into_iter().chain(WORDS) {
        for source in DWORDS
            .into_iter()
            .chain(WORDS)
            .chain([Register::Al, Register::Ah])
        {
            if target.width() == source.width() {
                continue;
            }
            for left_bad in [false, true] {
                let good = Source::Register(target);
                let bad = Source::Register(source);
                assert_eq!(
                    single(Operation::SignedMultiply {
                        target,
                        left: if left_bad { bad } else { good },
                        right: if left_bad { good } else { bad }
                    })
                    .compile(&CompileOptions::default())
                    .unwrap_err(),
                    CompileError::WidthMismatch(RegionId(7))
                );
            }
        }
    }
    for left_bad in [false, true] {
        assert_eq!(
            single(Operation::SignedMultiply {
                target: Register::Ax,
                left: Source::Immediate(if left_bad { 0xffff_ffff } else { 1 }),
                right: Source::Immediate(if left_bad { 1 } else { 0x10000 })
            })
            .compile(&CompileOptions::default())
            .unwrap_err(),
            CompileError::ImmediateOutOfRange(RegionId(7))
        );
    }
}
