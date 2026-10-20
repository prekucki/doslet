//! Generic CPU dispatch with an exclusively borrowed Wasmtime memory owner.
use super::*;
use doslet_x86::procedure_calls::memory::{
    MemoryProcedureBus, MemoryProcedureEntry, MemoryProcedureOutcome, MemoryProcedureRegistry,
    MemoryProcedureResult,
};
use doslet_x86::procedure_calls::{FrozenCode, ProcedureContext, ProcedureRegisters};

const CALLER: u32 = RETURN - 5;

struct EngineBus<'a> {
    runner: &'a mut MemoryProcedureRunner,
    host_writes: u64,
    windows: [[SegmentBounds; 6]; 2],
}
impl EngineBus<'_> {
    fn read<T>(
        &mut self,
        read: impl FnOnce(&mut Bus<'_>) -> Result<T, BusError>,
    ) -> Result<T, BusError> {
        read(&mut Bus {
            bytes: self.runner.memory_mut(),
            writes: 0,
        })
    }
    fn write<T>(
        &mut self,
        write: impl FnOnce(&mut Bus<'_>) -> Result<T, BusError>,
    ) -> Result<T, BusError> {
        let mut bus = Bus {
            bytes: self.runner.memory_mut(),
            writes: 0,
        };
        let result = write(&mut bus);
        self.host_writes += bus.writes;
        result
    }
    fn generation(&self, page: u32) -> Option<u64> {
        if page >= 16 {
            return None;
        }
        let start = GENERATIONS + page as usize * 8;
        Some(u64::from_le_bytes(
            self.runner.memory()[start..start + 8]
                .try_into()
                .expect("page"),
        ))
    }
}
impl MemoryBus for EngineBus<'_> {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        self.read(|bus| bus.read_u8(address))
    }
    fn read_u16(&mut self, address: u32) -> Result<u16, BusError> {
        self.read(|bus| bus.read_u16(address))
    }
    fn read_u32(&mut self, address: u32) -> Result<u32, BusError> {
        self.read(|bus| bus.read_u32(address))
    }
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        self.write(|bus| bus.write_u8(address, value))
    }
    fn write_u16(&mut self, address: u32, value: u16) -> Result<(), BusError> {
        self.write(|bus| bus.write_u16(address, value))
    }
    fn write_u32(&mut self, address: u32, value: u32) -> Result<(), BusError> {
        self.write(|bus| bus.write_u32(address, value))
    }
    fn is_plain_ram(&self, address: u32, length: u8) -> bool {
        u64::from(address) + u64::from(length) <= RAM as u64
    }
    fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
        if !self.is_plain_ram(address, length) {
            return None;
        }
        let start = RAM_POINTER + address as usize;
        self.runner.memory().get(start..start + length as usize)
    }
    fn code_check(&self, address: u32, length: u8) -> Option<CodeCheck> {
        if length == 0 || !self.is_plain_ram(address, length) {
            return None;
        }
        let pages = [address / 4096, (address + u32::from(length) - 1) / 4096];
        Some(CodeCheck {
            space: 1,
            pages,
            generations: [self.generation(pages[0])?, self.generation(pages[1])?],
        })
    }
    fn code_valid(&self, check: &CodeCheck) -> bool {
        check.space == 1
            && check
                .pages
                .into_iter()
                .zip(check.generations)
                .all(|(page, generation)| self.generation(page) == Some(generation))
    }
}
impl PortBus for EngineBus<'_> {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
        Err(BusError::UnmappedPort(port))
    }
    fn write_port_u8(&mut self, port: u16, _: u8) -> Result<(), BusError> {
        Err(BusError::UnmappedPort(port))
    }
}
impl MemoryProcedureBus for EngineBus<'_> {
    type Error = String;
    fn execute_memory_procedure(
        &mut self,
        function_id: u32,
        cpu: &Cpu,
    ) -> Result<MemoryProcedureResult, String> {
        // This fixture freezes mappings. A general owner refreshes descriptors
        // when any live window changes; this one explicitly rejects that case.
        if function_id != 0 || windows(cpu) != self.windows {
            return Err("fixture function or live memory context changed".into());
        }
        frame(cpu, 0)
            .write(self.runner.memory_mut())
            .map_err(|e| e.to_string())?;
        let exit = self.runner.call(0)?;
        let result = Frame::read(self.runner.memory()).map_err(|e| e.to_string())?;
        Ok(MemoryProcedureResult {
            registers: ProcedureRegisters {
                gpr: result.gpr,
                flags: result.eflags,
            },
            instruction_pointer: result.address.offset,
            instructions: u32::try_from(result.charged_work).map_err(|e| e.to_string())?,
            completed: exit == Exit::Complete,
        })
    }
}

