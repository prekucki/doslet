use super::*;

#[test]
fn single_string_elements_match_x86_in_both_directions_and_address_widths() {
    for width in [Width::Byte, Width::Word, Width::Dword] {
        for address16 in [false, true] {
            for backwards in [false, true] {
                let mut load = Vec::new();
                let mut store = Vec::new();
                if address16 {
                    load.push(0x67);
                    store.push(0x67);
                }
                if width == Width::Word {
                    load.push(0x66);
                    store.push(0x66);
                }
                load.push(if width == Width::Byte { 0xac } else { 0xad });
                store.push(if width == Width::Byte { 0xaa } else { 0xab });
                let (program, code) = program(
                    &[
                        (
                            Operation::StringLoad {
                                width,
                                address16,
                                segment: Segment::Ds,
                            },
                            load.as_slice(),
                        ),
                        (
                            Operation::StringStore { width, address16 },
                            store.as_slice(),
                        ),
                    ],
                    Width::Dword,
                    0,
                    true,
                );
                let mut frame = initial();
                frame.eflags = 0x8d7 | if backwards { 0x400 } else { 0 };
                frame.gpr[6] = 0x1003 | if address16 { 0xabcd0000 } else { 0 };
                frame.gpr[7] = 0x1804 | if address16 { 0x98760000 } else { 0 };
                differential(&program, &code, frame, descriptor(), true, |bytes| {
                    let at = RAM + (0x1003 - RAM_BASE) as usize;
                    bytes[at..at + 4].copy_from_slice(&0x807f12fe_u32.to_le_bytes());
                });
            }
        }
    }
}

#[test]
fn refused_string_access_changes_neither_index_nor_accumulator() {
    for operation in [
        Operation::StringLoad {
            width: Width::Dword,
            address16: false,
            segment: Segment::Ds,
        },
        Operation::StringStore {
            width: Width::Dword,
            address16: false,
        },
    ] {
        let (program, _) = program(&[(operation, &[0xad])], Width::Dword, 0, true);
        let mut runner = Runner::new(&program);
        let mut frame = initial();
        frame.gpr[6] = 0x7000;
        frame.gpr[7] = 0x7000;
        runner.set(frame, &descriptor());
        let before = runner.bytes().to_vec();
        assert_eq!(runner.call(), SIDE_EXIT);
        assert_eq!(runner.frame(), frame);
        assert_eq!(
            &runner.bytes()[RAM..RAM + RAM_LENGTH],
            &before[RAM..RAM + RAM_LENGTH]
        );
        assert_eq!(runner.effects().ram_writes, 0);
    }
}
