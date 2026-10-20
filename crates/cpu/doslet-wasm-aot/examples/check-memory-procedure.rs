//! Original-x86 differential checks and warmed direct-memory procedure timings.
//! No game, device, IRQ or browser throughput is inferred from these fixtures.
use doslet_bus::{BusError, CodeCheck, MemoryBus, PortBus};
use doslet_wasm_aot::procedure_memory::{Exit, MemoryProcedureArtifact, MemoryProcedureRunner};
use doslet_x86::{Access, Cpu, DescriptorTable, Gpr32, SegmentRegister};
use doslet_x86_wasm::static_program::{
    Address, Alu, Condition, Frame, Operation, Register, Source, Width,
    memory::{
        self, Descriptor, MemoryAddress, RamDescriptor, Segment, SegmentBounds, StackAddress,
    },
    procedure_memory::{
        ProcedureBlock, ProcedureCompileOptions, ProcedureFunction, ProcedureGraphFunction,
        ProcedureGraphProgram, ProcedureId, ProcedureInstruction, ProcedureProgram,
        ProcedureReturn, ProcedureTerminator, ProcedureTerminatorKind,
    },
};
use std::time::{Duration, Instant};

const RAM: usize = 65_536;
const RAM_POINTER: usize = 65_536;
const GENERATIONS: usize = 512;
const CODE_PAGES: usize = 640;
const START: u32 = 0x1000;
const RETURN: u32 = 0x3000;
const DATA: u32 = 0x4000;
const STACK: u32 = 0xf000;
const REGISTERS: [Gpr32; 8] = [
    Gpr32::Eax,
    Gpr32::Ecx,
    Gpr32::Edx,
    Gpr32::Ebx,
    Gpr32::Esp,
    Gpr32::Ebp,
    Gpr32::Esi,
    Gpr32::Edi,
];

/// A borrowed linear-memory bus: neither variant copies guest RAM per entry.
struct Bus<'a> {
    bytes: &'a mut [u8],
    writes: u64,
}
impl Bus<'_> {
    fn span(&self, address: u32, size: usize) -> Result<std::ops::Range<usize>, BusError> {
        let start = address as usize;
        if start.checked_add(size).is_none_or(|end| end > RAM) {
            return Err(BusError::UnmappedMemory(address));
        }
        Ok(RAM_POINTER + start..RAM_POINTER + start + size)
    }
    fn generation(&self, page: u32) -> u64 {
        let start = GENERATIONS + page as usize * 8;
        u64::from_le_bytes(self.bytes[start..start + 8].try_into().expect("page table"))
    }
    fn store(&mut self, address: u32, value: &[u8]) -> Result<(), BusError> {
        let span = self.span(address, value.len())?;
        self.bytes[span].copy_from_slice(value);
        let first = address / 4096;
        let last = (address + value.len() as u32 - 1) / 4096;
        for page in first..=last {
            let generation = self.generation(page) + 1;
            let start = GENERATIONS + page as usize * 8;
            self.bytes[start..start + 8].copy_from_slice(&generation.to_le_bytes());
        }
        self.writes += 1;
        Ok(())
    }
}
impl MemoryBus for Bus<'_> {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        Ok(self.bytes[self.span(address, 1)?.start])
    }
    fn read_u16(&mut self, address: u32) -> Result<u16, BusError> {
        Ok(u16::from_le_bytes(
            self.bytes[self.span(address, 2)?].try_into().expect("word"),
        ))
    }
    fn read_u32(&mut self, address: u32) -> Result<u32, BusError> {
        Ok(u32::from_le_bytes(
            self.bytes[self.span(address, 4)?]
                .try_into()
                .expect("dword"),
        ))
    }
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        self.store(address, &[value])
    }
    fn write_u16(&mut self, address: u32, value: u16) -> Result<(), BusError> {
        self.store(address, &value.to_le_bytes())
    }
    fn write_u32(&mut self, address: u32, value: u32) -> Result<(), BusError> {
        self.store(address, &value.to_le_bytes())
    }
    fn is_plain_ram(&self, address: u32, length: u8) -> bool {
        u64::from(address) + u64::from(length) <= RAM as u64
    }
    fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
        self.span(address, length as usize)
            .ok()
            .map(|span| &self.bytes[span])
    }
    fn code_check(&self, address: u32, length: u8) -> Option<CodeCheck> {
        if length == 0 || !self.is_plain_ram(address, length) {
            return None;
        }
        let pages = [address / 4096, (address + u32::from(length) - 1) / 4096];
        Some(CodeCheck {
            space: 1,
            pages,
            generations: pages.map(|page| self.generation(page)),
        })
    }
    fn code_valid(&self, check: &CodeCheck) -> bool {
        check.space == 1
            && check
                .pages
                .into_iter()
                .zip(check.generations)
                .all(|(page, generation)| page < 16 && self.generation(page) == generation)
    }
}
impl PortBus for Bus<'_> {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
        Err(BusError::UnmappedPort(port))
    }
    fn write_port_u8(&mut self, port: u16, _: u8) -> Result<(), BusError> {
        Err(BusError::UnmappedPort(port))
    }
}

