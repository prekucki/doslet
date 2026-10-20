//! Execute the emitted WASM and independently encoded x86, including code/SS
//! width disagreement and ignored instruction address-size overrides.

use super::*;
use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_x86::{Cpu, DescriptorTable, Gpr32, SegmentRegister};
use memory::{DeviceReadDescriptor, DeviceReadKind, StackAddress};

#[path = "return_tests.rs"]
mod return_tests;

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
const WORDS: [Register; 8] = [
    Register::Ax,
    Register::Cx,
    Register::Dx,
    Register::Bx,
    Register::Sp,
    Register::Bp,
    Register::Si,
    Register::Di,
];
const DWORDS: [Register; 8] = [
    Register::Eax,
    Register::Ecx,
    Register::Edx,
    Register::Ebx,
    Register::Esp,
    Register::Ebp,
    Register::Esi,
    Register::Edi,
];
const STACK_PAGES: u32 = 32;

fn single(op: Operation) -> Program {
    let mut p = program(Width::Dword, Source::Immediate(0), address(0x903));
    p.regions[0].operations[0] = op;
    p
}

fn width(op: Operation) -> Width {
    match op {
        Operation::Push { width, .. } => width,
        Operation::Pop { target } => target.width(),
        _ => unreachable!("stack fixture"),
    }
}

fn push(op: Operation) -> bool {
    matches!(op, Operation::Push { .. })
}

fn stack_descriptor(mode: StackAddress, base: u32) -> Descriptor {
    let mut d = descriptor();
    d.stack_address = mode;
    d.ram.linear_base = base;
    d.ram.generations_length = STACK_PAGES;
    d.ram.code_pages_length = STACK_PAGES;
    d.device.latches = [0x12, 0x34, 0x56, 0x78];
    d
}

fn prepare(runner: &mut Runner, p: &Program, frame: Frame, d: &Descriptor) {
    runner.set(p, frame, d);
    runner.bytes()[CODE_PAGES..CODE_PAGES + STACK_PAGES as usize].fill(0);
    runner.bytes()[CODE_PAGES + 4] = 1;
    for page in 0..STACK_PAGES {
        runner.set_generation(page, 11 + u64::from(page));
    }
}

fn encoding(op: Operation, code32: bool, address_override: bool, alternate: bool) -> Vec<u8> {
    let width = width(op);
    let mut out = vec![];
    if (width == Width::Dword) != code32 {
        out.push(0x66);
    }
    if address_override {
        out.push(0x67);
    }
    match op {
        Operation::Push {
            source: Source::Register(r),
            ..
        } => {
            if alternate {
                out.extend_from_slice(&[0xff, 0xf0 | r.index() as u8]);
            } else {
                out.push(0x50 | r.index() as u8);
            }
        }
        Operation::Push {
            source: Source::Immediate(v),
            ..
        } => {
            if alternate {
                assert_eq!((v as u8 as i8 as i32 as u32) & width.mask(), v);
                out.extend_from_slice(&[0x6a, v as u8]);
            } else {
                out.push(0x68);
                out.extend_from_slice(&v.to_le_bytes()[..width.bits() as usize / 8]);
            }
        }
        Operation::Pop { target } => {
            if alternate {
                out.extend_from_slice(&[0x8f, 0xc0 | target.index() as u8]);
            } else {
                out.push(0x58 | target.index() as u8);
            }
        }
        _ => unreachable!("stack fixture"),
    }
    out
}

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
        self.0.get(a as usize..a as usize + usize::from(len))
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

