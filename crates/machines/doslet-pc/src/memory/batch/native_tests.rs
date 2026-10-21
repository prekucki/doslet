use super::*;
use crate::{PcBus, PcMachine};
use doslet_config::HardwareConfig;
use doslet_x86::{Gpr16, SegmentRegister};

fn configured() -> PcBus {
    let mut bus = PcBus::from_config(HardwareConfig {
        memory_mib: 4,
        ..HardwareConfig::default()
    })
    .unwrap();
    bus.set_a20(true);
    bus.set_video_mode_13h();
    bus
}

fn same_bus(a: &PcBus, b: &PcBus) {
    assert_eq!(a.ram.storage.bytes(), b.ram.storage.bytes());
    assert_eq!(a.ram.storage.generations(), b.ram.storage.generations());
    assert_eq!(a.ram.writes(), b.ram.writes());
    assert_eq!(a.vram_view(), b.vram_view());
    assert_eq!(
        a.vga
            .direct_read_view_with_memory(a.vram_view())
            .unwrap()
            .latches,
        b.vga
            .direct_read_view_with_memory(b.vram_view())
            .unwrap()
            .latches
    );
    assert_eq!(a.device_accesses, b.device_accesses);
    assert_eq!(
        a.video_frame().unwrap().revision,
        b.video_frame().unwrap().revision
    );
    #[cfg(feature = "profiling")]
    assert_eq!(a.stats.bda_tick_reads, b.stats.bda_tick_reads);
    #[cfg(feature = "save-state")]
    {
        use doslet_state::State;
        assert!(a.save() == b.save(), "portable bus state differs");
    }
}

#[test]
fn owned_borrow_keeps_bytes_write_epochs_bda_reads_and_overflow_exact() {
    let mut fast = configured();
    let mut reference = configured();
    let pointer = fast.ram.storage.bytes().as_ptr();
    let checks = [0x2000, 0x1fff, 0x46b].map(|address| fast.code_check(address, 4).unwrap());
    let effects = {
        let mut bus = fast.interpreter_batch_bus().unwrap();
        assert_eq!(bus.bytes.as_ptr(), pointer);
        for address in [0x2000, 0x1fff, 0x46b] {
            assert_eq!(bus.read_u8(address), reference.read_u8(address));
            bus.write_u8(address, 0x71).unwrap();
            reference.write_u8(address, 0x71).unwrap();
            assert_eq!(bus.read_u16(address), reference.read_u16(address));
            bus.write_u16(address, 0xa55a).unwrap();
            reference.write_u16(address, 0xa55a).unwrap();
            assert_eq!(bus.read_u32(address), reference.read_u32(address));
            bus.write_u32(address, 0x1234_5678).unwrap();
            reference.write_u32(address, 0x1234_5678).unwrap();
        }
        for check in &checks {
            assert!(!bus.code_valid(check));
        }
        // A RAM-only borrow must decline complete spans touching VGA and ports.
        for (address, length) in [(APERTURE_START, 1), (APERTURE_START - 1, 2)] {
            assert!(!bus.is_batch_memory(address, length));
        }
        assert!(bus.read_u16(APERTURE_START - 1).is_err());
        assert!(bus.read_port_u8(0x61).is_err());
        assert!(bus.write_port_u8(0x61, 0).is_err());
        bus.finish()
    };
    fast.complete_interpreter_batch(effects);
    same_bus(&fast, &reference);
    assert_eq!(fast.ram.storage.bytes().as_ptr(), pointer);

    fast.ram.storage.generations_mut()[2] = u64::MAX;
    reference.ram.storage.generations_mut()[2] = u64::MAX;
    let check = fast.code_check(0x2000, 4).unwrap();
    let effects = {
        let mut bus = fast.interpreter_batch_bus().unwrap();
        bus.write_u32(0x2000, 42).unwrap();
        reference.write_u32(0x2000, 42).unwrap();
        assert!(!bus.code_valid(&check));
        assert!(bus.code_check(0x1000, 1).is_none());
        bus.finish()
    };
    fast.complete_interpreter_batch(effects);
    assert!(fast.code_check(0x1000, 1).is_none());
    assert!(fast.interpreter_batch_bus().is_none());
    same_bus(&fast, &reference);
}

#[test]
fn owned_borrow_declines_a20_aliasing_and_active_write_tracking() {
    let mut bus = configured();
    bus.set_a20(false);
    assert!(bus.interpreter_batch_bus().is_none());
    bus.set_a20(true);
    assert!(bus.interpreter_batch_bus().is_some());
    bus.ram.begin_write_log();
    assert!(bus.interpreter_batch_bus().is_none());
    assert!(bus.ram.end_write_log());
    let space = bus.ram.begin_recurrence();
    assert!(bus.interpreter_batch_bus().is_none());
    assert!(bus.ram.end_recurrence(space));
    assert!(bus.interpreter_batch_bus().is_some());
}

fn machine(program: &[u8], data_segment: u16, borrowed: bool) -> PcMachine {
    let mut machine = PcMachine::from_config(HardwareConfig {
        memory_mib: 4,
        ..HardwareConfig::default()
    })
    .unwrap();
    machine.bus.set_a20(true);
    machine.bus.set_video_mode_13h();
    machine
        .bus
        .debug_ram_mut(0x1000, program.len() as u32)
        .unwrap()
        .copy_from_slice(program);
    machine.cpu.set_dynamic_core(false);
    machine
        .cpu
        .load_segment(&mut machine.bus, SegmentRegister::Ds, data_segment)
        .unwrap();
    machine.cpu.registers.ip = 0x1000;
    machine.set_borrowed_interpreter_bus(borrowed);
    machine
}

