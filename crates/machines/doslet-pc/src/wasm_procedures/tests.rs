use super::*;
use alloc::vec;
use doslet_bus::PortBus;
use doslet_config::HardwareConfig;
use doslet_x86::flags;
use doslet_x86_wasm::static_program::{
    Alu, Condition, Operation, Register, Shift, ShiftCount, Source, Width,
    memory::{MemoryAddress, Segment},
    procedure_memory::{
        ProcedureBlock, ProcedureCompileOptions, ProcedureFunction, ProcedureGraphFunction,
        ProcedureGraphProgram, ProcedureId, ProcedureInstruction, ProcedureProgram,
        ProcedureReturn, ProcedureTerminator, ProcedureTerminatorKind,
    },
};

#[path = "finite_tests.rs"]
mod finite_tests;
#[path = "kernel_tests.rs"]
mod kernel_tests;
#[path = "rle_tests.rs"]
mod rle_tests;

const ENTRY: u32 = 0x8000;
const DATA: u32 = 0x2000;
const STACK: u32 = 0x4000;
const RETURN: u32 = 0x9000;
const CODE: &[u8] = &[0xa1, 0, 0x20, 0, 0, 0x83, 0xc0, 7, 0x89, 0x03, 0xc3];

#[allow(dead_code)]
mod fm_fixture {
    include!("../../../../../tests/fixtures/opl3.rs");
}

fn address(offset: u32) -> Address {
    Address { space: 0, offset }
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

fn module() -> Vec<u8> {
    ProcedureProgram {
        functions: vec![ProcedureFunction {
            id: ProcedureId(9),
            entry: address(ENTRY),
            instructions: vec![
                ProcedureInstruction {
                    address: address(ENTRY),
                    operation: Operation::LoadMemory {
                        target: Register::Eax,
                        address: operand(None, DATA),
                    },
                },
                ProcedureInstruction {
                    address: address(ENTRY + 5),
                    operation: Operation::Alu {
                        kind: Alu::Add,
                        target: Register::Eax,
                        source: Source::Immediate(7),
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
        }],
    }
    .compile(&ProcedureCompileOptions::default())
    .unwrap()
    .into_wasm()
}

fn binding() -> Binding {
    Binding {
        function_id: 9,
        entry_ip: ENTRY,
        maximum_instructions: 4,
        kernel: None,
        rle_kernel: None,
        finite_computation: None,
        kernel_interrupts: KernelInterrupts::Defer,
        code: vec![CodePart {
            linear: ENTRY,
            expected: CODE.to_vec(),
        }],
    }
}

fn descriptor(bus: &mut PcBus, selector: u16, base: u32, code: bool) {
    let bytes = [
        0xff,
        0xff,
        base as u8,
        (base >> 8) as u8,
        (base >> 16) as u8,
        if code { 0x9b } else { 0x93 },
        0xcf,
        (base >> 24) as u8,
    ];
    for (offset, byte) in bytes.into_iter().enumerate() {
        bus.write_u8(0x800 + u32::from(selector) + offset as u32, byte)
            .unwrap();
    }
}

fn machine() -> PcMachine {
    let mut machine = PcMachine::from_config(HardwareConfig {
        memory_mib: 4,
        ..HardwareConfig::default()
    })
    .unwrap();
    machine.bus.set_a20(true);
    descriptor(&mut machine.bus, 8, 0, true);
    descriptor(&mut machine.bus, 16, 0, false);
    machine.cpu.control.cr0 |= 1;
    machine.cpu.control.gdtr.base = 0x800;
    machine.cpu.control.gdtr.limit = 0xff;
    machine.bus.write_u8(0, 0x90).unwrap();
    machine.cpu.step(&mut machine.bus).unwrap();
    for segment in SegmentRegister::ALL {
        machine
            .cpu
            .load_segment(
                &mut machine.bus,
                segment,
                if segment == SegmentRegister::Cs {
                    8
                } else {
                    16
                },
            )
            .unwrap();
    }
    machine.cpu.registers.ip = 0;
    machine.cpu.step(&mut machine.bus).unwrap(); // Complete the SS interrupt shadow.
    machine.cpu.registers.ip = ENTRY;
    machine.cpu.registers.set32(Gpr32::Esp, STACK);
    machine.cpu.registers.set32(Gpr32::Ebx, DATA);
    machine.cpu.flags = 2;
    machine.cpu.set_dynamic_core(false);
    machine
        .bus
        .debug_ram_mut(ENTRY, CODE.len() as u32)
        .unwrap()
        .copy_from_slice(CODE);
    machine.bus.write_u32(DATA, 35).unwrap();
    machine.bus.write_u32(STACK, RETURN).unwrap();
    machine.bus.write_u8(RETURN, 0x90).unwrap();
    machine
}

fn installed() -> PcMachine {
    let mut machine = machine();
    machine
        .install_wasm_procedures(&module(), vec![binding()])
        .unwrap();
    machine
}

fn reference_steps(machine: &mut PcMachine, count: u32) {
    for _ in 0..count {
        assert_eq!(machine.step_with_idle_budget(1).unwrap().1, 1);
    }
}

fn same_state(a: &mut PcMachine, b: &mut PcMachine) {
    assert_eq!(a.cpu.registers, b.cpu.registers);
    assert_eq!(a.cpu.flags, b.cpu.flags);
    assert_eq!(a.cpu.control, b.cpu.control);
    assert_eq!(a.cpu.has_interrupt_shadow(), b.cpu.has_interrupt_shadow());
    assert_eq!(a.time(), b.time());
    assert_eq!(a.bus.ram.storage.bytes(), b.bus.ram.storage.bytes());
    assert_eq!(
        a.bus.ram.storage.generations(),
        b.bus.ram.storage.generations()
    );
    assert_eq!(a.bus.ram.writes(), b.bus.ram.writes());
    assert_eq!(a.bus.vram_view(), b.bus.vram_view());
    assert_eq!(a.bus.device_accesses, b.bus.device_accesses);
    assert_eq!(
        a.bus
            .vga
            .direct_read_view_with_memory(a.bus.vram_view())
            .unwrap()
            .latches,
        b.bus
            .vga
            .direct_read_view_with_memory(b.bus.vram_view())
            .unwrap()
            .latches,
    );
    assert_eq!(
        a.bus.video_frame().map(|frame| frame.revision),
        b.bus.video_frame().map(|frame| frame.revision),
    );
    assert_eq!(a.bus.pic.read_command(), b.bus.pic.read_command());
    assert_eq!(a.bus.dsp.diagnostics(), b.bus.dsp.diagnostics());
    let mut a_pcm = [doslet_pcm::EMPTY; 64];
    let mut b_pcm = [doslet_pcm::EMPTY; 64];
    assert_eq!(a.drain_pcm_timed(&mut a_pcm), b.drain_pcm_timed(&mut b_pcm));
    assert_eq!(a_pcm, b_pcm);
}

fn video_register(machine: &mut PcMachine, port: u16, index: u8, value: u8) {
    machine.bus.write_port_u8(port, index).unwrap();
    machine.bus.write_port_u8(port + 1, value).unwrap();
}

#[test]
fn compiled_video_stores_match_original_x86_all_write_modes() {
    let wasm = module();
    for chain4 in [false, true] {
        for write_mode in 0..4 {
            let mut fast = machine();
            let mut reference = machine();
            for machine in [&mut fast, &mut reference] {
                machine.bus.set_video_mode_13h();
                for offset in 0..256 {
                    machine
                        .bus
                        .write_u8(0xa0000 + offset, (offset as u8).wrapping_mul(37))
                        .unwrap();
                }
                if !chain4 {
                    video_register(machine, 0x3c4, 4, 6);
                }
                video_register(machine, 0x3c4, 2, 0x0b);
                video_register(machine, 0x3ce, 0, 5);
                video_register(machine, 0x3ce, 1, 9);
                video_register(machine, 0x3ce, 2, 3);
                video_register(machine, 0x3ce, 3, 0x1b); // Rotate three, XOR.
                video_register(machine, 0x3ce, 4, 2);
                video_register(machine, 0x3ce, 5, write_mode | 8); // Read compare.
                video_register(machine, 0x3ce, 7, 7);
                video_register(machine, 0x3ce, 8, 0x5a);
                machine.bus.read_u32(0xa0000).unwrap();
                machine.cpu.registers.set32(Gpr32::Ebx, 0xa0040);
            }
            fast.install_wasm_procedures(&wasm, vec![binding()])
                .unwrap();
            let pointer = fast.bus.vram_view().as_ptr();
            let accesses = fast.bus.device_accesses;
            assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 4);
            reference_steps(&mut reference, 4);
            assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 1);
            assert_eq!(fast.wasm_procedure_stats().unwrap().side_exits, 0);
            assert_eq!(fast.wasm_procedure_stats().unwrap().device_reads, 0);
            assert_eq!(fast.wasm_procedure_stats().unwrap().device_writes, 4);
            assert_eq!(fast.bus.device_accesses - accesses, 4);
            same_state(&mut fast, &mut reference);
            assert_eq!(fast.bus.vram_view().as_ptr(), pointer);
            // The next ordinary interpreter/device write observes the generated
            // latches and the very same backing; the next compiled call uses
            // latch changes made by the ordinary interpreter read.
            for machine in [&mut fast, &mut reference] {
                machine.bus.write_u8(0xa0000, 0x63).unwrap();
                machine.bus.read_u32(0xa0000).unwrap();
                machine.cpu.registers.ip = ENTRY;
                machine.cpu.registers.set32(Gpr32::Esp, STACK);
            }
            assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 4);
            reference_steps(&mut reference, 4);
            same_state(&mut fast, &mut reference);
            #[cfg(feature = "save-state")]
            {
                use doslet_state::State;
                assert_eq!(fast.save(), reference.save());
            }
        }
    }
}

#[test]
fn migrated_video_backing_is_shared_by_bus_scanout_banks_and_mode_resets() {
    let mut fast = machine();
    let mut reference = machine();
    for machine in [&mut fast, &mut reference] {
        machine.bus.set_video_vbe_101h(false);
        assert!(machine.bus.vga.set_bank(7));
        machine.bus.write_u8(0xa0012, 0x73).unwrap();
    }
    let old_pointer = fast.bus.vram_view().as_ptr();
    fast.install_wasm_procedures(&module(), vec![binding()])
        .unwrap();
    let pointer = fast.bus.vram_view().as_ptr();
    assert_ne!(pointer, old_pointer);
    assert!(fast.bus.direct_memory_view().is_none());
    assert_eq!(fast.bus.read_u8(0xa0012).unwrap(), 0x73);
    assert_eq!(reference.bus.read_u8(0xa0012).unwrap(), 0x73);
    for machine in [&mut fast, &mut reference] {
        assert!(machine.bus.vga.set_bank(3));
        machine.bus.write_u8(0xa0000, 0x5d).unwrap();
        machine.cpu.registers.set32(Gpr32::Ebx, 0xa0004);
    }
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 4);
    reference_steps(&mut reference, 4);
    same_state(&mut fast, &mut reference);
    assert_eq!(fast.bus.vram_view()[3 * doslet_vga::APERTURE_LEN + 4], 42);
    for machine in [&mut fast, &mut reference] {
        machine.bus.set_video_vbe_101h(true);
        assert!(machine.bus.vga.set_bank(7));
        assert_eq!(machine.bus.read_u8(0xa0012).unwrap(), 0x73);
        machine.bus.vga.set_mode_3h();
        assert_eq!(
            machine.bus.vram_view()[7 * doslet_vga::APERTURE_LEN + 0x12],
            0x73
        );
        machine.bus.set_video_mode_13h();
        assert!(machine.bus.vram_view().iter().all(|&byte| byte == 0));
        assert_eq!(
            machine.bus.video_frame().unwrap().pixels.as_ptr(),
            machine.bus.vram_view().as_ptr()
        );
        machine.bus.write_u8(0xa0007, 0x91).unwrap();
        machine.bus.set_video_vbe_101h(false);
        assert!(machine.bus.vram_view().iter().all(|&byte| byte == 0));
    }
    assert_eq!(fast.bus.vram_view().as_ptr(), pointer);
    same_state(&mut fast, &mut reference);
}

