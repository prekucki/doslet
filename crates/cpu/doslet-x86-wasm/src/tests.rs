extern crate std;
use crate::{
    Translation,
    host::{CpuRuntime, Host},
    native::Runner,
};
use alloc::{vec, vec::Vec};
use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_x86::{Cpu, DescriptorTable, Gpr32, SegmentRegister, StepOutcome};

#[derive(Clone)]
struct Bus(Vec<u8>);
impl MemoryBus for Bus {
    fn read_u8(&mut self, a: u32) -> Result<u8, BusError> {
        self.0
            .get(a as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(a))
    }
    fn write_u8(&mut self, a: u32, v: u8) -> Result<(), BusError> {
        *self
            .0
            .get_mut(a as usize)
            .ok_or(BusError::UnmappedMemory(a))? = v;
        Ok(())
    }
    fn code_bytes(&self, a: u32, len: u8) -> Option<&[u8]> {
        self.0.get(a as usize..a as usize + len as usize)
    }
}
impl PortBus for Bus {
    fn read_port_u8(&mut self, p: u16) -> Result<u8, BusError> {
        Ok(p as u8)
    }
    fn write_port_u8(&mut self, _p: u16, _v: u8) -> Result<(), BusError> {
        Ok(())
    }
}
fn profile(records: &[(u32, &[u8])]) -> Vec<u8> {
    let mut p = b"DSJ1".to_vec();
    p.extend_from_slice(&(records.len() as u16).to_le_bytes());
    for &(hint, code) in records {
        p.extend_from_slice(&hint.to_le_bytes());
        p.push(code.len() as u8);
        p.extend_from_slice(code);
    }
    p
}
fn runtime(code: &[u8], flags: u32) -> CpuRuntime<Bus> {
    let mut bus = Bus(vec![0; 65536]);
    bus.0[0x1000..0x1000 + code.len()].copy_from_slice(code);
    // Flat code/data descriptors, loaded through the real segment validator.
    bus.0[0x208..0x210].copy_from_slice(&[0xff, 0xff, 0, 0, 0, 0x9a, 0xcf, 0]);
    bus.0[0x210..0x218].copy_from_slice(&[0xff, 0xff, 0, 0, 0, 0x92, 0xcf, 0]);
    let mut cpu = Cpu::default();
    cpu.set_decode_cache(false);
    cpu.set_dynamic_core(false);
    cpu.control.gdtr = DescriptorTable {
        base: 0x200,
        limit: 23,
    };
    cpu.set_protected_mode(true);
    cpu.load_segment(&mut bus, SegmentRegister::Cs, 8).unwrap();
    cpu.load_segment(&mut bus, SegmentRegister::Ds, 16).unwrap();
    cpu.registers.ip = 0x1000;
    cpu.flags = flags;
    CpuRuntime {
        cpu,
        bus,
        translated_limit: crate::MAX_QUANTUM,
    }
}
fn compare(runner: &Runner<CpuRuntime<Bus>>, reference: &CpuRuntime<Bus>) {
    let actual = runner.host().runtime();
    assert!(
        reference.cpu.matches(&actual.cpu.snapshot()),
        "CPU mismatch at {:x}/{:x}",
        actual.cpu.registers.ip,
        reference.cpu.registers.ip
    );
    assert_eq!(actual.bus.0, reference.bus.0);
}
fn step(reference: &mut CpuRuntime<Bus>, budget: u32) -> u32 {
    let mut done = 0;
    while done < budget && !reference.cpu.is_halted() {
        let outcome = reference.cpu.step(&mut reference.bus).unwrap();
        done += 1;
        if outcome == StepOutcome::Halted {
            break;
        }
    }
    done
}
#[test]
fn internal_dispatch_runs_two_blocks_and_backedge_without_host_exits() {
    let a = &[0x40, 0xeb, 0x0d][..]; // inc eax; jmp 0x1010
    let b = &[0x43, 0xeb, 0xed][..]; // inc ebx; jmp 0x1000
    let mut code = vec![0x90; 19];
    code[..3].copy_from_slice(a);
    code[16..19].copy_from_slice(b);
    let p = profile(&[(0x1000, a), (0x1010, b)]);
    let mut runner = Runner::new(Host::new(
        Translation::from_profile(&p).unwrap(),
        runtime(&code, 0x203),
    ))
    .unwrap();
    let mut reference = runtime(&code, 0x203);
    assert_eq!(runner.run(4000).unwrap(), step(&mut reference, 4000));
    compare(&runner, &reference);
    assert_eq!(runner.host().stats().translated_steps, 4000);
    assert_eq!(runner.host().stats().polls, 2);
    assert_eq!(runner.host().stats().grants, 1);
}
#[test]
fn arithmetic_matches_interpreter_for_boundaries_and_random_inputs() {
    let values = [
        0,
        1,
        0x7fff_ffff,
        0x8000_0000,
        0xffff_ffff,
        0xffff_fffe,
        0xff,
        0x10,
    ];
    // Every ALU form, CMP, TEST, INC/DEC, NEG/NOT and XCHG.
    let codes: &[&[u8]] = &[
        &[0x01, 0xd8],
        &[0x09, 0xd8],
        &[0x11, 0xd8],
        &[0x19, 0xd8],
        &[0x21, 0xd8],
        &[0x29, 0xd8],
        &[0x31, 0xd8],
        &[0x39, 0xd8],
        &[0x85, 0xd8],
        &[0x40],
        &[0x48],
        &[0xf7, 0xd8],
        &[0xf7, 0xd0],
        &[0x93],
    ];
    for &code in codes {
        let p = profile(&[(0x1000, code)]);
        let mut runner = Runner::new(Host::new(
            Translation::from_profile(&p).unwrap(),
            runtime(code, 2),
        ))
        .unwrap();
        let mut reference = runtime(code, 2);
        let mut seed = 0x1234_5678u32;
        for case in 0..192 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let left = if case < 64 { values[case / 8] } else { seed };
            let right = if case < 64 {
                values[case % 8]
            } else {
                seed.rotate_left(13)
            };
            for runtime in [runner.host_mut().runtime_mut(), &mut reference] {
                runtime.cpu.registers.ip = 0x1000;
                runtime.cpu.registers.set32(Gpr32::Eax, left);
                runtime.cpu.registers.set32(Gpr32::Ebx, right);
                runtime.cpu.flags = 0x202 | ((case as u32 & 1) * 0x8d5);
            }
            assert_eq!(runner.run(1).unwrap(), step(&mut reference, 1));
            compare(&runner, &reference);
        }
        assert_eq!(runner.host().stats().translated_steps, 192);
    }
}
#[test]
fn all_branch_conditions_match_for_all_arithmetic_flag_combinations() {
    for condition in 0..16 {
        let code = [0x70 + condition, 2];
        let mut runner = Runner::new(Host::new(
            Translation::from_profile(&profile(&[(0x1000, &code)])).unwrap(),
            runtime(&code, 2),
        ))
        .unwrap();
        let mut reference = runtime(&code, 2);
        for mask in 0..32 {
            let flags = [1, 4, 0x40, 0x80, 0x800]
                .iter()
                .enumerate()
                .fold(0x202, |f, (i, bit)| {
                    f | if mask & (1 << i) != 0 { *bit } else { 0 }
                });
            for runtime in [runner.host_mut().runtime_mut(), &mut reference] {
                runtime.cpu.registers.ip = 0x1000;
                runtime.cpu.flags = flags;
            }
            runner.run(1).unwrap();
            step(&mut reference, 1);
            compare(&runner, &reference);
        }
        assert_eq!(runner.host().stats().translated_steps, 32);
    }
}
#[test]
fn interpreter_memory_write_invalidates_later_translated_code() {
    // MOV byte [0x1007], 0x48; INC eax; JMP back to rewritten INC/DEC.
    let code = [0xc6, 0x05, 0x07, 0x10, 0, 0, 0x48, 0x40, 0xeb, 0xfd];
    let p = profile(&[(0x1007, &code[7..])]);
    let mut runner = Runner::new(Host::new(
        Translation::from_profile(&p).unwrap(),
        runtime(&code, 0x203),
    ))
    .unwrap();
    let mut reference = runtime(&code, 0x203);
    runner.run(20).unwrap();
    step(&mut reference, 20);
    compare(&runner, &reference);
    assert_eq!(runner.host().stats().translated_steps, 0);
    assert_eq!(runner.host().stats().interpreted_steps, 20);
}
#[test]
fn unsupported_memory_io_and_halt_use_interpreter_then_resume_translation() {
    let code = [0x40, 0xa3, 0x00, 0x30, 0, 0, 0xec, 0x43, 0xf4];
    let p = profile(&[(0x1000, &code)]);
    let mut runner = Runner::new(Host::new(
        Translation::from_profile(&p).unwrap(),
        runtime(&code, 2),
    ))
    .unwrap();
    let mut reference = runtime(&code, 2);
    assert_eq!(runner.run(100).unwrap(), step(&mut reference, 100));
    compare(&runner, &reference);
    assert_eq!(runner.host().stats().translated_steps, 2);
    assert_eq!(runner.host().stats().interpreted_steps, 3);
    assert_eq!(runner.run(100).unwrap(), 0);
}
#[test]
fn relocation_zero_budget_short_budget_and_quantum_limits() {
    let code = [0x40, 0x43, 0xeb, 0xfc];
    let p = profile(&[(0x5000, &code)]);
    let mut runner = Runner::new(Host::new(
        Translation::from_profile(&p).unwrap(),
        runtime(&code, 0x203),
    ))
    .unwrap();
    let mut reference = runtime(&code, 0x203);
    assert_eq!(runner.run(0).unwrap(), 0);
    for budget in [1, 2, 3, 7, 50, 10000] {
        assert_eq!(runner.run(budget).unwrap(), step(&mut reference, budget));
        compare(&runner, &reference);
    }
    assert!(runner.host().stats().translated_steps > 9000);
    assert!(runner.host().stats().grants >= 3);
}
#[test]
fn no_scheduler_grant_or_debug_state_means_interpreter() {
    for flags in [0x202, 0x302, 0x10202] {
        let code = [0x40, 0xeb, 0xfd];
        let p = profile(&[(0x1000, &code)]);
        let mut rt = runtime(&code, flags);
        if flags == 0x202 {
            rt.translated_limit = 0;
        }
        let mut runner =
            Runner::new(Host::new(Translation::from_profile(&p).unwrap(), rt)).unwrap();
        runner.run(10).unwrap();
        assert_eq!(runner.host().stats().translated_steps, 0);
        assert_eq!(runner.host().stats().interpreted_steps, 10);
    }
}
#[test]
fn malformed_profiles_are_rejected_and_unknown_code_has_no_compiled_entry() {
    let good = profile(&[(0x1000, &[0x40, 0x43])]);
    for end in 0..good.len() {
        assert!(Translation::from_profile(&good[..end]).is_err());
    }
    let mut extra = good.clone();
    extra.push(0);
    assert!(Translation::from_profile(&extra).is_err());
    let mut bad = good;
    bad[10] = 241;
    assert!(Translation::from_profile(&bad).is_err());
    let t = Translation::from_profile(&profile(&[(0x1000, &[0x0f, 0xff])])).unwrap();
    assert!(t.blocks().is_empty());
    let mut runner = Runner::new(Host::new(t, runtime(&[0xf4], 2))).unwrap();
    assert_eq!(runner.run(10).unwrap(), 1);
}