fn run_exactly(machine: &mut PcMachine, steps: u32) {
    let mut remaining = steps;
    while remaining != 0 {
        let (_, completed) = machine.step_with_idle_budget(remaining).unwrap();
        assert!(completed != 0 && completed <= remaining);
        remaining -= completed;
    }
}

fn same_machine(a: &mut PcMachine, b: &mut PcMachine) {
    assert!(a.cpu.matches(&b.cpu.snapshot()));
    assert_eq!(a.time(), b.time());
    same_bus(&a.bus, &b.bus);
    let mut pcm_a = [[0; 2]; 16];
    let mut pcm_b = [[0; 2]; 16];
    assert_eq!(a.drain_pcm(&mut pcm_a), b.drain_pcm(&mut pcm_b));
    assert_eq!(pcm_a, pcm_b);
    #[cfg(feature = "save-state")]
    {
        use doslet_state::State;
        assert!(a.save() == b.save(), "portable machine state differs");
    }
}

#[test]
fn owned_borrow_matches_pc_execution_for_smc_vga_crossings_ports_and_tracking() {
    // Change the immediate of the next MOV, then store/load across a RAM page.
    let smc = [
        0xb8, 1, 0, 0x2e, 0xc6, 6, 0x0a, 0x10, 7, 0xb8, 2, 0, 0x26, 0xa3, 0xff, 0x1f, 0x26, 0xa1,
        0xff, 0x1f, 0x90,
    ];
    // DS=9fff: first store/load cross RAM/VGA; the following ones hit VGA.
    let video = [
        0xb8, 0x34, 0x12, 0xa3, 0x0f, 0, 0xa1, 0x0f, 0, 0xa3, 0x10, 0, 0xa1, 0x10, 0, 0x26, 0xa3,
        0xff, 0x1f, 0x26, 0xa1, 0xff, 0x1f, 0x90,
    ];
    let ports = [0xba, 0x61, 0, 0xec, 0xee, 0xa3, 0, 0x20, 0x90];
    for (program, data_segment, steps, expected_ax) in [
        (smc.as_slice(), 0, 6, Some(7)),
        (video.as_slice(), 0x9fff, 8, Some(0x1234)),
        (ports.as_slice(), 0, 5, None),
    ] {
        // Active proofs must use the ordinary bus even with the policy enabled.
        for tracking in 0..3 {
            let mut fast = machine(program, data_segment, true);
            let mut reference = machine(program, data_segment, false);
            let spaces = match tracking {
                1 => {
                    fast.bus.ram.begin_write_log();
                    reference.bus.ram.begin_write_log();
                    None
                }
                2 => Some((
                    fast.bus.ram.begin_recurrence(),
                    reference.bus.ram.begin_recurrence(),
                )),
                _ => None,
            };
            run_exactly(&mut fast, steps);
            run_exactly(&mut reference, steps);
            if let Some(expected) = expected_ax {
                assert_eq!(fast.cpu.registers.get(Gpr16::Ax), expected);
            }
            if tracking == 1 {
                assert_eq!(
                    fast.bus.ram.end_write_log(),
                    reference.bus.ram.end_write_log()
                );
            }
            if let Some((a, b)) = spaces {
                assert_eq!(
                    fast.bus.ram.end_recurrence(a),
                    reference.bus.ram.end_recurrence(b)
                );
            }
            same_machine(&mut fast, &mut reference);
        }
    }
}

#[test]
fn owned_borrow_first_decode_and_repeated_batches_allocate_nothing() {
    let mut machine = machine(&[0xb8, 1, 0, 0xa3, 0xff, 0x1f, 0x40, 0xeb, 0xf7], 0, true);
    let measured = allocation_counter::measure(|| {
        for _ in 0..32 {
            run_exactly(&mut machine, 32);
        }
    });
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
}

#[test]
fn zero_prefix_owned_borrow_retries_vga_rep_as_a_complete_pc_batch() {
    // VGA is the first operand of every REP iteration. After RAM-only refusal,
    // the PC bus must batch the whole repeat before stopping at the port read.
    let program = [0xf3, 0xa5, 0xe4, 0x61];
    let mut fast = machine(&program, 0xa000, true);
    let mut reference = machine(&program, 0xa000, false);
    for machine in [&mut fast, &mut reference] {
        machine.cpu.registers.set(Gpr16::Cx, 16);
        machine.cpu.registers.set(Gpr16::Si, 0);
        machine.cpu.registers.set(Gpr16::Di, 0x2000);
        for offset in 0..32 {
            machine
                .bus
                .write_u8(APERTURE_START + offset, offset as u8 ^ 0xa5)
                .unwrap();
        }
    }
    let a = fast.step_with_idle_budget(32).unwrap();
    let b = reference.step_with_idle_budget(32).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.1, 16, "VGA REP must stay batched after a zero prefix");
    assert_eq!(fast.cpu.registers.ip, 0x1002);
    assert_eq!(fast.cpu.registers.get(Gpr16::Cx), 0);
    same_machine(&mut fast, &mut reference);
}

#[cfg(feature = "save-state")]
#[test]
fn owned_borrow_policy_is_not_saved() {
    use doslet_state::State;
    let machine = machine(&[0x90], 0, cfg!(target_arch = "wasm32"));
    assert_eq!(
        machine.borrowed_interpreter_bus,
        cfg!(target_arch = "wasm32")
    );
    let restored = PcMachine::load(machine.save()).unwrap();
    assert_eq!(
        restored.borrowed_interpreter_bus,
        !cfg!(target_arch = "wasm32")
    );
}
