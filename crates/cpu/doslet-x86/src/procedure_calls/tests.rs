use super::*;
use doslet_bus::PortBus;

const ENTRY: u32 = 0x20;
const STACK: u32 = 0x100;
const WORK: u32 = 4;
const FUNCTION: u32 = 73;

struct Bus {
    ram: [u8; 512],
    plain: bool,
    fail_read: bool,
}

impl MemoryBus for Bus {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        if self.fail_read {
            return Err(BusError::UnmappedMemory(address));
        }
        self.ram
            .get(address as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(address))
    }

    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        *self
            .ram
            .get_mut(address as usize)
            .ok_or(BusError::UnmappedMemory(address))? = value;
        Ok(())
    }

    fn is_plain_ram(&self, address: u32, length: u8) -> bool {
        self.plain
            && address
                .checked_add(u32::from(length))
                .is_some_and(|end| end as usize <= self.ram.len())
    }
}

impl PortBus for Bus {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
        Err(BusError::UnmappedPort(port))
    }

    fn write_port_u8(&mut self, port: u16, _: u8) -> Result<(), BusError> {
        Err(BusError::UnmappedPort(port))
    }
}

#[derive(Default)]
struct Executor {
    calls: u32,
    bad: Option<ProcedureResultError>,
    fail: bool,
}

impl ProcedureExecutor for Executor {
    type Error = &'static str;

    fn execute(
        &mut self,
        function_id: u32,
        mut input: ProcedureRegisters,
    ) -> Result<ProcedureResult, Self::Error> {
        assert_eq!(function_id, FUNCTION);
        self.calls += 1;
        if self.fail {
            return Err("backend failure");
        }
        (input.gpr[0], input.flags) = flags::alu32(input.flags, input.gpr[0], input.gpr[3], 0);
        input.gpr[2] = input.gpr[0];
        (input.gpr[6], input.flags) = flags::alu32(input.flags, input.gpr[6], input.gpr[2], 6);
        let mut result = ProcedureResult {
            registers: input,
            instructions: WORK,
        };
        match self.bad {
            Some(ProcedureResultError::InstructionCount) => result.instructions += 1,
            Some(ProcedureResultError::StackPointer) => result.registers.gpr[4] += 4,
            Some(ProcedureResultError::ControlFlags) => result.registers.flags ^= flags::INTERRUPT,
            None => {}
        }
        Ok(result)
    }
}

fn setup() -> (Cpu, Bus) {
    let mut cpu = Cpu::default();
    cpu.set_protected_mode(true);
    for (segment, access, selector) in [
        (SegmentRegister::Cs, 0x9B, 8),
        (SegmentRegister::Ss, 0x93, 16),
    ] {
        cpu.registers.set_segment(
            segment,
            SegmentCache {
                selector,
                base: 0,
                limit: u32::MAX,
                access,
                flags: 0xC0,
                default32: true,
                usable: true,
            },
        );
    }
    cpu.registers.set32(Gpr32::Esp, STACK);
    let mut bus = Bus {
        ram: [0; 512],
        plain: true,
        fail_read: false,
    };
    // CALL 20h; ... ADD EAX,EBX; MOV EDX,EAX; XOR ESI,EDX; RET32.
    bus.ram[..5].copy_from_slice(&[0xE8, 0x1B, 0, 0, 0]);
    bus.ram[ENTRY as usize..ENTRY as usize + 7]
        .copy_from_slice(&[0x01, 0xD8, 0x89, 0xC2, 0x31, 0xD6, 0xC3]);
    (cpu, bus)
}

fn entry(cpu: &Cpu) -> ProcedureEntry {
    ProcedureEntry {
        entry_ip: ENTRY,
        context: ProcedureContext::capture(cpu).unwrap(),
        function_id: FUNCTION,
        abi: ProcedureAbi::REGISTER_LEAF_NEAR32,
        instruction_cost: WORK,
    }
}