fn address(offset: u32) -> Address {
    Address { space: 1, offset }
}
fn operand(base: Register, displacement: u32) -> MemoryAddress {
    MemoryAddress {
        base: Some(base),
        index: None,
        scale: 1,
        displacement,
        address16: false,
        segment: Segment::Ds,
    }
}

struct Fixture {
    code: Vec<u8>,
    instructions: Vec<ProcedureInstruction>,
    controls: Vec<ProcedureTerminator>,
    maximum_work: u32,
}
impl Fixture {
    fn emit(&mut self, code: &[u8], operation: Operation) {
        self.instructions.push(ProcedureInstruction {
            address: address(START + self.code.len() as u32),
            operation,
        });
        self.code.extend_from_slice(code);
    }
    fn memory(cells: usize, branches: bool) -> Self {
        let mut fixture = Self {
            code: vec![],
            instructions: vec![],
            controls: vec![],
            maximum_work: 0,
        };
        fixture.emit(
            &[0x55],
            Operation::Push {
                width: Width::Dword,
                source: Source::Register(Register::Ebp),
            },
        );
        fixture.emit(
            &[0x89, 0xe5],
            Operation::Move {
                target: Register::Ebp,
                source: Source::Register(Register::Esp),
            },
        );
        fixture.emit(
            &[0x53],
            Operation::Push {
                width: Width::Dword,
                source: Source::Register(Register::Ebx),
            },
        );
        for cell in 0..cells {
            let displacement = cell as u32 * 8;
            let mut load = vec![0x8b, 0x86];
            load.extend_from_slice(&displacement.to_le_bytes());
            fixture.emit(
                &load,
                Operation::LoadMemory {
                    target: Register::Eax,
                    address: operand(Register::Esi, displacement),
                },
            );
            let mut extend = vec![0x0f, 0xb6, 0x96];
            extend.extend_from_slice(&(displacement + 4).to_le_bytes());
            fixture.emit(
                &extend,
                Operation::ExtendMemory {
                    target: Register::Edx,
                    width: Width::Byte,
                    address: operand(Register::Esi, displacement + 4),
                    signed: false,
                },
            );
            fixture.emit(
                &[0x66, 0x01, 0xd0],
                Operation::Alu {
                    kind: Alu::Add,
                    target: Register::Ax,
                    source: Source::Register(Register::Dx),
                },
            );
            fixture.emit(
                &[0x6b, 0xc0, 3],
                Operation::SignedMultiply {
                    target: Register::Eax,
                    left: Source::Register(Register::Eax),
                    right: Source::Immediate(3),
                },
            );
            if branches {
                fixture.emit(
                    &[0xa8, 1],
                    Operation::Alu {
                        kind: Alu::Test,
                        target: Register::Al,
                        source: Source::Immediate(1),
                    },
                );
                let branch = START + fixture.code.len() as u32;
                fixture.controls.push(ProcedureTerminator {
                    address: address(branch),
                    kind: ProcedureTerminatorKind::Branch {
                        condition: Condition::Equal,
                        taken: address(branch + 6),
                        not_taken: address(branch + 2),
                    },
                });
                fixture.code.extend_from_slice(&[0x74, 4]);
                fixture.emit(
                    &[0x31, 0xd0],
                    Operation::Alu {
                        kind: Alu::Xor,
                        target: Register::Eax,
                        source: Source::Register(Register::Edx),
                    },
                );
                fixture.controls.push(ProcedureTerminator {
                    address: address(branch + 4),
                    kind: ProcedureTerminatorKind::Jump(address(branch + 8)),
                });
                fixture.code.extend_from_slice(&[0xeb, 2]);
                fixture.emit(&[0xf7, 0xd8], Operation::Neg(Register::Eax));
            }
            fixture.emit(
                &[0x30, 0xd0],
                Operation::Alu {
                    kind: Alu::Xor,
                    target: Register::Al,
                    source: Source::Register(Register::Dl),
                },
            );
            let mut store = vec![0x89, 0x87];
            store.extend_from_slice(&displacement.to_le_bytes());
            fixture.emit(
                &store,
                Operation::StoreMemory {
                    width: Width::Dword,
                    address: operand(Register::Edi, displacement),
                    source: Source::Register(Register::Eax),
                },
            );
        }
        fixture.emit(
            &[0x5b],
            Operation::Pop {
                target: Register::Ebx,
            },
        );
        fixture.emit(
            &[0x5d],
            Operation::Pop {
                target: Register::Ebp,
            },
        );
        fixture
    }
    fn program(&self) -> ProcedureProgram {
        ProcedureProgram {
            functions: vec![ProcedureFunction {
                id: ProcedureId(0),
                entry: address(START),
                instructions: self.instructions.clone(),
                terminator: ProcedureReturn {
                    address: address(START + self.code.len() as u32),
                    width: Width::Dword,
                    pop_bytes: 0,
                },
            }],
        }
    }

