extern crate std;
use super::super::super::*;
use crate::static_program::{
    Frame,
    memory::{
        self, Descriptor, DeviceReadDescriptor, DeviceReadKind, DeviceWriteDescriptor,
        MemoryAddress, RamDescriptor, Segment, SegmentBounds, StackAddress,
    },
    procedure_memory::{
        COMPLETE, ProcedureCompileOptions, ProcedureFunction, ProcedureId, ProcedureInstruction,
        ProcedureProgram, ProcedureReturn, SIDE_EXIT,
    },
};
use doslet_vga::{APERTURE_LEN, VRAM_LEN, Vga};
use wasmi::{Engine, Linker, Memory, MemoryType, Module, Store, TypedFunc};

const FRAME: usize = 256;
const RAM: usize = 4096;
const GENERATIONS: usize = 24576;
const CODE: usize = 25088;
const VIDEO: usize = 65536;
const ENTRY: Address = Address {
    space: 0,
    offset: 0x8000,
};

struct Runner {
    store: Store<()>,
    memory: Memory,
    run: TypedFunc<u32, u32>,
}
impl Runner {
    fn new(width: Width) -> Self {
        Self::with_operations(&[device_store(width)])
    }
    fn with_operations(operations: &[Operation]) -> Self {
        let program = ProcedureProgram {
            functions: alloc::vec![ProcedureFunction {
                id: ProcedureId(3),
                entry: ENTRY,
                instructions: operations
                    .iter()
                    .enumerate()
                    .map(|(index, &operation)| ProcedureInstruction {
                        address: Address {
                            offset: ENTRY.offset + index as u32 * 4,
                            ..ENTRY
                        },
                        operation,
                    })
                    .collect(),
                terminator: ProcedureReturn {
                    address: Address {
                        offset: ENTRY.offset + operations.len() as u32 * 4,
                        ..ENTRY
                    },
                    width: Width::Dword,
                    pop_bytes: 0
                },
            }],
        };
        let compiled = program
            .compile(&ProcedureCompileOptions::default())
            .unwrap();
        let engine = Engine::default();
        let module = Module::new(&engine, compiled.wasm()).unwrap();
        let mut store = Store::new(&engine, ());
        let memory = Memory::new(&mut store, MemoryType::new(10, Some(10))).unwrap();
        let mut linker = Linker::new(&engine);
        linker
            .define(abi::IMPORT_MODULE, abi::MEMORY_IMPORT, memory)
            .unwrap();
        let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
        let run = instance
            .get_typed_func(&store, "memory_procedure_3")
            .unwrap();
        Self { store, memory, run }
    }
    fn reset(&mut self, vga: &Vga, descriptor: &Descriptor, offset: usize) {
        let bytes = self.memory.data_mut(&mut self.store);
        bytes.fill(0);
        bytes[VIDEO..VIDEO + VRAM_LEN].copy_from_slice(vga.vram_view());
        let mut frame = Frame {
            address: ENTRY,
            eflags: 0x8d7,
            ..Frame::default()
        };
        frame.gpr[0] = 0x1234_ff89;
        frame.gpr[4] = 0x2400;
        frame.gpr[7] = 0xa0000 + offset as u32;
        frame.write(&mut bytes[FRAME..]).unwrap();
        descriptor
            .write(&mut bytes[FRAME + memory::OFFSET as usize..])
            .unwrap();
        bytes[RAM + 0x2400 - 0x500..RAM + 0x2400 - 0x500 + 4]
            .copy_from_slice(&0x8765_4321u32.to_le_bytes());
    }
    fn call(&mut self) -> u32 {
        self.run.call(&mut self.store, FRAME as u32).unwrap()
    }
    fn effects(&self) -> memory::Effects {
        memory::Effects::read(&self.memory.data(&self.store)[FRAME + memory::OFFSET as usize..])
            .unwrap()
    }
}
fn device_store(width: Width) -> Operation {
    Operation::StoreMemory {
        width,
        address: MemoryAddress {
            base: Some(Register::Edi),
            index: None,
            scale: 1,
            displacement: 0,
            address16: false,
            segment: Segment::Ds,
        },
        source: Source::Register(match width {
            Width::Byte => Register::Ah,
            Width::Word => Register::Ax,
            Width::Dword => Register::Eax,
        }),
    }
}
fn register(vga: &mut Vga, bank: u16, index: u8, value: u8) {
    assert!(vga.write_port(bank, index));
    assert!(vga.write_port(bank + 1, value));
}
fn setup(mode: u8, rotate: u8, mask: u8, chained: bool) -> (Vga, Descriptor) {
    let mut vga = Vga::default();
    vga.set_mode_13h();
    register(&mut vga, 0x3c4, 4, 6);
    for plane in 0..4 {
        register(&mut vga, 0x3c4, 2, 1 << plane);
        assert!(vga.write_memory(0x111, 0x36u8.rotate_left(plane)));
    }
    vga.read_memory(0x111).unwrap();
    register(&mut vga, 0x3c4, 4, if chained { 14 } else { 6 });
    register(&mut vga, 0x3c4, 2, mask);
    register(&mut vga, 0x3ce, 0, 0x9);
    register(&mut vga, 0x3ce, 1, 0x6);
    register(&mut vga, 0x3ce, 3, rotate);
    register(&mut vga, 0x3ce, 5, 0x40 | mode);
    register(&mut vga, 0x3ce, 8, 0xa5);
    let descriptor = Descriptor {
        ram: RamDescriptor {
            pointer: RAM as u32,
            linear_base: 0x500,
            length: 0x4000,
            excluded_start: 0xa0000,
            excluded_length: APERTURE_LEN as u32,
            generations_pointer: GENERATIONS as u32,
            generations_length: 32,
            code_pages_pointer: CODE as u32,
            code_pages_length: 32,
        },
        stack_address: StackAddress::Bits32,
        windows: [[SegmentBounds {
            valid: true,
            base: 0,
            first: 0,
            last: u32::MAX,
        }; 6]; 2],
        device: DeviceReadDescriptor {
            guest_start: 0xa0000,
            guest_length: APERTURE_LEN as u32,
            pointer: VIDEO as u32,
            backing_length: VRAM_LEN as u32,
            kind: DeviceReadKind::FourLanes,
            chain4: chained,
            latches: vga.direct_read_view().unwrap().latches,
            ..DeviceReadDescriptor::default()
        },
        device_write: DeviceWriteDescriptor {
            enabled: true,
            plane_mask: mask,
            mode,
            set_reset: 9,
            enable_set_reset: 6,
            rotate,
            bit_mask: 0xa5,
        },
        ..Descriptor::default()
    };
    (vga, descriptor)
}

