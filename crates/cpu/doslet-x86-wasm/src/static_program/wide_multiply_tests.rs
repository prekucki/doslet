use super::*;

const REGISTERS: [Register; 24] = [
    Register::Eax,
    Register::Ecx,
    Register::Edx,
    Register::Ebx,
    Register::Esp,
    Register::Ebp,
    Register::Esi,
    Register::Edi,
    Register::Ax,
    Register::Cx,
    Register::Dx,
    Register::Bx,
    Register::Sp,
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

fn accumulator(width: Width) -> Register {
    match width {
        Width::Byte => Register::Al,
        Width::Word => Register::Ax,
        Width::Dword => Register::Eax,
    }
}

fn put(gpr: &mut [u32; 8], register: Register, value: u32) {
    let mask = register.width().mask() << register.shift();
    let old = &mut gpr[register.index() as usize];
    *old = (*old & !mask) | ((value << register.shift()) & mask);
}

fn encoding(register: Register, signed: bool, code32: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if register.width() != Width::Byte && (register.width() == Width::Dword) != code32 {
        bytes.push(0x66);
    }
    bytes.extend_from_slice(&[
        if register.width() == Width::Byte {
            0xf6
        } else {
            0xf7
        },
        0xc0 | (if signed { 5 } else { 4 }) << 3 | register_code(register),
    ]);
    bytes
}

fn set_cpu(cpu: &mut Cpu, frame: Frame) {
    cpu.registers.ip = 0x1000;
    cpu.flags = frame.eflags;
    for (register, value) in REGS.into_iter().zip(frame.gpr) {
        cpu.registers.set32(register, value);
    }
}

#[test]
fn wide_multiply_all_aliases_signedness_code_sizes_boundary_products_and_random_match_cpu() {
    let cases: Vec<_> = REGISTERS
        .into_iter()
        .flat_map(|register| [false, true].map(|signed| (register, signed)))
        .collect();
    let p = Program {
        regions: cases
            .iter()
            .enumerate()
            .map(|(i, &(register, signed))| {
                let id = i as u32 + 1;
                region(
                    id,
                    vec![Operation::WideMultiply {
                        signed,
                        width: register.width(),
                        source: Source::Register(register),
                    }],
                    Terminator::Fallthrough(RegionId(id)),
                )
            })
            .collect(),
    };
    let mut runner = Runner::new(&p, CompileOptions::default());
    let mut seed = 0x9b31_24e7u32;
    for code32 in [false, true] {
        for (i, &(register, signed)) in cases.iter().enumerate() {
            let (mut cpu, mut bus) = reference(&encoding(register, signed, code32));
            bus.0[0x20e] = if code32 { 0xcf } else { 0x8f };
            cpu.load_segment(&mut bus, SegmentRegister::Cs, 8).unwrap();
            let width = register.width();
            let sign = 1 << (width.bits() - 1);
            let boundaries = [
                0,
                1,
                2,
                0x7f,
                sign - 1,
                sign,
                width.mask(),
                width.mask() - 1,
            ];
            for sample in 0..80 {
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
                put(&mut gpr, accumulator(width), left);
                put(&mut gpr, register, right);
                for flags in [0, 1, 0x8d4, 0x8d7] {
                    let initial = Frame {
                        gpr,
                        eflags: 0xa550_0400 | flags,
                        address: p.regions[i].entry,
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
                        "register={register:?} signed={signed} code32={code32} sample={sample}"
                    );
                    assert_eq!(runner.frame().eflags & !0x801, initial.eflags & !0x801);
                    assert_eq!(runner.frame().eflags & 1, runner.frame().eflags >> 11 & 1);
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
            assert_eq!(byte, 0x5a, "wide multiply wrote outside its frame at {i}");
        }
    }
}

#[test]
fn wide_multiply_immediates_capture_accumulator_and_preserve_destination_high_halves() {
    for width in [Width::Byte, Width::Word, Width::Dword] {
        let sign = 1 << (width.bits() - 1);
        for signed in [false, true] {
            for left in [0, 1, 2, sign - 1, sign, width.mask()] {
                for right in [0, 1, 2, sign - 1, sign, width.mask()] {
                    let p = single(Operation::WideMultiply {
                        signed,
                        width,
                        source: Source::Immediate(right),
                    });
                    let mut runner = Runner::new(&p, CompileOptions::default());
                    let mut initial = Frame {
                        gpr: [0xa531_7b92, 0xa53a_8001, 0x5ac7_2de9, 4, 5, 6, 7, 8],
                        eflags: 0xa550_0cd5,
                        address: p.regions[0].entry,
                        remaining_budget: 1,
                        ..Frame::default()
                    };
                    put(&mut initial.gpr, accumulator(width), left);
                    // Independently sign-extend using the operand's numeric range.
                    let operand = |value: u32| {
                        i128::from(value)
                            - if signed && value & sign != 0 {
                                1i128 << width.bits()
                            } else {
                                0
                            }
                    };
                    let product = operand(left) * operand(right);
                    let overflow = if signed {
                        product < -i128::from(sign) || product >= i128::from(sign)
                    } else {
                        product > i128::from(width.mask())
                    };
                    let mut expected = initial;
                    if width == Width::Byte {
                        put(&mut expected.gpr, Register::Ax, product as u32);
                    } else {
                        put(&mut expected.gpr, accumulator(width), product as u32);
                        put(
                            &mut expected.gpr,
                            if width == Width::Word {
                                Register::Dx
                            } else {
                                Register::Edx
                            },
                            (product >> width.bits()) as u32,
                        );
                    }
                    expected.eflags = (initial.eflags & !0x801) | if overflow { 0x801 } else { 0 };
                    expected.remaining_budget = 0;
                    expected.charged_work = 1;
                    runner.set(initial);
                    assert_eq!(runner.call(), abi::EXIT_BUDGET);
                    assert_eq!(
                        runner.frame(),
                        expected,
                        "{width:?} signed={signed} {left:#x} * {right:#x}"
                    );
                }
            }
        }
    }
}

#[test]
fn wide_multiply_mixes_with_alu_and_hot_structural_services() {
    let operations = vec![
        Operation::WideMultiply {
            signed: false,
            width: Width::Byte,
            source: Source::Register(Register::Ah),
        },
        Operation::Alu {
            kind: Alu::Adc,
            target: Register::Eax,
            source: Source::Immediate(0x1234_0080),
        },
        Operation::WideMultiply {
            signed: true,
            width: Width::Word,
            source: Source::Register(Register::Dx),
        },
        Operation::Alu {
            kind: Alu::Sbb,
            target: Register::Cx,
            source: Source::Register(Register::Ax),
        },
        Operation::WideMultiply {
            signed: true,
            width: Width::Dword,
            source: Source::Register(Register::Edx),
        },
    ];
    let mut code = encoding(Register::Ah, false, true);
    code.extend(super::encoding(operations[1]));
    code.extend(encoding(Register::Dx, true, true));
    code.extend(super::encoding(operations[3]));
    code.extend(encoding(Register::Edx, true, true));
    for hot in [false, true] {
        let mut r = region(1, operations.clone(), Terminator::Fallthrough(RegionId(1)));
        if hot {
            r.accounting = Accounting::UncountedHot;
        }
        let p = Program { regions: vec![r] };
        let mut runner = Runner::new(
            &p,
            CompileOptions {
                service_interval_regions: 3,
                ..CompileOptions::default()
            },
        );
        let initial = Frame {
            gpr: [0x9234_80ff; 8],
            eflags: 0xa550_0cd5,
            address: p.regions[0].entry,
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
        assert_eq!(
            runner.frame(),
            Frame {
                gpr: core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
                eflags: cpu.flags,
                remaining_budget: if hot { 1 } else { 0 },
                charged_work: if hot { 71 } else { 76 },
                status: initial.status | if hot { abi::UNCOUNTED_SEEN } else { 0 },
                ..initial
            }
        );
        assert_eq!(runner.store.data().events.len(), usize::from(hot));
        assert_eq!(
            runner.frame().exact_total(),
            if hot { None } else { Some(76) }
        );
    }
}

#[test]
fn wide_multiply_needs_only_original_frame_and_allocates_nothing() {
    for width in [Width::Byte, Width::Word, Width::Dword] {
        for signed in [false, true] {
            let p = single(Operation::WideMultiply {
                signed,
                width,
                source: Source::Register(accumulator(width)),
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
            assert_eq!(
                Module::new(&Engine::new(&config), compiled.wasm())
                    .unwrap()
                    .imports()
                    .count(),
                2
            );
            let mut runner = Runner::with_compiled(&compiled);
            let ptr = runner.memory.data(&runner.store).len() - abi::FRAME_BYTES;
            let initial = Frame {
                gpr: [0x8000_8080; 8],
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
}

#[test]
fn wide_multiply_rejects_mismatched_sources_and_out_of_width_immediates() {
    for width in [Width::Byte, Width::Word, Width::Dword] {
        for signed in [false, true] {
            for source in REGISTERS {
                if source.width() == width {
                    continue;
                }
                assert_eq!(
                    single(Operation::WideMultiply {
                        signed,
                        width,
                        source: Source::Register(source)
                    })
                    .compile(&CompileOptions::default())
                    .unwrap_err(),
                    CompileError::WidthMismatch(RegionId(7))
                );
            }
            if width != Width::Dword {
                assert_eq!(
                    single(Operation::WideMultiply {
                        signed,
                        width,
                        source: Source::Immediate(width.mask() + 1)
                    })
                    .compile(&CompileOptions::default())
                    .unwrap_err(),
                    CompileError::ImmediateOutOfRange(RegionId(7))
                );
            }
        }
    }
}