fn json_node(pc: u32, bits: u8, code: &[u8]) -> serde_json::Value {
    let mut bytes = [0x90; 15];
    bytes[..code.len()].copy_from_slice(code);
    serde_json::json!({"pc":pc,"bits":bits,"bytes":bytes,"tier":"fast","reason":"learned"})
}
#[test]
fn speed_tiers_stitches_observed_nodes_but_does_not_decode_unobserved_windows() {
    let code = [0x40, 0x43, 0xeb, 0xfc];
    let nodes = vec![
        json_node(0x1000, 32, &code),
        json_node(0x1001, 32, &code[1..]),
        json_node(0x1002, 32, &code[2..]),
    ];
    for metadata in [
        serde_json::json!({"version":1}),
        serde_json::json!({"learning_version":1,"identity":"exe-sha256:fixture"}),
    ] {
        let mut value = metadata;
        value["nodes"] = serde_json::json!(nodes);
        let json = serde_json::to_vec(&value).unwrap();
        let t = Translation::from_bytes(&json).unwrap();
        assert_eq!(t.blocks().len(), 3);
        assert_eq!(t.blocks()[0].instructions.len(), 3);
        let mut runner = Runner::new(Host::new(t, runtime(&code, 0x203))).unwrap();
        let mut reference = runtime(&code, 0x203);
        runner.run(3000).unwrap();
        step(&mut reference, 3000);
        compare(&runner, &reference);
        assert_eq!(runner.host().stats().translated_steps, 3000);
        assert_eq!(runner.host().stats().polls, 2);
    }
    let value = serde_json::json!({"nodes":[json_node(0x1000, 32, &code)]});
    let t = Translation::from_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(t.blocks()[0].bytes, [0x40]);
}
#[test]
fn speed_profile_conflicts_modes_and_unsupported_code_retain_fallback() {
    let mut conflict = json_node(0x1000, 32, &[0x40]);
    conflict["reason"] = "code-conflict".into();
    let value = serde_json::json!({"nodes":[conflict,
        json_node(0x1001,16,&[0x43]), json_node(0x1002,32,&[0x8b,0x03]),
        json_node(0x1004,32,&[0x40]), json_node(0x1004,32,&[0x48]),
        json_node(0x1010,32,&[0x40])]});
    let t = Translation::from_speed_tiers(&serde_json::to_vec(&value).unwrap()).unwrap();
    let report = t.report();
    assert_eq!(report.input_nodes, 6);
    assert_eq!(report.code16_nodes, 1);
    assert_eq!(report.conflicting_nodes, 2);
    assert_eq!(report.unsupported_nodes, 1);
    assert_eq!(report.compiled_entries, 1);
    assert_eq!(t.blocks()[0].address_hint, 0x1010);
    for bad in [
        serde_json::json!({"nodes":[{"pc":0,"bits":32,"bytes":[64]}]}),
        serde_json::json!({"version":2,"nodes":[]}),
        serde_json::json!({"nodes":[json_node(0,64,&[0x40])]}),
    ] {
        assert!(Translation::from_speed_tiers(&serde_json::to_vec(&bad).unwrap()).is_err());
    }
}
#[test]
fn speed_profile_capacity_prioritizes_frequency_and_generated_large_module_is_valid() {
    let mut nodes: Vec<_> = (0..crate::MAX_BLOCKS + 1)
        .map(|i| json_node(0x1000 + i as u32 * 16, 32, &[0x40]))
        .collect();
    nodes[crate::MAX_BLOCKS]["high_count"] = 10000.into();
    let t = Translation::from_speed_tiers(
        &serde_json::to_vec(&serde_json::json!({"nodes":nodes})).unwrap(),
    )
    .unwrap();
    assert_eq!(t.report().omitted_entries, 1);
    assert_eq!(
        t.blocks()[0].address_hint,
        0x1000 + crate::MAX_BLOCKS as u32 * 16
    );
    let engine = wasmi::Engine::default();
    wasmi::Module::new(&engine, t.wasm()).unwrap();
}

