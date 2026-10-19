extern crate std;

use super::*;
use alloc::{vec, vec::Vec};
use memory::{Descriptor, MemoryAddress, RamDescriptor, RegionGuard, Segment, SegmentBounds};
use wasmi::{Caller, Config, Engine, Linker, Memory, MemoryType, Module, Store, TypedFunc};

const FRAME: usize = 256;
const RAM: usize = 4096;
const GENERATIONS: usize = 24576;
const GUARDS: usize = 25088;

#[derive(Default)]
struct State {
    callbacks: Vec<(u32, Frame, Option<memory::Effects>)>,
    refill: Option<u32>,
}

struct Runner {
    store: Store<State>,
    memory: Memory,
    run: TypedFunc<u32, u32>,
}

fn options() -> CompileOptions {
    CompileOptions {
        direct_memory: true,
        inline_admission: true,
        service_interval_regions: 128,
        ..CompileOptions::default()
    }
}

impl Runner {
    fn new(program: &Program, options: CompileOptions) -> Self {
        let compiled = program.compile(&options).unwrap();
        let mut config = Config::default();
        config.wasm_tail_call(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, compiled.wasm()).unwrap();
        // Deliberately provide no admission import. Successful instantiation
        // verifies that inline mode needs only memory and ordinary runtime.
        assert_eq!(module.imports().count(), 2);
        let mut store = Store::new(&engine, State::default());
        let memory = Memory::new(&mut store, MemoryType::new(1, Some(1))).unwrap();
        let mut linker = Linker::new(&engine);
        linker
            .define(abi::IMPORT_MODULE, abi::MEMORY_IMPORT, memory)
            .unwrap();
        linker
            .func_wrap(
                abi::IMPORT_MODULE,
                abi::RUNTIME_IMPORT,
                |mut caller: Caller<'_, State>, pointer: u32, reason: u32| -> u32 {
                    let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                    let bytes = &memory.data(&caller)[pointer as usize..];
                    let frame = Frame::read(bytes).unwrap();
                    let effects = memory::Effects::read(&bytes[memory::OFFSET as usize..]).ok();
                    caller.data_mut().callbacks.push((reason, frame, effects));
                    if let Some(grant) = caller.data_mut().refill.take() {
                        let bytes = memory.data_mut(&mut caller);
                        put_word(bytes, pointer as usize, memory::INLINE_GRANT, grant);
                        put_word(bytes, pointer as usize, memory::INLINE_RESERVED, 0);
                        abi::CONTINUE
                    } else {
                        abi::EXIT
                    }
                },
            )
            .unwrap();
        let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
        let run = instance
            .get_typed_func::<u32, u32>(&store, abi::RUN_EXPORT)
            .unwrap();
        Self { store, memory, run }
    }

    fn bytes(&mut self) -> &mut [u8] {
        self.memory.data_mut(&mut self.store)
    }

    fn set(&mut self, program: &Program, frame: Frame, descriptor: Descriptor) {
        frame.write(&mut self.bytes()[FRAME..]).unwrap();
        descriptor
            .write(&mut self.bytes()[FRAME + memory::OFFSET as usize..])
            .unwrap();
        self.bytes()[GENERATIONS..GENERATIONS + 8].copy_from_slice(&3u64.to_le_bytes());
        self.bytes()[GENERATIONS + 8..GENERATIONS + 16].copy_from_slice(&5u64.to_le_bytes());
        for region in &program.regions {
            self.guard(region.id.0, guard());
        }
    }

    fn guard(&mut self, id: u32, guard: RegionGuard) {
        guard
            .write(&mut self.bytes()[GUARDS + id as usize * memory::REGION_GUARD_BYTES..])
            .unwrap();
    }

    fn word(&mut self, offset: u32, value: u32) {
        put_word(self.bytes(), FRAME, offset, value);
    }

    fn frame(&self) -> Frame {
        Frame::read(&self.memory.data(&self.store)[FRAME..]).unwrap()
    }

    fn effects(&self) -> memory::Effects {
        memory::Effects::read(&self.memory.data(&self.store)[FRAME + memory::OFFSET as usize..])
            .unwrap()
    }

    fn call(&mut self) -> u32 {
        self.run.call(&mut self.store, FRAME as u32).unwrap()
    }
}

