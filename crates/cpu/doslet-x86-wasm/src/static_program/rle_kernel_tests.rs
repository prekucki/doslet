extern crate std;

use super::*;
use crate::static_program::{self as rle_api, Frame, abi};
use alloc::{vec, vec::Vec};
use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_x86::{Cpu, DescriptorTable, Gpr32, SegmentRegister};
use wasmi::{Engine, Linker, Memory, MemoryType, Module, Store, TypedFunc};

mod fixture {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../tests/fixtures/rle_procedure.rs"
    ));
}
const ID: ProcedureId = ProcedureId(19);
const FRAME: usize = 256;
const RAM: usize = 4096;
const RAM_LENGTH: usize = 65536;
const ENTRY: u32 = 0x8000;
const RETURN: u32 = 0x7000;
const SOURCE: u32 = 0x1800;
const DESTINATION: u32 = 0x3000;
const COUNT: u32 = 0x4800;
const VALUE: u32 = 0x4900;
const STACK: u32 = 0x6000;
const REGS: [Gpr32; 8] = [
    Gpr32::Eax,
    Gpr32::Ecx,
    Gpr32::Edx,
    Gpr32::Ebx,
    Gpr32::Esp,
    Gpr32::Ebp,
    Gpr32::Esi,
    Gpr32::Edi,
];

