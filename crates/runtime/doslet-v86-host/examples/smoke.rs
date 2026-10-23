//! Execute a small real-mode program using the imported v86 JIT.
use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_v86::Registers;
use doslet_v86_host::{Core, Execution};

struct Devices;
impl MemoryBus for Devices {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        Err(BusError::UnmappedMemory(address))
    }
    fn write_u8(&mut self, address: u32, _: u8) -> Result<(), BusError> {
        Err(BusError::UnmappedMemory(address))
    }
}
impl PortBus for Devices {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
        Err(BusError::UnmappedPort(port))
    }
    fn write_port_u8(&mut self, port: u16, _: u8) -> Result<(), BusError> {
        Err(BusError::UnmappedPort(port))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let started = std::time::Instant::now();
    let core = Core::embedded()?;
    println!("Embedded v86 loaded in {:?}", started.elapsed());
    let mut cpu = core.instantiate(Devices, 0x20_0000)?;
    cpu.write_ram(0x100, &[0x40, 0xe2, 0xfd, 0xf4])?; // INC AX; LOOP; HLT.
    cpu.reset_real_mode(Registers {
        eip: 0x100,
        eflags: 2,
        gpr: [0, 4000, 0, 0, 0xfffe, 0, 0, 0],
        ..Registers::default()
    })?;
    cpu.prepare_current()?;
    let mut steps = 0;
    let mut halted = false;
    for _ in 0..100 {
        let result = cpu.run(4096, Execution::Jit)?;
        steps += result.steps;
        if result.halted {
            halted = true;
            break;
        }
    }
    let registers = cpu.registers()?;
    let stats = cpu.statistics()?;
    if !halted || registers.gpr[0] != 4000 || steps != 8001 || stats.jit_entries == 0 {
        return Err("v86 smoke result did not match the expected guest execution".into());
    }
    println!(
        "v86: AX={}, steps={}, compiled={}, JIT entries={}",
        registers.gpr[0], steps, stats.compiled_modules, stats.jit_entries
    );
    Ok(())
}
