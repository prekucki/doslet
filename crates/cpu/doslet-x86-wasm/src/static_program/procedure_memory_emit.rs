//! Whole straight-line procedure; shares all instruction and memory semantics.

use super::*;
use crate::static_program::memory;
use crate::static_program::procedure_memory::{
    COMPLETE, ProcedureCompileError, ProcedureFunction, SIDE_EXIT,
};

pub(super) const PC: u32 = WIDE + memory_emit::PROCEDURE_LOCAL_COUNT + 1;
pub(super) const COMPLETED: u32 = PC + 1;
pub(super) const EXIT_STATUS: u32 = COMPLETED + 1;
pub(super) const VREG_LOCAL_BASE: u32 = EXIT_STATUS + 1;
pub(super) const MAX_VIRTUAL_REGISTERS: usize = 32;

#[derive(Clone, Copy)]
pub(super) struct VirtualRegisters {
    linear: [u32; MAX_VIRTUAL_REGISTERS],
    len: usize,
}

impl VirtualRegisters {
    pub(super) fn collect(
        operations: impl IntoIterator<Item = Operation>,
    ) -> Result<Self, ProcedureCompileError> {
        let mut vregs = Self {
            linear: [0; MAX_VIRTUAL_REGISTERS],
            len: 0,
        };
        for op in operations {
            let mut slots = [None; 2];
            let s = |src: Source| match src {
                Source::VirtualRegister(addr) => Some(addr),
                _ => None,
            };
            let a = |addr: memory::MemoryAddress| {
                (addr.scale & memory::VIRTUAL_DISPLACEMENT != 0).then_some(addr.displacement)
            };
            match op {
                Operation::Move { source, .. }
                | Operation::Alu { source, .. }
                | Operation::WideMultiply { source, .. }
                | Operation::Push { source, .. } => slots[0] = s(source),
                Operation::SignedMultiply { left, right, .. } => {
                    slots = [s(left), s(right)];
                }
                Operation::Lea { address, .. }
                | Operation::LoadMemory { address, .. }
                | Operation::ExtendMemory { address, .. }
                | Operation::WideMultiplyMemory { address, .. }
                | Operation::AluMemory { address, .. }
                | Operation::UnaryMemory { address, .. } => slots[0] = a(address),
                Operation::SignedMultiplyMemory {
                    address, source, ..
                }
                | Operation::StoreMemory {
                    address, source, ..
                }
                | Operation::CompareMemory {
                    address, source, ..
                }
                | Operation::AluMemoryStore {
                    address, source, ..
                } => slots = [a(address), s(source)],
                Operation::X87(instruction) => {
                    slots[0] = instruction.operation.address().and_then(a)
                }
                _ => {}
            }
            for addr in slots.into_iter().flatten() {
                if let Err(pos) = vregs.linear[..vregs.len].binary_search(&addr) {
                    if vregs.len == MAX_VIRTUAL_REGISTERS {
                        return Err(ProcedureCompileError::TooManyOperations);
                    }
                    vregs.linear.copy_within(pos..vregs.len, pos + 1);
                    vregs.linear[pos] = addr;
                    vregs.len += 1;
                }
            }
        }
        Ok(vregs)
    }

    pub(super) fn emit_prologue(&self, f: &mut Function, entry: Address) {
        memory_emit::procedure_virtual_registers(
            f,
            &self.linear[..self.len],
            VREG_LOCAL_BASE,
            |f| refused(f, entry, 0),
        );
    }

    fn local(&self, addr: u32) -> u32 {
        let index = self.linear[..self.len]
            .binary_search(&addr)
            .expect("collected virtual register");
        VREG_LOCAL_BASE + index as u32
    }

    fn bind_source(&self, source: Source) -> Source {
        match source {
            Source::VirtualRegister(addr) => Source::VirtualRegister(self.local(addr)),
            other => other,
        }
    }

    pub(super) fn bind_address(&self, mut address: memory::MemoryAddress) -> memory::MemoryAddress {
        if address.scale & memory::VIRTUAL_DISPLACEMENT != 0 {
            address.displacement = self.local(address.displacement);
        }
        address
    }

