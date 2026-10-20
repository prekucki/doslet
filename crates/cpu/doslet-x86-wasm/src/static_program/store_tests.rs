extern crate std;

use super::*;
use alloc::{vec, vec::Vec};
use memory::{Descriptor, MemoryAddress, RamDescriptor, RegionGuard, Segment, SegmentBounds};
use wasmi::{Caller, Config, Engine, Linker, Memory, MemoryType, Module, Store, TypedFunc};

#[path = "memory_rmw_tests.rs"]
mod memory_rmw_tests;
#[path = "memory_write_helper_tests.rs"]
mod memory_write_helper_tests;
#[path = "stack_tests.rs"]
mod stack_tests;

const FRAME: usize = 256;
const RAM: usize = 4096;
const GENERATIONS: usize = 24576;
const CODE_PAGES: usize = 24832;
const GUARDS: usize = 25088;
const RAM_BASE: u32 = 0x500;
const RAM_LENGTH: usize = 0x4000;
const PAGE_COUNT: u32 = 5;
const HELPER_STORAGE: usize = 30000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HelperCall {
    linear: u32,
    width: u32,
    value: u32,
    frame: Frame,
    effects: memory::Effects,
}

#[derive(Default)]
struct State {
    callbacks: Vec<(u32, Frame)>,
    admissions: Vec<(u32, u32)>,
    refuse: bool,
    refuse_region: Option<u32>,
    continue_services: usize,
    helper_calls: Vec<HelperCall>,
    helper_status: Option<u32>,
    helper_grant: Option<u32>,
    helper_committed: u64,
    helper_invalidate_guard: Option<u32>,
}

struct Runner {
    store: Store<State>,
    memory: Memory,
    run: TypedFunc<u32, u32>,
}

fn options(inline: bool) -> CompileOptions {
    CompileOptions {
        direct_memory: true,
        runtime_admission: !inline,
        inline_admission: inline,
        ..CompileOptions::default()
    }
}

impl Runner {
    fn new(program: &Program, inline: bool) -> Self {
        Self::with_aliases(program, options(inline), &[])
    }

