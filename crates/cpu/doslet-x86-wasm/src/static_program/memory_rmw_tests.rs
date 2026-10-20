use super::*;
use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_x86::{Cpu, DescriptorTable, Gpr32, SegmentRegister};

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
const ALIASES: [Register; 24] = [
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
const KINDS: [Alu; 7] = [
    Alu::Add,
    Alu::Adc,
    Alu::Sub,
    Alu::Sbb,
    Alu::And,
    Alu::Or,
    Alu::Xor,
];
const UNARY: [MemoryUnary; 4] = [
    MemoryUnary::Inc,
    MemoryUnary::Dec,
    MemoryUnary::Neg,
    MemoryUnary::Not,
];

fn single(op: Operation) -> Program {
    let mut p = program(Width::Dword, Source::Immediate(0), address(0x903));
    p.regions[0].operations[0] = op;
    p
}

fn operands(op: Operation) -> (Width, MemoryAddress) {
    match op {
        Operation::AluMemoryStore { width, address, .. }
        | Operation::UnaryMemory { width, address, .. } => (width, address),
        _ => unreachable!("RMW fixture"),
    }
}

fn register_code(r: Register) -> u8 {
    (r.index() + if r.shift() == 8 { 4 } else { 0 }) as u8
}

// Encode actual x86 independently of the generated WASM implementation.
fn encoding(op: Operation) -> Vec<u8> {
    let (width, a) = operands(op);
    let mut out = vec![match a.segment {
        Segment::Es => 0x26,
        Segment::Cs => 0x2e,
        Segment::Ss => 0x36,
        Segment::Ds => 0x3e,
        Segment::Fs => 0x64,
        Segment::Gs => 0x65,
    }];
    if a.address16 {
        out.push(0x67);
    }
    if width == Width::Word {
        out.push(0x66);
    }
    let word = u8::from(width != Width::Byte);
    let (opcode, field, immediate) = match op {
        Operation::AluMemoryStore { kind, source, .. } => {
            let group = match kind {
                Alu::Add => 0,
                Alu::Or => 1,
                Alu::Adc => 2,
                Alu::Sbb => 3,
                Alu::And => 4,
                Alu::Sub => 5,
                Alu::Xor => 6,
                _ => unreachable!("read-only ALU"),
            };
            match source {
                Source::Register(r) => (group * 8 + word, register_code(r), None),
                Source::Immediate(v) => (0x80 + word, group, Some(v)),
            }
        }
        Operation::UnaryMemory { kind, .. } => match kind {
            MemoryUnary::Inc => (0xfe + word, 0, None),
            MemoryUnary::Dec => (0xfe + word, 1, None),
            MemoryUnary::Not => (0xf6 + word, 2, None),
            MemoryUnary::Neg => (0xf6 + word, 3, None),
        },
        _ => unreachable!("RMW fixture"),
    };
    out.push(opcode);
    if a.address16 {
        if a.base.is_none() && a.index.is_none() {
            out.push(field << 3 | 6);
        } else {
            assert_eq!(
                (a.base, a.index),
                (Some(Register::Ebx), Some(Register::Esi))
            );
            out.push(0x80 | field << 3);
        }
        out.extend_from_slice(&(a.displacement as u16).to_le_bytes());
    } else {
        if a.base.is_none() && a.index.is_none() {
            out.push(field << 3 | 5);
        } else {
            out.push(if a.base.is_some() { 0x80 } else { 0 } | field << 3 | 4);
            out.push(
                (a.scale.trailing_zeros() as u8) << 6
                    | a.index.map_or(4, register_code) << 3
                    | a.base.map_or(5, register_code),
            );
        }
        out.extend_from_slice(&a.displacement.to_le_bytes());
    }
    if let Some(v) = immediate {
        out.extend_from_slice(&v.to_le_bytes()[..width.bits() as usize / 8]);
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

fn reference(code: &[u8]) -> (Cpu, Bus) {
    let mut bus = Bus(vec![0; 65536]);
    bus.0[0x8000..0x8000 + code.len()].copy_from_slice(code);
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
    cpu.load_segment(&mut bus, SegmentRegister::Cs, 8).unwrap();
    for s in [
        SegmentRegister::Es,
        SegmentRegister::Ds,
        SegmentRegister::Ss,
        SegmentRegister::Fs,
        SegmentRegister::Gs,
    ] {
        cpu.load_segment(&mut bus, s, 16).unwrap();
    }
    (cpu, bus)
}

fn differential(op: Operation, inline: bool) {
    let p = single(op);
    let mut runner = Runner::new(&p, inline);
    let (mut cpu, mut bus) = reference(&encoding(op));
    let (width, a) = operands(op);
    let sign = 1 << (width.bits() - 1);
    let boundaries = [
        0,
        1,
        0xf,
        0x10,
        sign - 1,
        sign,
        width.mask(),
        width.mask() - 1,
    ];
    let mut seed = 0x8193_23a5u32;
    for sample in 0..96 {
        for carry in 0..=1 {
            let mut random = || {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                seed
            };
            let mut frame = Frame {
                gpr: core::array::from_fn(|_| random()),
                eflags: 0xa550_0cd4 | carry,
                charged_work: 39,
                status: 0x8000_0000,
                ..initial()
            };
            let value = if sample < 64 {
                boundaries[sample / 8]
            } else {
                random()
            };
            let old = if sample < 64 {
                boundaries[sample % 8]
            } else {
                random()
            };
            if let Operation::AluMemoryStore {
                source: Source::Register(r),
                ..
            } = op
            {
                let gpr = &mut frame.gpr[r.index() as usize];
                *gpr = (*gpr & !(r.width().mask() << r.shift()))
                    | ((value & r.width().mask()) << r.shift());
            }
            if let Some(r) = a.base {
                frame.gpr[r.index() as usize] = 0x8001_0400;
            }
            if let Some(r) = a.index {
                frame.gpr[r.index() as usize] = 0x8123_0001;
            }
            let ea = a
                .displacement
                .wrapping_add(a.base.map_or(0, |r| frame.gpr[r.index() as usize]))
                .wrapping_add(
                    a.index
                        .map_or(0, |r| frame.gpr[r.index() as usize])
                        .wrapping_mul(u32::from(a.scale)),
                );
            let linear = if a.address16 { ea & 0xffff } else { ea };
            let bytes = width.bits() as usize / 8;
            let start = RAM + (linear - RAM_BASE) as usize;
            runner.set(&p, frame, &descriptor());
            runner.bytes()[start..start + 4].copy_from_slice(&old.to_le_bytes());
            let effect_offset = FRAME + memory::OFFSET as usize + memory::RAM_WRITES as usize;
            runner.bytes()[effect_offset..effect_offset + 8]
                .copy_from_slice(&0x1_ffff_ffffu64.to_le_bytes());
            bus.0[RAM_BASE as usize..RAM_BASE as usize + RAM_LENGTH]
                .copy_from_slice(&runner.bytes()[RAM..RAM + RAM_LENGTH]);
            cpu.registers.ip = 0x8000;
            cpu.flags = frame.eflags;
            for (r, v) in REGS.into_iter().zip(frame.gpr) {
                cpu.registers.set32(r, v);
            }
            cpu.step(&mut bus).unwrap();
            assert_eq!(runner.call(), abi::EXIT_BUDGET, "{op:?}");
            assert_eq!(
                runner.frame(),
                Frame {
                    gpr: core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
                    eflags: cpu.flags,
                    charged_work: 40,
                    remaining_budget: 0,
                    ..frame
                },
                "{op:?}, old={old:x}, value={value:x}, carry={carry}"
            );
            assert_eq!(
                &runner.bytes()[RAM..RAM + RAM_LENGTH],
                &bus.0[RAM_BASE as usize..RAM_BASE as usize + RAM_LENGTH],
                "{op:?}"
            );
            let first = linear / 4096;
            let last = (linear + bytes as u32 - 1) / 4096;
            for page in 0..PAGE_COUNT {
                assert_eq!(
                    runner.generation(page),
                    11 + u64::from(page) + u64::from(page == first || page == last)
                );
            }
            assert_eq!(
                runner.effects().ram_writes,
                0x2_0000_0000 + u64::from(first != last)
            );
            assert_eq!(runner.effects().device_reads, 0);
            assert_eq!(runner.effects().inline_reserved, u32::from(inline));
            assert_eq!(runner.effects().inline_remaining, 100 - u32::from(inline));
            assert!(runner.store.data().callbacks.is_empty());
            runner.store.data_mut().admissions.clear();
        }
    }
}

#[test]
fn rmw_alu_matches_cpu_for_every_register_alias_kind_width_boundary_pair_and_carry() {
    for r in ALIASES {
        for kind in KINDS {
            differential(
                Operation::AluMemoryStore {
                    kind,
                    width: r.width(),
                    source: Source::Register(r),
                    address: address(0x903),
                },
                false,
            );
        }
    }
}

#[test]
fn rmw_immediates_and_unary_match_cpu_including_page_crossings_and_signed_extremes() {
    for width in [Width::Byte, Width::Word, Width::Dword] {
        for kind in KINDS {
            for value in [0, 1, 0xf, 0x10, 1 << (width.bits() - 1), width.mask()] {
                differential(
                    Operation::AluMemoryStore {
                        kind,
                        width,
                        source: Source::Immediate(value),
                        address: address(0xfff),
                    },
                    true,
                );
            }
        }
        for kind in UNARY {
            for inline in [false, true] {
                differential(
                    Operation::UnaryMemory {
                        kind,
                        width,
                        address: address(0xfff),
                    },
                    inline,
                );
            }
        }
    }
}

#[test]
fn rmw_preserves_old_source_and_address_aliases_for_sib_and_address16() {
    for r in ALIASES {
        let base = ALIASES[r.index() as usize];
        let base_value: u32 = if base == Register::Ecx {
            0x8123_0001
        } else {
            0x8001_0400
        };
        for kind in [Alu::Adc, Alu::Sbb] {
            differential(
                Operation::AluMemoryStore {
                    kind,
                    width: r.width(),
                    source: Source::Register(r),
                    address: MemoryAddress {
                        base: Some(base),
                        index: Some(Register::Ecx),
                        scale: 4,
                        displacement: 0x903u32
                            .wrapping_sub(base_value)
                            .wrapping_sub(0x8123_0001u32.wrapping_mul(4)),
                        segment: Segment::Ss,
                        ..address(0)
                    },
                },
                true,
            );
        }
    }
    for width in [Width::Byte, Width::Word, Width::Dword] {
        for kind in UNARY {
            differential(
                Operation::UnaryMemory {
                    kind,
                    width,
                    address: MemoryAddress {
                        base: Some(Register::Ebx),
                        index: Some(Register::Esi),
                        address16: true,
                        displacement: 0x903u32.wrapping_sub(0x8001_0400).wrapping_sub(0x8123_0001),
                        segment: Segment::Fs,
                        ..address(0)
                    },
                },
                true,
            );
        }
    }
}

fn representative_operations() -> [Operation; 2] {
    [
        Operation::AluMemoryStore {
            kind: Alu::Adc,
            width: Width::Dword,
            source: Source::Register(Register::Eax),
            address: address(0xfff),
        },
        Operation::UnaryMemory {
            kind: MemoryUnary::Dec,
            width: Width::Dword,
            address: address(0xfff),
        },
    ]
}

#[test]
fn rmw_read_and_write_windows_must_both_cover_the_complete_operand_and_agree_on_base() {
    for op in representative_operations() {
        let p = single(op);
        for inline in [false, true] {
            for access in 0..2 {
                for fault in 0..4 {
                    let mut d = descriptor();
                    let window = &mut d.windows[access][Segment::Ds as usize];
                    match fault {
                        0 => window.valid = false,
                        1 => window.first = 0x1000,
                        2 => window.last = 0x1001,
                        _ => window.base = 1,
                    }
                    let mut runner = Runner::new(&p, inline);
                    runner.set(&p, initial(), &d);
                    runner.assert_refused(initial());
                }
            }
        }
    }
}

#[test]
fn rmw_rejects_malformed_ram_and_generation_tables_and_both_protected_or_exhausted_pages() {
    for op in representative_operations() {
        let p = single(op);
        for inline in [false, true] {
            for (offset, value) in [
                (memory::VERSION_OFFSET, memory::VERSION + 1),
                (memory::RAM_POINTER, u32::MAX),
                (memory::RAM_POINTER, 65536 - RAM_LENGTH as u32 + 1),
                (memory::RAM_BASE, RAM_BASE - 1),
                (memory::RAM_LENGTH, u32::MAX),
                (memory::GENERATIONS_POINTER, u32::MAX),
                (memory::GENERATIONS_POINTER, 65536 - PAGE_COUNT * 8 + 1),
                (memory::GENERATIONS_LENGTH, 1),
                (memory::CODE_PAGES_POINTER, u32::MAX),
                (memory::CODE_PAGES_POINTER, 65536 - PAGE_COUNT + 1),
                (memory::CODE_PAGES_LENGTH, 1),
            ] {
                let mut runner = Runner::new(&p, inline);
                runner.set(&p, initial(), &descriptor());
                runner.word(offset, value);
                runner.assert_refused(initial());
            }
            for page in [0, 1] {
                for protected in [false, true] {
                    let mut runner = Runner::new(&p, inline);
                    runner.set(&p, initial(), &descriptor());
                    if protected {
                        runner.bytes()[CODE_PAGES + page] = 1;
                    } else {
                        runner.set_generation(page as u32, u64::MAX);
                    }
                    runner.assert_refused(initial());
                }
            }
        }
    }
}

#[test]
fn rmw_rejects_unmapped_mmio_and_cross_span_operands_before_latches_reads_or_admission() {
    for linear in [0x4ff, 0x44fe, 0x4500, 0x9ffff, 0xa0000, u32::MAX - 1] {
        for kind in UNARY {
            for inline in [false, true] {
                let p = single(Operation::UnaryMemory {
                    kind,
                    width: Width::Dword,
                    address: address(linear),
                });
                let mut d = descriptor();
                d.device = memory::DeviceReadDescriptor {
                    guest_start: 0xa0000,
                    guest_length: 0x10000,
                    pointer: 32768,
                    backing_length: 8192,
                    kind: memory::DeviceReadKind::FourLanes,
                    latches: [90; 4],
                    ..memory::DeviceReadDescriptor::default()
                };
                if (0x9ffff..=0xa0000).contains(&linear) {
                    // Even RAM which advertises the whole aperture cannot
                    // bypass MMIO, including a crossing from ordinary RAM.
                    d.ram.linear_base = 0x9f000;
                }
                let mut runner = Runner::new(&p, inline);
                runner.set(&p, initial(), &d);
                runner.bytes()[32768..40960].fill(0xb7);
                runner.assert_refused(initial());
                assert_eq!(runner.effects().latches, [90; 4]);
            }
        }
    }
    for op in representative_operations() {
        let p = single(op);
        let mut d = descriptor();
        d.ram.excluded_start = 0x1001;
        d.ram.excluded_length = 1;
        let mut runner = Runner::new(&p, true);
        runner.set(&p, initial(), &d);
        runner.assert_refused(initial());
    }
}

#[test]
fn rmw_source_guard_and_runtime_admission_refusals_preserve_every_byte() {
    for op in representative_operations() {
        let p = single(op);
        let mut runner = Runner::new(&p, true);
        runner.set(&p, initial(), &descriptor());
        runner.guard(
            0,
            RegionGuard {
                valid: false,
                ..RegionGuard::default()
            },
        );
        runner.assert_refused(initial());
        let mut runner = Runner::new(&p, false);
        runner.set(&p, initial(), &descriptor());
        runner.store.data_mut().refuse = true;
        let before = runner.bytes().to_vec();
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        assert_eq!(runner.bytes(), before);
        assert_eq!(runner.store.data().admissions, vec![(0, 1)]);
    }
}

#[test]
fn rmw_updates_generations_before_the_next_guard_and_flushes_prior_work_on_refusal() {
    for op in representative_operations() {
        let mut p = single(op);
        p.regions[0].terminator = Terminator::Fallthrough(RegionId(1));
        p.regions.push(Region {
            id: RegionId(1),
            entry: entry(1),
            operations: vec![Operation::Inc(Register::Ecx)],
            terminator: Terminator::Fallthrough(RegionId(0)),
            accounting: Accounting::Counted,
        });
        let frame = Frame {
            remaining_budget: 2,
            ..initial()
        };
        let mut runner = Runner::new(&p, true);
        runner.set(&p, frame, &descriptor());
        runner.guard(
            1,
            RegionGuard {
                valid: true,
                pages: [1, 1],
                generations: [12, 12],
            },
        );
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        assert_eq!(runner.generation(0), 12);
        assert_eq!(runner.generation(1), 13);
        assert_eq!(runner.frame().gpr, frame.gpr);
        assert_eq!(runner.frame().address, entry(1));
        assert_eq!(runner.frame().charged_work, 1);
        assert_eq!(runner.frame().remaining_budget, 1);
        assert_eq!(runner.effects().ram_writes, 2);
        assert_eq!(runner.effects().inline_reserved, 1);

        // A failed RMW reached after successful register work must flush at
        // that RMW entry and leave its flags, RAM and generations untouched.
        p.regions.swap(0, 1);
        p.regions[0].id = RegionId(0);
        p.regions[0].entry = entry(0);
        p.regions[0].terminator = Terminator::Fallthrough(RegionId(1));
        p.regions[1].id = RegionId(1);
        p.regions[1].entry = entry(1);
        p.regions[1].terminator = Terminator::Fallthrough(RegionId(0));
        let mut runner = Runner::new(&p, true);
        runner.set(&p, frame, &descriptor());
        runner.set_generation(1, u64::MAX);
        let before = runner.bytes()[RAM..RAM + RAM_LENGTH].to_vec();
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        assert_eq!(&runner.bytes()[RAM..RAM + RAM_LENGTH], before);
        assert_eq!(runner.generation(0), 11);
        assert_eq!(runner.generation(1), u64::MAX);
        assert_eq!(runner.frame().gpr[1], frame.gpr[1] + 1);
        assert_eq!(runner.frame().address, entry(1));
        assert_eq!(runner.frame().charged_work, 1);
        assert_eq!(runner.frame().remaining_budget, 1);
        assert_eq!(runner.effects().ram_writes, 0);
        assert_eq!(runner.effects().inline_reserved, 1);
    }
}

#[test]
fn rmw_accepts_matching_nonzero_segment_bases_and_exits_at_a_fallback_boundary() {
    for kind in UNARY {
        let mut p = single(Operation::UnaryMemory {
            kind,
            width: Width::Dword,
            address: MemoryAddress {
                segment: Segment::Gs,
                ..address(0x103)
            },
        });
        let boundary = Address {
            space: 7,
            offset: 0x1234,
        };
        p.regions[0].terminator = Terminator::Fallback(boundary);
        let mut d = descriptor();
        for access in 0..2 {
            d.windows[access][Segment::Gs as usize].base = 0x800;
        }
        let mut runner = Runner::new(&p, true);
        let frame = Frame {
            remaining_budget: 2,
            ..initial()
        };
        runner.set(&p, frame, &d);
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        assert_eq!(runner.frame().gpr, initial().gpr);
        assert_eq!(runner.frame().address, boundary);
        assert_eq!(runner.frame().charged_work, 1);
        assert_eq!(runner.frame().remaining_budget, 1);
        assert_eq!(runner.effects().ram_writes, 1);
        assert_eq!(runner.effects().inline_reserved, 1);
        let old = 0x5a5a_5a5au32;
        let expected = match kind {
            MemoryUnary::Inc => old + 1,
            MemoryUnary::Dec => old - 1,
            MemoryUnary::Neg => old.wrapping_neg(),
            MemoryUnary::Not => !old,
        };
        assert_eq!(
            &runner.bytes()[RAM + 0x403..RAM + 0x407],
            &expected.to_le_bytes()
        );
        assert_eq!(runner.generation(0), 12);
        assert_eq!(
            runner.store.data().callbacks,
            vec![(abi::REASON_FALLBACK, runner.frame())]
        );
    }
}

#[test]
fn rmw_ir_rejects_read_only_alu_kinds_invalid_sources_addresses_and_mixed_hot_regions() {
    for op in representative_operations() {
        let original = single(op);
        assert_eq!(
            original.compile(&CompileOptions::default()).unwrap_err(),
            CompileError::DirectMemoryDisabled(RegionId(0))
        );
        let mut mixed = original.clone();
        mixed.regions[0].operations.push(Operation::Nop);
        assert_eq!(
            mixed.compile(&options(true)).unwrap_err(),
            CompileError::MixedMemoryRegion(RegionId(0))
        );
        let mut hot = original.clone();
        hot.regions[0].accounting = Accounting::UncountedHot;
        assert_eq!(
            hot.compile(&CompileOptions {
                direct_memory: true,
                ..CompileOptions::default()
            })
            .unwrap_err(),
            CompileError::MemoryUncountedHot(RegionId(0))
        );
        let mut jump = original;
        jump.regions[0].terminator = Terminator::Jump(RegionId(0));
        assert_eq!(
            jump.compile(&options(true)).unwrap_err(),
            CompileError::MixedMemoryRegion(RegionId(0))
        );
    }
    for kind in [Alu::Cmp, Alu::Test] {
        let p = single(Operation::AluMemoryStore {
            kind,
            width: Width::Dword,
            address: address(0x903),
            source: Source::Immediate(0),
        });
        assert_eq!(
            p.compile(&options(true)).unwrap_err(),
            CompileError::InvalidMemoryOperation(RegionId(0))
        );
    }
    for (source, error) in [
        (
            Source::Register(Register::Ax),
            CompileError::WidthMismatch(RegionId(0)),
        ),
        (
            Source::Immediate(256),
            CompileError::ImmediateOutOfRange(RegionId(0)),
        ),
    ] {
        let p = single(Operation::AluMemoryStore {
            kind: Alu::Add,
            width: Width::Byte,
            address: address(0x903),
            source,
        });
        assert_eq!(p.compile(&options(true)).unwrap_err(), error);
    }
    for invalid in [
        MemoryAddress {
            scale: 3,
            ..address(0x903)
        },
        MemoryAddress {
            scale: 2,
            address16: true,
            ..address(0x903)
        },
        MemoryAddress {
            base: Some(Register::Ax),
            ..address(0x903)
        },
        MemoryAddress {
            index: Some(Register::Ah),
            ..address(0x903)
        },
    ] {
        let p = single(Operation::UnaryMemory {
            kind: MemoryUnary::Dec,
            width: Width::Dword,
            address: invalid,
        });
        assert_eq!(
            p.compile(&options(true)).unwrap_err(),
            CompileError::InvalidMemoryAddress(RegionId(0))
        );
    }
}

#[test]
fn warmed_inline_rmw_execution_allocates_nothing() {
    for op in representative_operations() {
        let p = single(op);
        let mut runner = Runner::new(&p, true);
        runner.set(&p, initial(), &descriptor());
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        let info = allocation_counter::measure(|| {
            for _ in 0..8 {
                initial().write(&mut runner.bytes()[FRAME..]).unwrap();
                assert_eq!(runner.call(), abi::EXIT_BUDGET);
            }
        });
        assert_eq!(info.count_total, 0);
    }
}
