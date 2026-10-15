//! Native compiler adapter. No bus, device or filesystem access.
//! Compilation allocates explicitly; executing a compiled block does not.
#![cfg(all(
    not(target_arch = "wasm32"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]

use cranelift_codegen::ir::{
    AbiParam, BlockArg, InstBuilder, MemFlags, UserFuncName, Value, condcodes::IntCC, types,
};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_jit::{JITBuilder, JITModule};
use cranelift_module::{Module, default_libcall_names};

mod trace;
pub use trace::*;

pub const MAX_INSTRUCTIONS: usize = 16;
/// Maximum functions owned by one compiler. The CPU uses smaller LRU banks.
pub const MAX_COMPILED_BLOCKS: usize = 4096;

#[derive(Clone, Copy, Debug)]
pub enum Source {
    Register(u8),
    Immediate(u32),
}
#[derive(Clone, Copy, Debug)]
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
    /// Last instruction only: a relative jump back to the start of this block.
    /// The CPU decoder verifies the displacement before constructing this IR.
    LoopBranch {
        condition: Option<u8>,
    },
}
#[derive(Clone, Copy, Debug)]
pub struct Instruction {
    pub length: u8,
    pub operation: Operation,
}

#[derive(Clone, Copy, Debug)]
pub struct CompiledBlock(usize);
type Entry = unsafe extern "C" fn(*mut u32, *mut u32, *mut u32, u32) -> u32;