#[test]
fn direct_vga_stores_match_the_device_for_modes_planes_rops_widths_and_latches() {
    for width in [Width::Byte, Width::Word, Width::Dword] {
        let mut runner = Runner::new(width);
        for mode in 0..4 {
            for rotate in [0, 3, 0x0f, 0x12, 0x1d] {
                for mask in [0, 1, 6, 15] {
                    for chained in [false, true] {
                        let (mut vga, descriptor) = setup(mode, rotate, mask, chained);
                        let offset = 0x283;
                        runner.reset(&vga, &descriptor, offset);
                        let revision = vga.frame().unwrap().revision;
                        let value = if width == Width::Byte {
                            0xff
                        } else {
                            0x1234_ff89
                        };
                        for byte in 0..width.bits() / 8 {
                            assert!(
                                vga.write_memory(
                                    offset + byte as usize,
                                    (value >> (byte * 8)) as u8
                                )
                            );
                        }
                        assert_eq!(
                            runner.call(),
                            COMPLETE,
                            "{width:?}/{mode}/{rotate}/{mask}/{chained}"
                        );
                        assert_eq!(
                            &runner.memory.data(&runner.store)[VIDEO..VIDEO + VRAM_LEN],
                            vga.vram_view()
                        );
                        let effects = runner.effects();
                        assert_eq!(effects.device_writes, u64::from(width.bits() / 8));
                        assert_eq!(
                            effects.device_changes,
                            vga.frame().unwrap().revision.wrapping_sub(revision)
                        );
                        assert_eq!(effects.device_reads, 0);
                        assert_eq!(effects.ram_writes, 0);
                        assert_eq!(effects.latches, descriptor.device.latches);
                    }
                }
            }
        }
    }
}