fn put_word(bytes: &mut [u8], frame: usize, offset: u32, value: u32) {
    let offset = frame + memory::OFFSET as usize + offset as usize;
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn guard() -> RegionGuard {
    RegionGuard {
        valid: true,
        pages: [0, 1],
        generations: [3, 5],
    }
}

fn descriptor() -> Descriptor {
    Descriptor {
        ram: RamDescriptor {
            pointer: RAM as u32,
            linear_base: 0x500,
            length: 0x2000,
            generations_pointer: GENERATIONS as u32,
            generations_length: 2,
            ..RamDescriptor::default()
        },
        windows: [[SegmentBounds {
            valid: true,
            base: 0,
            first: 0,
            last: u32::MAX,
        }; 6]; 2],
        inline_grant: 100,
        region_guards_pointer: GUARDS as u32,
        region_guards_length: 3,
        ..Descriptor::default()
    }
}

fn address(id: u32) -> Address {
    Address {
        space: 7,
        offset: id * 16,
    }
}

fn initial(budget: u64) -> Frame {
    Frame {
        gpr: [0; 8],
        address: address(0),
        remaining_budget: budget,
        eflags: 2,
        ..Frame::default()
    }
}

fn region(id: u32, operations: Vec<Operation>, terminator: Terminator) -> Region {
    Region {
        id: RegionId(id),
        entry: address(id),
        operations,
        terminator,
        accounting: Accounting::Counted,
    }
}

fn register_program() -> Program {
    Program {
        regions: vec![region(
            0,
            vec![Operation::Inc(Register::Ecx)],
            Terminator::Jump(RegionId(0)),
        )],
    }
}

fn memory_program() -> Program {
    Program {
        regions: vec![region(
            0,
            vec![Operation::LoadMemory {
                target: Register::Eax,
                address: MemoryAddress {
                    base: None,
                    index: None,
                    scale: 1,
                    displacement: 0x503,
                    address16: false,
                    segment: Segment::Ds,
                },
            }],
            Terminator::Fallthrough(RegionId(0)),
        )],
    }
}

#[test]
fn inline_admission_register_memory_loop_has_no_callbacks_before_soft_end() {
    let program = Program {
        regions: vec![
            region(
                0,
                vec![Operation::Inc(Register::Ecx)],
                Terminator::Fallthrough(RegionId(1)),
            ),
            Region {
                id: RegionId(1),
                entry: address(1),
                terminator: Terminator::Fallthrough(RegionId(2)),
                ..memory_program().regions.remove(0)
            },
            region(
                2,
                vec![Operation::Alu {
                    kind: Alu::Add,
                    target: Register::Ebx,
                    source: Source::Register(Register::Eax),
                }],
                Terminator::Jump(RegionId(0)),
            ),
        ],
    };
    let mut runner = Runner::new(&program, options());
    runner.set(&program, initial(12), descriptor());
    runner.bytes()[RAM + 3..RAM + 7].copy_from_slice(&9u32.to_le_bytes());
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    let frame = runner.frame();
    assert_eq!(frame.gpr[0], 9);
    assert_eq!(frame.gpr[1], 3);
    assert_eq!(frame.gpr[3], 27);
    assert_eq!(frame.charged_work, 12);
    assert_eq!(frame.address, address(0));
    assert_eq!(runner.effects().inline_reserved, 12);
    assert_eq!(runner.effects().inline_remaining, 88);
    assert!(runner.store.data().callbacks.is_empty());
}

#[test]
fn inline_admission_uses_sparse_region_ids_and_each_regions_cost() {
    let program = Program {
        regions: vec![
            region(
                10,
                vec![Operation::Nop],
                Terminator::Fallthrough(RegionId(200)),
            ),
            region(200, vec![Operation::Nop; 3], Terminator::Jump(RegionId(10))),
        ],
    };
    let mut runner = Runner::new(&program, options());
    runner.set(
        &program,
        Frame {
            address: address(10),
            ..initial(5)
        },
        Descriptor {
            region_guards_length: 201,
            ..descriptor()
        },
    );
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    assert_eq!(runner.frame().address, address(10));
    assert_eq!(runner.frame().charged_work, 5);
    assert_eq!(runner.effects().inline_reserved, 5);
    assert_eq!(runner.effects().inline_remaining, 95);
    assert!(runner.store.data().callbacks.is_empty());
}

#[test]
fn inline_admission_reservation_counter_accepts_exact_maximum() {
    let program = register_program();
    let mut runner = Runner::new(&program, options());
    runner.set(&program, initial(2), descriptor());
    runner.word(memory::INLINE_RESERVED, u32::MAX - 2);
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    assert_eq!(runner.effects().inline_reserved, u32::MAX);
    assert_eq!(runner.effects().inline_remaining, 98);
    assert_eq!(runner.frame().charged_work, 2);
    assert!(runner.store.data().callbacks.is_empty());

    let frame = Frame {
        remaining_budget: 2,
        ..runner.frame()
    };
    frame.write(&mut runner.bytes()[FRAME..]).unwrap();
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.frame(), frame);
    assert_eq!(runner.effects().inline_reserved, u32::MAX);
    assert_eq!(runner.effects().inline_remaining, 98);
}

