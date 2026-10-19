use super::*;
use crate::{SegmentCache, state::SegmentWindow};
use doslet_bus::{BusError, PortBus};

const ENTRY: u32 = 0x20;
const STACK: u32 = 0x100;
const DATA: u32 = 0x180;
const TARGET: u32 = 0x80;
const FUNCTION: u32 = 73;
const WORK: u32 = 7;
const BRANCH_WORK: u32 = 6;
const BODY: &[u8] = &[
    0x53, // PUSH EBX (writes RAM even though POP later balances ESP).
    0x89, 0xC3, // MOV EBX,EAX.
    0x5A, // POP EDX.
    0xC7, 0x04, 0x24, 0x80, 0, 0, 0, // MOV [ESP],80h (replace actual return slot).
    0x01, 0x07, // ADD [EDI],EAX.
    0x8B, 0x37, // MOV ESI,[EDI].
    0xC3, // RET uses the modified slot, not the entering CALL's target.
];
const BRANCH_BODY: &[u8] = &[
    0x85, 0xC0, // TEST EAX,EAX.
    0x74, 0x02, // JZ forward to PUSH EAX.
    0x01, 0xD8, // ADD EAX,EBX.
    0x50, // PUSH EAX.
    0x5A, // POP EDX.
    0xC3, // RET.
];

#[test]
fn complete_tier_proof_charges_exact_counts_only_after_a_validated_result() {
    let counts = [2, 1, 3, 1];
    let (mut cpu, mut bus) = enter(Program::ReturnSlot);
    cpu.set_execution_labels(Some(alloc::vec::Vec::new()))
        .unwrap();
    let entries = [entry(&cpu)];
    assert_eq!(
        cpu.try_call_memory_procedure_with_tier_counts(&mut bus, &registry(&entries), WORK, counts),
        Ok(MemoryProcedureOutcome::Complete { instructions: WORK })
    );
    assert_eq!(cpu.execution_counts(), counts);
    assert_eq!(cpu.registers.ip, TARGET);

    for fault in [Fault::Count(WORK - 1, true), Fault::Count(1, false)] {
        let (mut cpu, mut bus) = enter(Program::ReturnSlot);
        cpu.set_execution_labels(Some(alloc::vec::Vec::new()))
            .unwrap();
        let entries = [entry(&cpu)];
        let before = cpu.snapshot();
        bus.fault = Some(fault);
        assert_eq!(
            cpu.try_call_memory_procedure_with_tier_counts(
                &mut bus,
                &registry(&entries),
                WORK,
                counts
            ),
            Err(MemoryProcedureError::InvalidResult(
                MemoryProcedureResultError::InstructionCount
            ))
        );
        assert!(cpu.matches(&before));
        assert_eq!(cpu.execution_counts(), [0; 4]);
        assert_eq!(
            bus.load(DATA),
            0xDEAD_BEEF,
            "committed store must remain fatal"
        );
    }
}

#[derive(Clone, Copy)]
enum Program {
    ReturnSlot,
    Branch,
}

#[derive(Clone, Copy)]
enum Fault {
    Backend,
    Count(u32, bool),
    Flag(u32),
}

struct Bus {
    ram: [u8; 512],
    program: Program,
    calls: u32,
    reads: u32,
    writes: u32,
    stores: u32,
    stop_after: u32,
    fault: Option<Fault>,
}

