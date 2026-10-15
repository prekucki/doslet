use doslet_x86::x87_decode::{Encoding, MemoryFormat, Mnemonic, Operands, decode};

#[test]
fn all_2048_encodings_have_the_expected_isa_class() {
    // Intel opcode maps A-7..A-22, with P6/SSE3 additions separated from 387.
    let memory_groups_387 = [0xFF_u8, 0xFD, 0xFF, 0xAD, 0xFF, 0xDD, 0xFF, 0xFD];
    let register_387: [&[(u8, u8)]; 8] = [
        &[(0xC0, 0xFF)],
        &[
            (0xC0, 0xD0),
            (0xE0, 0xE1),
            (0xE4, 0xE5),
            (0xE8, 0xEE),
            (0xF0, 0xFF),
        ],
        &[(0xE9, 0xE9)],
        &[(0xE0, 0xE4)],
        &[(0xC0, 0xCF), (0xE0, 0xFF)],
        &[(0xC0, 0xC7), (0xD0, 0xEF)],
        &[(0xC0, 0xCF), (0xD9, 0xD9), (0xE0, 0xFF)],
        &[(0xE0, 0xE0)],
    ];
    let mut counts = [0; 3];
    for opcode in 0xD8..=0xDF {
        for modrm in 0..=255 {
            let group = (modrm >> 3) & 7;
            let i387 = if modrm < 0xC0 {
                memory_groups_387[usize::from(opcode - 0xD8)] & (1 << group) != 0
            } else {
                register_387[usize::from(opcode - 0xD8)]
                    .iter()
                    .any(|&(first, last)| (first..=last).contains(&modrm))
            };
            let later = if modrm < 0xC0 {
                matches!(opcode, 0xDB | 0xDD | 0xDF) && group == 1
            } else {
                (matches!(opcode, 0xDA | 0xDB) && modrm <= 0xDF)
                    || (matches!(opcode, 0xDB | 0xDF) && (0xE8..=0xF7).contains(&modrm))
            };
            let encoding = decode(opcode, modrm);
            assert_eq!(
                matches!(encoding, Encoding::I387(_)),
                i387,
                "{opcode:02X} {modrm:02X}"
            );
            assert_eq!(
                matches!(encoding, Encoding::LaterIsa(_)),
                later,
                "{opcode:02X} {modrm:02X}"
            );
            counts[if i387 {
                0
            } else if later {
                1
            } else {
                2
            }] += 1;
        }
    }
    assert_eq!(counts, [1620, 168, 260]);
    for opcode in 0..=255 {
        if !(0xD8..=0xDF).contains(&opcode) {
            assert_eq!(decode(opcode, 0), Encoding::NotX87);
        }
    }
}

#[test]
fn arithmetic_operand_direction_and_pop_count_match_intel_syntax() {
    use Mnemonic::*;
    for (opcode, names) in [
        (0xD8, [Fadd, Fmul, Fcom, Fcomp, Fsub, Fsubr, Fdiv, Fdivr]),
        (0xDC, [Fadd, Fmul, Fcom, Fcomp, Fsubr, Fsub, Fdivr, Fdiv]),
        (
            0xDE,
            [Faddp, Fmulp, Fcom, Fcompp, Fsubrp, Fsubp, Fdivrp, Fdivp],
        ),
    ] {
        for group in 0..8 {
            for index in 0..8 {
                let Encoding::I387(instruction) = decode(opcode, 0xC0 | (group << 3) | index)
                else {
                    assert!(opcode != 0xD8 && matches!(group, 2 | 3));
                    continue;
                };
                assert_eq!(instruction.mnemonic, names[usize::from(group)]);
                if opcode == 0xDE && group == 3 {
                    assert_eq!(instruction.operands, Operands::None);
                    assert_eq!(instruction.pops(), 2);
                } else {
                    let operands = if opcode != 0xD8 {
                        Operands::StackTop(index)
                    } else if matches!(group, 2 | 3) {
                        Operands::Stack(index)
                    } else {
                        Operands::TopStack(index)
                    };
                    assert_eq!(instruction.operands, operands);
                    assert_eq!(instruction.pops(), u8::from(opcode == 0xDE || group == 3));
                }
            }
        }
    }
}

#[test]
fn memory_groups_name_their_formats_including_extended_and_state_layouts() {
    use MemoryFormat::*;
    use Mnemonic::*;
    let rows = [
        (
            0xD9,
            [
                Some((Fld, Real32)),
                None,
                Some((Fst, Real32)),
                Some((Fstp, Real32)),
                Some((Fldenv, Environment)),
                Some((Fldcw, Control16)),
                Some((Fnstenv, Environment)),
                Some((Fnstcw, Control16)),
            ],
        ),
        (
            0xDB,
            [
                Some((Fild, Int32)),
                None,
                Some((Fist, Int32)),
                Some((Fistp, Int32)),
                None,
                Some((Fld, Real80)),
                None,
                Some((Fstp, Real80)),
            ],
        ),
        (
            0xDD,
            [
                Some((Fld, Real64)),
                None,
                Some((Fst, Real64)),
                Some((Fstp, Real64)),
                Some((Frstor, State)),
                None,
                Some((Fnsave, State)),
                Some((Fnstsw, Status16)),
            ],
        ),
        (
            0xDF,
            [
                Some((Fild, Int16)),
                None,
                Some((Fist, Int16)),
                Some((Fistp, Int16)),
                Some((Fbld, Bcd80)),
                Some((Fild, Int64)),
                Some((Fbstp, Bcd80)),
                Some((Fistp, Int64)),
            ],
        ),
    ];
    for (opcode, row) in rows {
        for (group, expected) in row.into_iter().enumerate() {
            if let Some((mnemonic, format)) = expected {
                for mode in [0, 0x40, 0x80] {
                    for rm in 0..8 {
                        let Encoding::I387(instruction) =
                            decode(opcode, mode | (group as u8 * 8) | rm)
                        else {
                            panic!()
                        };
                        assert_eq!(instruction.mnemonic, mnemonic);
                        assert_eq!(instruction.operands, Operands::Memory(format));
                    }
                }
            }
        }
    }
    assert_eq!(
        (Environment.bytes(false), Environment.bytes(true)),
        (14, 28)
    );
    assert_eq!((State.bytes(false), State.bytes(true)), (94, 108));
    assert_eq!(Real80.bytes(true), 10);
}