#[test]
fn inline_admission_refuses_bad_grants_tables_and_counters_before_effects() {
    let program = register_program();
    for (offset, value) in [
        (memory::VERSION_OFFSET, memory::VERSION + 1),
        (memory::INLINE_GRANT, 0),
        (memory::INLINE_GRANT, 1),
        (memory::REGION_GUARDS_LENGTH, 0),
        (memory::REGION_GUARDS_LENGTH, u32::MAX),
        (memory::REGION_GUARDS_POINTER, u32::MAX),
        (memory::REGION_GUARDS_POINTER, 65536 - 31),
        (memory::GENERATIONS_LENGTH, 0),
        (memory::GENERATIONS_LENGTH, u32::MAX),
        (memory::GENERATIONS_POINTER, u32::MAX),
        (memory::GENERATIONS_POINTER, 65536 - 15),
        (memory::INLINE_RESERVED, u32::MAX),
    ] {
        let mut runner = Runner::new(&program, options());
        runner.set(&program, initial(10), descriptor());
        runner.word(offset, value);
        let effects = runner.effects();
        assert_eq!(runner.call(), abi::EXIT_RUNTIME, "offset {offset}");
        assert_eq!(runner.frame(), initial(10), "offset {offset}");
        assert_eq!(runner.effects(), effects, "offset {offset}");
        assert_eq!(runner.store.data().callbacks.len(), 1);
        assert_eq!(runner.store.data().callbacks[0].0, abi::REASON_FALLBACK);
    }
}

#[test]
fn inline_admission_refuses_invalid_and_stale_source_guards() {
    let program = register_program();
    for guard in [
        RegionGuard {
            valid: false,
            ..guard()
        },
        RegionGuard {
            pages: [2, 1],
            ..guard()
        },
        RegionGuard {
            pages: [0, 2],
            ..guard()
        },
        RegionGuard {
            generations: [4, 5],
            ..guard()
        },
        RegionGuard {
            generations: [3, 6],
            ..guard()
        },
    ] {
        let mut runner = Runner::new(&program, options());
        runner.set(&program, initial(10), descriptor());
        runner.guard(0, guard);
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        assert_eq!(runner.frame(), initial(10));
        assert_eq!(runner.effects().inline_reserved, 0);
        assert_eq!(runner.effects().inline_remaining, 100);
    }
    // Only the exact valid encoding is admitted, not arbitrary nonzero words.
    let mut runner = Runner::new(&program, options());
    runner.set(&program, initial(10), descriptor());
    runner.bytes()[GUARDS..GUARDS + 4].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.frame(), initial(10));
}

#[test]
fn inline_admission_observes_actual_generation_table_changes() {
    let program = register_program();
    let mut runner = Runner::new(&program, options());
    runner.set(&program, initial(2), descriptor());
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    assert_eq!(runner.frame().gpr[1], 1);
    let mut frame = runner.frame();
    frame.remaining_budget = 2;
    frame.write(&mut runner.bytes()[FRAME..]).unwrap();
    runner.bytes()[GENERATIONS + 8..GENERATIONS + 16].copy_from_slice(&6u64.to_le_bytes());
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.frame(), frame);
    assert_eq!(runner.effects().inline_reserved, 2);
    assert_eq!(runner.effects().inline_remaining, 98);
}

