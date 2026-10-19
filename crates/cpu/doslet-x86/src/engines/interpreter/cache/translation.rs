//! Portable register-only translation IR, decoded by the execution decoder.
//! No instruction here accesses guest memory, devices, segments or control flags.
use super::{Instruction as Decoded, Operation as Op, Target, Value};

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
/// Length from the ordinary execution decoder. Foreign owners use this only
/// to validate instruction boundaries in their frozen source proofs.
pub fn instruction_length(bytes: &[u8], code32: bool) -> Option<u8> {
    Some(Decoded::decode_mode(bytes, code32)?.length)
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
/// Control-flow shape from the execution decoder, without executing operands.
pub fn near_flow(bytes: &[u8], code32: bool) -> Option<NearFlow> {
    let instruction = Decoded::decode_mode(bytes, code32)?;
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
