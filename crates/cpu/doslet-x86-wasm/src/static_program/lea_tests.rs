use super::*;
use memory::{MemoryAddress, Segment};

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
const SEGMENTS: [Segment; 6] = [
    Segment::Es,
    Segment::Cs,
    Segment::Ss,
    Segment::Ds,
    Segment::Fs,
    Segment::Gs,
];

fn absolute(displacement: u32) -> MemoryAddress {
    MemoryAddress {
        base: None,
        index: None,
        scale: 1,
        displacement,
        address16: false,
        segment: Segment::Ds,
    }
}

struct Form {
    address: MemoryAddress,
    /// Original ModR/M, optional SIB and displacement; destination field is 0.
    bytes: Vec<u8>,
}

fn forms(address16: bool) -> Vec<Form> {
    let mut forms = Vec::new();
    if address16 {
        for (rm, base, index) in [
            (0, Some(Register::Ebx), Some(Register::Esi)),
            (1, Some(Register::Ebx), Some(Register::Edi)),
            (2, Some(Register::Ebp), Some(Register::Esi)),
            (3, Some(Register::Ebp), Some(Register::Edi)),
            (4, Some(Register::Esi), None),
            (5, Some(Register::Edi), None),
            (6, Some(Register::Ebp), None),
            (7, Some(Register::Ebx), None),
        ] {
            for mode in 0..3 {
                let displacement = match mode {
                    0 => 0,
                    1 => (-123_i32) as u32,
                    _ => 0x8123,
                };
                let mut bytes = vec![(mode << 6) | rm];
                match mode {
                    0 if rm == 6 => bytes.extend_from_slice(&0u16.to_le_bytes()),
                    0 => {}
                    1 => bytes.push(displacement as u8),
                    _ => bytes.extend_from_slice(&(displacement as u16).to_le_bytes()),
                }
                forms.push(Form {
                    address: MemoryAddress {
                        base: if mode == 0 && rm == 6 { None } else { base },
                        index,
                        address16: true,
                        ..absolute(displacement)
                    },
                    bytes,
                });
            }
        }
        forms.push(Form {
            address: MemoryAddress {
                address16: true,
                ..absolute(0xfeff)
            },
            bytes: vec![0x06, 0xff, 0xfe],
        });
    } else {
        for base in DWORDS {
            for displacement in [0, (-128_i32) as u32, 0x8000_0081] {
                let mode = if displacement == 0 && base != Register::Ebp {
                    0
                } else if displacement == 0 || displacement == (-128_i32) as u32 {
                    1
                } else {
                    2
                };
                let mut bytes = vec![(mode << 6) | register_code(base)];
                if base == Register::Esp {
                    bytes.push(0x24); // ESP base, absent SIB index.
                }
                if mode == 1 {
                    bytes.push(displacement as u8);
                } else if mode == 2 {
                    bytes.extend_from_slice(&displacement.to_le_bytes());
                }
                forms.push(Form {
                    address: MemoryAddress {
                        base: Some(base),
                        ..absolute(displacement)
                    },
                    bytes,
                });
            }
            for (shift, scale) in [1, 2, 4, 8].into_iter().enumerate() {
                let index = if base == Register::Esp {
                    Register::Ecx
                } else {
                    base
                };
                forms.push(Form {
                    address: MemoryAddress {
                        base: Some(base),
                        index: Some(index),
                        scale,
                        ..absolute((-7_i32) as u32)
                    },
                    bytes: vec![
                        0x44,
                        ((shift as u8) << 6) | (register_code(index) << 3) | register_code(base),
                        0xf9,
                    ],
                });
                if base != Register::Esp {
                    let mut bytes = vec![0x04, ((shift as u8) << 6) | register_code(base) << 3 | 5];
                    bytes.extend_from_slice(&0xffff_fffbu32.to_le_bytes());
                    forms.push(Form {
                        address: MemoryAddress {
                            index: Some(base),
                            scale,
                            ..absolute(0xffff_fffb)
                        },
                        bytes,
                    });
                }
            }
        }
        for mut bytes in [vec![0x05], vec![0x04, 0x25]] {
            bytes.extend_from_slice(&0xfedc_ba98u32.to_le_bytes());
            forms.push(Form {
                address: absolute(0xfedc_ba98),
                bytes,
            });
        }
    }
    for (index, form) in forms.iter_mut().enumerate() {
        form.address.segment = SEGMENTS[index % SEGMENTS.len()];
    }
    forms
}

