use super::*;
use doslet_x86_wasm::static_program::{
    Address, Alu, Operation, Register, Source, Width, abi,
    memory::{MemoryAddress, Segment},
    procedure_memory::{
        ProcedureCompileOptions, ProcedureFunction, ProcedureId, ProcedureInstruction,
        ProcedureProgram, ProcedureReturn,
    },
};
use wasm_encoder::{
    CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection, ImportSection,
    Instruction as W, MemArg, Module, TypeSection, ValType,
};

const RAM_BYTES: usize = 4 * 1024 * 1024;
const ENTRY: u32 = 0x8000;
const DATA: usize = 0x2000;
const STACK: usize = 0x4000;
const RETURN: u32 = 0x9000;

fn address(offset: u32) -> Address {
    Address { space: 1, offset }
}

fn operand(base: Option<Register>, displacement: u32) -> MemoryAddress {
    MemoryAddress {
        base,
        index: None,
        scale: 1,
        displacement,
        address16: false,
        segment: Segment::Ds,
    }
}

fn fixture() -> Vec<u8> {
    ProcedureProgram {
        functions: [(3, 1), (9, 7)]
            .into_iter()
            .map(|(id, increment)| ProcedureFunction {
                id: ProcedureId(id),
                entry: address(ENTRY),
                instructions: vec![
                    ProcedureInstruction {
                        address: address(ENTRY),
                        operation: Operation::LoadMemory {
                            target: Register::Eax,
                            address: operand(None, DATA as u32),
                        },
                    },
                    ProcedureInstruction {
                        address: address(ENTRY + 5),
                        operation: Operation::Alu {
                            kind: Alu::Add,
                            target: Register::Eax,
                            source: Source::Immediate(increment),
                        },
                    },
                    ProcedureInstruction {
                        address: address(ENTRY + 8),
                        operation: Operation::StoreMemory {
                            address: operand(Some(Register::Ebx), 0),
                            width: Width::Dword,
                            source: Source::Register(Register::Eax),
                        },
                    },
                ],
                terminator: ProcedureReturn {
                    address: address(ENTRY + 10),
                    width: Width::Dword,
                    pop_bytes: 0,
                },
            })
            .collect(),
    }
    .compile(&ProcedureCompileOptions::default())
    .unwrap()
    .into_wasm()
}

fn descriptor(owner: &SharedProcedureMemory) -> Descriptor {
    owner.descriptor(
        [[SegmentBounds {
            valid: true,
            base: 0,
            first: 0,
            last: u32::MAX,
        }; 6]; 2],
        StackAddress::Bits32,
    )
}

fn initial() -> Frame {
    let mut frame = Frame {
        address: address(ENTRY),
        remaining_budget: 123,
        ..Frame::default()
    };
    frame.gpr[Register::Ebx as usize] = DATA as u32;
    frame.gpr[Register::Esp as usize] = STACK as u32;
    frame
}

fn owner() -> SharedProcedureMemory {
    let mut owner = SharedProcedureMemory::new(&fixture(), &[3, 9], RAM_BYTES).unwrap();
    owner.ram_mut()[STACK..STACK + 4].copy_from_slice(&RETURN.to_le_bytes());
    owner.protect_source(ENTRY, 11).unwrap();
    owner
}