fn enter() -> (Cpu, Bus) {
    let (mut cpu, mut bus) = setup();
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.ip, ENTRY);
    assert_eq!(cpu.registers.get32(Gpr32::Esp), STACK - 4);
    assert_eq!(bus.read_u32(STACK - 4), Ok(5));
    (cpu, bus)
}

fn registry(entries: &[ProcedureEntry]) -> ProcedureRegistry<'_> {
    ProcedureRegistry::new(entries, FrozenCode::AssumeImmutable).unwrap()
}

#[test]
fn actual_guest_call_leaf_ret_matches_interpreter_state_and_every_ram_byte() {
    for cache in [false, true] {
        for (eax, ebx, esi) in [
            (0, 0, 0),
            (u32::MAX, 1, 0),
            (0x7FFF_FFFF, 1, 0x8000_0000),
            (0x1234_5678, 0xABCD_EF01, 0x5555_AAAA),
        ] {
            let (mut reference, mut reference_bus) = setup();
            let (mut bound, mut bound_bus) = setup();
            for cpu in [&mut reference, &mut bound] {
                cpu.set_decode_cache(cache);
                for (code, value) in [eax, 11, 22, ebx, STACK, 55, esi, 77]
                    .into_iter()
                    .enumerate()
                {
                    cpu.registers.set32(Gpr32::decode(code as u8), value);
                }
                cpu.flags |= flags::DIRECTION | flags::CARRY;
            }
            reference.step(&mut reference_bus).unwrap();
            bound.step(&mut bound_bus).unwrap();
            let entries = [entry(&bound)];
            let mut executor = Executor::default();
            for _ in 0..WORK {
                reference.step(&mut reference_bus).unwrap();
            }
            assert_eq!(
                bound.try_call_procedure(&mut bound_bus, &registry(&entries), &mut executor, WORK),
                Ok(ProcedureCallOutcome::Executed { instructions: WORK })
            );
            assert!(bound.matches(&reference.snapshot()));
            assert_eq!(reference_bus.ram, bound_bus.ram);
            assert_eq!(bound.registers.ip, 5);
            assert_eq!(bound.registers.get32(Gpr32::Esp), STACK);
            assert_eq!(executor.calls, 1);
            assert_eq!(bound.execution_counts(), reference.execution_counts());
        }
    }
}

#[test]
fn ordinary_step_keeps_its_one_instruction_contract() {
    let (mut cpu, mut bus) = enter();
    let entries = [entry(&cpu)];
    let _bindings = registry(&entries);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.ip, ENTRY + 2);
    assert_eq!(cpu.registers.get32(Gpr32::Esp), STACK - 4);
}

#[test]
fn actual_call_and_return_wrap_esp_exactly_as_the_interpreter_does() {
    struct WrapBus {
        low: Bus,
        high: [u8; 4],
    }
    const HIGH: u32 = u32::MAX - 3;
    impl MemoryBus for WrapBus {
        fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
            if address >= HIGH {
                Ok(self.high[(address - HIGH) as usize])
            } else {
                self.low.read_u8(address)
            }
        }
        fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
            if address >= HIGH {
                self.high[(address - HIGH) as usize] = value;
                Ok(())
            } else {
                self.low.write_u8(address, value)
            }
        }
        fn is_plain_ram(&self, address: u32, length: u8) -> bool {
            (address == HIGH && length == 4) || self.low.is_plain_ram(address, length)
        }
    }
    impl PortBus for WrapBus {
        fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
            self.low.read_port_u8(port)
        }
        fn write_port_u8(&mut self, port: u16, value: u8) -> Result<(), BusError> {
            self.low.write_port_u8(port, value)
        }
    }
    let (mut reference, low) = setup();
    let mut reference_bus = WrapBus { low, high: [0; 4] };
    let (mut bound, low) = setup();
    let mut bound_bus = WrapBus { low, high: [0; 4] };
    reference.registers.set32(Gpr32::Esp, 0);
    bound.registers.set32(Gpr32::Esp, 0);
    reference.step(&mut reference_bus).unwrap();
    bound.step(&mut bound_bus).unwrap();
    assert_eq!(bound.registers.get32(Gpr32::Esp), HIGH);
    assert_eq!(bound_bus.high, 5_u32.to_le_bytes());
    let entries = [entry(&bound)];
    for _ in 0..WORK {
        reference.step(&mut reference_bus).unwrap();
    }
    assert_eq!(
        bound.try_call_procedure(
            &mut bound_bus,
            &registry(&entries),
            &mut Executor::default(),
            WORK
        ),
        Ok(ProcedureCallOutcome::Executed { instructions: WORK })
    );
    assert!(bound.matches(&reference.snapshot()));
    assert_eq!(bound.registers.get32(Gpr32::Esp), 0);
    assert_eq!(bound_bus.low.ram, reference_bus.low.ram);
    assert_eq!(bound_bus.high, reference_bus.high);
}