#[test]
fn inline_admission_memory_preflight_and_guard_failures_reserve_no_work() {
    let program = memory_program();
    for invalid_memory in [false, true] {
        let mut runner = Runner::new(&program, options());
        runner.set(&program, initial(10), descriptor());
        if invalid_memory {
            runner.word(memory::RAM_POINTER, u32::MAX);
        } else {
            runner.guard(
                0,
                RegionGuard {
                    valid: false,
                    ..guard()
                },
            );
        }
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        assert_eq!(runner.frame(), initial(10));
        assert_eq!(runner.effects().inline_reserved, 0);
        assert_eq!(runner.effects().inline_remaining, 100);
        assert_eq!(runner.effects().device_reads, 0);
    }
}

#[test]
fn inline_admission_device_effects_wait_until_guard_success() {
    let program = memory_program();
    for valid in [false, true] {
        let mut runner = Runner::new(&program, options());
        let descriptor = Descriptor {
            device: memory::DeviceReadDescriptor {
                guest_start: 0x500,
                guest_length: 0x100,
                pointer: 16384,
                backing_length: 32,
                kind: memory::DeviceReadKind::FourLanes,
                read_plane: 1,
                latches: [90; 4],
                ..memory::DeviceReadDescriptor::default()
            },
            ..descriptor()
        };
        runner.set(&program, initial(1), descriptor);
        runner.guard(0, RegionGuard { valid, ..guard() });
        for offset in 0..32 {
            runner.bytes()[16384 + offset] = (offset + 1) as u8;
        }
        if valid {
            assert_eq!(runner.call(), abi::EXIT_BUDGET);
            assert_eq!(runner.frame().gpr[0], u32::from_le_bytes([14, 18, 22, 26]));
            assert_eq!(runner.effects().device_reads, 4);
            assert_eq!(runner.effects().latches, [25, 26, 27, 28]);
            assert_eq!(runner.effects().inline_reserved, 1);
            assert!(runner.store.data().callbacks.is_empty());
        } else {
            assert_eq!(runner.call(), abi::EXIT_RUNTIME);
            assert_eq!(runner.frame(), initial(1));
            assert_eq!(runner.effects().device_reads, 0);
            assert_eq!(runner.effects().latches, [90; 4]);
            assert_eq!(runner.effects().inline_reserved, 0);
            assert_eq!(runner.effects().inline_remaining, 100);
        }
    }
}

#[test]
fn inline_admission_refusal_flushes_prior_work_at_current_entry() {
    let program = Program {
        regions: vec![
            region(
                0,
                vec![Operation::Inc(Register::Ecx)],
                Terminator::Fallthrough(RegionId(1)),
            ),
            region(
                1,
                vec![Operation::Inc(Register::Eax)],
                Terminator::Jump(RegionId(0)),
            ),
        ],
    };
    let mut runner = Runner::new(&program, options());
    runner.set(&program, initial(10), descriptor());
    runner.guard(
        1,
        RegionGuard {
            valid: false,
            ..guard()
        },
    );
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.frame().gpr[0], 0);
    assert_eq!(runner.frame().gpr[1], 1);
    assert_eq!(runner.frame().address, address(1));
    assert_eq!(runner.frame().remaining_budget, 9);
    assert_eq!(runner.frame().charged_work, 1);
    assert_eq!(runner.effects().inline_reserved, 1);
    assert_eq!(runner.effects().inline_remaining, 99);
}

#[test]
fn inline_admission_runtime_can_reconcile_reservations_and_refill_grant() {
    let program = register_program();
    let mut runner = Runner::new(&program, options());
    runner.set(
        &program,
        initial(6),
        Descriptor {
            inline_grant: 2,
            ..descriptor()
        },
    );
    runner.store.data_mut().refill = Some(4);
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    assert_eq!(runner.frame().gpr[1], 3);
    assert_eq!(runner.frame().charged_work, 6);
    assert_eq!(runner.effects().inline_reserved, 4);
    assert_eq!(runner.effects().inline_remaining, 0);
    let callbacks = &runner.store.data().callbacks;
    assert_eq!(callbacks.len(), 1);
    assert_eq!(callbacks[0].0, abi::REASON_FALLBACK);
    assert_eq!(callbacks[0].1.charged_work, 2);
    assert_eq!(callbacks[0].2.unwrap().inline_reserved, 2);
}

