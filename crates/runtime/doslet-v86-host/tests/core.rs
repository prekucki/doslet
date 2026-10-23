use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_v86::Registers;
use doslet_v86_host::{Core, Cpu, Execution};
use std::sync::OnceLock;

#[derive(Default)]
struct Devices {
    output: Vec<(u16, u8)>,
    mapped: [u8; 16],
    mapped_writes: u32,
}
impl MemoryBus for Devices {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        address
            .checked_sub(0xa0000)
            .and_then(|index| self.mapped.get(index as usize))
            .copied()
            .ok_or(BusError::UnmappedMemory(address))
    }
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        let target = address
            .checked_sub(0xa0000)
            .and_then(|index| self.mapped.get_mut(index as usize))
            .ok_or(BusError::UnmappedMemory(address))?;
        *target = value;
        self.mapped_writes += 1;
        Ok(())
    }
}
impl PortBus for Devices {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
        if port == 0x1234 {
            Ok(0x56)
        } else {
            Err(BusError::UnmappedPort(port))
        }
    }
    fn write_port_u8(&mut self, port: u16, value: u8) -> Result<(), BusError> {
        self.output.push((port, value));
        Ok(())
    }
}

fn core() -> &'static Core {
    static CORE: OnceLock<Core> = OnceLock::new();
    CORE.get_or_init(|| {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../target/v86/doslet-v86.wasm"
        );
        Core::compile(&std::fs::read(path).expect(
            "first run cargo run --manifest-path tools/v86-build/Cargo.toml --release --locked",
        ))
        .unwrap()
    })
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn borrowed_devices_access_the_same_ram_and_expire_after_the_call() {
    use doslet_v86::DeviceRequest;
    struct Borrowed<'a>(&'a mut usize);
    impl doslet_v86_host::devices::DeviceHost for Borrowed<'_> {
        fn access(&mut self, ram: &mut [u8], request: DeviceRequest) -> Result<u32, BusError> {
            *self.0 += 1;
            match request {
                DeviceRequest::WriteMemory {
                    address: 0xa0000,
                    value,
                    width: 1,
                } => {
                    ram[0x600] = value as u8;
                    Ok(0)
                }
                DeviceRequest::ReadPort {
                    port: 0x1234,
                    width: 1,
                } => Ok(u32::from(ram[0x600])),
                _ => panic!("unexpected device request: {request:?}"),
            }
        }
    }
    let mut cpu = cpu(&[
        0xb8, 0, 0xa0, 0x8e, 0xc0, 0x26, 0xc6, 0x06, 0, 0, 0x5a, 0xba, 0x34, 0x12, 0xec, 0xf4,
    ]);
    let mut calls = 0;
    let result = cpu
        .run_with_devices(&mut Borrowed(&mut calls), 100, Execution::Interpreter, None)
        .unwrap();
    assert!(result.halted);
    assert_eq!(calls, 2);
    assert_eq!(cpu.ram(0x600, 1).unwrap(), [0x5a]);
    assert_eq!(cpu.registers().unwrap().gpr[0] & 255, 0x5a);
    assert_eq!(cpu.bus().mapped_writes, 0);
    let registers = Registers {
        eip: 0x10e,
        gpr: [0, 0, 0x1234, 0, 0x9000, 0, 0, 0],
        ..Registers::default()
    };
    cpu.reset_real_mode(registers).unwrap();
    finish(&mut cpu, Execution::Interpreter);
    assert_eq!(cpu.registers().unwrap().gpr[0] & 255, 0x56);
    assert_eq!(calls, 2);
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn machine_grants_include_ordinary_extended_opcodes_but_stop_before_rdtsc() {
    let mut cpu = cpu(&[0xb9, 0, 1, 0x0f, 0xb6, 0xc0, 0xe2, 0xfb, 0xf4]);
    cpu.set_machine_boundaries(true).unwrap();
    assert_eq!(cpu.run(128, Execution::Interpreter).unwrap().steps, 128);
    assert!(!cpu.run(128, Execution::Jit).unwrap().halted);
    cpu.write_ram(0x200, &[0x90, 0x0f, 0x31, 0xf4]).unwrap();
    cpu.reset_real_mode(Registers {
        eip: 0x200,
        ..Registers::default()
    })
    .unwrap();
    let result = cpu.run(128, Execution::Jit).unwrap();
    assert_eq!(result.steps, 1);
    assert_eq!(cpu.registers().unwrap().eip, 0x201);
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn background_compilation_cannot_reinstall_code_invalidated_by_the_host() {
    let mut cpu = cpu(&[0xb9, 0xff, 0xff, 0x40, 0xe2, 0xfd, 0xeb, 0xf8]);
    cpu.set_machine_boundaries(true).unwrap();
    for _ in 0..1000 {
        cpu.run(4096, Execution::Jit).unwrap();
        if cpu.statistics().unwrap().compilation_pending {
            break;
        }
    }
    assert!(cpu.statistics().unwrap().compilation_pending);
    // This write happens while native compilation is outstanding. Publishing
    // its eventual result must honor upstream invalidation of that page.
    cpu.write_ram(0x100, &[0xb8, 0x34, 0x12, 0xf4]).unwrap();
    cpu.wait_for_compilation().unwrap();
    let mut context = cpu.context().unwrap();
    context.registers.eip = 0x100;
    cpu.restore_context(&context).unwrap();
    assert_eq!(finish(&mut cpu, Execution::Jit), 2);
    assert_eq!(cpu.registers().unwrap().gpr[0], 0x1234);
}

fn cpu(program: &[u8]) -> Cpu<Devices> {
    let mut cpu = core().instantiate(Devices::default(), 0x20_0000).unwrap();
    cpu.write_ram(0x100, program).unwrap();
    cpu.reset_real_mode(Registers {
        eip: 0x100,
        eflags: 2,
        gpr: [0, 4000, 0, 0, 0x9000, 0, 0, 0],
        ..Registers::default()
    })
    .unwrap();
    cpu
}

fn finish(cpu: &mut Cpu<Devices>, execution: Execution) -> u32 {
    let mut total = 0;
    for _ in 0..100 {
        let result = cpu.run(4096, execution).unwrap();
        total += result.steps;
        if result.halted {
            return total;
        }
    }
    panic!("guest did not halt within test budget");
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn executes_real_mode_arithmetic_memory_and_halt() {
    let mut cpu = cpu(&[0xb8, 0x34, 0x12, 0x05, 2, 0, 0xa3, 0, 2, 0xf4]);
    assert_eq!(finish(&mut cpu, Execution::Interpreter), 4);
    assert_eq!(cpu.registers().unwrap().gpr[0], 0x1236);
    assert_eq!(cpu.ram(0x200, 2).unwrap(), [0x36, 0x12]);
    assert_eq!(cpu.run(0, Execution::Interpreter).unwrap().steps, 0);
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn upstream_jit_really_executes_and_matches_its_interpreter() {
    let program = [0x40, 0xe2, 0xfd, 0xf4];
    let mut reference = cpu(&program);
    let expected = finish(&mut reference, Execution::Interpreter);
    let mut compiled = cpu(&program);
    compiled.prepare_current().unwrap();
    assert!(compiled.statistics().unwrap().compiled_modules > 0);
    assert_eq!(finish(&mut compiled, Execution::Jit), expected);
    assert!(compiled.statistics().unwrap().jit_entries > 0);
    assert_eq!(
        compiled.registers().unwrap(),
        reference.registers().unwrap()
    );
    assert_eq!(
        compiled.ram(0, 0xa0000).unwrap(),
        reference.ram(0, 0xa0000).unwrap()
    );
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn host_code_write_invalidates_the_upstream_jit() {
    let mut compiled = cpu(&[0x40, 0xe2, 0xfd, 0xf4]);
    compiled.prepare_current().unwrap();
    let before = compiled.statistics().unwrap();
    compiled.write_ram(0x100, &[0x48]).unwrap(); // DEC replaces INC.
    assert!(compiled.statistics().unwrap().invalidated_modules > before.invalidated_modules);
    finish(&mut compiled, Execution::Jit);
    assert_eq!(
        compiled.registers().unwrap().gpr[0] & 0xffff,
        (0u16).wrapping_sub(4000) as u32
    );
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn guest_self_modification_invalidates_active_code() {
    // The first instruction changes the loop's INC to DEC in the same page.
    let program = [
        0xc6, 0x06, 0x08, 0x01, 0x48, 0xb9, 0xa0, 0x0f, 0x40, 0xe2, 0xfd, 0xf4,
    ];
    let mut reference = cpu(&program);
    let expected = finish(&mut reference, Execution::Interpreter);
    let mut compiled = cpu(&program);
    compiled.prepare_current().unwrap();
    assert_eq!(finish(&mut compiled, Execution::Jit), expected);
    assert_eq!(
        compiled.registers().unwrap(),
        reference.registers().unwrap()
    );
    assert_eq!(compiled.ram(0x108, 1).unwrap(), [0x48]);
    assert_eq!(
        compiled.ram(0, 0xa0000).unwrap(),
        reference.ram(0, 0xa0000).unwrap()
    );
    assert!(compiled.statistics().unwrap().invalidated_modules > 0);
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn divide_fault_dispatch_counts_match_the_interpreter() {
    let program = [0xb8, 1, 0, 0x31, 0xd2, 0xf7, 0xf2, 0xf4];
    let setup = || {
        let mut cpu = cpu(&program);
        cpu.write_ram(0, &[0, 4, 0, 0]).unwrap(); // #DE -> 0000:0400.
        // Advance saved fault IP past DIV, then return to HLT.
        cpu.write_ram(0x400, &[0x5b, 0x83, 0xc3, 2, 0x53, 0xcf])
            .unwrap();
        cpu
    };
    let mut reference = setup();
    let expected = finish(&mut reference, Execution::Interpreter);
    let mut compiled = setup();
    compiled.prepare_current().unwrap();
    assert_eq!(finish(&mut compiled, Execution::Jit), expected);
    assert_eq!(
        compiled.registers().unwrap(),
        reference.registers().unwrap()
    );
    assert_eq!(
        compiled.ram(0, 0xa0000).unwrap(),
        reference.ram(0, 0xa0000).unwrap()
    );
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn ports_reach_the_host_and_bus_errors_stop_execution() {
    let mut cpu = cpu(&[0xba, 0x34, 0x12, 0xec, 0xee, 0xf4]);
    finish(&mut cpu, Execution::Interpreter);
    assert_eq!(cpu.bus().output, [(0x1234, 0x56)]);
    assert!(cpu.ram(0xa0000, 1).is_err());
    assert!(cpu.ram(u32::MAX, 2).is_err());
    let mut faulty = self::cpu(&[0xba, 0x99, 0, 0xec, 0xf4]);
    assert!(faulty.run(10, Execution::Interpreter).is_err());
    assert!(faulty.run(10, Execution::Interpreter).is_err());
    // APIC addresses must reach the external bus rather than a private v86 chip.
    let mut apic = self::cpu(&[0x66, 0x67, 0xa1, 0, 0, 0xe0, 0xfe, 0xf4]);
    assert!(apic.run(1, Execution::Interpreter).is_err());
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn independent_instances_do_not_share_cpu_ram_or_jit_state() {
    let mut first = cpu(&[0x40, 0xf4]);
    let mut second = cpu(&[0x48, 0xf4]);
    finish(&mut first, Execution::Interpreter);
    finish(&mut second, Execution::Interpreter);
    assert_eq!(first.registers().unwrap().gpr[0], 1);
    assert_eq!(second.registers().unwrap().gpr[0], 0xffff);
    first.write_ram(0x500, &[42]).unwrap();
    assert_eq!(second.ram(0x500, 1).unwrap(), [0]);
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn rep_yields_after_one_element() {
    let mut cpu = cpu(&[0xbf, 0, 4, 0xf3, 0xaa, 0xf4]);
    cpu.run(1, Execution::Interpreter).unwrap(); // MOV DI,400h
    cpu.run(1, Execution::Interpreter).unwrap(); // one STOSB
    let registers = cpu.registers().unwrap();
    assert_eq!(registers.gpr[1], 3999);
    assert_eq!(registers.gpr[7], 0x401);
    assert_eq!(registers.eip, 0x103);
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn rep_device_writes_yield_and_comparisons_commit_flags() {
    let mut cpu = cpu(&[0xf3, 0xaa, 0xf4]);
    cpu.reset_real_mode(Registers {
        eip: 0x100,
        eflags: 2,
        gpr: [42, 4000, 0, 0, 0x9000, 0, 0, 0],
        segments: [0xa000, 0, 0, 0, 0, 0],
    })
    .unwrap();
    cpu.run(1, Execution::Interpreter).unwrap();
    assert_eq!(cpu.bus().mapped_writes, 1);
    assert_eq!(cpu.bus().mapped[0], 42);
    assert_eq!(cpu.registers().unwrap().gpr[1], 3999);

    let mut compare = self::cpu(&[0xbf, 0, 4, 0xb0, 3, 0xf3, 0xae, 0xf4]);
    compare.write_ram(0x400, &[3, 4]).unwrap();
    compare.run(2, Execution::Interpreter).unwrap();
    compare.run(1, Execution::Interpreter).unwrap();
    let equal = compare.registers().unwrap();
    assert_ne!(equal.eflags & 0x40, 0);
    assert_eq!(equal.eip, 0x105);
    compare.run(1, Execution::Interpreter).unwrap();
    let different = compare.registers().unwrap();
    assert_eq!(different.eflags & 0x40, 0);
    assert_eq!(different.eip, 0x107);
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn external_interrupt_wakes_halt_and_iret_restores_context() {
    let mut cpu = cpu(&[0xf4, 0xf4]);
    assert!(!cpu.inject_interrupt(0x30).unwrap());
    cpu.write_ram(0xc0, &[0, 4, 0, 0]).unwrap(); // IVT 30h -> 0000:0400.
    cpu.write_ram(0x400, &[0x40, 0xcf]).unwrap(); // INC AX; IRET.
    cpu.reset_real_mode(Registers {
        eip: 0x100,
        eflags: 0x202,
        gpr: [0, 0, 0, 0, 0x9000, 0, 0, 0],
        ..Registers::default()
    })
    .unwrap();
    assert!(cpu.run(1, Execution::Interpreter).unwrap().halted);
    assert!(cpu.inject_interrupt(0x30).unwrap());
    assert!(!cpu.run(2, Execution::Interpreter).unwrap().halted);
    let registers = cpu.registers().unwrap();
    assert_eq!(registers.eip, 0x101);
    assert_eq!(registers.gpr[0], 1);
    assert_eq!(registers.gpr[4], 0x9000);
    assert_ne!(registers.eflags & 0x200, 0);
    assert!(cpu.run(1, Execution::Interpreter).unwrap().halted);
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn mov_ss_inhibits_external_interrupt_until_following_instruction() {
    for execution in [Execution::Interpreter, Execution::Jit] {
        let mut cpu = cpu(&[0xb8, 0x00, 0x20, 0x8e, 0xd0, 0xbc, 0x00, 0x80, 0xf4]);
        cpu.write_ram(0xc0, &[0, 4, 0, 0]).unwrap();
        cpu.write_ram(0x400, &[0xcf]).unwrap();
        cpu.reset_real_mode(Registers {
            eip: 0x100,
            eflags: 0x202,
            gpr: [0, 0, 0, 0, 0x9000, 0, 0, 0],
            ..Registers::default()
        })
        .unwrap();
        cpu.run(1, execution).unwrap();
        let loaded_ss = cpu.run(1, execution).unwrap();
        assert!(loaded_ss.interrupt_shadow, "{execution:?}");
        assert!(!cpu.inject_interrupt(0x30).unwrap(), "{execution:?}");
        let loaded_sp = cpu.run(1, execution).unwrap();
        assert!(!loaded_sp.interrupt_shadow, "{execution:?}");
        assert_eq!(cpu.registers().unwrap().gpr[4] & 0xffff, 0x8000);
        assert!(cpu.inject_interrupt(0x30).unwrap(), "{execution:?}");
    }
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn pop_ss_and_lss_inhibit_external_interrupt_for_one_instruction() {
    for execution in [Execution::Interpreter, Execution::Jit] {
        for pop in [true, false] {
            let program: &[u8] = if pop {
                &[0xb8, 0x00, 0x20, 0x50, 0x17, 0xbc, 0x00, 0x80, 0xf4]
            } else {
                &[0x0f, 0xb2, 0x26, 0x00, 0x03, 0x90, 0xf4]
            };
            let mut cpu = cpu(program);
            cpu.write_ram(0xc0, &[0, 4, 0, 0]).unwrap();
            cpu.write_ram(0x400, &[0xcf]).unwrap();
            if !pop {
                cpu.write_ram(0x300, &[0x00, 0x80, 0x00, 0x20]).unwrap();
            }
            cpu.reset_real_mode(Registers {
                eip: 0x100,
                eflags: 0x202,
                gpr: [0, 0, 0, 0, 0x9000, 0, 0, 0],
                ..Registers::default()
            })
            .unwrap();
            if pop {
                cpu.run(2, execution).unwrap(); // MOV AX; PUSH AX.
            }
            assert!(
                cpu.run(1, execution).unwrap().interrupt_shadow,
                "{execution:?} pop={pop}"
            );
            assert!(
                !cpu.inject_interrupt(0x30).unwrap(),
                "{execution:?} pop={pop}"
            );
            assert!(
                !cpu.run(1, execution).unwrap().interrupt_shadow,
                "{execution:?} pop={pop}"
            );
            assert!(
                cpu.inject_interrupt(0x30).unwrap(),
                "{execution:?} pop={pop}"
            );
        }
    }
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn guest_switches_to_protected_code32_and_runs_the_jit() {
    let bootstrap = [
        0x0f, 0x01, 0x16, 0x00, 0x03, 0x0f, 0x20, 0xc0, 0x66, 0x83, 0xc8, 1, 0x0f, 0x22, 0xc0,
        0x66, 0xea, 0, 4, 0, 0, 8, 0,
    ];
    let mut compiled = cpu(&bootstrap);
    compiled.write_ram(0x300, &[23, 0, 0x20, 3, 0, 0]).unwrap();
    compiled
        .write_ram(
            0x320,
            &[
                0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0, 0, 0, 0x9a, 0xcf, 0, 0xff, 0xff, 0, 0, 0,
                0x92, 0xcf, 0,
            ],
        )
        .unwrap();
    compiled
        .write_ram(0x400, &[0x40, 0xe2, 0xfd, 0xf4])
        .unwrap();
    for _ in 0..5 {
        compiled.run(1, Execution::Interpreter).unwrap();
    }
    assert_eq!(compiled.registers().unwrap().segments[1], 8);
    assert_eq!(compiled.registers().unwrap().eip, 0x400);
    compiled.prepare_current().unwrap();
    finish(&mut compiled, Execution::Jit);
    assert!(compiled.statistics().unwrap().jit_entries > 0);
    // EAX retains reset CR0 bits plus PE, then receives 4000 INCs.
    assert_eq!(compiled.registers().unwrap().gpr[0], 0x6000_0011u32 + 4000);
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn iret_to_not_present_code_delivers_np_without_consuming_the_return_frame() {
    for execution in [Execution::Interpreter, Execution::Jit] {
        let mut cpu = cpu(&[0xcf]);
        cpu.write_ram(0x200, &[0xf4]).unwrap(); // #NP handler: HLT.
        cpu.write_ram(
            0x300,
            &[
                0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0, 0, 0, 0x9b, 0x40, 0, 0xff, 0xff, 0, 0, 0,
                0x93, 0x40, 0, 0xff, 0xff, 0, 0, 0, 0x1b, 0x40, 0,
            ],
        )
        .unwrap();
        cpu.write_ram(0x400 + 11 * 8, &[0, 2, 8, 0, 0, 0x8e, 0, 0])
            .unwrap();
        let frame: Vec<u8> = [0x1234u32, 24, 2]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        cpu.write_ram(0x8000, &frame).unwrap();
        let mut state = cpu.context().unwrap();
        state.protected = true;
        state.cr[0] |= 1;
        state.code32 = true;
        state.stack32 = true;
        state.gdtr = (0x300, 31);
        state.idtr = (0x400, 0x7ff);
        state.registers.gpr[4] = 0x8000;
        for (index, selector, access) in [(1, 8, 0x9b), (2, 16, 0x93)] {
            state.segments[index] = doslet_v86::Segment {
                selector,
                base: 0,
                limit: 0xffff,
                access,
                default32: true,
                usable: true,
            };
        }
        cpu.restore_context(&state).unwrap();
        finish(&mut cpu, execution);
        let registers = cpu.registers().unwrap();
        assert_eq!(registers.segments[1], 8);
        assert_eq!(registers.eip, 0x201);
        assert_eq!(registers.gpr[4], 0x7ff0);
        let expected: Vec<u8> = [24u32, 0x100, 8, 2]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        assert_eq!(cpu.ram(0x7ff0, 16).unwrap(), expected);
        assert_eq!(cpu.ram(0x8000, 12).unwrap(), frame);
    }
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn rejected_iret_selectors_deliver_guest_faults_with_the_original_frame_intact() {
    // selector, descriptor access, table limit, current CPL, target IP,
    // descriptor limit, exception vector, selector error code.
    let cases = [
        (0u16, 0x9b, 31, 0, 0x1234u16, 0xffff, 13, 0),
        (3, 0x9b, 31, 0, 0x1234, 0xffff, 13, 0),
        (32, 0x9b, 31, 0, 0x1234, 0xffff, 13, 32),
        (4, 0x9b, 31, 0, 0x1234, 0xffff, 13, 4),
        (24, 0x9b, 30, 0, 0x1234, 0xffff, 13, 24),
        (24, 0x93, 31, 0, 0x1234, 0xffff, 13, 24),
        (24, 0x89, 31, 0, 0x1234, 0xffff, 13, 24),
        (24, 0xfb, 31, 0, 0x1234, 0xffff, 13, 24),
        (24, 0xff, 31, 0, 0x1234, 0xffff, 13, 24),
        (24, 0x9b, 31, 3, 0x1234, 0xffff, 13, 24),
        (24, 0x1b, 31, 0, 0x1234, 0xffff, 11, 24),
        (24, 0x9b, 31, 0, 0x1234, 0x100, 13, 0),
    ];
    for code32 in [false, true] {
        for execution in [Execution::Interpreter, Execution::Jit] {
            for (selector, access, table_limit, cpl, target, limit, vector, error) in cases {
                let mut cpu = cpu(&[0xcf]);
                let code_selector = 8 | u16::from(cpl);
                let mut descriptors = [0u8; 32];
                for (offset, rights, descriptor_limit) in [
                    (8, 0x9b | (cpl << 5), 0xffffu16),
                    (16, 0x93 | (cpl << 5), 0xffff),
                    (24, access, limit),
                ] {
                    descriptors[offset..offset + 2]
                        .copy_from_slice(&descriptor_limit.to_le_bytes());
                    descriptors[offset + 5] = rights;
                    descriptors[offset + 6] = 0x40;
                }
                cpu.write_ram(0x300, &descriptors).unwrap();
                cpu.write_ram(
                    0x400 + vector * 8,
                    &[0, 2, code_selector as u8, 0, 0, 0x8e, 0, 0],
                )
                .unwrap();
                let values = [u32::from(target), u32::from(selector), 2];
                let frame: Vec<u8> = values
                    .into_iter()
                    .flat_map(|value| {
                        let bytes = value.to_le_bytes();
                        bytes[..if code32 { 4 } else { 2 }].to_vec()
                    })
                    .collect();
                cpu.write_ram(0x8000, &frame).unwrap();
                let mut state = cpu.context().unwrap();
                state.protected = true;
                state.cr[0] |= 1;
                state.code32 = code32;
                state.stack32 = true;
                state.cpl = cpl;
                state.gdtr = (0x300, table_limit);
                state.idtr = (0x400, 0x7ff);
                state.registers.gpr[4] = 0x8000;
                for (index, sel, rights, default32) in [
                    (1, code_selector, 0x9b | (cpl << 5), code32),
                    (2, 16 | u16::from(cpl), 0x93 | (cpl << 5), true),
                ] {
                    state.segments[index] = doslet_v86::Segment {
                        selector: sel,
                        base: 0,
                        limit: 0xffff,
                        access: rights,
                        default32,
                        usable: true,
                    };
                }
                cpu.restore_context(&state).unwrap();
                cpu.run(1, execution).unwrap();
                let registers = cpu.registers().unwrap();
                assert_eq!(
                    registers.eip, 0x200,
                    "selector={selector:x}, width32={code32}"
                );
                assert_eq!(registers.segments[1], code_selector);
                assert_eq!(registers.gpr[4], 0x7ff0);
                let expected: Vec<u8> = [error, 0x100, u32::from(code_selector), 2]
                    .into_iter()
                    .flat_map(u32::to_le_bytes)
                    .collect();
                assert_eq!(cpu.ram(0x7ff0, 16).unwrap(), expected);
                assert_eq!(cpu.ram(0x8000, frame.len() as u32).unwrap(), frame);
                assert!(cpu.iret_diagnostic().contains("IRET fault:"));
                assert!(cpu.iret_diagnostic().contains("ss:esp="));
                if code32 && selector == 32 {
                    // A second failure during #GP delivery must retain the
                    // original rejected return, rather than just its panic.
                    state.idtr.1 = 0;
                    cpu.restore_context(&state).unwrap();
                    let error = cpu.run(1, execution).unwrap_err();
                    let message = format!("{error:#}");
                    assert!(
                        message.contains("IRET fault: CS outside table"),
                        "{message}"
                    );
                    assert!(message.contains("Unimplemented: #GP handler"), "{message}");
                }
            }
        }
    }
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn exception_exits_preserve_fault_state_but_deliver_software_and_external_interrupts() {
    let mut cpu = cpu(&[0xfa]); // CLI faults at CPL3, IOPL0.
    cpu.write_ram(
        0x300,
        &[
            0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff, 0, 0, 0, 0xfb, 0x40, 0, 0xff, 0xff, 0, 0, 0, 0xf3,
            0x40, 0,
        ],
    )
    .unwrap();
    cpu.write_ram(0x400 + 13 * 8, &[0, 2, 11, 0, 0, 0xee, 0, 0])
        .unwrap();
    let mut state = cpu.context().unwrap();
    state.protected = true;
    state.cr[0] |= 1;
    state.code32 = true;
    state.stack32 = true;
    state.cpl = 3;
    state.gdtr = (0x300, 23);
    state.idtr = (0x400, 0x7ff);
    state.registers.gpr[4] = 0x8000;
    state.registers.eflags = 0x202;
    for (index, selector, access) in [(1, 11, 0xfb), (2, 19, 0xf3)] {
        state.segments[index] = doslet_v86::Segment {
            selector,
            base: 0,
            limit: 0xffff,
            access,
            default32: true,
            usable: true,
        };
    }
    cpu.restore_context(&state).unwrap();
    cpu.set_exception_exits(true).unwrap();
    let before_stack = cpu.ram(0x7ff0, 16).unwrap().to_vec();
    let result = cpu.run(1, Execution::Jit).unwrap();
    assert_eq!(
        result.exception,
        Some(doslet_v86::Exception {
            vector: 13,
            error_code: Some(0)
        })
    );
    assert_eq!(cpu.registers().unwrap().eip, 0x100);
    assert_eq!(cpu.registers().unwrap().gpr[4], 0x8000);
    assert_eq!(cpu.ram(0x7ff0, 16).unwrap(), before_stack);
    // An explicit INT 0Dh and PIC vector 0Dh have no exception error word.
    // Neither may be mistaken for the intercepted #GP(0).
    cpu.write_ram(0x100, &[0xcd, 13]).unwrap();
    let software = cpu.run(1, Execution::Interpreter).unwrap();
    assert_eq!(software.exception, None);
    assert_eq!(cpu.registers().unwrap().eip, 0x200);
    assert_eq!(cpu.registers().unwrap().gpr[4], 0x7ff4);
    cpu.restore_context(&state).unwrap();
    assert!(cpu.inject_interrupt(13).unwrap());
    assert_eq!(cpu.registers().unwrap().eip, 0x200);
    assert_eq!(cpu.registers().unwrap().gpr[4], 0x7ff4);
    assert_eq!(cpu.run(0, Execution::Interpreter).unwrap().exception, None);
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn pic_ports_belong_to_the_external_bus() {
    let mut cpu = cpu(&[0xb0, 0x20, 0xe6, 0x20, 0xf4]);
    finish(&mut cpu, Execution::Interpreter);
    assert_eq!(cpu.bus().output, [(0x20, 0x20)]);
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn x87_executes_in_the_upstream_cpu() {
    let mut cpu = cpu(&[0xd9, 0xe8, 0xd9, 0xe8, 0xde, 0xc1, 0xd9, 0x1e, 0, 4, 0xf4]);
    finish(&mut cpu, Execution::Interpreter);
    assert_eq!(cpu.ram(0x400, 4).unwrap(), 2.0f32.to_le_bytes());
}

#[test]
#[ignore = "requires the pinned v86 WASM module built by tools/v86-build"]
fn real_mode_integer_execution_matches_doslet_step_by_step() {
    use doslet_x86::{Cpu as Reference, Gpr32};
    struct Ram(Vec<u8>);
    impl MemoryBus for Ram {
        fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
            self.0
                .get(address as usize)
                .copied()
                .ok_or(BusError::UnmappedMemory(address))
        }
        fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
            *self
                .0
                .get_mut(address as usize)
                .ok_or(BusError::UnmappedMemory(address))? = value;
            Ok(())
        }
    }
    impl PortBus for Ram {
        fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
            Err(BusError::UnmappedPort(port))
        }
        fn write_port_u8(&mut self, port: u16, _: u8) -> Result<(), BusError> {
            Err(BusError::UnmappedPort(port))
        }
    }
    let program = [
        0xb8, 0xff, 0x7f, 0x05, 1, 0, 0x50, 0x5b, 0x31, 0xd8, 0xb0, 0x80, 0x04, 0x80, 0xa3, 0, 4,
        0xf4,
    ];
    let mut upstream = cpu(&program);
    let mut reference = Reference::default();
    reference.registers.ip = 0x100;
    reference.registers.set32(Gpr32::Ecx, 4000);
    reference.registers.set32(Gpr32::Esp, 0x9000);
    let mut ram = Ram(vec![0; 0x20_0000]);
    ram.0[0x100..0x100 + program.len()].copy_from_slice(&program);
    let registers = [
        Gpr32::Eax,
        Gpr32::Ecx,
        Gpr32::Edx,
        Gpr32::Ebx,
        Gpr32::Esp,
        Gpr32::Ebp,
        Gpr32::Esi,
        Gpr32::Edi,
    ];
    for _ in 0..9 {
        reference.step(&mut ram).unwrap();
        upstream.run(1, Execution::Interpreter).unwrap();
        let actual = upstream.registers().unwrap();
        for (index, register) in registers.into_iter().enumerate() {
            assert_eq!(actual.gpr[index], reference.registers.get32(register));
        }
        assert_eq!(actual.eip, reference.registers.ip);
        // AF is architecturally undefined after XOR.
        assert_eq!(actual.eflags & 0x8c5, reference.flags & 0x8c5);
        assert_eq!(upstream.ram(0, 0xa0000).unwrap(), &ram.0[..0xa0000]);
    }
}
