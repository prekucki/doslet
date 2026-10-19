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
const BYTES: [Register; 8] = [
    Register::Al,
    Register::Cl,
    Register::Dl,
    Register::Bl,
    Register::Ah,
    Register::Ch,
    Register::Dh,
    Register::Bh,
];

fn encoding(target: Register, source: Register, signed: bool, code32: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if (target.width() == Width::Dword) != code32 {
        bytes.push(0x66);
    }
    bytes.extend_from_slice(&[
        0x0f,
        0xb6 | u8::from(source.width() == Width::Word) | (u8::from(signed) << 3),
        0xc0 | register_code(target) << 3 | register_code(source),
    ]);
    bytes
}

fn reference_mode(code: &[u8], code32: bool) -> (Cpu, Bus) {
    let (mut cpu, mut bus) = reference(code);
    bus.0[0x20e] = if code32 { 0xcf } else { 0x8f };
    cpu.load_segment(&mut bus, SegmentRegister::Cs, 8).unwrap();
    (cpu, bus)
}

fn set_reference(cpu: &mut Cpu, frame: Frame) {
    cpu.registers.ip = 0x1000;
    cpu.flags = frame.eflags;
    for (r, v) in REGS.into_iter().zip(frame.gpr) {
        cpu.registers.set32(r, v);
    }
}

#[test]
fn extension_all_alias_pairs_signedness_code_sizes_boundaries_and_random_match_cpu() {
    let mut cases = Vec::new();
    for target in DWORDS.into_iter().chain(WORDS) {
        for source in BYTES.into_iter().chain(WORDS) {
            for signed in [false, true] {
                cases.push((target, source, signed));
            }
        }
    }
    let program = Program {
        regions: cases
            .iter()
            .enumerate()
            .map(|(i, &(target, source, signed))| {
                let id = i as u32 + 1;
                region(
                    id,
                    vec![Operation::Extend {
                        target,
                        source,
                        signed,
                    }],
                    Terminator::Fallthrough(RegionId(id)),
                )
            })
            .collect(),
    };
    let mut runner = Runner::new(&program, CompileOptions::default());
    let boundaries = [
        0,
        1,
        0x7f,
        0x80,
        0xff,
        0x7fff,
        0x8000,
        0xffff,
        0x1234_8081,
        u32::MAX,
    ];
    let mut seed = 0x7123_89ab_u32;
    for code32 in [false, true] {
        for (i, &(target, source, signed)) in cases.iter().enumerate() {
            let (mut cpu, mut bus) =
                reference_mode(&encoding(target, source, signed, code32), code32);
            for sample in 0..18 {
                let mut gpr = core::array::from_fn(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    seed
                });
                let value = if sample < boundaries.len() {
                    boundaries[sample]
                } else {
                    seed
                };
                let r = &mut gpr[source.index() as usize];
                *r = (*r & !(source.width().mask() << source.shift()))
                    | ((value & source.width().mask()) << source.shift());
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
                    set_reference(&mut cpu, initial);
                    cpu.step(&mut bus).unwrap();
                    assert_eq!(runner.call(), abi::EXIT_BUDGET);
                    assert_eq!(
                        runner.frame(),
                        Frame {
                            gpr: core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
                            remaining_budget: 0,
                            charged_work: 40,
                            ..initial
                        },
                        "target={target:?} source={source:?} signed={signed} code32={code32} value={value:x}"
                    );
                    assert_eq!(cpu.flags, initial.eflags);
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
            assert_eq!(byte, 0x5a, "register extension wrote memory at {i}");
        }
    }
}

#[test]
fn extensions_mix_with_register_alu_and_hot_structural_services() {
    let operations = vec![
        Operation::Extend {
            target: Register::Ax,
            source: Register::Ah,
            signed: true,
        },
        Operation::Alu {
            kind: Alu::Adc,
            target: Register::Eax,
            source: Source::Immediate(0x1234_0080),
        },
        Operation::Extend {
            target: Register::Ecx,
            source: Register::Al,
            signed: false,
        },
        Operation::Exchange {
            left: Register::Ah,
            right: Register::Cl,
        },
        Operation::Extend {
            target: Register::Eax,
            source: Register::Ax,
            signed: true,
        },
        Operation::Extend {
            target: Register::Dx,
            source: Register::Dx,
            signed: false,
        },
    ];
    let mut code = Vec::new();
    for &op in &operations {
        if let Operation::Extend {
            target,
            source,
            signed,
        } = op
        {
            code.extend(encoding(target, source, signed, true));
        } else {
            code.extend(super::encoding(op));
        }
    }
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
        set_reference(&mut cpu, initial);
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
            charged_work: if hot { 71 } else { 77 },
            status: initial.status | if hot { abi::UNCOUNTED_SEEN } else { 0 },
            ..initial
        };
        assert_eq!(runner.frame(), expected);
        assert_eq!(
            runner.frame().exact_total(),
            if hot { None } else { Some(77) }
        );
        if hot {
            assert_eq!(
                runner.store.data().events,
                vec![(abi::REASON_SERVICE, expected)]
            );
        } else {
            assert!(runner.store.data().events.is_empty());
        }
    }
}

#[test]
fn register_extension_needs_only_original_frame_and_allocates_nothing() {
    for signed in [false, true] {
        let program = single(Operation::Extend {
            target: Register::Eax,
            source: Register::Ah,
            signed,
        });
        let compiled = program.compile(&CompileOptions::default()).unwrap();
        assert_eq!(
            compiled.wasm(),
            program
                .compile(&CompileOptions {
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
            gpr: [0x9234_80ff; 8],
            eflags: 0xa550_0cd5,
            address: program.regions[0].entry,
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
            let result = Frame::read(&runner.memory.data(&runner.store)[ptr..]).unwrap();
            assert_eq!(result.gpr[0], if signed { 0xffff_ff80 } else { 0x80 });
            assert_eq!(result.eflags, initial.eflags);
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
fn register_extension_rejects_byte_targets_and_dword_sources() {
    for target in DWORDS.into_iter().chain(WORDS).chain(BYTES) {
        for source in DWORDS.into_iter().chain(WORDS).chain(BYTES) {
            if target.width() != Width::Byte && source.width() != Width::Dword {
                continue;
            }
            for signed in [false, true] {
                assert_eq!(
                    single(Operation::Extend {
                        target,
                        source,
                        signed
                    })
                    .compile(&CompileOptions::default())
                    .unwrap_err(),
                    CompileError::WidthMismatch(RegionId(7))
                );
            }
        }
    }
}
