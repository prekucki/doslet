use super::*;
use crate::{PcBus, memory::storage::Storage};
use alloc::{boxed::Box, vec, vec::Vec};
use doslet_config::HardwareConfig;
use doslet_wasm_aot::procedure_memory::SharedProcedureMemory;
use doslet_x86::{Cpu, Gpr16, SegmentRegister};
use doslet_x86_wasm::static_program::{
    Address, Width,
    procedure_memory::{
        ProcedureCompileOptions, ProcedureFunction, ProcedureId, ProcedureProgram, ProcedureReturn,
    },
};

fn module() -> Vec<u8> {
    ProcedureProgram {
        functions: vec![ProcedureFunction {
            id: ProcedureId(9),
            entry: Address {
                space: 0,
                offset: 0x8000,
            },
            instructions: vec![],
            terminator: ProcedureReturn {
                address: Address {
                    space: 0,
                    offset: 0x8000,
                },
                width: Width::Dword,
                pop_bytes: 0,
            },
        }],
    }
    .compile(&ProcedureCompileOptions::default())
    .unwrap()
    .into_wasm()
}

fn configured(read_kind: u8, write_mode: u8) -> PcBus {
    let mut bus = PcBus::from_config(HardwareConfig {
        memory_mib: 4,
        ..HardwareConfig::default()
    })
    .unwrap();
    bus.set_a20(true);
    if read_kind == 0 {
        bus.set_video_vbe_101h(false);
        assert!(bus.vga.set_bank(7));
    } else {
        bus.set_video_mode_13h();
    }
    for offset in 0..64 {
        bus.write_u8(APERTURE_START + offset, (offset * 7 + 3) as u8)
            .unwrap();
    }
    if read_kind >= 2 {
        for (port, index, value) in [(0x3c4, 4, 6), (0x3ce, 4, 2)] {
            assert!(bus.vga.write_port(port, index));
            assert!(bus.vga.write_port(port + 1, value));
        }
    }
    for (port, index, value) in [
        (0x3c4, 2, 5),
        (0x3ce, 0, 9),
        (0x3ce, 1, 3),
        (0x3ce, 3, 0x1b),
        (
            0x3ce,
            5,
            0x40 | if read_kind == 3 { 8 } else { 0 } | write_mode,
        ),
        (0x3ce, 7, 5),
        (0x3ce, 8, 0xa5),
    ] {
        assert!(bus.vga.write_port(port, index));
        assert!(bus.vga.write_port(port + 1, value));
    }
    bus
}

fn shared(mut bus: PcBus, wasm: &[u8]) -> PcBus {
    let mut owner = SharedProcedureMemory::new(wasm, &[9], bus.ram.len()).unwrap();
    owner.ram_mut().copy_from_slice(bus.ram.storage.bytes());
    owner
        .generations_mut()
        .copy_from_slice(bus.ram.storage.generations());
    bus.vga.transfer_memory_into(owner.video_mut());
    bus.ram.replace_storage(Storage::Wasm(Box::new(owner)));
    bus
}

fn same_bus(a: &PcBus, b: &PcBus) {
    assert_eq!(a.ram.storage.bytes(), b.ram.storage.bytes());
    assert_eq!(a.ram.storage.generations(), b.ram.storage.generations());
    assert_eq!(a.ram.writes(), b.ram.writes());
    assert_eq!(a.vram_view(), b.vram_view());
    assert_eq!(a.device_accesses, b.device_accesses);
    assert_eq!(
        a.video_frame().unwrap().revision,
        b.video_frame().unwrap().revision
    );
    assert_eq!(
        a.vga
            .direct_read_view_with_memory(a.vram_view())
            .unwrap()
            .latches,
        b.vga
            .direct_read_view_with_memory(b.vram_view())
            .unwrap()
            .latches,
    );
    #[cfg(feature = "save-state")]
    {
        use doslet_state::State;
        assert!(a.save() == b.save(), "portable bus state differs");
    }
}