#[test]
fn native_and_both_procedure_exports_share_ram_and_generations_in_place() {
    fn assert_send<T: Send>() {}
    assert_send::<SharedProcedureMemory>();
    let mut owner = owner();
    let descriptor = descriptor(&owner);
    let ram_pointer = owner.ram().as_ptr();
    let generations_pointer = owner.generations().as_ptr();
    let (ram, generations) = owner.parts_mut();
    ram[DATA..DATA + 4].copy_from_slice(&0x1234_5678_u32.to_le_bytes());
    generations[DATA / CODE_PAGE_BYTES] = 41;
    let mut frame = initial();
    let (exit, effects) = owner.execute(3, &mut frame, &descriptor).unwrap();
    assert_eq!(exit, Exit::Complete);
    assert_eq!(frame.gpr[0], 0x1234_5679);
    assert_eq!(frame.gpr[4], STACK as u32 + 4);
    assert_eq!(frame.address, address(RETURN));
    assert_eq!(frame.charged_work, 4);
    assert_eq!(effects.ram_writes, 1);
    assert_eq!(&owner.ram()[DATA..DATA + 4], &0x1234_5679_u32.to_le_bytes());
    assert_eq!(owner.generations()[DATA / CODE_PAGE_BYTES], 42);

    // A native write and its generation update become the second export's
    // input. Both functions share one instance, RAM image and generation table.
    owner.ram_mut()[DATA..DATA + 4].copy_from_slice(&100_u32.to_le_bytes());
    owner.generations_mut()[DATA / CODE_PAGE_BYTES] = 53;
    let mut frame = initial();
    let (exit, effects) = owner.execute(9, &mut frame, &descriptor).unwrap();
    assert_eq!(exit, Exit::Complete);
    assert_eq!(frame.gpr[0], 107);
    assert_eq!(effects.ram_writes, 1);
    assert_eq!(&owner.ram()[DATA..DATA + 4], &107_u32.to_le_bytes());
    assert_eq!(owner.generations()[DATA / CODE_PAGE_BYTES], 54);
    let layout = owner.layout();
    let bytes = owner.memory.data(&owner.store);
    let generation = layout.generations_pointer as usize + DATA / CODE_PAGE_BYTES * 8;
    assert_eq!(&bytes[generation..generation + 8], &54_u64.to_le_bytes());
    assert_eq!(ram_pointer, owner.ram().as_ptr());
    assert_eq!(generations_pointer, owner.generations().as_ptr());
}