#[test]
fn warmed_execution_allocates_nothing_across_translated_and_interpreted_work() {
    for code in [&[0x40, 0x43, 0xeb, 0xfc][..], &[0x40, 0xec, 0xeb, 0xfc][..]] {
        let p = profile(&[(0x1000, code)]);
        let mut runner = Runner::new(Host::new(
            Translation::from_profile(&p).unwrap(),
            runtime(code, 0x203),
        ))
        .unwrap();
        runner.run(4000).unwrap(); // Reserve the native WASM engine's execution stack.
        let info = allocation_counter::measure(|| {
            for _ in 0..4 {
                assert_eq!(runner.run(4000).unwrap(), 4000);
            }
        });
        assert_eq!(info.count_total, 0);
    }
}

#[test]
fn a_runtime_deadline_commits_state_and_rechecks_code_before_more_translation() {
    use crate::host::Runtime;
    struct Timed {
        inner: CpuRuntime<Bus>,
        ticks: u32,
    }
    impl Timed {
        fn account(&mut self, steps: u32) {
            self.ticks += steps;
            if self.ticks == 7 {
                self.inner.bus.0[0x1000] = 0x48;
            }
        }
    }
    impl Runtime for Timed {
        type Error = doslet_x86::CpuError;
        fn cpu(&self) -> &Cpu {
            &self.inner.cpu
        }
        fn cpu_mut(&mut self) -> &mut Cpu {
            &mut self.inner.cpu
        }
        fn code_bytes(&self, a: u32, l: u8) -> Option<&[u8]> {
            self.inner.bus.code_bytes(a, l)
        }
        fn translated_budget(&self, remaining: u32) -> u32 {
            if self.ticks < 7 {
                remaining.min(7 - self.ticks)
            } else {
                remaining
            }
        }
        fn account_translated(&mut self, steps: u32) -> Result<(), Self::Error> {
            assert!(self.ticks >= 7 || self.ticks + steps <= 7);
            self.account(steps);
            Ok(())
        }
        fn interpret(&mut self) -> Result<StepOutcome, Self::Error> {
            let result = self.inner.interpret()?;
            self.account(1);
            Ok(result)
        }
    }
    let code = [0x40, 0xeb, 0xfd];
    let p = profile(&[(0x1000, &code)]);
    let mut runner = Runner::new(Host::new(
        Translation::from_profile(&p).unwrap(),
        Timed {
            inner: runtime(&code, 0x203),
            ticks: 0,
        },
    ))
    .unwrap();
    let mut reference = Timed {
        inner: runtime(&code, 0x203),
        ticks: 0,
    };
    runner.run(30).unwrap();
    for _ in 0..30 {
        reference.interpret().unwrap();
    }
    let actual = runner.host().runtime();
    assert_eq!(actual.ticks, reference.ticks);
    assert!(actual.inner.cpu.matches(&reference.inner.cpu.snapshot()));
    assert_eq!(actual.inner.bus.0, reference.inner.bus.0);
    assert_eq!(runner.host().stats().translated_steps, 6);
}

#[test]
fn runtime_errors_stop_without_replaying_the_faulting_instruction() {
    let p = profile(&[]);
    let mut runner = Runner::new(Host::new(
        Translation::from_profile(&p).unwrap(),
        runtime(&[0x0f, 0xff], 2),
    ))
    .unwrap();
    assert!(runner.run(10).is_err());
    let polls = runner.host().stats().polls;
    assert!(runner.run(10).is_err());
    assert_eq!(runner.host().stats().polls, polls);
}

#[test]
fn identical_code_at_multiple_profile_addresses_keeps_all_successors_bound() {
    let code = [0x40, 0x40, 0xeb, 0xfc];
    let p = profile(&[
        (0x1000, &code[..1]),
        (0x1001, &code[1..2]),
        (0x1002, &code[2..]),
    ]);
    let mut runner = Runner::new(Host::new(
        Translation::from_profile(&p).unwrap(),
        runtime(&code, 0x203),
    ))
    .unwrap();
    let mut reference = runtime(&code, 0x203);
    runner.run(4000).unwrap();
    step(&mut reference, 4000);
    compare(&runner, &reference);
    assert_eq!(runner.host().stats().translated_steps, 4000);
    assert_eq!(runner.host().stats().polls, 2);
}
