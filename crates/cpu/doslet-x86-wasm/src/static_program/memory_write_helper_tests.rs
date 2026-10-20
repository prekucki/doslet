use super::*;

const VGA: u32 = 0xa0000;

fn helper_options() -> CompileOptions {
    CompileOptions {
        memory_write_helper: true,
        ..options(true)
    }
}

fn runner(program: &Program) -> Runner {
    Runner::with_aliases(program, helper_options(), &[])
}

#[test]
fn memory_write_helper_fast_ram_retains_direct_store_and_generation_updates() {
    for (width, value) in [
        (Width::Byte, 0xab),
        (Width::Word, 0xabcd),
        (Width::Dword, 0x1234_abcd),
    ] {
        let program = program(width, Source::Immediate(value), address(0xfff));
        let mut runner = runner(&program);
        runner.set(&program, initial(), &descriptor());
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        assert!(runner.store.data().helper_calls.is_empty());
        assert!(runner.store.data().callbacks.is_empty());
        assert_eq!(runner.generation(0), 12);
        assert_eq!(
            runner.generation(1),
            if width == Width::Byte { 12 } else { 13 }
        );
        let offset = RAM + (0xfff - RAM_BASE) as usize;
        assert_eq!(
            &runner.bytes()[offset..offset + width.bits() as usize / 8],
            &value.to_le_bytes()[..width.bits() as usize / 8]
        );
        assert_eq!(runner.effects().inline_reserved, 1);
        assert_eq!(runner.frame().charged_work, 1);
    }
}

#[test]
fn memory_write_helper_passes_width_aliases_live_ea_and_selected_write_segment() {
    for (register, value) in [
        (Register::Al, 0xdd),
        (Register::Ah, 0xcc),
        (Register::Ax, 0xccdd),
        (Register::Eax, 0xaabb_ccdd),
    ] {
        for address16 in [false, true] {
            let operand = MemoryAddress {
                base: Some(Register::Eax),
                index: Some(Register::Edx),
                scale: if address16 { 1 } else { 4 },
                displacement: 0x1234,
                address16,
                segment: Segment::Fs,
            };
            let program = program(register.width(), Source::Register(register), operand);
            let mut runner = runner(&program);
            let frame = initial();
            let ea = frame.gpr[0]
                .wrapping_add(frame.gpr[2].wrapping_mul(u32::from(operand.scale)))
                .wrapping_add(operand.displacement);
            let ea = if address16 { ea & 0xffff } else { ea };
            let mut descriptor = descriptor();
            descriptor.windows[1][Segment::Fs as usize].base = VGA.wrapping_sub(ea);
            // The unused read window deliberately refuses the operand.
            descriptor.windows[0][Segment::Fs as usize].valid = false;
            runner.set(&program, frame, &descriptor);
            assert_eq!(runner.call(), abi::EXIT_BUDGET);
            assert_eq!(
                runner.store.data().helper_calls,
                vec![HelperCall {
                    linear: VGA,
                    width: register.width().bits() / 8,
                    value,
                    frame,
                    effects: memory::Effects {
                        inline_reserved: 1,
                        inline_remaining: 99,
                        ..memory::Effects::default()
                    },
                }]
            );
            let result = runner.frame();
            assert_eq!(result.gpr, frame.gpr);
            assert_eq!(result.eflags, frame.eflags);
            assert_eq!(result.charged_work, 1);
            assert_eq!(result.remaining_budget, 0);
            assert_eq!(runner.store.data().helper_committed, 1);
            assert_eq!(
                &runner.bytes()
                    [HELPER_STORAGE..HELPER_STORAGE + register.width().bits() as usize / 8],
                &value.to_le_bytes()[..register.width().bits() as usize / 8]
            );
            assert!(runner.store.data().callbacks.is_empty());
        }
    }
}