fn reference(code: &[u8], code32: bool, mode: StackAddress, ss_base: u32) -> (Cpu, Bus) {
    let mut bus = Bus(vec![0; 0x20_000]);
    bus.0[0x8000..0x8000 + code.len()].copy_from_slice(code);
    bus.0[0x208..0x218].copy_from_slice(&[
        0xff,
        0xff,
        0,
        0,
        0,
        0x9a,
        0x8f | if code32 { 0x40 } else { 0 },
        0,
        0xff,
        0xff,
        0,
        0,
        0,
        0x92,
        0x8f | if mode == StackAddress::Bits32 {
            0x40
        } else {
            0
        },
        0,
    ]);
    bus.0[0x212..0x214].copy_from_slice(&(ss_base as u16).to_le_bytes());
    bus.0[0x214] = (ss_base >> 16) as u8;
    bus.0[0x217] = (ss_base >> 24) as u8;
    let mut cpu = Cpu::default();
    cpu.set_decode_cache(false);
    cpu.set_dynamic_core(false);
    cpu.control.gdtr = DescriptorTable {
        base: 0x200,
        limit: 23,
    };
    cpu.set_protected_mode(true);
    cpu.load_segment(&mut bus, SegmentRegister::Cs, 8).unwrap();
    for s in [
        SegmentRegister::Ss,
        SegmentRegister::Ds,
        SegmentRegister::Es,
    ] {
        cpu.load_segment(&mut bus, s, 16).unwrap();
    }
    (cpu, bus)
}

fn differential(
    op: Operation,
    mode: StackAddress,
    code32: bool,
    address_override: bool,
    alternate: bool,
) {
    let p = single(op);
    let code = encoding(op, code32, address_override, alternate);
    let (mut cpu, mut bus) = reference(&code, code32, mode, 0);
    let base = if mode == StackAddress::Bits32 {
        0x1_0000
    } else {
        RAM_BASE
    };
    let d = stack_descriptor(mode, base);
    let bytes = width(op).bits() / 8;
    for inline in [false, true] {
        let mut runner = Runner::new(&p, inline);
        for sample in 0..8 {
            let mut frame = initial();
            frame.gpr[4] = if mode == StackAddress::Bits16 {
                0xdead_0000
            } else {
                0x1_0000
            } | (0xffd + sample % 4);
            frame.eflags = 0xa550_0cd4 | (sample & 1);
            frame.charged_work = 39;
            frame.status = 0x8000_0000;
            let values = [0, 1, 0x7f, 0x80, 0x7fff, 0x8000, u32::MAX, 0x8192_3746];
            let value = values[sample as usize];
            if let Operation::Push {
                source: Source::Register(r),
                ..
            } = op
                && r.index() != Register::Esp.index()
            {
                let old = &mut frame.gpr[r.index() as usize];
                *old = (*old & !r.width().mask()) | (value & r.width().mask());
            }
            let mut ea = frame.gpr[4].wrapping_sub(if push(op) { bytes } else { 0 });
            if mode == StackAddress::Bits16 {
                ea &= 0xffff;
            }
            let start = RAM + (ea - base) as usize;
            prepare(&mut runner, &p, frame, &d);
            runner.bytes()[start..start + 4].copy_from_slice(&value.to_le_bytes());
            bus.0[base as usize..base as usize + RAM_LENGTH]
                .copy_from_slice(&runner.bytes()[RAM..RAM + RAM_LENGTH]);
            cpu.registers.ip = 0x8000;
            cpu.flags = frame.eflags;
            for (r, v) in REGS.into_iter().zip(frame.gpr) {
                cpu.registers.set32(r, v);
            }
            cpu.step(&mut bus).unwrap();
            assert_eq!(runner.call(), abi::EXIT_BUDGET, "{op:?} {mode:?}");
            assert_eq!(
                runner.frame(),
                Frame {
                    gpr: core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
                    eflags: cpu.flags,
                    remaining_budget: 0,
                    charged_work: 40,
                    ..frame
                },
                "{op:?} {mode:?} code32={code32} address_override={address_override} alternate={alternate}"
            );
            assert_eq!(
                &runner.bytes()[RAM..RAM + RAM_LENGTH],
                &bus.0[base as usize..base as usize + RAM_LENGTH]
            );
            let first = ea >> 12;
            let last = (ea + bytes - 1) >> 12;
            for page in 0..STACK_PAGES {
                assert_eq!(
                    runner.generation(page),
                    11 + u64::from(page) + u64::from(push(op) && (page == first || page == last))
                );
            }
            assert_eq!(
                runner.effects(),
                memory::Effects {
                    device_reads: 0,
                    latches: d.device.latches,
                    ram_writes: if push(op) {
                        1 + u64::from(first != last)
                    } else {
                        0
                    },
                    inline_reserved: u32::from(inline),
                    inline_remaining: 100 - u32::from(inline),
                    ..memory::Effects::default()
                }
            );
            assert!(runner.store.data().callbacks.is_empty());
            assert_eq!(
                runner.store.data().admissions,
                if inline { vec![] } else { vec![(0, 1)] }
            );
            runner.store.data_mut().admissions.clear();
        }
    }
}

