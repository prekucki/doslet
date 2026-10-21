extern crate std;

use super::*;
use alloc::{vec, vec::Vec};
use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_x86::{Cpu, DescriptorTable, Gpr32, SegmentRegister};
use wasmi::{Caller, Config, Engine, Linker, Memory, MemoryType, Module, Store, TypedFunc};

#[path = "alias_tests.rs"]
mod alias_tests;
#[path = "dispatcher_tests.rs"]
mod dispatcher_tests;
#[path = "extend_tests.rs"]
mod extend_tests;
#[path = "lea_tests.rs"]
mod lea_tests;
#[path = "multiply_tests.rs"]
mod multiply_tests;
#[path = "procedure_tests.rs"]
mod procedure_tests;
#[path = "shift_tests.rs"]
mod shift_tests;
#[path = "wide_multiply_tests.rs"]
mod wide_multiply_tests;

const PTR: usize = 256;
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

#[derive(Default)]
struct State {
    events: Vec<(u32, Frame)>,
    stop_after: usize,
    redirect: Option<Frame>,
    admissions: Vec<(u32, u32)>,
    horizon: Option<u32>,
    refill_horizon: Option<u32>,
}
struct Runner {
    store: Store<State>,
    memory: Memory,
    run: TypedFunc<u32, u32>,
}
impl Runner {
    fn new(program: &Program, options: CompileOptions) -> Self {
        Self::with_aliases(program, options, &[])
    }
    fn with_aliases(program: &Program, options: CompileOptions, aliases: &[EntryAlias]) -> Self {
        let compiled = program.compile_with_aliases(&options, aliases).unwrap();
        Self::with_compiled(&compiled)
    }
    fn with_compiled(compiled: &CompiledProgram) -> Self {
        let mut config = Config::default();
        config.wasm_tail_call(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, compiled.wasm()).unwrap();
        let mut store = Store::new(
            &engine,
            State {
                stop_after: 1,
                ..State::default()
            },
        );
        let memory = Memory::new(&mut store, MemoryType::new(1, Some(1))).unwrap();
        memory.data_mut(&mut store).fill(0x5a);
        let mut linker = Linker::new(&engine);
        linker
            .define(abi::IMPORT_MODULE, abi::MEMORY_IMPORT, memory)
            .unwrap();
        linker
            .func_wrap(
                abi::IMPORT_MODULE,
                abi::RUNTIME_IMPORT,
                |mut caller: Caller<'_, State>, ptr: u32, reason: u32| -> u32 {
                    let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                    let (data, state) = memory.data_and_store_mut(&mut caller);
                    let bytes = &mut data[ptr as usize..ptr as usize + abi::FRAME_BYTES];
                    let mut frame = Frame::read(bytes).unwrap();
                    state.events.push((reason, frame));
                    if let Some(replacement) = state.redirect.take() {
                        frame = replacement;
                        frame.write(bytes).unwrap();
                        state.horizon = state.refill_horizon.take();
                    }
                    if state.events.len() >= state.stop_after {
                        abi::EXIT
                    } else {
                        abi::CONTINUE
                    }
                },
            )
            .unwrap();
        linker
            .func_wrap(
                abi::IMPORT_MODULE,
                abi::ADMIT_IMPORT,
                |mut caller: Caller<'_, State>, ptr: u32, id: u32, required: u32| -> u32 {
                    assert_eq!(ptr, PTR as u32);
                    let state = caller.data_mut();
                    state.admissions.push((id, required));
                    if let Some(horizon) = &mut state.horizon {
                        if *horizon < required {
                            return 0;
                        }
                        *horizon -= required;
                    }
                    1
                },
            )
            .unwrap();
        let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
        let run = instance
            .get_typed_func::<u32, u32>(&store, abi::RUN_EXPORT)
            .unwrap();
        Self { store, memory, run }
    }
    fn set(&mut self, frame: Frame) {
        frame
            .write(&mut self.memory.data_mut(&mut self.store)[PTR..PTR + abi::FRAME_BYTES])
            .unwrap();
    }
    fn frame(&self) -> Frame {
        Frame::read(&self.memory.data(&self.store)[PTR..]).unwrap()
    }
    fn call(&mut self) -> u32 {
        self.run.call(&mut self.store, PTR as u32).unwrap()
    }
}
fn address(space: u32, offset: u32) -> Address {
    Address { space, offset }
}
fn region(id: u32, operations: Vec<Operation>, terminator: Terminator) -> Region {
    Region {
        id: RegionId(id),
        entry: address(3, id * 0x10),
        operations,
        terminator,
        accounting: Accounting::Counted,
    }
}
fn single(op: Operation) -> Program {
    Program {
        regions: vec![region(7, vec![op], Terminator::Fallthrough(RegionId(7)))],
    }
}

fn admission_options() -> CompileOptions {
    CompileOptions {
        runtime_admission: true,
        ..CompileOptions::default()
    }
}

#[test]
fn admission_refusal_precedes_all_effects_and_uses_the_current_entry() {
    let program = Program {
        regions: vec![
            region(
                1,
                vec![Operation::Inc(Register::Eax), Operation::Inc(Register::Ebx)],
                Terminator::Branch {
                    condition: Condition::Equal,
                    taken: RegionId(2),
                    not_taken: RegionId(3),
                },
            ),
            region(2, vec![], Terminator::Fallback(address(9, 0))),
            region(3, vec![], Terminator::Fallback(address(9, 1))),
        ],
    };
    let mut runner = Runner::new(&program, admission_options());
    runner.store.data_mut().horizon = Some(0);
    let initial = Frame {
        gpr: [11, 22, 33, 44, 55, 66, 77, 88],
        eflags: 0xa55a_0247,
        address: program.regions[0].entry,
        remaining_budget: 101,
        charged_work: 37,
        status: 0x8000_0000,
    };
    runner.set(initial);
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.frame(), initial);
    assert_eq!(runner.store.data().admissions, vec![(1, 3)]);
    assert_eq!(
        runner.store.data().events,
        vec![(abi::REASON_FALLBACK, initial)]
    );
}

