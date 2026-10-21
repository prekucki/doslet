// Synthetic owner-authored RLE procedure for CPU/runtime/compiler tests.
// Its instruction encodings and code layout are generated from operations;
// no executable image or commercial instruction bytes are included.

use super::rle_api::{
    Address, Alu, Condition, Operation, Register, Source, Width,
    memory::{MemoryAddress, Segment},
    procedure_memory::{
        ProcedureBlock, ProcedureGraphFunction, ProcedureId, ProcedureInstruction,
        ProcedureTerminator, ProcedureTerminatorKind,
    },
};
use alloc::{vec, vec::Vec};

pub struct RleFixture {
    pub function: ProcedureGraphFunction,
    pub code: Vec<u8>,
}
fn mem(base: Register, index: Option<Register>, displacement: u32) -> MemoryAddress {
    MemoryAddress {
        base: Some(base),
        index,
        scale: 1,
        displacement,
        address16: false,
        segment: if matches!(base, Register::Esp | Register::Ebp) {
            Segment::Ss
        } else {
            Segment::Ds
        },
    }
}
fn mov(target: Register, source: Register) -> Operation {
    Operation::Move {
        target,
        source: Source::Register(source),
    }
}
fn alu(target: Register, kind: Alu, source: Source) -> Operation {
    Operation::Alu {
        target,
        kind,
        source,
    }
}
fn imm(target: Register, kind: Alu, source: u32) -> Operation {
    alu(target, kind, Source::Immediate(source))
}
fn load(target: Register, base: Register, displacement: u32) -> Operation {
    Operation::LoadMemory {
        target,
        address: mem(base, None, displacement),
    }
}
fn save(base: Register, displacement: u32, source: Register) -> Operation {
    Operation::StoreMemory {
        address: mem(base, None, displacement),
        width: source.width(),
        source: Source::Register(source),
    }
}
fn cmp_run() -> Operation {
    Operation::CompareMemory {
        test: false,
        address: mem(Register::Ebx, None, 0),
        width: Width::Dword,
        source: Source::Immediate(0),
    }
}
fn cmp_n() -> Operation {
    Operation::AluMemory {
        kind: Alu::Cmp,
        target: Register::Edx,
        address: mem(Register::Esp, None, 4),
    }
}
fn lea(target: Register, base: Register) -> Operation {
    Operation::Lea {
        target,
        address: mem(base, None, 1),
    }
}