impl MemoryBus for Bus {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        self.reads += 1;
        self.ram
            .get(address as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(address))
    }

    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        self.writes += 1;
        *self
            .ram
            .get_mut(address as usize)
            .ok_or(BusError::UnmappedMemory(address))? = value;
        Ok(())
    }

    fn is_plain_ram(&self, address: u32, length: u8) -> bool {
        address
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

// This test backend implements the two fixed guest fixtures directly, without
// executing a second CPU or using Cpu::step. It builds windows from each live
// CPU borrow and owns the same RAM observed by the ordinary interpreter bus.
impl Bus {
    fn direct_address(&self, window: Option<SegmentWindow>, offset: u32) -> Option<u32> {
        let address = window?.address(offset, 4)?;
        self.is_plain_ram(address, 4).then_some(address)
    }

    fn load(&self, address: u32) -> u32 {
        u32::from_le_bytes(
            self.ram[address as usize..address as usize + 4]
                .try_into()
                .unwrap(),
        )
    }

    fn store(&mut self, address: u32, value: u32) {
        self.ram[address as usize..address as usize + 4].copy_from_slice(&value.to_le_bytes());
        self.stores += 1;
    }
}

impl MemoryProcedureBus for Bus {
    type Error = &'static str;

    fn execute_memory_procedure(
        &mut self,
        function_id: u32,
        cpu: &Cpu,
    ) -> Result<MemoryProcedureResult, Self::Error> {
        assert_eq!(function_id, FUNCTION);
        self.calls += 1;
        let mut result = MemoryProcedureResult {
            registers: ProcedureRegisters {
                gpr: core::array::from_fn(|code| cpu.registers.get32(Gpr32::decode(code as u8))),
                flags: cpu.flags,
            },
            instruction_pointer: cpu.registers.ip,
            instructions: 0,
            completed: false,
        };
        if let Some(fault) = self.fault {
            // Fatal results deliberately follow a committed guest store. The
            // CPU hook cannot roll this back or safely interpret the entry.
            self.store(DATA, 0xDEAD_BEEF);
            result.registers.gpr[0] ^= u32::MAX;
            result.instruction_pointer = TARGET;
            result.instructions = WORK;
            result.completed = true;
            match fault {
                Fault::Backend => return Err("committed backend failure"),
                Fault::Count(count, completed) => {
                    result.instructions = count;
                    result.completed = completed;
                }
                Fault::Flag(mask) => result.registers.flags ^= mask,
            }
            return Ok(result);
        }
        let stack_read = cpu.registers.window(SegmentRegister::Ss, Access::Read);
        let stack_write = cpu.registers.window(SegmentRegister::Ss, Access::Write);
        let data_read = cpu.registers.window(SegmentRegister::Ds, Access::Read);
        let data_write = cpu.registers.window(SegmentRegister::Ds, Access::Write);
        let mut offset = 0;
        while result.instructions < self.stop_after {
            let gpr = &mut result.registers.gpr;
            let esp = gpr[Gpr32::Esp as usize];
            let eax = gpr[Gpr32::Eax as usize];
            let ebx = gpr[Gpr32::Ebx as usize];
            let edi = gpr[Gpr32::Edi as usize];
            match (self.program, offset) {
                (Program::ReturnSlot, 0) | (Program::Branch, 6) => {
                    let next = esp.wrapping_sub(4);
                    let Some(address) = self.direct_address(stack_write, next) else {
                        break;
                    };
                    self.store(
                        address,
                        if matches!(self.program, Program::ReturnSlot) {
                            ebx
                        } else {
                            eax
                        },
                    );
                    gpr[Gpr32::Esp as usize] = next;
                    offset += 1;
                }
                (Program::ReturnSlot, 1) => {
                    gpr[Gpr32::Ebx as usize] = eax;
                    offset = 3;
                }
                (Program::ReturnSlot, 3) | (Program::Branch, 7) => {
                    let Some(address) = self.direct_address(stack_read, esp) else {
                        break;
                    };
                    gpr[Gpr32::Edx as usize] = self.load(address);
                    gpr[Gpr32::Esp as usize] = esp.wrapping_add(4);
                    offset += 1;
                }
                (Program::ReturnSlot, 4) => {
                    let Some(address) = self.direct_address(stack_write, esp) else {
                        break;
                    };
                    self.store(address, TARGET);
                    offset = 11;
                }
                (Program::ReturnSlot, 11) => {
                    // Validate both access kinds before the instruction's
                    // first memory effect, just as the backend contract states.
                    let Some(address) = self.direct_address(data_read, edi) else {
                        break;
                    };
                    if self.direct_address(data_write, edi) != Some(address) {
                        break;
                    }
                    let (value, flags) =
                        flags::alu32(result.registers.flags, self.load(address), eax, 0);
                    self.store(address, value);
                    result.registers.flags = flags;
                    offset = 13;
                }
                (Program::ReturnSlot, 13) => {
                    let Some(address) = self.direct_address(data_read, edi) else {
                        break;
                    };
                    gpr[Gpr32::Esi as usize] = self.load(address);
                    offset = 15;
                }
                (Program::Branch, 0) => {
                    (_, result.registers.flags) = flags::alu32(result.registers.flags, eax, eax, 4);
                    offset = 2;
                }
                (Program::Branch, 2) => {
                    offset = if result.registers.flags & flags::ZERO != 0 {
                        6
                    } else {
                        4
                    };
                }
                (Program::Branch, 4) => {
                    (gpr[Gpr32::Eax as usize], result.registers.flags) =
                        flags::alu32(result.registers.flags, eax, ebx, 0);
                    offset = 6;
                }
                (Program::ReturnSlot, 15) | (Program::Branch, 8) => {
                    let Some(address) = self.direct_address(stack_read, esp) else {
                        break;
                    };
                    let target = self.load(address);
                    if cpu
                        .registers
                        .address(SegmentRegister::Cs, target, 1, Access::Execute)
                        .is_err()
                    {
                        break;
                    }
                    gpr[Gpr32::Esp as usize] = esp.wrapping_add(4);
                    result.instruction_pointer = target;
                    result.instructions += 1;
                    result.completed = true;
                    return Ok(result);
                }
                _ => unreachable!(),
            }
            result.instructions += 1;
            result.instruction_pointer = ENTRY + offset;
        }
        Ok(result)
    }
}

fn setup(program: Program) -> (Cpu, Bus) {
    let mut cpu = Cpu::default().with_x87();
    cpu.set_protected_mode(true);
    for segment in SegmentRegister::ALL {
        let code = segment == SegmentRegister::Cs;
        cpu.registers.set_segment(
            segment,
            SegmentCache {
                selector: if code { 8 } else { 16 },
                base: 0,
                limit: u32::MAX,
                access: if code { 0x9B } else { 0x93 },
                flags: 0xC0,
                default32: true,
                usable: true,
            },
        );
    }
    for (code, value) in [7, 11, 22, 0x1234_5678, STACK, 55, 66, DATA]
        .into_iter()
        .enumerate()
    {
        cpu.registers.set32(Gpr32::decode(code as u8), value);
    }
    cpu.flags |= flags::DIRECTION | flags::CARRY;
    let mut bus = Bus {
        ram: [0; 512],
        program,
        calls: 0,
        reads: 0,
        writes: 0,
        stores: 0,
        stop_after: u32::MAX,
        fault: None,
    };
    bus.ram[..5].copy_from_slice(&[0xE8, 0x1B, 0, 0, 0]);
    let body = match program {
        Program::ReturnSlot => BODY,
        Program::Branch => BRANCH_BODY,
    };
    bus.ram[ENTRY as usize..ENTRY as usize + body.len()].copy_from_slice(body);
    bus.ram[DATA as usize..DATA as usize + 4].copy_from_slice(&0x7FFF_FFFFu32.to_le_bytes());
    (cpu, bus)
}

fn enter(program: Program) -> (Cpu, Bus) {
    let (mut cpu, mut bus) = setup(program);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.ip, ENTRY);
    assert_eq!(cpu.registers.get32(Gpr32::Esp), STACK - 4);
    assert_eq!(bus.load(STACK - 4), 5);
    bus.reads = 0;
    bus.writes = 0;
    (cpu, bus)
}