#[test]
fn simple_replacement_fast_path_matches_vga_with_zero_one_and_multiple_planes() {
    for width in [Width::Byte, Width::Word, Width::Dword] {
        let mut runner = Runner::new(width);
        for mask in [0, 1, 6, 15] {
            for chained in [false, true] {
                let (mut vga, mut descriptor) = setup(0, 0, mask, chained);
                register(&mut vga, 0x3ce, 1, 0);
                register(&mut vga, 0x3ce, 8, 255);
                descriptor.device_write.enable_set_reset = 0;
                descriptor.device_write.bit_mask = 255;
                runner.reset(&vga, &descriptor, 0x283);
                let value = if width == Width::Byte {
                    0xff
                } else {
                    0x1234_ff89
                };
                for byte in 0..width.bits() / 8 {
                    assert!(vga.write_memory(0x283 + byte as usize, (value >> (byte * 8)) as u8));
                }
                assert_eq!(runner.call(), COMPLETE);
                assert_eq!(
                    &runner.memory.data(&runner.store)[VIDEO..VIDEO + VRAM_LEN],
                    vga.vram_view()
                );
            }
        }
    }
}

#[test]
fn device_store_refuses_whole_span_and_aliased_or_disabled_controls_before_effects() {
    let mut runner = Runner::new(Width::Word);
    for case in 0..5 {
        let (vga, mut descriptor) = setup(0, 0, 15, false);
        let mut offset = 0x283;
        match case {
            0 => offset = APERTURE_LEN - 1,
            1 => descriptor.device_write.enabled = false,
            2 => descriptor.device.pointer = FRAME as u32,
            3 => descriptor.device.pointer = RAM as u32,
            4 => descriptor.device_write.mode = 4,
            _ => unreachable!(),
        }
        runner.reset(&vga, &descriptor, offset);
        assert_eq!(runner.call(), SIDE_EXIT);
        assert_eq!(
            &runner.memory.data(&runner.store)[VIDEO..VIDEO + VRAM_LEN],
            vga.vram_view()
        );
        assert_eq!(runner.effects().device_writes, 0);
        let frame = Frame::read(&runner.memory.data(&runner.store)[FRAME..]).unwrap();
        assert_eq!(frame.address, ENTRY);
        assert_eq!(frame.exact_total(), Some(0));
    }
}

