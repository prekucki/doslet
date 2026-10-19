extern crate std;

use super::*;
use alloc::{vec, vec::Vec};
use memory::{
    Descriptor, DeviceReadDescriptor, DeviceReadKind, MemoryAddress, RamDescriptor, Segment,
    SegmentBounds,
};
use wasmi::{Caller, Config, Engine, Linker, Memory, MemoryType, Module, Store, TypedFunc};

#[path = "memory_alu_tests.rs"]
mod memory_alu_tests;

const FRAME: usize = 256;
const RAM: usize = 4096;
const VRAM: usize = 16384;

#[derive(Default)]
struct State {
    callbacks: Vec<(u32, Frame)>,
    admissions: Vec<(u32, u32)>,
    refuse: bool,
}

struct Runner {
    store: Store<State>,
    memory: Memory,
    run: TypedFunc<u32, u32>,
}

impl Runner {
    fn new(program: &Program, admission: bool) -> Self {
        Self::with_options(program, options(admission))
    }

    fn with_options(program: &Program, options: CompileOptions) -> Self {
        let compiled = program.compile(&options).unwrap();
        let mut config = Config::default();
        config.wasm_tail_call(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, compiled.wasm()).unwrap();
        assert_eq!(
            module.imports().count(),
            2 + usize::from(options.runtime_admission)
        );
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
                    let frame = Frame::read(&memory.data(&caller)[pointer as usize..]).unwrap();
                    caller.data_mut().callbacks.push((reason, frame));
                    abi::EXIT
                },
            )
            .unwrap();
        linker
            .func_wrap(
                abi::IMPORT_MODULE,
                abi::ADMIT_IMPORT,
                |mut caller: Caller<'_, State>, _: u32, region: u32, required: u32| -> u32 {
                    caller.data_mut().admissions.push((region, required));
                    u32::from(!caller.data().refuse)
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

    fn set(&mut self, frame: Frame, descriptor: &Descriptor) {
        frame.write(&mut self.bytes()[FRAME..]).unwrap();
        descriptor
            .write(&mut self.bytes()[FRAME + memory::OFFSET as usize..])
            .unwrap();
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

fn options(admission: bool) -> CompileOptions {
    CompileOptions {
        direct_memory: true,
        runtime_admission: admission,
        ..CompileOptions::default()
    }
}

fn address(displacement: u32) -> MemoryAddress {
    MemoryAddress {
        base: None,
        index: None,
        scale: 1,
        displacement,
        address16: false,
        segment: Segment::Ds,
    }
}

fn region(id: u32, target: Register, address: MemoryAddress, next: u32) -> Region {
    Region {
        id: RegionId(id),
        entry: Address {
            space: 7,
            offset: id * 16,
        },
        operations: vec![Operation::LoadMemory { target, address }],
        terminator: Terminator::Fallthrough(RegionId(next)),
        accounting: Accounting::Counted,
    }
}

fn program(target: Register, address: MemoryAddress) -> Program {
    Program {
        regions: vec![region(1, target, address, 1)],
    }
}

#[test]
fn large_memory_program_fits_bounded_output_without_repeating_refusal_flushes() {
    const REGIONS: u32 = 10_000;
    let regions = (0..REGIONS)
        .map(|id| {
            let mut value = region(id, Register::Eax, address(0x1234), (id + 1) % REGIONS);
            value.operations[0] = match id % 4 {
                0 => value.operations[0],
                1 => Operation::StoreMemory {
                    address: address(0x1234),
                    width: Width::Dword,
                    source: Source::Register(Register::Eax),
                },
                2 => Operation::AluMemoryStore {
                    kind: Alu::Adc,
                    address: address(0x1234),
                    width: Width::Dword,
                    source: Source::Register(Register::Eax),
                },
                _ => Operation::UnaryMemory {
                    kind: MemoryUnary::Dec,
                    address: address(0x1234),
                    width: Width::Dword,
                },
            };
            value
        })
        .collect();
    let compiled = Program { regions }
        .compile(&CompileOptions {
            direct_memory: true,
            inline_admission: true,
            max_output_bytes: 24 * 1024 * 1024,
            ..CompileOptions::default()
        })
        .expect("large memory modules must share each instruction's refusal path");
    let mut config = Config::default();
    config.wasm_tail_call(true);
    Module::new(&Engine::new(&config), compiled.wasm()).unwrap();
}

fn descriptor() -> Descriptor {
    Descriptor {
        ram: RamDescriptor {
            pointer: RAM as u32,
            linear_base: 0x500,
            length: 0x2000,
            ..RamDescriptor::default()
        },
        windows: [[SegmentBounds {
            valid: true,
            base: 0,
            first: 0,
            last: u32::MAX,
        }; 6]; 2],
        ..Descriptor::default()
    }
}

fn initial() -> Frame {
    Frame {
        gpr: [0xaabb_ccdd; 8],
        address: Address {
            space: 7,
            offset: 16,
        },
        remaining_budget: 1,
        eflags: 0x8d7,
        ..Frame::default()
    }
}

fn device(kind: DeviceReadKind) -> Descriptor {
    Descriptor {
        device: DeviceReadDescriptor {
            guest_start: 0xa0000,
            guest_length: 0x10000,
            pointer: VRAM as u32,
            backing_length: 4096,
            kind,
            latches: [90; 4],
            ..DeviceReadDescriptor::default()
        },
        ..descriptor()
    }
}

#[test]
fn inline_ram_loads_preserve_aliases_and_have_no_runtime_callbacks() {
    for (target, expected) in [
        (Register::Al, 0xaabb_cc11),
        (Register::Ah, 0xaabb_11dd),
        (Register::Ax, 0xaabb_2211),
        (Register::Eax, 0x4433_2211),
    ] {
        let mut runner = Runner::new(&program(target, address(0x503)), true);
        runner.set(initial(), &descriptor());
        runner.bytes()[RAM + 3..RAM + 7].copy_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        assert_eq!(runner.frame().gpr[0], expected);
        assert_eq!(runner.frame().eflags, initial().eflags);
        assert_eq!(runner.frame().charged_work, 1);
        assert_eq!(runner.store.data().admissions, vec![(1, 1)]);
        assert!(runner.store.data().callbacks.is_empty());
        assert_eq!(runner.effects().device_reads, 0);
    }
}

#[test]
fn sib_address_size_masking_and_segment_base_wrap_use_old_full_registers() {
    let cases = [
        (
            MemoryAddress {
                base: Some(Register::Eax),
                index: Some(Register::Ecx),
                scale: 4,
                displacement: 7,
                ..address(0)
            },
            0x500,
            3,
            0,
            0x513,
        ),
        (
            MemoryAddress {
                base: Some(Register::Eax),
                index: Some(Register::Ecx),
                address16: true,
                displacement: 5,
                ..address(0)
            },
            0xffff_ffff,
            0x10500,
            0,
            0x504,
        ),
        (address(0x1503), 0, 0, 0xffff_f000, 0x503),
    ];
    for (effective, eax, ecx, segment_base, linear) in cases {
        let mut descriptor = descriptor();
        descriptor.windows[0][Segment::Ds as usize].base = segment_base;
        let mut frame = initial();
        frame.gpr[0] = eax;
        frame.gpr[1] = ecx;
        let mut runner = Runner::new(&program(Register::Ax, effective), true);
        runner.set(frame, &descriptor);
        let offset = RAM + (linear - 0x500) as usize;
        runner.bytes()[offset..offset + 2].copy_from_slice(&[0x81, 0x72]);
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        assert_eq!(runner.frame().gpr[0], (eax & 0xffff_0000) | 0x7281);
        assert!(runner.store.data().callbacks.is_empty());
    }
}

#[test]
fn inline_linear_device_reads_bank_offset_and_counts_each_ordered_byte() {
    for (target, bytes) in [(Register::Al, 1), (Register::Ax, 2), (Register::Eax, 4)] {
        let mut descriptor = device(DeviceReadKind::Linear);
        descriptor.device.base_offset = 37;
        let mut runner = Runner::new(&program(target, address(0xa0003)), true);
        runner.set(initial(), &descriptor);
        runner.bytes()[VRAM + 40..VRAM + 44].copy_from_slice(&[0x91, 0x82, 0x73, 0x64]);
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        let mask = target.width().mask();
        assert_eq!(
            runner.frame().gpr[0],
            (initial().gpr[0] & !mask) | (0x6473_8291 & mask)
        );
        assert_eq!(runner.effects().device_reads, bytes);
        assert_eq!(runner.effects().latches, [90; 4]);
        assert!(runner.store.data().callbacks.is_empty());
    }
}

#[test]
fn four_lane_chain4_and_planar_reads_update_actual_last_group_latches() {
    for chain4 in [false, true] {
        for plane in 0..4 {
            for target in [Register::Al, Register::Ah, Register::Ax, Register::Eax] {
                let mut descriptor = device(DeviceReadKind::FourLanes);
                descriptor.device.chain4 = chain4;
                descriptor.device.read_plane = plane;
                let mut runner = Runner::new(&program(target, address(0xa0003)), true);
                runner.set(initial(), &descriptor);
                for offset in 0..64 {
                    runner.bytes()[VRAM + offset] = (offset as u8).wrapping_mul(7).wrapping_add(9);
                }
                let bytes = target.width().bits() / 8;
                let mut value = 0;
                let mut latches = [0; 4];
                for byte in 0..bytes {
                    let offset = 3 + byte;
                    let group = if chain4 { offset & !3 } else { offset * 4 };
                    latches.copy_from_slice(
                        &runner.bytes()[VRAM + group as usize..VRAM + group as usize + 4],
                    );
                    value |= u32::from(
                        latches[if chain4 {
                            (offset & 3) as usize
                        } else {
                            plane as usize
                        }],
                    ) << (byte * 8);
                }
                assert_eq!(runner.call(), abi::EXIT_BUDGET);
                let mask = target.width().mask() << target.shift();
                assert_eq!(
                    runner.frame().gpr[0],
                    (initial().gpr[0] & !mask) | (value << target.shift())
                );
                assert_eq!(runner.effects().device_reads, u64::from(bytes));
                assert_eq!(runner.effects().latches, latches);
                assert!(runner.store.data().callbacks.is_empty());
            }
        }
    }
}

#[test]
fn four_lane_read_compare_matches_care_and_compare_bits_for_every_plane() {
    let latches = [0xa5, 0x73, 0xc9, 0x2e];
    // Exhaust every color/care combination. Reuse the actual compiled Wasm.
    let mut runner = Runner::new(&program(Register::Al, address(0xa0001)), true);
    for compare in 0..16 {
        for care in 0..16 {
            let mut descriptor = device(DeviceReadKind::FourLanes);
            descriptor.device.compare_enabled = true;
            descriptor.device.compare = compare;
            descriptor.device.care = care;
            runner.set(initial(), &descriptor);
            runner.bytes()[VRAM + 4..VRAM + 8].copy_from_slice(&latches);
            let mut expected = 255;
            for (plane, latch) in latches.iter().enumerate() {
                if care & (1 << plane) != 0 {
                    expected &= *latch ^ if compare & (1 << plane) != 0 { 0 } else { 255 };
                }
            }
            assert_eq!(runner.call(), abi::EXIT_BUDGET);
            assert_eq!(runner.frame().gpr[0] & 255, u32::from(expected));
            assert_eq!(runner.effects().latches, latches);
        }
    }
    assert!(runner.store.data().callbacks.is_empty());
}

#[test]
fn wide_compare_reads_each_group_in_order_and_accumulates_a_wide_counter() {
    for chain4 in [false, true] {
        let mut descriptor = device(DeviceReadKind::FourLanes);
        descriptor.device.chain4 = chain4;
        descriptor.device.compare_enabled = true;
        descriptor.device.compare = 5;
        descriptor.device.care = 13;
        let mut runner = Runner::new(&program(Register::Eax, address(0xa0003)), false);
        runner.set(initial(), &descriptor);
        for offset in 0..64 {
            runner.bytes()[VRAM + offset] = (offset as u8).wrapping_mul(53).wrapping_add(17);
        }
        let counter = FRAME + memory::OFFSET as usize + memory::DEVICE_READS as usize;
        runner.bytes()[counter..counter + 8].copy_from_slice(&0x1_ffff_fffeu64.to_le_bytes());
        let mut expected = 0;
        let mut last_latches = [0; 4];
        for byte in 0..4 {
            let offset = 3 + byte;
            let group = if chain4 { offset & !3 } else { offset * 4 };
            last_latches.copy_from_slice(&runner.bytes()[VRAM + group..VRAM + group + 4]);
            let value = last_latches[0] & last_latches[2] & !last_latches[3];
            expected |= u32::from(value) << (byte * 8);
        }
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        assert_eq!(runner.frame().gpr[0], expected);
        assert_eq!(runner.effects().latches, last_latches);
        assert_eq!(runner.effects().device_reads, 0x2_0000_0002);
        assert!(runner.store.data().admissions.is_empty());
        assert!(runner.store.data().callbacks.is_empty());
    }
}

#[test]
fn the_last_wide_device_byte_is_preflighted_before_any_latch_or_admission() {
    for (kind, chain4, backing_length) in [
        (DeviceReadKind::Linear, false, 6),
        (DeviceReadKind::FourLanes, true, 7),
        (DeviceReadKind::FourLanes, false, 27),
    ] {
        let mut descriptor = device(kind);
        descriptor.device.chain4 = chain4;
        descriptor.device.backing_length = backing_length;
        let mut runner = Runner::new(&program(Register::Eax, address(0xa0003)), true);
        runner.set(initial(), &descriptor);
        runner.bytes()[VRAM..VRAM + 64].fill(255);
        refused(&mut runner, initial(), 0);
        assert_eq!(runner.effects().latches, [90; 4]);
    }
}

#[test]
fn a_successful_isolated_load_can_exit_at_an_explicit_fallback_boundary() {
    let mut program = program(Register::Al, address(0x503));
    let next = Address {
        space: 7,
        offset: 19,
    };
    program.regions[0].terminator = Terminator::Fallback(next);
    let mut runner = Runner::new(&program, false);
    let mut frame = initial();
    frame.remaining_budget = 2;
    runner.set(frame, &descriptor());
    runner.bytes()[RAM + 3] = 0x12;
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.frame().gpr[0], 0xaabb_cc12);
    assert_eq!(runner.frame().address, next);
    assert_eq!(runner.frame().charged_work, 1);
    assert_eq!(runner.frame().remaining_budget, 1);
    assert_eq!(
        runner.store.data().callbacks,
        vec![(abi::REASON_FALLBACK, runner.frame())]
    );
}

fn refused(runner: &mut Runner, frame: Frame, expected_admissions: usize) {
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.frame(), frame);
    assert_eq!(runner.store.data().admissions.len(), expected_admissions);
    assert_eq!(
        runner.store.data().callbacks,
        vec![(abi::REASON_FALLBACK, frame)]
    );
    assert_eq!(runner.effects().device_reads, 0);
}