fn entry(cpu: &Cpu) -> MemoryProcedureEntry {
    MemoryProcedureEntry {
        entry_ip: ENTRY,
        context: ProcedureContext::capture(cpu).unwrap(),
        function_id: FUNCTION,
        maximum_instructions: WORK,
    }
}

fn registry(entries: &[MemoryProcedureEntry]) -> MemoryProcedureRegistry<'_> {
    MemoryProcedureRegistry::new(entries, FrozenCode::AssumeImmutable).unwrap()
}

#[test]
fn guest_call_balanced_stack_memory_write_modified_return_and_true_ret_match_every_byte() {
    for cache in [false, true] {
        for eax in [0, 1, u32::MAX, 0x8000_0001, 0x1234_5678] {
            let (mut reference, mut reference_bus) = setup(Program::ReturnSlot);
            let (mut bound, mut bound_bus) = setup(Program::ReturnSlot);
            for cpu in [&mut reference, &mut bound] {
                cpu.set_decode_cache(cache);
                cpu.registers.set32(Gpr32::Eax, eax);
            }
            reference.step(&mut reference_bus).unwrap();
            bound.step(&mut bound_bus).unwrap();
            let entries = [entry(&bound)];
            for _ in 0..WORK {
                reference.step(&mut reference_bus).unwrap();
            }
            bound_bus.reads = 0;
            bound_bus.writes = 0;
            assert_eq!(
                bound.try_call_memory_procedure(&mut bound_bus, &registry(&entries), WORK),
                Ok(MemoryProcedureOutcome::Complete { instructions: WORK })
            );
            assert!(bound.matches(&reference.snapshot()));
            assert_eq!(reference_bus.ram, bound_bus.ram);
            assert_eq!(bound.registers.ip, TARGET);
            assert_eq!(bound.registers.get32(Gpr32::Esp), STACK);
            assert_eq!(bound_bus.load(STACK - 4), TARGET);
            assert_eq!(bound_bus.load(STACK - 8), 0x1234_5678);
            assert_eq!(bound_bus.calls, 1);
            assert_eq!(bound_bus.reads, 0, "hook must not pre-read the RET slot");
            assert_eq!(bound_bus.writes, 0);
            assert_eq!(bound_bus.stores, 3);
            assert_eq!(bound.execution_counts(), reference.execution_counts());
        }
    }
}