#[test]
fn registry_rejects_unsupported_abi_unsorted_duplicate_and_unbounded_work() {
    let (cpu, _) = enter();
    let valid = entry(&cpu);
    for (entries, expected) in [
        (
            [valid, valid],
            ProcedureRegistryError::EntriesNotStrictlySorted,
        ),
        (
            [
                ProcedureEntry {
                    entry_ip: ENTRY + 1,
                    ..valid
                },
                valid,
            ],
            ProcedureRegistryError::EntriesNotStrictlySorted,
        ),
    ] {
        assert_eq!(
            ProcedureRegistry::new(&entries, FrozenCode::AssumeImmutable).unwrap_err(),
            expected
        );
    }
    for abi in [
        ProcedureAbi {
            return_width: 2,
            stack_cleanup: 0,
        },
        ProcedureAbi {
            return_width: 4,
            stack_cleanup: 8,
        },
    ] {
        assert_eq!(
            ProcedureRegistry::new(
                &[ProcedureEntry { abi, ..valid }],
                FrozenCode::AssumeImmutable
            )
            .unwrap_err(),
            ProcedureRegistryError::UnsupportedAbi
        );
    }
    for instruction_cost in [0, MAX_PROCEDURE_INSTRUCTIONS + 1] {
        assert_eq!(
            ProcedureRegistry::new(
                &[ProcedureEntry {
                    instruction_cost,
                    ..valid
                }],
                FrozenCode::AssumeImmutable
            )
            .unwrap_err(),
            ProcedureRegistryError::InvalidInstructionCost
        );
    }
    assert!(registry(&[]).entries().is_empty());
}

