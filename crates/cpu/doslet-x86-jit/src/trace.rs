//! Bounded RAM traces. Every memory access is checked before execution.
use super::*;

#[derive(Clone, Copy, Debug)]
pub struct DwordMemoryAddress {
    pub base: Option<u8>,
    pub index: Option<u8>,
    /// Index shift (0 through 3).
    pub scale: u8,
    pub displacement: u32,
}
#[derive(Clone, Copy, Debug)]
pub enum TraceSource {
    Register(u8),
    Immediate(u32),
    Memory(DwordMemoryAddress),
    /// Legacy byte-register encoding: AL..BL then AH..BH.
    ByteRegister(u8),
    ByteMemory(DwordMemoryAddress),
}
#[derive(Clone, Copy, Debug)]
pub enum TraceOperation {
    Lea {
        target: u8,
        address: DwordMemoryAddress,
    },
    MoveByte {
        target: u8,
        source: TraceSource,
    },
    StoreByte {
        address: DwordMemoryAddress,
        source: TraceSource,
    },
    /// CMP (7) or TEST (4), including memory on the left.
    Compare {
        left: TraceSource,
        right: TraceSource,
        operation: u8,
        byte: bool,
    },
    Push {
        source: TraceSource,
    },
    Pop {
        register: u8,
    },
    /// Direct near CALL target relative to the trace entry EIP.
    Call {
        target: u32,
    },
    /// Indirect near CALL with an absolute 32-bit target EIP.
    CallIndirect {
        target: TraceSource,
    },
    Return {
        release: u16,
    },
    Move {
        target: u8,
        source: TraceSource,
    },
    /// Dword MOV to RAM; the source must be a register or immediate.
    Store {
        address: DwordMemoryAddress,
        source: TraceSource,
    },
    Alu {
        target: u8,
        source: TraceSource,
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
    /// Dword XCHG with ordinary RAM. Address and both operands use old state.
    ExchangeMemory {
        address: DwordMemoryAddress,
        register: u8,
    },
    Unary {
        register: u8,
        negate: bool,
    },
    /// Target offset relative to entry EIP, with wrapping 32-bit addition.
    Branch {
        condition: Option<u8>,
        target: u32,
    },
}
#[derive(Clone, Copy, Debug)]
pub struct TraceInstruction {
    pub offset: u8,
    pub length: u8,
    pub operation: TraceOperation,
}
#[derive(Clone, Copy, Debug)]
pub struct CompiledTrace(usize);

/// Optional host diagnostics produced during explicit compilation, outside guest
/// execution and timed measurements. `code` is the actual native code buffer
/// emitted by Cranelift; `ir` is the generated IR before backend compilation.
#[derive(Debug)]
pub struct TraceCompilation {
    pub code: Vec<u8>,
    pub ir: String,
}
/// Native exit classification is emitted only by explicitly profiled traces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum TraceExitReason {
    Uninstrumented = 0,
    Budget = 1,
    ExternalEdge = 2,
    LoadGuard = 3,
    StoreBounds = 4,
    ActiveCode = 5,
    GenerationBounds = 6,
    GenerationExhausted = 7,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TraceExecution {
    pub steps: u32,
    pub reason: TraceExitReason,
}
impl TraceExecution {
    fn uninstrumented(steps: u32) -> Self {
        Self {
            steps,
            reason: TraceExitReason::Uninstrumented,
        }
    }
    fn packed(result: u64) -> Self {
        let reason = match result >> 32 {
            1 => TraceExitReason::Budget,
            2 => TraceExitReason::ExternalEdge,
            3 => TraceExitReason::LoadGuard,
            4 => TraceExitReason::StoreBounds,
            5 => TraceExitReason::ActiveCode,
            6 => TraceExitReason::GenerationBounds,
            7 => TraceExitReason::GenerationExhausted,
            _ => TraceExitReason::Uninstrumented,
        };
        Self {
            steps: result as u32,
            reason,
        }
    }
}
#[derive(Clone, Copy)]
struct TraceExit {
    block: cranelift_codegen::ir::Block,
    profiled: bool,
}
type ReadOnlyEntry =
    unsafe extern "C" fn(*mut u32, *mut u32, *mut u32, u32, u32, *const u8, usize, u32, u32) -> u32;
type WritableEntry = unsafe extern "C" fn(
    *mut u32,
    *mut u32,
    *mut u32,
    u32,
    u32,
    *mut u8,
    usize,
    *mut u64,
    usize,
    u32,
    u32,
    u32,
    u32,
) -> u32;
type ProfiledReadOnlyEntry =
    unsafe extern "C" fn(*mut u32, *mut u32, *mut u32, u32, u32, *const u8, usize, u32, u32) -> u64;
type ProfiledWritableEntry = unsafe extern "C" fn(
    *mut u32,
    *mut u32,
    *mut u32,
    u32,
    u32,
    *mut u8,
    usize,
    *mut u64,
    usize,
    u32,
    u32,
    u32,
    u32,
) -> u64;
#[derive(Clone, Copy)]
pub(super) enum TraceEntry {
    ReadOnly(ReadOnlyEntry),
    Writable(WritableEntry),
    ProfiledReadOnly(ProfiledReadOnlyEntry),
    ProfiledWritable(ProfiledWritableEntry),
}

impl Compiler {
    pub fn compile_trace(
        &mut self,
        instructions: &[TraceInstruction],
    ) -> Result<CompiledTrace, String> {
        self.compile_trace_impl(instructions, false, false)
            .map(|(block, _)| block)
    }

    /// Compiles while collecting the actual native code buffer and input IR.
    /// This allocating diagnostic operation belongs outside `advance` and any
    /// timed execution region. Ordinary compilation does not copy these buffers.
    pub fn compile_trace_diagnostic(
        &mut self,
        instructions: &[TraceInstruction],
    ) -> Result<(CompiledTrace, TraceCompilation), String> {
        self.compile_trace_impl(instructions, true, false)
            .map(|(block, diagnostic)| (block, diagnostic.expect("diagnostics requested")))
    }

    /// Explicit host preparation for exit profiling. Its native return value
    /// includes the reason; ordinary compilation retains its original ABI/code.
    pub fn compile_trace_profiled(
        &mut self,
        instructions: &[TraceInstruction],
    ) -> Result<CompiledTrace, String> {
        self.compile_trace_impl(instructions, false, true)
            .map(|(block, _)| block)
    }