#[test]
fn refusal_after_a_direct_edge_flushes_only_completed_work_and_redispatches() {
    let program = Program {
        regions: vec![
            region(
                1,
                vec![Operation::Inc(Register::Eax)],
                Terminator::Jump(RegionId(2)),
            ),
            region(
                2,
                vec![Operation::Inc(Register::Ebx)],
                Terminator::Jump(RegionId(2)),
            ),
            region(
                3,
                vec![Operation::Inc(Register::Ecx)],
                Terminator::Fallback(address(8, 90)),
            ),
        ],
    };
    let mut runner = Runner::new(&program, admission_options());
    let initial = Frame {
        gpr: [5, 10, 15, 20, 25, 30, 35, 40],
        address: program.regions[0].entry,
        remaining_budget: 100,
        charged_work: 50,
        ..Frame::default()
    };
    let state = runner.store.data_mut();
    state.horizon = Some(2);
    state.stop_after = 2;
    // Model the fallback interpreting one instruction and selecting a new entry.
    state.redirect = Some(Frame {
        gpr: [99, 100, 15, 20, 25, 30, 35, 40],
        address: program.regions[2].entry,
        remaining_budget: 97,
        charged_work: 53,
        ..Frame::default()
    });
    runner.set(initial);
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    let state = runner.store.data();
    let refused = state.events[0];
    assert_eq!(refused.0, abi::REASON_FALLBACK);
    assert_eq!(refused.1.address, program.regions[1].entry);
    assert_eq!(refused.1.gpr, [6, 10, 15, 20, 25, 30, 35, 40]);
    assert_eq!(
        (refused.1.charged_work, refused.1.remaining_budget),
        (52, 98)
    );
    assert_eq!(state.admissions, vec![(1, 2), (2, 2), (3, 1)]);
    assert_eq!(runner.frame().gpr, [99, 101, 15, 20, 25, 30, 35, 40]);
    assert_eq!(
        (runner.frame().charged_work, runner.frame().remaining_budget),
        (54, 96)
    );
    assert_eq!(runner.frame().address, address(8, 90));
}

#[test]
fn hard_admission_horizon_is_independent_of_soft_budget_overshoot() {
    let program = Program {
        regions: vec![region(
            1,
            vec![Operation::Inc(Register::Eax), Operation::Nop],
            Terminator::Jump(RegionId(1)),
        )],
    };
    let mut runner = Runner::new(&program, admission_options());
    runner.store.data_mut().horizon = Some(5);
    runner.set(Frame {
        address: program.regions[0].entry,
        remaining_budget: 100,
        ..Frame::default()
    });
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.frame().gpr[0], 1);
    assert_eq!(runner.frame().charged_work, 3);
    assert_eq!(runner.store.data().horizon, Some(2));
    assert_eq!(runner.store.data().admissions, vec![(1, 3), (1, 3)]);

    let mut runner = Runner::new(&program, admission_options());
    runner.store.data_mut().horizon = Some(5);
    runner.set(Frame {
        address: program.regions[0].entry,
        remaining_budget: 1,
        ..Frame::default()
    });
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    assert_eq!(
        (runner.frame().charged_work, runner.frame().remaining_budget),
        (3, 0)
    );
    assert_eq!(runner.store.data().admissions, vec![(1, 3)]);
    assert!(runner.store.data().events.is_empty());
}

#[test]
fn admission_keeps_structural_service_bounds_in_work_and_zero_work_cycles() {
    for operations in [vec![Operation::Nop], vec![]] {
        let program = Program {
            regions: vec![region(1, operations, Terminator::Fallthrough(RegionId(1)))],
        };
        let mut runner = Runner::new(
            &program,
            CompileOptions {
                service_interval_regions: 3,
                ..admission_options()
            },
        );
        runner.set(Frame {
            address: program.regions[0].entry,
            remaining_budget: 100,
            ..Frame::default()
        });
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        assert_eq!(runner.store.data().events.len(), 1);
        assert_eq!(runner.store.data().events[0].0, abi::REASON_SERVICE);
        let count = program.regions[0].completed_instructions() as u64;
        assert_eq!(runner.frame().charged_work, 3 * count);
        assert_eq!(runner.store.data().admissions.len(), 3 * count as usize);

        let exhausted = Frame {
            address: program.regions[0].entry,
            ..Frame::default()
        };
        runner.set(exhausted);
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        assert_eq!(runner.frame(), exhausted);
        assert_eq!(runner.store.data().events.len(), 1);
        assert_eq!(runner.store.data().admissions.len(), 3 * count as usize);
    }

    let stub = Program {
        regions: vec![region(1, vec![], Terminator::Fallback(address(8, 90)))],
    };
    let mut runner = Runner::new(&stub, admission_options());
    runner.store.data_mut().horizon = Some(0);
    runner.set(Frame {
        address: stub.regions[0].entry,
        remaining_budget: 100,
        ..Frame::default()
    });
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert!(runner.store.data().admissions.is_empty());
    assert_eq!(runner.store.data().events[0].0, abi::REASON_FALLBACK);
    assert_eq!(runner.frame().address, address(8, 90));
    assert_eq!(runner.frame().charged_work, 0);
}