#[test]
fn borrowed_video_widths_match_pc_bus_in_linear_chain4_and_planar_modes() {
    let wasm = module();
    for read_kind in 0..4 {
        for write_mode in 0..4 {
            let mut fast = shared(configured(read_kind, write_mode), &wasm);
            let mut reference = configured(read_kind, write_mode);
            let ram_pointer = fast.ram.storage.bytes().as_ptr();
            let video_pointer = fast.vram_view().as_ptr();
            let effects = {
                let mut bus = fast.interpreter_batch_bus().unwrap();
                assert_eq!(bus.bytes.as_ptr(), ram_pointer);
                assert_eq!(bus.video.as_ref().unwrap().bytes.as_ptr(), video_pointer);
                for address in [
                    0x1fff,
                    APERTURE_START,
                    APERTURE_START + 1,
                    APERTURE_START + 7,
                ] {
                    assert!(bus.is_batch_memory(address, 4));
                    assert_eq!(bus.read_u8(address), reference.read_u8(address));
                    bus.write_u8(address, 0x5c).unwrap();
                    reference.write_u8(address, 0x5c).unwrap();
                    assert_eq!(bus.read_u16(address), reference.read_u16(address));
                    bus.write_u16(address, 0xa37d).unwrap();
                    reference.write_u16(address, 0xa37d).unwrap();
                    assert_eq!(bus.read_u32(address), reference.read_u32(address));
                    bus.write_u32(address, 0x1759_b2e8).unwrap();
                    reference.write_u32(address, 0x1759_b2e8).unwrap();
                }
                for (address, length) in [
                    (APERTURE_START - 1, 2),
                    (APERTURE_START + APERTURE_LEN as u32 - 1, 2),
                    (u32::MAX, 4),
                    (APERTURE_START, 0),
                ] {
                    assert_eq!(
                        bus.is_batch_memory(address, length),
                        reference.is_batch_memory(address, length)
                    );
                    assert!(!bus.is_batch_memory(address, length));
                }
                assert!(!bus.is_plain_ram(APERTURE_START, 1));
                assert!(bus.code_check(APERTURE_START, 1).is_none());
                bus.finish()
            };
            fast.complete_interpreter_batch(effects);
            same_bus(&fast, &reference);
            assert_eq!(fast.ram.storage.bytes().as_ptr(), ram_pointer);
            assert_eq!(fast.vram_view().as_ptr(), video_pointer);
        }
    }
}

// Byte/word/dword VGA loads/stores, read-modify-write, a crossing RAM write and
// three bounded REP iterations must stay in one slice before the port access.
const PROGRAM: &[u8] = &[
    0xb8, 0xcd, 0xab, 0xa3, 0, 0, 0x8b, 0x1e, 0, 0, 0x81, 6, 2, 0, 0x11, 0x11, 0x66, 0xa1, 4, 0,
    0x66, 0xa3, 8, 0, 0x26, 0xa3, 0xff, 0x1f, 0x26, 0xa1, 0xff, 0x1f, 0xf3, 0xa5, 0xe4, 0x61,
];

fn cpu(bus: &mut PcBus) -> Cpu {
    bus.debug_ram_mut(0x1000, PROGRAM.len() as u32)
        .unwrap()
        .copy_from_slice(PROGRAM);
    let mut cpu = Cpu::default();
    cpu.set_dynamic_core(false);
    cpu.load_segment(bus, SegmentRegister::Ds, 0xa000).unwrap();
    cpu.registers.ip = 0x1000;
    cpu.registers.set(Gpr16::Si, 0x10);
    cpu.registers.set(Gpr16::Di, 0x2020);
    cpu.registers.set(Gpr16::Cx, 3);
    cpu
}

#[test]
fn borrowed_vga_instructions_and_rep_keep_pc_batch_boundaries_and_state() {
    let wasm = module();
    for read_kind in 0..4 {
        for write_mode in 0..4 {
            let mut fast = shared(configured(read_kind, write_mode), &wasm);
            let mut reference = configured(read_kind, write_mode);
            let mut fast_cpu = cpu(&mut fast);
            let mut reference_cpu = cpu(&mut reference);
            let effects = {
                let mut bus = fast.interpreter_batch_bus().unwrap();
                assert_eq!(fast_cpu.interpret_batch(&mut bus, 64, |_| 64), 11);
                bus.finish()
            };
            fast.complete_interpreter_batch(effects);
            assert_eq!(
                reference_cpu.interpret_batch(&mut reference, 64, |_| 64),
                11
            );
            assert_eq!(fast_cpu.registers.ip, 0x1000 + PROGRAM.len() as u32 - 2);
            assert!(fast_cpu.matches(&reference_cpu.snapshot()));
            same_bus(&fast, &reference);
        }
    }
}

#[test]
fn borrowed_vga_first_and_repeated_instruction_batches_allocate_nothing() {
    let mut bus = shared(configured(2, 0), &module());
    let mut cpu = cpu(&mut bus);
    let measured = allocation_counter::measure(|| {
        for _ in 0..32 {
            cpu.registers.ip = 0x1000;
            cpu.registers.set(Gpr16::Si, 0x10);
            cpu.registers.set(Gpr16::Di, 0x2020);
            cpu.registers.set(Gpr16::Cx, 3);
            let effects = {
                let mut borrowed = bus.interpreter_batch_bus().unwrap();
                assert_eq!(cpu.interpret_batch(&mut borrowed, 64, |_| 64), 11);
                borrowed.finish()
            };
            bus.complete_interpreter_batch(effects);
        }
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}