    fn compile_trace_impl(
        &mut self,
        instructions: &[TraceInstruction],
        diagnostic: bool,
        profiled: bool,
    ) -> Result<(CompiledTrace, Option<TraceCompilation>), String> {
        if instructions.is_empty()
            || instructions.len() > MAX_INSTRUCTIONS
            || instructions[0].offset != 0
        {
            return Err("invalid JIT trace length or entry".into());
        }
        let mut end = 0u16;
        for instruction in instructions {
            if u16::from(instruction.offset) < end
                || !(1..=15).contains(&instruction.length)
                || !trace_valid(instruction.operation)
            {
                return Err("invalid JIT trace instruction".into());
            }
            end = u16::from(instruction.offset) + u16::from(instruction.length);
            if end > 240 {
                return Err("JIT trace span exceeds limit".into());
            }
        }
        // A target inside an instruction must never be mistaken for a legal exit.
        for instruction in instructions {
            if let TraceOperation::Branch { target, .. } | TraceOperation::Call { target } =
                instruction.operation
                && instructions.iter().any(|i| {
                    target > u32::from(i.offset)
                        && target < u32::from(i.offset) + u32::from(i.length)
                })
            {
                return Err("JIT trace branch enters an instruction".into());
            }
        }
        if self.functions.len() + self.traces.len() >= MAX_COMPILED_BLOCKS {
            return Err("JIT compilation capacity exhausted".into());
        }
        let has_stores = instructions.iter().any(|i| {
            matches!(
                i.operation,
                TraceOperation::Store { .. }
                    | TraceOperation::ExchangeMemory { .. }
                    | TraceOperation::StoreByte { .. }
                    | TraceOperation::Push { .. }
                    | TraceOperation::Call { .. }
                    | TraceOperation::CallIndirect { .. }
            )
        });
        let module = self.module.as_mut().expect("compiler owns module");
        let pointer = module.target_config().pointer_type();
        let mut ctx = module.make_context();
        for ty in [
            pointer,
            pointer,
            pointer,
            types::I32,
            types::I32,
            pointer,
            pointer,
        ] {
            ctx.func.signature.params.push(AbiParam::new(ty));
        }
        if has_stores {
            for ty in [pointer, pointer, types::I32, types::I32] {
                ctx.func.signature.params.push(AbiParam::new(ty));
            }
        }
        // An optional excluded address interval can describe an MMIO hole in
        // the borrowed backing slice. It is checked by every full-width access.
        for _ in 0..2 {
            ctx.func.signature.params.push(AbiParam::new(types::I32));
        }
        ctx.func.signature.returns.push(AbiParam::new(if profiled {
            types::I64
        } else {
            types::I32
        }));
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
            let p = b.block_params(entry).to_vec();
            let regs = core::array::from_fn(|i| {
                b.ins()
                    .load(types::I32, MemFlags::trusted(), p[0], (i * 4) as i32)
            });
            let flags = b.ins().load(types::I32, MemFlags::trusted(), p[1], 0);
            let ip = b.ins().load(types::I32, MemFlags::trusted(), p[2], 0);
            // All exits share one state store and epilogue. Registers that no
            // instruction can modify stay exclusively in the caller's state.
            let dirty = instructions.iter().fold(0u8, |mask, instruction| {
                mask | match instruction.operation {
                    TraceOperation::Move { target, .. }
                    | TraceOperation::Lea { target, .. }
                    | TraceOperation::Alu {
                        target,
                        store: true,
                        ..
                    } => 1 << target,
                    TraceOperation::Inc { register, .. }
                    | TraceOperation::ExchangeMemory { register, .. }
                    | TraceOperation::Unary { register, .. } => 1 << register,
                    TraceOperation::Exchange { left, right } => (1 << left) | (1 << right),
                    TraceOperation::MoveByte { target, .. } => 1 << (target & 3),
                    TraceOperation::Pop { register } => (1 << register) | (1 << 4),
                    TraceOperation::Push { .. }
                    | TraceOperation::Call { .. }
                    | TraceOperation::CallIndirect { .. }
                    | TraceOperation::Return { .. } => 1 << 4,
                    _ => 0,
                }
            });
            let exit = TraceExit {
                block: b.create_block(),
                profiled,
            };
            for _ in 0..if profiled { 12 } else { 11 } {
                b.append_block_param(exit.block, types::I32);
            }
            let nodes: Vec<_> = instructions.iter().map(|_| b.create_block()).collect();
            for &node in &nodes {
                for _ in 0..10 {
                    b.append_block_param(node, types::I32);
                }
            }
            b.ins().jump(nodes[0], &loop_arguments(&regs, flags, p[3]));
            for (index, instruction) in instructions.iter().enumerate() {
                b.switch_to_block(nodes[index]);
                let mut regs: [Value; 8] =
                    core::array::from_fn(|i| b.block_params(nodes[index])[i]);
                let mut flags = b.block_params(nodes[index])[8];
                let remaining = b.block_params(nodes[index])[9];
                let current_ip = b.ins().iadd_imm(ip, i64::from(instruction.offset));
                let run = b.create_block();
                let exhausted = b.create_block();
                let enough = b.ins().icmp_imm(IntCC::UnsignedGreaterThan, remaining, 0);
                b.ins().brif(enough, run, &[], exhausted, &[]);
                b.switch_to_block(exhausted);
                trace_exit(
                    &mut b,
                    &regs,
                    flags,
                    current_ip,
                    remaining,
                    exit,
                    TraceExitReason::Budget,
                );
                b.switch_to_block(run);
                let state = TraceState {
                    p: &p,
                    flags,
                    ip: current_ip,
                    remaining,
                    exit,
                };
                let mut absolute_target = None;
                match instruction.operation {
                    TraceOperation::Move { target, source } => {
                        regs[target as usize] = trace_read(&mut b, &regs, source, state)
                    }
                    TraceOperation::Lea { target, address } => {
                        regs[target as usize] = trace_effective_address(&mut b, &regs, address)
                    }
                    TraceOperation::MoveByte { target, source } => {
                        let value = trace_read(&mut b, &regs, source, state);
                        let byte = b.ins().band_imm(value, 255);
                        let shift = if target >= 4 { 8 } else { 0 };
                        let byte = b.ins().ishl_imm(byte, shift);
                        let register = (target & 3) as usize;
                        let previous = b.ins().band_imm(regs[register], !(255i64 << shift));
                        regs[register] = b.ins().bor(previous, byte);
                    }
                    TraceOperation::Store { address, source }
                    | TraceOperation::StoreByte { address, source } => {
                        let value = trace_read(&mut b, &regs, source, state);
                        let width =
                            if matches!(instruction.operation, TraceOperation::StoreByte { .. }) {
                                1
                            } else {
                                4
                            };
                        trace_store(&mut b, &regs, address, value, width, state);
                    }
                    TraceOperation::Compare {
                        left,
                        right,
                        operation,
                        byte,
                    } => {
                        let left = trace_read(&mut b, &regs, left, state);
                        let right = trace_read(&mut b, &regs, right, state);
                        flags = if byte {
                            trace_byte_flags(&mut b, flags, left, right, operation)
                        } else {
                            alu(&mut b, flags, left, right, operation, 0x8d5, false).1
                        };
                    }
                    TraceOperation::Alu {
                        target,
                        source,
                        operation,
                        store,
                    } => {
                        let right = trace_read(&mut b, &regs, source, state);
                        let (result, next) = alu(
                            &mut b,
                            flags,
                            regs[target as usize],
                            right,
                            operation,
                            0x8d5,
                            false,
                        );
                        flags = next;
                        if store {
                            regs[target as usize] = result;
                        }
                    }
                    TraceOperation::Inc {
                        register,
                        decrement,
                    } => {
                        let one = b.ins().iconst(types::I32, 1);
                        let (result, next) = alu(
                            &mut b,
                            flags,
                            regs[register as usize],
                            one,
                            if decrement { 5 } else { 0 },
                            0x8d4,
                            true,
                        );
                        regs[register as usize] = result;
                        flags = next;
                    }
                    TraceOperation::Exchange { left, right } => {
                        regs.swap(left as usize, right as usize)
                    }
                    TraceOperation::ExchangeMemory { address, register } => {
                        // Both operands and the address remain based on the old
                        // registers until every write guard has succeeded.
                        let old_register = regs[register as usize];
                        let old_memory =
                            trace_read(&mut b, &regs, TraceSource::Memory(address), state);
                        trace_store(&mut b, &regs, address, old_register, 4, state);
                        regs[register as usize] = old_memory;
                    }
                    TraceOperation::Unary { register, negate } => {
                        let old = regs[register as usize];
                        regs[register as usize] = if negate {
                            let zero = b.ins().iconst(types::I32, 0);
                            let (result, next) = alu(&mut b, flags, zero, old, 5, 0x8d5, false);
                            flags = next;
                            result
                        } else {
                            b.ins().bnot(old)
                        };
                    }
                    TraceOperation::Push { source } => {
                        let value = trace_read(&mut b, &regs, source, state);
                        trace_store(&mut b, &regs, stack_address(u32::MAX - 3), value, 4, state);
                        regs[4] = b.ins().iadd_imm(regs[4], -4);
                    }
                    TraceOperation::Pop { register } => {
                        let value =
                            trace_read(&mut b, &regs, TraceSource::Memory(stack_address(0)), state);
                        regs[4] = b.ins().iadd_imm(regs[4], 4);
                        regs[register as usize] = value;
                    }
                    TraceOperation::Call { .. } | TraceOperation::CallIndirect { .. } => {
                        if let TraceOperation::CallIndirect { target } = instruction.operation {
                            absolute_target = Some(trace_read(&mut b, &regs, target, state));
                        }
                        let return_ip = b.ins().iadd_imm(current_ip, i64::from(instruction.length));
                        trace_store(
                            &mut b,
                            &regs,
                            stack_address(u32::MAX - 3),
                            return_ip,
                            4,
                            state,
                        );
                        regs[4] = b.ins().iadd_imm(regs[4], -4);
                    }
                    TraceOperation::Return { release } => {
                        absolute_target = Some(trace_read(
                            &mut b,
                            &regs,
                            TraceSource::Memory(stack_address(0)),
                            state,
                        ));
                        regs[4] = b.ins().iadd_imm(regs[4], 4 + i64::from(release));
                    }
                    TraceOperation::Branch { .. } => {}
                }
                let remaining = b.ins().iadd_imm(remaining, -1);
                let fallthrough = u32::from(instruction.offset) + u32::from(instruction.length);
                if let Some(target) = absolute_target {
                    trace_transfer_dynamic(
                        &mut b,
                        instructions,
                        &nodes,
                        target,
                        &regs,
                        flags,
                        remaining,
                        ip,
                        exit,
                    );
                } else if let TraceOperation::Call { target } = instruction.operation {
                    trace_transfer(
                        &mut b,
                        instructions,
                        &nodes,
                        target,
                        &regs,
                        flags,
                        remaining,
                        ip,
                        exit,
                    );
                } else if let TraceOperation::Branch { condition, target } = instruction.operation {
                    let taken = branch_condition(&mut b, flags, condition);
                    let yes = b.create_block();
                    let no = b.create_block();
                    b.ins().brif(taken, yes, &[], no, &[]);
                    b.switch_to_block(yes);
                    trace_transfer(
                        &mut b,
                        instructions,
                        &nodes,
                        target,
                        &regs,
                        flags,
                        remaining,
                        ip,
                        exit,
                    );
                    b.switch_to_block(no);
                    trace_transfer(
                        &mut b,
                        instructions,
                        &nodes,
                        fallthrough,
                        &regs,
                        flags,
                        remaining,
                        ip,
                        exit,
                    );
                } else {
                    trace_transfer(
                        &mut b,
                        instructions,
                        &nodes,
                        fallthrough,
                        &regs,
                        flags,
                        remaining,
                        ip,
                        exit,
                    );
                }
            }
            b.switch_to_block(exit.block);
            let state = b.block_params(exit.block).to_vec();
            for (i, &register) in state[..8].iter().enumerate() {
                if dirty & (1 << i) != 0 {
                    b.ins()
                        .store(MemFlags::trusted(), register, p[0], (i * 4) as i32);
                }
            }
            b.ins().store(MemFlags::trusted(), state[8], p[1], 0);
            b.ins().store(MemFlags::trusted(), state[9], p[2], 0);
            let completed = b.ins().isub(p[3], state[10]);
            let result = if profiled {
                let steps = b.ins().uextend(types::I64, completed);
                let reason = b.ins().uextend(types::I64, state[11]);
                let reason = b.ins().ishl_imm(reason, 32);
                b.ins().bor(steps, reason)
            } else {
                completed
            };
            b.ins().return_(&[result]);
            b.seal_all_blocks();
            b.finalize();
        }
        let ir = diagnostic.then(|| ctx.func.to_string());
        module
            .define_function(id, &mut ctx)
            .map_err(|e| e.to_string())?;
        let compilation = ir.map(|ir| TraceCompilation {
            code: ctx
                .compiled_code()
                .expect("successful definition emits machine code")
                .code_buffer()
                .to_vec(),
            ir,
        });
        module.finalize_definitions().map_err(|e| e.to_string())?;
        let block = CompiledTrace(self.traces.len());
        self.traces.push(trace_pointer(
            module.get_finalized_function(id),
            has_stores,
            profiled,
        ));
        Ok((block, compilation))
    }
    /// Borrows a device-free linear RAM span for this call only. Guest addresses
    /// never become host pointers until a complete dword has passed bounds checks.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_trace(
        &self,
        block: CompiledTrace,
        registers: &mut [u32; 8],
        flags: &mut u32,
        ip: &mut u32,
        budget: u32,
        ram_base: u32,
        ram: &[u8],
    ) -> u32 {
        self.execute_trace_report(block, registers, flags, ip, budget, ram_base, ram)
            .steps
    }

    /// Reports the native exit reason for explicitly profiled traces. Ordinary
    /// handles return `Uninstrumented`, with the same architectural state.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_trace_report(
        &self,
        block: CompiledTrace,
        registers: &mut [u32; 8],
        flags: &mut u32,
        ip: &mut u32,
        budget: u32,
        ram_base: u32,
        ram: &[u8],
    ) -> TraceExecution {
        self.execute_trace_report_with_exclusion(
            block, registers, flags, ip, budget, ram_base, ram, 0, 0,
        )
    }

    /// Executes with one runtime interval excluded from the RAM backing slice.
    /// Accesses touching any excluded byte exit before memory or state effects.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_trace_report_with_exclusion(
        &self,
        block: CompiledTrace,
        registers: &mut [u32; 8],
        flags: &mut u32,
        ip: &mut u32,
        budget: u32,
        ram_base: u32,
        ram: &[u8],
        excluded_start: u32,
        excluded_len: u32,
    ) -> TraceExecution {
        match self.traces[block.0] {
            TraceEntry::ReadOnly(entry) => TraceExecution::uninstrumented(trace_invoke(
                entry,
                registers,
                flags,
                ip,
                budget.min(256),
                ram_base,
                ram,
                excluded_start,
                excluded_len,
            )),
            TraceEntry::ProfiledReadOnly(entry) => trace_invoke_profiled(
                entry,
                registers,
                flags,
                ip,
                budget.min(256),
                ram_base,
                ram,
                excluded_start,
                excluded_len,
            ),
            TraceEntry::Writable(_) | TraceEntry::ProfiledWritable(_) => {
                TraceExecution::uninstrumented(0)
            }
        }
    }

    /// Executes a trace using a borrowed writable ordinary-RAM span and its full
    /// physical-page generation table. Every successful store increments each
    /// touched page exactly once. A store to the protected code span, exhausted
    /// generation or out-of-range address exits before any store side effect.
    /// The caller must map linear addresses in the span directly to physical RAM
    /// and protect the executing code; device memory must not appear in `ram`.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_trace_writable(
        &self,
        block: CompiledTrace,
        registers: &mut [u32; 8],
        flags: &mut u32,
        ip: &mut u32,
        budget: u32,
        ram_base: u32,
        ram: &mut [u8],
        page_generations: &mut [u64],
        protected_start: u32,
        protected_len: u32,
    ) -> u32 {
        self.execute_trace_writable_report(
            block,
            registers,
            flags,
            ip,
            budget,
            ram_base,
            ram,
            page_generations,
            protected_start,
            protected_len,
        )
        .steps
    }

    /// Writable counterpart of `execute_trace_report`; neither call allocates.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_trace_writable_report(
        &self,
        block: CompiledTrace,
        registers: &mut [u32; 8],
        flags: &mut u32,
        ip: &mut u32,
        budget: u32,
        ram_base: u32,
        ram: &mut [u8],
        page_generations: &mut [u64],
        protected_start: u32,
        protected_len: u32,
    ) -> TraceExecution {
        self.execute_trace_writable_report_with_exclusion(
            block,
            registers,
            flags,
            ip,
            budget,
            ram_base,
            ram,
            page_generations,
            protected_start,
            protected_len,
            0,
            0,
        )
    }

    /// Executes with one runtime interval excluded from the RAM backing slice.
    /// Accesses touching any excluded byte exit before memory or state effects.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_trace_writable_report_with_exclusion(
        &self,
        block: CompiledTrace,
        registers: &mut [u32; 8],
        flags: &mut u32,
        ip: &mut u32,
        budget: u32,
        ram_base: u32,
        ram: &mut [u8],
        page_generations: &mut [u64],
        protected_start: u32,
        protected_len: u32,
        excluded_start: u32,
        excluded_len: u32,
    ) -> TraceExecution {
        match self.traces[block.0] {
            TraceEntry::ReadOnly(entry) => TraceExecution::uninstrumented(trace_invoke(
                entry,
                registers,
                flags,
                ip,
                budget.min(256),
                ram_base,
                ram,
                excluded_start,
                excluded_len,
            )),
            TraceEntry::ProfiledReadOnly(entry) => trace_invoke_profiled(
                entry,
                registers,
                flags,
                ip,
                budget.min(256),
                ram_base,
                ram,
                excluded_start,
                excluded_len,
            ),
            TraceEntry::Writable(entry) => TraceExecution::uninstrumented(trace_invoke_writable(
                entry,
                registers,
                flags,
                ip,
                budget.min(256),
                ram_base,
                ram,
                page_generations,
                protected_start,
                protected_len,
                excluded_start,
                excluded_len,
            )),
            TraceEntry::ProfiledWritable(entry) => trace_invoke_writable_profiled(
                entry,
                registers,
                flags,
                ip,
                budget.min(256),
                ram_base,
                ram,
                page_generations,
                protected_start,
                protected_len,
                excluded_start,
                excluded_len,
            ),
        }
    }
}
fn trace_address_valid(address: DwordMemoryAddress) -> bool {
    address.base.is_none_or(|r| r < 8) && address.index.is_none_or(|r| r < 8) && address.scale < 4
}
#[derive(Clone, Copy)]
struct TraceState<'a> {
    p: &'a [Value],
    flags: Value,
    ip: Value,
    remaining: Value,
    exit: TraceExit,
}
fn stack_address(displacement: u32) -> DwordMemoryAddress {
    DwordMemoryAddress {
        base: Some(4),
        index: None,
        scale: 0,
        displacement,
    }
}
fn trace_effective_address(
    b: &mut FunctionBuilder<'_>,
    regs: &[Value; 8],
    address: DwordMemoryAddress,
) -> Value {
    let mut value = b.ins().iconst(types::I32, i64::from(address.displacement));
    if let Some(base) = address.base {
        value = b.ins().iadd(value, regs[base as usize]);
    }
    if let Some(index) = address.index {
        let scaled = b
            .ins()
            .ishl_imm(regs[index as usize], i64::from(address.scale));
        value = b.ins().iadd(value, scaled);
    }
    value
}
/// Computes a wrapped effective address and a complete-width RAM bounds proof.
fn trace_memory_address(
    b: &mut FunctionBuilder<'_>,
    regs: &[Value; 8],
    address: DwordMemoryAddress,
    width: u8,
    p: &[Value],
) -> (Value, Value, Value) {
    let value = trace_effective_address(b, regs, address);
    let wide = b.ins().uextend(types::I64, value);
    let base = b.ins().uextend(types::I64, p[4]);
    let offset = b.ins().isub(wide, base);
    let after = b.ins().iadd_imm(offset, i64::from(width));
    let above = b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, wide, base);
    let fits = b.ins().icmp(IntCC::UnsignedLessThanOrEqual, after, p[6]);
    let nowrap = b.ins().icmp_imm(
        IntCC::UnsignedLessThanOrEqual,
        wide,
        i64::from(u32::MAX) - i64::from(width) + 1,
    );
    let fits = b.ins().band(fits, nowrap);
    let valid = b.ins().band(above, fits);
    let excluded_start = b.ins().uextend(types::I64, p[p.len() - 2]);
    let excluded_len = b.ins().uextend(types::I64, p[p.len() - 1]);
    let excluded_end = b.ins().iadd(excluded_start, excluded_len);
    let end = b.ins().iadd_imm(wide, i64::from(width) - 1);
    let before = b.ins().icmp(IntCC::UnsignedLessThan, end, excluded_start);
    let after = b
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, wide, excluded_end);
    let disabled = b.ins().icmp_imm(IntCC::Equal, excluded_len, 0);
    let outside = b.ins().bor(before, after);
    let outside = b.ins().bor(outside, disabled);
    let valid = b.ins().band(valid, outside);
    (wide, offset, valid)
}
fn trace_guard(
    b: &mut FunctionBuilder<'_>,
    regs: &[Value; 8],
    valid: Value,
    state: TraceState<'_>,
    reason: TraceExitReason,
) {
    let success = b.create_block();
    let failure = b.create_block();
    b.ins().brif(valid, success, &[], failure, &[]);
    b.switch_to_block(failure);
    trace_exit(
        b,
        regs,
        state.flags,
        state.ip,
        state.remaining,
        state.exit,
        reason,
    );
    b.switch_to_block(success);
}
fn trace_read(
    b: &mut FunctionBuilder<'_>,
    regs: &[Value; 8],
    source: TraceSource,
    state: TraceState<'_>,
) -> Value {
    match source {
        TraceSource::Register(r) => regs[r as usize],
        TraceSource::ByteRegister(r) => {
            let value = regs[(r & 3) as usize];
            let value = if r >= 4 {
                b.ins().ushr_imm(value, 8)
            } else {
                value
            };
            b.ins().band_imm(value, 255)
        }
        TraceSource::Immediate(i) => b.ins().iconst(types::I32, i64::from(i)),
        TraceSource::Memory(address) | TraceSource::ByteMemory(address) => {
            let byte = matches!(source, TraceSource::ByteMemory(_));
            let (_, offset, valid) =
                trace_memory_address(b, regs, address, if byte { 1 } else { 4 }, state.p);
            trace_guard(b, regs, valid, state, TraceExitReason::LoadGuard);
            let host = b.ins().iadd(state.p[5], offset);
            let loaded = b.ins().load(
                if byte { types::I8 } else { types::I32 },
                MemFlags::new().with_endianness(cranelift_codegen::ir::Endianness::Little),
                host,
                0,
            );
            if byte {
                b.ins().uextend(types::I32, loaded)
            } else {
                loaded
            }
        }
    }
}
fn trace_store(
    b: &mut FunctionBuilder<'_>,
    regs: &[Value; 8],
    address: DwordMemoryAddress,
    value: Value,
    width: u8,
    state: TraceState<'_>,
) {
    let p = state.p;
    let (wide, offset, valid) = trace_memory_address(b, regs, address, width, p);
    let end = b.ins().iadd_imm(wide, i64::from(width) - 1);
    let first_page = b.ins().ushr_imm(wide, 12);
    let last_page = b.ins().ushr_imm(end, 12);
    let page_fits = b.ins().icmp(IntCC::UnsignedLessThan, last_page, p[8]);
    let protected_start = b.ins().uextend(types::I64, p[9]);
    let protected_len = b.ins().uextend(types::I64, p[10]);
    let protected_end = b.ins().iadd(protected_start, protected_len);
    let before = b.ins().icmp(IntCC::UnsignedLessThan, end, protected_start);
    let after = b
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, wide, protected_end);
    let empty = b.ins().icmp_imm(IntCC::Equal, protected_len, 0);
    let unprotected = b.ins().bor(before, after);
    let unprotected = b.ins().bor(unprotected, empty);
    if state.exit.profiled {
        for (condition, reason) in [
            (valid, TraceExitReason::StoreBounds),
            (page_fits, TraceExitReason::GenerationBounds),
            (unprotected, TraceExitReason::ActiveCode),
        ] {
            trace_guard(b, regs, condition, state, reason);
        }
    } else {
        let valid = b.ins().band(valid, page_fits);
        let valid = b.ins().band(valid, unprotected);
        trace_guard(b, regs, valid, state, TraceExitReason::StoreBounds);
    }
    let first_offset = b.ins().ishl_imm(first_page, 3);
    let last_offset = b.ins().ishl_imm(last_page, 3);
    let first_ptr = b.ins().iadd(p[7], first_offset);
    let last_ptr = b.ins().iadd(p[7], last_offset);
    let first_generation = b.ins().load(types::I64, MemFlags::trusted(), first_ptr, 0);
    let last_generation = b.ins().load(types::I64, MemFlags::trusted(), last_ptr, 0);
    let first_ok = b.ins().icmp_imm(IntCC::NotEqual, first_generation, -1);
    let last_ok = b.ins().icmp_imm(IntCC::NotEqual, last_generation, -1);
    let valid = b.ins().band(first_ok, last_ok);
    trace_guard(b, regs, valid, state, TraceExitReason::GenerationExhausted);
    let host = b.ins().iadd(p[5], offset);
    let value = if width == 1 {
        b.ins().ireduce(types::I8, value)
    } else {
        value
    };
    b.ins().store(
        MemFlags::new().with_endianness(cranelift_codegen::ir::Endianness::Little),
        value,
        host,
        0,
    );
    let first_next = b.ins().iadd_imm(first_generation, 1);
    let last_next = b.ins().iadd_imm(last_generation, 1);
    b.ins().store(MemFlags::trusted(), first_next, first_ptr, 0);
    let distinct = b.ins().icmp(IntCC::NotEqual, first_page, last_page);
    let second = b.create_block();
    let committed = b.create_block();
    b.ins().brif(distinct, second, &[], committed, &[]);
    b.switch_to_block(second);
    b.ins().store(MemFlags::trusted(), last_next, last_ptr, 0);
    b.ins().jump(committed, &[]);
    b.switch_to_block(committed);
}
fn trace_byte_flags(
    b: &mut FunctionBuilder<'_>,
    previous: Value,
    left: Value,
    right: Value,
    operation: u8,
) -> Value {
    let left = b.ins().band_imm(left, 255);
    let right = b.ins().band_imm(right, 255);
    let result = if operation == 4 {
        b.ins().band(left, right)
    } else {
        b.ins().isub(left, right)
    };
    let result = b.ins().band_imm(result, 255);
    let retained = b.ins().band_imm(previous, !0x8d5i64);
    let mut flags = b.ins().bor_imm(retained, 2);
    let zero = b.ins().icmp_imm(IntCC::Equal, result, 0);
    let zero = flag(b, zero, 6);
    flags = b.ins().bor(flags, zero);
    let sign = b.ins().band_imm(result, 0x80);
    flags = b.ins().bor(flags, sign);
    let parity = b.ins().popcnt(result);
    let parity = b.ins().band_imm(parity, 1);
    let parity = b.ins().bxor_imm(parity, 1);
    let parity = b.ins().ishl_imm(parity, 2);
    flags = b.ins().bor(flags, parity);
    if operation == 7 {
        let carry = b.ins().icmp(IntCC::UnsignedLessThan, left, right);
        let carry = flag(b, carry, 0);
        flags = b.ins().bor(flags, carry);
        let lr = b.ins().bxor(left, right);
        let changed = b.ins().bxor(left, result);
        let auxiliary = b.ins().bxor(lr, result);
        let auxiliary = b.ins().band_imm(auxiliary, 0x10);
        flags = b.ins().bor(flags, auxiliary);
        let overflow = b.ins().band(lr, changed);
        let overflow = b.ins().band_imm(overflow, 0x80);
        let overflow = b.ins().ishl_imm(overflow, 4);
        flags = b.ins().bor(flags, overflow);
    }
    flags
}
fn trace_valid(op: TraceOperation) -> bool {
    let source_valid = |s| match s {
        TraceSource::Register(r) => r < 8,
        TraceSource::Immediate(_) => true,
        TraceSource::Memory(a) => trace_address_valid(a),
        _ => false,
    };
    let byte_valid = |s| match s {
        TraceSource::ByteRegister(r) => r < 8,
        TraceSource::Immediate(i) => i <= 255,
        TraceSource::ByteMemory(a) => trace_address_valid(a),
        _ => false,
    };
    let memory = |s| matches!(s, TraceSource::Memory(_) | TraceSource::ByteMemory(_));
    match op {
        TraceOperation::Move { target, source } => target < 8 && source_valid(source),
        TraceOperation::Lea { target, address } => target < 8 && trace_address_valid(address),
        TraceOperation::MoveByte { target, source } => target < 8 && byte_valid(source),
        TraceOperation::Store { address, source } => {
            trace_address_valid(address) && !memory(source) && source_valid(source)
        }
        TraceOperation::StoreByte { address, source } => {
            trace_address_valid(address) && !memory(source) && byte_valid(source)
        }
        TraceOperation::Compare {
            left,
            right,
            operation,
            byte,
        } => {
            matches!(operation, 4 | 7)
                && !(memory(left) && memory(right))
                && if byte {
                    byte_valid(left) && byte_valid(right)
                } else {
                    source_valid(left) && source_valid(right)
                }
        }
        TraceOperation::Alu {
            target,
            source,
            operation,
            ..
        } => target < 8 && source_valid(source) && operation < 8,
        TraceOperation::Inc { register, .. }
        | TraceOperation::Unary { register, .. }
        | TraceOperation::Pop { register } => register < 8,
        TraceOperation::Exchange { left, right } => left < 8 && right < 8,
        TraceOperation::ExchangeMemory { address, register } => {
            register < 8 && trace_address_valid(address)
        }
        TraceOperation::Push { source } | TraceOperation::CallIndirect { target: source } => {
            source_valid(source)
        }
        TraceOperation::Branch { condition, .. } => condition.is_none_or(|c| c < 16),
        TraceOperation::Call { .. } | TraceOperation::Return { .. } => true,
    }
}
#[allow(clippy::too_many_arguments)]
fn trace_exit(
    b: &mut FunctionBuilder<'_>,
    regs: &[Value; 8],
    flags: Value,
    ip: Value,
    remaining: Value,
    exit: TraceExit,
    reason: TraceExitReason,
) {
    let mut arguments: Vec<BlockArg> = (0..11)
        .map(|i| {
            match i {
                0..=7 => regs[i],
                8 => flags,
                9 => ip,
                _ => remaining,
            }
            .into()
        })
        .collect();
    if exit.profiled {
        arguments.push(b.ins().iconst(types::I32, reason as i64).into());
    }
    b.ins().jump(exit.block, &arguments);
}
#[allow(clippy::too_many_arguments)]
fn trace_transfer(
    b: &mut FunctionBuilder<'_>,
    instructions: &[TraceInstruction],
    nodes: &[cranelift_codegen::ir::Block],
    target: u32,
    regs: &[Value; 8],
    flags: Value,
    remaining: Value,
    ip: Value,
    exit: TraceExit,
) {
    if let Some(index) = instructions
        .iter()
        .position(|i| u32::from(i.offset) == target)
    {
        b.ins()
            .jump(nodes[index], &loop_arguments(regs, flags, remaining));
    } else {
        let next = b.ins().iadd_imm(ip, i64::from(target));
        trace_exit(
            b,
            regs,
            flags,
            next,
            remaining,
            exit,
            TraceExitReason::ExternalEdge,
        );
    }
}
#[allow(clippy::too_many_arguments)]
fn trace_transfer_dynamic(
    b: &mut FunctionBuilder<'_>,
    instructions: &[TraceInstruction],
    nodes: &[cranelift_codegen::ir::Block],
    target: Value,
    regs: &[Value; 8],
    flags: Value,
    remaining: Value,
    entry_ip: Value,
    exit: TraceExit,
) {
    for (index, instruction) in instructions.iter().enumerate() {
        let candidate = b.ins().iadd_imm(entry_ip, i64::from(instruction.offset));
        let matches = b.ins().icmp(IntCC::Equal, target, candidate);
        let next = b.create_block();
        b.ins().brif(
            matches,
            nodes[index],
            &loop_arguments(regs, flags, remaining),
            next,
            &[],
        );
        b.switch_to_block(next);
    }
    trace_exit(
        b,
        regs,
        flags,
        target,
        remaining,
        exit,
        TraceExitReason::ExternalEdge,
    );
}
#[allow(unsafe_code)]
fn trace_pointer(code: *const u8, has_stores: bool, profiled: bool) -> TraceEntry {
    // SAFETY: The private pointer uses the signature selected during codegen
    // and remains owned by the compiler's executable-memory module.
    unsafe {
        match (has_stores, profiled) {
            (false, false) => {
                TraceEntry::ReadOnly(core::mem::transmute::<*const u8, ReadOnlyEntry>(code))
            }
            (true, false) => {
                TraceEntry::Writable(core::mem::transmute::<*const u8, WritableEntry>(code))
            }
            (false, true) => TraceEntry::ProfiledReadOnly(core::mem::transmute::<
                *const u8,
                ProfiledReadOnlyEntry,
            >(code)),
            (true, true) => TraceEntry::ProfiledWritable(core::mem::transmute::<
                *const u8,
                ProfiledWritableEntry,
            >(code)),
        }
    }
}
#[allow(unsafe_code, clippy::too_many_arguments)]
fn trace_invoke(
    entry: ReadOnlyEntry,
    registers: &mut [u32; 8],
    flags: &mut u32,
    ip: &mut u32,
    budget: u32,
    ram_base: u32,
    ram: &[u8],
    excluded_start: u32,
    excluded_len: u32,
) -> u32 {
    // SAFETY: State borrows are disjoint and alive throughout the call. All RAM
    // loads are bounded by the borrowed slice, and generated code never stores
    // to it or retains any pointer.
    unsafe {
        entry(
            registers.as_mut_ptr(),
            flags,
            ip,
            budget,
            ram_base,
            ram.as_ptr(),
            ram.len(),
            excluded_start,
            excluded_len,
        )
    }
}