    fn graph_program(&self) -> ProcedureGraphProgram {
        let mut boundaries = vec![address(START)];
        for control in &self.controls {
            match control.kind {
                ProcedureTerminatorKind::Branch {
                    taken, not_taken, ..
                } => {
                    boundaries.extend([taken, not_taken]);
                }
                ProcedureTerminatorKind::Jump(target) => boundaries.push(target),
                _ => unreachable!("fixture controls"),
            }
        }
        boundaries.sort_unstable();
        boundaries.dedup();
        let mut blocks = Vec::new();
        for (index, &entry) in boundaries.iter().enumerate() {
            let end = boundaries.get(index + 1).copied();
            let contains =
                |address: Address| address >= entry && end.is_none_or(|end| address < end);
            let instructions = self
                .instructions
                .iter()
                .filter(|op| contains(op.address))
                .copied()
                .collect();
            let terminator = self
                .controls
                .iter()
                .find(|control| contains(control.address))
                .copied()
                .unwrap_or_else(|| match end {
                    Some(target) => ProcedureTerminator {
                        address: target,
                        kind: ProcedureTerminatorKind::Fallthrough(target),
                    },
                    None => ProcedureTerminator {
                        address: address(START + self.code.len() as u32),
                        kind: ProcedureTerminatorKind::Return {
                            width: Width::Dword,
                            pop_bytes: 0,
                        },
                    },
                });
            blocks.push(ProcedureBlock {
                entry,
                instructions,
                terminator,
            });
        }
        ProcedureGraphProgram {
            functions: vec![ProcedureGraphFunction {
                id: ProcedureId(0),
                entry: address(START),
                blocks,
            }],
        }
    }
}

