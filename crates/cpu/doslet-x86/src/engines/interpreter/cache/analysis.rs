//! Bounded control-flow inventory of loaded flat 32-bit code. Never an idle proof.
//! Instruction lengths and effects come from the execution decoder.
use super::{Instruction, NO_REGISTER, Operation, Target, Value};
use crate::{Access, Registers, SegmentCache, SegmentRegister};
use doslet_bus::{CodeCheck, MemoryBus};

pub const NODES: usize = 64;
pub const CALL_DEPTH: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operand {
    Register(u8),
    Absolute {
        segment: SegmentRegister,
        address: u32,
    },
    Immediate(u32),
    PortDx,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    Move {
        target: Operand,
        source: Operand,
    },
    Compare {
        left: Operand,
        right: Operand,
        test: bool,
    },
    Mask {
        target: Operand,
        mask: u32,
    },
    RegisterWork,
    Write(Operand),
    Input,
    Output,
    Call,
    Return,
    Branch,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Next,
    Branch { condition: Option<u8>, target: u32 },
    Call(u32),
    Return,
    InterruptReturn,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Decoded {
    pub length: u8,
    pub width: u8,
    pub effect: Effect,
    pub flow: Flow,
}
fn operand(t: Target) -> Operand {
    match t {
        Target::Register(r) => Operand::Register(r),
        Target::Memory {
            segment,
            base: NO_REGISTER,
            index: NO_REGISTER,
            displacement,
            address32: true,
            ..
        } => Operand::Absolute {
            segment,
            address: displacement,
        },
        _ => Operand::Unknown,
    }
}
fn value(v: Value) -> Operand {
    match v {
        Value::Immediate(i) => Operand::Immediate(i),
        Value::Operand(t) => operand(t),
    }
}
/// Unsupported forms retain their decoded length and explicit unknown effects.
/// An unsupported instruction length stops analysis rather than resynchronizing bytes.
pub fn decode(bytes: &[u8], ip: u32) -> Option<Decoded> {
    // IRETD has no cached execution form. This is a control-flow marker only;
    // architectural restoration still runs through the reference interpreter.
    if bytes.first() == Some(&0xcf) {
        return Some(Decoded {
            length: 1,
            width: 4,
            effect: Effect::Return,
            flow: Flow::InterruptReturn,
        });
    }
    let i = Instruction::decode_mode(bytes, true)?;
    let next = ip.checked_add(u32::from(i.length))?;
    let mut flow = Flow::Next;
    let effect = match i.operation {
        Operation::Move { target, source } => Effect::Move {
            target: operand(target),
            source: value(source),
        },
        Operation::Alu {
            target,
            source,
            operation,
            store: false,
        } if matches!(operation, 4 | 7) => Effect::Compare {
            left: operand(target),
            right: value(source),
            test: operation == 4,
        },
        Operation::Alu {
            target: Target::Register(reg),
            source: Value::Immediate(mask),
            operation: 4,
            store: true,
        } => Effect::Mask {
            target: Operand::Register(reg),
            mask,
        },
        Operation::Alu {
            target: Target::Register(_),
            source: Value::Immediate(_) | Value::Operand(Target::Register(_)),
            ..
        }
        | Operation::Inc { .. }
        | Operation::Exchange {
            target: Target::Register(_),
            ..
        } => Effect::RegisterWork,
        Operation::Alu {
            target,
            store: true,
            ..
        }
        | Operation::IncTarget { target, .. } => Effect::Write(operand(target)),
        Operation::Branch {
            condition,
            displacement,
        } if i.branch32 => {
            flow = Flow::Branch {
                condition,
                target: next.checked_add_signed(displacement)?,
            };
            Effect::Branch
        }
        Operation::Call(d) if i.branch32 => {
            flow = Flow::Call(next.checked_add_signed(d)?);
            Effect::Call
        }
        Operation::Return(_) if i.branch32 => {
            flow = Flow::Return;
            Effect::Return
        }
        Operation::CallIndirect(_)
        | Operation::JumpIndirect(_)
        | Operation::Branch { .. }
        | Operation::Call(_)
        | Operation::Return(_)
        | Operation::Loop { .. } => {
            flow = Flow::Unknown;
            Effect::Unknown
        }
        Operation::Input => Effect::Input,
        Operation::Output => Effect::Output,
        _ => Effect::Unknown,
    };
    Some(Decoded {
        length: i.length,
        width: i.width,
        effect,
        flow,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Coverage {
    #[default]
    Empty,
    CompleteGraph,
    UnsupportedContext,
    UnavailableCode,
    UnsupportedInstruction,
    UnknownEffect,
    UnknownTarget,
    Capacity,
    CallDepth,
    OverlappingCode,
    Invalidated,
}
impl Coverage {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Empty => "no graph",
            Self::CompleteGraph => {
                "bounded graph decoded; runtime source and episodes still required"
            }
            Self::UnsupportedContext => "requires flat 32-bit segments without paging",
            Self::UnavailableCode => "code unavailable without device reads",
            Self::UnsupportedInstruction => "instruction not supported by shared decoder",
            Self::UnknownEffect => "unmodeled instruction or operand effects",
            Self::UnknownTarget => "indirect or unsupported control transfer",
            Self::Capacity => "graph capacity exhausted",
            Self::CallDepth => "direct call depth exhausted",
            Self::OverlappingCode => "branch enters another instruction",
            Self::Invalidated => "code generation or execution context changed",
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub ip: u32,
    pub decoded: Decoded,
    pub version: CodeCheck,
    pub depth: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Context {
    pub cs: u16,
    pub segments: [SegmentCache; 6],
    pub cr0: u32,
    pub cr3: u32,
}
impl Context {
    pub fn new(r: &Registers, cr0: u32, cr3: u32) -> Option<Self> {
        let segments = SegmentRegister::ALL.map(|s| r.segment_cache(s));
        if !r.code32()
            || cr0 & 0x8000_0000 != 0
            || [
                SegmentRegister::Cs,
                SegmentRegister::Ds,
                SegmentRegister::Ss,
            ]
            .iter()
            .any(|&seg| {
                let s = r.segment_cache(seg);
                !s.usable || s.base != 0 || s.limit != u32::MAX
            })
            || !r.stack32()
        {
            return None;
        }
        Some(Self {
            cs: r.cs,
            segments,
            cr0,
            cr3,
        })
    }
}
/// One instruction per node: a conservative basic-block subdivision. All
/// successors are explicit; return edges need the runtime call stack.
pub struct Graph {
    pub context: Option<Context>,
    pub coverage: Coverage,
    pub nodes: [Option<Node>; NODES],
    pub len: usize,
    pub backedges: u32,
    pub predicates: u32,
    pub unknown_effects: u32,
}
impl Default for Graph {
    fn default() -> Self {
        Self {
            context: None,
            coverage: Coverage::Empty,
            nodes: [None; NODES],
            len: 0,
            backedges: 0,
            predicates: 0,
            unknown_effects: 0,
        }
    }
}
impl Graph {
    fn incomplete(&mut self, reason: Coverage) {
        if self.coverage == Coverage::CompleteGraph {
            self.coverage = reason;
        }
    }
    pub fn valid<B: MemoryBus>(&self, bus: &B, context: Option<Context>) -> bool {
        self.context.is_some()
            && self.context == context
            && self.nodes[..self.len]
                .iter()
                .flatten()
                .all(|n| bus.code_valid(&n.version))
    }
    pub fn analyze<B: MemoryBus>(&mut self, bus: &B, r: &Registers, cr0: u32, cr3: u32) {
        *self = Self::default();
        self.context = Context::new(r, cr0, cr3);
        if self.context.is_none() {
            self.coverage = Coverage::UnsupportedContext;
            return;
        }
        self.coverage = Coverage::CompleteGraph;
        let mut pending = [(0u32, 0u8); NODES];
        pending[0] = (r.ip, 0);
        let mut count = 1;
        while count != 0 {
            count -= 1;
            let (ip, depth) = pending[count];
            if self.nodes[..self.len].iter().flatten().any(|n| n.ip == ip) {
                continue;
            }
            if self.len == NODES {
                self.incomplete(Coverage::Capacity);
                break;
            }
            if self.nodes[..self.len]
                .iter()
                .flatten()
                .any(|n| ip > n.ip && ip < n.ip + u32::from(n.decoded.length))
            {
                self.incomplete(Coverage::OverlappingCode);
                continue;
            }
            // Incremental side-effect-free fetch accepts code ending at a RAM boundary.
            let mut bytes = [0; 15];
            let mut available = 0;
            for length in 1..=15u8 {
                if r.address(SegmentRegister::Cs, ip, length, Access::Execute)
                    .is_err()
                {
                    break;
                }
                let Some(code) = bus.code_bytes(ip, length) else {
                    break;
                };
                if code.len() < usize::from(length) {
                    break;
                }
                bytes[..usize::from(length)].copy_from_slice(&code[..usize::from(length)]);
                available = usize::from(length);
            }
            if available == 0 {
                self.incomplete(Coverage::UnavailableCode);
                continue;
            }
            let Some(d) = decode(&bytes[..available], ip) else {
                self.incomplete(Coverage::UnsupportedInstruction);
                continue;
            };
            let Some(version) = bus.code_check(ip, d.length) else {
                self.incomplete(Coverage::UnavailableCode);
                continue;
            };
            let Some(next) = ip.checked_add(u32::from(d.length)) else {
                self.incomplete(Coverage::UnknownTarget);
                continue;
            };
            if self.nodes[..self.len]
                .iter()
                .flatten()
                .any(|n| n.ip > ip && n.ip < next)
            {
                self.incomplete(Coverage::OverlappingCode);
                continue;
            }
            self.nodes[self.len] = Some(Node {
                ip,
                decoded: d,
                version,
                depth,
            });
            self.len += 1;
            if matches!(d.effect, Effect::Compare { .. }) {
                self.predicates += 1;
            }
            if matches!(
                d.effect,
                Effect::Unknown
                    | Effect::Move {
                        target: Operand::Unknown,
                        ..
                    }
                    | Effect::Move {
                        source: Operand::Unknown,
                        ..
                    }
                    | Effect::Compare {
                        left: Operand::Unknown,
                        ..
                    }
                    | Effect::Compare {
                        right: Operand::Unknown,
                        ..
                    }
                    | Effect::Write(Operand::Unknown)
            ) {
                self.unknown_effects += 1;
                self.incomplete(Coverage::UnknownEffect);
            }
            let mut successors = [None; 2];
            match d.flow {
                Flow::Next => successors[0] = Some((next, depth)),
                Flow::Branch { condition, target } => {
                    self.backedges += u32::from(target <= ip);
                    successors[0] = Some((target, depth));
                    if condition.is_some() {
                        successors[1] = Some((next, depth));
                    }
                }
                Flow::Call(target) => {
                    successors[0] = Some((next, depth));
                    if depth < CALL_DEPTH {
                        successors[1] = Some((target, depth + 1));
                    } else {
                        self.incomplete(Coverage::CallDepth);
                    }
                }
                Flow::Return | Flow::InterruptReturn => {}
                Flow::Unknown => self.incomplete(Coverage::UnknownTarget),
            }
            for successor in successors.into_iter().flatten() {
                if self.nodes[..self.len]
                    .iter()
                    .flatten()
                    .any(|n| n.ip == successor.0)
                    || pending[..count].iter().any(|p| p.0 == successor.0)
                {
                    continue;
                }
                if count == NODES {
                    self.incomplete(Coverage::Capacity);
                    break;
                }
                pending[count] = successor;
                count += 1;
            }
        }
    }
}