#[test]
fn successful_shared_memory_execution_allocates_nothing_after_setup() {
    let mut owner = owner();
    let descriptor = descriptor(&owner);
    // Warm Wasmtime's first-entry machinery before measuring steady execution.
    owner.execute(3, &mut initial(), &descriptor).unwrap();
    let allocation = allocation_counter::measure(|| {
        for id in [3, 9].into_iter().cycle().take(128) {
            let mut frame = initial();
            let (exit, effects) = owner.execute(id, &mut frame, &descriptor).unwrap();
            assert_eq!(exit, Exit::Complete);
            assert_eq!(effects.ram_writes, 1);
        }
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
    assert_eq!(owner.generations()[DATA / CODE_PAGE_BYTES], 129);
}

#[test]
fn resolved_exports_keep_their_identity_and_repeated_calls_reset_effects_and_grants() {
    let mut owner = owner();
    let mut descriptor = descriptor(&owner);
    descriptor.inline_grant = 123;
    assert_eq!(owner.function_index(3), Some(0));
    assert_eq!(owner.function_index(9), Some(1));
    assert_eq!(owner.function_index(4), None);
    for (index, expected) in [(0, 1u32), (1, 8), (0, 9)] {
        let (_, effects) = owner
            .execute_index(index, &mut initial(), &descriptor)
            .unwrap();
        assert_eq!(effects.ram_writes, 1);
        assert_eq!(effects.inline_remaining, 123);
        assert_eq!(&owner.ram()[DATA..DATA + 4], &expected.to_le_bytes());
        // Generated bounded code consumes its in-memory grant. Model that
        // output here too, since this fixture has a straight-line body.
        let start = memory::OFFSET as usize + memory::INLINE_GRANT as usize;
        owner.memory.data_mut(&mut owner.store)[start..start + 4].fill(0);
    }
    let before = owner.ram()[DATA..DATA + 4].to_vec();
    assert!(owner.execute_index(2, &mut initial(), &descriptor).is_err());
    assert_eq!(&owner.ram()[DATA..DATA + 4], before.as_slice());
    // A formerly valid descriptor must not license an altered fixed mapping.
    descriptor.ram.pointer += 4;
    assert!(owner.execute_index(0, &mut initial(), &descriptor).is_err());
    assert_eq!(&owner.ram()[DATA..DATA + 4], before.as_slice());
}

#[test]
fn repeated_entries_refresh_segment_permissions_limits_and_vga_controls() {
    let mut owner = owner();
    let mut descriptor = descriptor(&owner);
    owner.ram_mut()[DATA..DATA + 4].copy_from_slice(&55u32.to_le_bytes());
    assert_eq!(
        owner
            .execute_index(0, &mut initial(), &descriptor)
            .unwrap()
            .0,
        Exit::Complete
    );
    descriptor.windows[0][Segment::Ds as usize].last = DATA as u32 + 2;
    let (exit, effects) = owner.execute_index(0, &mut initial(), &descriptor).unwrap();
    assert_eq!(exit, Exit::SideExit);
    assert_eq!(effects.ram_writes, 0);
    descriptor.windows[0][Segment::Ds as usize].last = u32::MAX;
    descriptor.windows[1][Segment::Ds as usize].valid = false;
    let (exit, effects) = owner.execute_index(0, &mut initial(), &descriptor).unwrap();
    assert_eq!(exit, Exit::SideExit);
    assert_eq!(effects.ram_writes, 0);
    descriptor.windows[1][Segment::Ds as usize].valid = true;
    assert_eq!(
        owner
            .execute_index(0, &mut initial(), &descriptor)
            .unwrap()
            .0,
        Exit::Complete
    );
    let layout = owner.layout();
    descriptor.device = memory::DeviceReadDescriptor {
        guest_start: VGA_START,
        guest_length: 65536,
        pointer: layout.video_pointer,
        backing_length: layout.video_bytes as u32,
        kind: memory::DeviceReadKind::FourLanes,
        latches: [0x12, 0x34, 0x56, 0x78],
        ..memory::DeviceReadDescriptor::default()
    };
    descriptor.device_write = memory::DeviceWriteDescriptor {
        enabled: true,
        plane_mask: 1,
        mode: 1, // Copy the live latches into enabled planes.
        bit_mask: 255,
        ..memory::DeviceWriteDescriptor::default()
    };
    let mut frame = initial();
    frame.gpr[3] = VGA_START;
    assert_eq!(
        owner.execute_index(0, &mut frame, &descriptor).unwrap().0,
        Exit::Complete
    );
    assert_eq!(&owner.video()[..4], &[0x12, 0, 0, 0]);
    descriptor.device.latches = [0xab, 0xcd, 0xef, 0x98];
    descriptor.device_write.plane_mask = 2;
    let mut frame = initial();
    frame.gpr[3] = VGA_START;
    let (_, effects) = owner.execute_index(0, &mut frame, &descriptor).unwrap();
    assert_eq!(effects.ram_writes, 0);
    assert_eq!(effects.device_writes, 4);
    assert_eq!(&owner.video()[..4], &[0x12, 0xcd, 0, 0]);
}

#[test]
fn protected_store_returns_the_completed_prefix_without_changing_source_bytes() {
    let mut owner = owner();
    let descriptor = descriptor(&owner);
    owner.ram_mut()[DATA..DATA + 4].copy_from_slice(&55_u32.to_le_bytes());
    let mut frame = initial();
    frame.gpr[3] = ENTRY;
    let (exit, effects) = owner.execute(3, &mut frame, &descriptor).unwrap();
    assert_eq!(exit, Exit::SideExit);
    assert_eq!(frame.address, address(ENTRY + 8));
    assert_eq!(frame.gpr[0], 56);
    assert_eq!(frame.gpr[4], STACK as u32);
    assert_eq!(frame.charged_work, 2);
    assert_eq!(effects.ram_writes, 0);
    assert_eq!(&owner.ram()[ENTRY as usize..ENTRY as usize + 4], &[0; 4]);
    assert_eq!(owner.generations()[ENTRY as usize / CODE_PAGE_BYTES], 0);
}

#[test]
fn native_and_wasm_device_stores_share_the_same_physical_video_backing() {
    let mut owner = owner();
    let layout = owner.layout();
    let mut descriptor = descriptor(&owner);
    descriptor.device = memory::DeviceReadDescriptor {
        guest_start: VGA_START,
        guest_length: 65536,
        pointer: layout.video_pointer,
        backing_length: layout.video_bytes as u32,
        kind: memory::DeviceReadKind::FourLanes,
        ..memory::DeviceReadDescriptor::default()
    };
    descriptor.device_write = memory::DeviceWriteDescriptor {
        enabled: true,
        plane_mask: 5,
        bit_mask: 255,
        ..memory::DeviceWriteDescriptor::default()
    };
    owner.ram_mut()[DATA..DATA + 4].copy_from_slice(&55u32.to_le_bytes());
    owner.video_mut().fill(0x11);
    let pointer = owner.video().as_ptr();
    let mut frame = initial();
    frame.gpr[3] = VGA_START + 0x123;
    let (exit, effects) = owner.execute(3, &mut frame, &descriptor).unwrap();
    assert_eq!(exit, Exit::Complete);
    assert_eq!(effects.device_writes, 4);
    assert_eq!(effects.device_changes, 8);
    assert_eq!(effects.ram_writes, 0);
    assert_eq!(owner.video().as_ptr(), pointer);
    for byte in 0..4 {
        let value = if byte == 0 { 56 } else { 0 };
        assert_eq!(
            &owner.video()[(0x123 + byte) * 4..][..4],
            &[value, 0x11, value, 0x11]
        );
    }
    let allocation = allocation_counter::measure(|| {
        let mut frame = initial();
        frame.gpr[3] = VGA_START + 0x123;
        let (_, effects) = owner.execute(3, &mut frame, &descriptor).unwrap();
        assert_eq!(effects.device_changes, 0);
    });
    assert_eq!(allocation.count_total, 0, "{allocation:?}");
    assert_eq!(allocation.bytes_total, 0, "{allocation:?}");
}

#[test]
fn layout_keeps_all_spans_disjoint_even_with_32_mib_ram() {
    for megabytes in [4, 8, 16, 32] {
        let layout = SharedProcedureLayout::new(megabytes * 1024 * 1024).unwrap();
        assert!(abi::FRAME_BYTES + memory::BYTES <= layout.generations_pointer as usize);
        assert_eq!(layout.generations_pointer, GENERATIONS_POINTER as u32);
        assert_eq!(layout.generations_pointer % 8, 0);
        assert_eq!(
            layout.code_pages_pointer,
            layout.generations_pointer + layout.generations_length * 8
        );
        assert!(layout.code_pages_pointer + layout.code_pages_length <= layout.ram_pointer);
        assert_eq!(layout.ram_pointer as usize % WASM_PAGE_BYTES, 0);
        assert_eq!(
            layout.memory_bytes,
            layout.video_pointer as usize + layout.video_bytes
        );
        assert_eq!(
            layout.video_pointer as usize,
            layout.ram_pointer as usize + layout.ram_bytes
        );
        assert_eq!(layout.video_bytes, 512 * 1024);
    }
    let mut owner = SharedProcedureMemory::new(&fixture(), &[3, 9], 32 * 1024 * 1024).unwrap();
    assert_eq!(owner.layout().ram_pointer, 131_072);
    assert_eq!(owner.ram().len(), 32 * 1024 * 1024);
    assert_eq!(owner.generations().len(), 8192);
    let (ram, generations) = owner.parts_mut();
    *ram.last_mut().unwrap() = 0x5a;
    *generations.last_mut().unwrap() = 0x1234_5678_90ab_cdef;
    assert_eq!(*owner.ram().last().unwrap(), 0x5a);
    assert_eq!(*owner.generations().last().unwrap(), 0x1234_5678_90ab_cdef);
    let descriptor = descriptor(&owner);
    assert_eq!(
        descriptor.ram.pointer,
        owner.layout().ram_pointer + RAM_BASE
    );
    assert_eq!(descriptor.ram.linear_base, RAM_BASE);
    assert_eq!(
        descriptor.ram.length as usize,
        owner.ram().len() - RAM_BASE as usize
    );
    assert_eq!(descriptor.ram.excluded_start, VGA_START);
    assert_eq!(descriptor.ram.excluded_length, VGA_END - VGA_START);
}

#[test]
fn setup_bounds_and_fixed_descriptor_are_checked_before_execution() {
    for ids in [&[][..], &[2, 1], &[2, 2]] {
        assert!(SharedProcedureMemory::new(&[], ids, RAM_BYTES).is_err());
    }
    assert!(SharedProcedureMemory::new(&[], &[0; MAX_PROCEDURES + 1], RAM_BYTES).is_err());
    assert!(SharedProcedureMemory::new(&[], &[1], 1024 * 1024).is_err());
    let mut owner = owner();
    for (linear, len) in [
        (0, 1),
        (RAM_BASE - 1, 2),
        (VGA_START, 1),
        (VGA_START - 1, 2),
        (VGA_END - 1, 2),
        (RAM_BYTES as u32 - 1, 2),
        (u32::MAX, 2),
        (ENTRY, 0),
    ] {
        assert!(owner.protect_source(linear, len).is_err());
    }
    owner.protect_source(0x9fff, 2).unwrap();
    let layout = owner.layout();
    let bits = &owner.memory.data(&owner.store)[layout.code_pages_pointer as usize..]
        [..layout.code_pages_length as usize];
    assert_eq!(&bits[8..=10], &[1, 1, 1]);
    let mut descriptor = descriptor(&owner);
    descriptor.ram.generations_pointer += 1;
    assert!(owner.execute(3, &mut initial(), &descriptor).is_err());
    descriptor.ram = layout.ram_descriptor();
    assert!(owner.execute(4, &mut initial(), &descriptor).is_err());
    assert_eq!(
        owner.execute(3, &mut initial(), &descriptor).unwrap().0,
        Exit::Complete
    );
}

fn failure_module(status: Option<u32>) -> Vec<u8> {
    let mut types = TypeSection::new();
    types.ty().function([ValType::I32], [ValType::I32]);
    let mut imports = ImportSection::new();
    imports.import(
        abi::IMPORT_MODULE,
        abi::MEMORY_IMPORT,
        EntityType::Memory(wasm_encoder::MemoryType {
            minimum: 1,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        }),
    );
    let mut functions = FunctionSection::new();
    functions.function(0);
    let mut exports = ExportSection::new();
    exports.export("memory_procedure_7", ExportKind::Func, 0);
    let mut function = Function::new([]);
    for instruction in [
        W::I32Const((WASM_PAGE_BYTES + DATA) as i32),
        W::I32Const((WASM_PAGE_BYTES + DATA) as i32),
        W::I32Load(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }),
        W::I32Const(1),
        W::I32Add,
        W::I32Store(MemArg {
            offset: 0,
            align: 2,
            memory_index: 0,
        }),
    ] {
        function.instruction(&instruction);
    }
    if let Some(status) = status {
        function.instruction(&W::I32Const(status as i32));
    } else {
        function.instruction(&W::Unreachable);
    }
    function.instruction(&W::End);
    let mut code = CodeSection::new();
    code.function(&function);
    let mut module = Module::new();
    module.section(&types);
    module.section(&imports);
    module.section(&functions);
    module.section(&exports);
    module.section(&code);
    module.finish()
}

#[test]
fn traps_and_invalid_status_poison_entry_without_replaying_prior_stores() {
    for status in [None, Some(17)] {
        let mut owner =
            SharedProcedureMemory::new(&failure_module(status), &[7], RAM_BYTES).unwrap();
        let descriptor = descriptor(&owner);
        assert!(owner.execute(7, &mut initial(), &descriptor).is_err());
        assert_eq!(&owner.ram()[DATA..DATA + 4], &1_u32.to_le_bytes());
        let error = owner.execute(7, &mut initial(), &descriptor).unwrap_err();
        assert!(error.contains("stopped"), "{error}");
        assert_eq!(&owner.ram()[DATA..DATA + 4], &1_u32.to_le_bytes());
    }
}