#[test]
fn segment_windows_linear_overflow_and_ram_boundaries_refuse_before_admission() {
    let cases = [
        (
            0x503,
            SegmentBounds {
                valid: false,
                base: 0,
                first: 0,
                last: u32::MAX,
            },
        ),
        (
            0x503,
            SegmentBounds {
                valid: true,
                base: 0,
                first: 0x504,
                last: u32::MAX,
            },
        ),
        (
            0x503,
            SegmentBounds {
                valid: true,
                base: 0,
                first: 0,
                last: 0x505,
            },
        ),
        (
            u32::MAX - 1,
            SegmentBounds {
                valid: true,
                base: 0,
                first: 0,
                last: u32::MAX,
            },
        ),
        (
            0x503,
            SegmentBounds {
                valid: true,
                base: u32::MAX - 0x504,
                first: 0,
                last: u32::MAX,
            },
        ),
        (
            0x4ff,
            SegmentBounds {
                valid: true,
                base: 0,
                first: 0,
                last: u32::MAX,
            },
        ),
        (
            0x24fe,
            SegmentBounds {
                valid: true,
                base: 0,
                first: 0,
                last: u32::MAX,
            },
        ),
    ];
    for (offset, window) in cases {
        let mut descriptor = descriptor();
        descriptor.windows[0][Segment::Ds as usize] = window;
        let mut runner = Runner::new(&program(Register::Eax, address(offset)), true);
        runner.set(initial(), &descriptor);
        refused(&mut runner, initial(), 0);
    }
}