fn initialize(
    runner: &mut MemoryProcedureRunner,
    fixture: &Fixture,
    seed: u32,
) -> Result<Cpu, String> {
    let bytes = runner.memory_mut();
    bytes.fill(0);
    let ram = &mut bytes[RAM_POINTER..RAM_POINTER + RAM];
    ram[START as usize..START as usize + fixture.code.len()].copy_from_slice(&fixture.code);
    ram[START as usize + fixture.code.len()] = 0xc3;
    ram[STACK as usize..STACK as usize + 4].copy_from_slice(&RETURN.to_le_bytes());
    ram[0x208..0x218].copy_from_slice(&[
        0xff, 0xff, 0, 0, 0, 0x9a, 0xcf, 0, 0xff, 0xff, 0, 0, 0, 0x92, 0xcf, 0,
    ]);
    let mut random = seed;
    for byte in &mut ram[DATA as usize..DATA as usize + 1024] {
        random = random.wrapping_mul(1664525).wrapping_add(1013904223);
        *byte = (random >> 24) as u8;
    }
    let mut cpu = Cpu::default();
    cpu.set_dynamic_core(false);
    cpu.set_decode_cache(true);
    cpu.control.gdtr = DescriptorTable {
        base: 0x200,
        limit: 23,
    };
    cpu.set_protected_mode(true);
    let mut bus = Bus { bytes, writes: 0 };
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
    cpu.flags = 0x202;
    cpu.registers.ip = START;
    for (register, value) in REGISTERS
        .into_iter()
        .zip([seed, 13, 7, 0x98765432, STACK, 0x12345678, DATA, DATA])
    {
        cpu.registers.set32(register, value);
    }
    let descriptor = Descriptor {
        ram: RamDescriptor {
            pointer: RAM_POINTER as u32 + 0x500,
            linear_base: 0x500,
            length: RAM as u32 - 0x500,
            generations_pointer: GENERATIONS as u32,
            generations_length: 16,
            code_pages_pointer: CODE_PAGES as u32,
            code_pages_length: 16,
            ..Default::default()
        },
        stack_address: StackAddress::Bits32,
        windows: windows(&cpu),
        ..Default::default()
    };
    descriptor
        .write(&mut bytes[memory::OFFSET as usize..])
        .map_err(|e| e.to_string())?;
    for page in START as usize / 4096..=(START as usize + fixture.code.len()) / 4096 {
        bytes[CODE_PAGES + page] = 1;
    }
    frame(&cpu, 0).write(bytes).map_err(|e| e.to_string())?;
    Ok(cpu)
}

fn frame(cpu: &Cpu, charged_work: u64) -> Frame {
    Frame {
        gpr: REGISTERS.map(|reg| cpu.registers.get32(reg)),
        eflags: cpu.flags,
        address: address(cpu.registers.ip),
        charged_work,
        ..Default::default()
    }
}

fn windows(cpu: &Cpu) -> [[SegmentBounds; 6]; 2] {
    [Access::Read, Access::Write].map(|access| {
        [
            SegmentRegister::Es,
            SegmentRegister::Cs,
            SegmentRegister::Ss,
            SegmentRegister::Ds,
            SegmentRegister::Fs,
            SegmentRegister::Gs,
        ]
        .map(|segment| {
            cpu.registers
                .window(segment, access)
                .map_or(SegmentBounds::default(), |window| {
                    let (base, first, last) = window.parts();
                    SegmentBounds {
                        valid: true,
                        base,
                        first,
                        last,
                    }
                })
        })
    })
}

struct Measurement {
    elapsed: Duration,
    frame: Frame,
    memory: Vec<u8>,
    generations: Vec<u8>,
    writes: u64,
    cpu: Option<doslet_x86::CpuSnapshot>,
}

