//! Scalar support gate and conservative read-before-write/stack summaries.

use iced_x86::{
    CodeSize, Instruction, InstructionInfoFactory, Mnemonic, OpAccess, OpKind, Register,
    RflagsBits, UsedMemory,
};

use super::{MAX_PROCEDURE_INSTRUCTIONS, ObjectAddress, StackRead};

pub(super) fn near_return(instruction: &Instruction) -> Option<(u8, u16)> {
    if instruction.mnemonic() != Mnemonic::Ret {
        return None;
    }
    let pop = match instruction.op_count() {
        0 => 0,
        1 if instruction.op0_kind() == OpKind::Immediate16 => instruction.immediate16(),
        _ => return None,
    };
    let width = instruction.stack_pointer_increment() - i32::from(pop);
    matches!(width, 2 | 4).then_some((width as u8, pop))
}

fn register_width(register: Register) -> Option<usize> {
    register_bytes(register).map(|(_, mask)| mask.count_ones() as usize)
}

fn scalar_operand(instruction: &Instruction, operand: u32) -> bool {
    match instruction.op_kind(operand) {
        OpKind::Register => register_width(instruction.op_register(operand)).is_some(),
        OpKind::Memory => {
            (instruction.mnemonic() == Mnemonic::Lea
                || matches!(instruction.memory_size().size(), 1 | 2 | 4))
                && [instruction.memory_base(), instruction.memory_index()]
                    .into_iter()
                    .all(|register| {
                        register == Register::None || register.is_gpr16() || register.is_gpr32()
                    })
        }
        OpKind::Immediate8
        | OpKind::Immediate16
        | OpKind::Immediate32
        | OpKind::Immediate8to16
        | OpKind::Immediate8to32 => true,
        _ => false,
    }
}

fn immediate(instruction: &Instruction, operand: u32) -> bool {
    matches!(
        instruction.op_kind(operand),
        OpKind::Immediate8
            | OpKind::Immediate16
            | OpKind::Immediate32
            | OpKind::Immediate8to16
            | OpKind::Immediate8to32
    )
}

fn shift_count(instruction: &Instruction, operand: u32) -> bool {
    instruction.op_kind(operand) == OpKind::Immediate8
        || instruction.op_kind(operand) == OpKind::Register
            && instruction.op_register(operand) == Register::CL
}

