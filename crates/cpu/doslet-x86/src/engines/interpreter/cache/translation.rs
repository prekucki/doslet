//! Portable register-only translation IR, decoded by the execution decoder.
//! No instruction here accesses guest memory, devices, segments or control flags.
use super::{Instruction as Decoded, Operation as Op, Reader, Target, Value};

/// Boundary metadata for reference-executed two-operand IMUL. This deliberately
/// does not construct a cached operation or expand the register translation IR.
fn imul_metadata(bytes: &[u8], code32: bool) -> Option<(u8, u8)> {
    let mut reader = Reader {
        bytes: &bytes[..bytes.len().min(15)],
        position: 0,
        disp32_offset: u8::MAX,
        imm32_offset: u8::MAX,
    };
    let mut address32 = code32;
    loop {
        match reader.byte()? {
            // Operand-size overrides select valid word/dword forms but do not
            // change this instruction's encoded operand length.
            0x66 | 0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 => {}
            0x67 => address32 = !code32,
            0x0f => break,
            // LOCK/REP and other opcodes retain conservative refusal.
            _ => return None,
        }
    }
    if reader.byte()? != 0xaf {
        return None;
    }
    let modrm = reader.byte()?;
    reader.target(modrm, address32, None)?;
    Some((reader.position as u8, reader.disp32_offset))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Register(u8),
    Immediate(u32),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Move {
        target: u8,
        source: Source,
    },
    Alu {
        target: u8,
        source: Source,
        operation: u8,
        store: bool,
    },
    Inc {
        register: u8,
        decrement: bool,
    },
    Exchange {
        left: u8,
        right: u8,
    },
    Unary {
        register: u8,
        negate: bool,
    },
    Branch {
        condition: Option<u8>,
        displacement: i32,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Instruction {
    pub length: u8,
    pub operation: Operation,
}
/// Diagnostic shape from the same decoder, including widths not yet emitted.
#[derive(Clone, Copy, Debug)]
pub struct Shape {
    pub width: u8,
    pub register_family: bool,
    pub branch: bool,
}
/// Length from the execution decoder or explicitly supported reference-only
/// metadata. Foreign owners use this only to validate instruction boundaries
/// in their frozen source proofs; it does not imply translation support.
pub fn instruction_length(bytes: &[u8], code32: bool) -> Option<u8> {
    Decoded::decode_mode(bytes, code32)
        .map(|instruction| instruction.length)
        .or_else(|| imul_metadata(bytes, code32).map(|(length, _)| length))
}

/// Whether two 32-bit instruction encodings have the same decoded instruction
/// length and immutable bytes outside any 4-byte disp32 or imm32 operand slot.
pub fn same_template32(a: &[u8], b: &[u8]) -> bool {
    let Some((length, disp32_offset, imm32_offset)) = Decoded::decode_mode(a, true)
        .map(|instruction| {
            (
                instruction.length,
                instruction.disp32_offset,
                instruction.imm32_offset,
            )
        })
        .or_else(|| {
            imul_metadata(a, true).map(|(length, displacement)| (length, displacement, u8::MAX))
        })
    else {
        return false;
    };
    let len = usize::from(length);
    if a.len() < len || b.len() < len {
        return false;
    }
    if a[..len] == b[..len] {
        return true;
    }
    if disp32_offset == u8::MAX && imm32_offset == u8::MAX {
        return false;
    }
    let mut masked_a = [0u8; 15];
    let mut masked_b = [0u8; 15];
    masked_a[..len].copy_from_slice(&a[..len]);
    masked_b[..len].copy_from_slice(&b[..len]);
    for off in [disp32_offset, imm32_offset] {
        if off != u8::MAX {
            let start = usize::from(off);
            if start + 4 <= len {
                masked_a[start..start + 4].fill(0);
                masked_b[start..start + 4].fill(0);
            }
        }
    }
    masked_a[..len] == masked_b[..len]
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NearFlow {
    Next,
    Branch {
        condition: Option<u8>,
        displacement: i32,
    },
    Return {
        pop: u16,
        width: u8,
    },
    Other,
}
/// Control-flow metadata without executing operands. Reference-only IMUL
/// metadata is a fallthrough; translation/cache execution support is separate.
pub fn near_flow(bytes: &[u8], code32: bool) -> Option<NearFlow> {
    let Some(instruction) = Decoded::decode_mode(bytes, code32) else {
        return imul_metadata(bytes, code32).map(|_| NearFlow::Next);
    };
    Some(match instruction.operation {
        Op::Branch {
            condition,
            displacement,
        } if instruction.branch32 => NearFlow::Branch {
            condition,
            displacement,
        },
        Op::Return(pop) => NearFlow::Return {
            pop,
            width: instruction.width,
        },
        Op::Call(_)
        | Op::CallIndirect(_)
        | Op::JumpIndirect(_)
        | Op::Loop { .. }
        | Op::Branch { .. } => NearFlow::Other,
        _ => NearFlow::Next,
    })
}
pub fn inspect(bytes: &[u8], code32: bool) -> Option<Shape> {
    let i = Decoded::decode_mode(bytes, code32)?;
    let register_source =
        |v| matches!(v, Value::Immediate(_) | Value::Operand(Target::Register(_)));
    let register_family = match i.operation {
        Op::Move {
            target: Target::Register(_),
            source,
        }
        | Op::Alu {
            target: Target::Register(_),
            source,
            ..
        } => register_source(source),
        Op::Inc { .. }
        | Op::Exchange {
            target: Target::Register(_),
            ..
        }
        | Op::Unary {
            target: Target::Register(_),
            ..
        } => true,
        _ => false,
    };
    Some(Shape {
        width: i.width,
        register_family,
        branch: matches!(i.operation, Op::Branch { .. }),
    })
}
/// Decode default-32-bit code. Other widths and faultable operations fall back.
pub fn decode(bytes: &[u8]) -> Option<Instruction> {
    let i = Decoded::decode_mode(bytes, true)?;
    if i.width == 2 || !i.branch32 || (!i.dword && !matches!(i.operation, Op::Branch { .. })) {
        return None;
    }
    let source = |v| match v {
        Value::Immediate(v) => Some(Source::Immediate(v)),
        Value::Operand(Target::Register(r)) => Some(Source::Register(r)),
        _ => None,
    };
    let operation = match i.operation {
        Op::Move {
            target: Target::Register(target),
            source: s,
        } => Operation::Move {
            target,
            source: source(s)?,
        },
        Op::Alu {
            target: Target::Register(target),
            source: s,
            operation,
            store,
        } => Operation::Alu {
            target,
            source: source(s)?,
            operation,
            store,
        },
        Op::Inc {
            register,
            decrement,
        } => Operation::Inc {
            register,
            decrement,
        },
        Op::Exchange {
            target: Target::Register(left),
            register: right,
        } => Operation::Exchange { left, right },
        Op::Unary {
            target: Target::Register(register),
            negate,
        } => Operation::Unary { register, negate },
        Op::Branch {
            condition,
            displacement,
        } => Operation::Branch {
            condition,
            displacement,
        },
        _ => return None,
    };
    Some(Instruction {
        length: i.length,
        operation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imul_metadata_covers_widths_addressing_and_exact_truncation() {
        for (bytes, code32) in [
            (&[0x0f, 0xaf, 0xc1][..], true),
            (&[0x66, 0x0f, 0xaf, 0xc1][..], true),
            (&[0x0f, 0xaf, 0xc1][..], false),
            (&[0x66, 0x0f, 0xaf, 0xc1][..], false),
            (&[0x0f, 0xaf, 0x05, 1, 2, 3, 4][..], true),
            (&[0x0f, 0xaf, 0x44, 0x24, 0x80][..], true),
            (&[0x64, 0x0f, 0xaf, 0x84, 0x8d, 1, 2, 3, 4][..], true),
            (&[0x0f, 0xaf, 0x04, 0x25, 1, 2, 3, 4][..], true),
            (&[0x67, 0x0f, 0xaf, 0x06, 1, 2][..], true),
            (&[0x0f, 0xaf, 0x06, 1, 2][..], false),
            (&[0x67, 0x0f, 0xaf, 0x04, 0x25, 1, 2, 3, 4][..], false),
            (&[0x67, 0x67, 0x0f, 0xaf, 0x06, 1, 2][..], true),
        ] {
            assert_eq!(instruction_length(bytes, code32), Some(bytes.len() as u8));
            assert_eq!(near_flow(bytes, code32), Some(NearFlow::Next));
            for end in 0..bytes.len() {
                assert_eq!(instruction_length(&bytes[..end], code32), None);
                assert_eq!(near_flow(&bytes[..end], code32), None);
            }
            // Boundary metadata does not silently select a cached handler or
            // claim that the register translation IR can execute IMUL.
            assert!(Decoded::decode_mode(bytes, code32).is_none());
            assert!(inspect(bytes, code32).is_none());
            assert!(decode(bytes).is_none());
        }
    }

    #[test]
    fn imul_metadata_bounds_prefixes_and_masks_only_disp32_template_slots() {
        let mut longest = [0x66; 15];
        longest[12..].copy_from_slice(&[0x0f, 0xaf, 0xc1]);
        assert_eq!(instruction_length(&longest, true), Some(15));
        let mut overlong = [0x66; 16];
        overlong[13..].copy_from_slice(&[0x0f, 0xaf, 0xc1]);
        assert_eq!(instruction_length(&overlong, true), None);
        assert_eq!(near_flow(&overlong, true), None);
        for prefix in [0xf0, 0xf2, 0xf3] {
            let bytes = [prefix, 0x0f, 0xaf, 0xc1];
            assert_eq!(instruction_length(&bytes, true), None);
            assert_eq!(near_flow(&bytes, true), None);
        }
        // This fallback has no immediate field and must not rescue incomplete
        // immediate-bearing instructions or unrelated unsupported 0F forms.
        for bytes in [
            &[0x69, 0xc1, 1][..],
            &[0x6b, 0xc1],
            &[0xb8, 1, 2],
            &[0x0f, 0x0b],
        ] {
            assert_eq!(instruction_length(bytes, true), None);
            assert_eq!(near_flow(bytes, true), None);
        }
        let original = [0x64, 0x0f, 0xaf, 0x84, 0x8d, 1, 2, 3, 4];
        let mut changed = original;
        changed[5..].copy_from_slice(&[5, 6, 7, 8]);
        assert!(same_template32(&original, &changed));
        assert!(!same_template32(&original, &changed[..8]));
        for index in 0..5 {
            let mut structural = changed;
            structural[index] ^= 1;
            assert!(!same_template32(&original, &structural));
        }
        assert!(!same_template32(
            &[0x0f, 0xaf, 0x40, 1],
            &[0x0f, 0xaf, 0x40, 2]
        ));
        assert!(!same_template32(
            &[0x67, 0x0f, 0xaf, 0x06, 1, 2],
            &[0x67, 0x0f, 0xaf, 0x06, 3, 4]
        ));
    }

    #[test]
    fn diagnostic_widths_distinguish_operand_size_from_code_mode() {
        for (bytes, code32, width) in [
            (&[0xb4, 0x12][..], true, 1),
            (&[0x66, 0xb8, 1, 0][..], true, 2),
            (&[0xb8, 1, 0, 0, 0][..], true, 4),
            (&[0xb8, 1, 0][..], false, 2),
            (&[0x66, 0xb8, 1, 0, 0, 0][..], false, 4),
        ] {
            let shape = inspect(bytes, code32).unwrap();
            assert_eq!(shape.width, width);
            assert!(shape.register_family);
            assert!(!shape.branch);
        }
    }
}
