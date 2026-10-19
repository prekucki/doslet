extern crate std;

use super::*;
use crate::static_program::{
    Alu, Frame, MemoryUnary, Register, Source,
    memory::{
        self, Descriptor, MemoryAddress, RamDescriptor, Segment, SegmentBounds, StackAddress,
    },
};
use alloc::{vec, vec::Vec};
use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_x86::{Cpu, DescriptorTable, Gpr32, SegmentRegister};
use wasmi::{Engine, Linker, Memory, MemoryType, Module, Store, TypedFunc};

const FRAME: usize = 256;
const RAM: usize = 4096;
const RAM_BASE: u32 = 0x500;
const RAM_LENGTH: usize = 0x4000;
const GENERATIONS: usize = 24576;
const PROTECTED: usize = 25088;
const PAGES: usize = 32;
const ENTRY: Address = Address {
    space: 0x71,
    offset: 0x8000,
};
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

struct Runner {
    store: Store<()>,
    memory: Memory,
    run: TypedFunc<u32, u32>,
}
impl Runner {
    fn new(program: &ProcedureProgram) -> Self {
        let compiled = program
            .compile(&ProcedureCompileOptions::default())
            .unwrap();
        Self::compiled(&compiled)
    }
    fn compiled(compiled: &CompiledProcedureProgram) -> Self {
        let engine = Engine::default();
        let module = Module::new(&engine, compiled.wasm()).unwrap();
        assert_eq!(
            module.imports().count(),
            1,
            "memory must be the sole import"
        );
        let mut store = Store::new(&engine, ());
        let memory = Memory::new(&mut store, MemoryType::new(1, Some(1))).unwrap();
        let mut linker = Linker::new(&engine);
        linker
            .define(abi::IMPORT_MODULE, abi::MEMORY_IMPORT, memory)
            .unwrap();
        let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
        let run = instance
            .get_typed_func(&store, compiled.export_name(ProcedureId(3)).unwrap())
            .unwrap();
        Self { store, memory, run }
    }
    fn bytes(&mut self) -> &mut [u8] {
        self.memory.data_mut(&mut self.store)
    }
    fn frame(&self) -> Frame {
        Frame::read(&self.memory.data(&self.store)[FRAME..]).unwrap()
    }
    fn effects(&self) -> memory::Effects {
        memory::Effects::read(&self.memory.data(&self.store)[FRAME + memory::OFFSET as usize..])
            .unwrap()
    }
    fn call(&mut self) -> u32 {
        self.run.call(&mut self.store, FRAME as u32).unwrap()
    }
    fn generation(&self, page: usize) -> u64 {
        u64::from_le_bytes(
            self.memory.data(&self.store)[GENERATIONS + page * 8..][..8]
                .try_into()
                .unwrap(),
        )
    }
    fn set(&mut self, frame: Frame, descriptor: &Descriptor) {
        self.bytes().fill(0xa5);
        frame.write(&mut self.bytes()[FRAME..]).unwrap();
        descriptor
            .write(&mut self.bytes()[FRAME + memory::OFFSET as usize..])
            .unwrap();
        self.bytes()[PROTECTED..PROTECTED + PAGES].fill(0);
        self.bytes()[PROTECTED + 8] = 1;
        for page in 0..PAGES {
            self.set_generation(page, 11 + page as u64);
        }
    }
    fn set_generation(&mut self, page: usize, value: u64) {
        self.bytes()[GENERATIONS + page * 8..][..8].copy_from_slice(&value.to_le_bytes());
    }
    fn word(&mut self, offset: u32, value: u32) {
        let offset = FRAME + memory::OFFSET as usize + offset as usize;
        self.bytes()[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}

#[path = "procedure_memory_graph_tests.rs"]
mod graph_tests;
#[path = "procedure_string_tests.rs"]
mod string_tests;
fn descriptor() -> Descriptor {
    Descriptor {
        ram: RamDescriptor {
            pointer: RAM as u32,
            linear_base: RAM_BASE,
            length: RAM_LENGTH as u32,
            generations_pointer: GENERATIONS as u32,
            generations_length: PAGES as u32,
            code_pages_pointer: PROTECTED as u32,
            code_pages_length: PAGES as u32,
            ..RamDescriptor::default()
        },
        windows: [[SegmentBounds {
            valid: true,
            base: 0,
            first: 0,
            last: u32::MAX,
        }; 6]; 2],
        stack_address: StackAddress::Bits32,
        inline_grant: 93,
        ..Descriptor::default()
    }
}
fn initial() -> Frame {
    Frame {
        gpr: [
            0x1234_5678,
            0xfedc_ba98,
            0x3141_5926,
            0x1003,
            0x2400,
            0x9988_7766,
            0xaacc_eeff,
            0x1133_5577,
        ],
        eflags: 0x202,
        address: ENTRY,
        charged_work: 39,
        remaining_budget: 987,
        status: 0x8000_0000,
    }
}
fn address(base: Option<Register>, displacement: u32, segment: Segment) -> MemoryAddress {
    MemoryAddress {
        base,
        index: None,
        scale: 1,
        displacement,
        address16: false,
        segment,
    }
}
fn ds(offset: u32) -> MemoryAddress {
    address(None, offset, Segment::Ds)
}
fn bp(offset: i32) -> MemoryAddress {
    address(Some(Register::Ebp), offset as u32, Segment::Ss)
}
fn program(
    pairs: &[(Operation, &[u8])],
    ret_width: Width,
    pop: u16,
    code32: bool,
) -> (ProcedureProgram, Vec<u8>) {
    let mut code = Vec::new();
    let mut instructions = Vec::new();
    for &(operation, bytes) in pairs {
        instructions.push(ProcedureInstruction {
            address: Address {
                offset: ENTRY.offset + code.len() as u32,
                ..ENTRY
            },
            operation,
        });
        code.extend_from_slice(bytes);
    }
    let terminator = ProcedureReturn {
        address: Address {
            offset: ENTRY.offset + code.len() as u32,
            ..ENTRY
        },
        width: ret_width,
        pop_bytes: pop,
    };
    if (ret_width == Width::Dword) != code32 {
        code.push(0x66);
    }
    if pop == 0 {
        code.push(0xc3);
    } else {
        code.push(0xc2);
        code.extend_from_slice(&pop.to_le_bytes());
    }
    (
        ProcedureProgram {
            functions: vec![ProcedureFunction {
                id: ProcedureId(3),
                entry: ENTRY,
                instructions,
                terminator,
            }],
        },
        code,
    )
}

struct Bus {
    bytes: Vec<u8>,
    touched: [bool; PAGES],
}
impl MemoryBus for Bus {
    fn read_u8(&mut self, a: u32) -> Result<u8, BusError> {
        self.bytes
            .get(a as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(a))
    }
    fn write_u8(&mut self, a: u32, value: u8) -> Result<(), BusError> {
        *self
            .bytes
            .get_mut(a as usize)
            .ok_or(BusError::UnmappedMemory(a))? = value;
        self.touched[a as usize >> 12] = true;
        Ok(())
    }
    fn code_bytes(&self, a: u32, len: u8) -> Option<&[u8]> {
        self.bytes.get(a as usize..a as usize + len as usize)
    }
}
impl PortBus for Bus {
    fn read_port_u8(&mut self, p: u16) -> Result<u8, BusError> {
        Err(BusError::UnmappedPort(p))
    }
    fn write_port_u8(&mut self, p: u16, _: u8) -> Result<(), BusError> {
        Err(BusError::UnmappedPort(p))
    }
}
fn reference(code: &[u8], code32: bool, stack: StackAddress, ss_base: u32) -> (Cpu, Bus) {
    let mut bus = Bus {
        bytes: vec![0; 0x20_000],
        touched: [false; PAGES],
    };
    bus.bytes[ENTRY.offset as usize..ENTRY.offset as usize + code.len()].copy_from_slice(code);
    bus.bytes[0x208..0x220].copy_from_slice(&[
        0xff,
        0xff,
        0,
        0,
        0,
        0x9a,
        if code32 { 0xcf } else { 0x8f },
        0,
        0xff,
        0xff,
        ss_base as u8,
        (ss_base >> 8) as u8,
        (ss_base >> 16) as u8,
        0x92,
        if stack == StackAddress::Bits32 {
            0xcf
        } else {
            0x8f
        },
        (ss_base >> 24) as u8,
        0xff,
        0xff,
        0,
        0,
        0,
        0x92,
        0xcf,
        0,
    ]);
    let mut cpu = Cpu::default();
    cpu.set_decode_cache(false);
    cpu.set_dynamic_core(false);
    cpu.control.gdtr = DescriptorTable {
        base: 0x200,
        limit: 31,
    };
    cpu.set_protected_mode(true);
    cpu.load_segment(&mut bus, SegmentRegister::Cs, 8).unwrap();
    cpu.load_segment(&mut bus, SegmentRegister::Ss, 16).unwrap();
    cpu.load_segment(&mut bus, SegmentRegister::Ds, 24).unwrap();
    cpu.load_segment(&mut bus, SegmentRegister::Es, 24).unwrap();
    (cpu, bus)
}
fn differential(
    program: &ProcedureProgram,
    code: &[u8],
    frame: Frame,
    d: Descriptor,
    code32: bool,
    setup: impl FnOnce(&mut [u8]),
) {
    let mut runner = Runner::new(program);
    runner.set(frame, &d);
    let start = RAM
        + ((frame.gpr[4]
            & if d.stack_address == StackAddress::Bits16 {
                0xffff
            } else {
                u32::MAX
            })
        .wrapping_add(d.windows[0][Segment::Ss as usize].base)
            - d.ram.linear_base) as usize;
    runner.bytes()[start..start + 4].copy_from_slice(&0x9123_8bad_u32.to_le_bytes());
    setup(runner.bytes());
    let (mut cpu, mut bus) = reference(
        code,
        code32,
        d.stack_address,
        d.windows[0][Segment::Ss as usize].base,
    );
    bus.bytes[d.ram.linear_base as usize..d.ram.linear_base as usize + RAM_LENGTH]
        .copy_from_slice(&runner.bytes()[RAM..RAM + RAM_LENGTH]);
    cpu.flags = frame.eflags;
    cpu.registers.ip = ENTRY.offset;
    for (reg, value) in REGS.into_iter().zip(frame.gpr) {
        cpu.registers.set32(reg, value);
    }
    let mut generations = core::array::from_fn::<_, PAGES, _>(|i| 11 + i as u64);
    let mut writes = 0;
    for instruction in &program.functions[0].instructions {
        assert_eq!(cpu.registers.ip, instruction.address.offset);
        bus.touched.fill(false);
        cpu.step(&mut bus).unwrap();
        for (generation, touched) in generations.iter_mut().zip(bus.touched) {
            if touched {
                *generation += 1;
                writes += 1;
            }
        }
    }
    assert_eq!(
        cpu.registers.ip,
        program.functions[0].terminator.address.offset
    );
    cpu.step(&mut bus).unwrap();
    assert_eq!(runner.call(), COMPLETE);
    assert_eq!(
        runner.frame(),
        Frame {
            gpr: core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
            eflags: cpu.flags,
            address: Address {
                offset: cpu.registers.ip,
                ..ENTRY
            },
            charged_work: frame.charged_work + program.functions[0].instructions.len() as u64 + 1,
            ..frame
        }
    );
    assert_eq!(
        &runner.bytes()[RAM..RAM + RAM_LENGTH],
        &bus.bytes[d.ram.linear_base as usize..d.ram.linear_base as usize + RAM_LENGTH]
    );
    for (page, generation) in generations.into_iter().enumerate() {
        assert_eq!(runner.generation(page), generation, "page {page}");
    }
    assert_eq!(
        runner.effects(),
        memory::Effects {
            ram_writes: writes,
            inline_remaining: d.inline_grant,
            ..memory::Effects::default()
        }
    );
    assert_eq!(&runner.bytes()[FRAME + 44..FRAME + 48], &[0xa5; 4]);
    assert_eq!(&runner.bytes()[FRAME + 68..FRAME + 72], &[0xa5; 4]);
}

#[test]
fn real_stack_prologue_arguments_locals_and_epilogue_match_original_x86() {
    let pairs: &[(Operation, &[u8])] = &[
        (
            Operation::Push {
                width: Width::Dword,
                source: Source::Register(Register::Ebp),
            },
            &[0x55],
        ),
        (
            Operation::Move {
                target: Register::Ebp,
                source: Source::Register(Register::Esp),
            },
            &[0x89, 0xe5],
        ),
        (
            Operation::Alu {
                kind: Alu::Sub,
                target: Register::Esp,
                source: Source::Immediate(8),
            },
            &[0x83, 0xec, 8],
        ),
        (
            Operation::LoadMemory {
                target: Register::Eax,
                address: bp(8),
            },
            &[0x8b, 0x45, 8],
        ),
        (
            Operation::LoadMemory {
                target: Register::Ah,
                address: bp(12),
            },
            &[0x8a, 0x65, 12],
        ),
        (
            Operation::AluMemory {
                kind: Alu::Add,
                target: Register::Eax,
                address: bp(16),
            },
            &[0x03, 0x45, 16],
        ),
        (
            Operation::StoreMemory {
                width: Width::Dword,
                source: Source::Register(Register::Eax),
                address: bp(-4),
            },
            &[0x89, 0x45, 0xfc],
        ),
        (
            Operation::ExtendMemory {
                target: Register::Ecx,
                width: Width::Byte,
                signed: false,
                address: bp(-1),
            },
            &[0x0f, 0xb6, 0x4d, 0xff],
        ),
        (
            Operation::ExtendMemory {
                target: Register::Edx,
                width: Width::Word,
                signed: true,
                address: bp(-4),
            },
            &[0x0f, 0xbf, 0x55, 0xfc],
        ),
        (
            Operation::SignedMultiplyMemory {
                target: Register::Edx,
                source: Source::Register(Register::Edx),
                address: bp(16),
            },
            &[0x0f, 0xaf, 0x55, 16],
        ),
        (
            Operation::CompareMemory {
                test: false,
                width: Width::Dword,
                source: Source::Register(Register::Ecx),
                address: bp(-4),
            },
            &[0x39, 0x4d, 0xfc],
        ),
        (
            Operation::AluMemoryStore {
                kind: Alu::Add,
                width: Width::Dword,
                source: Source::Register(Register::Edx),
                address: bp(-4),
            },
            &[0x01, 0x55, 0xfc],
        ),
        (
            Operation::UnaryMemory {
                kind: MemoryUnary::Inc,
                width: Width::Word,
                address: bp(-2),
            },
            &[0x66, 0xff, 0x45, 0xfe],
        ),
        (
            Operation::Move {
                target: Register::Esp,
                source: Source::Register(Register::Ebp),
            },
            &[0x89, 0xec],
        ),
        (
            Operation::Pop {
                target: Register::Ebp,
            },
            &[0x5d],
        ),
    ];
    let (p, code) = program(pairs, Width::Dword, 8, true);
    for seed in [0_u32, 1, 0x7fff_ffff, 0x8000_0000, u32::MAX] {
        differential(&p, &code, initial(), descriptor(), true, |bytes| {
            let stack = RAM + (initial().gpr[4] - RAM_BASE) as usize;
            for (offset, value) in [(4, seed), (8, 0x89ab_cdef), (12, seed.wrapping_mul(37))] {
                bytes[stack + offset..stack + offset + 4].copy_from_slice(&value.to_le_bytes());
            }
        });
    }
}

#[test]
fn byte_high_word_dword_memory_source_address_aliases_and_lea_match_x86() {
    for (target, load, store) in [
        (Register::Al, &[0x8a, 0x03][..], &[0x88, 0x43, 1][..]),
        (Register::Ah, &[0x8a, 0x23][..], &[0x88, 0x63, 1][..]),
        (
            Register::Ax,
            &[0x66, 0x8b, 0x03][..],
            &[0x66, 0x89, 0x43, 1][..],
        ),
        (Register::Eax, &[0x8b, 0x03][..], &[0x89, 0x43, 1][..]),
    ] {
        let addr = address(Some(Register::Ebx), 0, Segment::Ds);
        let pairs = [
            (
                Operation::LoadMemory {
                    target,
                    address: addr,
                },
                load,
            ),
            (
                Operation::StoreMemory {
                    width: target.width(),
                    source: Source::Register(target),
                    address: MemoryAddress {
                        displacement: 1,
                        ..addr
                    },
                },
                store,
            ),
            (
                Operation::LoadMemory {
                    target: Register::Bl,
                    address: addr,
                },
                &[0x8a, 0x1b][..],
            ),
            (
                Operation::StoreMemory {
                    width: Width::Byte,
                    source: Source::Register(Register::Bh),
                    address: addr,
                },
                &[0x88, 0x3b][..],
            ),
            (
                Operation::ExtendMemory {
                    target: Register::Ebx,
                    width: Width::Byte,
                    signed: false,
                    address: addr,
                },
                &[0x0f, 0xb6, 0x1b][..],
            ),
            (
                Operation::Lea {
                    target: Register::Ebx,
                    address: MemoryAddress {
                        base: Some(Register::Ebx),
                        index: Some(Register::Eax),
                        scale: 4,
                        displacement: (-3_i32) as u32,
                        address16: false,
                        segment: Segment::Ds,
                    },
                },
                &[0x8d, 0x5c, 0x83, 0xfd][..],
            ),
        ];
        let (p, code) = program(&pairs, Width::Dword, 0, true);
        for offset in [0xffd, 0xffe, 0xfff, 0x1000] {
            let mut frame = initial();
            frame.gpr[3] = offset;
            differential(&p, &code, frame, descriptor(), true, |_| {});
        }
    }
}

fn prefix() -> [(Operation, &'static [u8]); 3] {
    [
        (
            Operation::Move {
                target: Register::Eax,
                source: Source::Immediate(0x1234_5678),
            },
            &[0xb8, 0x78, 0x56, 0x34, 0x12],
        ),
        (
            Operation::StoreMemory {
                width: Width::Dword,
                source: Source::Register(Register::Eax),
                address: ds(0x1300),
            },
            &[0xa3, 0, 0x13, 0, 0],
        ),
        (
            Operation::Move {
                target: Register::Ah,
                source: Source::Immediate(0x9a),
            },
            &[0xb4, 0x9a],
        ),
    ]
}

/// Independently execute only the prefix. The refused instruction must have no
/// architectural or RAM effects, while every prefix effect survives the exit.
fn refused_after_prefix(
    op: Option<Operation>,
    d: Descriptor,
    configure: impl FnOnce(&mut Runner),
    mut frame: Frame,
) {
    let mut pairs = prefix().to_vec();
    if let Some(op) = op {
        pairs.push((op, &[0x90]));
    }
    let (p, code) = program(&pairs, Width::Dword, 0, true);
    frame.gpr[3] = 0x2000;
    let mut runner = Runner::new(&p);
    runner.set(frame, &d);
    configure(&mut runner);
    let (mut cpu, mut bus) = reference(&code, true, d.stack_address, 0);
    bus.bytes[d.ram.linear_base as usize..d.ram.linear_base as usize + RAM_LENGTH]
        .copy_from_slice(&runner.bytes()[RAM..RAM + RAM_LENGTH]);
    cpu.flags = frame.eflags;
    cpu.registers.ip = ENTRY.offset;
    for (r, value) in REGS.into_iter().zip(frame.gpr) {
        cpu.registers.set32(r, value);
    }
    for _ in 0..prefix().len() {
        cpu.step(&mut bus).unwrap();
    }
    let before = runner.bytes().to_vec();
    assert_eq!(runner.call(), SIDE_EXIT, "{op:?}");
    assert_eq!(
        runner.frame(),
        Frame {
            gpr: core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
            eflags: cpu.flags,
            address: Address {
                offset: cpu.registers.ip,
                ..ENTRY
            },
            charged_work: frame.charged_work + prefix().len() as u64,
            ..frame
        },
        "{op:?}"
    );
    assert_eq!(
        &runner.bytes()[RAM..RAM + RAM_LENGTH],
        &bus.bytes[d.ram.linear_base as usize..d.ram.linear_base as usize + RAM_LENGTH],
        "{op:?}"
    );
    for page in 0..PAGES {
        let old = u64::from_le_bytes(before[GENERATIONS + page * 8..][..8].try_into().unwrap());
        assert_eq!(
            runner.generation(page),
            old + u64::from(page == 1),
            "{op:?} page {page}"
        );
    }
    assert_eq!(
        runner.effects(),
        memory::Effects {
            ram_writes: 1,
            inline_remaining: d.inline_grant,
            ..memory::Effects::default()
        }
    );
    // Control tables, padding and unrelated backing must remain byte-identical.
    for (offset, &old) in before.iter().enumerate() {
        if (FRAME..FRAME + 44).contains(&offset)
            || (FRAME + abi::CHARGED_WORK as usize..FRAME + abi::STATUS as usize + 4)
                .contains(&offset)
            || (FRAME + memory::OFFSET as usize + memory::RAM_WRITES as usize
                ..FRAME + memory::OFFSET as usize + memory::RAM_WRITES as usize + 8)
                .contains(&offset)
            || (RAM + (0x1300 - RAM_BASE) as usize..RAM + (0x1300 - RAM_BASE) as usize + 4)
                .contains(&offset)
            || (GENERATIONS + 8..GENERATIONS + 16).contains(&offset)
        {
            continue;
        }
        assert_eq!(
            runner.bytes()[offset],
            old,
            "unexpected write at {offset:#x} for {op:?}"
        );
    }
}

#[test]
fn every_memory_family_refuses_with_exact_committed_prefix_and_current_pc() {
    let addr = address(Some(Register::Ebx), 0, Segment::Ds);
    let operations = [
        Operation::LoadMemory {
            target: Register::Ah,
            address: addr,
        },
        Operation::LoadMemory {
            target: Register::Ax,
            address: addr,
        },
        Operation::LoadMemory {
            target: Register::Ebx,
            address: addr,
        },
        Operation::StoreMemory {
            width: Width::Dword,
            source: Source::Register(Register::Ebx),
            address: addr,
        },
        Operation::AluMemory {
            kind: Alu::Adc,
            target: Register::Eax,
            address: addr,
        },
        Operation::CompareMemory {
            test: false,
            width: Width::Dword,
            source: Source::Immediate(9),
            address: addr,
        },
        Operation::ExtendMemory {
            width: Width::Byte,
            target: Register::Ebx,
            signed: true,
            address: addr,
        },
        Operation::SignedMultiplyMemory {
            target: Register::Ebx,
            source: Source::Register(Register::Ebx),
            address: addr,
        },
        Operation::WideMultiplyMemory {
            width: Width::Dword,
            signed: true,
            address: addr,
        },
        Operation::AluMemoryStore {
            kind: Alu::Adc,
            width: Width::Dword,
            source: Source::Register(Register::Ebx),
            address: addr,
        },
        Operation::UnaryMemory {
            kind: MemoryUnary::Neg,
            width: Width::Dword,
            address: addr,
        },
    ];
    for op in operations {
        let mut d = descriptor();
        d.windows[0][Segment::Ds as usize].last = 0x1fff;
        d.windows[1][Segment::Ds as usize].last = 0x1fff;
        refused_after_prefix(Some(op), d, |_| {}, initial());
    }
    let mut d = descriptor();
    d.stack_address = StackAddress::Disabled;
    refused_after_prefix(
        Some(Operation::Push {
            width: Width::Dword,
            source: Source::Register(Register::Esp),
        }),
        d,
        |_| {},
        initial(),
    );
    let mut frame = initial();
    frame.gpr[4] = RAM_BASE + RAM_LENGTH as u32 - 1;
    refused_after_prefix(
        Some(Operation::Pop {
            target: Register::Esp,
        }),
        descriptor(),
        |_| {},
        frame,
    );
    refused_after_prefix(None, descriptor(), |_| {}, frame);
}

#[test]
fn crossed_span_protected_source_and_generation_exhaustion_refuse_after_prior_store() {
    let op = Operation::StoreMemory {
        width: Width::Dword,
        source: Source::Register(Register::Eax),
        address: ds(0x1fff),
    };
    let mut d = descriptor();
    d.ram.excluded_start = 0x2000;
    d.ram.excluded_length = 1;
    refused_after_prefix(Some(op), d, |_| {}, initial());
    refused_after_prefix(
        Some(op),
        descriptor(),
        |r| r.bytes()[PROTECTED + 2] = 1,
        initial(),
    );
    refused_after_prefix(
        Some(op),
        descriptor(),
        |r| r.set_generation(2, u64::MAX),
        initial(),
    );
    // Both page guards are checked before the first byte or generation changes.
    refused_after_prefix(
        Some(op),
        descriptor(),
        |r| r.set_generation(1, u64::MAX - 1),
        initial(),
    );
}

#[test]
fn malformed_backing_tables_and_overlapping_control_buffers_decline_before_store() {
    let (p, _) = program(
        &[(
            Operation::StoreMemory {
                width: Width::Dword,
                source: Source::Register(Register::Eax),
                address: ds(0x1300),
            },
            &[0x90],
        )],
        Width::Dword,
        0,
        true,
    );
    for (offset, value) in [
        (memory::RAM_POINTER, FRAME as u32),
        (memory::RAM_POINTER, GENERATIONS as u32),
        (memory::GENERATIONS_POINTER, RAM as u32),
        (memory::CODE_PAGES_POINTER, RAM as u32),
        (memory::CODE_PAGES_POINTER, GENERATIONS as u32),
        (memory::GENERATIONS_POINTER, FRAME as u32),
        (memory::CODE_PAGES_POINTER, FRAME as u32),
        (memory::GENERATIONS_POINTER, u32::MAX - 3),
        (memory::CODE_PAGES_POINTER, u32::MAX),
        (memory::GENERATIONS_LENGTH, u32::MAX),
        (memory::CODE_PAGES_LENGTH, u32::MAX),
    ] {
        let mut runner = Runner::new(&p);
        let frame = initial();
        runner.set(frame, &descriptor());
        runner.word(offset, value);
        let before = runner.bytes().to_vec();
        assert_eq!(runner.call(), SIDE_EXIT, "offset={offset} value={value}");
        assert_eq!(runner.frame(), frame);
        assert_eq!(runner.bytes(), before, "offset={offset} value={value}");
    }
}

#[test]
fn stack_address_width_operand_width_and_unsigned_ret_adjustment_match_x86() {
    for code32 in [false, true] {
        for stack in [StackAddress::Bits16, StackAddress::Bits32] {
            for width in [Width::Word, Width::Dword] {
                let register = if width == Width::Word {
                    Register::Sp
                } else {
                    Register::Esp
                };
                let pop_register = if width == Width::Word {
                    Register::Ax
                } else {
                    Register::Eax
                };
                let mut push = vec![];
                let mut pop = vec![];
                if (width == Width::Dword) != code32 {
                    push.push(0x66);
                    pop.push(0x66);
                }
                push.push(0x54);
                pop.push(0x58);
                let (p, code) = program(
                    &[
                        (
                            Operation::Push {
                                width,
                                source: Source::Register(register),
                            },
                            &push,
                        ),
                        (
                            Operation::Pop {
                                target: pop_register,
                            },
                            &pop,
                        ),
                    ],
                    width,
                    u16::MAX,
                    code32,
                );
                let mut frame = initial();
                if stack == StackAddress::Bits16 {
                    frame.gpr[4] |= 0xdead_0000;
                }
                let mut d = descriptor();
                d.stack_address = stack;
                differential(&p, &code, frame, d, code32, |_| {});
            }
        }
    }
}

#[test]
fn limits_validation_output_determinism_and_work_metadata() {
    let (p, _) = program(&prefix(), Width::Dword, 0, true);
    let compiled = p.compile(&ProcedureCompileOptions::default()).unwrap();
    assert_eq!(compiled.maximum_work(ProcedureId(3)), Some(4));
    assert_eq!(compiled.entry(ProcedureId(3)), Some(ENTRY));
    assert_eq!(
        compiled.procedure_ids().collect::<Vec<_>>(),
        vec![ProcedureId(3)]
    );
    assert_eq!(compiled.export_name(ProcedureId(4)), None);
    assert_eq!(
        p.compile(&ProcedureCompileOptions {
            max_procedures: 0,
            ..ProcedureCompileOptions::default()
        })
        .unwrap_err(),
        ProcedureCompileError::TooManyProcedures
    );
    assert_eq!(
        p.compile(&ProcedureCompileOptions {
            max_procedure_operations: 2,
            ..ProcedureCompileOptions::default()
        })
        .unwrap_err(),
        ProcedureCompileError::ProcedureTooLong(ProcedureId(3))
    );
    assert_eq!(
        p.compile(&ProcedureCompileOptions {
            max_total_operations: 2,
            ..ProcedureCompileOptions::default()
        })
        .unwrap_err(),
        ProcedureCompileError::TooManyOperations
    );
    assert_eq!(
        p.compile(&ProcedureCompileOptions {
            max_output_bytes: 8,
            ..ProcedureCompileOptions::default()
        })
        .unwrap_err(),
        ProcedureCompileError::OutputTooLarge
    );
    assert_eq!(
        p.compile(&ProcedureCompileOptions {
            max_total_operations: MAX_TOTAL_OPERATIONS + 1,
            ..ProcedureCompileOptions::default()
        })
        .unwrap_err(),
        ProcedureCompileError::InvalidOptions
    );
    let mut invalid = p.clone();
    invalid.functions[0].instructions[1].address = ENTRY;
    assert_eq!(
        invalid
            .compile(&ProcedureCompileOptions::default())
            .unwrap_err(),
        ProcedureCompileError::InvalidAddresses(ProcedureId(3))
    );
    let mut invalid = p.clone();
    invalid.functions[0].instructions[0].operation = Operation::Move {
        target: Register::Al,
        source: Source::Register(Register::Ax),
    };
    assert!(matches!(
        invalid.compile(&ProcedureCompileOptions::default()),
        Err(ProcedureCompileError::InvalidOperation {
            operation: 0,
            reason: CompileError::WidthMismatch(_),
            ..
        })
    ));
    let mut invalid = p.clone();
    invalid.functions[0].terminator.width = Width::Byte;
    assert_eq!(
        invalid
            .compile(&ProcedureCompileOptions::default())
            .unwrap_err(),
        ProcedureCompileError::InvalidReturn(ProcedureId(3))
    );
    let mut duplicate = p.clone();
    duplicate.functions.push(p.functions[0].clone());
    assert_eq!(
        duplicate
            .compile(&ProcedureCompileOptions::default())
            .unwrap_err(),
        ProcedureCompileError::DuplicateId(ProcedureId(3))
    );
    let mut shuffled = p.clone();
    let mut other = p.functions[0].clone();
    other.id = ProcedureId(1);
    shuffled.functions.push(other);
    let first = shuffled
        .compile(&ProcedureCompileOptions::default())
        .unwrap();
    shuffled.functions.reverse();
    assert_eq!(
        first.wasm(),
        shuffled
            .compile(&ProcedureCompileOptions::default())
            .unwrap()
            .wasm()
    );
}

#[test]
fn execution_allocates_nothing_and_preserves_saturating_work_contract() {
    let (p, _) = program(&prefix(), Width::Dword, 0, true);
    let mut runner = Runner::new(&p);
    let mut frame = initial();
    frame.charged_work = u64::MAX - 1;
    runner.set(frame, &descriptor());
    let stack = RAM + (frame.gpr[4] - RAM_BASE) as usize;
    runner.bytes()[stack..stack + 4].copy_from_slice(&0x4321_u32.to_le_bytes());
    // Wasmi lazily prepares its engine stacks on first invocation. Warm it
    // before measuring the repeatable generated execution boundary.
    assert_eq!(runner.call(), COMPLETE);
    runner.set(frame, &descriptor());
    runner.bytes()[stack..stack + 4].copy_from_slice(&0x4321_u32.to_le_bytes());
    let measured = allocation_counter::measure(|| assert_eq!(runner.call(), COMPLETE));
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
    assert_eq!(runner.frame().charged_work, u64::MAX);
    assert_eq!(runner.frame().status, frame.status | abi::COUNTER_OVERFLOW);
    assert_eq!(runner.frame().remaining_budget, frame.remaining_budget);
}