/// Owns all executable storage and keeps function pointers private.
pub struct Compiler {
    module: Option<JITModule>,
    functions: Vec<Entry>,
    traces: Vec<trace::TraceEntry>,
    capacity: usize,
}
impl Compiler {
    pub fn new() -> Result<Self, String> {
        Self::with_capacity(MAX_COMPILED_BLOCKS)
    }
    /// Bound one independently reclaimable executable-memory bank.
    pub fn with_capacity(capacity: usize) -> Result<Self, String> {
        if !(1..=MAX_COMPILED_BLOCKS).contains(&capacity) {
            return Err("invalid JIT compilation capacity".into());
        }
        let builder = JITBuilder::with_flags(&[("opt_level", "speed")], default_libcall_names())
            .map_err(|e| e.to_string())?;
        Ok(Self {
            module: Some(JITModule::new(builder)),
            functions: Vec::with_capacity(capacity),
            traces: Vec::new(),
            capacity,
        })
    }
    pub fn remaining_capacity(&self) -> usize {
        self.capacity - self.functions.len() - self.traces.len()
    }
    /// Validates the complete IR before emitting code. The only accessible memory
    /// is eight borrowed general registers plus separate EFLAGS and EIP words.
    pub fn compile(&mut self, instructions: &[Instruction]) -> Result<CompiledBlock, String> {
        if instructions.is_empty() || instructions.len() > MAX_INSTRUCTIONS {
            return Err("invalid JIT block length".into());
        }
        for (index, instruction) in instructions.iter().enumerate() {
            if !(1..=15).contains(&instruction.length) || !valid(instruction.operation) {
                return Err("invalid JIT instruction".into());
            }
            if matches!(instruction.operation, Operation::LoopBranch { .. })
                && (index == 0 || index + 1 != instructions.len())
            {
                return Err("JIT loop branch must follow a body and terminate the block".into());
            }
        }
        if self.remaining_capacity() == 0 {
            return Err("JIT compilation capacity exhausted".into());
        }
        let loop_condition = match instructions.last().unwrap().operation {
            Operation::LoopBranch { condition } => Some(condition),
            _ => None,
        };
        // Arithmetic flags are observable at block exit and by ADC/SBB. Earlier
        // overwritten flags need no host instructions, including parity/overflow.
        // Loops conservatively retain every arithmetic flag at every operation:
        // the next iteration can consume carry from the previous iteration.
        let mut needed_flags = [0; MAX_INSTRUCTIONS];
        let mut live = 0x8D5;
        for (index, instruction) in instructions.iter().enumerate().rev() {
            let written = match instruction.operation {
                Operation::Alu { .. } | Operation::Unary { negate: true, .. } => 0x8D5,
                Operation::Inc { .. } => 0x8D4,
                _ => 0,
            };
            needed_flags[index] = if loop_condition.is_some() {
                written
            } else {
                live & written
            };
            live &= !written;
            if matches!(
                instruction.operation,
                Operation::Alu {
                    operation: 2 | 3,
                    ..
                }
            ) {
                live |= 1;
            }
        }
        let module = self.module.as_mut().expect("compiler owns its module");
        let mut ctx = module.make_context();
        for _ in 0..3 {
            ctx.func
                .signature
                .params
                .push(AbiParam::new(module.target_config().pointer_type()));
        }
        ctx.func.signature.params.push(AbiParam::new(types::I32));
        ctx.func.signature.returns.push(AbiParam::new(types::I32));
        let id = module
            .declare_anonymous_function(&ctx.func.signature)
            .map_err(|e| e.to_string())?;
        ctx.func.name = UserFuncName::user(0, id.as_u32());
        let mut frontend = FunctionBuilderContext::new();
        {
            let mut b = FunctionBuilder::new(&mut ctx.func, &mut frontend);
            let entry = b.create_block();
            b.switch_to_block(entry);
            b.append_block_params_for_function_params(entry);
            let state = b.block_params(entry)[0];
            let flags_ptr = b.block_params(entry)[1];
            let ip_ptr = b.block_params(entry)[2];
            let budget = b.block_params(entry)[3];
            let count = instructions.len() as i64;
            let start = b.create_block();
            let insufficient = b.create_block();
            let enough = b
                .ins()
                .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, budget, count);
            b.ins().brif(enough, start, &[], insufficient, &[]);
            b.switch_to_block(insufficient);
            let zero = b.ins().iconst(types::I32, 0);
            b.ins().return_(&[zero]);
            b.switch_to_block(start);
            let mut registers: [Value; 8] = core::array::from_fn(|i| {
                b.ins()
                    .load(types::I32, MemFlags::trusted(), state, (i * 4) as i32)
            });
            let original_registers = registers;
            let mut flags = b.ins().load(types::I32, MemFlags::trusted(), flags_ptr, 0);
            let original_flags = flags;
            let ip = b.ins().load(types::I32, MemFlags::trusted(), ip_ptr, 0);
            let loop_header = loop_condition.map(|_| {
                let header = b.create_block();
                for _ in 0..10 {
                    b.append_block_param(header, types::I32);
                }
                let arguments = loop_arguments(&registers, flags, budget);
                b.ins().jump(header, &arguments);
                b.switch_to_block(header);
                registers = core::array::from_fn(|i| b.block_params(header)[i]);
                flags = b.block_params(header)[8];
                header
            });
            let mut dirty = [false; 8];
            let mut length = 0;
            for (index, instruction) in instructions.iter().enumerate() {
                length += i64::from(instruction.length);
                match instruction.operation {
                    Operation::Move { target, source } => {
                        registers[target as usize] = value(&mut b, &registers, source);
                        dirty[target as usize] = true;
                    }
                    Operation::Alu {
                        target,
                        source,
                        operation,
                        store,
                    } => {
                        let right = value(&mut b, &registers, source);
                        let (result, next_flags) = alu(
                            &mut b,
                            flags,
                            registers[target as usize],
                            right,
                            operation,
                            needed_flags[index],
                            false,
                        );
                        flags = next_flags;
                        if store {
                            registers[target as usize] = result;
                            dirty[target as usize] = true;
                        }
                    }
                    Operation::Inc {
                        register,
                        decrement,
                    } => {
                        let one = b.ins().iconst(types::I32, 1);
                        let (result, next_flags) = alu(
                            &mut b,
                            flags,
                            registers[register as usize],
                            one,
                            if decrement { 5 } else { 0 },
                            needed_flags[index],
                            true,
                        );
                        flags = next_flags;
                        registers[register as usize] = result;
                        dirty[register as usize] = true;
                    }
                    Operation::Exchange { left, right } => {
                        registers.swap(left as usize, right as usize);
                        dirty[left as usize] = true;
                        dirty[right as usize] = true;
                    }
                    Operation::Unary { register, negate } => {
                        let old = registers[register as usize];
                        registers[register as usize] = if negate {
                            let zero = b.ins().iconst(types::I32, 0);
                            let (result, next_flags) =
                                alu(&mut b, flags, zero, old, 5, needed_flags[index], false);
                            flags = next_flags;
                            result
                        } else {
                            b.ins().bnot(old)
                        };
                        dirty[register as usize] = true;
                    }
                    Operation::LoopBranch { .. } => {}
                }
            }
            let (completed, next_ip) = if let Some(condition) = loop_condition {
                let header = loop_header.unwrap();
                let remaining = b.block_params(header)[9];
                let remaining = b.ins().iadd_imm(remaining, -count);
                let taken = branch_condition(&mut b, flags, condition);
                let enough = b
                    .ins()
                    .icmp_imm(IntCC::UnsignedGreaterThanOrEqual, remaining, count);
                let repeat = b.ins().band(taken, enough);
                let exit = b.create_block();
                let arguments = loop_arguments(&registers, flags, remaining);
                b.ins().brif(repeat, header, &arguments, exit, &[]);
                b.switch_to_block(exit);
                let end_ip = b.ins().iadd_imm(ip, length);
                let next_ip = b.ins().select(taken, ip, end_ip);
                (b.ins().isub(budget, remaining), next_ip)
            } else {
                (
                    b.ins().iconst(types::I32, count),
                    b.ins().iadd_imm(ip, length),
                )
            };
            for (i, register) in registers.into_iter().enumerate() {
                if dirty[i] && (loop_header.is_some() || register != original_registers[i]) {
                    b.ins()
                        .store(MemFlags::trusted(), register, state, (i * 4) as i32);
                }
            }
            if flags != original_flags {
                b.ins().store(MemFlags::trusted(), flags, flags_ptr, 0);
            }
            b.ins().store(MemFlags::trusted(), next_ip, ip_ptr, 0);
            b.ins().return_(&[completed]);
            b.seal_all_blocks();
            b.finalize();
        }
        module
            .define_function(id, &mut ctx)
            .map_err(|e| e.to_string())?;
        module.finalize_definitions().map_err(|e| e.to_string())?;
        let code = module.get_finalized_function(id);
        let block = CompiledBlock(self.functions.len());
        self.functions.push(entry_pointer(code));
        Ok(block)
    }
    pub fn execute(
        &self,
        block: CompiledBlock,
        registers: &mut [u32; 8],
        flags: &mut u32,
        ip: &mut u32,
        budget: u32,
    ) -> u32 {
        invoke(self.functions[block.0], registers, flags, ip, budget)
    }
}