#[test]
fn invalid_device_capability_preserves_prior_ram_and_register_work() {
    let mut runner = Runner::with_operations(&[
        Operation::Move {
            target: Register::Ebx,
            source: Source::Immediate(0x1357_9bdf),
        },
        Operation::StoreMemory {
            width: Width::Dword,
            address: MemoryAddress {
                base: None,
                index: None,
                scale: 1,
                displacement: 0x1300,
                address16: false,
                segment: Segment::Ds,
            },
            source: Source::Register(Register::Ebx),
        },
        Operation::Move {
            target: Register::Ah,
            source: Source::Immediate(0x5a),
        },
        device_store(Width::Word),
    ]);
    for case in 0..10 {
        let (vga, mut descriptor) = setup(0, 0, 15, false);
        match case {
            0 => descriptor.device_write.enabled = false,
            1 => descriptor.device_write.mode = 4,
            2 => descriptor.device.pointer = FRAME as u32,
            3 => descriptor.device.pointer = RAM as u32,
            4 => descriptor.device.pointer = GENERATIONS as u32,
            5 => descriptor.device.pointer = CODE as u32,
            6 => descriptor.device.pointer = 10 * 65536 - 1,
            7 => descriptor.device.backing_length = u32::MAX,
            8 => descriptor.device.kind = DeviceReadKind::Disabled,
            // The cached layout is valid but the second physical group is
            // outside the advertised backing. No first-byte effect may leak.
            9 => descriptor.device.backing_length = 0x284 * 4,
            _ => unreachable!(),
        }
        runner.reset(&vga, &descriptor, 0x283);
        assert_eq!(runner.call(), SIDE_EXIT, "case {case}");
        let bytes = runner.memory.data(&runner.store);
        let frame = Frame::read(&bytes[FRAME..]).unwrap();
        assert_eq!(frame.gpr[0], 0x1234_5a89, "case {case}");
        assert_eq!(frame.gpr[3], 0x1357_9bdf, "case {case}");
        assert_eq!(frame.gpr[4], 0x2400, "case {case}");
        assert_eq!(frame.eflags, 0x8d7, "case {case}");
        assert_eq!(frame.address.offset, ENTRY.offset + 12, "case {case}");
        assert_eq!(frame.exact_total(), Some(3), "case {case}");
        assert_eq!(
            &bytes[RAM + 0x1300 - 0x500..RAM + 0x1300 - 0x500 + 4],
            &0x1357_9bdfu32.to_le_bytes(),
            "case {case}"
        );
        assert_eq!(
            &bytes[GENERATIONS + 8..GENERATIONS + 16],
            &1_u64.to_le_bytes(),
            "case {case}"
        );
        assert_eq!(&bytes[VIDEO..VIDEO + VRAM_LEN], vga.vram_view());
        let effects = runner.effects();
        assert_eq!(effects.ram_writes, 1, "case {case}");
        assert_eq!(effects.device_writes, 0, "case {case}");
        assert_eq!(effects.device_changes, 0, "case {case}");
    }
}

#[test]
fn device_capability_is_revalidated_on_every_call_without_trapping_on_truncation() {
    let mut runner = Runner::with_operations(&[
        Operation::Move {
            target: Register::Ebx,
            source: Source::Immediate(0x1357_9bdf),
        },
        device_store(Width::Byte),
    ]);
    let (vga, mut descriptor) = setup(0, 0, 15, false);
    runner.reset(&vga, &descriptor, 0x283);
    assert_eq!(runner.call(), COMPLETE);
    descriptor.device_write.enabled = false;
    runner.reset(&vga, &descriptor, 0x283);
    assert_eq!(runner.call(), SIDE_EXIT);
    let frame = Frame::read(&runner.memory.data(&runner.store)[FRAME..]).unwrap();
    assert_eq!(frame.address.offset, ENTRY.offset + 4);
    assert_eq!(frame.exact_total(), Some(1));

    // Keep the whole CPU frame in bounds while truncating the descriptor.
    let truncated = 10 * 65536 - abi::FRAME_BYTES;
    let mut frame = Frame {
        address: ENTRY,
        ..Frame::default()
    };
    frame.gpr[7] = 0xa0283;
    frame
        .write(&mut runner.memory.data_mut(&mut runner.store)[truncated..])
        .unwrap();
    assert_eq!(
        runner
            .run
            .call(&mut runner.store, truncated as u32)
            .unwrap(),
        SIDE_EXIT
    );
    let frame = Frame::read(&runner.memory.data(&runner.store)[truncated..]).unwrap();
    assert_eq!(frame.gpr[3], 0x1357_9bdf);
    assert_eq!(frame.address.offset, ENTRY.offset + 4);
    assert_eq!(frame.exact_total(), Some(1));
}