    pub(super) fn bind(&self, op: Operation) -> Operation {
        if self.len == 0 {
            return op;
        }
        match op {
            Operation::Move { target, source } => Operation::Move {
                target,
                source: self.bind_source(source),
            },
            Operation::SignedMultiply {
                target,
                left,
                right,
            } => Operation::SignedMultiply {
                target,
                left: self.bind_source(left),
                right: self.bind_source(right),
            },
            Operation::WideMultiply {
                signed,
                width,
                source,
            } => Operation::WideMultiply {
                signed,
                width,
                source: self.bind_source(source),
            },
            Operation::Lea { target, address } => Operation::Lea {
                target,
                address: self.bind_address(address),
            },
            Operation::LoadMemory { target, address } => Operation::LoadMemory {
                target,
                address: self.bind_address(address),
            },
            Operation::ExtendMemory {
                target,
                width,
                address,
                signed,
            } => Operation::ExtendMemory {
                target,
                width,
                address: self.bind_address(address),
                signed,
            },
            Operation::SignedMultiplyMemory {
                target,
                address,
                source,
            } => Operation::SignedMultiplyMemory {
                target,
                address: self.bind_address(address),
                source: self.bind_source(source),
            },
            Operation::WideMultiplyMemory {
                signed,
                width,
                address,
            } => Operation::WideMultiplyMemory {
                signed,
                width,
                address: self.bind_address(address),
            },
            Operation::StoreMemory {
                address,
                width,
                source,
            } => Operation::StoreMemory {
                address: self.bind_address(address),
                width,
                source: self.bind_source(source),
            },
            Operation::Push { width, source } => Operation::Push {
                width,
                source: self.bind_source(source),
            },
            Operation::Alu {
                kind,
                target,
                source,
            } => Operation::Alu {
                kind,
                target,
                source: self.bind_source(source),
            },
            Operation::AluMemory {
                kind,
                target,
                address,
            } => Operation::AluMemory {
                kind,
                target,
                address: self.bind_address(address),
            },
            Operation::CompareMemory {
                test,
                address,
                width,
                source,
            } => Operation::CompareMemory {
                test,
                address: self.bind_address(address),
                width,
                source: self.bind_source(source),
            },
            Operation::AluMemoryStore {
                kind,
                address,
                width,
                source,
            } => Operation::AluMemoryStore {
                kind,
                address: self.bind_address(address),
                width,
                source: self.bind_source(source),
            },
            Operation::UnaryMemory {
                kind,
                address,
                width,
            } => Operation::UnaryMemory {
                kind,
                address: self.bind_address(address),
                width,
            },
            Operation::X87(mut instruction) => {
                if let Some(address) = instruction.operation.address() {
                    instruction.operation = instruction
                        .operation
                        .with_address(self.bind_address(address));
                }
                Operation::X87(instruction)
            }
            other => other,
        }
    }
    pub(super) fn locals_end(&self) -> u32 {
        VREG_LOCAL_BASE + self.len as u32
    }
}

/// The common exit block encloses each preflight's success/failure blocks.
/// A refused operand reaches this code with only the success block remaining.
fn refused(f: &mut Function, address: Address, completed: usize) {
    c(f, address.offset);
    set(f, PC);
    c(f, completed as u32);
    set(f, COMPLETED);
    c(f, SIDE_EXIT);
    set(f, EXIT_STATUS);
    ins(f, W::Br(1));
}

pub(in crate::static_program) fn body(
    procedure: &ProcedureFunction,
    max_output: usize,
) -> Result<Function, ProcedureCompileError> {
    let vregs = VirtualRegisters::collect(
        procedure
            .instructions
            .iter()
            .map(|instruction| instruction.operation),
    )?;
    let mut f = initialize_with_vregs(
        procedure.instructions.iter().any(|instruction| {
            matches!(
                instruction.operation,
                Operation::StoreMemory { .. } | Operation::StringStore { .. }
            )
        }),
        &vregs,
    );
    ins(&mut f, W::Block(BlockType::Empty)); // common exit
    vregs.emit_prologue(&mut f, procedure.entry);
    for (completed, instruction) in procedure.instructions.iter().enumerate() {
        instruction_body(&mut f, &vregs, instruction.operation, |f| {
            refused(f, instruction.address, completed)
        });
        if f.byte_len() > max_output {
            return Err(ProcedureCompileError::OutputTooLarge);
        }
    }
    let terminator = procedure.terminator;
    stack_emit::procedure_preflight(&mut f, terminator.width, false, |f| {
        refused(f, terminator.address, procedure.instructions.len())
    });
    stack_emit::near_return(&mut f, terminator.width, terminator.pop_bytes);
    get(&mut f, RESULT);
    set(&mut f, PC);
    c(&mut f, procedure.instructions.len() as u32 + 1);
    set(&mut f, COMPLETED);
    c(&mut f, COMPLETE);
    set(&mut f, EXIT_STATUS);
    ins(&mut f, W::End);
    finish(&mut f, procedure.entry);
    if f.byte_len() > max_output {
        return Err(ProcedureCompileError::OutputTooLarge);
    }
    Ok(f)
}

pub(super) fn initialize(device_stores: bool) -> Function {
    initialize_with_vregs(
        device_stores,
        &VirtualRegisters {
            linear: [0; MAX_VIRTUAL_REGISTERS],
            len: 0,
        },
    )
}