#[test]
fn register_stack_wasm_matches_cpu_for_all_aliases_independent_ss_code_and_address_widths() {
    for r in WORDS.into_iter().chain(DWORDS) {
        for op in [
            Operation::Push {
                width: r.width(),
                source: Source::Register(r),
            },
            Operation::Pop { target: r },
        ] {
            for mode in [StackAddress::Bits16, StackAddress::Bits32] {
                for code32 in [false, true] {
                    for address_override in [false, true] {
                        differential(op, mode, code32, address_override, address_override);
                    }
                }
            }
        }
    }
}

#[test]
fn immediate_stack_wasm_matches_cpu_for_full_width_and_sign_extended_encodings() {
    for width in [Width::Word, Width::Dword] {
        for value in [
            0,
            0x7f,
            0x80,
            0x7fff,
            0x8000,
            width.mask(),
            width.mask() - 127,
        ] {
            let op = Operation::Push {
                width,
                source: Source::Immediate(value),
            };
            for mode in [StackAddress::Bits16, StackAddress::Bits32] {
                for code32 in [false, true] {
                    differential(op, mode, code32, true, false);
                    if (value as u8 as i8 as i32 as u32) & width.mask() == value {
                        differential(op, mode, code32, false, true);
                    }
                }
            }
        }
    }
}

#[test]
fn stack_pointer_wrapping_and_crossing_64k_match_cpu_without_wrapping_operand_bytes() {
    for (mode, old_esp, base, ss_base, op) in [
        (
            StackAddress::Bits16,
            0xdead_0000,
            0xf000,
            0,
            Operation::Push {
                width: Width::Word,
                source: Source::Register(Register::Sp),
            },
        ),
        (
            StackAddress::Bits16,
            0xdead_0002,
            0xf000,
            0,
            Operation::Push {
                width: Width::Dword,
                source: Source::Register(Register::Esp),
            },
        ),
        (
            StackAddress::Bits16,
            0xdead_fffe,
            0xf000,
            0,
            Operation::Pop {
                target: Register::Ax,
            },
        ),
        (
            StackAddress::Bits16,
            0xdead_ffff,
            0xf000,
            0,
            Operation::Pop {
                target: Register::Eax,
            },
        ),
        (
            StackAddress::Bits16,
            0xdead_fffe,
            0xf000,
            0,
            Operation::Pop {
                target: Register::Sp,
            },
        ),
        (
            StackAddress::Bits16,
            0xdead_ffff,
            0xf000,
            0,
            Operation::Pop {
                target: Register::Esp,
            },
        ),
        (
            StackAddress::Bits32,
            0,
            RAM_BASE,
            0x1000,
            Operation::Push {
                width: Width::Dword,
                source: Source::Register(Register::Esp),
            },
        ),
        (
            StackAddress::Bits32,
            0xffff_fffc,
            RAM_BASE,
            0x1000,
            Operation::Pop {
                target: Register::Eax,
            },
        ),
        (
            StackAddress::Bits32,
            0,
            RAM_BASE,
            0x1000,
            Operation::Push {
                width: Width::Word,
                source: Source::Register(Register::Sp),
            },
        ),
        (
            StackAddress::Bits32,
            0xffff_fffe,
            RAM_BASE,
            0x1000,
            Operation::Pop {
                target: Register::Ax,
            },
        ),
        (
            StackAddress::Bits32,
            0x1_fffe,
            0xf000,
            0xffff_0000,
            Operation::Pop {
                target: Register::Sp,
            },
        ),
        (
            StackAddress::Bits32,
            0x1_ffff,
            0xf000,
            0xffff_0000,
            Operation::Pop {
                target: Register::Sp,
            },
        ),
        (
            StackAddress::Bits32,
            0x1_0000,
            0xf000,
            0,
            Operation::Push {
                width: Width::Word,
                source: Source::Register(Register::Sp),
            },
        ),
        (
            StackAddress::Bits32,
            0x1_0002,
            0xf000,
            0,
            Operation::Push {
                width: Width::Dword,
                source: Source::Register(Register::Esp),
            },
        ),
    ] {
        let p = single(op);
        let code = encoding(op, true, true, false);
        for inline in [false, true] {
            let (mut cpu, mut bus) = reference(&code, true, mode, ss_base);
            let mut d = stack_descriptor(mode, base);
            for access in &mut d.windows {
                access[Segment::Ss as usize].base = ss_base;
            }
            let mut frame = initial();
            frame.gpr[4] = old_esp;
            let mut runner = Runner::new(&p, inline);
            prepare(&mut runner, &p, frame, &d);
            bus.0[base as usize..base as usize + RAM_LENGTH]
                .copy_from_slice(&runner.bytes()[RAM..RAM + RAM_LENGTH]);
            cpu.registers.ip = 0x8000;
            cpu.flags = frame.eflags;
            for (r, v) in REGS.into_iter().zip(frame.gpr) {
                cpu.registers.set32(r, v);
            }
            cpu.step(&mut bus).unwrap();
            assert_eq!(runner.call(), abi::EXIT_BUDGET, "{op:?} {mode:?}");
            assert_eq!(
                runner.frame().gpr,
                core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
                "{op:?} {mode:?}"
            );
            assert_eq!(runner.frame().eflags, cpu.flags);
            assert_eq!(
                &runner.bytes()[RAM..RAM + RAM_LENGTH],
                &bus.0[base as usize..base as usize + RAM_LENGTH]
            );
        }
    }
}