fn valid(operation: Operation) -> bool {
    let source_valid = |source| match source {
        Source::Register(r) => r < 8,
        Source::Immediate(_) => true,
    };
    match operation {
        Operation::Move { target, source } => target < 8 && source_valid(source),
        Operation::Alu {
            target,
            source,
            operation,
            ..
        } => target < 8 && source_valid(source) && operation < 8,
        Operation::Inc { register, .. } | Operation::Unary { register, .. } => register < 8,
        Operation::Exchange { left, right } => left < 8 && right < 8,
        Operation::LoopBranch { condition } => condition.is_none_or(|condition| condition < 16),
    }
}
fn loop_arguments(registers: &[Value; 8], flags: Value, remaining: Value) -> [BlockArg; 10] {
    core::array::from_fn(|i| {
        match i {
            0..=7 => registers[i],
            8 => flags,
            _ => remaining,
        }
        .into()
    })
}

fn branch_condition(b: &mut FunctionBuilder<'_>, flags: Value, condition: Option<u8>) -> Value {
    let Some(condition) = condition else {
        return b.ins().iconst(types::I8, 1);
    };
    let masked = |b: &mut FunctionBuilder<'_>, mask| b.ins().band_imm(flags, mask);
    let value = match condition >> 1 {
        0 => masked(b, 0x800),
        1 => masked(b, 1),
        2 => masked(b, 0x40),
        3 => masked(b, 0x41),
        4 => masked(b, 0x80),
        5 => masked(b, 4),
        6 | 7 => {
            let overflow = b.ins().ushr_imm(flags, 4);
            let sign_differs = b.ins().bxor(flags, overflow);
            let sign_differs = b.ins().band_imm(sign_differs, 0x80);
            if condition >> 1 == 7 {
                let zero = masked(b, 0x40);
                b.ins().bor(sign_differs, zero)
            } else {
                sign_differs
            }
        }
        _ => unreachable!("validated condition"),
    };
    b.ins().icmp_imm(
        if condition & 1 == 0 {
            IntCC::NotEqual
        } else {
            IntCC::Equal
        },
        value,
        0,
    )
}
fn value(b: &mut FunctionBuilder<'_>, registers: &[Value; 8], source: Source) -> Value {
    match source {
        Source::Register(r) => registers[r as usize],
        Source::Immediate(i) => b.ins().iconst(types::I32, i64::from(i)),
    }
}
fn flag(b: &mut FunctionBuilder<'_>, condition: Value, shift: i64) -> Value {
    let value = b.ins().uextend(types::I32, condition);
    b.ins().ishl_imm(value, shift)
}
fn alu(
    b: &mut FunctionBuilder<'_>,
    previous: Value,
    left: Value,
    right: Value,
    operation: u8,
    needed: u32,
    preserve_carry: bool,
) -> (Value, Value) {
    let subtract = matches!(operation, 3 | 5 | 7);
    let logical = matches!(operation, 1 | 4 | 6);
    let carry = if matches!(operation, 2 | 3) {
        b.ins().band_imm(previous, 1)
    } else {
        b.ins().iconst(types::I32, 0)
    };
    let rhs = b.ins().iadd(right, carry);
    let result = match operation {
        1 => b.ins().bor(left, right),
        4 => b.ins().band(left, right),
        6 => b.ins().bxor(left, right),
        _ if subtract => b.ins().isub(left, rhs),
        _ => b.ins().iadd(left, rhs),
    };
    let written = if preserve_carry { 0x8D4_i64 } else { 0x8D5_i64 };
    let retained = b.ins().band_imm(previous, !written);
    let mut flags = b.ins().bor_imm(retained, 2);
    if needed & 0x40 != 0 {
        let zero = b.ins().icmp_imm(IntCC::Equal, result, 0);
        let zero = flag(b, zero, 6);
        flags = b.ins().bor(flags, zero);
    }
    if needed & 0x80 != 0 {
        let sign = b.ins().ushr_imm(result, 24);
        let sign = b.ins().band_imm(sign, 0x80);
        flags = b.ins().bor(flags, sign);
    }
    if needed & 4 != 0 {
        let low = b.ins().band_imm(result, 255);
        let parity = b.ins().popcnt(low);
        let parity = b.ins().band_imm(parity, 1);
        let parity = b.ins().bxor_imm(parity, 1);
        let parity = b.ins().ishl_imm(parity, 2);
        flags = b.ins().bor(flags, parity);
    }
    if !logical && !preserve_carry && needed & 1 != 0 {
        let l = b.ins().uextend(types::I64, left);
        let r = b.ins().uextend(types::I64, right);
        let carry = b.ins().uextend(types::I64, carry);
        let rhs = b.ins().iadd(r, carry);
        let carry = if subtract {
            b.ins().icmp(IntCC::UnsignedLessThan, l, rhs)
        } else {
            let wide = b.ins().iadd(l, rhs);
            b.ins()
                .icmp_imm(IntCC::UnsignedGreaterThan, wide, i64::from(u32::MAX))
        };
        let carry = b.ins().uextend(types::I32, carry);
        flags = b.ins().bor(flags, carry);
    }
    if !logical && needed & 0x10 != 0 {
        let lr = b.ins().bxor(left, right);
        let auxiliary = b.ins().bxor(lr, result);
        let auxiliary = b.ins().band_imm(auxiliary, 0x10);
        flags = b.ins().bor(flags, auxiliary);
    }
    if !logical && needed & 0x800 != 0 {
        let lr = b.ins().bxor(left, right);
        let lr = if subtract { lr } else { b.ins().bnot(lr) };
        let changed = b.ins().bxor(left, result);
        let overflow = b.ins().band(lr, changed);
        let overflow = b.ins().ushr_imm(overflow, 20);
        let overflow = b.ins().band_imm(overflow, 0x800);
        flags = b.ins().bor(flags, overflow);
    }
    (result, flags)
}