#[test]
fn admission_rejects_uncounted_hot_even_without_work() {
    for operations in [vec![Operation::Nop], vec![]] {
        let mut program = Program {
            regions: vec![region(1, operations, Terminator::Fallthrough(RegionId(1)))],
        };
        program.regions[0].accounting = Accounting::UncountedHot;
        assert!(program.compile(&CompileOptions::default()).is_ok());
        assert_eq!(
            program.compile(&admission_options()).unwrap_err(),
            CompileError::AdmissionUncountedHot(RegionId(1))
        );
    }
}

struct Bus(Vec<u8>);
impl MemoryBus for Bus {
    fn read_u8(&mut self, a: u32) -> Result<u8, BusError> {
        self.0
            .get(a as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(a))
    }
    fn write_u8(&mut self, a: u32, value: u8) -> Result<(), BusError> {
        *self
            .0
            .get_mut(a as usize)
            .ok_or(BusError::UnmappedMemory(a))? = value;
        Ok(())
    }
    fn code_bytes(&self, a: u32, len: u8) -> Option<&[u8]> {
        self.0.get(a as usize..a as usize + len as usize)
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
fn reference(code: &[u8]) -> (Cpu, Bus) {
    let mut bus = Bus(vec![0; 65536]);
    bus.0[0x1000..0x1000 + code.len()].copy_from_slice(code);
    bus.0[0x208..0x210].copy_from_slice(&[0xff, 0xff, 0, 0, 0, 0x9a, 0xcf, 0]);
    let mut cpu = Cpu::default();
    cpu.set_decode_cache(false);
    cpu.set_dynamic_core(false);
    cpu.control.gdtr = DescriptorTable {
        base: 0x200,
        limit: 15,
    };
    cpu.set_protected_mode(true);
    cpu.load_segment(&mut bus, SegmentRegister::Cs, 8).unwrap();
    (cpu, bus)
}
fn register_code(r: Register) -> u8 {
    (r.index() + if r.shift() == 8 { 4 } else { 0 }) as u8
}
fn encoding(op: Operation) -> Vec<u8> {
    let r = match op {
        Operation::Move { target, .. } | Operation::Alu { target, .. } => target,
        Operation::Inc(r) | Operation::Dec(r) | Operation::Neg(r) | Operation::Not(r) => r,
        Operation::Exchange { left, .. } => left,
        Operation::Shift { .. } | Operation::DoubleShift { .. } => {
            return shift_tests::encoding(op);
        }
        Operation::Nop => return vec![0x90],
        Operation::Lea { .. }
        | Operation::Extend { .. }
        | Operation::SignedMultiply { .. }
        | Operation::SignedMultiplyMemory { .. }
        | Operation::WideMultiply { .. }
        | Operation::WideMultiplyMemory { .. }
        | Operation::ExtendMemory { .. }
        | Operation::LoadMemory { .. }
        | Operation::Push { .. }
        | Operation::Pop { .. }
        | Operation::StoreMemory { .. }
        | Operation::StringLoad { .. }
        | Operation::StringStore { .. }
        | Operation::AluMemory { .. }
        | Operation::AluMemoryStore { .. }
        | Operation::UnaryMemory { .. }
        | Operation::CompareMemory { .. } => {
            unreachable!("register-only fixture encoder")
        }
    };
    let width = r.width();
    let mut out = Vec::new();
    if width == Width::Word {
        out.push(0x66);
    }
    let word = u8::from(width != Width::Byte);
    let immediate = |out: &mut Vec<u8>, value: u32| {
        out.extend_from_slice(&value.to_le_bytes()[..width.bits() as usize / 8]);
    };
    match op {
        Operation::Move {
            target,
            source: Source::Register(s),
        } => out.extend_from_slice(&[
            0x88 + word,
            0xc0 | register_code(s) << 3 | register_code(target),
        ]),
        Operation::Move {
            target,
            source: Source::Immediate(v),
        } => {
            out.push(if word == 0 { 0xb0 } else { 0xb8 } + register_code(target));
            immediate(&mut out, v);
        }
        Operation::Move {
            source: Source::VirtualRegister(_),
            ..
        } => unreachable!("test source"),
        Operation::Alu {
            kind,
            target,
            source,
        } => {
            let group = match kind {
                Alu::Add => 0,
                Alu::Or => 1,
                Alu::Adc => 2,
                Alu::Sbb => 3,
                Alu::And => 4,
                Alu::Sub => 5,
                Alu::Xor => 6,
                Alu::Cmp => 7,
                Alu::Test => 0,
            };
            match source {
                Source::Register(s) => out.extend_from_slice(&[
                    if kind == Alu::Test {
                        0x84 + word
                    } else {
                        group * 8 + word
                    },
                    0xc0 | register_code(s) << 3 | register_code(target),
                ]),
                Source::Immediate(v) => {
                    out.extend_from_slice(&[
                        if kind == Alu::Test {
                            0xf6 + word
                        } else {
                            0x80 + word
                        },
                        0xc0 | group << 3 | register_code(target),
                    ]);
                    immediate(&mut out, v);
                }
                Source::VirtualRegister(_) => unreachable!("test source"),
            }
        }
        Operation::Inc(r) | Operation::Dec(r) => out.extend_from_slice(&[
            0xfe + word,
            0xc0 | u8::from(matches!(op, Operation::Dec(_))) << 3 | register_code(r),
        ]),
        Operation::Neg(r) | Operation::Not(r) => out.extend_from_slice(&[
            0xf6 + word,
            0xc0 | if matches!(op, Operation::Neg(_)) {
                3 << 3
            } else {
                2 << 3
            } | register_code(r),
        ]),
        Operation::Exchange { left, right } => out.extend_from_slice(&[
            0x86 + word,
            0xc0 | register_code(right) << 3 | register_code(left),
        ]),
        Operation::Nop | Operation::Shift { .. } | Operation::DoubleShift { .. } => unreachable!(),
        Operation::Lea { .. }
        | Operation::Extend { .. }
        | Operation::SignedMultiply { .. }
        | Operation::SignedMultiplyMemory { .. }
        | Operation::WideMultiply { .. }
        | Operation::WideMultiplyMemory { .. }
        | Operation::ExtendMemory { .. }
        | Operation::LoadMemory { .. }
        | Operation::Push { .. }
        | Operation::Pop { .. }
        | Operation::StoreMemory { .. }
        | Operation::StringLoad { .. }
        | Operation::StringStore { .. }
        | Operation::AluMemory { .. }
        | Operation::AluMemoryStore { .. }
        | Operation::UnaryMemory { .. }
        | Operation::CompareMemory { .. } => {
            unreachable!("register-only fixture encoder")
        }
    }
    out
}
fn differential(op: Operation, cases: usize) {
    let program = single(op);
    let mut runner = Runner::new(&program, CompileOptions::default());
    let code = encoding(op);
    let (mut cpu, mut bus) = reference(&code);
    let boundary = [
        0,
        1,
        0xf,
        0x10,
        0x7f,
        0x80,
        0xff,
        0x7fff,
        0x8000,
        0xffff,
        0x7fff_ffff,
        0x8000_0000,
        u32::MAX,
        u32::MAX - 1,
    ];
    let mut seed = 0xf00d_cafeu32;
    for case in 0..cases {
        let mut gpr = [0; 8];
        for r in &mut gpr {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            *r = seed;
        }
        if case < boundary.len() * boundary.len() {
            let target = match op {
                Operation::Alu { target, .. }
                | Operation::Move { target, .. }
                | Operation::Shift { target, .. }
                | Operation::DoubleShift { target, .. } => target,
                Operation::Inc(r) | Operation::Dec(r) | Operation::Neg(r) | Operation::Not(r) => r,
                Operation::Exchange { left, .. } => left,
                Operation::Nop => Register::Eax,
                Operation::Lea { .. }
                | Operation::Extend { .. }
                | Operation::SignedMultiply { .. }
                | Operation::SignedMultiplyMemory { .. }
                | Operation::WideMultiply { .. }
                | Operation::WideMultiplyMemory { .. }
                | Operation::ExtendMemory { .. }
                | Operation::LoadMemory { .. }
                | Operation::Push { .. }
                | Operation::Pop { .. }
                | Operation::StoreMemory { .. }
                | Operation::StringLoad { .. }
                | Operation::StringStore { .. }
                | Operation::AluMemory { .. }
                | Operation::AluMemoryStore { .. }
                | Operation::UnaryMemory { .. }
                | Operation::CompareMemory { .. } => {
                    unreachable!("register-only differential test")
                }
            };
            let source = match op {
                Operation::Alu {
                    source: Source::Register(r),
                    ..
                }
                | Operation::Move {
                    source: Source::Register(r),
                    ..
                } => Some(r),
                Operation::Exchange { right, .. } => Some(right),
                _ => None,
            };
            let merge = |old: u32, r: Register, value: u32| {
                (old & !(r.width().mask() << r.shift())) | ((value & r.width().mask()) << r.shift())
            };
            let i = target.index() as usize;
            gpr[i] = merge(gpr[i], target, boundary[case / boundary.len()]);
            if let Some(source) = source {
                let i = source.index() as usize;
                gpr[i] = merge(gpr[i], source, boundary[case % boundary.len()]);
            }
        }
        for carry in 0..=1 {
            // Preserve nonarithmetic flags, including a reserved high bit.
            let flags = 0x2000_06d4 | carry | if case % 3 == 0 { 0 } else { 2 };
            let frame = Frame {
                gpr,
                eflags: flags,
                address: program.regions[0].entry,
                remaining_budget: 1,
                ..Frame::default()
            };
            runner.set(frame);
            for (r, value) in REGS.into_iter().zip(gpr) {
                cpu.registers.set32(r, value);
            }
            cpu.flags = flags;
            cpu.registers.ip = 0x1000;
            cpu.step(&mut bus).unwrap();
            assert_eq!(runner.call(), abi::EXIT_BUDGET, "{op:?}");
            let actual = runner.frame();
            assert_eq!(
                actual.gpr,
                core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
                "{op:?} case={case} CF={carry}"
            );
            assert_eq!(actual.eflags, cpu.flags, "{op:?} case={case} CF={carry}");
            assert_eq!(actual.charged_work, 1);
            assert_eq!(actual.exact_total(), Some(1));
        }
    }
    assert!(runner.store.data().events.is_empty());
}

#[test]
fn alu_all_widths_match_interpreter_boundary_pairs_and_random_inputs() {
    for (target, source) in [
        (Register::Al, Register::Bl),
        (Register::Ah, Register::Ch),
        (Register::Ax, Register::Bx),
        (Register::Eax, Register::Ebx),
    ] {
        for kind in [
            Alu::Add,
            Alu::Adc,
            Alu::Sub,
            Alu::Sbb,
            Alu::And,
            Alu::Or,
            Alu::Xor,
            Alu::Cmp,
            Alu::Test,
        ] {
            differential(
                Operation::Alu {
                    kind,
                    target,
                    source: Source::Register(source),
                },
                260,
            );
        }
    }
}

#[test]
fn mixed_alias_sequences_cross_direct_region_edges_like_interpretation() {
    let aliases = [
        Register::Eax,
        Register::Ecx,
        Register::Edx,
        Register::Ebx,
        Register::Esp,
        Register::Ebp,
        Register::Esi,
        Register::Edi,
        Register::Ax,
        Register::Cx,
        Register::Dx,
        Register::Bx,
        Register::Sp,
        Register::Bp,
        Register::Si,
        Register::Di,
        Register::Al,
        Register::Cl,
        Register::Dl,
        Register::Bl,
        Register::Ah,
        Register::Ch,
        Register::Dh,
        Register::Bh,
    ];
    let kinds = [
        Alu::Add,
        Alu::Adc,
        Alu::Sub,
        Alu::Sbb,
        Alu::And,
        Alu::Or,
        Alu::Xor,
        Alu::Cmp,
        Alu::Test,
    ];
    let mut seed = 0x81d3_7c21u32;
    for _ in 0..24 {
        let mut random = || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed
        };
        let initial = Frame {
            gpr: core::array::from_fn(|_| random()),
            address: address(0, 0),
            eflags: random() & 0x8d7,
            remaining_budget: 128,
            ..Frame::default()
        };
        let mut program = Program::default();
        let mut code = Vec::new();
        for i in 0..16 {
            let mut operations = Vec::new();
            for _ in 0..8 {
                let target = aliases[random() as usize % aliases.len()];
                let base = match target.width() {
                    Width::Dword => 0,
                    Width::Word => 8,
                    Width::Byte => 16,
                };
                let other = aliases[base + random() as usize % 8];
                let source = if random() & 1 == 0 {
                    Source::Register(other)
                } else {
                    Source::Immediate(random() & target.width().mask())
                };
                let op = match random() % 8 {
                    0 => Operation::Nop,
                    1 => Operation::Move { target, source },
                    2 => Operation::Inc(target),
                    3 => Operation::Dec(target),
                    4 => Operation::Neg(target),
                    5 => Operation::Not(target),
                    6 => Operation::Exchange {
                        left: target,
                        right: other,
                    },
                    _ => Operation::Alu {
                        kind: kinds[random() as usize % kinds.len()],
                        target,
                        source,
                    },
                };
                code.extend_from_slice(&encoding(op));
                operations.push(op);
            }
            program.regions.push(Region {
                id: RegionId(1000 - i),
                entry: address(0, i * 8),
                operations,
                terminator: Terminator::Fallthrough(RegionId(if i == 15 { 1000 } else { 999 - i })),
                accounting: Accounting::Counted,
            });
        }
        let mut runner = Runner::new(&program, CompileOptions::default());
        runner.set(initial);
        let (mut cpu, mut bus) = reference(&code);
        cpu.registers.ip = 0x1000;
        cpu.flags = initial.eflags;
        for (r, value) in REGS.into_iter().zip(initial.gpr) {
            cpu.registers.set32(r, value);
        }
        for _ in 0..128 {
            cpu.step(&mut bus).unwrap();
        }
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        let actual = runner.frame();
        assert_eq!(
            actual.gpr,
            core::array::from_fn(|i| cpu.registers.get32(REGS[i]))
        );
        assert_eq!(actual.eflags, cpu.flags);
        assert_eq!(actual.charged_work, 128);
        assert_eq!(actual.address, address(0, 0));
        assert!(runner.store.data().events.is_empty());
    }
}

#[test]
fn immediates_unary_moves_and_exchanges_match_interpreter() {
    for (r, s) in [
        (Register::Al, Register::Ah),
        (Register::Ah, Register::Al),
        (Register::Cl, Register::Ch),
        (Register::Dl, Register::Dh),
        (Register::Bl, Register::Bh),
        (Register::Ax, Register::Bx),
        (Register::Sp, Register::Di),
        (Register::Bp, Register::Si),
        (Register::Eax, Register::Ebx),
        (Register::Esp, Register::Edi),
        (Register::Ebp, Register::Esi),
    ] {
        for op in [
            Operation::Move {
                target: r,
                source: Source::Register(s),
            },
            Operation::Move {
                target: r,
                source: Source::Immediate(r.width().mask()),
            },
            Operation::Exchange { left: r, right: s },
            Operation::Exchange { left: r, right: r },
            Operation::Inc(r),
            Operation::Dec(r),
            Operation::Neg(r),
            Operation::Not(r),
        ] {
            differential(op, 260);
        }
        for kind in [
            Alu::Add,
            Alu::Adc,
            Alu::Sub,
            Alu::Sbb,
            Alu::And,
            Alu::Or,
            Alu::Xor,
            Alu::Cmp,
            Alu::Test,
        ] {
            differential(
                Operation::Alu {
                    kind,
                    target: r,
                    source: Source::Immediate(r.width().mask()),
                },
                260,
            );
        }
    }
    differential(Operation::Nop, 1);
}

#[test]
fn direct_self_and_cross_region_loops_charge_branch_and_allow_soft_overshoot() {
    let program = Program {
        regions: vec![
            region(
                9,
                vec![Operation::Inc(Register::Ebx)],
                Terminator::Jump(RegionId(7)),
            ),
            region(
                7,
                vec![Operation::Inc(Register::Eax), Operation::Nop],
                Terminator::Jump(RegionId(9)),
            ),
        ],
    };
    let mut runner = Runner::new(&program, CompileOptions::default());
    runner.set(Frame {
        address: address(3, 0x70),
        remaining_budget: 11,
        ..Frame::default()
    });
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    let frame = runner.frame();
    assert_eq!(frame.gpr[0], 3);
    assert_eq!(frame.gpr[3], 2);
    assert_eq!(frame.charged_work, 13);
    assert_eq!(frame.remaining_budget, 0);
    assert_eq!(frame.address, address(3, 0x90));
    assert!(runner.store.data().events.is_empty());
    // Many tail transfers would overflow a normal call stack.
    let mut runner = Runner::new(
        &single(Operation::Inc(Register::Eax)),
        CompileOptions {
            service_interval_regions: 4096,
            ..CompileOptions::default()
        },
    );
    runner.store.data_mut().stop_after = usize::MAX;
    runner.set(Frame {
        address: address(3, 0x70),
        remaining_budget: 200_000,
        ..Frame::default()
    });
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    assert_eq!(runner.frame().gpr[0], 200_000);
    assert_eq!(runner.store.data().events.len(), 48);
}

#[test]
fn conditional_backedge_and_mixed_hot_counted_edges_keep_their_policies() {
    let program = Program {
        regions: vec![
            region(
                1,
                vec![Operation::Dec(Register::Ecx)],
                Terminator::Branch {
                    condition: Condition::NotEqual,
                    taken: RegionId(1),
                    not_taken: RegionId(2),
                },
            ),
            region(
                2,
                vec![Operation::Inc(Register::Ebx)],
                Terminator::Jump(RegionId(2)),
            ),
        ],
    };
    let mut runner = Runner::new(&program, CompileOptions::default());
    runner.set(Frame {
        gpr: [0, 11, 0, 0, 0, 0, 0, 0],
        address: address(3, 0x10),
        remaining_budget: 23,
        ..Frame::default()
    });
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    assert_eq!(runner.frame().gpr[1], 0);
    assert_eq!(runner.frame().gpr[3], 1);
    assert_eq!(runner.frame().charged_work, 24);
    assert_eq!(runner.frame().address, address(3, 0x20));
    let mut program = Program {
        regions: vec![
            region(
                1,
                vec![Operation::Inc(Register::Eax)],
                Terminator::Jump(RegionId(2)),
            ),
            region(
                2,
                vec![Operation::Inc(Register::Ebx)],
                Terminator::Jump(RegionId(1)),
            ),
        ],
    };
    program.regions[0].accounting = Accounting::UncountedHot;
    let mut runner = Runner::new(&program, CompileOptions::default());
    runner.set(Frame {
        address: address(3, 0x10),
        remaining_budget: 3,
        ..Frame::default()
    });
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    let frame = runner.frame();
    assert_eq!(frame.gpr[0], 2);
    assert_eq!(frame.gpr[3], 2);
    assert_eq!(frame.charged_work, 4);
    assert!(frame.uncounted_seen());
    assert_eq!(frame.exact_total(), None);
}

#[test]
fn every_jcc_matches_interpreter_for_every_arithmetic_flag_combination() {
    let conditions = [
        Condition::Overflow,
        Condition::NotOverflow,
        Condition::Below,
        Condition::AboveOrEqual,
        Condition::Equal,
        Condition::NotEqual,
        Condition::BelowOrEqual,
        Condition::Above,
        Condition::Sign,
        Condition::NotSign,
        Condition::Parity,
        Condition::NotParity,
        Condition::Less,
        Condition::GreaterOrEqual,
        Condition::LessOrEqual,
        Condition::Greater,
    ];
    for (code, condition) in conditions.into_iter().enumerate() {
        let program = Program {
            regions: vec![
                region(
                    1,
                    vec![],
                    Terminator::Branch {
                        condition,
                        taken: RegionId(2),
                        not_taken: RegionId(3),
                    },
                ),
                region(2, vec![], Terminator::Jump(RegionId(2))),
                region(3, vec![], Terminator::Jump(RegionId(3))),
            ],
        };
        let mut runner = Runner::new(&program, CompileOptions::default());
        let (mut cpu, mut bus) = reference(&[0x70 + code as u8, 7]);
        for combo in 0u32..32 {
            let flags = 2
                | (combo & 1)
                | ((combo >> 1 & 1) << 2)
                | ((combo >> 2 & 1) << 6)
                | ((combo >> 3 & 1) << 7)
                | ((combo >> 4 & 1) << 11);
            runner.set(Frame {
                address: address(3, 0x10),
                eflags: flags,
                remaining_budget: 1,
                ..Frame::default()
            });
            cpu.flags = flags;
            cpu.registers.ip = 0x1000;
            cpu.step(&mut bus).unwrap();
            assert_eq!(runner.call(), abi::EXIT_BUDGET);
            assert_eq!(
                runner.frame().address,
                address(
                    3,
                    if cpu.registers.ip == 0x1009 {
                        0x20
                    } else {
                        0x30
                    }
                ),
                "{condition:?} flags={flags:x}"
            );
            assert_eq!(runner.frame().charged_work, 1);
            assert_eq!(runner.frame().eflags, flags);
        }
    }
}

#[test]
fn callbacks_flush_small_state_and_resume_mutated_cpu_at_new_address() {
    let program = Program {
        regions: vec![
            region(
                1,
                vec![Operation::Inc(Register::Eax)],
                Terminator::Jump(RegionId(1)),
            ),
            region(
                2,
                vec![Operation::Move {
                    target: Register::Ah,
                    source: Source::Immediate(0x7b),
                }],
                Terminator::Jump(RegionId(2)),
            ),
        ],
    };
    let mut runner = Runner::new(
        &program,
        CompileOptions {
            service_interval_regions: 3,
            ..CompileOptions::default()
        },
    );
    runner.set(Frame {
        address: address(3, 0x10),
        remaining_budget: 100,
        ..Frame::default()
    });
    let replacement = Frame {
        gpr: [0x1122_3344; 8],
        eflags: 0x203,
        address: address(3, 0x20),
        remaining_budget: 1,
        charged_work: 90,
        ..Frame::default()
    };
    runner.store.data_mut().stop_after = 2;
    runner.store.data_mut().redirect = Some(replacement);
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    let event = runner.store.data().events[0];
    assert_eq!(event.0, abi::REASON_SERVICE);
    assert_eq!(event.1.gpr[0], 3);
    assert_eq!(event.1.charged_work, 6);
    assert_eq!(event.1.remaining_budget, 94);
    let frame = runner.frame();
    assert_eq!(frame.gpr[0], 0x1122_7b44);
    assert_eq!(frame.gpr[1..], replacement.gpr[1..]);
    assert_eq!(frame.eflags, replacement.eflags);
    assert_eq!(frame.charged_work, 92);
    assert_eq!(frame.address, address(3, 0x20));
    let bytes = runner.memory.data(&runner.store);
    for (i, &byte) in bytes.iter().enumerate() {
        if !(PTR..PTR + abi::FRAME_BYTES).contains(&i)
            || (PTR + 44..PTR + 48).contains(&i)
            || (PTR + 68..PTR + 72).contains(&i)
        {
            assert_eq!(byte, 0x5a, "write outside defined frame at {i}");
        }
    }
}

#[test]
fn unknown_entries_and_explicit_boundaries_delegate_fallback_without_guessing_counts() {
    let program = Program {
        regions: vec![region(
            7,
            vec![Operation::Inc(Register::Eax)],
            Terminator::Fallback(address(99, 0x1234)),
        )],
    };
    let mut runner = Runner::new(&program, CompileOptions::default());
    runner.set(Frame {
        address: address(0xffff_ffff, 0xffff_ffff),
        remaining_budget: 8,
        charged_work: 20,
        ..Frame::default()
    });
    runner.store.data_mut().stop_after = 2;
    runner.store.data_mut().redirect = Some(Frame {
        address: address(3, 0x70),
        remaining_budget: 8,
        charged_work: 21,
        ..Frame::default()
    });
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    let events = &runner.store.data().events;
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].0, abi::REASON_FALLBACK);
    assert_eq!(events[0].1.charged_work, 20);
    assert_eq!(events[1].0, abi::REASON_FALLBACK);
    assert_eq!(events[1].1.address, address(99, 0x1234));
    assert_eq!(events[1].1.charged_work, 22);
    assert_eq!(events[1].1.gpr[0], 1);
}