#[test]
fn memory_write_helper_direct_proof_refusals_preserve_all_bytes_before_call() {
    let program = program(Width::Dword, Source::Immediate(0xabcd), address(0xfff));
    for case in 0..4 {
        let mut runner = runner(&program);
        runner.set(&program, initial(), &descriptor());
        match case {
            0 => runner.bytes()[CODE_PAGES + 1] = 1,
            1 => runner.set_generation(1, u64::MAX),
            2 => runner.word(memory::RAM_POINTER, u32::MAX),
            3 => runner.word(memory::RAM_EXCLUDED_START, 0xfff),
            _ => unreachable!(),
        }
        if case == 3 {
            runner.word(memory::RAM_EXCLUDED_LENGTH, 4);
        }
        let ram_before = runner.bytes()[RAM..RAM + RAM_LENGTH].to_vec();
        let generations_before =
            runner.bytes()[GENERATIONS..GENERATIONS + PAGE_COUNT as usize * 8].to_vec();
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
        assert_eq!(runner.store.data().helper_calls.len(), 1);
        assert_eq!(runner.store.data().helper_calls[0].linear, 0xfff);
        assert_eq!(runner.bytes()[RAM..RAM + RAM_LENGTH], ram_before);
        assert_eq!(
            runner.bytes()[GENERATIONS..GENERATIONS + PAGE_COUNT as usize * 8],
            generations_before
        );
        assert!(runner.store.data().callbacks.is_empty());
    }
}

fn with_prefix() -> Program {
    let mut program = program(Width::Dword, Source::Register(Register::Eax), address(VGA));
    program.regions[0].id = RegionId(1);
    program.regions[0].entry = entry(1);
    program.regions[0].terminator = Terminator::Fallthrough(RegionId(0));
    program.regions.push(Region {
        id: RegionId(0),
        entry: entry(0),
        operations: vec![
            Operation::Alu {
                kind: Alu::Add,
                target: Register::Eax,
                source: Source::Immediate(1),
            },
            Operation::Move {
                target: Register::Ecx,
                source: Source::Immediate(7),
            },
        ],
        terminator: Terminator::Fallthrough(RegionId(1)),
        accounting: Accounting::Counted,
    });
    program
}

#[test]
fn memory_write_helper_keeps_live_registers_and_flags_without_flushing_frame() {
    let mut program = with_prefix();
    program.regions[0].terminator = Terminator::Fallthrough(RegionId(2));
    program.regions.push(Region {
        id: RegionId(2),
        entry: entry(2),
        operations: vec![Operation::Alu {
            kind: Alu::Adc,
            target: Register::Edx,
            source: Source::Immediate(0),
        }],
        terminator: Terminator::Fallthrough(RegionId(0)),
        accounting: Accounting::Counted,
    });
    let mut frame = initial();
    frame.gpr[0] = u32::MAX;
    frame.gpr[2] = 10;
    frame.remaining_budget = 4;
    let mut runner = runner(&program);
    let mut descriptor = descriptor();
    descriptor.region_guards_length = 3;
    runner.set(&program, frame, &descriptor);
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    let calls = &runner.store.data().helper_calls;
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].frame, frame);
    assert_eq!(calls[0].value, 0);
    assert_eq!(calls[0].effects.inline_reserved, 3);
    assert_eq!(runner.store.data().helper_committed, 3);
    assert_eq!(runner.frame().gpr[0], 0);
    assert_eq!(runner.frame().gpr[1], 7);
    assert_eq!(runner.frame().gpr[2], 11);
    assert_eq!(runner.frame().eflags & 0x8d5, 0);
    assert_eq!(runner.frame().charged_work, 4);
    assert_eq!(runner.effects().inline_reserved, 1);
    assert!(runner.store.data().callbacks.is_empty());
}

#[test]
fn memory_write_helper_decline_rolls_back_only_current_reservation() {
    let program = with_prefix();
    let mut runner = runner(&program);
    let mut frame = initial();
    frame.remaining_budget = 10;
    runner.set(&program, frame, &descriptor());
    runner.store.data_mut().helper_status = Some(0);
    let before = runner.bytes()[HELPER_STORAGE..HELPER_STORAGE + 4].to_vec();
    assert_eq!(runner.call(), abi::EXIT_RUNTIME);
    assert_eq!(runner.store.data().helper_calls.len(), 1);
    assert_eq!(
        runner.store.data().helper_calls[0].effects.inline_reserved,
        3
    );
    assert_eq!(runner.store.data().helper_committed, 0);
    assert_eq!(runner.effects().inline_reserved, 2);
    assert_eq!(runner.effects().inline_remaining, 98);
    let result = runner.frame();
    assert_eq!(result.address, entry(1));
    assert_eq!(result.gpr[0], frame.gpr[0].wrapping_add(1));
    assert_eq!(result.gpr[1], 7);
    assert_eq!(result.charged_work, 2);
    assert_eq!(result.remaining_budget, 8);
    assert_eq!(
        runner.store.data().callbacks,
        vec![(abi::REASON_FALLBACK, result)]
    );
    assert_eq!(runner.bytes()[HELPER_STORAGE..HELPER_STORAGE + 4], before);
}