// SAFETY: Only Cranelift functions with the signature emitted above enter this
// boundary. Their loads/stores are fixed to the borrowed registers/flags/IP, no guest bytes
// become host addresses, and executable storage outlives every private pointer.
#[allow(unsafe_code)]
fn entry_pointer(code: *const u8) -> Entry {
    unsafe { core::mem::transmute::<*const u8, Entry>(code) }
}
#[allow(unsafe_code)]
fn invoke(
    entry: Entry,
    registers: &mut [u32; 8],
    flags: &mut u32,
    ip: &mut u32,
    budget: u32,
) -> u32 {
    unsafe { entry(registers.as_mut_ptr(), flags, ip, budget) }
}
impl Drop for Compiler {
    #[allow(unsafe_code)]
    fn drop(&mut self) {
        // No function pointer escapes Compiler; no invocation can overlap Drop.
        if let Some(module) = self.module.take() {
            unsafe { module.free_memory() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bank_capacity_is_shared_by_register_and_trace_functions() {
        assert!(Compiler::with_capacity(0).is_err());
        assert!(Compiler::with_capacity(MAX_COMPILED_BLOCKS + 1).is_err());
        for profiled in [false, true] {
            let mut compiler = Compiler::with_capacity(2).unwrap();
            let instruction = Instruction {
                length: 1,
                operation: Operation::Inc {
                    register: 0,
                    decrement: false,
                },
            };
            let register = compiler.compile(&[instruction]).unwrap();
            assert_eq!(compiler.remaining_capacity(), 1);
            let instruction = TraceInstruction {
                offset: 0,
                length: 1,
                operation: TraceOperation::Inc {
                    register: 0,
                    decrement: true,
                },
            };
            let trace = if profiled {
                compiler.compile_trace_profiled(&[instruction])
            } else {
                compiler.compile_trace(&[instruction])
            }
            .unwrap();
            assert_eq!(compiler.remaining_capacity(), 0);
            assert!(compiler.compile_trace(&[instruction]).is_err());
            assert!(
                compiler
                    .compile(&[Instruction {
                        length: 1,
                        operation: Operation::Exchange { left: 0, right: 1 }
                    }])
                    .is_err()
            );
            let mut registers = [0; 8];
            let mut flags = 0x202;
            let mut ip = 0;
            assert_eq!(
                compiler.execute(register, &mut registers, &mut flags, &mut ip, 1),
                1
            );
            assert_eq!(registers[0], 1);
            assert_eq!(
                compiler.execute_trace(trace, &mut registers, &mut flags, &mut ip, 1, 0, &[]),
                1
            );
            assert_eq!(registers[0], 0);
            assert_eq!(ip, 2);
        }
    }

    #[test]
    fn compiler_rejects_invalid_ir_before_codegen_and_keeps_stores_in_the_state() {
        let mut compiler = Compiler::new().unwrap();
        let valid = Instruction {
            length: 5,
            operation: Operation::Move {
                target: 7,
                source: Source::Immediate(42),
            },
        };
        assert!(compiler.compile(&[]).is_err());
        assert!(compiler.compile(&[valid; MAX_INSTRUCTIONS + 1]).is_err());
        for invalid in [
            Instruction { length: 0, ..valid },
            Instruction {
                length: 16,
                ..valid
            },
            Instruction {
                operation: Operation::Move {
                    target: 8,
                    source: Source::Immediate(1),
                },
                ..valid
            },
            Instruction {
                operation: Operation::Move {
                    target: 0,
                    source: Source::Register(8),
                },
                ..valid
            },
            Instruction {
                operation: Operation::Alu {
                    target: 0,
                    source: Source::Immediate(1),
                    operation: 8,
                    store: true,
                },
                ..valid
            },
            Instruction {
                operation: Operation::LoopBranch {
                    condition: Some(16),
                },
                ..valid
            },
        ] {
            assert!(compiler.compile(&[invalid]).is_err());
        }
        let branch = Instruction {
            length: 2,
            operation: Operation::LoopBranch { condition: None },
        };
        assert!(compiler.compile(&[branch]).is_err());
        assert!(compiler.compile(&[valid, branch, valid]).is_err());
        let code = compiler.compile(&[valid; MAX_INSTRUCTIONS]).unwrap();
        #[repr(C)]
        struct Guarded {
            before: [u32; 4],
            state: [u32; 10],
            after: [u32; 4],
        }
        let mut guarded = Guarded {
            before: [0xCAFE; 4],
            state: [0x1234; 10],
            after: [0xBEEF; 4],
        };
        let (registers, tail) = guarded.state.split_at_mut(8);
        let (flags, ip) = tail.split_at_mut(1);
        let registers: &mut [u32; 8] = registers.try_into().unwrap();
        assert_eq!(
            compiler.execute(code, registers, &mut flags[0], &mut ip[0], 15),
            0
        );
        assert_eq!(*registers, [0x1234; 8]);
        assert_eq!(flags[0], 0x1234);
        assert_eq!(ip[0], 0x1234);
        assert_eq!(
            compiler.execute(
                code,
                registers,
                &mut flags[0],
                &mut ip[0],
                MAX_INSTRUCTIONS as u32,
            ),
            MAX_INSTRUCTIONS as u32
        );
        assert_eq!(guarded.before, [0xCAFE; 4]);
        assert_eq!(guarded.after, [0xBEEF; 4]);
        assert_eq!(&guarded.state[..7], &[0x1234; 7]);
        assert_eq!(guarded.state[7], 42);
        assert_eq!(guarded.state[8], 0x1234);
        assert_eq!(guarded.state[9], 0x1234 + 5 * MAX_INSTRUCTIONS as u32);
    }

    #[test]
    fn native_loops_preserve_budget_remainders_and_exit_ip() {
        let mut compiler = Compiler::new().unwrap();
        for condition in [None, Some(5)] {
            let block = compiler
                .compile(&[
                    Instruction {
                        length: 1,
                        operation: Operation::Inc {
                            register: 1,
                            decrement: true,
                        },
                    },
                    Instruction {
                        length: 2,
                        operation: Operation::LoopBranch { condition },
                    },
                ])
                .unwrap();
            for budget in [0, 1, 2, 3, 4, 5, 6, 7, 8] {
                let mut registers = [0xCAFE; 8];
                registers[1] = 3;
                let mut flags = 0x203;
                let mut ip = 0x1000;
                let iterations = if condition.is_some() {
                    (budget / 2).min(3)
                } else {
                    budget / 2
                };
                let completed =
                    compiler.execute(block, &mut registers, &mut flags, &mut ip, budget);
                assert_eq!(completed, iterations * 2);
                assert_eq!(registers[1], 3_u32.wrapping_sub(iterations));
                assert_eq!(registers[0], 0xCAFE);
                assert_eq!(&registers[2..], &[0xCAFE; 6]);
                assert_eq!(flags & 0x201, 0x201); // IF and carry survive DEC.
                assert_eq!(
                    ip,
                    if condition.is_some() && iterations == 3 {
                        0x1003
                    } else {
                        0x1000
                    }
                );
                if iterations == 0 {
                    assert_eq!(flags, 0x203);
                } else if iterations == 3 {
                    assert_eq!(flags, 0x247); // zero, parity and the preserved carry.
                }
            }
        }
    }

    #[test]
    fn loop_carry_crosses_the_backedge_and_large_budget_exits_on_condition() {
        let mut compiler = Compiler::new().unwrap();
        let block = compiler
            .compile(&[
                Instruction {
                    length: 3,
                    operation: Operation::Alu {
                        target: 0,
                        source: Source::Immediate(0),
                        operation: 2,
                        store: true,
                    },
                },
                Instruction {
                    length: 1,
                    operation: Operation::Inc {
                        register: 1,
                        decrement: true,
                    },
                },
                Instruction {
                    length: 2,
                    operation: Operation::LoopBranch { condition: Some(5) },
                },
            ])
            .unwrap();
        let mut registers = [0; 8];
        registers[0] = u32::MAX;
        registers[1] = 3;
        let mut flags = 3;
        let mut ip = 0x1000;
        assert_eq!(
            compiler.execute(block, &mut registers, &mut flags, &mut ip, u32::MAX),
            9
        );
        assert_eq!(registers[0], 1);
        assert_eq!(registers[1], 0);
        assert_eq!(flags, 0x46);
        assert_eq!(ip, 0x1006);
    }
}