#[test]
fn first_and_repeated_compiled_video_calls_allocate_nothing() {
    let mut machine = machine();
    machine.bus.set_video_mode_13h();
    machine.cpu.registers.set32(Gpr32::Ebx, 0xa0000);
    machine
        .install_wasm_procedures(&module(), vec![binding()])
        .unwrap();
    let pointer = machine.bus.vram_view().as_ptr();
    let measured = allocation_counter::measure(|| {
        for value in 0..32 {
            machine.cpu.registers.ip = ENTRY;
            machine.cpu.registers.set32(Gpr32::Esp, STACK);
            machine.bus.write_u32(DATA, value).unwrap();
            assert_eq!(machine.step_with_idle_budget(32).unwrap().1, 4);
            assert_eq!(machine.bus.read_u32(0xa0000).unwrap(), value + 7);
        }
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
    assert_eq!(machine.bus.vram_view().as_ptr(), pointer);
    assert_eq!(machine.wasm_procedure_stats().unwrap().calls, 32);
    assert_eq!(machine.wasm_procedure_stats().unwrap().device_writes, 128);
}

#[test]
fn actual_pc_procedure_matches_reference_without_backing_replacement() {
    let mut fast = installed();
    let mut reference = machine();
    let pointer = fast.bus.ram.storage.bytes().as_ptr();
    let generations = fast.bus.ram.storage.generations().as_ptr();
    let (outcome, count) = fast.step_with_idle_budget(32).unwrap();
    assert_eq!(outcome, doslet_x86::StepOutcome::Continue);
    assert_eq!(count, 4);
    reference_steps(&mut reference, count);
    same_state(&mut fast, &mut reference);
    assert_eq!(fast.bus.ram.storage.bytes().as_ptr(), pointer);
    assert_eq!(fast.bus.ram.storage.generations().as_ptr(), generations);
    let Storage::Wasm(owner) = &fast.bus.ram.storage else {
        panic!("shared owner");
    };
    assert_eq!(owner.ram().as_ptr(), pointer);
    assert_eq!(owner.generations().as_ptr(), generations);
    assert_eq!(
        fast.wasm_procedure_stats().unwrap().translated_instructions,
        4
    );
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 1);
    assert_eq!(fast.cpu.registers.ip, RETURN);
}

#[test]
fn caller_loop_preserves_device_deadlines_and_pcm_across_many_entries() {
    let mut fast = installed();
    let mut reference = machine();
    for machine in [&mut fast, &mut reference] {
        for register in [0x30, 0x31, 0x34, 0x35] {
            machine.bus.write_port_u8(0x224, register).unwrap();
            machine.bus.write_port_u8(0x225, 0xf8).unwrap();
        }
        for (register, value) in fm_fixture::OPL_TONE {
            machine.bus.write_port_u8(0x388, register as u8).unwrap();
            machine.bus.write_port_u8(0x389, value).unwrap();
        }
        let mut caller = vec![0xe8];
        caller.extend_from_slice(&(ENTRY - 0x1005).to_le_bytes());
        caller.push(0xe9);
        caller.extend_from_slice(&(-10i32).to_le_bytes());
        machine
            .bus
            .debug_ram_mut(0x1000, caller.len() as u32)
            .unwrap()
            .copy_from_slice(&caller);
        machine.cpu.registers.ip = 0x1000;
        machine.cpu.registers.set32(Gpr32::Esp, STACK + 4);
    }
    let mut completed = 0;
    while completed < 4096 {
        let count = fast
            .step_with_idle_budget((4096 - completed).min(128))
            .unwrap()
            .1;
        assert_ne!(count, 0);
        reference_steps(&mut reference, count);
        assert_eq!(fast.cpu.registers, reference.cpu.registers);
        assert_eq!(fast.cpu.flags, reference.cpu.flags);
        completed += count;
    }
    same_state(&mut fast, &mut reference);
    assert!(fast.wasm_procedure_stats().unwrap().calls > 100);
    assert!(fast.wasm_procedure_stats().unwrap().budget_refusals > 0);
    assert!(fast.audio_status().pcm_frames > 0);
}

#[test]
fn ordinary_cached_batch_yields_inside_block_before_prepared_entry() {
    let mut fast = installed();
    fast.bus.write_u8(ENTRY - 1, 0x90).unwrap();
    fast.cpu.registers.ip = ENTRY - 1;
    assert_eq!(fast.step_with_idle_budget(128).unwrap().1, 1);
    assert_eq!(fast.cpu.registers.ip, ENTRY);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 0);
    assert_eq!(fast.step_with_idle_budget(128).unwrap().1, 4);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 1);
}