#[test]
fn budget_and_runtime_hazards_decline_before_backend_or_cpu_effects() {
    for hazard in 0..21 {
        let (mut cpu, mut bus) = enter();
        let entries = [entry(&cpu)];
        let mut budget = WORK;
        let expected = match hazard {
            0 => {
                budget = WORK - 1;
                ProcedureDecline::Budget
            }
            1 => {
                cpu.registers.ip += 1;
                ProcedureDecline::UnknownEntry
            }
            2 => {
                cpu.flags |= flags::TRAP;
                ProcedureDecline::DebugOrVirtual8086
            }
            3 => {
                cpu.flags |= flags::RESUME;
                ProcedureDecline::DebugOrVirtual8086
            }
            4 => {
                cpu.flags |= flags::VIRTUAL;
                ProcedureDecline::DebugOrVirtual8086
            }
            5 => {
                cpu.control.cr0 |= 0x8000_0000;
                ProcedureDecline::Context
            }
            6 => {
                cpu.control.cr0 &= !1;
                ProcedureDecline::Context
            }
            7 => {
                cpu.registers.caches[SegmentRegister::Cs as usize].default32 = false;
                ProcedureDecline::Context
            }
            8 => {
                cpu.registers.caches[SegmentRegister::Ss as usize].default32 = false;
                ProcedureDecline::Context
            }
            9 => {
                cpu.registers.caches[SegmentRegister::Cs as usize].base = 16;
                ProcedureDecline::Context
            }
            10 => {
                cpu.registers.ss += 8;
                ProcedureDecline::Context
            }
            11 => {
                cpu.set_execution_labels(Some(alloc::vec::Vec::new()))
                    .unwrap();
                ProcedureDecline::ExecutionLabels
            }
            12 => {
                cpu.registers.caches[SegmentRegister::Ss as usize].base = 16;
                ProcedureDecline::Context
            }
            13 => {
                cpu.set_current_privilege_level(1);
                ProcedureDecline::Context
            }
            14 => {
                cpu.control.cr0 ^= 4;
                ProcedureDecline::Context
            }
            15 => {
                bus.ram[ENTRY as usize] = 0xFB;
                cpu.step(&mut bus).unwrap();
                cpu.registers.ip = ENTRY;
                assert!(cpu.has_interrupt_shadow());
                ProcedureDecline::InterruptShadow
            }
            16 => {
                bus.ram[ENTRY as usize] = 0xF4;
                cpu.step(&mut bus).unwrap();
                cpu.registers.ip = ENTRY;
                assert!(cpu.is_halted());
                ProcedureDecline::Halted
            }
            17 => {
                cpu.registers.caches[SegmentRegister::Cs as usize].limit = ENTRY + 1;
                ProcedureDecline::Context
            }
            18 => {
                cpu.registers.caches[SegmentRegister::Cs as usize].access = 0x93;
                ProcedureDecline::Context
            }
            19 => {
                cpu.registers.caches[SegmentRegister::Cs as usize].usable = false;
                ProcedureDecline::Context
            }
            20 => {
                cpu.registers.caches[SegmentRegister::Cs as usize].access &= !0x80;
                ProcedureDecline::Context
            }
            _ => unreachable!(),
        };
        let before = cpu.snapshot();
        let ram = bus.ram;
        let mut executor = Executor::default();
        assert_eq!(
            cpu.try_call_procedure(&mut bus, &registry(&entries), &mut executor, budget),
            Ok(ProcedureCallOutcome::Declined(expected))
        );
        assert!(cpu.matches(&before));
        assert_eq!(bus.ram, ram);
        assert_eq!(executor.calls, 0);
        assert_eq!(cpu.execution_counts(), [0; 4]);
    }
}

#[test]
fn allowed_arithmetic_flags_change_but_all_other_flags_must_be_preserved() {
    struct FlagsExecutor(u32);
    impl ProcedureExecutor for FlagsExecutor {
        type Error = ();
        fn execute(
            &mut self,
            _: u32,
            mut input: ProcedureRegisters,
        ) -> Result<ProcedureResult, ()> {
            input.flags ^= self.0;
            Ok(ProcedureResult {
                registers: input,
                instructions: WORK,
            })
        }
    }
    let arithmetic = flags::CARRY
        | flags::PARITY
        | flags::AUXILIARY
        | flags::ZERO
        | flags::SIGN
        | flags::OVERFLOW;
    for bit in 0..32 {
        let mask = 1 << bit;
        let (mut cpu, mut bus) = enter();
        let entries = [entry(&cpu)];
        let before = cpu.snapshot();
        let result = cpu.try_call_procedure(
            &mut bus,
            &registry(&entries),
            &mut FlagsExecutor(mask),
            WORK,
        );
        if mask & arithmetic != 0 {
            assert_eq!(
                result,
                Ok(ProcedureCallOutcome::Executed { instructions: WORK })
            );
        } else {
            assert_eq!(
                result,
                Err(ProcedureCallError::InvalidResult(
                    ProcedureResultError::ControlFlags
                ))
            );
            assert!(cpu.matches(&before));
        }
    }
}