fn standard_ops() -> [Operation; 2] {
    [
        Operation::Push {
            width: Width::Dword,
            source: Source::Register(Register::Esp),
        },
        Operation::Pop {
            target: Register::Esp,
        },
    ]
}

#[test]
fn missing_unknown_or_truncated_stack_capability_refuses_before_admission_and_effects() {
    assert_eq!(Descriptor::default().stack_address, StackAddress::Disabled);
    assert_eq!(memory::BYTES, 384);
    assert_eq!(abi::FRAME_BYTES, 72);
    for op in standard_ops() {
        let p = single(op);
        for inline in [false, true] {
            for (offset, value) in [
                (memory::STACK_ADDRESS, 0),
                (memory::STACK_ADDRESS, 3),
                (memory::STACK_ADDRESS, u32::MAX),
                (memory::VERSION_OFFSET, memory::VERSION + 1),
                (memory::RAM_POINTER, u32::MAX),
                (memory::RAM_POINTER, 65535),
                (memory::RAM_BASE, 0x1004),
                (memory::RAM_LENGTH, 1),
            ] {
                let mut runner = Runner::new(&p, inline);
                let mut frame = initial();
                frame.gpr[4] = 0x1003;
                prepare(
                    &mut runner,
                    &p,
                    frame,
                    &stack_descriptor(StackAddress::Bits32, RAM_BASE),
                );
                runner.word(offset, value);
                runner.assert_refused(frame);
            }
            // The frame itself is valid but the following descriptor is not.
            let mut runner = Runner::new(&p, inline);
            let pointer = 65536 - abi::FRAME_BYTES;
            let frame = initial();
            frame.write(&mut runner.bytes()[pointer..]).unwrap();
            let before = runner.memory.data(&runner.store).to_vec();
            assert_eq!(
                runner.run.call(&mut runner.store, pointer as u32).unwrap(),
                abi::EXIT_RUNTIME
            );
            assert_eq!(runner.memory.data(&runner.store), before);
            assert!(runner.store.data().admissions.is_empty());
            assert_eq!(
                runner.store.data().callbacks,
                vec![(abi::REASON_FALLBACK, frame)]
            );
        }
    }
}