#[test]
fn uncounted_hot_loop_has_structural_service_without_guest_accounting() {
    let mut program = single(Operation::Inc(Register::Eax));
    program.regions[0].accounting = Accounting::UncountedHot;
    let mut runner = Runner::new(
        &program,
        CompileOptions {
            service_interval_regions: 5,
            ..CompileOptions::default()
        },
    );
    runner.set(Frame {
        address: address(3, 0x70),
        remaining_budget: 1,
        charged_work: 1234,
        status: 0x8000,
        ..Frame::default()
    });
    runner.store.data_mut().stop_after = 3;
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    let frame = runner.frame();
    assert_eq!(frame.gpr[0], 15);
    assert_eq!(frame.remaining_budget, 1);
    assert_eq!(frame.charged_work, 1234);
    assert!(frame.uncounted_seen());
    assert_eq!(frame.exact_total(), None);
    assert_eq!(frame.status, 0x8001);
    for (i, &(reason, frame)) in runner.store.data().events.iter().enumerate() {
        assert_eq!(reason, abi::REASON_SERVICE);
        assert_eq!(frame.gpr[0], (i as u32 + 1) * 5);
        assert_eq!(frame.remaining_budget, 1);
        assert_eq!(frame.charged_work, 1234);
    }
}

#[test]
fn zero_instruction_loops_are_serviced_and_budget_zero_never_executes() {
    let program = Program {
        regions: vec![region(1, vec![], Terminator::Fallthrough(RegionId(1)))],
    };
    let mut runner = Runner::new(
        &program,
        CompileOptions {
            service_interval_regions: 2,
            ..CompileOptions::default()
        },
    );
    runner.set(Frame {
        address: address(3, 0x10),
        remaining_budget: 1,
        ..Frame::default()
    });
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.frame().charged_work, 0);
    assert_eq!(runner.store.data().events[0].0, abi::REASON_SERVICE);
    runner.set(Frame {
        address: address(0, 99),
        remaining_budget: 0,
        ..Frame::default()
    });
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    assert_eq!(runner.store.data().events.len(), 1);
}