#[test]
fn stack_and_return_preflight_use_full_segment_permissions_and_ram_contract() {
    for hazard in 0..6 {
        let (mut cpu, mut bus) = enter();
        let expected = match hazard {
            0 => {
                bus.plain = false;
                ProcedureDecline::ReturnStack
            }
            1 => {
                cpu.registers.caches[SegmentRegister::Ss as usize].usable = false;
                ProcedureDecline::ReturnStack
            }
            2 => {
                cpu.registers.caches[SegmentRegister::Ss as usize].access = 0x99;
                ProcedureDecline::ReturnStack
            }
            3 => {
                cpu.registers.caches[SegmentRegister::Ss as usize].limit = STACK - 2;
                ProcedureDecline::ReturnStack
            }
            4 => {
                cpu.registers.set32(Gpr32::Esp, u32::MAX - 1);
                ProcedureDecline::ReturnStack
            }
            5 => {
                cpu.registers.caches[SegmentRegister::Ss as usize].access = 0x97;
                cpu.registers.caches[SegmentRegister::Ss as usize].limit = STACK;
                ProcedureDecline::ReturnStack
            }
            _ => unreachable!(),
        };
        // Bind this exact context, so SS permission failures cannot be hidden
        // behind a mere descriptor-mismatch decline.
        let entries = [entry(&cpu)];
        let before = cpu.snapshot();
        let ram = bus.ram;
        let mut executor = Executor::default();
        assert_eq!(
            cpu.try_call_procedure(&mut bus, &registry(&entries), &mut executor, WORK),
            Ok(ProcedureCallOutcome::Declined(expected))
        );
        assert!(cpu.matches(&before));
        assert_eq!(ram, bus.ram);
        assert_eq!(executor.calls, 0);
    }
}

#[test]
fn invalid_results_and_backend_errors_never_commit_cpu_or_ram() {
    for bad in [
        None,
        Some(ProcedureResultError::InstructionCount),
        Some(ProcedureResultError::StackPointer),
        Some(ProcedureResultError::ControlFlags),
    ] {
        let (mut cpu, mut bus) = enter();
        let entries = [entry(&cpu)];
        let before = cpu.snapshot();
        let ram = bus.ram;
        let mut executor = Executor {
            bad,
            fail: bad.is_none(),
            ..Executor::default()
        };
        let expected = bad.map_or(
            ProcedureCallError::Backend("backend failure"),
            ProcedureCallError::InvalidResult,
        );
        assert_eq!(
            cpu.try_call_procedure(&mut bus, &registry(&entries), &mut executor, WORK),
            Err(expected)
        );
        assert!(cpu.matches(&before));
        assert_eq!(ram, bus.ram);
        assert_eq!(executor.calls, 1);
    }
}

#[test]
fn bus_contract_violation_is_explicit_and_does_not_commit_cpu() {
    let (mut cpu, mut bus) = enter();
    let entries = [entry(&cpu)];
    let before = cpu.snapshot();
    let mut executor = Executor::default();
    bus.fail_read = true;
    assert_eq!(
        cpu.try_call_procedure(&mut bus, &registry(&entries), &mut executor, WORK),
        Err(ProcedureCallError::Bus(BusError::UnmappedMemory(STACK - 4)))
    );
    assert!(cpu.matches(&before));
    assert_eq!(executor.calls, 0);
}

#[test]
fn no_allocation_after_setup_for_success_and_decline() {
    let (mut cpu, mut bus) = enter();
    let entries = [entry(&cpu)];
    let bindings = registry(&entries);
    let mut executor = Executor::default();
    let allocations = allocation_counter::measure(|| {
        for _ in 0..1000 {
            cpu.registers.ip = ENTRY;
            cpu.registers.set32(Gpr32::Esp, STACK - 4);
            assert_eq!(
                cpu.try_call_procedure(&mut bus, &bindings, &mut executor, WORK - 1),
                Ok(ProcedureCallOutcome::Declined(ProcedureDecline::Budget))
            );
            assert_eq!(
                cpu.try_call_procedure(&mut bus, &bindings, &mut executor, WORK),
                Ok(ProcedureCallOutcome::Executed { instructions: WORK })
            );
        }
    });
    assert_eq!(allocations.count_total, 0);
    assert_eq!(allocations.bytes_total, 0);
}
