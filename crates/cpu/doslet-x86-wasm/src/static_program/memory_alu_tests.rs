use super::*;
use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_x86::{Cpu, DescriptorTable, Gpr32, SegmentRegister};

#[path = "memory_extend_tests.rs"]
mod memory_extend_tests;
#[path = "memory_multiply_tests.rs"]
mod memory_multiply_tests;
#[path = "memory_wide_multiply_tests.rs"]
mod memory_wide_multiply_tests;

const GENERATIONS: usize = 24576;
const GUARDS: usize = 25088;
const BASE_VALUE: u32 = 0x0271_8503;
const INDEX_VALUE: u32 = 0x0123_4001;
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
const KINDS: [Alu; 9] = [
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

fn single(op: Operation) -> Program {
    let mut p = program(Register::Eax, address(0));
    p.regions[0].operations[0] = op;
    p
}

fn operands(op: Operation) -> (Width, MemoryAddress) {
    match op {
        Operation::AluMemory {
            target, address, ..
        }
        | Operation::SignedMultiplyMemory {
            target, address, ..
        } => (target.width(), address),
        Operation::CompareMemory { width, address, .. } => (width, address),
        Operation::ExtendMemory { width, address, .. } => (width, address),
        Operation::WideMultiplyMemory { width, address, .. } => (width, address),
        _ => unreachable!("memory ALU fixture"),
    }
}

fn register_code(r: Register) -> u8 {
    (r.index() + if r.shift() == 8 { 4 } else { 0 }) as u8
}

// Independent x86 fixture encoding: use a disp32 SIB so base and index can
// alias any operand, including ESP/EBP. The reference CPU decodes real bytes.
fn encoding(op: Operation, code32: bool) -> Vec<u8> {
    let (width, a) = operands(op);
    let mut out = Vec::new();
    if matches!(
        op,
        Operation::SignedMultiplyMemory { .. } | Operation::WideMultiplyMemory { .. }
    ) {
        out.push(match a.segment {
            Segment::Es => 0x26,
            Segment::Cs => 0x2e,
            Segment::Ss => 0x36,
            Segment::Ds => 0x3e,
            Segment::Fs => 0x64,
            Segment::Gs => 0x65,
        });
    }
    if a.address16 == code32 {
        out.push(0x67);
    }
    let operand_width = match op {
        Operation::ExtendMemory { target, .. } => target.width(),
        _ => width,
    };
    if operand_width != Width::Byte && (operand_width == Width::Dword) != code32 {
        out.push(0x66);
    }
    let word = u8::from(width != Width::Byte);
    let (opcode, field, immediate) = match op {
        Operation::WideMultiplyMemory { signed, .. } => {
            (0xf6 + word, if signed { 5 } else { 4 }, None)
        }
        Operation::SignedMultiplyMemory { target, source, .. } => match source {
            Source::Register(r) => {
                assert_eq!(r, target);
                out.push(0x0f);
                (0xaf, register_code(target), None)
            }
            Source::Immediate(v) => (0x69, register_code(target), Some(v)),
            Source::VirtualRegister(_) => unreachable!("test source"),
        },
        Operation::ExtendMemory { target, signed, .. } => {
            out.push(0x0f);
            (
                0xb6 | u8::from(width == Width::Word) | (u8::from(signed) << 3),
                register_code(target),
                None,
            )
        }
        Operation::AluMemory { kind, target, .. } => {
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
            (
                if kind == Alu::Test {
                    0x84 + word
                } else {
                    group * 8 + 2 + word
                },
                register_code(target),
                None,
            )
        }
        Operation::CompareMemory {
            test,
            source: Source::Register(r),
            ..
        } => (
            if test { 0x84 + word } else { 0x38 + word },
            register_code(r),
            None,
        ),
        Operation::CompareMemory {
            test,
            source: Source::Immediate(v),
            ..
        } => (
            if test { 0xf6 + word } else { 0x80 + word },
            if test { 0 } else { 7 },
            Some(v),
        ),
        _ => unreachable!("memory ALU fixture"),
    };
    out.push(opcode);
    if a.address16 {
        assert_eq!(
            (a.base, a.index),
            (Some(Register::Ebx), Some(Register::Esi))
        );
        out.push(0x80 | field << 3); // [BX + SI + disp16]
        out.extend_from_slice(&(a.displacement as u16).to_le_bytes());
    } else if a.base.is_none() && a.index.is_none() {
        out.push(field << 3 | 5);
    } else {
        out.push(if a.base.is_some() { 0x80 } else { 0 } | field << 3 | 4);
        out.push(
            (a.scale.trailing_zeros() as u8) << 6
                | a.index.map_or(4, register_code) << 3
                | a.base.map_or(5, register_code),
        );
    }
    if !a.address16 {
        out.extend_from_slice(&a.displacement.to_le_bytes());
    }
    if let Some(value) = immediate {
        out.extend_from_slice(&value.to_le_bytes()[..width.bits() as usize / 8]);
    }
    out
}

/// A byte bus for the independently decoded CPU. Device bytes use an explicit
/// planar/chain4 model; instruction fetches never count as device reads.
struct Bus {
    ram: Vec<u8>,
    device: DeviceReadDescriptor,
    vram: Vec<u8>,
    reads: u64,
    writes: u32,
}
impl MemoryBus for Bus {
    fn read_u8(&mut self, a: u32) -> Result<u8, BusError> {
        let d = &mut self.device;
        if a >= d.guest_start && a - d.guest_start < d.guest_length {
            let offset = (a - d.guest_start) as usize;
            self.reads += 1;
            if d.kind == DeviceReadKind::Linear {
                return Ok(self.vram[offset + d.base_offset as usize]);
            }
            let group = if d.chain4 { offset & !3 } else { offset * 4 };
            d.latches.copy_from_slice(&self.vram[group..group + 4]);
            if d.compare_enabled {
                let mut value = 255;
                for (plane, latch) in d.latches.iter().enumerate() {
                    if d.care & (1 << plane) != 0 {
                        value &= if d.compare & (1 << plane) == 0 {
                            !latch
                        } else {
                            *latch
                        };
                    }
                }
                Ok(value)
            } else {
                Ok(d.latches[if d.chain4 {
                    offset & 3
                } else {
                    d.read_plane as usize
                }])
            }
        } else {
            self.ram
                .get(a as usize)
                .copied()
                .ok_or(BusError::UnmappedMemory(a))
        }
    }
    fn write_u8(&mut self, a: u32, v: u8) -> Result<(), BusError> {
        self.writes += 1;
        *self
            .ram
            .get_mut(a as usize)
            .ok_or(BusError::UnmappedMemory(a))? = v;
        Ok(())
    }
    fn code_bytes(&self, a: u32, len: u8) -> Option<&[u8]> {
        self.ram.get(a as usize..a as usize + usize::from(len))
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

fn reference(code: &[u8], descriptor: &Descriptor) -> (Cpu, Bus) {
    let mut bus = Bus {
        ram: vec![0; 65536],
        device: descriptor.device,
        vram: (0..4096)
            .map(|i| (i as u8).wrapping_mul(53).wrapping_add(17))
            .collect(),
        reads: 0,
        writes: 0,
    };
    bus.ram[0x1000..0x1000 + code.len()].copy_from_slice(code);
    bus.ram[0x208..0x218].copy_from_slice(&[
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
    for s in [SegmentRegister::Ds, SegmentRegister::Ss] {
        cpu.load_segment(&mut bus, s, 16).unwrap();
    }
    bus.writes = 0;
    (cpu, bus)
}

fn put(gpr: &mut [u32; 8], r: Register, value: u32) {
    let old = &mut gpr[r.index() as usize];
    *old = (*old & !(r.width().mask() << r.shift())) | ((value & r.width().mask()) << r.shift());
}

fn read_proofs(runner: &mut Runner, descriptor: &mut Descriptor) {
    descriptor.ram.generations_pointer = GENERATIONS as u32;
    descriptor.ram.generations_length = 2;
    descriptor.inline_grant = 100;
    descriptor.region_guards_pointer = GUARDS as u32;
    descriptor.region_guards_length = 2;
    runner.bytes()[GENERATIONS..GENERATIONS + 16].copy_from_slice(&[0x3a; 16]);
    memory::RegionGuard {
        valid: true,
        pages: [0, 0],
        generations: [0x3a3a_3a3a_3a3a_3a3a; 2],
    }
    .write(&mut runner.bytes()[GUARDS + memory::REGION_GUARD_BYTES..])
    .unwrap();
}

// Callers choose a displacement matching the fixed old base/index values, so
// operand aliases still select one known byte while other GPRs vary.
fn differential(op: Operation, inline: bool, descriptor: Descriptor, samples: usize) {
    differential_mode(op, inline, descriptor, samples, true);
}

fn differential_mode(
    op: Operation,
    inline: bool,
    mut descriptor: Descriptor,
    samples: usize,
    code32: bool,
) {
    let p = single(op);
    let mut runner = Runner::with_options(
        &p,
        CompileOptions {
            direct_memory: true,
            runtime_admission: !inline,
            inline_admission: inline,
            ..CompileOptions::default()
        },
    );
    read_proofs(&mut runner, &mut descriptor);
    let (mut cpu, mut bus) = reference(&encoding(op, code32), &descriptor);
    if !code32 {
        bus.ram[0x20e] = 0x8f;
        cpu.load_segment(&mut bus, SegmentRegister::Cs, 8).unwrap();
    }
    let (width, a) = operands(op);
    if matches!(
        op,
        Operation::SignedMultiplyMemory { .. } | Operation::WideMultiplyMemory { .. }
    ) {
        let base = descriptor.windows[0][a.segment as usize].base;
        bus.ram[0x212..0x214].copy_from_slice(&(base as u16).to_le_bytes());
        bus.ram[0x214] = (base >> 16) as u8;
        bus.ram[0x217] = (base >> 24) as u8;
        let segment = match a.segment {
            Segment::Es => SegmentRegister::Es,
            Segment::Ss => SegmentRegister::Ss,
            Segment::Ds => SegmentRegister::Ds,
            Segment::Fs => SegmentRegister::Fs,
            Segment::Gs => SegmentRegister::Gs,
            Segment::Cs => unreachable!("data-source fixture"),
        };
        cpu.load_segment(&mut bus, segment, 16).unwrap();
        bus.writes = 0;
    }
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
    let mut seed = 0x9137_b25au32;
    for sample in 0..samples {
        for carry in 0..=1 {
            let mut random = || {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                seed
            };
            let mut gpr = core::array::from_fn(|_| random());
            let value = if sample < 64 {
                boundaries[sample / 8]
            } else {
                random()
            };
            let memory_value = if sample < 64 {
                boundaries[sample % 8]
            } else {
                random()
            };
            match op {
                Operation::WideMultiplyMemory { width, .. } => put(
                    &mut gpr,
                    match width {
                        Width::Byte => Register::Al,
                        Width::Word => Register::Ax,
                        Width::Dword => Register::Eax,
                    },
                    value,
                ),
                Operation::AluMemory { target, .. }
                | Operation::ExtendMemory { target, .. }
                | Operation::SignedMultiplyMemory { target, .. } => put(&mut gpr, target, value),
                Operation::CompareMemory {
                    source: Source::Register(r),
                    ..
                } => put(&mut gpr, r, value),
                _ => {}
            }
            // Fixed old base/index values make SIB aliases observable while
            // keeping the tested operand inside the borrowed RAM/device span.
            if let Some(base) = a.base {
                gpr[base.index() as usize] = BASE_VALUE;
            }
            if let Some(index) = a.index {
                gpr[index.index() as usize] = INDEX_VALUE;
            }
            let mut linear = a
                .displacement
                .wrapping_add(a.base.map_or(0, |r| gpr[r.index() as usize]))
                .wrapping_add(
                    a.index
                        .map_or(0, |r| gpr[r.index() as usize])
                        .wrapping_mul(u32::from(a.scale)),
                );
            if a.address16 {
                linear &= 0xffff;
            }
            linear = linear.wrapping_add(descriptor.windows[0][a.segment as usize].base);
            let frame = Frame {
                gpr,
                eflags: 0xa550_0cd4 | carry,
                charged_work: 39,
                status: 0x8000_0000,
                ..initial()
            };
            runner.set(frame, &descriptor);
            runner.bytes()[VRAM..VRAM + bus.vram.len()].copy_from_slice(&bus.vram);
            if descriptor.device.guest_length == 0 {
                let offset = RAM + (linear - descriptor.ram.linear_base) as usize;
                runner.bytes()[offset..offset + 4].copy_from_slice(&memory_value.to_le_bytes());
                bus.ram[linear as usize..linear as usize + 4]
                    .copy_from_slice(&memory_value.to_le_bytes());
            }
            // Start counters above u32 to catch accidental truncation, and keep
            // a nonzero staged write count to detect read-path write effects.
            let effect_base = FRAME + memory::OFFSET as usize;
            runner.bytes()[effect_base + memory::RAM_WRITES as usize
                ..effect_base + memory::RAM_WRITES as usize + 8]
                .copy_from_slice(&0x1_0000_0007u64.to_le_bytes());
            runner.bytes()[effect_base + memory::DEVICE_READS as usize
                ..effect_base + memory::DEVICE_READS as usize + 8]
                .copy_from_slice(&0x1_ffff_fffeu64.to_le_bytes());
            let before_ram = runner.bytes()[RAM..RAM + 0x2000].to_vec();
            let before_vram = runner.bytes()[VRAM..VRAM + 4096].to_vec();
            bus.reads = 0;
            bus.writes = 0;
            bus.device.latches = descriptor.device.latches;
            cpu.registers.ip = 0x1000;
            cpu.flags = frame.eflags;
            for (r, v) in REGS.into_iter().zip(gpr) {
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
                "{op:?}, sample={sample}, CF={carry}"
            );
            let effects = runner.effects();
            assert_eq!(effects.device_reads, 0x1_ffff_fffe + bus.reads, "{op:?}");
            assert_eq!(effects.latches, bus.device.latches, "{op:?}");
            assert_eq!(effects.ram_writes, 0x1_0000_0007);
            assert_eq!(effects.inline_reserved, u32::from(inline));
            assert_eq!(effects.inline_remaining, 100 - u32::from(inline));
            assert_eq!(runner.bytes()[RAM..RAM + 0x2000], before_ram);
            assert_eq!(runner.bytes()[VRAM..VRAM + 4096], before_vram);
            assert_eq!(runner.bytes()[GENERATIONS..GENERATIONS + 16], [0x3a; 16]);
            assert_eq!(bus.writes, 0);
            assert!(runner.store.data().callbacks.is_empty());
        }
    }
    assert_eq!(
        runner.store.data().admissions.len(),
        if inline { 0 } else { samples * 2 }
    );
    assert!(runner.store.data().admissions.iter().all(|&v| v == (1, 1)));
}

#[test]
fn read_alu_matches_reference_for_every_alias_kind_boundary_pair_and_carry() {
    for target in ALIASES {
        for kind in KINDS {
            differential(
                Operation::AluMemory {
                    kind,
                    target,
                    address: address(0x503),
                },
                false,
                descriptor(),
                80,
            );
        }
    }
}

#[test]
fn read_alu_snapshots_target_base_and_index_aliases_with_inline_admission() {
    for target in ALIASES {
        let base = ALIASES[target.index() as usize];
        let a = MemoryAddress {
            base: Some(base),
            index: Some(Register::Edx),
            scale: 4,
            // EDX as both base/index is intentionally included.
            ..address(
                0x503u32
                    .wrapping_sub(if base == Register::Edx {
                        INDEX_VALUE
                    } else {
                        BASE_VALUE
                    })
                    .wrapping_sub(INDEX_VALUE * 4),
            )
        };
        for kind in KINDS {
            differential(
                Operation::AluMemory {
                    kind,
                    target,
                    address: a,
                },
                true,
                descriptor(),
                8,
            );
        }
        for test in [false, true] {
            differential(
                Operation::CompareMemory {
                    test,
                    address: a,
                    width: target.width(),
                    source: Source::Register(target),
                },
                true,
                descriptor(),
                8,
            );
        }
    }
}

#[test]
fn memory_alu_address16_wraps_before_loading_and_preserves_partial_registers() {
    let a = MemoryAddress {
        base: Some(Register::Ebx),
        index: Some(Register::Esi),
        address16: true,
        ..address(0x503u32.wrapping_sub(BASE_VALUE).wrapping_sub(INDEX_VALUE))
    };
    for target in [
        Register::Ah,
        Register::Cl,
        Register::Bx,
        Register::Si,
        Register::Ebx,
    ] {
        for kind in KINDS {
            differential(
                Operation::AluMemory {
                    kind,
                    target,
                    address: a,
                },
                true,
                descriptor(),
                8,
            );
        }
        for test in [false, true] {
            differential(
                Operation::CompareMemory {
                    test,
                    address: a,
                    width: target.width(),
                    source: Source::Register(target),
                },
                false,
                descriptor(),
                8,
            );
        }
    }
}

#[test]
fn memory_left_cmp_test_matches_reference_for_all_register_aliases_and_immediates() {
    for test in [false, true] {
        for source in ALIASES.into_iter().map(Source::Register).chain([
            Source::Immediate(0),
            Source::Immediate(0x81),
            Source::Immediate(0xff),
        ]) {
            let widths = match source {
                Source::Register(r) => vec![r.width()],
                Source::Immediate(_) => vec![Width::Byte, Width::Word, Width::Dword],
                Source::VirtualRegister(_) => unreachable!("test source"),
            };
            for width in widths {
                differential(
                    Operation::CompareMemory {
                        test,
                        address: address(0x503),
                        width,
                        source,
                    },
                    true,
                    descriptor(),
                    80,
                );
            }
        }
        for width in [Width::Word, Width::Dword] {
            differential(
                Operation::CompareMemory {
                    test,
                    address: address(0x503),
                    width,
                    source: Source::Immediate(width.mask()),
                },
                false,
                descriptor(),
                80,
            );
        }
    }
}

#[test]
fn memory_alu_planar_chain4_and_compare_reads_match_reference_effects() {
    for chain4 in [false, true] {
        for compare in [false, true] {
            for target in [Register::Ah, Register::Cl, Register::Ax, Register::Eax] {
                let mut d = device(DeviceReadKind::FourLanes);
                d.device.chain4 = chain4;
                d.device.read_plane = 2;
                d.device.compare_enabled = compare;
                d.device.compare = 5;
                d.device.care = 13;
                for kind in KINDS {
                    differential(
                        Operation::AluMemory {
                            kind,
                            target,
                            address: address(0xa0003),
                        },
                        true,
                        d,
                        8,
                    );
                }
                for test in [false, true] {
                    for source in [
                        Source::Register(target),
                        Source::Immediate(target.width().mask()),
                    ] {
                        differential(
                            Operation::CompareMemory {
                                test,
                                address: address(0xa0003),
                                width: target.width(),
                                source,
                            },
                            false,
                            d,
                            8,
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn memory_alu_rejects_complete_spans_before_admission_or_any_effect() {
    for memory_left in [false, true] {
        for case in 0..8 {
            let mut d = device(DeviceReadKind::FourLanes);
            let mut offset = 0xa0003;
            match case {
                0 => d.windows[0][Segment::Ds as usize].valid = false,
                1 => d.windows[0][Segment::Ds as usize].last = offset + 2,
                2 => d.device.guest_length = 6,
                3 => d.device.backing_length = 27,
                4 => d.device.pointer = 65520,
                5 => {
                    offset = 0x503;
                    d.ram.pointer = 65534;
                }
                6 => {
                    offset = 0x24fe;
                }
                7 => {
                    offset = u32::MAX - 1;
                }
                _ => unreachable!(),
            }
            let op = if memory_left {
                Operation::CompareMemory {
                    test: false,
                    address: address(offset),
                    width: Width::Dword,
                    source: Source::Register(Register::Ecx),
                }
            } else {
                Operation::AluMemory {
                    kind: Alu::Adc,
                    target: Register::Eax,
                    address: address(offset),
                }
            };
            for inline in [false, true] {
                let mut runner = Runner::with_options(
                    &single(op),
                    CompileOptions {
                        direct_memory: true,
                        runtime_admission: !inline,
                        inline_admission: inline,
                        ..CompileOptions::default()
                    },
                );
                read_proofs(&mut runner, &mut d);
                runner.set(initial(), &d);
                runner.bytes()[VRAM..VRAM + 64].fill(0xa5);
                let before = runner.bytes().to_vec();
                refused(&mut runner, initial(), 0);
                assert_eq!(runner.bytes(), before);
            }
        }
    }
}

#[test]
fn memory_alu_admission_refusal_precedes_device_reads_and_preserves_registers() {
    for op in [
        Operation::AluMemory {
            kind: Alu::Sbb,
            target: Register::Ah,
            address: address(0xa0003),
        },
        Operation::CompareMemory {
            test: true,
            address: address(0xa0003),
            width: Width::Dword,
            source: Source::Register(Register::Ecx),
        },
    ] {
        let mut runner = Runner::new(&single(op), true);
        runner.set(initial(), &device(DeviceReadKind::FourLanes));
        runner.store.data_mut().refuse = true;
        let before = runner.bytes().to_vec();
        refused(&mut runner, initial(), 1);
        assert_eq!(runner.bytes(), before);
    }
}

#[test]
fn memory_alu_validation_rejects_disabled_mixed_hot_addresses_widths_and_immediates() {
    for op in [
        Operation::AluMemory {
            kind: Alu::Cmp,
            target: Register::Al,
            address: address(0x503),
        },
        Operation::CompareMemory {
            test: true,
            address: address(0x503),
            width: Width::Byte,
            source: Source::Immediate(0xff),
        },
    ] {
        let mut p = single(op);
        assert_eq!(
            p.compile(&CompileOptions::default()).unwrap_err(),
            CompileError::DirectMemoryDisabled(RegionId(1))
        );
        p.regions[0].operations.push(Operation::Nop);
        assert_eq!(
            p.compile(&options(false)).unwrap_err(),
            CompileError::MixedMemoryRegion(RegionId(1))
        );
        p.regions[0].operations.pop();
        p.regions[0].accounting = Accounting::UncountedHot;
        assert_eq!(
            p.compile(&options(false)).unwrap_err(),
            CompileError::MemoryUncountedHot(RegionId(1))
        );
    }
    for test in [false, true] {
        for (width, source, error) in [
            (
                Width::Byte,
                Source::Register(Register::Ax),
                CompileError::WidthMismatch(RegionId(1)),
            ),
            (
                Width::Word,
                Source::Register(Register::Ecx),
                CompileError::WidthMismatch(RegionId(1)),
            ),
            (
                Width::Byte,
                Source::Immediate(256),
                CompileError::ImmediateOutOfRange(RegionId(1)),
            ),
            (
                Width::Word,
                Source::Immediate(65536),
                CompileError::ImmediateOutOfRange(RegionId(1)),
            ),
        ] {
            let op = Operation::CompareMemory {
                test,
                address: address(0x503),
                width,
                source,
            };
            assert_eq!(single(op).compile(&options(false)).unwrap_err(), error);
        }
    }
    for a in [
        MemoryAddress {
            base: Some(Register::Ah),
            ..address(0)
        },
        MemoryAddress {
            scale: 3,
            ..address(0)
        },
    ] {
        for op in [
            Operation::AluMemory {
                kind: Alu::Add,
                target: Register::Eax,
                address: a,
            },
            Operation::CompareMemory {
                test: false,
                address: a,
                width: Width::Dword,
                source: Source::Immediate(0),
            },
        ] {
            assert_eq!(
                single(op).compile(&options(false)).unwrap_err(),
                CompileError::InvalidMemoryAddress(RegionId(1))
            );
        }
    }
}