struct Runner {
    store: Store<()>,
    memory: Memory,
    run: TypedFunc<u32, u32>,
}
impl Runner {
    fn new(compiled: &CompiledProcedureProgram) -> Self {
        let engine = Engine::default();
        let module = Module::new(&engine, compiled.wasm()).unwrap();
        assert_eq!(module.imports().count(), 1);
        let mut store = Store::new(&engine, ());
        let memory = Memory::new(&mut store, MemoryType::new(2, Some(2))).unwrap();
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
    fn bytes(&mut self) -> &mut [u8] {
        self.memory.data_mut(&mut self.store)
    }
    fn call(&mut self) -> u32 {
        self.run.call(&mut self.store, FRAME as u32).unwrap()
    }
    fn frame(&self) -> Frame {
        Frame::read(&self.memory.data(&self.store)[FRAME..]).unwrap()
    }
}

struct Bus {
    bytes: Vec<u8>,
    count_store: bool,
    value_store: bool,
    any_store: bool,
}
impl MemoryBus for Bus {
    fn read_u8(&mut self, a: u32) -> Result<u8, BusError> {
        self.bytes
            .get(a as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(a))
    }
    fn write_u8(&mut self, a: u32, v: u8) -> Result<(), BusError> {
        *self
            .bytes
            .get_mut(a as usize)
            .ok_or(BusError::UnmappedMemory(a))? = v;
        self.any_store = true;
        self.count_store |= (COUNT..COUNT + 4).contains(&a);
        self.value_store |= (VALUE..VALUE + 4).contains(&a);
        Ok(())
    }
    fn code_bytes(&self, a: u32, len: u8) -> Option<&[u8]> {
        self.bytes.get(a as usize..a as usize + len as usize)
    }
}
impl PortBus for Bus {
    fn read_port_u8(&mut self, p: u16) -> Result<u8, BusError> {
        Err(BusError::UnmappedPort(p))
    }
    fn write_port_u8(&mut self, p: u16, _: u8) -> Result<(), BusError> {
        Err(BusError::UnmappedPort(p))
    }
}
fn cpu() -> (Cpu, Bus) {
    let mut bus = Bus {
        bytes: vec![0; RAM_LENGTH],
        count_store: false,
        value_store: false,
        any_store: false,
    };
    bus.bytes[0x208..0x220].copy_from_slice(&[
        0xff, 0xff, 0, 0, 0, 0x9a, 0xcf, 0, 0xff, 0xff, 0, 0, 0, 0x92, 0xcf, 0, 0xff, 0xff, 0, 0,
        0, 0x92, 0xcf, 0,
    ]);
    let mut cpu = Cpu::default();
    cpu.set_decode_cache(false);
    cpu.set_dynamic_core(false);
    cpu.control.gdtr = DescriptorTable {
        base: 0x200,
        limit: 31,
    };
    cpu.set_protected_mode(true);
    cpu.load_segment(&mut bus, SegmentRegister::Cs, 8).unwrap();
    cpu.load_segment(&mut bus, SegmentRegister::Ss, 16).unwrap();
    cpu.load_segment(&mut bus, SegmentRegister::Ds, 24).unwrap();
    (cpu, bus)
}
fn dword(bytes: &mut [u8], offset: u32, value: u32) {
    bytes[offset as usize..offset as usize + 4].copy_from_slice(&value.to_le_bytes());
}
fn program() -> (ProcedureGraphProgram, Vec<u8>) {
    let f = fixture::rle_fixture(ID, 7, ENTRY);
    (
        ProcedureGraphProgram {
            functions: vec![f.function],
        },
        f.code,
    )
}

#[test]
fn rle_kernel_matches_independent_x86_for_literals_runs_pending_zero_and_truncated_runs() {
    let (program, code) = program();
    assert!(
        program
            .compile_with_work_limit(&ProcedureCompileOptions::default(), 4096)
            .is_err()
    );
    let compiled = program
        .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 4096)
        .unwrap();
    assert_eq!(compiled.rle_kernel(ID), Some(RleKernel));
    assert_eq!(compiled.semantic_kernel(ID), None);
    assert_eq!(compiled.maximum_work(ID), Some(u64::from(u32::MAX)));
    let mut runner = Runner::new(&compiled);
    let cases: &[(&[u8], u32, u32, u32)] = &[
        (&[1, 2, 3, 4], 4, 0, 0x89ab_cdef),
        (&[1, 2, 3], 3, (-7i32) as u32, 0x89ab_cdef),
        (&[0xc4, 0xfe], 4, 0, 0),
        (&[1, 0xc3, 0xee, 2, 0xc5, 7], 8, 0, 0x1234_5678),
        (&[0xc0, 0xaa, 0xc0, 0xbb, 1, 0xc2, 0xcc], 3, 0, 0),
        (&[], 3, 3, 0x89ab_cdef),
        (&[], 3, 8, 0x89ab_cdef),
        (&[], 3, i32::MAX as u32, 0x89ab_cdef),
        (&[0xc8, 0xb7], 3, 0, 0),
        (&[4, 5, 0xc7, 6], 5, 2, 0x1122_33ee),
        (
            &[
                0xff, 7, 0xff, 8, 0xff, 9, 0xff, 10, 0xff, 11, 0xff, 12, 0xff, 13, 0xff, 14, 0xff,
                15, 0xff, 16, 0xca, 17,
            ],
            640,
            0,
            0,
        ),
    ];
    for &(source, n, initial_run, initial_value) in cases {
        for flags in [0, 0x8d7, 0x246, 0xffff_ffff] {
            let input = Frame {
                gpr: [
                    SOURCE,
                    n,
                    0x8877_6655,
                    DESTINATION,
                    STACK,
                    0x1122_3344,
                    0xaabb_ccdd,
                    0x9988_7766,
                ],
                eflags: flags,
                address: program.functions[0].entry,
                charged_work: 39,
                remaining_budget: 1,
                status: 0x8000_0000,
            };
            let mut initial = vec![0xa5; RAM_LENGTH];
            initial[ENTRY as usize..ENTRY as usize + code.len()].copy_from_slice(&code);
            initial[SOURCE as usize..SOURCE as usize + source.len()].copy_from_slice(source);
            dword(&mut initial, COUNT, initial_run);
            dword(&mut initial, VALUE, initial_value);
            dword(&mut initial, STACK, RETURN);
            dword(&mut initial, STACK + 4, COUNT);
            dword(&mut initial, STACK + 8, VALUE);
            let (mut cpu, mut bus) = cpu();
            bus.bytes.copy_from_slice(&initial);
            cpu.flags = flags;
            cpu.registers.ip = ENTRY;
            for (r, v) in REGS.into_iter().zip(input.gpr) {
                cpu.registers.set32(r, v);
            }
            let (mut work, mut count_writes, mut value_writes, mut writes) = (0, 0, 0, 0);
            while cpu.registers.ip != RETURN {
                bus.count_store = false;
                bus.value_store = false;
                bus.any_store = false;
                cpu.step(&mut bus).unwrap();
                work += 1;
                count_writes += u32::from(bus.count_store);
                value_writes += u32::from(bus.value_store);
                writes += u64::from(bus.any_store);
                assert!(work < 20000);
            }
            runner.bytes().fill(0);
            input.write(&mut runner.bytes()[FRAME..]).unwrap();
            memory::Descriptor::default()
                .write(&mut runner.bytes()[FRAME + memory::OFFSET as usize..])
                .unwrap();
            runner.bytes()[RAM..RAM + RAM_LENGTH].copy_from_slice(&initial);
            RleCall {
                source_pointer: RAM as u32 + SOURCE,
                destination_pointer: RAM as u32 + DESTINATION,
                count_pointer: RAM as u32 + COUNT,
                value_pointer: RAM as u32 + VALUE,
                stack_pointer: RAM as u32 + STACK - 24,
                return_target: RETURN,
                work,
                output_count: n,
                initial_run,
                initial_value,
                source_length: cpu.registers.get32(Gpr32::Eax),
                count_offset: COUNT,
                count_writes,
                value_writes,
            }
            .write(&mut runner.bytes()[FRAME + RleCall::OFFSET as usize..])
            .unwrap();
            let allocation_input = (n == 640 && flags == 0).then(|| runner.bytes().to_vec());
            assert_eq!(
                runner.call(),
                COMPLETE,
                "source={source:?}, n={n}, run={initial_run}, flags={flags:x}"
            );
            let expected = Frame {
                gpr: core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
                eflags: cpu.flags,
                address: Address {
                    offset: RETURN,
                    ..input.address
                },
                charged_work: input.charged_work + u64::from(work),
                ..input
            };
            assert_eq!(
                runner.frame(),
                expected,
                "source={source:?}, n={n}, run={initial_run}, flags={flags:x}"
            );
            assert_eq!(
                &runner.bytes()[RAM..RAM + RAM_LENGTH],
                &bus.bytes,
                "full ordinary RAM differs"
            );
            let effects =
                memory::Effects::read(&runner.bytes()[FRAME + memory::OFFSET as usize..]).unwrap();
            assert_eq!(effects.ram_writes, writes);
            assert_eq!(
                writes,
                6 + u64::from(n) + u64::from(count_writes) + u64::from(value_writes)
            );
            assert_eq!(effects.device_reads, 0);
            assert_eq!(effects.device_writes, 0);
            if let Some(before) = allocation_input {
                assert!(
                    work > 4096,
                    "proved computation must exceed the ordinary loop limit"
                );
                runner.bytes().copy_from_slice(&before);
                let allocations =
                    allocation_counter::measure(|| assert_eq!(runner.call(), COMPLETE));
                assert_eq!(allocations.count_total, 0);
                assert_eq!(allocations.bytes_total, 0);
                runner.bytes().copy_from_slice(&before);
                Frame {
                    charged_work: u64::MAX - u64::from(work) + 1,
                    ..input
                }
                .write(&mut runner.bytes()[FRAME..])
                .unwrap();
                assert_eq!(runner.call(), COMPLETE);
                assert_eq!(runner.frame().charged_work, u64::MAX);
                assert_eq!(runner.frame().status, input.status | abi::COUNTER_OVERFLOW);
                assert_eq!(runner.frame().remaining_budget, 1);
            }
        }
    }
}

