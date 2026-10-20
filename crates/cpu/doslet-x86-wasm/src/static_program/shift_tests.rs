use super::*;

const ALIASES: [Register; 24] = [
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
const SHIFTS: [Shift; 7] = [
    Shift::Shl,
    Shift::Shr,
    Shift::Sar,
    Shift::Rol,
    Shift::Ror,
    Shift::Rcl,
    Shift::Rcr,
];

pub(super) fn encoding(op: Operation) -> Vec<u8> {
    let (target, count) = match op {
        Operation::Shift { target, count, .. } | Operation::DoubleShift { target, count, .. } => {
            (target, count)
        }
        _ => unreachable!("shift fixture"),
    };
    let mut code = Vec::new();
    if target.width() == Width::Word {
        code.push(0x66);
    }
    match op {
        Operation::Shift { kind, .. } => {
            let group = match kind {
                Shift::Rol => 0,
                Shift::Ror => 1,
                Shift::Rcl => 2,
                Shift::Rcr => 3,
                Shift::Shl => 4,
                Shift::Shr => 5,
                Shift::Sar => 7,
            };
            code.extend_from_slice(&[
                match count {
                    ShiftCount::One => 0xd0,
                    ShiftCount::Immediate(_) => 0xc0,
                    ShiftCount::Cl => 0xd2,
                } + u8::from(target.width() != Width::Byte),
                0xc0 | group << 3 | register_code(target),
            ]);
        }
        Operation::DoubleShift { kind, source, .. } => code.extend_from_slice(&[
            0x0f,
            match kind {
                DoubleShift::Left => 0xa4,
                DoubleShift::Right => 0xac,
            } + u8::from(count == ShiftCount::Cl),
            0xc0 | register_code(source) << 3 | register_code(target),
        ]),
        _ => unreachable!("shift fixture"),
    }
    if let ShiftCount::Immediate(n) = count {
        code.push(n);
    } else if count == ShiftCount::One && matches!(op, Operation::DoubleShift { .. }) {
        code.push(1);
    }
    code
}

fn counts(width: Width) -> [u8; 9] {
    let n = width.bits() as u8;
    [0, 1, n - 1, n, n + 1, 31, 32, 33, 255]
}

fn put(gpr: &mut [u32; 8], register: Register, value: u32) {
    let shift = register.shift();
    let mask = register.width().mask();
    let backing = &mut gpr[register.index() as usize];
    *backing = (*backing & !(mask << shift)) | ((value & mask) << shift);
}

/// Compare complete frames against an independently decoded reference CPU step.
/// Reuse each compiled module for boundary values, random values and both CFs.
fn compare(op: Operation, count_values: &[u8], sal_encoding: bool) {
    let program = single(op);
    let mut runner = Runner::new(&program, CompileOptions::default());
    let mut code = encoding(op);
    if sal_encoding {
        let modrm = usize::from(code[0] == 0x66) + 1;
        code[modrm] |= 0x10; // SHL /4 and SAL /6 share the same IR operation.
    }
    let (mut cpu, mut bus) = reference(&code);
    let (target, source, count) = match op {
        Operation::Shift { target, count, .. } => (target, None, count),
        Operation::DoubleShift {
            target,
            source,
            count,
            ..
        } => (target, Some(source), count),
        _ => unreachable!("shift fixture"),
    };
    let sign = 1 << (target.width().bits() - 1);
    let values = [
        0,
        1,
        2,
        sign - 1,
        sign,
        sign + 1,
        target.width().mask(),
        0xa55a_c33c,
    ];
    let mut seed = 0x5432_abcd_u32;
    for &n in count_values {
        for sample in 0..values.len() + 12 {
            for flags in [0, 1, 0x8d4, 0x8d7] {
                let mut gpr = core::array::from_fn(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    seed
                });
                let value = values.get(sample).copied().unwrap_or(seed);
                put(&mut gpr, target, value);
                if let Some(source) = source {
                    put(
                        &mut gpr,
                        source,
                        if sample < values.len() {
                            values[(sample + 3) % values.len()]
                        } else {
                            seed
                        },
                    );
                }
                // Set CL last so ECX/CX/CL destinations and sources really
                // overlap the count instead of hiding aliasing behind setup.
                if count == ShiftCount::Cl {
                    put(&mut gpr, Register::Cl, u32::from(n));
                }
                let initial = Frame {
                    gpr,
                    eflags: 0xa550_0400 | flags,
                    address: program.regions[0].entry,
                    remaining_budget: 1,
                    charged_work: 39,
                    status: 0x8000_0000,
                };
                runner.set(initial);
                cpu.registers.ip = 0x1000;
                cpu.flags = initial.eflags;
                for (r, value) in REGS.into_iter().zip(gpr) {
                    cpu.registers.set32(r, value);
                }
                cpu.step(&mut bus).unwrap();
                assert_eq!(runner.call(), abi::EXIT_BUDGET);
                let expected = Frame {
                    gpr: core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
                    eflags: cpu.flags,
                    remaining_budget: 0,
                    charged_work: 40,
                    ..initial
                };
                assert_eq!(
                    runner.frame(),
                    expected,
                    "{op:?}, CL={n}, value={value:#x}, flags={flags:#x}"
                );
                assert!(runner.store.data().events.is_empty());
            }
        }
    }
}