    fn with_aliases(program: &Program, options: CompileOptions, aliases: &[EntryAlias]) -> Self {
        let compiled = program.compile_with_aliases(&options, aliases).unwrap();
        let mut config = Config::default();
        config.wasm_tail_call(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, compiled.wasm()).unwrap();
        assert_eq!(
            module.imports().count(),
            2 + usize::from(options.runtime_admission) + usize::from(options.memory_write_helper)
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
                    if reason == abi::REASON_SERVICE && caller.data().continue_services != 0 {
                        caller.data_mut().continue_services -= 1;
                        abi::CONTINUE
                    } else {
                        abi::EXIT
                    }
                },
            )
            .unwrap();
        if options.runtime_admission {
            linker
                .func_wrap(
                    abi::IMPORT_MODULE,
                    abi::ADMIT_IMPORT,
                    |mut caller: Caller<'_, State>, _: u32, id: u32, count: u32| -> u32 {
                        caller.data_mut().admissions.push((id, count));
                        u32::from(!caller.data().refuse && caller.data().refuse_region != Some(id))
                    },
                )
                .unwrap();
        }
        if options.memory_write_helper {
            linker
                .func_wrap(
                    abi::IMPORT_MODULE,
                    abi::MEMORY_WRITE_IMPORT,
                    |mut caller: Caller<'_, State>,
                     pointer: u32,
                     linear: u32,
                     width: u32,
                     value: u32|
                     -> u32 {
                        let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                        let bytes = &memory.data(&caller)[pointer as usize..];
                        let frame = Frame::read(bytes).unwrap();
                        let effects =
                            memory::Effects::read(&bytes[memory::OFFSET as usize..]).unwrap();
                        let status = caller.data().helper_status.unwrap_or(1);
                        let grant = caller.data().helper_grant;
                        let invalidate = caller.data().helper_invalidate_guard;
                        caller.data_mut().helper_calls.push(HelperCall {
                            linear,
                            width,
                            value,
                            frame,
                            effects,
                        });
                        if status == 1 {
                            caller.data_mut().helper_committed +=
                                u64::from(effects.inline_reserved);
                            let bytes = memory.data_mut(&mut caller);
                            let descriptor = pointer as usize + memory::OFFSET as usize;
                            memory::Effects::clear(&mut bytes[descriptor..]).unwrap();
                            if let Some(grant) = grant {
                                let offset = descriptor + memory::INLINE_GRANT as usize;
                                bytes[offset..offset + 4].copy_from_slice(&grant.to_le_bytes());
                            }
                            if let Some(id) = invalidate {
                                bytes[GUARDS + id as usize * memory::REGION_GUARD_BYTES..][..4]
                                    .fill(0);
                            }
                            // Model an accepted ordinary bus write into fixed fixture
                            // storage; the runtime tests cover real RAM/VGA routing.
                            bytes[HELPER_STORAGE..HELPER_STORAGE + width as usize]
                                .copy_from_slice(&value.to_le_bytes()[..width as usize]);
                        }
                        status
                    },
                )
                .unwrap();
        }
        let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
        let run = instance
            .get_typed_func::<u32, u32>(&store, abi::RUN_EXPORT)
            .unwrap();
        Self { store, memory, run }
    }

    fn bytes(&mut self) -> &mut [u8] {
        self.memory.data_mut(&mut self.store)
    }

    fn set(&mut self, program: &Program, frame: Frame, descriptor: &Descriptor) {
        frame.write(&mut self.bytes()[FRAME..]).unwrap();
        descriptor
            .write(&mut self.bytes()[FRAME + memory::OFFSET as usize..])
            .unwrap();
        self.bytes()[RAM..RAM + RAM_LENGTH].fill(0x5a);
        self.bytes()[CODE_PAGES..CODE_PAGES + PAGE_COUNT as usize].fill(0);
        // Every prepared source page is protected in ordinary fixtures.
        self.bytes()[CODE_PAGES + 4] = 1;
        for page in 0..PAGE_COUNT {
            self.set_generation(page, 11 + u64::from(page));
        }
        for region in &program.regions {
            self.guard(
                region.id.0,
                RegionGuard {
                    valid: true,
                    pages: [4, 4],
                    generations: [15, 15],
                },
            );
        }
    }

    fn word(&mut self, offset: u32, value: u32) {
        let offset = FRAME + memory::OFFSET as usize + offset as usize;
        self.bytes()[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn guard(&mut self, id: u32, guard: RegionGuard) {
        guard
            .write(&mut self.bytes()[GUARDS + id as usize * memory::REGION_GUARD_BYTES..])
            .unwrap();
    }

    fn set_generation(&mut self, page: u32, generation: u64) {
        let offset = GENERATIONS + page as usize * 8;
        self.bytes()[offset..offset + 8].copy_from_slice(&generation.to_le_bytes());
    }

    fn generation(&self, page: u32) -> u64 {
        let offset = GENERATIONS + page as usize * 8;
        u64::from_le_bytes(
            self.memory.data(&self.store)[offset..offset + 8]
                .try_into()
                .unwrap(),
        )
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

    fn assert_refused(&mut self, frame: Frame) {
        let before = self.memory.data(&self.store).to_vec();
        let effects = self.effects();
        assert_eq!(self.call(), abi::EXIT_RUNTIME);
        assert_eq!(self.frame(), frame);
        assert_eq!(self.effects(), effects);
        assert_eq!(self.memory.data(&self.store), before);
        assert!(self.store.data().admissions.is_empty());
        assert_eq!(
            self.store.data().callbacks,
            vec![(abi::REASON_FALLBACK, frame)]
        );
    }
}

fn descriptor() -> Descriptor {
    Descriptor {
        ram: RamDescriptor {
            pointer: RAM as u32,
            linear_base: RAM_BASE,
            length: RAM_LENGTH as u32,
            generations_pointer: GENERATIONS as u32,
            generations_length: PAGE_COUNT,
            code_pages_pointer: CODE_PAGES as u32,
            code_pages_length: PAGE_COUNT,
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
        region_guards_length: 2,
        ..Descriptor::default()
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

fn entry(id: u32) -> Address {
    Address {
        space: 7,
        offset: id * 16,
    }
}

fn initial() -> Frame {
    Frame {
        gpr: [
            0xaabb_ccdd,
            0x1122_3344,
            0x5566_7788,
            0x99aa_bbcc,
            0x500,
            0x600,
            0x700,
            0x800,
        ],
        address: entry(0),
        remaining_budget: 1,
        eflags: 0x8d7,
        ..Frame::default()
    }
}

fn program(width: Width, source: Source, address: MemoryAddress) -> Program {
    Program {
        regions: vec![Region {
            id: RegionId(0),
            entry: entry(0),
            operations: vec![Operation::StoreMemory {
                address,
                width,
                source,
            }],
            terminator: Terminator::Fallthrough(RegionId(0)),
            accounting: Accounting::Counted,
        }],
    }
}

#[test]
fn direct_stores_write_exact_widths_and_read_register_aliases_without_changing_cpu_state() {
    for (width, source, expected) in [
        (Width::Byte, Source::Immediate(0x81), 0x81u32),
        (Width::Word, Source::Immediate(0x8291), 0x8291),
        (Width::Dword, Source::Immediate(0x8473_6291), 0x8473_6291),
        (Width::Byte, Source::Register(Register::Al), 0xdd),
        (Width::Byte, Source::Register(Register::Ah), 0xcc),
        (Width::Byte, Source::Register(Register::Ch), 0x33),
        (Width::Word, Source::Register(Register::Ax), 0xccdd),
        (Width::Word, Source::Register(Register::Cx), 0x3344),
        (Width::Dword, Source::Register(Register::Eax), 0xaabb_ccdd),
        (Width::Dword, Source::Register(Register::Ecx), 0x1122_3344),
    ] {
        for inline in [false, true] {
            let program = program(width, source, address(0x503));
            let mut runner = Runner::new(&program, inline);
            runner.set(&program, initial(), &descriptor());
            let counter = FRAME + memory::OFFSET as usize + memory::RAM_WRITES as usize;
            runner.bytes()[counter..counter + 8].copy_from_slice(&0x1_ffff_ffffu64.to_le_bytes());
            assert_eq!(runner.call(), abi::EXIT_BUDGET);
            let bytes = width.bits() as usize / 8;
            assert_eq!(
                &runner.bytes()[RAM + 3..RAM + 3 + bytes],
                &expected.to_le_bytes()[..bytes]
            );
            assert_eq!(runner.bytes()[RAM + 2], 0x5a);
            assert_eq!(runner.bytes()[RAM + 3 + bytes], 0x5a);
            let mut expected_frame = initial();
            expected_frame.remaining_budget = 0;
            expected_frame.charged_work = 1;
            assert_eq!(runner.frame(), expected_frame);
            assert_eq!(runner.generation(0), 12);
            assert_eq!(runner.generation(1), 12);
            assert_eq!(runner.effects().ram_writes, 0x2_0000_0000);
            assert_eq!(runner.effects().device_reads, 0);
            assert_eq!(runner.effects().inline_reserved, u32::from(inline));
            assert_eq!(runner.effects().inline_remaining, 100 - u32::from(inline));
            assert_eq!(
                runner.store.data().admissions,
                if inline { vec![] } else { vec![(0, 1)] }
            );
            assert!(runner.store.data().callbacks.is_empty());
        }
    }
}

#[test]
fn stores_increment_each_distinct_physical_page_once_including_unaligned_crossings() {
    for (width, linear, pages) in [
        (Width::Byte, 0xfff, 1),
        (Width::Word, 0xffe, 1),
        (Width::Dword, 0xffc, 1),
        (Width::Word, 0xfff, 2),
        (Width::Dword, 0xffd, 2),
        (Width::Dword, 0xfff, 2),
    ] {
        let value = 0x8172_6354 & width.mask();
        let program = program(width, Source::Immediate(value), address(linear));
        let mut runner = Runner::new(&program, true);
        runner.set(&program, initial(), &descriptor());
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        let start = RAM + (linear - RAM_BASE) as usize;
        assert_eq!(
            &runner.bytes()[start..start + width.bits() as usize / 8],
            &value.to_le_bytes()[..width.bits() as usize / 8]
        );
        assert_eq!(runner.generation(0), 12);
        assert_eq!(runner.generation(1), 12 + u64::from(pages == 2));
        assert_eq!(runner.effects().ram_writes, pages);
        assert_eq!(runner.effects().inline_reserved, 1);
    }
}

#[test]
fn store_preflight_rejects_malformed_full_tables_and_ram_spans_before_admission() {
    let program = program(Width::Dword, Source::Immediate(0x1234_5678), address(0xfff));
    for inline in [false, true] {
        for (offset, value) in [
            (memory::VERSION_OFFSET, memory::VERSION + 1),
            (memory::RAM_POINTER, u32::MAX),
            (memory::RAM_POINTER, 65536 - RAM_LENGTH as u32 + 1),
            (memory::RAM_BASE, RAM_BASE - 1),
            (memory::RAM_LENGTH, u32::MAX),
            (memory::GENERATIONS_POINTER, u32::MAX),
            (memory::GENERATIONS_POINTER, 65536 - PAGE_COUNT * 8 + 1),
            (memory::GENERATIONS_LENGTH, u32::MAX),
            (memory::GENERATIONS_LENGTH, 0),
            (memory::GENERATIONS_LENGTH, 1),
            (memory::CODE_PAGES_POINTER, u32::MAX),
            (memory::CODE_PAGES_POINTER, 65536 - PAGE_COUNT + 1),
            (memory::CODE_PAGES_LENGTH, u32::MAX),
            (memory::CODE_PAGES_LENGTH, 0),
            (memory::CODE_PAGES_LENGTH, 1),
        ] {
            let mut runner = Runner::new(&program, inline);
            runner.set(&program, initial(), &descriptor());
            runner.word(offset, value);
            runner.assert_refused(initial());
        }
    }
}

#[test]
fn protected_or_exhausted_second_page_refuses_before_any_partial_store_or_reservation() {
    let program = program(Width::Dword, Source::Immediate(0x1234_5678), address(0xfff));
    for inline in [false, true] {
        for page in [0, 1] {
            for protected in [false, true] {
                let mut runner = Runner::new(&program, inline);
                runner.set(&program, initial(), &descriptor());
                if protected {
                    runner.bytes()[CODE_PAGES + page as usize] = 2;
                } else {
                    runner.set_generation(page, u64::MAX);
                }
                runner.assert_refused(initial());
            }
        }
    }
}

#[test]
fn stores_refuse_read_only_segments_vga_exclusions_and_cross_span_operands() {
    for linear in [0x503, 0x4500 - 2, 0x4ff, 0xa0000, 0x9ffff, u32::MAX - 1] {
        let program = program(
            Width::Dword,
            Source::Immediate(0x1234_5678),
            address(linear),
        );
        let mut descriptor = descriptor();
        descriptor.device = memory::DeviceReadDescriptor {
            guest_start: 0xa0000,
            guest_length: 0x10000,
            pointer: 32768,
            backing_length: 8192,
            kind: memory::DeviceReadKind::FourLanes,
            latches: [90; 4],
            ..memory::DeviceReadDescriptor::default()
        };
        if linear == 0x503 {
            // The readable window remains valid: stores must select access=1.
            descriptor.windows[1][Segment::Ds as usize].valid = false;
        }
        let mut runner = Runner::new(&program, true);
        runner.set(&program, initial(), &descriptor);
        runner.bytes()[32768..40960].fill(0xb7);
        runner.assert_refused(initial());
        assert_eq!(runner.effects().latches, [90; 4]);
    }
    // An otherwise ordinary RAM span must still reject its excluded interval.
    let program = program(Width::Dword, Source::Immediate(1), address(0x503));
    let mut runner = Runner::new(&program, true);
    let mut descriptor = descriptor();
    descriptor.ram.excluded_start = 0x505;
    descriptor.ram.excluded_length = 1;
    runner.set(&program, initial(), &descriptor);
    runner.assert_refused(initial());
}

#[test]
fn device_apertures_refuse_even_when_the_ram_span_contains_the_whole_operand() {
    for linear in [0x9ffff, 0xa0000] {
        let program = program(
            Width::Dword,
            Source::Immediate(0x1234_5678),
            address(linear),
        );
        let mut descriptor = descriptor();
        descriptor.ram.linear_base = 0x9f000;
        descriptor.ram.generations_pointer = 40960;
        descriptor.ram.generations_length = 256;
        descriptor.ram.code_pages_pointer = 43008;
        descriptor.ram.code_pages_length = 256;
        descriptor.device = memory::DeviceReadDescriptor {
            guest_start: 0xa0000,
            guest_length: 0x10000,
            pointer: 32768,
            backing_length: 8192,
            kind: memory::DeviceReadKind::FourLanes,
            latches: [90; 4],
            ..memory::DeviceReadDescriptor::default()
        };
        let mut runner = Runner::new(&program, false);
        runner.set(&program, initial(), &descriptor);
        runner.bytes()[32768..40960].fill(0xb7);
        runner.assert_refused(initial());
    }
}

#[test]
fn a_completed_store_exits_at_its_explicit_fallback_boundary_with_one_instruction_charged() {
    let mut program = program(Width::Byte, Source::Immediate(0x81), address(0x503));
    program.regions[0].terminator = Terminator::Fallback(entry(1));
    let mut runner = Runner::new(&program, true);
    let frame = Frame {
        remaining_budget: 2,
        ..initial()
    };
    runner.set(&program, frame, &descriptor());
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.bytes()[RAM + 3], 0x81);
    assert_eq!(runner.frame().address, entry(1));
    assert_eq!(runner.frame().charged_work, 1);
    assert_eq!(runner.frame().remaining_budget, 1);
    assert_eq!(runner.effects().ram_writes, 1);
    assert_eq!(runner.effects().inline_reserved, 1);
    assert_eq!(
        runner.store.data().callbacks,
        vec![(abi::REASON_FALLBACK, runner.frame())]
    );
}

#[test]
fn store_refusal_after_prior_work_flushes_current_entry_without_reserving_the_store() {
    let mut program = program(Width::Dword, Source::Immediate(1), address(0x503));
    program.regions[0].id = RegionId(1);
    program.regions[0].entry = entry(1);
    program.regions.insert(
        0,
        Region {
            id: RegionId(0),
            entry: entry(0),
            operations: vec![Operation::Inc(Register::Ecx)],
            terminator: Terminator::Fallthrough(RegionId(1)),
            accounting: Accounting::Counted,
        },
    );
    let mut runner = Runner::new(&program, true);
    let frame = Frame {
        remaining_budget: 2,
        ..initial()
    };
    runner.set(&program, frame, &descriptor());
    runner.set_generation(0, u64::MAX);
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.bytes()[RAM + 3], 0x5a);
    assert_eq!(runner.generation(0), u64::MAX);
    assert_eq!(runner.frame().gpr[1], frame.gpr[1] + 1);
    assert_eq!(runner.frame().address, entry(1));
    assert_eq!(runner.frame().charged_work, 1);
    assert_eq!(runner.frame().remaining_budget, 1);
    assert_eq!(runner.effects().ram_writes, 0);
    assert_eq!(runner.effects().inline_reserved, 1);
    assert_eq!(runner.effects().inline_remaining, 99);
}

#[test]
fn store_source_and_effective_address_use_old_registers_and_the_write_segment_window() {
    let program = program(
        Width::Byte,
        Source::Register(Register::Ah),
        MemoryAddress {
            base: Some(Register::Eax),
            index: Some(Register::Ecx),
            displacement: 3,
            address16: true,
            ..address(0)
        },
    );
    let mut frame = initial();
    frame.gpr[0] = 0x1234_fffe;
    frame.gpr[1] = 0x5678_0500;
    let mut descriptor = descriptor();
    descriptor.windows[0][Segment::Ds as usize].valid = false;
    descriptor.windows[1][Segment::Ds as usize].base = 0x1000;
    let mut runner = Runner::new(&program, true);
    runner.set(&program, frame, &descriptor);
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    assert_eq!(runner.bytes()[RAM + 0x1001], 0xff);
    assert_eq!(runner.frame().gpr, frame.gpr);
    assert_eq!(runner.frame().eflags, frame.eflags);
    assert_eq!(runner.generation(0), 11);
    assert_eq!(runner.generation(1), 13);
}

#[test]
fn inline_store_reservation_waits_for_source_guards_and_runtime_admission_can_refuse() {
    let program = program(Width::Dword, Source::Immediate(0x1234_5678), address(0x503));
    let mut runner = Runner::new(&program, true);
    runner.set(&program, initial(), &descriptor());
    runner.guard(
        0,
        RegionGuard {
            valid: false,
            ..RegionGuard::default()
        },
    );
    runner.assert_refused(initial());
    let mut runner = Runner::new(&program, false);
    runner.set(&program, initial(), &descriptor());
    runner.store.data_mut().refuse = true;
    let before = runner.memory.data(&runner.store).to_vec();
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.memory.data(&runner.store), before);
    assert_eq!(runner.store.data().admissions, vec![(0, 1)]);
}

#[test]
fn store_updates_actual_generations_before_the_next_region_source_guard() {
    let mut program = program(Width::Dword, Source::Immediate(0x1234_5678), address(0x503));
    program.regions[0].terminator = Terminator::Fallthrough(RegionId(1));
    program.regions.push(Region {
        id: RegionId(1),
        entry: entry(1),
        operations: vec![Operation::Inc(Register::Ecx)],
        terminator: Terminator::Fallthrough(RegionId(0)),
        accounting: Accounting::Counted,
    });
    let frame = Frame {
        remaining_budget: 2,
        ..initial()
    };
    let mut runner = Runner::new(&program, true);
    runner.set(&program, frame, &descriptor());
    // Deliberately stale synthetic proof on a writable page verifies that the
    // next guard reads the actual table, independent of the source-page bitmap.
    runner.guard(
        1,
        RegionGuard {
            valid: true,
            pages: [0, 0],
            generations: [11, 11],
        },
    );
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.generation(0), 12);
    assert_eq!(runner.frame().gpr, frame.gpr);
    assert_eq!(runner.frame().address, entry(1));
    assert_eq!(runner.frame().charged_work, 1);
    assert_eq!(runner.frame().remaining_budget, 1);
    assert_eq!(runner.effects().inline_reserved, 1);
    assert_eq!(runner.effects().inline_remaining, 99);
    assert_eq!(runner.effects().ram_writes, 1);
    assert_eq!(
        runner.store.data().callbacks,
        vec![(abi::REASON_FALLBACK, runner.frame())]
    );
}

#[test]
fn store_ir_requires_valid_addresses_matching_sources_and_isolated_counted_regions() {
    let original = program(Width::Byte, Source::Immediate(1), address(0x503));
    assert_eq!(
        original.compile(&CompileOptions::default()).unwrap_err(),
        CompileError::DirectMemoryDisabled(RegionId(0))
    );
    for (width, source, error) in [
        (
            Width::Byte,
            Source::Register(Register::Ax),
            CompileError::WidthMismatch(RegionId(0)),
        ),
        (
            Width::Word,
            Source::Register(Register::Eax),
            CompileError::WidthMismatch(RegionId(0)),
        ),
        (
            Width::Byte,
            Source::Immediate(256),
            CompileError::ImmediateOutOfRange(RegionId(0)),
        ),
        (
            Width::Word,
            Source::Immediate(65536),
            CompileError::ImmediateOutOfRange(RegionId(0)),
        ),
    ] {
        assert_eq!(
            program(width, source, address(0x503))
                .compile(&options(true))
                .unwrap_err(),
            error
        );
    }
    for invalid in [
        MemoryAddress {
            scale: 3,
            ..address(0x503)
        },
        MemoryAddress {
            scale: 2,
            address16: true,
            ..address(0x503)
        },
        MemoryAddress {
            base: Some(Register::Ax),
            ..address(0x503)
        },
        MemoryAddress {
            index: Some(Register::Ah),
            ..address(0x503)
        },
    ] {
        assert_eq!(
            program(Width::Byte, Source::Immediate(1), invalid)
                .compile(&options(true))
                .unwrap_err(),
            CompileError::InvalidMemoryAddress(RegionId(0))
        );
    }
    let mut mixed = original.clone();
    mixed.regions[0].operations.push(Operation::Nop);
    assert_eq!(
        mixed.compile(&options(true)).unwrap_err(),
        CompileError::MixedMemoryRegion(RegionId(0))
    );
    let mut jump = original.clone();
    jump.regions[0].terminator = Terminator::Jump(RegionId(0));
    assert_eq!(
        jump.compile(&options(true)).unwrap_err(),
        CompileError::MixedMemoryRegion(RegionId(0))
    );
    let mut hot = original;
    hot.regions[0].accounting = Accounting::UncountedHot;
    assert_eq!(
        hot.compile(&CompileOptions {
            direct_memory: true,
            ..CompileOptions::default()
        })
        .unwrap_err(),
        CompileError::MemoryUncountedHot(RegionId(0))
    );
}
