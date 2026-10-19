use super::*;
use doslet_x86_wasm::static_program::memory::{
    Descriptor, Effects, RamDescriptor, Segment, SegmentBounds, StackAddress,
};

#[derive(Default)]
struct Calls {
    callbacks: Vec<(u32, Frame)>,
    admissions: Vec<(u32, u32)>,
    refuse: bool,
}

struct CallResult {
    frame: Frame,
    calls: Calls,
    ram: [u8; 16],
    generation: u64,
    effects: Effects,
}

fn descriptor(mode: StackAddress) -> Descriptor {
    Descriptor {
        stack_address: mode,
        ram: RamDescriptor {
            pointer: 4096,
            linear_base: 0x500,
            length: 16,
            generations_pointer: 6000,
            generations_length: 1,
            code_pages_pointer: 7000,
            code_pages_length: 1,
            ..RamDescriptor::default()
        },
        windows: [[SegmentBounds {
            valid: true,
            base: 0,
            first: 0,
            last: u32::MAX,
        }; 6]; 2],
        ..Descriptor::default()
    }
}

fn initial(entry: Address, mode: StackAddress, budget: u64) -> Frame {
    Frame {
        gpr: [
            0x1234_5678,
            0,
            0,
            0,
            if mode == StackAddress::Bits16 {
                0xabcd_0508
            } else {
                0x508
            },
            0,
            0,
            0,
        ],
        eflags: 0xad7,
        address: entry,
        remaining_budget: budget,
        ..Frame::default()
    }
}

fn run_call(
    prepared: &PreparedProgram,
    frame: Frame,
    descriptor: Descriptor,
    refuse: bool,
) -> CallResult {
    let compiled = prepared
        .program()
        .compile(&CompileOptions {
            direct_memory: true,
            runtime_admission: true,
            ..CompileOptions::default()
        })
        .unwrap();
    let engine = Engine::default();
    let module = Module::new(&engine, compiled.wasm()).unwrap();
    // CALL uses the existing stack operation and ordinary admission import.
    assert_eq!(module.imports().count(), 3);
    let mut store = Store::new(
        &engine,
        Calls {
            refuse,
            ..Calls::default()
        },
    );
    let memory = Memory::new(&mut store, MemoryType::new(1, Some(1))).unwrap();
    let mut linker = Linker::new(&engine);
    linker
        .define(abi::IMPORT_MODULE, abi::MEMORY_IMPORT, memory)
        .unwrap();
    linker
        .func_wrap(
            abi::IMPORT_MODULE,
            abi::RUNTIME_IMPORT,
            |mut caller: Caller<'_, Calls>, pointer: u32, reason: u32| -> u32 {
                let memory = caller.get_export("memory").unwrap().into_memory().unwrap();
                let frame = Frame::read(&memory.data(&caller)[pointer as usize..]).unwrap();
                caller.data_mut().callbacks.push((reason, frame));
                abi::EXIT
            },
        )
        .unwrap();
    linker
        .func_wrap(
            abi::IMPORT_MODULE,
            abi::ADMIT_IMPORT,
            |mut caller: Caller<'_, Calls>, _: u32, region: u32, work: u32| -> u32 {
                caller.data_mut().admissions.push((region, work));
                u32::from(!caller.data().refuse)
            },
        )
        .unwrap();
    let instance = linker.instantiate_and_start(&mut store, &module).unwrap();
    frame.write(memory.data_mut(&mut store)).unwrap();
    descriptor
        .write(&mut memory.data_mut(&mut store)[72..])
        .unwrap();
    memory.data_mut(&mut store)[4096..4112].fill(0xa5);
    instance
        .get_typed_func::<u32, u32>(&store, abi::RUN_EXPORT)
        .unwrap()
        .call(&mut store, 0)
        .unwrap();
    let bytes = memory.data(&store);
    CallResult {
        frame: Frame::read(bytes).unwrap(),
        ram: bytes[4096..4112].try_into().unwrap(),
        generation: u64::from_le_bytes(bytes[6000..6008].try_into().unwrap()),
        effects: Effects::read(&bytes[72..]).unwrap(),
        calls: store.into_data(),
    }
}

fn prepare<'a>(graph: &StaticCfg<'a>, binding: &DosFlatBindingPlan<'a>) -> PreparedProgram {
    PreparedProgram::from_bound_cfg(
        graph,
        binding,
        LoweringLimits {
            direct_memory: true,
            ..LoweringLimits::default()
        },
    )
    .unwrap()
}

fn region(prepared: &PreparedProgram, entry: Address) -> &Region {
    prepared
        .program()
        .regions
        .iter()
        .find(|region| region.entry == entry)
        .unwrap()
}