fn complete_borrow(machine: &mut PcMachine, effects: crate::memory::batch::Effects) {
    #[cfg(feature = "profiling")]
    {
        machine.bus.stats.bda_tick_reads += effects.bda_tick_reads;
    }
    machine.bus.ram.complete_interpreter_batch(effects);
}

#[test]
fn borrowed_ram_matches_pc_widths_code_checks_and_page_accounting() {
    let mut fast = installed();
    let mut reference = machine();
    let check = fast.bus.code_check(0x2fff, 4).unwrap();
    let untouched = fast.bus.code_check(RETURN, 1).unwrap();
    let pointer = fast.bus.ram.storage.bytes().as_ptr();
    let writes = fast.bus.ram.writes();
    let mut bus = fast.bus.ram.interpreter_batch_bus().unwrap();
    assert_eq!(bus.code_check(0x2fff, 4), Some(check));
    assert_eq!(
        bus.code_bytes(RETURN, 1).unwrap().as_ptr(),
        pointer.wrapping_add(RETURN as usize)
    );
    assert!(bus.code_valid(&check));
    for (address, length) in [(0, 1), (0x2fff, 4), (0xbffff, 2), (0xc0000, 4)] {
        assert_eq!(
            bus.is_plain_ram(address, length),
            reference.bus.is_plain_ram(address, length)
        );
    }
    for address in [0x9fffe, 0xbffff, 0xc0000, 0x3fffff] {
        assert_eq!(
            bus.code_bytes(address, 4),
            reference.bus.code_bytes(address, 4)
        );
    }
    assert_eq!(bus.read_u8(0x46c), reference.bus.read_u8(0x46c));
    assert_eq!(bus.read_u16(0x46d), reference.bus.read_u16(0x46d));
    assert_eq!(bus.read_u32(0x46a), reference.bus.read_u32(0x46a));
    bus.write_u8(0x2fff, 0x5a).unwrap();
    reference.bus.write_u8(0x2fff, 0x5a).unwrap();
    bus.write_u16(0x2fff, 0x1234).unwrap();
    reference.bus.write_u16(0x2fff, 0x1234).unwrap();
    bus.write_u32(0x2fff, 0x89abcdef).unwrap();
    reference.bus.write_u32(0x2fff, 0x89abcdef).unwrap();
    assert_eq!(bus.read_u8(0x2fff), reference.bus.read_u8(0x2fff));
    assert_eq!(bus.read_u16(0x2fff), reference.bus.read_u16(0x2fff));
    assert_eq!(bus.read_u32(0x2fff), reference.bus.read_u32(0x2fff));
    assert!(!bus.code_valid(&check));
    assert!(bus.code_valid(&untouched));
    let changed = bus.code_check(0x2fff, 4).unwrap();
    let effects = bus.finish();
    assert_eq!(effects.page_writes, 5);
    complete_borrow(&mut fast, effects);
    assert_eq!(fast.bus.ram.writes(), writes + 5);
    assert_eq!(fast.bus.code_check(0x2fff, 4), Some(changed));
    assert_eq!(changed.space, check.space);
    assert!(fast.bus.code_valid(&untouched));
    assert_eq!(fast.bus.ram.storage.bytes().as_ptr(), pointer);
    #[cfg(feature = "profiling")]
    assert_eq!(
        fast.bus.stats.bda_tick_reads,
        reference.bus.stats.bda_tick_reads
    );
    same_state(&mut fast, &mut reference);
}

#[test]
fn borrowed_ram_declines_complete_mmio_and_faulting_spans_without_effects() {
    let mut fast = installed();
    let mut reference = machine();
    let check = fast.bus.code_check(0x9fffe, 2).unwrap();
    let mut bus = fast.bus.ram.interpreter_batch_bus().unwrap();
    for address in [0x9ffff, 0xa0000, 0xaffff, 0x3fffff, u32::MAX] {
        assert!(!bus.is_plain_ram(address, 2));
        assert!(!bus.is_batch_memory(address, 4));
        assert!(bus.code_check(address, 4).is_none());
        assert!(bus.read_u16(address).is_err());
        assert!(bus.read_u32(address).is_err());
        assert!(bus.write_u16(address, 0x1234).is_err());
        assert!(bus.write_u32(address, 0x12345678).is_err());
    }
    assert!(bus.read_u8(0xa0000).is_err());
    assert!(bus.write_u8(0xaffff, 1).is_err());
    assert!(bus.read_port_u8(0x61).is_err());
    assert!(bus.write_port_u8(0x61, 1).is_err());
    assert!(!bus.is_plain_ram(0x2000, 0));
    assert!(bus.code_valid(&check));
    let effects = bus.finish();
    assert_eq!(effects.page_writes, 0);
    complete_borrow(&mut fast, effects);
    same_state(&mut fast, &mut reference);
}