#[test]
fn ram_exclusions_and_mixed_ram_device_accesses_refuse_before_admission() {
    for offset in [0x502, 0x504, 0x507] {
        let mut descriptor = descriptor();
        descriptor.ram.excluded_start = 0x505;
        descriptor.ram.excluded_length = 4;
        let mut runner = Runner::new(&program(Register::Eax, address(offset)), true);
        runner.set(initial(), &descriptor);
        refused(&mut runner, initial(), 0);
    }
    // The device aperture is excluded even if an inconsistent RAM descriptor
    // omits its exclusion. A crossing access cannot use either backing.
    let mut descriptor = device(DeviceReadKind::Linear);
    descriptor.ram.linear_base = 0x9f000;
    for offset in [0x9fffd, 0xafffe] {
        let mut runner = Runner::new(&program(Register::Eax, address(offset)), true);
        runner.set(initial(), &descriptor);
        refused(&mut runner, initial(), 0);
    }
}

#[test]
fn descriptor_version_missing_backing_and_overflow_refuse_without_effects() {
    for case in 0..9 {
        let mut descriptor = device(DeviceReadKind::FourLanes);
        let mut offset = 0xa0001;
        match case {
            0 => descriptor.device.kind = DeviceReadKind::Disabled,
            1 => descriptor.device.backing_length = 7,
            2 => descriptor.device.pointer = 65530,
            3 => descriptor.device.read_plane = 4,
            4 => {
                descriptor.device.kind = DeviceReadKind::Linear;
                descriptor.device.base_offset = u32::MAX;
            }
            5 => {
                descriptor.device.guest_length = u32::MAX;
                offset = 0x8000_0000;
            }
            6 => {
                descriptor.ram.pointer = 65535;
                offset = 0x500;
            }
            7 => {
                descriptor.ram.pointer = u32::MAX;
                offset = 0x503;
            }
            8 => {}
            _ => unreachable!(),
        }
        let mut runner = Runner::new(&program(Register::Eax, address(offset)), true);
        runner.set(initial(), &descriptor);
        if case == 8 {
            runner.bytes()[FRAME + memory::OFFSET as usize..FRAME + memory::OFFSET as usize + 4]
                .fill(0);
        }
        refused(&mut runner, initial(), 0);
        assert_eq!(runner.effects().latches, [90; 4]);
    }
}

