extern crate std;

use super::*;
use crate::static_program::{Frame, abi};
use alloc::{vec, vec::Vec};
use memory::{
    Descriptor, DeviceReadDescriptor, DeviceReadKind, DeviceWriteDescriptor, RamDescriptor,
    SegmentBounds, StackAddress,
};
use wasmi::{Engine, Linker, Memory, MemoryType, Module, Store, TypedFunc};

const FRAME: usize = 256;
const RAM: usize = 4096;
const VIDEO: usize = 65536;
const VIDEO_LENGTH: usize = 262144;
const RAM_BASE: u32 = 0x500;
const RAM_LENGTH: u32 = 0x4000;
const GENERATIONS: u32 = 24576;
const PROTECTED: u32 = 25088;
const ID: ProcedureId = ProcedureId(3);

fn at(offset: u32) -> Address {
    Address {
        space: 7,
        offset: 0x8000 + offset,
    }
}

fn block(
    offset: u32,
    operations: Vec<Operation>,
    terminator: u32,
    kind: ProcedureTerminatorKind,
) -> ProcedureBlock {
    ProcedureBlock {
        entry: at(offset),
        instructions: operations
            .into_iter()
            .enumerate()
            .map(|(i, operation)| ProcedureInstruction {
                address: at(offset + i as u32),
                operation,
            })
            .collect(),
        terminator: ProcedureTerminator {
            address: at(terminator),
            kind,
        },
    }
}

fn program() -> ProcedureGraphProgram {
    use ProcedureTerminatorKind::{Branch, Fallthrough, Return};
    ProcedureGraphProgram {
        functions: vec![ProcedureGraphFunction {
            id: ID,
            entry: at(0),
            blocks: vec![
                block(
                    0,
                    vec![
                        Operation::Move {
                            target: Register::Ebx,
                            source: Source::Register(Register::Ecx),
                        },
                        Operation::Shift {
                            target: Register::Ebx,
                            kind: Shift::Shr,
                            count: ShiftCount::One,
                        },
                    ],
                    2,
                    Branch {
                        condition: Condition::Equal,
                        taken: at(40),
                        not_taken: at(3),
                    },
                ),
                block(3, vec![Operation::Nop; 4], 7, Fallthrough(at(7))),
                block(
                    7,
                    vec![
                        Operation::LoadMemory {
                            target: Register::Ax,
                            address: address(Register::Esi, 0),
                        },
                        Operation::StoreMemory {
                            width: Width::Byte,
                            source: Source::Register(Register::Al),
                            address: address(Register::Edi, 0),
                        },
                        alu(Register::Esi, Alu::Add, 2),
                        alu(Register::Edi, Alu::Add, 192),
                        Operation::StoreMemory {
                            width: Width::Byte,
                            source: Source::Register(Register::Ah),
                            address: address(Register::Edi, (-96i32) as u32),
                        },
                        Operation::Dec(Register::Ebx),
                    ],
                    13,
                    Branch {
                        condition: Condition::NotEqual,
                        taken: at(7),
                        not_taken: at(14),
                    },
                ),
                block(
                    14,
                    vec![alu(Register::Ecx, Alu::And, 1)],
                    15,
                    Branch {
                        condition: Condition::Equal,
                        taken: at(42),
                        not_taken: at(16),
                    },
                ),
                block(16, vec![Operation::Nop; 4], 40, Fallthrough(at(40))),
                block(
                    40,
                    vec![
                        Operation::LoadMemory {
                            target: Register::Al,
                            address: address(Register::Esi, 0),
                        },
                        Operation::StoreMemory {
                            width: Width::Byte,
                            source: Source::Register(Register::Al),
                            address: address(Register::Edi, 0),
                        },
                    ],
                    42,
                    Fallthrough(at(42)),
                ),
                block(
                    42,
                    vec![],
                    42,
                    Return {
                        width: Width::Dword,
                        pop_bytes: 0,
                    },
                ),
            ],
        }],
    }
}

fn descriptor(mask: u8) -> Descriptor {
    Descriptor {
        ram: RamDescriptor {
            pointer: RAM as u32,
            linear_base: RAM_BASE,
            length: RAM_LENGTH,
            generations_pointer: GENERATIONS,
            generations_length: 32,
            code_pages_pointer: PROTECTED,
            code_pages_length: 32,
            ..RamDescriptor::default()
        },
        stack_address: StackAddress::Bits32,
        device: DeviceReadDescriptor {
            guest_start: 0xa0000,
            guest_length: 65536,
            pointer: VIDEO as u32,
            backing_length: VIDEO_LENGTH as u32,
            kind: DeviceReadKind::FourLanes,
            latches: [0x15, 0x26, 0x37, 0x48],
            ..DeviceReadDescriptor::default()
        },
        device_write: DeviceWriteDescriptor {
            enabled: true,
            plane_mask: mask,
            bit_mask: 255,
            ..DeviceWriteDescriptor::default()
        },
        windows: [[SegmentBounds {
            valid: true,
            base: 0,
            first: 0,
            last: u32::MAX,
        }; 6]; 2],
        inline_grant: MAX_WORK_PER_CALL,
        ..Descriptor::default()
    }
}