#[test]
fn borrowed_ram_generation_exhaustion_matches_ordinary_writes() {
    let mut fast = installed();
    let mut reference = machine();
    for machine in [&mut fast, &mut reference] {
        machine.bus.ram.storage.generations_mut()[2] = u64::MAX;
    }
    let untouched = fast.bus.code_check(RETURN, 1).unwrap();
    let mut bus = fast.bus.ram.interpreter_batch_bus().unwrap();
    bus.write_u32(0x2fff, 0x12345678).unwrap();
    reference.bus.write_u32(0x2fff, 0x12345678).unwrap();
    assert!(!bus.code_valid(&untouched));
    assert!(bus.code_check(RETURN, 1).is_none());
    bus.write_u8(DATA, 42).unwrap();
    reference.bus.write_u8(DATA, 42).unwrap();
    let effects = bus.finish();
    assert_eq!(effects.page_writes, 3);
    assert!(effects.code_exhausted);
    complete_borrow(&mut fast, effects);
    assert_eq!(fast.bus.ram.storage.generations()[2], u64::MAX);
    assert!(!fast.bus.code_valid(&untouched));
    assert!(fast.bus.code_check(RETURN, 1).is_none());
    assert!(fast.bus.ram.interpreter_batch_bus().is_none());
    fast.bus.set_a20(false);
    fast.bus.set_a20(true);
    reference.bus.set_a20(false);
    reference.bus.set_a20(true);
    assert!(fast.bus.code_check(RETURN, 1).is_none());
    same_state(&mut fast, &mut reference);
}

#[test]
fn shared_ram_views_preserve_write_proofs_and_reject_invalid_spans_without_effects() {
    let mut fast = installed();
    let mut reference = machine();
    for machine in [&mut fast, &mut reference] {
        let ram = &mut machine.bus.ram;
        let writes = ram.writes();
        let generations = ram.storage.generations().to_vec();
        let check = ram.code_check(0x2fff..0x3003).unwrap();
        let space = ram.begin_recurrence();
        ram.begin_write_log();
        let size = ram.len();
        for range in [
            core::ops::Range { start: 4, end: 3 },
            size..size + 1,
            size + 1..size + 1,
        ] {
            assert!(ram.view_mut(range).is_none());
        }
        assert!(ram.view_mut(size..size).unwrap().is_empty());
        assert!(ram.write(size as u32, 1).is_err());
        assert_eq!(ram.writes(), writes);
        assert_eq!(ram.storage.generations(), generations);
        assert!(ram.code_valid(&check));

        // Overlapping byte/wide writes must remember the first old value,
        // including both pages. Restoring bytes proves recurrence even though
        // generations correctly retain evidence of every write.
        ram.view_mut(0x2fff..0x3003).unwrap().fill(42);
        ram.write(0x3000, 7).unwrap();
        assert!(!ram.code_valid(&check));
        ram.view_mut(0x2fff..0x3003).unwrap().fill(0);
        assert!(ram.end_write_log());
        assert!(ram.end_recurrence(space));
        assert_eq!(ram.writes(), writes + 5);
        let changed = ram.code_check(0x2fff..0x3003).unwrap();
        assert_eq!(changed.generations[0], check.generations[0] + 2);
        assert_eq!(changed.generations[1], check.generations[1] + 3);

        let space = ram.begin_recurrence();
        ram.begin_write_log();
        ram.view_mut(0x2fff..0x3003).unwrap()[0] = 1;
        assert!(!ram.end_write_log());
        assert!(!ram.end_recurrence(space));

        let untouched = ram
            .code_check(RETURN as usize..RETURN as usize + 1)
            .unwrap();
        ram.storage.generations_mut()[2] = u64::MAX;
        let next_page = ram.storage.generations()[3];
        let writes = ram.writes();
        ram.view_mut(0x2fff..0x3003).unwrap().fill(99);
        assert_eq!(ram.storage.generations()[2], u64::MAX);
        assert_eq!(ram.storage.generations()[3], next_page + 1);
        assert_eq!(ram.writes(), writes + 2);
        assert_eq!(ram.view(0x2fff..0x3003), Some(&[99; 4][..]));
        assert!(!ram.code_valid(&untouched));
        ram.set_mapping(1);
        assert!(ram.code_check(0x2fff..0x3003).is_none());
    }
    same_state(&mut fast, &mut reference);
    assert_eq!(fast.bus.ram.writes(), reference.bus.ram.writes());
    assert_eq!(
        fast.bus.ram.storage.generations(),
        reference.bus.ram.storage.generations()
    );
}

/// Original x86 encodings cover all access widths, cross-page stores/stack,
/// near CALL/RET in both operand widths, then an I/O batch boundary.
fn ordinary_program(machine: &mut PcMachine) -> u32 {
    let mut code = vec![
        0xb0, 0x5a, 0xa2, 0xff, 0x2f, 0, 0, 0x66, 0xb8, 0x34, 0x12, 0x66, 0xa3, 0xff, 0x2f, 0, 0,
        0xb8, 0xef, 0xcd, 0xab, 0x89, 0xa3, 0xff, 0x2f, 0, 0, 0xa0, 0xff, 0x2f, 0, 0, 0x66, 0xa1,
        0xff, 0x2f, 0, 0, 0xa1, 0xff, 0x2f, 0, 0,
    ];
    code.push(0xe8);
    code.extend_from_slice(&(0x1100u32 - (0x1000 + code.len() as u32 + 4)).to_le_bytes());
    code.extend_from_slice(&[0x66, 0xe8]);
    code.extend_from_slice(&((0x1120u32 - (0x1000 + code.len() as u32 + 2)) as u16).to_le_bytes());
    let io_ip = 0x1000 + code.len() as u32;
    code.extend_from_slice(&[0xe4, 0x61]);
    machine
        .bus
        .debug_ram_mut(0x1000, code.len() as u32)
        .unwrap()
        .copy_from_slice(&code);
    machine
        .bus
        .debug_ram_mut(0x1100, 2)
        .unwrap()
        .copy_from_slice(&[0x40, 0xc3]);
    machine
        .bus
        .debug_ram_mut(0x1120, 4)
        .unwrap()
        .copy_from_slice(&[0x66, 0x40, 0x66, 0xc3]);
    machine.cpu.registers.ip = 0x1000;
    machine.cpu.registers.set32(Gpr32::Esp, 0x4002);
    io_ip
}

#[test]
fn borrowed_interpreter_matches_pc_batches_for_original_x86_load_store_call_ret() {
    let mut fast = installed();
    let mut reference = machine();
    let io_ip = ordinary_program(&mut fast);
    assert_eq!(ordinary_program(&mut reference), io_ip);
    let pointer = fast.bus.ram.storage.bytes().as_ptr();
    assert_eq!(fast.step_with_idle_budget(64).unwrap().1, 15);
    assert_eq!(fast.cpu.registers.ip, io_ip);
    assert_eq!(reference.step_with_idle_budget(64).unwrap().1, 15);
    same_state(&mut fast, &mut reference);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 0);
    assert_eq!(fast.bus.ram.storage.bytes().as_ptr(), pointer);
    // The boundary instruction runs through PcBus after releasing the borrow.
    assert_eq!(fast.step_with_idle_budget(64).unwrap().1, 1);
    reference_steps(&mut reference, 1);
    same_state(&mut fast, &mut reference);
}