pub(super) fn measure(
    runner: &mut MemoryProcedureRunner,
    fixture: &Fixture,
    native: bool,
    iterations: usize,
    seed: u32,
) -> Result<Measurement, String> {
    let mut cpu = initialize(runner, fixture, seed)?;
    let context = ProcedureContext::capture(&cpu).ok_or("fixture context unsupported")?;
    let maximum_instructions = fixture.maximum_work;
    let entries = [MemoryProcedureEntry {
        entry_ip: START,
        context,
        function_id: 0,
        maximum_instructions,
    }];
    let registry = MemoryProcedureRegistry::new(&entries, FrozenCode::AssumeImmutable)
        .map_err(|e| format!("memory registry: {e:?}"))?;
    let bytes = runner.memory_mut();
    bytes[RAM_POINTER + CALLER as usize] = 0xe8;
    bytes[RAM_POINTER + CALLER as usize + 1..RAM_POINTER + RETURN as usize]
        .copy_from_slice(&START.wrapping_sub(RETURN).to_le_bytes());
    bytes[CODE_PAGES + CALLER as usize / 4096] = 1;
    let mut bus = EngineBus {
        runner,
        host_writes: 0,
        windows: windows(&cpu),
    };
    let warmup_entries = if iterations >= 1024 { 32 } else { 0 };
    let mut start = Instant::now();
    let mut instructions = 0_u64;
    if native {
        // Borrow the whole backing once, as an ordinary native bus would.
        // Looking up Wasmtime memory on every native load would artificially
        // slow the reference and inflate the measured AOT gain.
        let mut direct = Bus {
            bytes: bus.runner.memory_mut(),
            writes: 0,
        };
        for invocation in 0..warmup_entries + iterations {
            if invocation == warmup_entries {
                start = Instant::now();
            }
            cpu.registers.ip = CALLER;
            cpu.registers.set32(Gpr32::Esp, STACK);
            let mut count = 0;
            while cpu.registers.ip != RETURN && count < 4096 {
                cpu.step(&mut direct).map_err(|e| e.to_string())?;
                count += 1;
            }
            if cpu.registers.ip != RETURN {
                return Err("native failed to return".into());
            }
            instructions += count;
        }
        bus.host_writes = direct.writes;
    } else {
        for invocation in 0..warmup_entries + iterations {
            if invocation == warmup_entries {
                start = Instant::now();
            }
            cpu.registers.ip = CALLER;
            cpu.registers.set32(Gpr32::Esp, STACK);
            cpu.step(&mut bus).map_err(|e| e.to_string())?;
            instructions += 1; // Entering CALL is outside the procedure count.
            match cpu
                .try_call_memory_procedure(&mut bus, &registry, maximum_instructions)
                .map_err(|e| format!("integrated memory dispatch: {e:?}"))?
            {
                MemoryProcedureOutcome::Complete { instructions: body } => {
                    instructions += u64::from(body)
                }
                other => return Err(format!("unexpected integrated outcome: {other:?}")),
            }
        }
    }
    let elapsed = start.elapsed();
    let writes = bus.host_writes
        + memory::Effects::read(&bus.runner.memory()[memory::OFFSET as usize..])
            .map_err(|e| e.to_string())?
            .ram_writes;
    Ok(Measurement {
        elapsed,
        frame: frame(&cpu, instructions),
        memory: bus.runner.memory()[RAM_POINTER..RAM_POINTER + RAM].to_vec(),
        generations: bus.runner.memory()[GENERATIONS..GENERATIONS + 16 * 8].to_vec(),
        writes,
        cpu: Some(cpu.snapshot()),
    })
}