fn split(function: &ProcedureGraphFunction) -> ProcedureGraphFunction {
    let mut output = function.clone();
    output.blocks.clear();
    for block in &function.blocks {
        for (i, instruction) in block.instructions.iter().enumerate() {
            let next = block
                .instructions
                .get(i + 1)
                .map_or(block.terminator.address, |i| i.address);
            output.blocks.push(ProcedureBlock {
                entry: instruction.address,
                instructions: vec![*instruction],
                terminator: ProcedureTerminator {
                    address: next,
                    kind: ProcedureTerminatorKind::Fallthrough(next),
                },
            });
        }
        // Fallthrough terminators belong to the last subdivision, avoiding an
        // empty synthetic block (which the graph validator properly rejects).
        if let ProcedureTerminatorKind::Fallthrough(_) = block.terminator.kind {
            output.blocks.last_mut().unwrap().terminator = block.terminator;
        } else {
            output.blocks.push(ProcedureBlock {
                entry: block.terminator.address,
                instructions: vec![],
                terminator: block.terminator,
            });
        }
    }
    output.blocks.reverse();
    output
}

#[test]
fn rle_recognition_covers_complete_operations_edges_and_graph_validity() {
    let (program, _) = program();
    let function = &program.functions[0];
    assert_eq!(function.recognize_rle_kernel(), Some(RleKernel));
    let grouped = program
        .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 4096)
        .unwrap();
    let split = split(function);
    assert_eq!(split.recognize_rle_kernel(), Some(RleKernel));
    assert_eq!(
        ProcedureGraphProgram {
            functions: vec![split]
        }
        .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 4096)
        .unwrap()
        .wasm(),
        grouped.wasm()
    );
    let relocated = fixture::rle_fixture(ID, 98, 0x1234).function;
    assert_eq!(relocated.recognize_rle_kernel(), Some(RleKernel));
    for (bi, block) in function.blocks.iter().enumerate() {
        for ii in 0..block.instructions.len() {
            let mut bad = function.clone();
            bad.blocks[bi].instructions[ii].operation = Operation::Nop;
            assert_eq!(bad.recognize_rle_kernel(), None, "operation {bi}:{ii}");
        }
        let mut bad = function.clone();
        bad.blocks[bi].terminator.kind = ProcedureTerminatorKind::Jump(function.entry);
        assert_eq!(bad.recognize_rle_kernel(), None, "edge {bi}");
        if let ProcedureTerminatorKind::Branch {
            condition,
            taken,
            not_taken,
        } = block.terminator.kind
        {
            let mut swapped = function.clone();
            swapped.blocks[bi].terminator.kind = ProcedureTerminatorKind::Branch {
                condition,
                taken: not_taken,
                not_taken: taken,
            };
            assert_eq!(swapped.recognize_rle_kernel(), None, "swapped edge {bi}");
            let mut changed = function.clone();
            changed.blocks[bi].terminator.kind = ProcedureTerminatorKind::Branch {
                condition: if condition == Condition::Equal {
                    Condition::NotEqual
                } else {
                    Condition::Equal
                },
                taken,
                not_taken,
            };
            assert_eq!(changed.recognize_rle_kernel(), None, "condition {bi}");
        }
    }
    let mut added = function.clone();
    let last = added.blocks[0].instructions.last().unwrap().address;
    added.blocks[0].instructions.push(ProcedureInstruction {
        address: Address {
            offset: last.offset + 1,
            ..last
        },
        operation: Operation::Nop,
    });
    assert_eq!(added.recognize_rle_kernel(), None, "extra instruction");
    let mut extra = function.clone();
    let mut unreachable = extra.blocks.last().unwrap().clone();
    unreachable.entry.offset += 1000;
    unreachable.terminator.address = unreachable.entry;
    unreachable.instructions.clear();
    extra.blocks.push(unreachable);
    assert_eq!(extra.recognize_rle_kernel(), None);
    assert!(
        ProcedureGraphProgram {
            functions: vec![extra]
        }
        .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 4096)
        .is_err()
    );
    let mut overlap = function.clone();
    overlap.blocks[1].entry = overlap.blocks[0].entry;
    assert_eq!(overlap.recognize_rle_kernel(), None);
    let mut malformed = function.clone();
    malformed.blocks[0].instructions[0].operation = Operation::Push {
        width: Width::Byte,
        source: Source::Register(Register::Al),
    };
    assert_eq!(malformed.recognize_rle_kernel(), None);
    assert!(
        function.validate_with_work_limit(4096).is_err(),
        "default admission must stay narrow"
    );
}