fn encoding(target: Register, form: &Form, code32: bool) -> Vec<u8> {
    let mut code = vec![match form.address.segment {
        Segment::Es => 0x26,
        Segment::Cs => 0x2e,
        Segment::Ss => 0x36,
        Segment::Ds => 0x3e,
        Segment::Fs => 0x64,
        Segment::Gs => 0x65,
    }];
    if (target.width() == Width::Dword) != code32 {
        code.push(0x66);
    }
    if form.address.address16 == code32 {
        code.push(0x67);
    }
    code.push(0x8d);
    code.push(form.bytes[0] | register_code(target) << 3);
    code.extend_from_slice(&form.bytes[1..]);
    code
}

/// Once setup finishes, only instruction bytes exist on this reference bus.
/// Null data segments and an SS limit of zero must still allow every LEA.
struct CodeBus {
    bytes: Vec<u8>,
    code_end: usize,
    executing: bool,
}
impl MemoryBus for CodeBus {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        if self.executing && !(0x1000..self.code_end).contains(&(address as usize)) {
            return Err(BusError::UnmappedMemory(address));
        }
        self.bytes
            .get(address as usize)
            .copied()
            .ok_or(BusError::UnmappedMemory(address))
    }
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        assert!(!self.executing, "LEA wrote reference memory");
        *self
            .bytes
            .get_mut(address as usize)
            .ok_or(BusError::UnmappedMemory(address))? = value;
        Ok(())
    }
    fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
        let end = (address as usize).checked_add(usize::from(length))?;
        if self.executing
            && (!(0x1000..self.code_end).contains(&(address as usize)) || end > self.code_end)
        {
            return None;
        }
        self.bytes.get(address as usize..end)
    }
}
impl PortBus for CodeBus {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
        Err(BusError::UnmappedPort(port))
    }
    fn write_port_u8(&mut self, port: u16, _: u8) -> Result<(), BusError> {
        Err(BusError::UnmappedPort(port))
    }
}