#[test]
fn stack_segment_permissions_limits_and_excluded_or_device_ram_refuse_without_reads() {
    for op in standard_ops() {
        let p = single(op);
        let mut frame = initial();
        frame.gpr[4] = if push(op) { 0x1003 } else { 0xfff };
        let access = usize::from(push(op));
        let ss = Segment::Ss as usize;
        for inline in [false, true] {
            for case in 0..7 {
                let mut d = stack_descriptor(StackAddress::Bits32, RAM_BASE);
                match case {
                    0 => d.windows[access][ss].valid = false,
                    1 => d.windows[access][ss].first = 0x1000,
                    2 => d.windows[access][ss].last = 0x1001,
                    3 => d.windows[access][ss].base = u32::MAX - 0x1000,
                    4 => d.ram.excluded_start = 0x1000,
                    5 => d.device.guest_start = 0x1000,
                    6 => d.ram.length = 0x1001 - RAM_BASE,
                    _ => unreachable!(),
                }
                d.ram.excluded_length = u32::from(case == 4);
                if case == 5 {
                    d.device = DeviceReadDescriptor {
                        guest_start: 0x1000,
                        guest_length: 16,
                        pointer: 32768,
                        backing_length: 8192,
                        kind: DeviceReadKind::FourLanes,
                        latches: [0x12, 0x34, 0x56, 0x78],
                        ..DeviceReadDescriptor::default()
                    };
                }
                let mut runner = Runner::new(&p, inline);
                prepare(&mut runner, &p, frame, &d);
                runner.assert_refused(frame);
            }
        }
        for mode in [StackAddress::Bits16, StackAddress::Bits32] {
            let mut d = stack_descriptor(mode, RAM_BASE);
            d.windows[access][ss].last = 0xffff;
            let mut runner = Runner::new(&p, true);
            frame.gpr[4] = if push(op) { 2 } else { 0xfffe };
            prepare(&mut runner, &p, frame, &d);
            runner.assert_refused(frame);
        }
    }
}

#[test]
fn stack_uses_its_selected_ss_access_window_and_preserves_flags() {
    for op in standard_ops() {
        let p = single(op);
        let mut d = stack_descriptor(StackAddress::Bits16, RAM_BASE);
        let access = usize::from(push(op));
        d.windows[1 - access][Segment::Ss as usize].valid = false;
        d.windows[access][Segment::Ss as usize].base = 0x1000;
        let mut frame = initial();
        frame.gpr[4] = 0xdead_0600;
        let mut runner = Runner::new(&p, true);
        prepare(&mut runner, &p, frame, &d);
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        assert_eq!(runner.frame().eflags, frame.eflags);
        if push(op) {
            assert_eq!(runner.frame().gpr[4], 0xdead_05fc);
            let start = RAM + 0x15fc - RAM_BASE as usize;
            assert_eq!(
                &runner.bytes()[start..start + 4],
                &frame.gpr[4].to_le_bytes()
            );
        } else {
            assert_eq!(runner.frame().gpr[4], 0x5a5a_5a5a);
        }
        assert_eq!(runner.effects().device_reads, 0);
    }
}

#[test]
fn stack_push_preflights_both_pages_full_generation_tables_and_code_protection() {
    let p = single(standard_ops()[0]);
    let mut frame = initial();
    frame.gpr[4] = 0x1003; // four bytes at 0xfff touch two physical pages
    for inline in [false, true] {
        for (offset, value) in [
            (memory::GENERATIONS_POINTER, u32::MAX),
            (memory::GENERATIONS_POINTER, 65536 - STACK_PAGES * 8 + 1),
            (memory::GENERATIONS_LENGTH, u32::MAX),
            (memory::GENERATIONS_LENGTH, 0),
            (memory::GENERATIONS_LENGTH, 1),
            (memory::CODE_PAGES_POINTER, u32::MAX),
            (memory::CODE_PAGES_POINTER, 65536 - STACK_PAGES + 1),
            (memory::CODE_PAGES_LENGTH, u32::MAX),
            (memory::CODE_PAGES_LENGTH, 0),
            (memory::CODE_PAGES_LENGTH, 1),
        ] {
            let mut runner = Runner::new(&p, inline);
            prepare(
                &mut runner,
                &p,
                frame,
                &stack_descriptor(StackAddress::Bits32, RAM_BASE),
            );
            runner.word(offset, value);
            runner.assert_refused(frame);
        }
        for page in [0, 1] {
            for protected in [false, true] {
                let mut runner = Runner::new(&p, inline);
                prepare(
                    &mut runner,
                    &p,
                    frame,
                    &stack_descriptor(StackAddress::Bits32, RAM_BASE),
                );
                if protected {
                    runner.bytes()[CODE_PAGES + page as usize] = 1;
                } else {
                    runner.set_generation(page, u64::MAX);
                }
                runner.assert_refused(frame);
            }
        }
    }
}