#[test]
fn a_truncated_descriptor_at_memory_end_uses_fallback_without_trapping() {
    let mut runner = Runner::new(&program(Register::Eax, address(0x503)), true);
    let pointer = 65536 - abi::FRAME_BYTES;
    initial().write(&mut runner.bytes()[pointer..]).unwrap();
    assert_eq!(
        runner.run.call(&mut runner.store, pointer as u32).unwrap(),
        abi::EXIT_RUNTIME
    );
    assert!(runner.store.data().admissions.is_empty());
    assert_eq!(
        runner.store.data().callbacks,
        vec![(abi::REASON_FALLBACK, initial())]
    );
}

#[test]
fn admission_refusal_has_no_device_effects_or_completed_work() {
    let mut runner = Runner::new(&program(Register::Eax, address(0xa0003)), true);
    runner.set(initial(), &device(DeviceReadKind::FourLanes));
    runner.bytes()[VRAM..VRAM + 64].fill(255);
    runner.store.data_mut().refuse = true;
    refused(&mut runner, initial(), 1);
    assert_eq!(runner.effects().latches, [90; 4]);
}

#[test]
fn refusal_after_a_direct_edge_flushes_prior_work_without_reserving_failed_load() {
    let program = Program {
        regions: vec![
            region(1, Register::Al, address(0x503), 2),
            region(2, Register::Ax, address(0x2500), 1),
        ],
    };
    let mut runner = Runner::new(&program, true);
    let mut frame = initial();
    frame.remaining_budget = 3;
    runner.set(frame, &descriptor());
    runner.bytes()[RAM + 3] = 19;
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    let expected = Frame {
        gpr: [
            0xaabb_cc13,
            frame.gpr[1],
            frame.gpr[2],
            frame.gpr[3],
            frame.gpr[4],
            frame.gpr[5],
            frame.gpr[6],
            frame.gpr[7],
        ],
        address: Address {
            space: 7,
            offset: 32,
        },
        remaining_budget: 2,
        charged_work: 1,
        ..frame
    };
    assert_eq!(runner.frame(), expected);
    assert_eq!(runner.store.data().admissions, vec![(1, 1)]);
    assert_eq!(
        runner.store.data().callbacks,
        vec![(abi::REASON_FALLBACK, expected)]
    );
}