#[test]
fn memory_write_helper_repeated_acceptance_refreshes_descriptor_without_frame_reentry() {
    let program = program(Width::Word, Source::Immediate(0x1234), address(VGA));
    let mut runner = runner(&program);
    let mut frame = initial();
    frame.remaining_budget = 7;
    let mut descriptor = descriptor();
    descriptor.inline_grant = 1;
    runner.set(&program, frame, &descriptor);
    runner.store.data_mut().helper_grant = Some(1);
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    assert_eq!(runner.store.data().helper_calls.len(), 7);
    for call in &runner.store.data().helper_calls {
        assert_eq!(call.frame, frame);
        assert_eq!(call.effects.inline_reserved, 1);
        assert_eq!(call.effects.inline_remaining, 0);
    }
    assert_eq!(runner.store.data().helper_committed, 7);
    assert_eq!(runner.frame().charged_work, 7);
    assert_eq!(runner.effects().inline_reserved, 0);
    assert_eq!(runner.effects().inline_remaining, 1);
    assert!(runner.store.data().callbacks.is_empty());
}

#[test]
fn memory_write_helper_next_region_rechecks_refreshed_source_and_grant() {
    for invalidate in [false, true] {
        let program = with_prefix();
        let mut runner = runner(&program);
        let mut frame = initial();
        frame.remaining_budget = 10;
        runner.set(&program, frame, &descriptor());
        if invalidate {
            runner.store.data_mut().helper_invalidate_guard = Some(0);
        } else {
            runner.store.data_mut().helper_grant = Some(0);
        }
        assert_eq!(runner.call(), abi::EXIT_RUNTIME);
        assert_eq!(runner.store.data().helper_calls.len(), 1);
        assert_eq!(runner.store.data().helper_committed, 3);
        assert_eq!(runner.frame().address, entry(0));
        assert_eq!(runner.frame().charged_work, 3);
        assert_eq!(runner.effects().inline_reserved, 0);
        assert_eq!(runner.store.data().callbacks.len(), 1);
    }
}

#[test]
fn memory_write_helper_invalid_common_spans_source_or_grant_refuse_before_helper() {
    for case in 0..10 {
        let mut operand = address(VGA);
        let mut descriptor = descriptor();
        match case {
            0 => descriptor.windows[1][Segment::Ds as usize].valid = false,
            1 => descriptor.windows[1][Segment::Ds as usize].first = VGA + 1,
            2 => descriptor.windows[1][Segment::Ds as usize].last = VGA + 2,
            3 => operand.displacement = u32::MAX - 2,
            4 => descriptor.inline_grant = 0,
            5 => descriptor.region_guards_length = 0,
            9 => {
                descriptor.windows[1][Segment::Ds as usize].base = (u32::MAX - 1).wrapping_sub(VGA)
            }
            _ => {}
        }
        let program = program(Width::Dword, Source::Immediate(1), operand);
        let mut runner = runner(&program);
        runner.set(&program, initial(), &descriptor);
        match case {
            6 => runner.word(memory::VERSION_OFFSET, memory::VERSION + 1),
            7 => runner.guard(0, RegionGuard::default()),
            8 => runner.word(memory::INLINE_RESERVED, u32::MAX),
            _ => {}
        }
        runner.assert_refused(initial());
        assert!(runner.store.data().helper_calls.is_empty());
    }
}

#[test]
fn memory_write_helper_truncated_descriptor_falls_back_before_import() {
    let program = program(Width::Byte, Source::Immediate(7), address(VGA));
    let mut runner = runner(&program);
    let pointer = runner.bytes().len() - abi::FRAME_BYTES;
    initial().write(&mut runner.bytes()[pointer..]).unwrap();
    assert_eq!(
        runner.run.call(&mut runner.store, pointer as u32).unwrap(),
        abi::EXIT_RUNTIME
    );
    assert!(runner.store.data().helper_calls.is_empty());
    assert_eq!(
        runner.store.data().callbacks,
        vec![(abi::REASON_FALLBACK, initial())]
    );
}