fn measure(
    runner: &mut MemoryProcedureRunner,
    fixture: &Fixture,
    native: bool,
    iterations: usize,
    seed: u32,
) -> Result<Measurement, String> {
    let mut cpu = initialize(runner, fixture, seed)?;
    let mut boundary = frame(&cpu, 0);
    let warmup_entries = if iterations >= 1024 { 32 } else { 0 };
    let mut start = Instant::now();
    let mut writes = 0;
    if native {
        let mut bus = Bus {
            bytes: runner.memory_mut(),
            writes: 0,
        };
        for invocation in 0..warmup_entries + iterations {
            if invocation == warmup_entries {
                start = Instant::now();
            }
            cpu.registers.ip = START;
            cpu.registers.set32(Gpr32::Esp, STACK);
            let mut count = 0;
            while cpu.registers.ip != RETURN && count <= 4096 {
                cpu.step(&mut bus).map_err(|e| e.to_string())?;
                count += 1;
            }
            if cpu.registers.ip != RETURN {
                return Err("native procedure failed to return".into());
            }
            boundary.charged_work += count as u64;
        }
        writes = bus.writes;
        boundary = frame(&cpu, boundary.charged_work);
    } else {
        for invocation in 0..warmup_entries + iterations {
            if invocation == warmup_entries {
                start = Instant::now();
            }
            boundary.address = address(START);
            boundary.gpr[4] = STACK;
            boundary
                .write(runner.memory_mut())
                .map_err(|e| e.to_string())?;
            if runner.call(0)? != Exit::Complete {
                return Err(format!(
                    "unexpected AOT side exit: {:?}",
                    Frame::read(runner.memory()).map_err(|e| e.to_string())?
                ));
            }
            boundary = Frame::read(runner.memory()).map_err(|e| e.to_string())?;
        }
    }
    let elapsed = start.elapsed();
    if !native {
        writes = memory::Effects::read(&runner.memory()[memory::OFFSET as usize..])
            .map_err(|e| e.to_string())?
            .ram_writes;
    }
    Ok(Measurement {
        elapsed,
        frame: boundary,
        memory: runner.memory()[RAM_POINTER..RAM_POINTER + RAM].to_vec(),
        generations: runner.memory()[GENERATIONS..GENERATIONS + 16 * 8].to_vec(),
        writes,
        cpu: None,
    })
}