#[test]
fn stack_source_guard_or_horizon_refusal_precedes_ram_reads_writes_and_reservations() {
    for op in standard_ops() {
        let p = single(op);
        let mut frame = initial();
        frame.gpr[4] = 0x1003;
        for case in 0..7 {
            let mut runner = Runner::new(&p, true);
            prepare(
                &mut runner,
                &p,
                frame,
                &stack_descriptor(StackAddress::Bits32, RAM_BASE),
            );
            match case {
                0 => runner.word(memory::INLINE_GRANT, 0),
                1 => runner.word(memory::REGION_GUARDS_POINTER, u32::MAX),
                2 => runner.word(memory::REGION_GUARDS_LENGTH, 0),
                3 => runner.guard(0, RegionGuard::default()),
                4 => runner.set_generation(4, 16),
                5 => runner.word(memory::GENERATIONS_POINTER, u32::MAX),
                6 => runner.word(memory::INLINE_RESERVED, u32::MAX),
                _ => unreachable!(),
            }
            runner.assert_refused(frame);
        }
        let mut runner = Runner::new(&p, false);
        prepare(
            &mut runner,
            &p,
            frame,
            &stack_descriptor(StackAddress::Bits32, RAM_BASE),
        );
        runner.store.data_mut().refuse = true;
        let before = runner.memory.data(&runner.store).to_vec();
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        assert_eq!(runner.memory.data(&runner.store), before);
        assert_eq!(runner.store.data().admissions, vec![(0, 1)]);
    }
}

#[test]
fn stack_ir_requires_word_dword_operands_matching_sources_and_isolated_counted_regions() {
    for op in standard_ops() {
        let p = single(op);
        assert_eq!(
            p.compile(&CompileOptions::default()).unwrap_err(),
            CompileError::DirectMemoryDisabled(RegionId(0))
        );
        let mut mixed = p.clone();
        mixed.regions[0].operations.push(Operation::Nop);
        assert_eq!(
            mixed.compile(&options(false)).unwrap_err(),
            CompileError::MixedMemoryRegion(RegionId(0))
        );
        mixed = p.clone();
        mixed.regions[0].terminator = Terminator::Jump(RegionId(0));
        assert_eq!(
            mixed.compile(&options(false)).unwrap_err(),
            CompileError::MixedMemoryRegion(RegionId(0))
        );
        mixed = p.clone();
        mixed.regions[0].accounting = Accounting::UncountedHot;
        assert_eq!(
            mixed
                .compile(&CompileOptions {
                    direct_memory: true,
                    ..CompileOptions::default()
                })
                .unwrap_err(),
            CompileError::MemoryUncountedHot(RegionId(0))
        );
    }
    for op in [
        Operation::Push {
            width: Width::Byte,
            source: Source::Immediate(1),
        },
        Operation::Pop {
            target: Register::Al,
        },
        Operation::Pop {
            target: Register::Ah,
        },
        Operation::Push {
            width: Width::Word,
            source: Source::Register(Register::Eax),
        },
        Operation::Push {
            width: Width::Dword,
            source: Source::Register(Register::Ax),
        },
    ] {
        assert_eq!(
            single(op).compile(&options(true)).unwrap_err(),
            CompileError::WidthMismatch(RegionId(0))
        );
    }
    assert_eq!(
        single(Operation::Push {
            width: Width::Word,
            source: Source::Immediate(0x1_0000)
        })
        .compile(&options(true))
        .unwrap_err(),
        CompileError::ImmediateOutOfRange(RegionId(0))
    );
}