#[test]
fn inline_admission_truncated_descriptor_refuses_without_trapping() {
    let program = register_program();
    let mut runner = Runner::new(&program, options());
    let pointer = runner.bytes().len() - abi::FRAME_BYTES;
    initial(10).write(&mut runner.bytes()[pointer..]).unwrap();
    assert_eq!(
        runner.run.call(&mut runner.store, pointer as u32).unwrap(),
        abi::EXIT_RUNTIME
    );
    let callbacks = &runner.store.data().callbacks;
    assert_eq!(callbacks.len(), 1);
    assert_eq!(callbacks[0], (abi::REASON_FALLBACK, initial(10), None));
}

#[test]
fn inline_admission_zero_work_keeps_structural_quota_without_descriptor() {
    let program = Program {
        regions: vec![region(0, vec![], Terminator::Fallthrough(RegionId(0)))],
    };
    let mut runner = Runner::new(
        &program,
        CompileOptions {
            service_interval_regions: 3,
            ..options()
        },
    );
    initial(10).write(&mut runner.bytes()[FRAME..]).unwrap();
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.frame(), initial(10));
    assert_eq!(runner.store.data().callbacks.len(), 1);
    assert_eq!(runner.store.data().callbacks[0].0, abi::REASON_SERVICE);
}

#[test]
fn inline_admission_counted_work_obeys_structural_service_quota() {
    let program = register_program();
    let mut runner = Runner::new(
        &program,
        CompileOptions {
            service_interval_regions: 3,
            ..options()
        },
    );
    runner.set(&program, initial(10), descriptor());
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.frame().gpr[1], 3);
    assert_eq!(runner.frame().charged_work, 6);
    assert_eq!(runner.effects().inline_reserved, 6);
    assert_eq!(runner.store.data().callbacks.len(), 1);
    assert_eq!(runner.store.data().callbacks[0].0, abi::REASON_SERVICE);
    assert_eq!(
        runner.store.data().callbacks[0].2.unwrap().inline_reserved,
        6
    );
}

#[test]
fn inline_admission_rejects_incompatible_options_and_hot_work() {
    let mut program = register_program();
    for options in [
        CompileOptions {
            direct_memory: false,
            ..options()
        },
        CompileOptions {
            runtime_admission: true,
            ..options()
        },
    ] {
        assert_eq!(
            program.compile(&options).unwrap_err(),
            CompileError::InvalidOptions
        );
    }
    program.regions[0].accounting = Accounting::UncountedHot;
    assert_eq!(
        program.compile(&options()).unwrap_err(),
        CompileError::AdmissionUncountedHot(RegionId(0))
    );
}

#[test]
fn inline_admission_large_program_shares_guard_code() {
    // Match a whole executable's region count. Guard bodies repeated at every
    // entry used to exceed the unchanged 64-MiB output limit at this scale.
    let count = 50_000u32;
    let program = Program {
        regions: (0..count)
            .map(|id| {
                region(
                    id,
                    vec![Operation::Nop],
                    Terminator::Fallthrough(RegionId((id + 1) % count)),
                )
            })
            .collect(),
    };
    let baseline = program
        .compile(&CompileOptions {
            inline_admission: false,
            ..options()
        })
        .unwrap();
    let admitted = program.compile(&options()).unwrap();
    let added = admitted.wasm().len() - baseline.wasm().len();
    std::println!(
        "{count} regions: baseline {} bytes, admitted {} bytes, admission overhead {added} bytes",
        baseline.wasm().len(),
        admitted.wasm().len(),
    );
    // Each entry needs a small helper call and one ordinary refusal path.
    // The complete descriptor/generation validation belongs to one helper.
    assert!(added <= count as usize * 192 + 1024, "{added} bytes");
}