#[test]
fn each_prefix_side_exit_commits_exact_state_then_interpreter_continues_without_replay() {
    for prefix in 0..WORK {
        for cache in [false, true] {
            let (mut reference, mut reference_bus) = enter(Program::ReturnSlot);
            let (mut bound, mut bound_bus) = enter(Program::ReturnSlot);
            reference.set_decode_cache(cache);
            bound.set_decode_cache(cache);
            bound_bus.stop_after = prefix;
            let entries = [entry(&bound)];
            for _ in 0..prefix {
                reference.step(&mut reference_bus).unwrap();
            }
            assert_eq!(
                bound.try_call_memory_procedure(&mut bound_bus, &registry(&entries), WORK),
                Ok(MemoryProcedureOutcome::SideExit {
                    instructions: prefix
                })
            );
            assert!(bound.matches(&reference.snapshot()));
            assert_eq!(bound_bus.ram, reference_bus.ram);
            for _ in prefix..WORK {
                reference.step(&mut reference_bus).unwrap();
                bound.step(&mut bound_bus).unwrap();
                assert!(bound.matches(&reference.snapshot()));
                assert_eq!(bound_bus.ram, reference_bus.ram);
            }
            assert_eq!(bound.registers.ip, TARGET);
            assert_eq!(bound_bus.load(DATA), 0x8000_0006);
            assert_eq!(bound_bus.calls, 1);
        }
    }
}