#[test]
fn memory_write_helper_unexpected_status_traps_without_interpreter_replay() {
    let program = program(Width::Byte, Source::Immediate(7), address(VGA));
    let mut runner = runner(&program);
    runner.set(&program, initial(), &descriptor());
    runner.store.data_mut().helper_status = Some(2);
    assert!(runner.run.call(&mut runner.store, FRAME as u32).is_err());
    assert_eq!(runner.frame(), initial());
    assert_eq!(runner.store.data().helper_calls.len(), 1);
    assert!(runner.store.data().callbacks.is_empty());
}

#[test]
fn memory_write_helper_requires_inline_admission_and_excludes_hot_regions() {
    assert!(!CompileOptions::default().memory_write_helper);
    let mut program = program(Width::Byte, Source::Immediate(1), address(VGA));
    for options in [
        CompileOptions {
            memory_write_helper: true,
            ..CompileOptions::default()
        },
        CompileOptions {
            inline_admission: false,
            ..helper_options()
        },
        CompileOptions {
            runtime_admission: true,
            ..helper_options()
        },
        CompileOptions {
            direct_memory: false,
            ..helper_options()
        },
    ] {
        assert_eq!(
            program.compile(&options).unwrap_err(),
            CompileError::InvalidOptions
        );
    }
    program.regions[0].accounting = Accounting::UncountedHot;
    assert_eq!(
        program.compile(&helper_options()).unwrap_err(),
        CompileError::AdmissionUncountedHot(RegionId(0))
    );
}

#[test]
fn memory_write_helper_chunked_ret_dispatch_and_aliases_keep_function_indices() {
    let mut program = program(Width::Dword, Source::Immediate(0x1234), address(VGA));
    program.regions[0].id = RegionId(1);
    program.regions[0].entry = entry(1);
    program.regions[0].terminator = Terminator::Fallthrough(RegionId(1));
    program.regions.insert(
        0,
        Region {
            id: RegionId(0),
            entry: entry(0),
            operations: vec![],
            terminator: Terminator::Return {
                width: Width::Dword,
                pop_bytes: 0,
                target_space: 7,
            },
            accounting: Accounting::Counted,
        },
    );
    program.regions.extend((2..8193).map(|id| Region {
        id: RegionId(id),
        entry: entry(id),
        operations: vec![],
        terminator: Terminator::Fallthrough(RegionId(id)),
        accounting: Accounting::Counted,
    }));
    let alias = EntryAlias {
        address: Address {
            space: 7,
            offset: 0x2468,
        },
        target: RegionId(1),
    };
    // Keep the sparse alias address distinct from the original entry grid.
    let mut runner = Runner::with_aliases(&program, helper_options(), &[alias]);
    let mut frame = initial();
    frame.remaining_budget = 2;
    let mut descriptor = descriptor();
    descriptor.stack_address = memory::StackAddress::Bits32;
    let used = Program {
        regions: program.regions[..2].to_vec(),
    };
    runner.set(&used, frame, &descriptor);
    runner.bytes()[RAM..RAM + 4].copy_from_slice(&alias.address.offset.to_le_bytes());
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    assert_eq!(runner.frame().gpr[4], frame.gpr[4] + 4);
    assert_eq!(runner.frame().charged_work, 2);
    assert_eq!(runner.store.data().helper_calls.len(), 1);
    assert_eq!(runner.store.data().helper_committed, 2);
    assert!(runner.store.data().callbacks.is_empty());
}

#[test]
fn memory_write_helper_warmed_acceptance_allocates_nothing() {
    let program = program(Width::Dword, Source::Immediate(7), address(VGA));
    let mut runner = runner(&program);
    runner.set(&program, initial(), &descriptor());
    assert_eq!(runner.call(), abi::EXIT_BUDGET);
    runner.store.data_mut().helper_calls.clear();
    runner.set(&program, initial(), &descriptor());
    let allocation = allocation_counter::measure(|| {
        assert_eq!(runner.call(), abi::EXIT_BUDGET);
    });
    assert_eq!(allocation.count_total, 0);
    assert_eq!(allocation.bytes_total, 0);
}