fn assert_push(result: &CallResult, original: Frame, return_eip: u32) {
    let mut expected = [0xa5; 16];
    expected[4..8].copy_from_slice(&return_eip.to_le_bytes());
    assert_eq!(result.ram, expected);
    assert_eq!(result.frame.gpr[4], original.gpr[4] - 4);
    assert_eq!(result.generation, 1);
    assert_eq!(result.effects.ram_writes, 1);
    assert_eq!(result.effects.device_reads, 0);
}

#[test]
fn bound_near_call_is_one_isolated_instruction_with_linear_return_eip() {
    for prefix in [false, true] {
        let mut code = vec![0x43]; // INC EBX must not share CALL's preflight.
        if prefix {
            code.push(0x67); // Address size does not select SS.B or push width.
        }
        code.extend_from_slice(&[0xe8, 2, 0, 0, 0]);
        let continuation = code.len() as u32;
        let callee = continuation + 2;
        code.extend_from_slice(&[0x42, 0xf4, 0xb8, 0x21, 0x43, 0x65, 0x87, 0xc3]);
        let image = image(&code, DefaultBitness::Bits32);
        let binding = binding(&image, 0x1000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let prepared = prepare(&graph, &binding);
        assert_eq!(prepared.report().direct_calls, 1);
        assert_eq!(prepared.report().stack_operations, 0);
        assert_eq!(prepared.report().direct_transfers, 0);
        let call = region(&prepared, address(at(1)));
        assert_eq!(
            call.operations,
            [Operation::Push {
                width: Width::Dword,
                source: Source::Immediate(0x1000 + continuation),
            }]
        );
        assert_eq!(
            call.terminator,
            Terminator::Fallthrough(region(&prepared, address(at(callee))).id)
        );
        let call_sources = prepared.sources().iter().find(|s| s.id == call.id).unwrap();
        assert_eq!(call_sources.instructions(), [at(1)]);
        assert_eq!(region(&prepared, address(at(0))).operations.len(), 1);
        assert_eq!(
            region(&prepared, address(at(continuation))).entry,
            address(at(continuation))
        );
        for mode in [StackAddress::Bits16, StackAddress::Bits32] {
            let start = initial(address(at(1)), mode, 1);
            let one = run_call(&prepared, start, descriptor(mode), false);
            assert_push(&one, start, 0x1000 + continuation);
            assert_eq!(one.frame.address, address(at(callee)));
            assert_eq!(one.frame.charged_work, 1);
            assert_eq!(one.frame.remaining_budget, 0);
            assert_eq!(one.frame.eflags, start.eflags);
            let mut expected = start.gpr;
            expected[4] -= 4;
            assert_eq!(one.frame.gpr, expected);
            assert_eq!(one.calls.admissions, [(call.id.0, 1)]);
            assert!(one.calls.callbacks.is_empty());

            let start = initial(address(at(0)), mode, 100);
            let completed = run_call(&prepared, start, descriptor(mode), false);
            assert_eq!(completed.ram, one.ram);
            assert_eq!(completed.generation, 1);
            assert_eq!(completed.effects.ram_writes, 1);
            assert_eq!(completed.frame.gpr[4], start.gpr[4]);
            assert_eq!(completed.frame.gpr[0], 0x8765_4321);
            assert_eq!(completed.frame.gpr[3], 1);
            assert_eq!(completed.frame.gpr[2], 0);
            assert_eq!(completed.frame.charged_work, 4);
            assert_eq!(
                completed.frame.address,
                Address {
                    space: 0,
                    offset: 0x1000 + continuation
                }
            );
            assert_eq!(
                completed.calls.callbacks,
                [(abi::REASON_FALLBACK, completed.frame)]
            );
            // This fixture supplies no aliases: the translated RET completes
            // before delegating its linear destination. The continuation is
            // still a separate canonical entry that can resume directly.
            let (resumed, calls) = execute_setup(
                &prepared,
                Frame {
                    address: address(at(continuation)),
                    remaining_budget: 1,
                    ..start
                },
                CompileOptions {
                    direct_memory: true,
                    ..CompileOptions::default()
                },
                |_| {},
                |_| {},
            );
            assert_eq!(resumed.gpr[2], 1);
            assert_eq!(resumed.address, address(at(continuation + 1)));
            assert!(calls.is_empty());
        }
    }
}

#[test]
fn relocated_cross_object_call_retains_source_cs_and_pushes_caller_eip() {
    let mut image = image(&[0xe8, 0, 0, 0, 0, 0x43, 0xf4], DefaultBitness::Bits32);
    append_data_code(&mut image, &[0xb8, 0x21, 0x43, 0x65, 0x87, 0xc3]);
    image.sites.push(Site {
        source: at(1),
        kind: LeFixupSourceType::SelfRelative32,
        target: RelocationTarget {
            object_number: 2,
            offset: Some(0),
        },
        is_alias: false,
        provenance: 0..0,
    });
    let first = two_object_binding(&image, 0x3000);
    let mut supplied = [
        first.object_binding(1).unwrap(),
        first.object_binding(2).unwrap(),
    ];
    supplied[1].normal.as_mut().unwrap().limit = 0x3005; // This data selector is not the active CS.
    let binding = DosFlatBindingPlan::new(&image, &supplied, BindingLimits::default()).unwrap();
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared = prepare(&graph, &binding);
    assert_eq!(prepared.report().direct_calls, 1);
    assert_eq!(prepared.report().external_stubs, 0);
    let target = Address {
        space: 2,
        offset: 0,
    };
    assert_eq!(
        region(&prepared, address(at(0))).terminator,
        Terminator::Fallthrough(region(&prepared, target).id)
    );
    assert_eq!(image.object(1).unwrap().bytes()[1..5], [0, 0, 0, 0]);
    for mode in [StackAddress::Bits16, StackAddress::Bits32] {
        let start = initial(address(at(0)), mode, 100);
        let result = run_call(&prepared, start, descriptor(mode), false);
        assert_eq!(&result.ram[4..8], &0x1005u32.to_le_bytes());
        assert_eq!(result.generation, 1);
        assert_eq!(result.effects.ram_writes, 1);
        assert_eq!(result.frame.gpr[4], start.gpr[4]);
        assert_eq!(result.frame.gpr[0], 0x8765_4321);
        assert_eq!(result.frame.gpr[3], 0);
        assert_eq!(result.frame.eflags, start.eflags);
        assert_eq!(result.frame.charged_work, 3);
        assert_eq!(
            result.frame.address,
            Address {
                space: 0,
                offset: 0x1005
            }
        );
        assert_eq!(
            result.calls.callbacks,
            [(abi::REASON_FALLBACK, result.frame)]
        );
        assert_eq!(result.calls.admissions.len(), 3);
        assert!(result.calls.admissions.iter().all(|&(_, work)| work == 1));
    }
}

#[test]
fn direct_call_stack_preflight_and_admission_refuse_before_all_effects() {
    let image = image(
        &[0xe8, 2, 0, 0, 0, 0x43, 0xf4, 0x40, 0xc3],
        DefaultBitness::Bits32,
    );
    let binding = binding(&image, 0x1000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared = prepare(&graph, &binding);
    for mode in [StackAddress::Bits16, StackAddress::Bits32] {
        for failure in 0..7 {
            let mut descriptor = descriptor(mode);
            match failure {
                0 => descriptor.stack_address = StackAddress::Disabled,
                1 => descriptor.windows[1][Segment::Ss as usize].valid = false,
                2 => descriptor.windows[1][Segment::Ss as usize].last = 0x506,
                3 => descriptor.ram.length = 7,
                4 => descriptor.ram.generations_length = 0,
                5 => descriptor.ram.code_pages_pointer = 4096, // Protected page.
                6 => (), // Valid preflight, refused admission.
                _ => unreachable!(),
            }
            let start = initial(address(at(0)), mode, 100);
            let result = run_call(&prepared, start, descriptor, failure == 6);
            assert_eq!(result.frame, start, "{mode:?} failure {failure}");
            assert_eq!(result.ram, [0xa5; 16]);
            assert_eq!(result.generation, 0);
            assert_eq!(result.effects, Effects::default());
            assert_eq!(result.calls.callbacks, [(abi::REASON_FALLBACK, start)]);
            if failure == 6 {
                assert_eq!(
                    result.calls.admissions,
                    [(region(&prepared, start.address).id.0, 1)]
                );
            } else {
                assert!(result.calls.admissions.is_empty());
            }
        }
    }
}

#[test]
fn direct_call_to_proven_stopped_target_pushes_then_enters_explicit_stub() {
    let image = image(
        &[0xe8, 2, 0, 0, 0, 0x43, 0xf4, 0x0f],
        DefaultBitness::Bits32,
    );
    let binding = binding(&image, 0x1000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let prepared = prepare(&graph, &binding);
    assert_eq!(prepared.report().direct_calls, 1);
    assert_eq!(prepared.report().external_stubs, 1);
    let start = initial(address(at(0)), StackAddress::Bits32, 100);
    let result = run_call(&prepared, start, descriptor(StackAddress::Bits32), false);
    assert_push(&result, start, 0x1005);
    assert_eq!(result.frame.charged_work, 1);
    assert_eq!(result.frame.address, address(at(7)));
    assert_eq!(
        result.calls.callbacks,
        [(abi::REASON_FALLBACK, result.frame)]
    );
    assert_eq!(result.calls.admissions.len(), 1);
}

#[test]
fn unresolved_indirect_far_and_near16_calls_remain_before_effects_fallbacks() {
    for (bitness, bytes) in [
        (
            DefaultBitness::Bits32,
            vec![0x66, 0xe8, 2, 0, 0x43, 0xf4, 0x40, 0xc3],
        ),
        (DefaultBitness::Bits32, vec![0xe8, 0, 0, 1, 0, 0x43, 0xf4]),
        (DefaultBitness::Bits32, vec![0xff, 0xd0, 0x43, 0xf4]),
        (DefaultBitness::Bits32, vec![0xff, 0x10, 0x43, 0xf4]),
        (DefaultBitness::Bits32, vec![0xff, 0x18, 0x43, 0xf4]),
        (
            DefaultBitness::Bits32,
            vec![0x9a, 0, 0, 0, 0, 8, 0, 0x43, 0xf4],
        ),
        (
            DefaultBitness::Bits16,
            vec![0xe8, 2, 0, 0x43, 0xf4, 0x40, 0xc3],
        ),
    ] {
        let image = image(&bytes, bitness);
        let binding = binding(&image, 0x1000);
        // Code16 remains the original unbound path; bound traversal refuses
        // that CS context without constructing speculative instruction nodes.
        let graph = if bitness == DefaultBitness::Bits16 {
            StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap()
        } else {
            StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap()
        };
        let prepared = prepare(&graph, &binding);
        assert_eq!(prepared.report().direct_calls, 0, "{bytes:02x?}");
        let start = initial(address(at(0)), StackAddress::Bits32, 100);
        let (frame, calls) = execute_setup(
            &prepared,
            start,
            CompileOptions {
                direct_memory: true,
                ..CompileOptions::default()
            },
            |_| {},
            |_| {},
        );
        assert_eq!(frame, start, "{bytes:02x?}");
        assert_eq!(calls, [(abi::REASON_FALLBACK, start.address)]);
    }
}

#[test]
fn resolved_calls_keep_default_and_unbound_lowering_unchanged() {
    let image = image(
        &[0xe8, 2, 0, 0, 0, 0x43, 0xf4, 0x40, 0xc3],
        DefaultBitness::Bits32,
    );
    let binding = binding(&image, 0x1000);
    for bound in [false, true] {
        let graph = if bound {
            StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap()
        } else {
            StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap()
        };
        let prepared = PreparedProgram::from_bound_cfg(
            &graph,
            &binding,
            LoweringLimits {
                direct_memory: !bound,
                ..LoweringLimits::default()
            },
        )
        .unwrap();
        assert_eq!(prepared.report().direct_calls, 0);
        let start = initial(address(at(0)), StackAddress::Bits32, 100);
        let (frame, calls) = execute(&prepared, start);
        assert_eq!(frame, start);
        assert_eq!(calls, [(abi::REASON_FALLBACK, start.address)]);
    }
}

#[test]
fn consecutive_calls_with_guidance_transitions_stay_inside_reserved_boundaries() {
    use crate::profile::Tier;
    let mut code = [0xe8, 0, 0, 0, 0].repeat(4);
    code.push(0xf4);
    let image = image(&code, DefaultBitness::Bits32);
    let binding = binding(&image, 0x1000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    assert_eq!(graph.nodes().len(), 5);
    for isolate in [false, true] {
        let guidance: Vec<_> = graph
            .nodes()
            .iter()
            .enumerate()
            .map(|(index, node)| InstructionGuidance {
                tier: if index % 2 == 0 {
                    Tier::Hot
                } else {
                    Tier::Cold
                },
                status: if isolate {
                    GuidanceStatus::ByteMismatch
                } else {
                    GuidanceStatus::Verified
                },
                ..InstructionGuidance::unknown(node.address)
            })
            .collect();
        // CALL contributes its own boundary, continuation and target, plus a
        // tier transition. Source conflicts add two further boundary requests.
        // The lowerer's capacity assertion catches any unbudgeted Vec growth.
        let prepared = PreparedProgram::prepare(
            &graph,
            &binding,
            Some(&guidance),
            false,
            &[],
            LoweringLimits {
                direct_memory: true,
                isolate_mutable_sources: isolate,
                ..LoweringLimits::default()
            },
        )
        .unwrap();
        assert_eq!(prepared.report().direct_calls, 4);
        assert_eq!(prepared.report().generated_regions, 5);
    }
}