fn compare(native: &Measurement, aot: &Measurement) -> Result<(), String> {
    if native.frame != aot.frame
        || native.cpu != aot.cpu
        || native.memory != aot.memory
        || native.generations != aot.generations
        || native.writes != aot.writes
    {
        return Err(format!(
            "CPU/RAM/generation/work mismatch: native={:?} aot={:?}; ram={} generations={} writes={}/{}",
            native.frame,
            aot.frame,
            native.memory == aot.memory,
            native.generations == aot.generations,
            native.writes,
            aot.writes
        ));
    }
    Ok(())
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn measure_selected(
    runner: &mut MemoryProcedureRunner,
    fixture: &Fixture,
    native: bool,
    iterations: usize,
    seed: u32,
    integrated: bool,
) -> Result<Measurement, String> {
    if integrated {
        #[cfg(feature = "procedure-calls")]
        return integration::measure(runner, fixture, native, iterations, seed);
        #[cfg(not(feature = "procedure-calls"))]
        return Err("--integrated requires --features procedure-calls".into());
    }
    measure(runner, fixture, native, iterations, seed)
}

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args
        .iter()
        .any(|arg| arg != "--bench" && arg != "--integrated")
        || args.len() > 2
    {
        return Err("usage: check-memory-procedure [--bench] [--integrated]".into());
    }
    let benchmark = args.iter().any(|arg| arg == "--bench");
    let integrated = args.iter().any(|arg| arg == "--integrated");
    println!(
        "Boundary: {}",
        if integrated {
            "interpreted CALL -> generic registered memory-procedure hook -> actual guest RET; timing includes registry/context checks and CPU commit"
        } else {
            "direct procedure entry; timing includes frame marshalling and Wasmtime entry/return"
        }
    );
    for (branches, cells) in [
        (false, 1),
        (false, 8),
        (false, 32),
        (true, 1),
        (true, 8),
        (true, 32),
    ] {
        let label = if branches {
            "memory+branches"
        } else {
            "memory"
        };
        let mut fixture = Fixture::memory(cells, branches);
        let compiled = if branches {
            fixture
                .graph_program()
                .compile(&ProcedureCompileOptions::default())
        } else {
            fixture
                .program()
                .compile(&ProcedureCompileOptions::default())
        }
        .map_err(|e| e.to_string())?;
        fixture.maximum_work = u32::try_from(
            compiled
                .maximum_work(ProcedureId(0))
                .ok_or("missing maximum work")?,
        )
        .map_err(|e| e.to_string())?;
        let artifact = MemoryProcedureArtifact::compile(&compiled)?;
        let mut native_runner = artifact.instantiate(ProcedureId(0), 2)?;
        let mut aot_runner = artifact.instantiate(ProcedureId(0), 2)?;
        for seed in 1..=32 {
            let native =
                measure_selected(&mut native_runner, &fixture, true, 16, seed, integrated)?;
            let aot = measure_selected(&mut aot_runner, &fixture, false, 16, seed, integrated)?;
            compare(&native, &aot)?;
        }
        println!(
            "PASS {label} cells={cells}: 32 seeds x 16 stateful entries, 8/16/32-bit operations, stack, all GPRs/flags/EIP, full 64KiB RAM, page generations, stores and completed work agree; WASM bytes={} AOT_compile_ms={:.3} load_ms={:.3}",
            compiled.wasm().len(),
            artifact.compile_seconds() * 1000.0,
            artifact.load_seconds() * 1000.0
        );
        if !benchmark {
            continue;
        }
        let iterations = 32_768;
        let native = measure_selected(
            &mut native_runner,
            &fixture,
            true,
            iterations,
            42,
            integrated,
        )?;
        let aot = measure_selected(&mut aot_runner, &fixture, false, iterations, 42, integrated)?;
        compare(&native, &aot)?; // Untimed warmup and verification.
        let mut native_ms = Vec::new();
        let mut aot_ms = Vec::new();
        for repeat in 0..7 {
            let (native, aot) = if repeat % 2 == 0 {
                (
                    measure_selected(
                        &mut native_runner,
                        &fixture,
                        true,
                        iterations,
                        42,
                        integrated,
                    )?,
                    measure_selected(&mut aot_runner, &fixture, false, iterations, 42, integrated)?,
                )
            } else {
                let aot =
                    measure_selected(&mut aot_runner, &fixture, false, iterations, 42, integrated)?;
                (
                    measure_selected(
                        &mut native_runner,
                        &fixture,
                        true,
                        iterations,
                        42,
                        integrated,
                    )?,
                    aot,
                )
            };
            compare(&native, &aot)?;
            native_ms.push(native.elapsed.as_secs_f64() * 1000.0);
            aot_ms.push(aot.elapsed.as_secs_f64() * 1000.0);
        }
        println!("samples {label} cells={cells} native_ms={native_ms:?} aot_ms={aot_ms:?}");
        let native = median(&mut native_ms);
        let aot = median(&mut aot_ms);
        println!(
            "BENCH {label} cells={cells} entries={iterations} repeats=7 warmup=1 maximum_instructions_per_entry={} native_cached_ms={native:.3} aot_ms={aot:.3} speedup={:.3} native_ns_entry={:.3} aot_ns_entry={:.3}",
            compiled
                .maximum_work(ProcedureId(0))
                .ok_or("missing maximum work")?,
            native / aot,
            native * 1e6 / iterations as f64,
            aot * 1e6 / iterations as f64
        );
    }
    Ok(())
}

#[cfg(feature = "procedure-calls")]
#[path = "check-memory-procedure/integration.rs"]
mod integration;