#[test]
fn acyclic_branch_reports_variable_work_but_requires_the_full_maximum_grant() {
    for eax in [0, 1, u32::MAX, 0x7FFF_FFFF, 0x8000_0000] {
        let (mut reference, mut reference_bus) = setup(Program::Branch);
        let (mut bound, mut bound_bus) = setup(Program::Branch);
        for cpu in [&mut reference, &mut bound] {
            cpu.registers.set32(Gpr32::Eax, eax);
        }
        reference.step(&mut reference_bus).unwrap();
        bound.step(&mut bound_bus).unwrap();
        let entries = [MemoryProcedureEntry {
            maximum_instructions: BRANCH_WORK,
            ..entry(&bound)
        }];
        let bindings = registry(&entries);
        let before = bound.snapshot();
        let ram = bound_bus.ram;
        assert_eq!(
            bound.try_call_memory_procedure(&mut bound_bus, &bindings, BRANCH_WORK - 1),
            Ok(MemoryProcedureOutcome::Declined(ProcedureDecline::Budget))
        );
        assert!(bound.matches(&before));
        assert_eq!(bound_bus.ram, ram);
        assert_eq!(bound_bus.calls, 0);
        let work = if eax == 0 { 5 } else { 6 };
        for _ in 0..work {
            reference.step(&mut reference_bus).unwrap();
        }
        assert_eq!(
            bound.try_call_memory_procedure(&mut bound_bus, &bindings, BRANCH_WORK),
            Ok(MemoryProcedureOutcome::Complete { instructions: work })
        );
        assert!(bound.matches(&reference.snapshot()));
        assert_eq!(bound_bus.ram, reference_bus.ram);
        assert_eq!(bound.registers.ip, 5);
    }
}

#[test]
fn live_data_segment_changes_exit_before_the_failing_instruction_and_keep_stack_prefix() {
    let (mut reference, mut reference_bus) = enter(Program::ReturnSlot);
    let (mut bound, mut bound_bus) = enter(Program::ReturnSlot);
    let entries = [entry(&bound)];
    // DS is deliberately absent from the basic CS/SS binding. The backend must
    // build its own live DS window every time, never reuse a setup-time proof.
    for cpu in [&mut reference, &mut bound] {
        cpu.registers.caches[SegmentRegister::Ds as usize].usable = false;
    }
    for _ in 0..4 {
        reference.step(&mut reference_bus).unwrap();
    }
    assert_eq!(
        bound.try_call_memory_procedure(&mut bound_bus, &registry(&entries), WORK),
        Ok(MemoryProcedureOutcome::SideExit { instructions: 4 })
    );
    assert!(bound.matches(&reference.snapshot()));
    assert_eq!(bound_bus.ram, reference_bus.ram);
    assert_eq!(bound.registers.ip, ENTRY + 11);
    assert_eq!(bound_bus.load(DATA), 0x7FFF_FFFF);
    assert_eq!(bound_bus.load(STACK - 4), TARGET);
    assert!(bound.step(&mut bound_bus).is_err());
    assert!(reference.step(&mut reference_bus).is_err());
    assert!(bound.matches(&reference.snapshot()));
    assert_eq!(bound_bus.ram, reference_bus.ram);
}

#[test]
fn ordinary_step_still_executes_one_instruction() {
    let (mut cpu, mut bus) = enter(Program::ReturnSlot);
    let entries = [entry(&cpu)];
    let _bindings = registry(&entries);
    cpu.step(&mut bus).unwrap();
    assert_eq!(cpu.registers.ip, ENTRY + 1);
    assert_eq!(cpu.registers.get32(Gpr32::Esp), STACK - 8);
    assert_eq!(bus.load(STACK - 8), 0x1234_5678);
    assert_eq!(bus.calls, 0);
}