/// Matches the initial scalar forms available in the independent WASM lowerer.
/// Acceptance describes analyzable effects, not successful code generation or
/// runtime admission. Unsupported forms remain explicit diagnostics.
pub(super) fn supported(instruction: &Instruction) -> bool {
    use Mnemonic as M;
    let count = instruction.op_count();
    let mnemonic = instruction.mnemonic();
    if matches!(mnemonic, M::Nop | M::Cbw | M::Cwde) {
        return count == 0;
    }
    if mnemonic == M::Ret {
        return near_return(instruction).is_some();
    }
    if !(0..count).all(|operand| scalar_operand(instruction, operand)) {
        return false;
    }
    let reg0 = instruction.op0_kind() == OpKind::Register;
    let reg1 = instruction.op1_kind() == OpKind::Register;
    let mem0 = instruction.op0_kind() == OpKind::Memory;
    let mem1 = instruction.op1_kind() == OpKind::Memory;
    match mnemonic {
        M::Push => {
            count == 1
                && (reg0 || immediate(instruction, 0))
                && matches!(instruction.stack_pointer_increment(), -2 | -4)
        }
        M::Pop => {
            count == 1 && reg0 && matches!(register_width(instruction.op0_register()), Some(2 | 4))
        }
        M::Mov => {
            count == 2
                && (reg0 || mem0)
                && (reg1 || mem1 || immediate(instruction, 1))
                && !(mem0 && mem1)
        }
        M::Movsx | M::Movzx => {
            if count != 2 || !reg0 || !(reg1 || mem1) {
                return false;
            }
            let target = register_width(instruction.op0_register()).unwrap_or(0);
            let source = if reg1 {
                register_width(instruction.op1_register()).unwrap_or(0)
            } else {
                instruction.memory_size().size()
            };
            matches!(target, 2 | 4) && matches!(source, 1 | 2) && target >= source
        }
        M::Lea => {
            count == 2
                && reg0
                && mem1
                && matches!(register_width(instruction.op0_register()), Some(2 | 4))
        }
        M::Add | M::Adc | M::Sub | M::Sbb | M::And | M::Or | M::Xor | M::Cmp | M::Test => {
            count == 2
                && (reg0 || mem0)
                && (reg1 || mem1 || immediate(instruction, 1))
                && !(mem0 && mem1)
        }
        M::Inc | M::Dec | M::Neg | M::Not => count == 1 && (reg0 || mem0),
        M::Xchg => count == 2 && reg0 && reg1,
        M::Mul | M::Imul if count == 1 => reg0 || mem0,
        M::Imul => {
            matches!(count, 2 | 3)
                && reg0
                && (reg1 || mem1)
                && matches!(register_width(instruction.op0_register()), Some(2 | 4))
                && (count == 2 || immediate(instruction, 2))
        }
        M::Shl | M::Sal | M::Shr | M::Sar | M::Rol | M::Ror | M::Rcl | M::Rcr => {
            count == 2 && reg0 && shift_count(instruction, 1)
        }
        M::Shld | M::Shrd => {
            if count != 3 || !reg0 || !reg1 || !shift_count(instruction, 2) {
                return false;
            }
            let width = register_width(instruction.op0_register()).unwrap_or(0);
            matches!(width, 2 | 4)
                && register_width(instruction.op1_register()) == Some(width)
                && (width != 2
                    || instruction.op2_kind() == OpKind::Immediate8
                        && instruction.immediate8() & 31 <= 16)
        }
        _ => false,
    }
}

fn read(access: OpAccess) -> bool {
    matches!(
        access,
        OpAccess::Read | OpAccess::CondRead | OpAccess::ReadWrite | OpAccess::ReadCondWrite
    )
}
fn write(access: OpAccess) -> bool {
    matches!(
        access,
        OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite
    )
}
fn definite_write(access: OpAccess) -> bool {
    matches!(access, OpAccess::Write | OpAccess::ReadWrite)
}

fn register_bytes(register: Register) -> Option<(usize, u8)> {
    use Register as R;
    Some(match register {
        R::EAX => (0, 15),
        R::ECX => (1, 15),
        R::EDX => (2, 15),
        R::EBX => (3, 15),
        R::ESP => (4, 15),
        R::EBP => (5, 15),
        R::ESI => (6, 15),
        R::EDI => (7, 15),
        R::AX => (0, 3),
        R::CX => (1, 3),
        R::DX => (2, 3),
        R::BX => (3, 3),
        R::SP => (4, 3),
        R::BP => (5, 3),
        R::SI => (6, 3),
        R::DI => (7, 3),
        R::AL => (0, 1),
        R::CL => (1, 1),
        R::DL => (2, 1),
        R::BL => (3, 1),
        R::AH => (0, 2),
        R::CH => (1, 2),
        R::DH => (2, 2),
        R::BH => (3, 2),
        _ => return None,
    })
}

fn eflags(iced: u32) -> u32 {
    [
        (RflagsBits::CF, 0),
        (RflagsBits::PF, 2),
        (RflagsBits::AF, 4),
        (RflagsBits::ZF, 6),
        (RflagsBits::SF, 7),
        (RflagsBits::IF, 9),
        (RflagsBits::DF, 10),
        (RflagsBits::OF, 11),
        (RflagsBits::AC, 18),
        (RflagsBits::UIF, 25),
    ]
    .into_iter()
    .fold(0, |result, (mask, bit)| {
        result | if iced & mask != 0 { 1 << bit } else { 0 }
    })
}