#[cfg(feature = "wasm-procedure-diagnostics")]
#[test]
fn diagnostic_modes_execute_valid_entries_without_aot_and_match_full_state() {
    for mode in [
        DiagnosticMode::SharedWithoutEntries,
        DiagnosticMode::SharedPcBus,
    ] {
        for destination in [DATA, 0xa0040] {
            let mut fast = installed();
            let mut reference = machine();
            for machine in [&mut fast, &mut reference] {
                machine.bus.set_video_mode_13h();
                machine.cpu.registers.set32(Gpr32::Ebx, destination);
                for (register, value) in fm_fixture::OPL_TONE {
                    machine.bus.write_port_u8(0x388, register as u8).unwrap();
                    machine.bus.write_port_u8(0x389, value).unwrap();
                }
            }
            let ram = fast.bus.ram.storage.bytes().as_ptr();
            let generations = fast.bus.ram.storage.generations().as_ptr();
            let video = fast.bus.vram_view().as_ptr();
            fast.set_wasm_procedure_diagnostic_mode(mode).unwrap();
            assert_eq!(fast.cpu.prepared_entry_index(), None);
            assert_eq!(fast.try_wasm_procedure(4).unwrap(), None);
            assert_eq!(fast.step_with_idle_budget(4).unwrap().1, 4);
            reference_steps(&mut reference, 4);
            assert_eq!(fast.cpu.registers.ip, RETURN);
            assert_eq!(fast.wasm_procedure_stats(), Some(Stats::default()));
            same_state(&mut fast, &mut reference);
            #[cfg(feature = "save-state")]
            {
                use doslet_state::State;
                assert_eq!(fast.save(), reference.save());
            }
            assert_eq!(fast.bus.ram.storage.bytes().as_ptr(), ram);
            assert_eq!(fast.bus.ram.storage.generations().as_ptr(), generations);
            assert_eq!(fast.bus.vram_view().as_ptr(), video);
            assert!(matches!(fast.bus.ram.storage, Storage::Wasm(_)));

            // Ordinary cross-page memory, CALL/RET and I/O retain their normal
            // state and deadline behavior after suppressing prepared entries.
            let io_ip = ordinary_program(&mut fast);
            assert_eq!(ordinary_program(&mut reference), io_ip);
            assert_eq!(fast.step_with_idle_budget(64).unwrap().1, 15);
            assert_eq!(reference.step_with_idle_budget(64).unwrap().1, 15);
            assert_eq!(fast.cpu.registers.ip, io_ip);
            same_state(&mut fast, &mut reference);
            assert_eq!(fast.step_with_idle_budget(64).unwrap().1, 1);
            reference_steps(&mut reference, 1);
            same_state(&mut fast, &mut reference);
            assert_eq!(fast.wasm_procedure_stats(), Some(Stats::default()));
        }
    }
}

#[cfg(feature = "wasm-procedure-diagnostics")]
#[test]
fn diagnostic_modes_restore_registered_entries_and_existing_source_guards() {
    let mut machine = installed();
    assert_eq!(machine.try_wasm_procedure(4).unwrap(), Some(4));
    assert!(machine.wasm_procedures.as_ref().unwrap().bindings[0].checked);
    for mode in [
        DiagnosticMode::SharedWithoutEntries,
        DiagnosticMode::SharedPcBus,
    ] {
        machine.cpu.registers.ip = ENTRY;
        machine.cpu.registers.set32(Gpr32::Esp, STACK);
        let snapshot = machine.cpu.snapshot();
        let time = machine.time();
        let stats = machine.wasm_procedure_stats();
        let ram = machine.bus.ram.storage.bytes().as_ptr();
        machine.set_wasm_procedure_diagnostic_mode(mode).unwrap();
        assert_eq!(machine.cpu.prepared_entry_index(), None);
        assert!(machine.cpu.matches(&snapshot));
        assert_eq!(machine.time(), time);
        assert_eq!(machine.wasm_procedure_stats(), stats);
        assert_eq!(machine.bus.ram.storage.bytes().as_ptr(), ram);
        assert!(machine.wasm_procedures.as_ref().unwrap().bindings[0].checked);
        assert_eq!(machine.try_wasm_procedure(4).unwrap(), None);

        // A changed source while dispatch is disabled must remain rejected
        // when normal entry dispatch is restored from the installed plan.
        machine.bus.write_u8(ENTRY + 7, 8).unwrap();
        machine
            .set_wasm_procedure_diagnostic_mode(DiagnosticMode::Normal)
            .unwrap();
        assert_eq!(machine.cpu.prepared_entry_index(), Some(0));
        assert_eq!(machine.try_wasm_procedure(4).unwrap(), None);
        assert_eq!(
            machine.wasm_procedure_stats().unwrap().calls,
            stats.unwrap().calls
        );
        machine.bus.write_u8(ENTRY + 7, 7).unwrap();
        assert_eq!(machine.try_wasm_procedure(4).unwrap(), Some(4));
        assert_eq!(
            machine.wasm_procedure_stats().unwrap().calls,
            stats.unwrap().calls + 1
        );
        assert_eq!(machine.cpu.registers.ip, RETURN);
        assert_eq!(machine.bus.ram.storage.bytes().as_ptr(), ram);
    }
    assert_eq!(machine.wasm_procedure_stats().unwrap().source_refusals, 2);
}

#[cfg(feature = "wasm-procedure-diagnostics")]
#[test]
fn diagnostic_interpreter_execution_allocates_nothing() {
    for mode in [
        DiagnosticMode::SharedWithoutEntries,
        DiagnosticMode::SharedPcBus,
    ] {
        let mut machine = installed();
        machine.set_wasm_procedure_diagnostic_mode(mode).unwrap();
        let measured = allocation_counter::measure(|| {
            for _ in 0..32 {
                machine.cpu.registers.ip = ENTRY;
                machine.cpu.registers.set32(Gpr32::Esp, STACK);
                assert_eq!(machine.step_with_idle_budget(4).unwrap().1, 4);
            }
        });
        assert_eq!(measured.count_total, 0);
        assert_eq!(measured.bytes_total, 0);
        assert_eq!(machine.wasm_procedure_stats(), Some(Stats::default()));
    }
}

#[cfg(feature = "wasm-procedure-diagnostics")]
#[test]
fn diagnostic_selection_requires_a_plan_and_preserves_fatal_execution_errors() {
    let mut machine = machine();
    assert!(
        machine
            .set_wasm_procedure_diagnostic_mode(DiagnosticMode::SharedPcBus)
            .is_err()
    );
    let mut wrong = binding();
    wrong.maximum_instructions = 3;
    machine
        .install_wasm_procedures(&module(), vec![wrong])
        .unwrap();
    let error = MachineError::WasmProcedure(ExecutionError::InvalidResult(
        MemoryProcedureResultError::InstructionCount,
    ));
    assert_eq!(machine.step_with_idle_budget(32), Err(error));
    for mode in [
        DiagnosticMode::SharedWithoutEntries,
        DiagnosticMode::SharedPcBus,
        DiagnosticMode::Normal,
    ] {
        machine.set_wasm_procedure_diagnostic_mode(mode).unwrap();
        assert_eq!(machine.step_with_idle_budget(32), Err(error));
    }
}

#[test]
fn borrowed_interpreter_keeps_selected_scheduler_heads() {
    let mut fast = installed();
    let mut reference = machine();
    for machine in [&mut fast, &mut reference] {
        machine
            .bus
            .debug_ram_mut(0x1000, 4)
            .unwrap()
            .copy_from_slice(&[0x90, 0x90, 0x90, 0x90]);
        machine.cpu.registers.ip = 0x1000;
    }
    fast.batch_yield_ip = Some([0x1002, 0x1003]);
    assert_eq!(fast.step_with_idle_budget(64).unwrap().1, 2);
    reference_steps(&mut reference, 2);
    same_state(&mut fast, &mut reference);
    assert_eq!(fast.step_with_idle_budget(64).unwrap().1, 1);
    reference_steps(&mut reference, 1);
    same_state(&mut fast, &mut reference);
}