#[test]
fn shifts_and_rotates_match_reference_for_every_alias_count_and_carry() {
    for kind in SHIFTS {
        for target in ALIASES {
            for n in counts(target.width()) {
                compare(
                    Operation::Shift {
                        kind,
                        target,
                        count: ShiftCount::Immediate(n),
                    },
                    &[n],
                    false,
                );
            }
            compare(
                Operation::Shift {
                    kind,
                    target,
                    count: ShiftCount::One,
                },
                &[1],
                false,
            );
            compare(
                Operation::Shift {
                    kind,
                    target,
                    count: ShiftCount::Cl,
                },
                &counts(target.width()),
                false,
            );
        }
    }
}

#[test]
fn sal_encoding_has_the_same_results_as_shl() {
    for target in [Register::Ah, Register::Cx, Register::Ebp] {
        for count in [ShiftCount::One, ShiftCount::Immediate(23), ShiftCount::Cl] {
            compare(
                Operation::Shift {
                    kind: Shift::Shl,
                    target,
                    count,
                },
                &counts(target.width()),
                true,
            );
        }
    }
}

#[test]
fn double_shifts_match_reference_with_count_and_source_aliasing() {
    // Cover every word/dword destination and its own source, plus overlap with
    // CX/ECX whether it supplies the source, destination or count.
    for target in ALIASES.into_iter().filter(|r| r.width() != Width::Byte) {
        let count_register = if target.width() == Width::Word {
            Register::Cx
        } else {
            Register::Ecx
        };
        for source in [
            target,
            count_register,
            if target.width() == Width::Word {
                Register::Bp
            } else {
                Register::Ebp
            },
        ] {
            for kind in [DoubleShift::Left, DoubleShift::Right] {
                for n in counts(target.width()) {
                    if target.width() == Width::Word && n & 31 > 16 {
                        continue;
                    }
                    compare(
                        Operation::DoubleShift {
                            kind,
                            target,
                            source,
                            count: ShiftCount::Immediate(n),
                        },
                        &[n],
                        false,
                    );
                }
                compare(
                    Operation::DoubleShift {
                        kind,
                        target,
                        source,
                        count: ShiftCount::One,
                    },
                    &[1],
                    false,
                );
                if target.width() == Width::Dword {
                    compare(
                        Operation::DoubleShift {
                            kind,
                            target,
                            source,
                            count: ShiftCount::Cl,
                        },
                        &counts(target.width()),
                        false,
                    );
                }
            }
        }
    }
}