fn reference(code: &[u8], code32: bool) -> (Cpu, CodeBus) {
    let mut bus = CodeBus {
        bytes: vec![0; 65536],
        code_end: 0x1000 + code.len(),
        executing: false,
    };
    bus.bytes[0x1000..bus.code_end].copy_from_slice(code);
    // Execute-only CS, and a one-byte SS window at a nonzero segment base.
    bus.bytes[0x208..0x218].copy_from_slice(&[
        0xff,
        0xff,
        0,
        0,
        0,
        0x98,
        if code32 { 0x40 } else { 0 },
        0,
        0,
        0,
        0x34,
        0x12,
        0x56,
        0x92,
        0,
        0x78,
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
    cpu.load_segment(&mut bus, SegmentRegister::Ss, 16).unwrap();
    for segment in [
        SegmentRegister::Ds,
        SegmentRegister::Es,
        SegmentRegister::Fs,
        SegmentRegister::Gs,
    ] {
        cpu.load_segment(&mut bus, segment, 0).unwrap();
        assert!(!cpu.registers.segment_cache(segment).usable);
    }
    assert_eq!(cpu.registers.segment_cache(SegmentRegister::Ss).limit, 0);
    bus.executing = true;
    (cpu, bus)
}

fn set_reference(cpu: &mut Cpu, frame: Frame) {
    cpu.registers.ip = 0x1000;
    cpu.flags = frame.eflags;
    for (register, value) in REGS.into_iter().zip(frame.gpr) {
        cpu.registers.set32(register, value);
    }
}

fn expected(cpu: &Cpu, frame: Frame) -> Frame {
    Frame {
        gpr: core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
        eflags: cpu.flags,
        remaining_budget: frame.remaining_budget.saturating_sub(1),
        charged_work: frame.charged_work + 1,
        ..frame
    }
}

#[test]
fn lea_all_targets_address_and_code_sizes_match_original_modrm_and_sib_cpu_steps() {
    let boundaries = [
        0,
        1,
        0xffff,
        0x1_0000,
        0x7fff_ffff,
        0x8000_0000,
        u32::MAX,
        0xa55a_8123,
    ];
    let mut seed = 0x5123_abcd_u32;
    for address16 in [false, true] {
        let forms = forms(address16);
        for target in DWORDS.into_iter().chain(WORDS) {
            let program = Program {
                regions: forms
                    .iter()
                    .enumerate()
                    .map(|(index, form)| {
                        let id = index as u32 + 1;
                        region(
                            id,
                            vec![Operation::Lea {
                                target,
                                address: form.address,
                            }],
                            Terminator::Fallthrough(RegionId(id)),
                        )
                    })
                    .collect(),
            };
            let mut runner = Runner::new(&program, CompileOptions::default());
            for code32 in [false, true] {
                for (index, form) in forms.iter().enumerate() {
                    let code = encoding(target, form, code32);
                    let (mut cpu, mut bus) = reference(&code, code32);
                    for sample in 0..12 {
                        let gpr = core::array::from_fn(|i| {
                            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                            match sample {
                                0..=3 => boundaries[sample * 2],
                                4..=7 => boundaries[(sample + i) % boundaries.len()],
                                _ => seed,
                            }
                        });
                        for flags in [0, 1, 0x8d4, 0x8d7] {
                            let initial = Frame {
                                gpr,
                                eflags: 0xa550_0400 | flags,
                                address: program.regions[index].entry,
                                remaining_budget: 1,
                                charged_work: 39,
                                status: 0x8000_0000,
                            };
                            runner.set(initial);
                            set_reference(&mut cpu, initial);
                            cpu.step(&mut bus).unwrap();
                            assert_eq!(runner.call(), abi::EXIT_BUDGET);
                            assert_eq!(
                                runner.frame(),
                                expected(&cpu, initial),
                                "target={target:?} address={:?} code32={code32} sample={sample}",
                                form.address
                            );
                            assert_eq!(runner.frame().eflags, initial.eflags);
                        }
                    }
                }
            }
            assert!(runner.store.data().events.is_empty());
            assert!(runner.store.data().admissions.is_empty());
            // The descriptor, RAM, device backing and reserved frame words all
            // retain the runner's garbage pattern after the whole matrix.
            for (i, &byte) in runner.memory.data(&runner.store).iter().enumerate() {
                if !(PTR..PTR + abi::FRAME_BYTES).contains(&i)
                    || (PTR + 44..PTR + 48).contains(&i)
                    || (PTR + 68..PTR + 72).contains(&i)
                {
                    assert_eq!(byte, 0x5a, "LEA changed memory at {i}");
                }
            }
        }
    }
}

#[test]
fn lea_masks_address_before_alias_write_and_snapshots_aliased_full_registers() {
    for (target, address, old, result) in [
        (
            Register::Ax,
            MemoryAddress {
                base: Some(Register::Eax),
                index: Some(Register::Eax),
                scale: 8,
                ..absolute(9)
            },
            0x8abc_fff1,
            0x8abc_ff82,
        ),
        (
            Register::Eax,
            MemoryAddress {
                base: Some(Register::Eax),
                index: Some(Register::Eax),
                address16: true,
                ..absolute(3)
            },
            0x8abc_ffff,
            1,
        ),
        (
            Register::Ax,
            MemoryAddress {
                base: Some(Register::Eax),
                ..absolute(5)
            },
            u32::MAX,
            0xffff_0004,
        ),
        (
            Register::Eax,
            MemoryAddress {
                index: Some(Register::Esp),
                scale: 4,
                ..absolute(8)
            },
            0xaaaa_ffff,
            4,
        ),
    ] {
        let program = single(Operation::Lea { target, address });
        let mut runner = Runner::new(&program, CompileOptions::default());
        let initial = Frame {
            gpr: [old, 0, 0, 0, u32::MAX, 0, 0, 0],
            address: program.regions[0].entry,
            remaining_budget: 1,
            eflags: 0xa550_0401,
            ..Frame::default()
        };
        runner.set(initial);
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        let mut expected = initial;
        expected.gpr[target.index() as usize] = result;
        expected.remaining_budget = 0;
        expected.charged_work = 1;
        assert_eq!(runner.frame(), expected);
    }
}

#[test]
fn lea_ignores_segments_and_direct_memory_option_without_new_imports_or_descriptor() {
    let mut baseline = None;
    for segment in SEGMENTS {
        let program = single(Operation::Lea {
            target: Register::Eax,
            address: MemoryAddress {
                segment,
                ..absolute(0xfedc_ba98)
            },
        });
        for direct_memory in [false, true] {
            let compiled = program
                .compile(&CompileOptions {
                    direct_memory,
                    ..CompileOptions::default()
                })
                .unwrap();
            if let Some(bytes) = &baseline {
                assert_eq!(compiled.wasm(), bytes);
            } else {
                baseline = Some(compiled.wasm().to_vec());
            }
            let mut config = Config::default();
            config.wasm_tail_call(true);
            let module = Module::new(&Engine::new(&config), compiled.wasm()).unwrap();
            assert_eq!(module.imports().count(), 2);
            let mut runner = Runner::with_compiled(&compiled);
            // Only the original CPU frame fits: reading even the first byte
            // of a descriptor or guest operand would trap at the memory end.
            let pointer = runner.memory.data(&runner.store).len() - abi::FRAME_BYTES;
            let initial = Frame {
                address: program.regions[0].entry,
                remaining_budget: 1,
                eflags: 0xdead_0401,
                ..Frame::default()
            };
            initial
                .write(&mut runner.memory.data_mut(&mut runner.store)[pointer..])
                .unwrap();
            assert_eq!(
                runner.run.call(&mut runner.store, pointer as u32).unwrap(),
                abi::EXIT_BUDGET
            );
            let mut expected = initial;
            expected.gpr[0] = 0xfedc_ba98;
            expected.remaining_budget = 0;
            expected.charged_work = 1;
            assert_eq!(
                Frame::read(&runner.memory.data(&runner.store)[pointer..]).unwrap(),
                expected
            );
            assert!(runner.store.data().events.is_empty());
            assert!(runner.store.data().admissions.is_empty());
        }
    }
}

#[test]
fn lea_mixed_register_region_matches_reference_with_soft_budget_overshoot() {
    let form = Form {
        address: MemoryAddress {
            base: Some(Register::Eax),
            index: Some(Register::Ecx),
            scale: 4,
            ..absolute((-7_i32) as u32)
        },
        bytes: vec![0x44, 0x88, 0xf9],
    };
    let operations = vec![
        Operation::Move {
            target: Register::Ecx,
            source: Source::Immediate(0x7fff_ffff),
        },
        Operation::Lea {
            target: Register::Ax,
            address: form.address,
        },
        Operation::Alu {
            kind: Alu::Adc,
            target: Register::Edi,
            source: Source::Register(Register::Eax),
        },
        Operation::Lea {
            target: Register::Eax,
            address: form.address,
        },
        Operation::Exchange {
            left: Register::Ah,
            right: Register::Al,
        },
    ];
    let mut code = Vec::new();
    for &op in &operations {
        if let Operation::Lea { target, .. } = op {
            code.extend(encoding(target, &form, true));
        } else {
            code.extend(super::encoding(op));
        }
    }
    let program = Program {
        regions: vec![region(1, operations, Terminator::Fallthrough(RegionId(1)))],
    };
    let initial = Frame {
        gpr: [0x9234_5678; 8],
        eflags: 0xa550_0cd5,
        address: program.regions[0].entry,
        remaining_budget: 1,
        charged_work: 71,
        status: 0x8000_0000,
    };
    let (mut cpu, mut bus) = reference(&code, true);
    set_reference(&mut cpu, initial);
    for _ in 0..5 {
        cpu.step(&mut bus).unwrap();
    }
    let mut runner = Runner::new(&program, CompileOptions::default());
    runner.set(initial);
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    assert_eq!(
        runner.frame(),
        Frame {
            charged_work: 76,
            ..expected(&cpu, initial)
        }
    );
    assert!(runner.store.data().events.is_empty());
}

#[test]
fn lea_hot_region_preserves_budget_and_uses_structural_service() {
    let form = Form {
        address: MemoryAddress {
            base: Some(Register::Eax),
            index: Some(Register::Eax),
            scale: 8,
            ..absolute(u32::MAX)
        },
        bytes: vec![0x44, 0xc0, 0xff],
    };
    let operations = vec![
        Operation::Lea {
            target: Register::Eax,
            address: form.address,
        },
        Operation::Not(Register::Edi),
    ];
    let mut r = region(1, operations, Terminator::Fallthrough(RegionId(1)));
    r.accounting = Accounting::UncountedHot;
    let program = Program { regions: vec![r] };
    let initial = Frame {
        gpr: [0x9234_5678; 8],
        eflags: 0xa550_0cd5,
        address: program.regions[0].entry,
        remaining_budget: 9,
        charged_work: 71,
        status: 0x8000_0000,
    };
    let mut code = encoding(Register::Eax, &form, true);
    code.extend(super::encoding(Operation::Not(Register::Edi)));
    let (mut cpu, mut bus) = reference(&code.repeat(3), true);
    set_reference(&mut cpu, initial);
    for _ in 0..6 {
        cpu.step(&mut bus).unwrap();
    }
    let mut runner = Runner::new(
        &program,
        CompileOptions {
            service_interval_regions: 3,
            ..CompileOptions::default()
        },
    );
    runner.set(initial);
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    let expected = Frame {
        gpr: core::array::from_fn(|i| cpu.registers.get32(REGS[i])),
        eflags: cpu.flags,
        status: initial.status | abi::UNCOUNTED_SEEN,
        ..initial
    };
    assert_eq!(runner.frame(), expected);
    assert_eq!(runner.frame().exact_total(), None);
    assert_eq!(
        runner.store.data().events,
        vec![(abi::REASON_SERVICE, expected)]
    );
    assert!(runner.store.data().admissions.is_empty());
}

#[test]
fn lea_rejects_byte_targets_invalid_scales_and_non_dword_address_registers() {
    for target in [
        Register::Al,
        Register::Cl,
        Register::Dl,
        Register::Bl,
        Register::Ah,
        Register::Ch,
        Register::Dh,
        Register::Bh,
    ] {
        assert_eq!(
            single(Operation::Lea {
                target,
                address: absolute(0)
            })
            .compile(&CompileOptions::default())
            .unwrap_err(),
            CompileError::WidthMismatch(RegionId(7))
        );
    }
    for address in [
        MemoryAddress {
            scale: 0,
            ..absolute(0)
        },
        MemoryAddress {
            scale: 3,
            ..absolute(0)
        },
        MemoryAddress {
            scale: 16,
            ..absolute(0)
        },
        MemoryAddress {
            address16: true,
            scale: 2,
            ..absolute(0)
        },
        MemoryAddress {
            address16: true,
            scale: 4,
            ..absolute(0)
        },
        MemoryAddress {
            address16: true,
            scale: 8,
            ..absolute(0)
        },
        MemoryAddress {
            base: Some(Register::Ax),
            ..absolute(0)
        },
        MemoryAddress {
            index: Some(Register::Ch),
            ..absolute(0)
        },
        MemoryAddress {
            base: Some(Register::Al),
            address16: true,
            ..absolute(0)
        },
        MemoryAddress {
            index: Some(Register::Si),
            address16: true,
            ..absolute(0)
        },
    ] {
        for direct_memory in [false, true] {
            assert_eq!(
                single(Operation::Lea {
                    target: Register::Eax,
                    address
                })
                .compile(&CompileOptions {
                    direct_memory,
                    ..CompileOptions::default()
                })
                .unwrap_err(),
                CompileError::InvalidMemoryAddress(RegionId(7))
            );
        }
    }
}

#[test]
fn lea_execution_allocates_nothing_after_setup() {
    let program = single(Operation::Lea {
        target: Register::Ax,
        address: MemoryAddress {
            base: Some(Register::Eax),
            index: Some(Register::Eax),
            scale: 8,
            ..absolute(u32::MAX)
        },
    });
    let mut runner = Runner::new(&program, CompileOptions::default());
    let initial = Frame {
        gpr: [0x9234_5678; 8],
        address: program.regions[0].entry,
        remaining_budget: 1,
        ..Frame::default()
    };
    runner.set(initial);
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    let allocation = allocation_counter::measure(|| {
        for _ in 0..1000 {
            runner.set(initial);
            assert_eq!(runner.call(), abi::EXIT_BUDGET);
        }
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(allocation.bytes_total, 0);
}