fn frame(count: u32) -> Frame {
    Frame {
        gpr: [
            0x1234_5678,
            count,
            0x3141_5926,
            0x9988_7766,
            0x2400,
            0xaabb_ccdd,
            0x1300,
            0xa0000 + 0x123,
        ],
        eflags: 0xad7,
        address: at(0),
        charged_work: 39,
        remaining_budget: 987,
        status: 0x8000_0000,
    }
}

struct Runner {
    store: Store<()>,
    memory: Memory,
    run: TypedFunc<u32, u32>,
}

impl Runner {
    fn new(compiled: &CompiledProcedureProgram) -> Self {
        let engine = Engine::default();
        let module = Module::new(&engine, compiled.wasm()).unwrap();
        assert_eq!(
            module.imports().count(),
            1,
            "only the common memory may be imported"
        );
        let mut store = Store::new(&engine, ());
        let memory = Memory::new(&mut store, MemoryType::new(10, Some(10))).unwrap();
        let mut linker = Linker::new(&engine);
        linker
            .define(abi::IMPORT_MODULE, abi::MEMORY_IMPORT, memory)
            .unwrap();
        let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
        let run = instance
            .get_typed_func(&store, compiled.export_name(ID).unwrap())
            .unwrap();
        Self { store, memory, run }
    }

    fn reset(&mut self, frame: Frame, descriptor: &Descriptor) {
        let bytes = self.memory.data_mut(&mut self.store);
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(7) ^ 3;
        }
        frame.write(&mut bytes[FRAME..]).unwrap();
        descriptor
            .write(&mut bytes[FRAME + memory::OFFSET as usize..])
            .unwrap();
        let stack = RAM
            + (frame.gpr[4]
                & if descriptor.stack_address == StackAddress::Bits16 {
                    65535
                } else {
                    u32::MAX
                })
            .wrapping_sub(RAM_BASE) as usize;
        if let Some(stack) = bytes.get_mut(stack..stack + 4) {
            stack.copy_from_slice(&0x9876_5432u32.to_le_bytes());
        }
    }

    fn bytes(&self) -> &[u8] {
        self.memory.data(&self.store)
    }
    fn call(&mut self) -> u32 {
        self.run.call(&mut self.store, FRAME as u32).unwrap()
    }
    fn frame(&self) -> Frame {
        Frame::read(&self.bytes()[FRAME..]).unwrap()
    }
    fn effects(&self) -> memory::Effects {
        memory::Effects::read(&self.bytes()[FRAME + memory::OFFSET as usize..]).unwrap()
    }
}

#[test]
fn computation_api_keeps_semantic_copy_priority_and_ordinary_work_bound() {
    let program = program();
    // The generic finite proof is also applicable, but semantic copy keeps its
    // existing bounded metadata and byte-for-byte generated implementation.
    assert!(program.functions[0].finite_maximum_work().is_ok());
    let options = ProcedureCompileOptions::default();
    for limit in [710, MAX_WORK_PER_CALL] {
        let copy = program.compile_with_copy_kernels(&options, limit).unwrap();
        let computation = program
            .compile_with_computation_kernels(&options, limit)
            .unwrap();
        let kernel = computation.semantic_kernel(ID).unwrap();
        assert_eq!(kernel.max_work_per_call, limit);
        assert_eq!(
            computation.maximum_work(ID),
            Some(u64::from(kernel.max_work_per_call))
        );
        assert_eq!(computation.finite_maximum_work(ID), None);
        assert_eq!(computation.finite_loop_guards(ID), None);
        assert_eq!(computation.wasm(), copy.wasm());
    }
}