pub(super) struct Effects {
    pub inputs: [u8; 8],
    pub modified: [u8; 8],
    pub defined: [u8; 8],
    pub input_flags: u32,
    pub modified_flags: u32,
    pub defined_flags: u32,
    pub reads: u16,
    pub writes: u16,
    pub stack_reads_uncertain: bool,
    pub stack_reads: [StackRead; MAX_PROCEDURE_INSTRUCTIONS as usize],
    pub stack_read_count: usize,
    affine: [Option<i32>; 8],
}

impl Effects {
    pub fn new() -> Self {
        let mut affine = [None; 8];
        affine[4] = Some(0);
        Self {
            inputs: [0; 8],
            modified: [0; 8],
            defined: [0; 8],
            input_flags: 0,
            modified_flags: 0,
            defined_flags: 0,
            reads: 0,
            writes: 0,
            stack_reads_uncertain: false,
            stack_reads: [StackRead {
                instruction: ObjectAddress {
                    object_number: 0,
                    offset: 0,
                },
                offset: 0,
                byte_len: 0,
            }; MAX_PROCEDURE_INSTRUCTIONS as usize],
            stack_read_count: 0,
            affine,
        }
    }

    pub fn add(
        &mut self,
        instruction: &Instruction,
        address: ObjectAddress,
        factory: &mut InstructionInfoFactory,
    ) {
        let shift = matches!(
            instruction.mnemonic(),
            Mnemonic::Shl
                | Mnemonic::Sal
                | Mnemonic::Shr
                | Mnemonic::Sar
                | Mnemonic::Rol
                | Mnemonic::Ror
                | Mnemonic::Rcl
                | Mnemonic::Rcr
                | Mnemonic::Shld
                | Mnemonic::Shrd
        );
        let variable_shift = shift
            && (0..instruction.op_count()).any(|operand| {
                instruction.op_kind(operand) == OpKind::Register
                    && instruction.op_register(operand) == Register::CL
            });
        let zero_shift = shift
            && (0..instruction.op_count()).any(|operand| {
                instruction.op_kind(operand) == OpKind::Immediate8
                    && instruction.immediate8() & 31 == 0
            });
        let info = factory.info(instruction);
        debug_assert!(info.used_registers().len() <= 10 && info.used_memory().len() <= 8);
        let mut read_bytes = [0; 8];
        let mut write_bytes = [0; 8];
        let mut definite = [0; 8];
        for used in info.used_registers() {
            if let Some((index, mask)) = register_bytes(used.register()) {
                if read(used.access()) {
                    read_bytes[index] |= mask;
                }
                if write(used.access()) {
                    write_bytes[index] |= mask;
                }
                if definite_write(used.access()) && !variable_shift && !zero_shift {
                    definite[index] |= mask;
                }
            }
        }
        // Code32 does not establish live SS.B. An implicit stack increment
        // definitely updates SP, but can preserve ESP's upper word on a
        // 16-bit stack. An explicit POP ESP still assigns the full register.
        if matches!(
            instruction.mnemonic(),
            Mnemonic::Push | Mnemonic::Pop | Mnemonic::Ret | Mnemonic::Call
        ) && !(instruction.mnemonic() == Mnemonic::Pop
            && instruction.op0_register() == Register::ESP)
        {
            definite[4] &= 3;
        }
        // Snapshot every read before applying any writes: XCHG and aliased
        // implicit MUL sources must not depend on info-table record order.
        for index in 0..8 {
            self.inputs[index] |= read_bytes[index] & !self.defined[index];
            self.modified[index] |= write_bytes[index];
            self.defined[index] |= definite[index];
        }
        // Undefined flags are retained by the reference CPU, and variable
        // shifts can preserve every flag for count zero. Neither kills inputs.
        let retained = instruction.rflags_undefined()
            | if variable_shift {
                instruction.rflags_modified()
            } else {
                0
            };
        self.input_flags |= eflags(instruction.rflags_read() | retained) & !self.defined_flags;
        self.modified_flags |= eflags(instruction.rflags_modified());
        if !variable_shift && !zero_shift {
            self.defined_flags |= eflags(
                instruction.rflags_written()
                    | instruction.rflags_cleared()
                    | instruction.rflags_set(),
            );
        }
        let mut has_read = false;
        let mut has_write = false;
        for memory in info.used_memory() {
            has_read |= read(memory.access());
            has_write |= write(memory.access());
            if read(memory.access()) {
                if let Some(offset) = self.stack_offset(memory) {
                    // Every admitted instruction has at most one read location.
                    self.stack_reads[self.stack_read_count] = StackRead {
                        instruction: address,
                        offset,
                        byte_len: memory.memory_size().size() as u8,
                    };
                    self.stack_read_count += 1;
                } else {
                    self.stack_reads_uncertain = true;
                }
            }
        }
        self.reads += u16::from(has_read);
        self.writes += u16::from(has_write);
        self.update_affine(instruction, write_bytes);
    }