#[test]
fn charged_counter_saturates_and_marks_total_unavailable() {
    let mut runner = Runner::new(&single(Operation::Nop), CompileOptions::default());
    runner.set(Frame {
        address: address(3, 0x70),
        remaining_budget: 2,
        charged_work: u64::MAX - 1,
        ..Frame::default()
    });
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    let frame = runner.frame();
    assert_eq!(frame.charged_work, u64::MAX);
    assert_eq!(frame.status, abi::COUNTER_OVERFLOW);
    assert_eq!(frame.exact_total(), None);
}

#[test]
fn sorted_dispatch_handles_all_entries_and_holes_deterministically() {
    let mut program = Program {
        regions: (0..129)
            .map(|i| {
                let mut r = region(
                    i,
                    vec![Operation::Move {
                        target: Register::Eax,
                        source: Source::Immediate(i),
                    }],
                    Terminator::Fallthrough(RegionId(i)),
                );
                r.entry = address(
                    if i % 2 == 0 { u32::MAX } else { i % 7 },
                    i.wrapping_mul(0xfedc_ba97),
                );
                r
            })
            .collect(),
    };
    let expected = program
        .compile(&CompileOptions::default())
        .unwrap()
        .into_wasm();
    program.regions.reverse();
    assert_eq!(
        program.compile(&CompileOptions::default()).unwrap().wasm(),
        expected
    );
    let mut runner = Runner::new(&program, CompileOptions::default());
    for r in &program.regions {
        runner.set(Frame {
            address: r.entry,
            remaining_budget: 1,
            ..Frame::default()
        });
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        assert_eq!(runner.frame().gpr[0], r.id.0);
        runner.set(Frame {
            address: address(r.entry.space, r.entry.offset.wrapping_add(1)),
            remaining_budget: 1,
            ..Frame::default()
        });
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    }
}