#[test]
fn registry_rejects_duplicate_unsorted_excessive_entries_and_zero_work() {
    let (cpu, _) = enter(Program::ReturnSlot);
    let valid = entry(&cpu);
    for entries in [
        [valid, valid],
        [
            MemoryProcedureEntry {
                entry_ip: ENTRY + 1,
                ..valid
            },
            valid,
        ],
    ] {
        assert_eq!(
            MemoryProcedureRegistry::new(&entries, FrozenCode::AssumeImmutable).unwrap_err(),
            MemoryProcedureRegistryError::EntriesNotStrictlySorted
        );
    }
    assert_eq!(
        MemoryProcedureRegistry::new(
            &[MemoryProcedureEntry {
                maximum_instructions: 0,
                ..valid
            }],
            FrozenCode::AssumeImmutable,
        )
        .unwrap_err(),
        MemoryProcedureRegistryError::InvalidMaximumInstructions
    );
    // A trusted whole-computation proof may exceed an interpreter batch. The
    // caller, rather than this registry, establishes its finite live bound.
    for maximum_instructions in [4097, u32::MAX] {
        let entries = [MemoryProcedureEntry {
            maximum_instructions,
            ..valid
        }];
        assert!(MemoryProcedureRegistry::new(&entries, FrozenCode::AssumeImmutable).is_ok());
    }
    let entries: alloc::vec::Vec<_> = (0..=MAX_PROCEDURE_ENTRIES)
        .map(|index| MemoryProcedureEntry {
            entry_ip: index as u32,
            ..valid
        })
        .collect();
    assert_eq!(
        MemoryProcedureRegistry::new(&entries, FrozenCode::AssumeImmutable).unwrap_err(),
        MemoryProcedureRegistryError::TooManyEntries
    );
    assert_eq!(
        registry(&entries[..MAX_PROCEDURE_ENTRIES]).entries().len(),
        MAX_PROCEDURE_ENTRIES
    );
    assert!(registry(&[]).entries().is_empty());
    assert_eq!(registry(&[valid]).entries(), &[valid]);
}