#[test]
fn double_shift_rejects_undefined_word_counts_and_invalid_widths() {
    for kind in [DoubleShift::Left, DoubleShift::Right] {
        for (target, source) in [
            (Register::Al, Register::Bl),
            (Register::Ax, Register::Ebx),
            (Register::Eax, Register::Bx),
        ] {
            let error = single(Operation::DoubleShift {
                kind,
                target,
                source,
                count: ShiftCount::One,
            })
            .compile(&CompileOptions::default())
            .unwrap_err();
            assert_eq!(error, CompileError::WidthMismatch(RegionId(7)));
        }
        for count in [
            ShiftCount::Cl,
            ShiftCount::Immediate(17),
            ShiftCount::Immediate(31),
            ShiftCount::Immediate(255),
        ] {
            let op = Operation::DoubleShift {
                kind,
                target: Register::Ax,
                source: Register::Cx,
                count,
            };
            assert_eq!(
                single(op).compile(&CompileOptions::default()).unwrap_err(),
                CompileError::InvalidShiftCount(RegionId(7))
            );
            // Immediate values are a real unsupported CPU domain, not merely
            // an emitter limitation. Dynamic word CL is conservatively refused.
            if count != ShiftCount::Cl {
                let (mut cpu, mut bus) = reference(&encoding(op));
                cpu.registers.ip = 0x1000;
                assert!(cpu.step(&mut bus).is_err());
            }
        }
    }
}

#[test]
fn shift_sequences_keep_aliases_and_flags_across_direct_edges() {
    let operations = [
        Operation::Shift {
            kind: Shift::Sar,
            target: Register::Eax,
            count: ShiftCount::Immediate(16),
        },
        Operation::Shift {
            kind: Shift::Shl,
            target: Register::Ebp,
            count: ShiftCount::Immediate(23),
        },
        Operation::DoubleShift {
            kind: DoubleShift::Left,
            target: Register::Eax,
            source: Register::Ebp,
            count: ShiftCount::Immediate(9),
        },
        Operation::DoubleShift {
            kind: DoubleShift::Right,
            target: Register::Ecx,
            source: Register::Eax,
            count: ShiftCount::Cl,
        },
        Operation::Shift {
            kind: Shift::Rcl,
            target: Register::Cl,
            count: ShiftCount::Cl,
        },
        Operation::Shift {
            kind: Shift::Rcr,
            target: Register::Ah,
            count: ShiftCount::Cl,
        },
        Operation::Shift {
            kind: Shift::Ror,
            target: Register::Cx,
            count: ShiftCount::One,
        },
        Operation::Shift {
            kind: Shift::Rol,
            target: Register::Ch,
            count: ShiftCount::Immediate(9),
        },
    ];
    let program = Program {
        regions: operations
            .into_iter()
            .enumerate()
            .map(|(i, op)| {
                region(
                    i as u32,
                    vec![op],
                    Terminator::Fallthrough(RegionId((i as u32 + 1) % operations.len() as u32)),
                )
            })
            .collect(),
    };
    let mut runner = Runner::new(&program, CompileOptions::default());
    let code: Vec<_> = operations.into_iter().flat_map(encoding).collect();
    let (mut cpu, mut bus) = reference(&code);
    for flags in [0, 1, 0x8d4, 0x8d7] {
        let initial = Frame {
            gpr: [0x9876_abcd, 0xb3a1_1709, 9, 17, 1, 0x1234_cafe, 23, 77],
            eflags: flags,
            address: program.regions[0].entry,
            remaining_budget: operations.len() as u64,
            ..Frame::default()
        };
        runner.set(initial);
        cpu.flags = flags;
        cpu.registers.ip = 0x1000;
        for (r, value) in REGS.into_iter().zip(initial.gpr) {
            cpu.registers.set32(r, value);
        }
        for _ in operations {
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        assert_eq!(
            runner.frame().gpr,
            core::array::from_fn(|i| cpu.registers.get32(REGS[i]))
        );
        assert_eq!(runner.frame().eflags, cpu.flags);
        assert_eq!(runner.frame().charged_work, operations.len() as u64);
        assert!(runner.store.data().events.is_empty());
    }
}