#[test]
fn borrowed_interpreter_preflights_a_crossing_mmio_operand_before_cpu_effects() {
    let mut machine = installed();
    machine
        .bus
        .debug_ram_mut(0x1000, 6)
        .unwrap()
        .copy_from_slice(&[0xa3, 0xff, 0xff, 9, 0, 0xf4]);
    machine.cpu.registers.ip = 0x1000;
    let snapshot = machine.cpu.snapshot();
    let before = machine.bus.code_check(0x9fffc, 4).unwrap();
    let writes = machine.bus.ram.writes();
    let mut bus = machine.bus.ram.interpreter_batch_bus().unwrap();
    assert_eq!(machine.cpu.interpret_batch(&mut bus, 32, |_| 32), 0);
    assert!(machine.cpu.matches(&snapshot));
    assert!(bus.code_valid(&before));
    let effects = bus.finish();
    assert_eq!(effects.page_writes, 0);
    complete_borrow(&mut machine, effects);
    assert_eq!(machine.bus.ram.writes(), writes);
}

#[test]
fn borrowed_interpreter_rechecks_self_modified_instructions_inside_a_batch() {
    let mut fast = installed();
    let mut reference = machine();
    for machine in [&mut fast, &mut reference] {
        // Replace the already-decoded INC EAX with INC EBX before reaching it.
        machine
            .bus
            .debug_ram_mut(0x1000, 10)
            .unwrap()
            .copy_from_slice(&[0xc6, 5, 7, 0x10, 0, 0, 0x43, 0x40, 0xe4, 0x61]);
        machine.cpu.registers.ip = 0x1000;
    }
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 2);
    assert_eq!(reference.step_with_idle_budget(32).unwrap().1, 2);
    assert_eq!(fast.cpu.registers.get32(Gpr32::Ebx), DATA + 1);
    assert_eq!(fast.cpu.registers.ip, 0x1008);
    same_state(&mut fast, &mut reference);
}

#[test]
fn wasm_ram_with_a20_disabled_keeps_pc_address_translation() {
    let mut fast = installed();
    let mut reference = machine();
    for machine in [&mut fast, &mut reference] {
        machine
            .bus
            .debug_ram_mut(0x1000, 7)
            .unwrap()
            .copy_from_slice(&[0xa1, 0, 0x20, 0x10, 0, 0xe4, 0x61]);
        machine.cpu.registers.ip = 0x1000;
        machine.bus.set_a20(false);
    }
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 1);
    assert_eq!(reference.step_with_idle_budget(32).unwrap().1, 1);
    assert_eq!(fast.cpu.registers.get32(Gpr32::Eax), 35);
    same_state(&mut fast, &mut reference);
}

#[test]
fn first_and_repeated_borrowed_interpreter_batches_allocate_nothing() {
    let mut machine = installed();
    let io_ip = ordinary_program(&mut machine);
    let measured = allocation_counter::measure(|| {
        for _ in 0..32 {
            machine.cpu.registers.ip = 0x1000;
            machine.cpu.registers.set32(Gpr32::Esp, 0x4002);
            assert_eq!(machine.step_with_idle_budget(64).unwrap().1, 15);
            assert_eq!(machine.cpu.registers.ip, io_ip);
        }
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
    assert_eq!(machine.wasm_procedure_stats().unwrap().calls, 0);
}

#[test]
fn borrowed_interpreter_declines_owned_and_recorded_storage() {
    let mut owned = machine();
    assert!(owned.bus.ram.interpreter_batch_bus().is_none());
    let mut machine = installed();
    machine.bus.ram.begin_write_log();
    assert!(machine.bus.ram.interpreter_batch_bus().is_none());
    assert!(machine.bus.ram.end_write_log());
    let space = machine.bus.ram.begin_recurrence();
    assert!(machine.bus.ram.interpreter_batch_bus().is_none());
    assert!(machine.bus.ram.end_recurrence(space));
    assert!(machine.bus.ram.interpreter_batch_bus().is_some());
}

#[test]
fn refused_store_returns_exact_prefix_and_never_replays_it() {
    let mut fast = installed();
    let mut reference = machine();
    for machine in [&mut fast, &mut reference] {
        machine.cpu.registers.set32(Gpr32::Ebx, ENTRY);
    }
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 2);
    reference_steps(&mut reference, 2);
    same_state(&mut fast, &mut reference);
    assert_eq!(fast.cpu.registers.ip, ENTRY + 8);
    assert_eq!(fast.wasm_procedure_stats().unwrap().side_exits, 1);
    assert_eq!(fast.step_with_idle_budget(1).unwrap().1, 1);
    reference_steps(&mut reference, 1);
    same_state(&mut fast, &mut reference);
    assert_eq!(fast.cpu.registers.ip, ENTRY + 10);
}

#[test]
fn zero_prefix_side_exit_interprets_current_instruction_once() {
    let mut fast = installed();
    let mut reference = machine();
    for machine in [&mut fast, &mut reference] {
        descriptor(&mut machine.bus, 24, 0x9e000, false);
        machine
            .cpu
            .load_segment(&mut machine.bus, SegmentRegister::Ds, 24)
            .unwrap();
    }
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 1);
    reference_steps(&mut reference, 1);
    same_state(&mut fast, &mut reference);
    assert_eq!(fast.cpu.registers.ip, ENTRY + 5);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 1);
    assert_eq!(
        fast.wasm_procedure_stats().unwrap().translated_instructions,
        0
    );
}

#[test]
fn live_nonflat_data_window_is_used_on_each_invocation() {
    let mut fast = installed();
    let mut reference = machine();
    for machine in [&mut fast, &mut reference] {
        descriptor(&mut machine.bus, 24, 0x10000, false);
        machine
            .cpu
            .load_segment(&mut machine.bus, SegmentRegister::Ds, 24)
            .unwrap();
        machine.bus.write_u32(0x10000 + DATA, 99).unwrap();
    }
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 4);
    reference_steps(&mut reference, 4);
    same_state(&mut fast, &mut reference);
    assert_eq!(fast.bus.read_u32(0x10000 + DATA).unwrap(), 106);
}

#[test]
fn cached_call_windows_follow_live_hidden_caches_selectors_and_mode() {
    let mut machine = machine();
    let check = |machine: &mut PcMachine| {
        let expected = [Access::Read, Access::Write].map(|access| {
            SegmentRegister::ALL.map(|segment| {
                machine.cpu.registers.window(segment, access).map_or(
                    SegmentBounds::default(),
                    |window| {
                        let (base, first, last) = window.parts();
                        SegmentBounds {
                            valid: true,
                            base,
                            first,
                            last,
                        }
                    },
                )
            })
        });
        assert_eq!(
            machine
                .bus
                .wasm_call_windows
                .resolve(&machine.cpu.registers),
            expected
        );
    };
    check(&mut machine);
    check(&mut machine);
    // Reloading the same selector changes its hidden descriptor without a
    // selector change, including read/write permissions and expand-down limits.
    for access in [0x91, 0x97, 0x93] {
        descriptor(&mut machine.bus, 24, 0x10000, false);
        machine.bus.write_u8(0x800 + 24 + 5, access).unwrap();
        machine
            .cpu
            .load_segment(&mut machine.bus, SegmentRegister::Ds, 24)
            .unwrap();
        check(&mut machine);
        check(&mut machine);
    }
    machine.cpu.registers.ds = 32; // Unsynchronized public selector write.
    check(&mut machine);
    assert!(!machine.bus.wasm_call_windows.windows[0][SegmentRegister::Ds as usize].valid);
    machine.cpu.set_protected_mode(false);
    check(&mut machine);
    assert_eq!(
        machine.bus.wasm_call_windows.windows[0][SegmentRegister::Ds as usize].base,
        32 * 16
    );
}