pub(super) fn initialize_with_vregs(device_stores: bool, vregs: &VirtualRegisters) -> Function {
    initialize_with_x87(device_stores, vregs, false)
}
pub(super) fn initialize_with_x87(
    device_stores: bool,
    vregs: &VirtualRegisters,
    x87: bool,
) -> Function {
    // Match the shared region emitter's local identities and value types.
    // Only FRAME is a parameter. QUOTA/BUDGET are unused padding.
    let mut locals = alloc::vec![
        (10, ValType::I32),
        (2, ValType::I64),
        (5, ValType::I32),
        (1, ValType::I64),
        (
            memory_emit::PROCEDURE_LOCAL_COUNT + 3 + vregs.len as u32,
            ValType::I32,
        ),
    ];
    if x87 {
        super::procedure_x87_emit::append_locals(&mut locals);
    }
    let mut f = Function::new(locals);
    for i in 0..8 {
        load(&mut f, abi::GPR + i * 4, false);
        set(&mut f, GPR + i);
    }
    load(&mut f, abi::EFLAGS, false);
    set(&mut f, FLAGS);
    load(&mut f, abi::CHARGED_WORK, true);
    set(&mut f, WORK);
    load(&mut f, abi::STATUS, false);
    set(&mut f, STATUS);
    if device_stores {
        memory_emit::procedure_initialize(&mut f);
    }
    f
}

pub(super) fn instruction_body(
    f: &mut Function,
    vregs: &VirtualRegisters,
    operation_value: Operation,
    refused: impl Fn(&mut Function),
) {
    let operation_value = vregs.bind(operation_value);
    use memory_emit::Access;
    let access = match operation_value {
        Operation::LoadMemory { target, address }
        | Operation::SignedMultiplyMemory {
            target, address, ..
        }
        | Operation::AluMemory {
            target, address, ..
        } => Some((target.width(), address, Access::RamRead)),
        Operation::CompareMemory { width, address, .. }
        | Operation::ExtendMemory { width, address, .. }
        | Operation::WideMultiplyMemory { width, address, .. } => {
            Some((width, address, Access::RamRead))
        }
        Operation::StoreMemory { width, address, .. } => {
            Some((width, address, Access::ProcedureWrite))
        }
        Operation::StringLoad {
            width,
            address16,
            segment,
        } => Some((
            width,
            memory::MemoryAddress::string(true, address16, segment),
            Access::RamRead,
        )),
        Operation::StringStore { width, address16 } => Some((
            width,
            memory::MemoryAddress::string(false, address16, memory::Segment::Es),
            Access::ProcedureWrite,
        )),
        Operation::AluMemoryStore { width, address, .. }
        | Operation::UnaryMemory { width, address, .. } => {
            Some((width, address, Access::ReadModifyWrite))
        }
        _ => None,
    };
    if let Some((width, address, access)) = access {
        memory_emit::procedure_access(f, width, address, access, &refused);
    }
    match operation_value {
        Operation::Push { width, .. } => stack_emit::procedure_preflight(f, width, true, &refused),
        Operation::Pop { target } => {
            stack_emit::procedure_preflight(f, target.width(), false, &refused)
        }
        _ => {}
    }
    match operation_value {
        Operation::StoreMemory { width, source, .. } => {
            memory_emit::procedure_store_memory(f, width, source);
        }
        Operation::StringStore { width, address16 } => {
            memory_emit::procedure_store_memory(f, width, Source::Register(accumulator(width)));
            string_advance(f, false, width, address16);
        }
        _ => operation(f, operation_value),
    }
}

pub(super) fn finish(f: &mut Function, entry: Address) {
    // Charge precisely the completed prefix once, with the standard sticky
    // saturation contract. The owner reconciles its external reservation.
    get(f, WORK);
    wide(f, u64::MAX);
    get(f, COMPLETED);
    ins(f, W::I64ExtendI32U);
    ins(f, W::I64Sub);
    ins(f, W::I64GtU);
    ins_hinted(f, false, W::If(BlockType::Empty));
    wide(f, u64::MAX);
    set(f, WORK);
    get(f, STATUS);
    c(f, abi::COUNTER_OVERFLOW);
    ins(f, W::I32Or);
    set(f, STATUS);
    ins(f, W::Else);
    get(f, WORK);
    get(f, COMPLETED);
    ins(f, W::I64ExtendI32U);
    ins(f, W::I64Add);
    set(f, WORK);
    ins(f, W::End);
    for i in 0..8 {
        store(f, abi::GPR + i * 4, GPR + i, false);
    }
    store(f, abi::EFLAGS, FLAGS, false);
    store_constant(f, abi::SPACE, entry.space);
    store(f, abi::OFFSET, PC, false);
    store(f, abi::CHARGED_WORK, WORK, true);
    store(f, abi::STATUS, STATUS, false);
    get(f, EXIT_STATUS);
    ins(f, W::End);
}