#[test]
fn memory_validation_is_opt_in_and_rejects_mixed_or_uncounted_regions() {
    let mut program = program(Register::Al, address(0x503));
    let error = |program: &Program, options: &CompileOptions| program.compile(options).unwrap_err();
    assert_eq!(
        error(&program, &CompileOptions::default()),
        CompileError::DirectMemoryDisabled(RegionId(1))
    );
    for invalid in [
        MemoryAddress {
            scale: 3,
            ..address(0)
        },
        MemoryAddress {
            scale: 2,
            address16: true,
            ..address(0)
        },
        MemoryAddress {
            base: Some(Register::Ax),
            ..address(0)
        },
        MemoryAddress {
            index: Some(Register::Ah),
            ..address(0)
        },
    ] {
        let invalid = self::program(Register::Al, invalid);
        assert_eq!(
            error(&invalid, &options(false)),
            CompileError::InvalidMemoryAddress(RegionId(1))
        );
    }
    program.regions[0].operations.push(Operation::Nop);
    assert_eq!(
        error(&program, &options(false)),
        CompileError::MixedMemoryRegion(RegionId(1))
    );
    program.regions[0].operations.pop();
    program.regions[0].terminator = Terminator::Jump(RegionId(1));
    assert_eq!(
        error(&program, &options(false)),
        CompileError::MixedMemoryRegion(RegionId(1))
    );
    program.regions[0].terminator = Terminator::Fallthrough(RegionId(1));
    program.regions[0].accounting = Accounting::UncountedHot;
    assert_eq!(
        error(&program, &options(false)),
        CompileError::MemoryUncountedHot(RegionId(1))
    );
}

#[test]
fn enabling_direct_memory_preserves_register_only_program_bytes() {
    let program = Program {
        regions: vec![Region {
            operations: vec![Operation::Inc(Register::Eax)],
            ..region(1, Register::Al, address(0), 1)
        }],
    };
    assert_eq!(
        program.compile(&CompileOptions::default()).unwrap().wasm(),
        program.compile(&options(false)).unwrap().wasm()
    );
}