/// Relocatable grouped graph and independently executable x86 encoding.
pub fn rle_fixture(id: ProcedureId, space: u32, base: u32) -> RleFixture {
    use Register::*;
    use Source::Register as R;
    // Numeric labels describe graph stages, never executable addresses.
    let stages = vec![
        (
            0,
            vec![
                Operation::Push {
                    width: Width::Dword,
                    source: R(Esi),
                },
                Operation::Push {
                    width: Width::Dword,
                    source: R(Edi),
                },
                Operation::Push {
                    width: Width::Dword,
                    source: R(Ebp),
                },
                imm(Esp, Alu::Sub, 12),
                load(Ebp, Esp, 32),
                save(Esp, 8, Eax),
                save(Esp, 0, Ebx),
                save(Esp, 4, Ecx),
                load(Ebx, Esp, 28),
                load(Edi, Esp, 0),
                alu(Eax, Alu::Xor, R(Eax)),
                alu(Edx, Alu::Xor, R(Edx)),
            ],
            Edge::Next(1),
        ),
        (
            1,
            vec![cmp_run()],
            Edge::Branch(Condition::LessOrEqual, 4, 2),
        ),
        (
            2,
            vec![cmp_n()],
            Edge::Branch(Condition::GreaterOrEqual, 4, 3),
        ),
        (
            3,
            vec![
                mov(Esi, Edx),
                alu(Esi, Alu::Add, R(Edi)),
                load(Cl, Ebp, 0),
                save(Esi, 0, Cl),
                load(Ecx, Ebx, 0),
                Operation::Dec(Ecx),
                Operation::Inc(Edx),
                save(Ebx, 0, Ecx),
            ],
            Edge::Jump(1),
        ),
        (
            4,
            vec![cmp_n()],
            Edge::Branch(Condition::GreaterOrEqual, 10, 5),
        ),
        (
            5,
            vec![
                load(Esi, Esp, 8),
                Operation::LoadMemory {
                    target: Cl,
                    address: mem(Esi, Some(Eax), 0),
                },
                imm(Cl, Alu::And, 0xc0),
                Operation::Extend {
                    target: Edi,
                    source: Cl,
                    signed: false,
                },
                lea(Esi, Eax),
                imm(Edi, Alu::Cmp, 0xc0),
            ],
            Edge::Branch(Condition::NotEqual, 9, 6),
        ),
        (
            6,
            vec![
                load(Edi, Esp, 8),
                alu(Eax, Alu::Add, R(Edi)),
                load(Al, Eax, 0),
                imm(Al, Alu::And, 63),
                imm(Eax, Alu::And, 255),
                save(Ebx, 0, Eax),
                lea(Eax, Esi),
                alu(Esi, Alu::Add, R(Edi)),
                Operation::ExtendMemory {
                    target: Esi,
                    width: Width::Byte,
                    address: mem(Esi, None, 0),
                    signed: false,
                },
                load(Edi, Esp, 4),
                save(Ebp, 0, Esi),
            ],
            Edge::Next(7),
        ),
        (
            7,
            vec![cmp_run()],
            Edge::Branch(Condition::LessOrEqual, 4, 8),
        ),
        (
            8,
            vec![alu(Edx, Alu::Cmp, R(Edi))],
            Edge::Branch(Condition::GreaterOrEqual, 4, 11),
        ),
        (
            11,
            vec![
                load(Ecx, Esp, 0),
                mov(Esi, Edx),
                alu(Esi, Alu::Add, R(Ecx)),
                load(Cl, Ebp, 0),
                save(Esi, 0, Cl),
                load(Esi, Ebx, 0),
                Operation::Dec(Esi),
                Operation::Inc(Edx),
                save(Ebx, 0, Esi),
            ],
            Edge::Jump(7),
        ),
        (
            9,
            vec![
                mov(Edi, Eax),
                mov(Eax, Esi),
                load(Esi, Esp, 8),
                load(Ecx, Esp, 0),
                alu(Esi, Alu::Add, R(Edi)),
                mov(Edi, Edx),
                alu(Edi, Alu::Add, R(Ecx)),
                load(Cl, Esi, 0),
                Operation::Inc(Edx),
                save(Edi, 0, Cl),
            ],
            Edge::Jump(4),
        ),
        (
            10,
            vec![
                imm(Esp, Alu::Add, 12),
                Operation::Pop { target: Ebp },
                Operation::Pop { target: Edi },
                Operation::Pop { target: Esi },
            ],
            Edge::Return,
        ),
    ];
    let at = |offset| Address {
        space,
        offset: base + offset,
    };
    let mut code = Vec::new();
    let mut blocks = Vec::new();
    let mut labels = [0; 12];
    let mut edges = Vec::new();
    for (label, operations, edge) in stages {
        labels[label] = code.len() as u32;
        let entry = at(code.len() as u32);
        let mut instructions = Vec::new();
        for operation in operations {
            instructions.push(ProcedureInstruction {
                address: at(code.len() as u32),
                operation,
            });
            code.extend(encode(operation));
        }
        let term = code.len() as u32;
        match edge {
            Edge::Next(_) => {}
            Edge::Branch(condition, _, _) => {
                code.extend([0x0f, 0x80 + condition as u8, 0, 0, 0, 0])
            }
            Edge::Jump(_) => code.extend([0xe9, 0, 0, 0, 0]),
            Edge::Return => code.extend([0xc2, 8, 0]),
        }
        edges.push(edge);
        blocks.push(ProcedureBlock {
            entry,
            instructions,
            terminator: ProcedureTerminator {
                address: at(term),
                kind: ProcedureTerminatorKind::Return {
                    width: Width::Dword,
                    pop_bytes: 8,
                },
            },
        });
    }
    for (block, edge) in blocks.iter_mut().zip(edges) {
        let offset = (block.terminator.address.offset - base) as usize;
        block.terminator.kind = match edge {
            Edge::Next(target) => ProcedureTerminatorKind::Fallthrough(at(labels[target])),
            Edge::Branch(condition, taken, not_taken) => {
                let displacement = labels[taken].wrapping_sub(offset as u32 + 6);
                code[offset + 2..offset + 6].copy_from_slice(&displacement.to_le_bytes());
                assert_eq!(labels[not_taken], offset as u32 + 6);
                ProcedureTerminatorKind::Branch {
                    condition,
                    taken: at(labels[taken]),
                    not_taken: at(labels[not_taken]),
                }
            }
            Edge::Jump(target) => {
                let displacement = labels[target].wrapping_sub(offset as u32 + 5);
                code[offset + 1..offset + 5].copy_from_slice(&displacement.to_le_bytes());
                ProcedureTerminatorKind::Jump(at(labels[target]))
            }
            Edge::Return => ProcedureTerminatorKind::Return {
                width: Width::Dword,
                pop_bytes: 8,
            },
        };
    }
    RleFixture {
        function: ProcedureGraphFunction {
            id,
            entry: at(0),
            blocks,
        },
        code,
    }
}
#[derive(Clone, Copy)]
enum Edge {
    Next(usize),
    Branch(Condition, usize, usize),
    Jump(usize),
    Return,
}
fn reg(r: Register) -> u8 {
    (r.index() + if r.shift() == 8 { 4 } else { 0 }) as u8
}
fn modrm(bytes: &mut Vec<u8>, r: u8, a: MemoryAddress) {
    // Always use disp32 and an explicit SIB. This is deliberately a synthetic
    // encoding choice, independent of compact encodings in any guest image.
    bytes.push(0x84 | (r << 3));
    bytes.push(a.index.map_or(4, reg) << 3 | reg(a.base.unwrap()));
    bytes.extend(a.displacement.to_le_bytes());
}
fn encode(op: Operation) -> Vec<u8> {
    let mut b = Vec::new();
    match op {
        Operation::Push {
            source: Source::Register(r),
            ..
        } => b.push(0x50 + reg(r)),
        Operation::Pop { target } => b.push(0x58 + reg(target)),
        Operation::Inc(r) => b.push(0x40 + reg(r)),
        Operation::Dec(r) => b.push(0x48 + reg(r)),
        Operation::Move {
            target,
            source: Source::Register(r),
        } => b.extend([0x8b, 0xc0 | (reg(target) << 3) | reg(r)]),
        Operation::LoadMemory { target, address } => {
            b.push(if target.width() == Width::Byte {
                0x8a
            } else {
                0x8b
            });
            modrm(&mut b, reg(target), address);
        }
        Operation::StoreMemory {
            width,
            source: Source::Register(r),
            address,
        } => {
            b.push(if width == Width::Byte { 0x88 } else { 0x89 });
            modrm(&mut b, reg(r), address);
        }
        Operation::Alu {
            target,
            kind,
            source,
        } => {
            let k = match kind {
                Alu::Add => 0,
                Alu::And => 4,
                Alu::Sub => 5,
                Alu::Xor => 6,
                Alu::Cmp => 7,
                _ => panic!("fixture ALU"),
            };
            match source {
                Source::Register(r) => b.extend([k * 8 + 3, 0xc0 | (reg(target) << 3) | reg(r)]),
                Source::Immediate(v) => {
                    b.extend([
                        if target.width() == Width::Byte {
                            0x80
                        } else {
                            0x81
                        },
                        0xc0 | (k << 3) | reg(target),
                    ]);
                    if target.width() == Width::Byte {
                        b.push(v as u8)
                    } else {
                        b.extend(v.to_le_bytes())
                    }
                }
                Source::VirtualRegister(_) => panic!("fixture source"),
            }
        }
        Operation::AluMemory {
            target,
            kind: Alu::Cmp,
            address,
        } => {
            b.push(0x3b);
            modrm(&mut b, reg(target), address)
        }
        Operation::CompareMemory {
            test: false,
            address,
            source: Source::Immediate(v),
            ..
        } => {
            b.push(0x81);
            modrm(&mut b, 7, address);
            b.extend(v.to_le_bytes())
        }
        Operation::Lea { target, address } => {
            b.push(0x8d);
            modrm(&mut b, reg(target), address)
        }
        Operation::Extend {
            target,
            source,
            signed: false,
        } => b.extend([0x0f, 0xb6, 0xc0 | (reg(target) << 3) | reg(source)]),
        Operation::ExtendMemory {
            target,
            address,
            signed: false,
            ..
        } => {
            b.extend([0x0f, 0xb6]);
            modrm(&mut b, reg(target), address)
        }
        _ => panic!("unsupported fixture operation {op:?}"),
    }
    b
}