#[allow(unsafe_code, clippy::too_many_arguments)]
fn trace_invoke_writable(
    entry: WritableEntry,
    registers: &mut [u32; 8],
    flags: &mut u32,
    ip: &mut u32,
    budget: u32,
    ram_base: u32,
    ram: &mut [u8],
    page_generations: &mut [u64],
    protected_start: u32,
    protected_len: u32,
    excluded_start: u32,
    excluded_len: u32,
) -> u32 {
    // SAFETY: All borrows are disjoint and valid for this call. Generated code
    // proves the entire RAM access and both generation indices before dereference,
    // rejects counter overflow before mutation, and never retains pointers.
    unsafe {
        entry(
            registers.as_mut_ptr(),
            flags,
            ip,
            budget,
            ram_base,
            ram.as_mut_ptr(),
            ram.len(),
            page_generations.as_mut_ptr(),
            page_generations.len(),
            protected_start,
            protected_len,
            excluded_start,
            excluded_len,
        )
    }
}

#[allow(unsafe_code, clippy::too_many_arguments)]
fn trace_invoke_profiled(
    entry: ProfiledReadOnlyEntry,
    registers: &mut [u32; 8],
    flags: &mut u32,
    ip: &mut u32,
    budget: u32,
    ram_base: u32,
    ram: &[u8],
    excluded_start: u32,
    excluded_len: u32,
) -> TraceExecution {
    // SAFETY: Same checked memory operations and borrow lifetimes as the normal
    // read-only ABI; only the private native return type differs.
    TraceExecution::packed(unsafe {
        entry(
            registers.as_mut_ptr(),
            flags,
            ip,
            budget,
            ram_base,
            ram.as_ptr(),
            ram.len(),
            excluded_start,
            excluded_len,
        )
    })
}
#[allow(unsafe_code, clippy::too_many_arguments)]
fn trace_invoke_writable_profiled(
    entry: ProfiledWritableEntry,
    registers: &mut [u32; 8],
    flags: &mut u32,
    ip: &mut u32,
    budget: u32,
    ram_base: u32,
    ram: &mut [u8],
    page_generations: &mut [u64],
    protected_start: u32,
    protected_len: u32,
    excluded_start: u32,
    excluded_len: u32,
) -> TraceExecution {
    // SAFETY: Same checked memory operations and borrow lifetimes as the normal
    // writable ABI; only the private native return type differs.
    TraceExecution::packed(unsafe {
        entry(
            registers.as_mut_ptr(),
            flags,
            ip,
            budget,
            ram_base,
            ram.as_mut_ptr(),
            ram.len(),
            page_generations.as_mut_ptr(),
            page_generations.len(),
            protected_start,
            protected_len,
            excluded_start,
            excluded_len,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ram_trace_follows_links_without_returning_and_preserves_partial_progress() {
        let mut compiler = Compiler::new().unwrap();
        let instructions = [
            TraceInstruction {
                offset: 0,
                length: 2,
                operation: TraceOperation::Move {
                    target: 0,
                    source: TraceSource::Memory(DwordMemoryAddress {
                        base: Some(0),
                        index: None,
                        scale: 0,
                        displacement: 0,
                    }),
                },
            },
            TraceInstruction {
                offset: 2,
                length: 2,
                operation: TraceOperation::Alu {
                    target: 0,
                    source: TraceSource::Register(0),
                    operation: 4,
                    store: false,
                },
            },
            TraceInstruction {
                offset: 4,
                length: 2,
                operation: TraceOperation::Branch {
                    condition: Some(5),
                    target: 0,
                },
            },
        ];
        let trace = compiler.compile_trace(&instructions).unwrap();
        // Unaligned addresses and little-endian links are intentional.
        let mut ram = [0u8; 17];
        ram[1..5].copy_from_slice(&0x1005u32.to_le_bytes());
        ram[5..9].copy_from_slice(&0x1009u32.to_le_bytes());
        for budget in 0..=12 {
            let mut regs = [0xCAFE; 8];
            regs[0] = 0x1001;
            let mut flags = 0x203;
            let mut ip = 0x2000;
            let completed =
                compiler.execute_trace(trace, &mut regs, &mut flags, &mut ip, budget, 0x1000, &ram);
            assert_eq!(completed, budget.min(9));
            assert_eq!(
                ip,
                if budget >= 9 {
                    0x2006
                } else {
                    0x2000 + (budget % 3) * 2
                }
            );
            assert_eq!(
                regs[0],
                match budget {
                    0 => 0x1001,
                    1..=3 => 0x1005,
                    4..=6 => 0x1009,
                    _ => 0,
                }
            );
            assert_eq!(&regs[1..], &[0xCAFE; 7]);
            if budget >= 8 {
                assert_eq!(flags, 0x246);
            }
        }
        // First link succeeds, second points outside the borrowed span. A failing
        // dword read neither retires nor changes its destination or flags.
        ram[1..5].copy_from_slice(&0x100Eu32.to_le_bytes());
        let mut regs = [0; 8];
        regs[0] = 0x1001;
        let mut flags = 0x203;
        let mut ip = 0x2000;
        assert_eq!(
            compiler.execute_trace(trace, &mut regs, &mut flags, &mut ip, 256, 0x1000, &ram),
            3
        );
        assert_eq!(regs[0], 0x100E);
        assert_eq!(ip, 0x2000);
        assert_eq!(flags, 0x202);
        for (base, address) in [(0x1000, 0xFFF), (0, u32::MAX - 2), (0x1000, 0x1010)] {
            regs[0] = address;
            flags = 0x203;
            ip = 0x2000;
            assert_eq!(
                compiler.execute_trace(trace, &mut regs, &mut flags, &mut ip, 256, base, &ram),
                0
            );
            assert_eq!(regs[0], address);
            assert_eq!(flags, 0x203);
            assert_eq!(ip, 0x2000);
        }
    }

    #[test]
    fn inc_and_dec_preserve_incoming_carry_when_the_arithmetic_wraps() {
        let mut compiler = Compiler::new().unwrap();
        for decrement in [false, true] {
            let instruction = TraceInstruction {
                offset: 0,
                length: 1,
                operation: TraceOperation::Inc {
                    register: 0,
                    decrement,
                },
            };
            for handle in [
                compiler.compile_trace(&[instruction]).unwrap(),
                compiler.compile_trace_profiled(&[instruction]).unwrap(),
            ] {
                for carry in [0, 1] {
                    let mut registers = [0xCAFE; 8];
                    registers[0] = if decrement { 0 } else { u32::MAX };
                    let mut flags = 0x202 | carry;
                    let mut ip = 0x1000;
                    assert_eq!(
                        compiler.execute_trace(
                            handle,
                            &mut registers,
                            &mut flags,
                            &mut ip,
                            1,
                            0,
                            &[]
                        ),
                        1
                    );
                    assert_eq!(registers[0], if decrement { u32::MAX } else { 0 });
                    assert_eq!(&registers[1..], &[0xCAFE; 7]);
                    // DEC 0: SF/PF/AF set. INC -1: ZF/PF/AF set. Neither
                    // instruction writes CF, even when its arithmetic borrows
                    // or carries; IF and all unrelated flags survive as well.
                    assert_eq!(flags, (if decrement { 0x296 } else { 0x256 }) | carry);
                    assert_eq!(ip, 0x1001);
                }
            }
        }
    }

    #[test]
    fn memory_exchange_uses_old_address_with_esp_and_other_register_aliases() {
        let mut compiler = Compiler::new().unwrap();
        for register in [0, 4] {
            let displacement = if register == 4 { 4 } else { 0 };
            let instruction = TraceInstruction {
                offset: 0,
                length: 3,
                operation: TraceOperation::ExchangeMemory {
                    address: DwordMemoryAddress {
                        base: Some(register),
                        index: None,
                        scale: 0,
                        displacement,
                    },
                    register,
                },
            };
            for handle in [
                compiler.compile_trace(&[instruction]).unwrap(),
                compiler.compile_trace_profiled(&[instruction]).unwrap(),
            ] {
                for budget in 0..=1 {
                    let mut regs = [10, 11, 12, 13, 14, 15, 16, 17];
                    regs[register as usize] = 0x2FFE - displacement;
                    let old_register = regs[register as usize];
                    let mut expected_regs = regs;
                    let mut flags = 0xA203;
                    let mut ip = 0x5000;
                    let mut ram = vec![0xCC; 8192];
                    ram[4094..4098].copy_from_slice(&0x12345678u32.to_le_bytes());
                    let mut generations = [7; 4];
                    let result = compiler.execute_trace_writable(
                        handle,
                        &mut regs,
                        &mut flags,
                        &mut ip,
                        budget,
                        0x2000,
                        &mut ram,
                        &mut generations,
                        0x5000,
                        3,
                    );
                    assert_eq!(result, budget);
                    if budget == 1 {
                        expected_regs[register as usize] = 0x12345678;
                        assert_eq!(&ram[4094..4098], &old_register.to_le_bytes());
                        assert_eq!(generations, [7, 7, 8, 8]);
                    } else {
                        assert_eq!(&ram[4094..4098], &0x12345678u32.to_le_bytes());
                        assert_eq!(generations, [7; 4]);
                    }
                    assert_eq!(regs, expected_regs);
                    assert_eq!(flags, 0xA203);
                    assert_eq!(ip, 0x5000 + budget * 3);
                    assert!(ram[..4094].iter().chain(&ram[4098..]).all(|&v| v == 0xCC));
                }
            }
        }
    }

    #[test]
    fn memory_exchange_failure_keeps_both_operands_unchanged() {
        let mut compiler = Compiler::new().unwrap();
        let instruction = TraceInstruction {
            offset: 0,
            length: 3,
            operation: TraceOperation::ExchangeMemory {
                address: DwordMemoryAddress {
                    base: Some(4),
                    index: None,
                    scale: 0,
                    displacement: 4,
                },
                register: 0,
            },
        };
        for (profiled, handle) in [
            (false, compiler.compile_trace(&[instruction]).unwrap()),
            (
                true,
                compiler.compile_trace_profiled(&[instruction]).unwrap(),
            ),
        ] {
            for (esp, table_len, exhausted, protected_start, excluded_start, reason) in [
                (0x3FFA, 4, false, 0x5000, 0, TraceExitReason::LoadGuard), // partial RAM span
                (
                    0x2FFA,
                    3,
                    false,
                    0x5000,
                    0,
                    TraceExitReason::GenerationBounds,
                ),
                (
                    0x2FFA,
                    4,
                    true,
                    0x5000,
                    0,
                    TraceExitReason::GenerationExhausted,
                ),
                (0x2FFA, 4, false, 0x3000, 0, TraceExitReason::ActiveCode),
                (0x2FFA, 4, false, 0x5000, 0x3000, TraceExitReason::LoadGuard),
            ] {
                let mut regs = [0xCAFEBABE, 11, 12, 13, esp, 15, 16, 17];
                let original = regs;
                let mut flags = 0xA203;
                let mut ip = 0x5000;
                let mut ram = vec![0xCC; 8192];
                let mut generations = [7, 7, 7, if exhausted { u64::MAX } else { 7 }];
                let old_generations = generations;
                let result = compiler.execute_trace_writable_report_with_exclusion(
                    handle,
                    &mut regs,
                    &mut flags,
                    &mut ip,
                    1,
                    0x2000,
                    &mut ram,
                    &mut generations[..table_len],
                    protected_start,
                    3,
                    excluded_start,
                    if excluded_start == 0 { 0 } else { 3 },
                );
                assert_eq!(
                    result,
                    TraceExecution {
                        steps: 0,
                        reason: if profiled {
                            reason
                        } else {
                            TraceExitReason::Uninstrumented
                        }
                    }
                );
                assert_eq!(regs, original);
                assert_eq!(flags, 0xA203);
                assert_eq!(ip, 0x5000);
                assert_eq!(generations, old_generations);
                assert!(ram.iter().all(|&v| v == 0xCC));
            }
        }
    }

    #[test]
    fn runtime_exclusion_rejects_every_overlapping_byte_of_loads_stores_and_stack() {
        let mut compiler = Compiler::new().unwrap();
        for byte in [false, true] {
            for store in [false, true] {
                let address = DwordMemoryAddress {
                    base: Some(1),
                    index: None,
                    scale: 0,
                    displacement: 0,
                };
                let operation = match (byte, store) {
                    (false, false) => TraceOperation::Move {
                        target: 0,
                        source: TraceSource::Memory(address),
                    },
                    (true, false) => TraceOperation::MoveByte {
                        target: 4,
                        source: TraceSource::ByteMemory(address),
                    },
                    (false, true) => TraceOperation::Store {
                        address,
                        source: TraceSource::Immediate(5),
                    },
                    (true, true) => TraceOperation::StoreByte {
                        address,
                        source: TraceSource::Immediate(5),
                    },
                };
                let instructions = [TraceInstruction {
                    offset: 0,
                    length: 2,
                    operation,
                }];
                for (profiled, handle) in [
                    (false, compiler.compile_trace(&instructions).unwrap()),
                    (
                        true,
                        compiler.compile_trace_profiled(&instructions).unwrap(),
                    ),
                ] {
                    for offset in 0..=16u32 {
                        let width = if byte { 1 } else { 4 };
                        let allowed = offset + width <= 16 && (offset + width <= 8 || offset >= 12);
                        let mut regs = [0xCAFEAB12, 0x1000 + offset, 12, 13, 14, 15, 16, 17];
                        let original = regs;
                        let mut flags = 0xA203;
                        let mut ip = 0x2000;
                        let mut ram = [0xCC; 16];
                        let mut generations = [7; 2];
                        let result = if store {
                            compiler.execute_trace_writable_report_with_exclusion(
                                handle,
                                &mut regs,
                                &mut flags,
                                &mut ip,
                                1,
                                0x1000,
                                &mut ram,
                                &mut generations,
                                0x2000,
                                2,
                                0x1008,
                                4,
                            )
                        } else {
                            compiler.execute_trace_report_with_exclusion(
                                handle, &mut regs, &mut flags, &mut ip, 1, 0x1000, &ram, 0x1008, 4,
                            )
                        };
                        assert_eq!(result.steps, u32::from(allowed));
                        assert_eq!(
                            result.reason,
                            if !profiled {
                                TraceExitReason::Uninstrumented
                            } else if allowed {
                                TraceExitReason::ExternalEdge
                            } else if store {
                                TraceExitReason::StoreBounds
                            } else {
                                TraceExitReason::LoadGuard
                            }
                        );
                        assert_eq!(flags, 0xA203);
                        assert_eq!(ip, if allowed { 0x2002 } else { 0x2000 });
                        if !allowed {
                            assert_eq!(regs, original);
                            assert_eq!(ram, [0xCC; 16]);
                            assert_eq!(generations, [7; 2]);
                        }
                    }
                }
            }
        }
        let source = DwordMemoryAddress {
            base: Some(0),
            index: None,
            scale: 0,
            displacement: 0,
        };
        for (operation, esp, expected) in [
            (
                TraceOperation::CallIndirect {
                    target: TraceSource::Memory(source),
                },
                0x1010,
                TraceExitReason::LoadGuard,
            ),
            (
                TraceOperation::Push {
                    source: TraceSource::Register(4),
                },
                0x100C,
                TraceExitReason::StoreBounds,
            ),
            (
                TraceOperation::Return { release: 8 },
                0x1008,
                TraceExitReason::LoadGuard,
            ),
        ] {
            let handle = compiler
                .compile_trace_profiled(&[TraceInstruction {
                    offset: 0,
                    length: 2,
                    operation,
                }])
                .unwrap();
            let mut regs = [0x1008, 1, 2, 3, esp, 5, 6, 7];
            let original = regs;
            let mut flags = 0xA203;
            let mut ip = 0x2000;
            let mut ram = [0xCC; 16];
            let mut generations = [7; 2];
            let result = compiler.execute_trace_writable_report_with_exclusion(
                handle,
                &mut regs,
                &mut flags,
                &mut ip,
                1,
                0x1000,
                &mut ram,
                &mut generations,
                0x2000,
                2,
                0x1008,
                4,
            );
            assert_eq!(
                result,
                TraceExecution {
                    steps: 0,
                    reason: expected
                }
            );
            assert_eq!(regs, original);
            assert_eq!(flags, 0xA203);
            assert_eq!(ip, 0x2000);
            assert_eq!(ram, [0xCC; 16]);
            assert_eq!(generations, [7; 2]);
        }
    }

    #[test]
    fn byte_compare_and_test_flags_match_all_operand_pairs() {
        let mut compiler = Compiler::new().unwrap();
        for operation in [4, 7] {
            let instruction = TraceInstruction {
                offset: 0,
                length: 2,
                operation: TraceOperation::Compare {
                    left: TraceSource::ByteRegister(4),
                    right: TraceSource::ByteRegister(7),
                    operation,
                    byte: true,
                },
            };
            let plain = compiler.compile_trace(&[instruction]).unwrap();
            let profiled = compiler.compile_trace_profiled(&[instruction]).unwrap();
            for left in 0u16..=255 {
                for right in 0u16..=255 {
                    let result = if operation == 4 {
                        (left & right) as u8
                    } else {
                        (left as u8).wrapping_sub(right as u8)
                    };
                    let mut expected = 0xA203u32 & !0x8D5 | 2;
                    if result == 0 {
                        expected |= 0x40;
                    }
                    if result & 0x80 != 0 {
                        expected |= 0x80;
                    }
                    if result.count_ones() % 2 == 0 {
                        expected |= 4;
                    }
                    if operation == 7 {
                        if left < right {
                            expected |= 1;
                        }
                        if left & 15 < right & 15 {
                            expected |= 0x10;
                        }
                        let signed = i16::from(left as u8 as i8) - i16::from(right as u8 as i8);
                        if !(-128..=127).contains(&signed) {
                            expected |= 0x800;
                        }
                    }
                    for handle in [plain, profiled] {
                        let mut regs = [0xCAFE0034; 8];
                        regs[0] |= u32::from(left) << 8;
                        regs[3] |= u32::from(right) << 8;
                        let original = regs;
                        let mut flags = 0xA203;
                        let mut ip = 0x2000;
                        assert_eq!(
                            compiler.execute_trace(
                                handle,
                                &mut regs,
                                &mut flags,
                                &mut ip,
                                1,
                                0,
                                &[]
                            ),
                            1
                        );
                        assert_eq!(
                            flags, expected,
                            "operation={operation} left={left} right={right}"
                        );
                        assert_eq!(regs, original);
                        assert_eq!(ip, 0x2002);
                    }
                }
            }
        }
    }

    #[test]
    fn byte_moves_preserve_aliases_and_access_only_one_byte() {
        let mut compiler = Compiler::new().unwrap();
        let address = |register| DwordMemoryAddress {
            base: Some(register),
            index: None,
            scale: 0,
            displacement: 0,
        };
        let instructions = [
            TraceInstruction {
                offset: 0,
                length: 2,
                operation: TraceOperation::MoveByte {
                    target: 4,
                    source: TraceSource::ByteMemory(address(1)),
                },
            },
            TraceInstruction {
                offset: 2,
                length: 2,
                operation: TraceOperation::StoreByte {
                    address: address(2),
                    source: TraceSource::ByteRegister(4),
                },
            },
            TraceInstruction {
                offset: 4,
                length: 2,
                operation: TraceOperation::MoveByte {
                    target: 2,
                    source: TraceSource::ByteRegister(4),
                },
            },
        ];
        for handle in [
            compiler.compile_trace(&instructions).unwrap(),
            compiler.compile_trace_profiled(&instructions).unwrap(),
        ] {
            for budget in 0..=3 {
                let mut regs = [0xCAFEAB12, 0x1000, 0x1001, 13, 14, 15, 16, 17];
                let mut flags = 0xA203;
                let mut ip = 0x2000;
                let mut ram = [0x81, 0xCC];
                let mut generations = [7; 2];
                assert_eq!(
                    compiler.execute_trace_writable(
                        handle,
                        &mut regs,
                        &mut flags,
                        &mut ip,
                        budget,
                        0x1000,
                        &mut ram,
                        &mut generations,
                        0x2000,
                        6
                    ),
                    budget
                );
                assert_eq!(regs[0], if budget > 0 { 0xCAFE8112 } else { 0xCAFEAB12 });
                assert_eq!(regs[2], if budget == 3 { 0x1081 } else { 0x1001 });
                assert_eq!(
                    ram,
                    if budget >= 2 {
                        [0x81, 0x81]
                    } else {
                        [0x81, 0xCC]
                    }
                );
                assert_eq!(generations, if budget >= 2 { [7, 8] } else { [7, 7] });
                assert_eq!(flags, 0xA203);
                assert_eq!(ip, 0x2000 + budget * 2);
            }
        }
    }

    #[test]
    fn lea_needs_no_ram_and_memory_left_compare_is_read_only() {
        let mut compiler = Compiler::new().unwrap();
        let address = DwordMemoryAddress {
            base: Some(4),
            index: Some(0),
            scale: 2,
            displacement: u32::MAX - 7,
        };
        let lea = compiler
            .compile_trace(&[TraceInstruction {
                offset: 0,
                length: 4,
                operation: TraceOperation::Lea { target: 2, address },
            }])
            .unwrap();
        let mut regs = [u32::MAX, 1, 2, 3, 8, 5, 6, 7];
        let mut flags = 0xA203;
        let mut ip = 0x2000;
        assert_eq!(
            compiler.execute_trace(lea, &mut regs, &mut flags, &mut ip, 1, 0, &[]),
            1
        );
        assert_eq!(regs[2], u32::MAX - 3);
        assert_eq!(flags, 0xA203);
        assert_eq!(ip, 0x2004);
        for byte in [false, true] {
            let source = if byte {
                TraceSource::ByteMemory(stack_address(0))
            } else {
                TraceSource::Memory(stack_address(0))
            };
            let right = if byte {
                TraceSource::ByteRegister(0)
            } else {
                TraceSource::Register(0)
            };
            let instructions = [TraceInstruction {
                offset: 0,
                length: 2,
                operation: TraceOperation::Compare {
                    left: source,
                    right,
                    operation: 7,
                    byte,
                },
            }];
            for handle in [
                compiler.compile_trace(&instructions).unwrap(),
                compiler.compile_trace_profiled(&instructions).unwrap(),
            ] {
                let mut regs = [1, 1, 2, 3, 0x1000, 5, 6, 7];
                let original = regs;
                let mut flags = 0xA203;
                let mut ip = 0x2000;
                assert_eq!(
                    compiler.execute_trace(
                        handle,
                        &mut regs,
                        &mut flags,
                        &mut ip,
                        1,
                        0x1000,
                        &[0, 0, 0, 0]
                    ),
                    1
                );
                assert_eq!(flags & 0x8D5, 0x95);
                assert_eq!(regs, original);
                assert_eq!(ip, 0x2002);
            }
        }
    }

    #[test]
    fn stack_aliases_and_call_return_stay_inside_native_cfg() {
        let mut compiler = Compiler::new().unwrap();
        let push_pop = [
            TraceInstruction {
                offset: 0,
                length: 1,
                operation: TraceOperation::Push {
                    source: TraceSource::Register(4),
                },
            },
            TraceInstruction {
                offset: 1,
                length: 1,
                operation: TraceOperation::Pop { register: 0 },
            },
        ];
        for handle in [
            compiler.compile_trace(&push_pop).unwrap(),
            compiler.compile_trace_profiled(&push_pop).unwrap(),
        ] {
            for budget in 0..=2 {
                let mut regs = [10, 11, 12, 13, 0x1020, 15, 16, 17];
                let mut flags = 0xA203;
                let mut ip = 0x2000;
                let mut ram = [0xCC; 32];
                let mut generations = [7; 2];
                assert_eq!(
                    compiler.execute_trace_writable(
                        handle,
                        &mut regs,
                        &mut flags,
                        &mut ip,
                        budget,
                        0x1000,
                        &mut ram,
                        &mut generations,
                        0x2000,
                        2
                    ),
                    budget
                );
                assert_eq!(regs[0], if budget == 2 { 0x1020 } else { 10 });
                assert_eq!(regs[4], if budget == 1 { 0x101C } else { 0x1020 });
                assert_eq!(flags, 0xA203);
                assert_eq!(ip, 0x2000 + budget);
                if budget > 0 {
                    assert_eq!(&ram[28..], &0x1020u32.to_le_bytes());
                    assert_eq!(generations, [7, 8]);
                }
            }
        }
        let pop_esp = compiler
            .compile_trace(&[TraceInstruction {
                offset: 0,
                length: 1,
                operation: TraceOperation::Pop { register: 4 },
            }])
            .unwrap();
        let mut regs = [0; 8];
        regs[4] = 0x1000;
        let mut flags = 0x203;
        let mut ip = 0x2000;
        assert_eq!(
            compiler.execute_trace(
                pop_esp,
                &mut regs,
                &mut flags,
                &mut ip,
                1,
                0x1000,
                &0xCAFEBABEu32.to_le_bytes()
            ),
            1
        );
        assert_eq!(regs[4], 0xCAFEBABE);
        // CALL pushes an internal return point; RET finds it in the same native
        // CFG, and the returned-to branch exits externally after three steps.
        let direct = [
            TraceInstruction {
                offset: 0,
                length: 5,
                operation: TraceOperation::Call { target: 7 },
            },
            TraceInstruction {
                offset: 5,
                length: 2,
                operation: TraceOperation::Branch {
                    condition: None,
                    target: 20,
                },
            },
            TraceInstruction {
                offset: 7,
                length: 1,
                operation: TraceOperation::Return { release: 0 },
            },
        ];
        let indirect = [
            TraceInstruction {
                operation: TraceOperation::CallIndirect {
                    target: TraceSource::Register(0),
                },
                ..direct[0]
            },
            direct[1],
            direct[2],
        ];
        for instructions in [direct, indirect] {
            for handle in [
                compiler.compile_trace(&instructions).unwrap(),
                compiler.compile_trace_profiled(&instructions).unwrap(),
            ] {
                for budget in 0..=4 {
                    let mut regs = [0x2007, 11, 12, 13, 0x1020, 15, 16, 17];
                    let mut flags = 0xA203;
                    let mut ip = 0x2000;
                    let mut ram = [0xCC; 32];
                    let mut generations = [7; 2];
                    assert_eq!(
                        compiler.execute_trace_writable(
                            handle,
                            &mut regs,
                            &mut flags,
                            &mut ip,
                            budget,
                            0x1000,
                            &mut ram,
                            &mut generations,
                            0x2000,
                            8
                        ),
                        budget.min(3)
                    );
                    assert_eq!(regs[4], if budget == 1 { 0x101C } else { 0x1020 });
                    assert_eq!(
                        ip,
                        match budget {
                            0 => 0x2000,
                            1 => 0x2007,
                            2 => 0x2005,
                            _ => 0x2014,
                        }
                    );
                    assert_eq!(flags, 0xA203);
                    if budget > 0 {
                        assert_eq!(&ram[28..], &0x2005u32.to_le_bytes());
                        assert_eq!(generations, [7, 8]);
                    }
                }
            }
        }
    }

    #[test]
    fn stack_guards_and_indirect_source_fail_before_any_stack_effect() {
        let mut compiler = Compiler::new().unwrap();
        for operation in [
            TraceOperation::Push {
                source: TraceSource::Register(4),
            },
            TraceOperation::Call { target: 0x1234 },
            TraceOperation::CallIndirect {
                target: TraceSource::Register(4),
            },
            TraceOperation::CallIndirect {
                target: TraceSource::Memory(DwordMemoryAddress {
                    base: Some(0),
                    index: None,
                    scale: 0,
                    displacement: 0,
                }),
            },
        ] {
            let instructions = [TraceInstruction {
                offset: 0,
                length: 2,
                operation,
            }];
            for handle in [
                compiler.compile_trace(&instructions).unwrap(),
                compiler.compile_trace_profiled(&instructions).unwrap(),
            ] {
                for (esp, source, exhausted, protected_start, protected_len) in [
                    (0x1002, 0x1000, false, 0x2000, 2),
                    (0x1008, 0x1000, true, 0x2000, 2),
                    (0x1008, 0x1000, false, 0x1004, 4),
                    (0x1008, 0xFFFF, false, 0x2000, 2),
                ] {
                    let mut regs = [source, 1, 2, 3, esp, 5, 6, 7];
                    let before = regs;
                    let mut flags = 0xA203;
                    let mut ip = 0x2000;
                    let mut ram = [0xCC; 8];
                    let mut generations = [7, if exhausted { u64::MAX } else { 7 }];
                    let original_generations = generations;
                    let source_failure = source == 0xFFFF
                        && matches!(
                            operation,
                            TraceOperation::CallIndirect {
                                target: TraceSource::Memory(_)
                            }
                        );
                    let should_fail =
                        esp == 0x1002 || exhausted || protected_start == 0x1004 || source_failure;
                    let result = compiler.execute_trace_writable(
                        handle,
                        &mut regs,
                        &mut flags,
                        &mut ip,
                        1,
                        0x1000,
                        &mut ram,
                        &mut generations,
                        protected_start,
                        protected_len,
                    );
                    assert_eq!(result, u32::from(!should_fail));
                    if should_fail {
                        assert_eq!(regs, before);
                        assert_eq!(flags, 0xA203);
                        assert_eq!(ip, 0x2000);
                        assert_eq!(ram, [0xCC; 8]);
                        assert_eq!(generations, original_generations);
                    }
                }
            }
        }
        for release in [0, 12, u16::MAX] {
            let instructions = [TraceInstruction {
                offset: 0,
                length: 1,
                operation: TraceOperation::Return { release },
            }];
            for handle in [
                compiler.compile_trace(&instructions).unwrap(),
                compiler.compile_trace_profiled(&instructions).unwrap(),
            ] {
                let mut regs = [0; 8];
                regs[4] = 0x1000;
                let mut flags = 0x203;
                let mut ip = 0x2000;
                assert_eq!(
                    compiler.execute_trace(
                        handle,
                        &mut regs,
                        &mut flags,
                        &mut ip,
                        1,
                        0x1000,
                        &0xCAFEBABEu32.to_le_bytes()
                    ),
                    1
                );
                assert_eq!(regs[4], 0x1004 + u32::from(release));
                assert_eq!(ip, 0xCAFEBABE);
                assert_eq!(flags, 0x203);
                regs[4] = 0x1001;
                ip = 0x2000;
                assert_eq!(
                    compiler
                        .execute_trace(handle, &mut regs, &mut flags, &mut ip, 1, 0x1000, &[0; 4]),
                    0
                );
                assert_eq!(regs[4], 0x1001);
                assert_eq!(ip, 0x2000);
            }
        }
    }

    #[test]
    fn profiled_readonly_exits_report_reason_with_identical_state() {
        let mut compiler = Compiler::new().unwrap();
        let instructions = [TraceInstruction {
            offset: 0,
            length: 2,
            operation: TraceOperation::Move {
                target: 0,
                source: TraceSource::Memory(DwordMemoryAddress {
                    base: Some(1),
                    index: None,
                    scale: 0,
                    displacement: 0,
                }),
            },
        }];
        let plain = compiler.compile_trace(&instructions).unwrap();
        let profiled = compiler.compile_trace_profiled(&instructions).unwrap();
        for (budget, address, reason, steps) in [
            (0, 0x1001, TraceExitReason::Budget, 0),
            (1, 0x1001, TraceExitReason::ExternalEdge, 1),
            (1, 0x1002, TraceExitReason::LoadGuard, 0),
        ] {
            let mut plain_regs = [10, address, 12, 13, 14, 15, 16, 17];
            let mut profiled_regs = plain_regs;
            let mut plain_flags = 0x203;
            let mut profiled_flags = plain_flags;
            let mut plain_ip = 0x2000;
            let mut profiled_ip = plain_ip;
            let ram = [0xCC, 0x78, 0x56, 0x34, 0x12];
            let reference = compiler.execute_trace_report(
                plain,
                &mut plain_regs,
                &mut plain_flags,
                &mut plain_ip,
                budget,
                0x1000,
                &ram,
            );
            let actual = compiler.execute_trace_report(
                profiled,
                &mut profiled_regs,
                &mut profiled_flags,
                &mut profiled_ip,
                budget,
                0x1000,
                &ram,
            );
            assert_eq!(
                reference,
                TraceExecution {
                    steps,
                    reason: TraceExitReason::Uninstrumented
                }
            );
            assert_eq!(actual, TraceExecution { steps, reason });
            assert_eq!(plain_regs, profiled_regs);
            assert_eq!(plain_flags, profiled_flags);
            assert_eq!(plain_ip, profiled_ip);
        }
        // The legacy steps-only entry point also accepts profiled handles.
        let mut regs = [0; 8];
        regs[1] = 0x1000;
        let mut flags = 0x203;
        let mut ip = 0x2000;
        assert_eq!(
            compiler.execute_trace(
                profiled,
                &mut regs,
                &mut flags,
                &mut ip,
                1,
                0x1000,
                &[1, 2, 3, 4]
            ),
            1
        );
        assert_eq!(regs[0], 0x04030201);
        assert_eq!(ip, 0x2002);
    }

    #[test]
    fn profiled_store_exits_distinguish_guards_without_changing_state() {
        let mut compiler = Compiler::new().unwrap();
        let instruction = TraceInstruction {
            offset: 0,
            length: 2,
            operation: TraceOperation::Store {
                address: DwordMemoryAddress {
                    base: Some(1),
                    index: None,
                    scale: 0,
                    displacement: 0,
                },
                source: TraceSource::Immediate(0x12345678),
            },
        };
        let plain = compiler.compile_trace(&[instruction]).unwrap();
        let profiled = compiler.compile_trace_profiled(&[instruction]).unwrap();
        for (
            budget,
            address,
            generation_len,
            exhausted,
            protected_start,
            protected_len,
            reason,
            steps,
        ) in [
            (0, 0x1001, 2, false, 0x2000, 2, TraceExitReason::Budget, 0),
            (
                1,
                0x1001,
                2,
                false,
                0x2000,
                2,
                TraceExitReason::ExternalEdge,
                1,
            ),
            (
                1,
                0x1002,
                2,
                false,
                0x2000,
                2,
                TraceExitReason::StoreBounds,
                0,
            ),
            (
                1,
                0x1001,
                1,
                false,
                0x2000,
                2,
                TraceExitReason::GenerationBounds,
                0,
            ),
            (
                1,
                0x1001,
                2,
                false,
                0x1003,
                2,
                TraceExitReason::ActiveCode,
                0,
            ),
            (
                1,
                0x1001,
                2,
                true,
                0x2000,
                2,
                TraceExitReason::GenerationExhausted,
                0,
            ),
        ] {
            let mut plain_regs = [10, address, 12, 13, 14, 15, 16, 17];
            let mut profiled_regs = plain_regs;
            let mut plain_flags = 0x203;
            let mut profiled_flags = plain_flags;
            let mut plain_ip = 0x2000;
            let mut profiled_ip = plain_ip;
            let mut plain_ram = [0xCC; 5];
            let mut profiled_ram = plain_ram;
            let mut plain_generations = [7, if exhausted { u64::MAX } else { 7 }];
            let mut profiled_generations = plain_generations;
            let reference = compiler.execute_trace_writable_report(
                plain,
                &mut plain_regs,
                &mut plain_flags,
                &mut plain_ip,
                budget,
                0x1000,
                &mut plain_ram,
                &mut plain_generations[..generation_len],
                protected_start,
                protected_len,
            );
            let actual = compiler.execute_trace_writable_report(
                profiled,
                &mut profiled_regs,
                &mut profiled_flags,
                &mut profiled_ip,
                budget,
                0x1000,
                &mut profiled_ram,
                &mut profiled_generations[..generation_len],
                protected_start,
                protected_len,
            );
            assert_eq!(
                reference,
                TraceExecution {
                    steps,
                    reason: TraceExitReason::Uninstrumented
                }
            );
            assert_eq!(actual, TraceExecution { steps, reason });
            assert_eq!(plain_regs, profiled_regs);
            assert_eq!(plain_flags, profiled_flags);
            assert_eq!(plain_ip, profiled_ip);
            assert_eq!(plain_ram, profiled_ram);
            assert_eq!(plain_generations, profiled_generations);
            // A read-only call rejects a writable profiled function before entry.
            let before = (profiled_regs, profiled_flags, profiled_ip);
            assert_eq!(
                compiler.execute_trace(
                    profiled,
                    &mut profiled_regs,
                    &mut profiled_flags,
                    &mut profiled_ip,
                    1,
                    0x1000,
                    &profiled_ram
                ),
                0
            );
            assert_eq!((profiled_regs, profiled_flags, profiled_ip), before);
        }
        let looped = compiler
            .compile_trace_profiled(&[
                instruction,
                TraceInstruction {
                    offset: 2,
                    length: 2,
                    operation: TraceOperation::Branch {
                        condition: None,
                        target: 0,
                    },
                },
            ])
            .unwrap();
        let mut regs = [0; 8];
        regs[1] = 0x1000;
        let mut flags = 0x203;
        let mut ip = 0x2000;
        let mut ram = [0; 4];
        let mut generations = [7; 2];
        let result = compiler.execute_trace_writable_report(
            looped,
            &mut regs,
            &mut flags,
            &mut ip,
            3,
            0x1000,
            &mut ram,
            &mut generations,
            0x2000,
            4,
        );
        assert_eq!(
            result,
            TraceExecution {
                steps: 3,
                reason: TraceExitReason::Budget
            }
        );
        assert_eq!(ip, 0x2002);
        assert_eq!(generations, [7, 9]);
        // Legacy steps-only writable invocation keeps working with profile handles.
        ip = 0x2000;
        assert_eq!(
            compiler.execute_trace_writable(
                profiled,
                &mut regs,
                &mut flags,
                &mut ip,
                1,
                0x1000,
                &mut ram,
                &mut generations,
                0x2000,
                2
            ),
            1
        );
        assert_eq!(ip, 0x2002);
    }

    #[test]
    fn writable_trace_checks_generations_code_protection_and_budget_before_stores() {
        let mut compiler = Compiler::new().unwrap();
        let address = DwordMemoryAddress {
            base: Some(1),
            index: None,
            scale: 0,
            displacement: 0,
        };
        let store = TraceInstruction {
            offset: 0,
            length: 2,
            operation: TraceOperation::Store {
                address,
                source: TraceSource::Register(0),
            },
        };
        let trace = compiler
            .compile_trace(&[
                store,
                TraceInstruction {
                    offset: 2,
                    length: 2,
                    operation: TraceOperation::Branch {
                        condition: None,
                        target: 0,
                    },
                },
            ])
            .unwrap();
        let mut ram = vec![0xCC; 8192];
        for (address, protected_start, protected_len, expected) in [
            (0x2001, 0x1000, 4, true),        // unaligned single page
            (0x2FFE, 0x1000, 4, true),        // two pages
            (0x2FFE, 0x3000, 2, false),       // overlaps protected bytes
            (0x2FFC, 0x3000, 2, true),        // ends just before protection
            (0x3002, 0x3000, 2, true),        // starts just after protection
            (0x4000, 0x1000, 4, false),       // outside slice
            (0x3FFE, 0x1000, 4, false),       // partial dword
            (0x1FFF, 0x1000, 4, false),       // below slice
            (u32::MAX - 2, 0x1000, 4, false), // address end overflow
        ] {
            for budget in 0..=5 {
                ram.fill(0xCC);
                let mut generations = [7u64; 4];
                let mut regs = [0xAABBCCDD, address, 12, 13, 14, 15, 16, 17];
                let before = regs;
                let mut flags = 0x203;
                let mut ip = 0x1000;
                let completed = compiler.execute_trace_writable(
                    trace,
                    &mut regs,
                    &mut flags,
                    &mut ip,
                    budget,
                    0x2000,
                    &mut ram,
                    &mut generations,
                    protected_start,
                    protected_len,
                );
                assert_eq!(completed, if expected { budget } else { 0 });
                assert_eq!(regs, before);
                assert_eq!(flags, 0x203);
                assert_eq!(ip, 0x1000 + if expected { budget % 2 * 2 } else { 0 });
                let writes = if expected { budget.div_ceil(2) } else { 0 };
                let mut expected_generations = [7u64; 4];
                if writes > 0 {
                    expected_generations[(address >> 12) as usize] += u64::from(writes);
                    if address >> 12 != (address + 3) >> 12 {
                        expected_generations[((address + 3) >> 12) as usize] += u64::from(writes);
                    }
                    let offset = (address - 0x2000) as usize;
                    assert_eq!(&ram[offset..offset + 4], &0xAABBCCDDu32.to_le_bytes());
                    assert!(
                        ram[..offset]
                            .iter()
                            .chain(&ram[offset + 4..])
                            .all(|&v| v == 0xCC)
                    );
                } else {
                    assert!(ram.iter().all(|&v| v == 0xCC));
                }
                assert_eq!(generations, expected_generations);
            }
        }
        for exhausted in [2, 3] {
            ram.fill(0xCC);
            let mut generations = [7u64; 4];
            generations[exhausted] = u64::MAX;
            let before = generations;
            let mut regs = [0; 8];
            regs[1] = 0x2FFE;
            let mut flags = 0x203;
            let mut ip = 0x1000;
            assert_eq!(
                compiler.execute_trace_writable(
                    trace,
                    &mut regs,
                    &mut flags,
                    &mut ip,
                    256,
                    0x2000,
                    &mut ram,
                    &mut generations,
                    0x1000,
                    4
                ),
                0
            );
            assert_eq!(generations, before);
            assert!(ram.iter().all(|&v| v == 0xCC));
            assert_eq!(flags, 0x203);
            assert_eq!(ip, 0x1000);
        }
        let mut regs = [0; 8];
        regs[1] = 0x2FFE;
        let mut flags = 0x203;
        let mut ip = 0x1000;
        let mut generations = [7u64; 3];
        assert_eq!(
            compiler.execute_trace_writable(
                trace,
                &mut regs,
                &mut flags,
                &mut ip,
                256,
                0x2000,
                &mut ram,
                &mut generations,
                0x1000,
                4
            ),
            0
        );
        assert_eq!(generations, [7; 3]);
        // The read-only API must never execute even the prefix of a store trace.
        let before = regs;
        assert_eq!(
            compiler.execute_trace(trace, &mut regs, &mut flags, &mut ip, 256, 0x2000, &ram),
            0
        );
        assert_eq!(regs, before);
        assert_eq!(flags, 0x203);
        assert_eq!(ip, 0x1000);
        assert!(
            compiler
                .compile_trace(&[TraceInstruction {
                    operation: TraceOperation::Store {
                        address,
                        source: TraceSource::Memory(address)
                    },
                    ..store
                }])
                .is_err()
        );
    }

    #[test]
    fn store_failure_preserves_completed_prefix_and_immediate_store_is_little_endian() {
        let mut compiler = Compiler::new().unwrap();
        let trace = compiler
            .compile_trace(&[
                TraceInstruction {
                    offset: 0,
                    length: 5,
                    operation: TraceOperation::Move {
                        target: 0,
                        source: TraceSource::Immediate(42),
                    },
                },
                TraceInstruction {
                    offset: 5,
                    length: 6,
                    operation: TraceOperation::Store {
                        address: DwordMemoryAddress {
                            base: None,
                            index: None,
                            scale: 0,
                            displacement: 0x1001,
                        },
                        source: TraceSource::Immediate(0x12345678),
                    },
                },
            ])
            .unwrap();
        for fits in [false, true] {
            let mut regs = [0; 8];
            let mut flags = 0x203;
            let mut ip = 0x2000;
            let mut ram = [0xCC; 5];
            let mut generations = [7u64; 2];
            let len = if fits { 5 } else { 4 };
            assert_eq!(
                compiler.execute_trace_writable(
                    trace,
                    &mut regs,
                    &mut flags,
                    &mut ip,
                    2,
                    0x1000,
                    &mut ram[..len],
                    &mut generations,
                    0x2000,
                    11
                ),
                if fits { 2 } else { 1 }
            );
            assert_eq!(regs[0], 42);
            assert_eq!(flags, 0x203);
            assert_eq!(ip, if fits { 0x200B } else { 0x2005 });
            assert_eq!(
                ram,
                if fits {
                    [0xCC, 0x78, 0x56, 0x34, 0x12]
                } else {
                    [0xCC; 5]
                }
            );
            assert_eq!(generations, if fits { [7, 8] } else { [7, 7] });
        }
    }

    #[test]
    fn shared_exit_preserves_registers_when_only_the_other_branch_writes() {
        let mut compiler = Compiler::new().unwrap();
        let trace = compiler
            .compile_trace(&[
                TraceInstruction {
                    offset: 0,
                    length: 2,
                    operation: TraceOperation::Branch {
                        condition: Some(4),
                        target: 6,
                    },
                },
                TraceInstruction {
                    offset: 2,
                    length: 4,
                    operation: TraceOperation::Move {
                        target: 0,
                        source: TraceSource::Immediate(99),
                    },
                },
            ])
            .unwrap();
        for taken in [false, true] {
            for budget in 0..=2 {
                let original = [10, 11, 12, 13, 14, 15, 16, 17];
                let mut regs = original;
                let mut flags = if taken { 0x242 } else { 0x202 };
                let expected_flags = flags;
                let mut ip = 0x1000;
                let completed =
                    compiler.execute_trace(trace, &mut regs, &mut flags, &mut ip, budget, 0, &[]);
                assert_eq!(completed, if taken { budget.min(1) } else { budget });
                let mut expected = original;
                if !taken && budget == 2 {
                    expected[0] = 99;
                }
                assert_eq!(regs, expected);
                assert_eq!(flags, expected_flags);
                assert_eq!(
                    ip,
                    0x1000
                        + match (taken, budget) {
                            (_, 0) => 0,
                            (false, 1) => 2,
                            _ => 6,
                        }
                );
            }
        }
    }

    #[test]
    fn trace_rejects_bad_ir_and_bounds_unconditional_backedges() {
        let mut compiler = Compiler::new().unwrap();
        let branch = TraceInstruction {
            offset: 0,
            length: 2,
            operation: TraceOperation::Branch {
                condition: None,
                target: 0,
            },
        };
        let (trace, diagnostic) = compiler.compile_trace_diagnostic(&[branch]).unwrap();
        assert!(!diagnostic.code.is_empty());
        assert!(diagnostic.ir.contains("function"));
        let mut regs = [42; 8];
        let mut flags = 0x203;
        let mut ip = 0xCAFE;
        assert_eq!(
            compiler.execute_trace(trace, &mut regs, &mut flags, &mut ip, u32::MAX, 0, &[]),
            256
        );
        assert_eq!(regs, [42; 8]);
        assert_eq!(flags, 0x203);
        assert_eq!(ip, 0xCAFE);
        assert!(compiler.compile_trace(&[]).is_err());
        for invalid in [
            TraceInstruction {
                length: 0,
                ..branch
            },
            TraceInstruction {
                offset: 1,
                ..branch
            },
            TraceInstruction {
                operation: TraceOperation::Branch {
                    condition: Some(16),
                    target: 0,
                },
                ..branch
            },
            TraceInstruction {
                operation: TraceOperation::Branch {
                    condition: None,
                    target: 1,
                },
                ..branch
            },
        ] {
            assert!(compiler.compile_trace(&[invalid]).is_err());
        }
    }
}