    /// A local body omits the callee's execution. No stack-relative hint after
    /// this point can rely on the callee preserving ESP or any saved alias.
    pub fn invalidate_call_stack_hints(&mut self) {
        self.affine = [None; 8];
        self.stack_reads_uncertain = true;
    }

    /// Reuse the bounded instruction-info/stack scratch when collecting one
    /// instruction's access masks for ordered call-effect composition.
    pub fn clear_access_summary(&mut self) {
        self.inputs = [0; 8];
        self.modified = [0; 8];
        self.defined = [0; 8];
        self.input_flags = 0;
        self.modified_flags = 0;
        self.defined_flags = 0;
        self.reads = 0;
        self.writes = 0;
    }

    fn stack_offset(&self, memory: &UsedMemory) -> Option<i32> {
        if memory.segment() != Register::SS
            || memory.index() != Register::None
            || memory.address_size() != CodeSize::Code32
        {
            return None;
        }
        let (base, mask) = register_bytes(memory.base())?;
        if mask != 15 {
            return None;
        }
        self.affine[base]?.checked_add(memory.displacement() as u32 as i32)
    }

    fn update_affine(&mut self, instruction: &Instruction, written: [u8; 8]) {
        let old = self.affine;
        for (index, mask) in written.into_iter().enumerate() {
            if mask != 0 {
                self.affine[index] = None;
            }
        }
        use Mnemonic as M;
        if matches!(instruction.mnemonic(), M::Push | M::Pop | M::Ret) {
            let pop_esp = instruction.mnemonic() == M::Pop
                && matches!(instruction.op0_register(), Register::SP | Register::ESP);
            if !pop_esp {
                self.affine[4] = old[4]
                    .and_then(|value| value.checked_add(instruction.stack_pointer_increment()));
            }
            return;
        }
        if instruction.op0_kind() != OpKind::Register {
            return;
        }
        let Some((target, 15)) = register_bytes(instruction.op0_register()) else {
            return;
        };
        match instruction.mnemonic() {
            M::Mov if instruction.op1_kind() == OpKind::Register => {
                if let Some((source, 15)) = register_bytes(instruction.op1_register()) {
                    self.affine[target] = old[source];
                }
            }
            M::Lea if instruction.memory_index() == Register::None => {
                if let Some((base, 15)) = register_bytes(instruction.memory_base()) {
                    self.affine[target] = old[base].and_then(|value| {
                        value.checked_add(instruction.memory_displacement32() as i32)
                    });
                }
            }
            M::Add | M::Sub if immediate(instruction, 1) => {
                let delta = instruction.immediate(1) as u32 as i32;
                self.affine[target] = old[target].and_then(|value| {
                    if instruction.mnemonic() == M::Add {
                        value.checked_add(delta)
                    } else {
                        value.checked_sub(delta)
                    }
                });
            }
            M::Inc => self.affine[target] = old[target].and_then(|value| value.checked_add(1)),
            M::Dec => self.affine[target] = old[target].and_then(|value| value.checked_sub(1)),
            _ => (),
        }
    }
}