#[test]
fn rle_call_header_roundtrips_and_bad_versions_decline_without_effects() {
    assert_eq!(RleCall::OFFSET, 456);
    let mut bytes = [0; RleCall::BYTES];
    let header = RleCall {
        work: 123,
        source_pointer: 456,
        ..RleCall::default()
    };
    header.write(&mut bytes).unwrap();
    assert_eq!(RleCall::read(&bytes), Some(header));
    assert_eq!(RleCall::read(&bytes[..63]), None);
    assert_eq!(header.write(&mut bytes[..63]), None);
    let (program, _) = program();
    let compiled = program
        .compile_with_computation_kernels(&ProcedureCompileOptions::default(), 4096)
        .unwrap();
    let mut runner = Runner::new(&compiled);
    let input = Frame {
        address: program.functions[0].entry,
        ..Frame::default()
    };
    for field in [0, 15] {
        runner.bytes().fill(0);
        input.write(&mut runner.bytes()[FRAME..]).unwrap();
        header
            .write(&mut runner.bytes()[FRAME + RleCall::OFFSET as usize..])
            .unwrap();
        dword(
            runner.bytes(),
            (FRAME + RleCall::OFFSET as usize + field * 4) as u32,
            12345,
        );
        let before = runner.bytes().to_vec();
        assert_eq!(runner.call(), SIDE_EXIT);
        assert_eq!(runner.bytes(), before);
    }
}