#[test]
fn copy_kernel_matches_bounded_procedure_for_counts_masks_and_stack_widths() {
    let program = program();
    let options = ProcedureCompileOptions::default();
    let compiled = program
        .compile_with_copy_kernels(&options, MAX_WORK_PER_CALL)
        .unwrap();
    let reference = program
        .compile_with_work_limit(&options, MAX_WORK_PER_CALL)
        .unwrap();
    let kernel = compiled.semantic_kernel(ID).unwrap();
    assert_eq!(
        kernel,
        CopyKernel {
            stride: 96,
            max_work_per_call: 4096,
            prefix_nops: 4,
            tail_nops: 4
        }
    );
    assert!(reference.semantic_kernel(ID).is_none());
    assert!(compiled.wasm().len() < reference.wasm().len());
    let mut optimized = Runner::new(&compiled);
    let mut ordinary = Runner::new(&reference);
    for count in [1, 2, 3, 200] {
        for mask in 0..=15 {
            for stack in [StackAddress::Bits16, StackAddress::Bits32] {
                let mut d = descriptor(mask);
                d.stack_address = stack;
                let mut input = frame(count);
                if stack == StackAddress::Bits16 {
                    input.gpr[4] |= 0xdead_0000;
                }
                optimized.reset(input, &d);
                ordinary.reset(input, &d);
                assert_eq!(
                    optimized.call(),
                    COMPLETE,
                    "count={count}, mask={mask}, stack={stack:?}"
                );
                assert_eq!(ordinary.call(), COMPLETE);
                assert_eq!(
                    optimized.bytes(),
                    ordinary.bytes(),
                    "count={count}, mask={mask}, stack={stack:?}"
                );
                assert_eq!(
                    optimized.frame().charged_work - input.charged_work,
                    u64::from(kernel.actual_work(count).unwrap())
                );
                assert_eq!(optimized.effects().device_writes, u64::from(count));
                assert_eq!(optimized.effects().device_reads, 0);
                assert_eq!(optimized.effects().ram_writes, 0);
                assert_eq!(optimized.effects().latches, d.device.latches);
                assert_eq!(optimized.effects().inline_remaining, MAX_WORK_PER_CALL);
                assert_eq!(optimized.effects().inline_reserved, 0);
            }
        }
    }
}

#[test]
fn copy_kernel_declines_atomically_for_full_spans_controls_layout_and_ret() {
    let compiled = program()
        .compile_with_copy_kernels(&ProcedureCompileOptions::default(), MAX_WORK_PER_CALL)
        .unwrap();
    let mut runner = Runner::new(&compiled);
    for refusal in 0..30 {
        let mut input = frame(200);
        let mut d = descriptor(15);
        match refusal {
            0 => input.gpr[1] = 0,
            1 => input.gpr[1] = u32::MAX,
            2 => d.inline_grant = 709,
            3 => input.gpr[6] = RAM_BASE + RAM_LENGTH - 199,
            4 => input.gpr[6] = u32::MAX - 198,
            5 => d.ram.excluded_start = input.gpr[6] + 100,
            6 => d.windows[0][memory::Segment::Ds as usize].last = input.gpr[6] + 198,
            7 => d.windows[0][memory::Segment::Ds as usize].base = u32::MAX,
            8 => input.gpr[7] = d.device.guest_start + d.device.guest_length - 199 * 96,
            9 => input.gpr[7] = u32::MAX - 199 * 96 + 1,
            10 => d.windows[1][memory::Segment::Ds as usize].last = input.gpr[7] + 199 * 96 - 1,
            11 => d.device.backing_length = (0x123 + 199 * 96 + 1) * 4 - 1,
            12 => d.device.kind = DeviceReadKind::Linear,
            13 => d.device.chain4 = true,
            14 => d.device_write.enabled = false,
            15 => d.device_write.mode = 1,
            16 => d.device_write.enable_set_reset = 1,
            17 => d.device_write.rotate = 1,
            18 => d.device_write.bit_mask = 254,
            19 => d.device_write.plane_mask = 16,
            20 => d.device.pointer = RAM as u32,
            21 => d.device.pointer = FRAME as u32,
            22 => d.device.pointer = GENERATIONS,
            23 => d.device.pointer = PROTECTED,
            24 => d.windows[0][memory::Segment::Ss as usize].last = input.gpr[4] + 2,
            25 => d.stack_address = StackAddress::Disabled,
            26 => d.ram.pointer = 655360 - RAM_LENGTH + 1,
            27 => d.device.pointer = 655360 - VIDEO_LENGTH as u32 + 1,
            28 => d.windows[1][memory::Segment::Ds as usize].valid = false,
            29 => d.windows[0][memory::Segment::Ds as usize].valid = false,
            _ => unreachable!(),
        }
        if refusal == 5 {
            d.ram.excluded_length = 1;
        }
        runner.reset(input, &d);
        let before = runner.bytes().to_vec();
        assert_eq!(runner.call(), SIDE_EXIT, "refusal={refusal}");
        assert_eq!(runner.bytes(), before, "refusal={refusal}");
    }
}

