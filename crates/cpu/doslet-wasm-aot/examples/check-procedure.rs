//! Executable pure-register procedure ABI test, not a game benchmark.
use doslet_bus::{BusError, CodeCheck, MemoryBus, PortBus};
use doslet_wasm_aot::procedure::ProcedureArtifact;
use doslet_x86::{Cpu, DescriptorTable, Gpr32, SegmentRegister};
use doslet_x86_wasm::static_program::procedure::{
    ProcedureCompileOptions, ProcedureFunction, ProcedureId, ProcedureOperation as P,
    ProcedureProgram,
};
use doslet_x86_wasm::static_program::{Alu, Operation, Register, Shift, ShiftCount, Source};

const REGS: [Gpr32; 8] = [
    Gpr32::Eax,
    Gpr32::Ecx,
    Gpr32::Edx,
    Gpr32::Ebx,
    Gpr32::Esp,
    Gpr32::Ebp,
    Gpr32::Esi,
    Gpr32::Edi,
];
const START: u32 = 0x1000;
const RETURN: u32 = 0x1800;
const STACK: u32 = 0x7000;

struct Bus(Vec<u8>, [u64; 16]);
impl MemoryBus for Bus {
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
        self.1[address as usize / 4096] += 1;
        Ok(())
    }
    fn read_u16(&mut self, address: u32) -> Result<u16, BusError> {
        let start = address as usize;
        let bytes = self
            .0
            .get(start..start + 2)
            .ok_or(BusError::UnmappedMemory(address))?;
        Ok(u16::from_le_bytes(bytes.try_into().expect("two bytes")))
    }
    fn read_u32(&mut self, address: u32) -> Result<u32, BusError> {
        let start = address as usize;
        let bytes = self
            .0
            .get(start..start + 4)
            .ok_or(BusError::UnmappedMemory(address))?;
        Ok(u32::from_le_bytes(bytes.try_into().expect("four bytes")))
    }
    fn write_u32(&mut self, address: u32, value: u32) -> Result<(), BusError> {
        let start = address as usize;
        self.0
            .get_mut(start..start + 4)
            .ok_or(BusError::UnmappedMemory(address))?
            .copy_from_slice(&value.to_le_bytes());
        let first = start / 4096;
        let last = (start + 3) / 4096;
        self.1[first] += 1;
        if last != first {
            self.1[last] += 1;
        }
        Ok(())
    }
    fn is_plain_ram(&self, address: u32, length: u8) -> bool {
        address as u64 + u64::from(length) <= self.0.len() as u64
    }
    fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
        let start = address as usize;
        self.0
            .get(start..(start + usize::from(length)).min(self.0.len()))
    }
    fn code_check(&self, address: u32, length: u8) -> Option<CodeCheck> {
        if length == 0 || !self.is_plain_ram(address, length) {
            return None;
        }
        let first = address / 4096;
        let last = (address + u32::from(length) - 1) / 4096;
        Some(CodeCheck {
            space: 1,
            pages: [first, last],
            generations: [self.1[first as usize], self.1[last as usize]],
        })
    }
    fn code_valid(&self, check: &CodeCheck) -> bool {
        check.space == 1
            && check
                .pages
                .into_iter()
                .zip(check.generations)
                .all(|(page, generation)| self.1.get(page as usize) == Some(&generation))
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

fn alu(kind: Alu, target: Register, source: Register) -> P {
    P::Execute(Operation::Alu {
        kind,
        target,
        source: Source::Register(source),
    })
}

fn program() -> ProcedureProgram {
    ProcedureProgram {
        functions: vec![
            ProcedureFunction {
                id: ProcedureId(0),
                operations: vec![
                    alu(Alu::Add, Register::Eax, Register::Edx),
                    P::Call(ProcedureId(1)),
                    alu(Alu::Add, Register::Ax, Register::Bx),
                    P::Call(ProcedureId(1)),
                ],
            },
            ProcedureFunction {
                id: ProcedureId(1),
                operations: vec![
                    alu(Alu::Xor, Register::Ah, Register::Cl),
                    P::Call(ProcedureId(2)),
                    alu(Alu::Adc, Register::Eax, Register::Ebx),
                ],
            },
            ProcedureFunction {
                id: ProcedureId(2),
                operations: vec![
                    P::Execute(Operation::Inc(Register::Ax)),
                    P::Execute(Operation::Shift {
                        kind: Shift::Ror,
                        target: Register::Al,
                        count: ShiftCount::One,
                    }),
                ],
            },
        ],
    }
}

fn reference() -> Result<(Cpu, Bus), String> {
    let mut bus = Bus(vec![0; 65536], [0; 16]);
    // A: ADD EAX,EDX; CALL B; ADD AX,BX; CALL B; RET.
    // B: XOR AH,CL; CALL C; ADC EAX,EBX; RET.
    // C: INC AX; ROR AL,1; RET.
    let code = [
        0x01, 0xd0, 0xe8, 9, 0, 0, 0, 0x66, 0x01, 0xd8, 0xe8, 1, 0, 0, 0, 0xc3, 0x30, 0xcc, 0xe8,
        9, 0, 0, 0, 0x11, 0xd8, 0xc3, 0x90, 0x90, 0x90, 0x90, 0x90, 0x90, 0x66, 0x40, 0xd0, 0xc8,
        0xc3,
    ];
    bus.0[START as usize..START as usize + code.len()].copy_from_slice(&code);
    bus.0[0x208..0x218].copy_from_slice(&[
        0xff, 0xff, 0, 0, 0, 0x9a, 0xcf, 0, 0xff, 0xff, 0, 0, 0, 0x92, 0xcf, 0,
    ]);
    let mut cpu = Cpu::default();
    cpu.set_decode_cache(false);
    cpu.set_dynamic_core(false);
    cpu.control.gdtr = DescriptorTable {
        base: 0x200,
        limit: 23,
    };
    cpu.set_protected_mode(true);
    cpu.load_segment(&mut bus, SegmentRegister::Cs, 8)
        .map_err(|e| e.to_string())?;
    for segment in [
        SegmentRegister::Ss,
        SegmentRegister::Ds,
        SegmentRegister::Es,
    ] {
        cpu.load_segment(&mut bus, segment, 16)
            .map_err(|e| e.to_string())?;
    }
    Ok((cpu, bus))
}

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args
        .iter()
        .any(|arg| arg != "--bench" && arg != "--integrated")
        || args.len() > 1
    {
        return Err("usage: check-procedure [--bench|--integrated]".into());
    }
    let compiled = program()
        .compile(&ProcedureCompileOptions::default())
        .map_err(|e| e.to_string())?;
    let artifact = ProcedureArtifact::compile(&compiled)?;
    let mut runner = artifact.instantiate(ProcedureId(0))?;
    let (mut cpu, mut bus) = reference()?;
    let mut random = 0x3141_5926_u32;
    for sample in 0..1024 {
        let mut input = [0; 8];
        for value in &mut input {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            *value = random;
        }
        input[4] = STACK;
        let flags = 0x202 | (random & 0x8d5);
        cpu.registers.ip = START;
        cpu.flags = flags;
        for (register, value) in REGS.into_iter().zip(input) {
            cpu.registers.set32(register, value);
        }
        bus.0[STACK as usize..STACK as usize + 4].copy_from_slice(&RETURN.to_le_bytes());
        let mut steps = 0_u64;
        while cpu.registers.ip != RETURN && steps < 64 {
            cpu.step(&mut bus).map_err(|e| e.to_string())?;
            steps += 1;
        }
        if cpu.registers.ip != RETURN {
            return Err("reference failed to return".into());
        }
        let actual = runner.call(input, flags)?;
        let mut expected = core::array::from_fn(|i| cpu.registers.get32(REGS[i]));
        // The pure-register ABI leaves guest call-frame entry/return to its
        // caller. The reference executes outer RET and therefore pops 4 bytes.
        expected[4] = expected[4]
            .checked_sub(4)
            .ok_or("reference stack underflow")?;
        if actual.gpr != expected
            || actual.eflags != cpu.flags
            || actual.completed_instructions != steps
            || steps != 19
        {
            return Err(format!(
                "differential failure sample={sample}: actual={actual:?}, expected={expected:x?} flags={:x} steps={steps}",
                cpu.flags
            ));
        }
    }
    println!(
        "PASS: 1024 original-x86 versus Cranelift AOT procedure executions; A -> B -> C, B invoked twice; 8/16/32-bit registers, flags and 19 bulk-counted instructions agree."
    );
    println!(
        "WASM bytes={} AOT_compile_ms={:.3} load_ms={:.3}; no runtime imports or guest-memory ABI. This is a register procedure test, not EXE conversion, full guest-state equivalence, game integration or a speed benchmark.",
        compiled.wasm().len(),
        artifact.compile_seconds() * 1000.0,
        artifact.load_seconds() * 1000.0
    );
    if args.first().is_some_and(|arg| arg == "--bench") {
        benchmark::run()?;
    }
    if args.first().is_some_and(|arg| arg == "--integrated") {
        #[cfg(feature = "procedure-calls")]
        integration::run(&artifact)?;
        #[cfg(not(feature = "procedure-calls"))]
        return Err("--integrated requires --features procedure-calls".into());
    }
    Ok(())
}

#[path = "check-procedure/benchmark.rs"]
mod benchmark;
#[cfg(feature = "procedure-calls")]
#[path = "check-procedure/integration.rs"]
mod integration;