#[test]
fn transient_hazards_and_context_changes_decline_before_any_bus_or_backend_effects() {
    for hazard in 0..23 {
        let (mut cpu, mut bus) = enter(Program::ReturnSlot);
        let entries = [entry(&cpu)];
        let mut budget = WORK;
        let expected = match hazard {
            0 => {
                budget -= 1;
                ProcedureDecline::Budget
            }
            1 => {
                cpu.registers.ip += 1;
                ProcedureDecline::UnknownEntry
            }
            2..=4 => {
                cpu.flags |= [flags::TRAP, flags::RESUME, flags::VIRTUAL][hazard - 2];
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
            21 => {
                cpu.registers.cs += 8;
                ProcedureDecline::Context
            }
            22 => {
                cpu.registers.protected = false;
                ProcedureDecline::Context
            }
            _ => unreachable!(),
        };
        let before = cpu.snapshot();
        let ram = bus.ram;
        bus.reads = 0;
        bus.writes = 0;
        assert_eq!(
            cpu.try_call_memory_procedure(&mut bus, &registry(&entries), budget),
            Ok(MemoryProcedureOutcome::Declined(expected))
        );
        assert!(cpu.matches(&before));
        assert_eq!(bus.ram, ram);
        assert_eq!((bus.calls, bus.reads, bus.writes, bus.stores), (0, 0, 0, 0));
    }
}

#[test]
fn fatal_backend_and_malformed_counts_leave_cpu_uncommitted_but_preserve_ram_effects() {
    for (fault, expected) in [
        (
            Fault::Backend,
            MemoryProcedureError::Backend("committed backend failure"),
        ),
        (
            Fault::Count(0, true),
            MemoryProcedureError::InvalidResult(MemoryProcedureResultError::InstructionCount),
        ),
        (
            Fault::Count(WORK + 1, true),
            MemoryProcedureError::InvalidResult(MemoryProcedureResultError::InstructionCount),
        ),
        (
            Fault::Count(WORK + 1, false),
            MemoryProcedureError::InvalidResult(MemoryProcedureResultError::InstructionCount),
        ),
        (
            Fault::Count(u32::MAX, false),
            MemoryProcedureError::InvalidResult(MemoryProcedureResultError::InstructionCount),
        ),
    ] {
        let (mut cpu, mut bus) = enter(Program::ReturnSlot);
        let entries = [entry(&cpu)];
        let before = cpu.snapshot();
        bus.fault = Some(fault);
        assert_eq!(
            cpu.try_call_memory_procedure(&mut bus, &registry(&entries), WORK),
            Err(expected)
        );
        assert!(cpu.matches(&before));
        assert_eq!(bus.load(DATA), 0xDEAD_BEEF);
        assert_eq!(bus.calls, 1);
        assert_eq!(bus.stores, 1);
    }
}

#[test]
fn only_arithmetic_flag_changes_are_accepted_and_other_result_errors_are_fatal() {
    let arithmetic = flags::CARRY
        | flags::PARITY
        | flags::AUXILIARY
        | flags::ZERO
        | flags::SIGN
        | flags::OVERFLOW;
    for bit in 0..32 {
        let mask = 1 << bit;
        let (mut cpu, mut bus) = enter(Program::ReturnSlot);
        let entries = [entry(&cpu)];
        let before = cpu.snapshot();
        let original_flags = cpu.flags;
        bus.fault = Some(Fault::Flag(mask));
        let outcome = cpu.try_call_memory_procedure(&mut bus, &registry(&entries), WORK);
        if mask & arithmetic != 0 {
            assert_eq!(
                outcome,
                Ok(MemoryProcedureOutcome::Complete { instructions: WORK })
            );
            assert_eq!(cpu.flags, original_flags ^ mask);
            assert_eq!(cpu.registers.ip, TARGET);
            assert_eq!(cpu.registers.get32(Gpr32::Eax), 7 ^ u32::MAX);
        } else {
            assert_eq!(
                outcome,
                Err(MemoryProcedureError::InvalidResult(
                    MemoryProcedureResultError::ControlFlags
                ))
            );
            assert!(cpu.matches(&before));
        }
        assert_eq!(bus.load(DATA), 0xDEAD_BEEF);
    }
}

#[test]
fn maximum_work_side_exit_is_accepted_and_commits_the_returned_cpu_state() {
    let (mut cpu, mut bus) = enter(Program::ReturnSlot);
    let entries = [entry(&cpu)];
    bus.fault = Some(Fault::Count(WORK, false));
    assert_eq!(
        cpu.try_call_memory_procedure(&mut bus, &registry(&entries), WORK),
        Ok(MemoryProcedureOutcome::SideExit { instructions: WORK })
    );
    assert_eq!(cpu.registers.ip, TARGET);
    assert_eq!(cpu.registers.get32(Gpr32::Esp), STACK - 4);
    assert_eq!(bus.load(DATA), 0xDEAD_BEEF);
}

#[test]
fn no_allocation_after_setup_for_completion_prefix_decline_and_fatal_result() {
    let (mut cpu, mut bus) = enter(Program::ReturnSlot);
    let entries = [entry(&cpu)];
    let bindings = registry(&entries);
    let allocations = allocation_counter::measure(|| {
        for _ in 0..1000 {
            for prefix in [0, 4, WORK] {
                cpu.registers.ip = ENTRY;
                cpu.registers.set32(Gpr32::Esp, STACK - 4);
                cpu.registers.set32(Gpr32::Edi, DATA);
                bus.stop_after = prefix;
                bus.fault = None;
                assert_eq!(
                    cpu.try_call_memory_procedure(&mut bus, &bindings, WORK - 1),
                    Ok(MemoryProcedureOutcome::Declined(ProcedureDecline::Budget))
                );
                let expected = if prefix == WORK {
                    MemoryProcedureOutcome::Complete { instructions: WORK }
                } else {
                    MemoryProcedureOutcome::SideExit {
                        instructions: prefix,
                    }
                };
                assert_eq!(
                    cpu.try_call_memory_procedure(&mut bus, &bindings, WORK),
                    Ok(expected)
                );
            }
            cpu.registers.ip = ENTRY;
            bus.fault = Some(Fault::Backend);
            assert_eq!(
                cpu.try_call_memory_procedure(&mut bus, &bindings, WORK),
                Err(MemoryProcedureError::Backend("committed backend failure"))
            );
        }
    });
    assert_eq!(allocations.count_total, 0);
    assert_eq!(allocations.bytes_total, 0);
}