#[test]
fn source_proof_retries_after_loading_and_refuses_modified_bytes() {
    let mut fast = machine();
    fast.bus
        .debug_ram_mut(ENTRY, CODE.len() as u32)
        .unwrap()
        .fill(0);
    fast.install_wasm_procedures(&module(), vec![binding()])
        .unwrap();
    let snapshot = fast.cpu.snapshot();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    assert!(fast.cpu.matches(&snapshot));
    fast.bus
        .debug_ram_mut(ENTRY, CODE.len() as u32)
        .unwrap()
        .copy_from_slice(CODE);
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(4));
    fast.cpu.registers.ip = ENTRY;
    fast.bus.write_u8(ENTRY + 7, 8).unwrap();
    let snapshot = fast.cpu.snapshot();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    assert!(fast.cpu.matches(&snapshot));
    assert_eq!(fast.wasm_procedure_stats().unwrap().source_refusals, 3);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 1);
}

#[test]
fn decoded_entry_boundaries_cover_fallthrough_calls_returns_and_indirect_jumps() {
    for transfer in 0..5 {
        let mut fast = machine();
        let mut reference = machine();
        let (start, prefix_steps, return_ip) = match transfer {
            0 => (ENTRY - 2, 2, RETURN),
            1 => (0xa000, 1, 0xa005),
            2 => (0xa000, 1, 0xa002),
            _ => (0xa000, 1, RETURN),
        };
        for machine in [&mut fast, &mut reference] {
            machine.cpu.registers.ip = start;
            match transfer {
                0 => {
                    machine.bus.write_u8(start, 0x90).unwrap();
                    machine.bus.write_u8(start + 1, 0x90).unwrap();
                }
                1 => {
                    machine.bus.write_u8(start, 0xe8).unwrap();
                    machine
                        .bus
                        .write_u32(start + 1, ENTRY.wrapping_sub(start + 5))
                        .unwrap();
                }
                2 | 4 => {
                    machine.cpu.registers.set32(Gpr32::Eax, ENTRY);
                    machine.bus.write_u8(start, 0xff).unwrap();
                    machine
                        .bus
                        .write_u8(start + 1, if transfer == 2 { 0xd0 } else { 0xe0 })
                        .unwrap();
                }
                3 => {
                    machine.bus.write_u8(start, 0xc3).unwrap();
                    machine.bus.write_u32(STACK, ENTRY).unwrap();
                    machine.bus.write_u32(STACK + 4, RETURN).unwrap();
                }
                _ => unreachable!(),
            }
        }
        fast.install_wasm_procedures(&module(), vec![binding()])
            .unwrap();
        assert_eq!(fast.step_with_idle_budget(32).unwrap().1, prefix_steps);
        reference_steps(&mut reference, prefix_steps);
        assert_eq!(fast.cpu.registers.ip, ENTRY);
        assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 0);
        same_state(&mut fast, &mut reference);
        assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 4);
        reference_steps(&mut reference, 4);
        assert_eq!(fast.cpu.registers.ip, return_ip);
        assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 1);
        same_state(&mut fast, &mut reference);
    }
}

#[test]
fn entry_hints_survive_decode_cache_toggles_but_cannot_authorize_changed_code() {
    let mut fast = installed();
    fast.cpu.set_decode_cache(false);
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(4));
    fast.cpu.set_decode_cache(true);
    fast.cpu.registers.ip = ENTRY;
    fast.cpu.registers.set32(Gpr32::Esp, STACK);
    fast.bus.write_u8(ENTRY + 7, 8).unwrap();
    let snapshot = fast.cpu.snapshot();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    assert!(fast.cpu.matches(&snapshot));
    assert_eq!(fast.wasm_procedure_stats().unwrap().source_refusals, 1);
    assert_eq!(fast.step_with_idle_budget(4).unwrap().1, 4);
    assert_eq!(fast.cpu.registers.ip, RETURN);
    assert_eq!(fast.cpu.registers.get32(Gpr32::Eax), 50);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 1);
}

#[test]
fn budget_mapping_recording_and_observation_preserve_reference_path() {
    let mut fast = installed();
    let snapshot = fast.cpu.snapshot();
    assert_eq!(fast.try_wasm_procedure(3).unwrap(), None);
    assert_eq!(fast.wasm_procedure_stats().unwrap().budget_refusals, 1);
    fast.bus.set_a20(false);
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    fast.bus.set_a20(true);
    fast.bus.begin_ram_write_log();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    assert!(fast.bus.end_ram_write_log());
    let space = fast.bus.ram.begin_recurrence();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    assert!(fast.bus.ram.end_recurrence(space));
    assert!(fast.cpu.matches(&snapshot));
    let mut observed = crate::StepObservation::default();
    assert_eq!(fast.step_observed(&mut observed).unwrap().1, 1);
    assert!(observed.instruction.is_some());
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 0);
    assert_eq!(fast.cpu.registers.ip, ENTRY + 5);
}

#[test]
fn cpu_hazards_and_execution_labels_decline_without_effects() {
    let mut fast = installed();
    for flag in [flags::TRAP, flags::RESUME, flags::VIRTUAL] {
        fast.cpu.flags = 2 | flag;
        let snapshot = fast.cpu.snapshot();
        assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
        assert!(fast.cpu.matches(&snapshot));
    }
    fast.cpu.flags = 2;
    fast.cpu.set_execution_labels(Some(Vec::new())).unwrap();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    fast.cpu.set_execution_labels(None).unwrap();
    fast.cpu.control.cr0 |= 0x8000_0000;
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 0);
}

#[test]
fn pending_irq_and_interrupt_shadow_precede_prepared_execution() {
    let mut fast = installed();
    fast.cpu.flags |= flags::INTERRUPT;
    fast.bus.pic.write_data(0);
    fast.bus.pic.raise_edge(0);
    assert!(fast.bus.pic.has_interrupt());
    let snapshot = fast.cpu.snapshot();
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    assert!(fast.cpu.matches(&snapshot));
    fast.cpu.flags = 2;
    fast.cpu
        .load_segment(&mut fast.bus, SegmentRegister::Ss, 16)
        .unwrap();
    assert!(fast.cpu.has_interrupt_shadow());
    assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 0);
}