#[test]
fn invalid_ir_and_limits_fail_before_producing_module() {
    let options = CompileOptions::default();
    let failure = |p: &Program, o: CompileOptions| p.compile(&o).unwrap_err();
    let mut p = single(Operation::Nop);
    p.regions.push(p.regions[0].clone());
    assert_eq!(failure(&p, options), CompileError::DuplicateId(RegionId(7)));
    p.regions[1].id = RegionId(8);
    assert_eq!(
        failure(&p, options),
        CompileError::DuplicateAddress(address(3, 0x70))
    );
    let p = single(Operation::Move {
        target: Register::Ah,
        source: Source::Register(Register::Ax),
    });
    assert_eq!(
        failure(&p, options),
        CompileError::WidthMismatch(RegionId(7))
    );
    let p = single(Operation::Alu {
        kind: Alu::Add,
        target: Register::Al,
        source: Source::Immediate(256),
    });
    assert_eq!(
        failure(&p, options),
        CompileError::ImmediateOutOfRange(RegionId(7))
    );
    let mut p = single(Operation::Nop);
    p.regions[0].terminator = Terminator::Jump(RegionId(99));
    assert_eq!(
        failure(&p, options),
        CompileError::UnknownTarget {
            from: RegionId(7),
            target: RegionId(99)
        }
    );
    let p = single(Operation::Nop);
    assert_eq!(
        failure(
            &p,
            CompileOptions {
                max_regions: 0,
                ..options
            }
        ),
        CompileError::TooManyRegions
    );
    assert_eq!(
        failure(
            &p,
            CompileOptions {
                max_total_instructions: 0,
                ..options
            }
        ),
        CompileError::TooManyInstructions
    );
    assert_eq!(
        failure(
            &p,
            CompileOptions {
                max_output_bytes: 8,
                ..options
            }
        ),
        CompileError::OutputTooLarge
    );
    for o in [
        CompileOptions {
            service_interval_regions: 0,
            ..options
        },
        CompileOptions {
            service_interval_regions: MAX_SERVICE_INTERVAL_REGIONS + 1,
            ..options
        },
        CompileOptions {
            max_regions: MAX_REGIONS + 1,
            ..options
        },
        CompileOptions {
            max_output_bytes: MAX_OUTPUT_BYTES + 1,
            ..options
        },
    ] {
        assert_eq!(failure(&p, o), CompileError::InvalidOptions);
    }
    let mut p = single(Operation::Nop);
    p.regions[0].operations = vec![Operation::Nop; 256];
    p.regions[0].terminator = Terminator::Jump(RegionId(7));
    assert_eq!(
        failure(&p, options),
        CompileError::RegionTooLong(RegionId(7))
    );
}

#[test]
fn static_region_limit_is_independent_of_the_profile_cache_capacity() {
    let program = Program {
        regions: (0..4097)
            .map(|i| {
                region(
                    i,
                    vec![Operation::Nop],
                    Terminator::Fallthrough(RegionId(i)),
                )
            })
            .collect(),
    };
    let mut runner = Runner::new(&program, CompileOptions::default());
    runner.set(Frame {
        address: address(3, 4096 * 16),
        remaining_budget: 1,
        ..Frame::default()
    });
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    assert_eq!(runner.frame().charged_work, 1);
}

#[test]
fn empty_program_fallback_and_frame_bounds_are_explicit() {
    let mut runner = Runner::new(&Program::default(), CompileOptions::default());
    runner.set(Frame {
        remaining_budget: 1,
        ..Frame::default()
    });
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.store.data().events[0].0, abi::REASON_FALLBACK);
    assert_eq!(Frame::read(&[0; 71]), Err(FrameError));
    assert_eq!(Frame::default().write(&mut [0; 71]), Err(FrameError));
    assert!(runner.run.call(&mut runner.store, 65500).is_err());
}