#[test]
fn copy_kernel_exact_range_ends_and_work_limit_are_accepted() {
    let compiled = program()
        .compile_with_copy_kernels(&ProcedureCompileOptions::default(), 710)
        .unwrap();
    let mut runner = Runner::new(&compiled);
    let mut d = descriptor(15);
    d.inline_grant = 710;
    let mut input = frame(200);
    input.gpr[6] = RAM_BASE + RAM_LENGTH - 200;
    input.gpr[7] = d.device.guest_start + d.device.guest_length - 1 - 199 * 96;
    d.windows[0][memory::Segment::Ds as usize].last = input.gpr[6] + 199;
    d.windows[1][memory::Segment::Ds as usize].last = input.gpr[7] + 199 * 96;
    runner.reset(input, &d);
    assert_eq!(runner.call(), COMPLETE);
    assert_eq!(runner.frame().charged_work, input.charged_work + 710);
}

#[test]
fn copy_kernel_pattern_near_misses_keep_ordinary_lowering_and_metadata_is_bounded() {
    let options = ProcedureCompileOptions::default();
    for mutation in 0..8 {
        let mut program = program();
        let blocks = &mut program.functions[0].blocks;
        match mutation {
            0 => {
                blocks[0].instructions[0].operation = Operation::Move {
                    target: Register::Ebx,
                    source: Source::Register(Register::Edx),
                }
            }
            1 => {
                blocks[0].instructions[1].operation = Operation::Shift {
                    target: Register::Ebx,
                    kind: Shift::Sar,
                    count: ShiftCount::One,
                }
            }
            2 => {
                blocks[2].instructions[0].operation = Operation::LoadMemory {
                    target: Register::Ax,
                    address: address(Register::Edi, 0),
                }
            }
            3 => blocks[2].instructions[3].operation = alu(Register::Edi, Alu::Add, 193),
            4 => {
                blocks[2].instructions[4].operation = Operation::StoreMemory {
                    width: Width::Byte,
                    source: Source::Register(Register::Al),
                    address: address(Register::Edi, (-96i32) as u32),
                }
            }
            5 => blocks[3].instructions[0].operation = alu(Register::Ecx, Alu::And, 3),
            6 => {
                blocks[5].instructions[0].operation = Operation::LoadMemory {
                    target: Register::Al,
                    address: address(Register::Esi, 1),
                }
            }
            7 => {
                blocks[6].terminator.kind = ProcedureTerminatorKind::Return {
                    width: Width::Word,
                    pop_bytes: 0,
                }
            }
            _ => unreachable!(),
        }
        let compiled = program
            .compile_with_copy_kernels(&options, MAX_WORK_PER_CALL)
            .unwrap();
        assert_eq!(compiled.semantic_kernel(ID), None, "mutation={mutation}");
        assert_eq!(
            compiled.wasm(),
            program
                .compile_with_work_limit(&options, MAX_WORK_PER_CALL)
                .unwrap()
                .wasm()
        );
    }
    let kernel = CopyKernel {
        stride: 96,
        max_work_per_call: 4096,
        prefix_nops: 4,
        tail_nops: 4,
    };
    assert_eq!(
        [1, 2, 3, 200].map(|n| kernel.actual_work(n)),
        [Some(6), Some(17), Some(23), Some(710)]
    );
    assert_eq!(kernel.actual_work(0), None);
    assert_eq!(kernel.actual_work(u32::MAX), None);
    assert!(
        !CopyKernel {
            stride: 0,
            ..kernel
        }
        .is_valid()
    );
    assert!(
        !CopyKernel {
            stride: u32::MAX,
            ..kernel
        }
        .is_valid()
    );
    assert!(
        !CopyKernel {
            prefix_nops: u32::MAX,
            ..kernel
        }
        .is_valid()
    );
    assert!(
        !CopyKernel {
            max_work_per_call: 4097,
            ..kernel
        }
        .is_valid()
    );
    let mut p = program();
    p.functions[0].blocks.reverse();
    assert_eq!(
        p.compile_with_copy_kernels(&options, 4096).unwrap().wasm(),
        program()
            .compile_with_copy_kernels(&options, 4096)
            .unwrap()
            .wasm()
    );
}

#[test]
fn copy_kernel_execution_allocates_nothing_and_charges_with_saturation() {
    let compiled = program()
        .compile_with_copy_kernels(&ProcedureCompileOptions::default(), MAX_WORK_PER_CALL)
        .unwrap();
    let mut runner = Runner::new(&compiled);
    let mut input = frame(200);
    input.charged_work = u64::MAX - 709;
    runner.reset(input, &descriptor(15));
    assert_eq!(runner.call(), COMPLETE); // warm the engine's lazy translation
    runner.reset(input, &descriptor(15));
    let measured = allocation_counter::measure(|| assert_eq!(runner.call(), COMPLETE));
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
    assert_eq!(runner.frame().charged_work, u64::MAX);
    assert_eq!(runner.frame().status, input.status | abi::COUNTER_OVERFLOW);
}