#[test]
fn protects_all_source_pages_and_accepts_reordered_parts() {
    let mut fast = machine();
    let mut sources = binding();
    sources.code = vec![
        CodePart {
            linear: ENTRY + 5,
            expected: CODE[5..].to_vec(),
        },
        CodePart {
            linear: ENTRY,
            expected: CODE[..5].to_vec(),
        },
        CodePart {
            linear: DATA,
            expected: 35u32.to_le_bytes().to_vec(),
        },
    ];
    fast.install_wasm_procedures(&module(), vec![sources])
        .unwrap();
    // The DATA page is protected even though it is not the entry's page.
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 2);
    assert_eq!(fast.cpu.registers.ip, ENTRY + 8);
    assert_eq!(fast.bus.read_u32(DATA).unwrap(), 35);
    assert_eq!(fast.wasm_procedure_stats().unwrap().side_exits, 1);
}

#[test]
fn install_failure_is_atomic_and_sources_are_bounded() {
    let mut machine = machine();
    let pointer = machine.bus.ram.storage.bytes().as_ptr();
    let check = machine.bus.code_check(ENTRY, 1).unwrap();
    let wasm = module();
    assert!(machine.install_wasm_procedures(&wasm, Vec::new()).is_err());
    let mut bad = binding();
    bad.maximum_instructions = 0;
    assert!(machine.install_wasm_procedures(&wasm, vec![bad]).is_err());
    let mut bad = binding();
    bad.code[0].linear = 0xa0000;
    assert!(machine.install_wasm_procedures(&wasm, vec![bad]).is_err());
    let mut bad = binding();
    bad.code[0].linear = u32::MAX;
    assert!(machine.install_wasm_procedures(&wasm, vec![bad]).is_err());
    assert!(
        machine
            .install_wasm_procedures(&wasm, vec![binding(), binding()])
            .is_err()
    );
    assert!(
        machine
            .install_wasm_procedures(&[0], vec![binding()])
            .is_err()
    );
    assert_eq!(machine.bus.ram.storage.bytes().as_ptr(), pointer);
    assert!(machine.bus.code_valid(&check));
    assert!(machine.wasm_procedure_stats().is_none());
    machine
        .install_wasm_procedures(&wasm, vec![binding()])
        .unwrap();
    assert!(!machine.bus.code_valid(&check));
    assert!(
        machine
            .install_wasm_procedures(&wasm, vec![binding()])
            .is_err()
    );
}

#[test]
fn setup_caps_registry_parts_and_source_bytes_before_compilation() {
    let mut machine = machine();
    let bindings = (0..=MAX_BINDINGS)
        .map(|id| Binding {
            function_id: id as u32,
            entry_ip: id as u32,
            maximum_instructions: 1,
            kernel: None,
            rle_kernel: None,
            finite_computation: None,
            kernel_interrupts: KernelInterrupts::Defer,
            code: Vec::new(),
        })
        .collect();
    assert!(
        machine
            .install_wasm_procedures(&[], bindings)
            .unwrap_err()
            .contains("4096 bindings")
    );
    let mut too_many_parts = binding();
    too_many_parts.code = (0..=MAX_SOURCE_PARTS)
        .map(|_| CodePart {
            linear: ENTRY,
            expected: Vec::new(),
        })
        .collect();
    assert!(
        machine
            .install_wasm_procedures(&[], vec![too_many_parts])
            .unwrap_err()
            .contains("65536 parts")
    );
    let mut too_many_bytes = binding();
    too_many_bytes.code[0].expected = vec![0; MAX_SOURCE_BYTES + 1];
    assert!(
        machine
            .install_wasm_procedures(&[], vec![too_many_bytes])
            .unwrap_err()
            .contains("16 MiB")
    );
    assert!(machine.wasm_procedure_stats().is_none());
}

#[test]
fn invalid_result_after_store_is_fatal_and_cannot_replay() {
    let mut machine = machine();
    let mut wrong = binding();
    wrong.maximum_instructions = 3;
    machine
        .install_wasm_procedures(&module(), vec![wrong])
        .unwrap();
    let snapshot = machine.cpu.snapshot();
    let time = machine.time();
    let error = MachineError::WasmProcedure(ExecutionError::InvalidResult(
        MemoryProcedureResultError::InstructionCount,
    ));
    assert_eq!(machine.step_with_idle_budget(32), Err(error));
    assert!(machine.cpu.matches(&snapshot));
    assert_eq!(machine.bus.read_u32(DATA).unwrap(), 42);
    assert_eq!(machine.time(), time);
    assert_eq!(machine.step_with_idle_budget(32), Err(error));
    assert_eq!(
        machine.step_observed(&mut crate::StepObservation::default()),
        Err(error)
    );
    assert_eq!(machine.bus.read_u32(DATA).unwrap(), 42);
}

#[test]
fn entries_prefix_exits_and_source_declines_allocate_nothing() {
    let mut machine = installed();
    // Wasmtime's first invocation and source proof both belong to measurement:
    // installation has already reserved the backing and every guard slot.
    let measured = allocation_counter::measure(|| {
        for _ in 0..32 {
            machine.cpu.registers.ip = ENTRY;
            machine.cpu.registers.set32(Gpr32::Esp, STACK);
            assert_eq!(machine.step_with_idle_budget(32).unwrap().1, 4);
        }
        machine.cpu.registers.set32(Gpr32::Ebx, ENTRY);
        for _ in 0..32 {
            machine.cpu.registers.ip = ENTRY;
            assert_eq!(machine.step_with_idle_budget(32).unwrap().1, 2);
        }
        machine.bus.write_u8(ENTRY + 7, 8).unwrap();
        for _ in 0..32 {
            machine.cpu.registers.ip = ENTRY;
            assert_eq!(machine.try_wasm_procedure(32).unwrap(), None);
        }
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
    let stats = machine.wasm_procedure_stats().unwrap();
    assert_eq!(stats.calls, 64);
    assert_eq!(stats.side_exits, 32);
    assert_eq!(stats.source_refusals, 32);
}

#[cfg(feature = "save-state")]
#[test]
fn saved_state_restores_plain_ram_and_drops_prepared_bindings() {
    use doslet_state::State;
    let mut original = installed();
    original.bus.set_video_vbe_101h(false);
    assert!(original.bus.vga.set_bank(7));
    original.bus.write_u32(0xa0010, 0x12345678).unwrap();
    original.step_with_idle_budget(32).unwrap();
    let mut restored = PcMachine::load(original.save()).unwrap();
    assert!(restored.wasm_procedure_stats().is_none());
    assert!(matches!(restored.bus.ram.storage, Storage::Owned { .. }));
    assert_eq!(
        original.bus.ram.storage.bytes(),
        restored.bus.ram.storage.bytes()
    );
    assert_eq!(original.cpu.registers, restored.cpu.registers);
    assert_eq!(original.time(), restored.time());
    assert_eq!(original.bus.vram_view(), restored.bus.vram_view());
    assert_eq!(restored.bus.vga.vram_view().len(), doslet_vga::VRAM_LEN);
    assert_eq!(restored.bus.vga.bank(), 7);
    assert_eq!(original.bus.read_u32(0xa0010).unwrap(), 0x12345678);
    assert_eq!(restored.bus.read_u32(0xa0010).unwrap(), 0x12345678);
    assert_eq!(original.save(), restored.save());
    restored.cpu.registers.ip = ENTRY;
    restored.bus.write_u32(STACK, RETURN).unwrap();
    restored.cpu.registers.set32(Gpr32::Esp, STACK);
    assert_eq!(restored.step_with_idle_budget(4).unwrap().1, 4);
    assert_eq!(restored.cpu.registers.ip, RETURN);
}
