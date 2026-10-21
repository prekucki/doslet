//! Procedures of a program whose selectors share a nonzero base, as a DJGPP
//! program's do: EIPs are offsets in a segment at `BASE`, its code segment
//! has a limit below 4 GiB, and the block can move.
use super::*;

const BASE: u32 = 0x10_0000;
const MOVED: u32 = 0x20_0000;
/// Page-granular code limit: 64 KiB.
const CODE_LIMIT: u32 = 0xffff;
const CODE_SELECTOR: u16 = 24;
const DATA_SELECTOR: u16 = 32;

/// A G=1 descriptor with an inclusive byte limit of `limit` (low 12 bits set).
fn paged_descriptor(bus: &mut PcBus, selector: u16, base: u32, limit: u32, code: bool) {
    let pages = limit >> 12;
    let bytes = [
        pages as u8,
        (pages >> 8) as u8,
        base as u8,
        (base >> 8) as u8,
        (base >> 16) as u8,
        if code { 0x9b } else { 0x93 },
        0xc0 | ((pages >> 16) as u8 & 0xf),
        (base >> 24) as u8,
    ];
    for (offset, byte) in bytes.into_iter().enumerate() {
        bus.write_u8(0x800 + u32::from(selector) + offset as u32, byte)
            .unwrap();
    }
}

/// Load the program's selectors at `base` and place its code, data and stack
/// at the same offsets as the flat fixture.
fn enter_block(machine: &mut PcMachine, base: u32) {
    paged_descriptor(&mut machine.bus, CODE_SELECTOR, base, CODE_LIMIT, true);
    paged_descriptor(&mut machine.bus, DATA_SELECTOR, base, u32::MAX, false);
    machine
        .bus
        .debug_ram_mut(base + ENTRY, CODE.len() as u32)
        .unwrap()
        .copy_from_slice(CODE);
    machine.bus.write_u8(base, 0x90).unwrap();
    machine.bus.write_u32(base + DATA, 35).unwrap();
    machine.bus.write_u32(base + STACK, RETURN).unwrap();
    machine.bus.write_u8(base + RETURN, 0x90).unwrap();
    for segment in SegmentRegister::ALL {
        let selector = if segment == SegmentRegister::Cs {
            CODE_SELECTOR
        } else {
            DATA_SELECTOR
        };
        machine
            .cpu
            .load_segment(&mut machine.bus, segment, selector)
            .unwrap();
    }
    machine.cpu.registers.ip = 0;
    machine.cpu.step(&mut machine.bus).unwrap(); // Complete the SS interrupt shadow.
    machine.cpu.registers.ip = ENTRY;
    machine.cpu.registers.set32(Gpr32::Esp, STACK);
    machine.cpu.registers.set32(Gpr32::Ebx, DATA);
    machine.cpu.flags = 2;
}

fn segmented() -> PcMachine {
    let mut machine = machine();
    enter_block(&mut machine, BASE);
    machine
}

fn segment_binding(code_segment: CodeSegment) -> Binding {
    let mut binding = binding();
    binding.code_segment = code_segment;
    binding.code[0].linear = code_segment.base + ENTRY;
    binding
}

const DJGPP: CodeSegment = CodeSegment {
    base: BASE,
    limit: CODE_LIMIT,
};

#[test]
fn procedures_run_in_a_code_segment_with_a_base_and_limit() {
    let mut fast = segmented();
    let mut reference = segmented();
    fast.install_wasm_procedures(&module(), vec![segment_binding(DJGPP)])
        .unwrap();
    assert_eq!(fast.wasm_procedure_code_segment(), Some(DJGPP));
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 4);
    reference_steps(&mut reference, 4);
    let stats = fast.wasm_procedure_stats().unwrap();
    assert_eq!((stats.calls, stats.translated_instructions), (1, 4));
    same_state(&mut fast, &mut reference);
    // Data and stack went through the shared selector base.
    assert_eq!(fast.bus.read_u32(BASE + DATA).unwrap(), 42);
    assert_eq!(fast.cpu.registers.ip, RETURN);
    assert_eq!(fast.cpu.registers.get32(Gpr32::Esp), STACK + 4);
}

#[test]
fn another_base_or_a_shorter_limit_declines_to_the_interpreter() {
    // Flat procedures never run in a based segment, nor those of another base.
    let mut flat = segmented();
    flat.install_wasm_procedures(&module(), vec![binding()])
        .unwrap();
    let mut wrong_base = segmented();
    wrong_base
        .install_wasm_procedures(
            &module(),
            vec![segment_binding(CodeSegment {
                base: MOVED,
                limit: CODE_LIMIT,
            })],
        )
        .unwrap();
    let mut longer = segmented();
    longer
        .install_wasm_procedures(
            &module(),
            vec![segment_binding(CodeSegment {
                base: BASE,
                limit: CODE_LIMIT + 0x1000,
            })],
        )
        .unwrap();
    for fast in [&mut flat, &mut wrong_base, &mut longer] {
        let mut reference = segmented();
        let (_, count) = fast.step_with_idle_budget(32).unwrap();
        reference_steps(&mut reference, count);
        assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 0);
        same_state(fast, &mut reference);
    }
}

#[test]
fn sources_and_entries_must_lie_inside_their_code_segment() {
    let mut machine = segmented();
    let mut outside = segment_binding(DJGPP);
    outside.entry_ip = CODE_LIMIT + 1;
    for (binding, error) in [
        (outside, "WASM procedure entry outside its code segment"),
        (
            segment_binding(CodeSegment {
                base: BASE,
                limit: ENTRY + 4,
            }),
            "WASM procedure source outside its code segment",
        ),
    ] {
        assert_eq!(
            machine
                .install_wasm_procedures(&module(), vec![binding])
                .unwrap_err(),
            error
        );
    }
    let mut mixed = segment_binding(DJGPP);
    mixed.function_id = 10;
    mixed.entry_ip = ENTRY + 5;
    mixed.code_segment.limit = CODE_LIMIT - 1;
    assert_eq!(
        machine
            .install_wasm_procedures(&module(), vec![segment_binding(DJGPP), mixed])
            .unwrap_err(),
        "WASM procedures must share one code segment"
    );
}

struct NoCache;
impl CompiledCache for NoCache {
    fn load(&mut self, _: &[u8], _: u64) -> Option<Vec<u8>> {
        None
    }
    fn store(&mut self, _: &[u8], _: u64, _: &[u8]) {}
}

#[test]
fn replaced_procedures_keep_ram_vram_and_restart_statistics() {
    let mut fast = segmented();
    let mut reference = segmented();
    for machine in [&mut fast, &mut reference] {
        machine.bus.set_video_mode_13h();
        machine.bus.write_u8(0xa0010, 0x5c).unwrap();
    }
    fast.install_wasm_procedures(&module(), vec![segment_binding(DJGPP)])
        .unwrap();
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 4);
    reference_steps(&mut reference, 4);
    let mut invalid = segment_binding(DJGPP);
    invalid.entry_ip = CODE_LIMIT + 1;
    assert!(
        fast.replace_wasm_procedures_cached(&module(), vec![invalid], &mut NoCache)
            .is_err()
    );
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 1);
    fast.replace_wasm_procedures_cached(&module(), vec![segment_binding(DJGPP)], &mut NoCache)
        .unwrap();
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 0);
    same_state(&mut fast, &mut reference);
    for machine in [&mut fast, &mut reference] {
        assert_eq!(machine.bus.read_u8(0xa0010).unwrap(), 0x5c);
        machine.cpu.registers.ip = ENTRY;
        machine.cpu.registers.set32(Gpr32::Esp, STACK);
    }
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 4);
    reference_steps(&mut reference, 4);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 1);
    same_state(&mut fast, &mut reference);
    // Installing without replacing still refuses a second plan.
    assert_eq!(
        fast.install_wasm_procedures(&module(), vec![segment_binding(DJGPP)])
            .unwrap_err(),
        "WASM procedures already installed"
    );
}

#[test]
fn uninstalled_procedures_return_memory_to_its_ordinary_owners() {
    let mut fast = segmented();
    let mut reference = segmented();
    for machine in [&mut fast, &mut reference] {
        machine.bus.set_video_mode_13h();
        machine.bus.write_u8(0xa0021, 0x6e).unwrap();
    }
    assert_eq!(
        fast.uninstall_wasm_procedures(),
        Err("WASM procedures are not installed")
    );
    fast.install_wasm_procedures(&module(), vec![segment_binding(DJGPP)])
        .unwrap();
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 4);
    reference_steps(&mut reference, 4);
    fast.uninstall_wasm_procedures().unwrap();
    assert!(matches!(fast.bus.ram.storage, Storage::Owned { .. }));
    assert!(fast.wasm_procedure_stats().is_none());
    assert!(!fast.cpu.has_prepared_entries());
    same_state(&mut fast, &mut reference);
    assert_eq!(fast.bus.vga.vram_view(), reference.bus.vga.vram_view());
    // The interpreter runs the entry; a module can be installed again.
    for machine in [&mut fast, &mut reference] {
        machine.cpu.registers.ip = ENTRY;
        machine.cpu.registers.set32(Gpr32::Esp, STACK);
    }
    let (_, count) = fast.step_with_idle_budget(32).unwrap();
    reference_steps(&mut reference, count);
    same_state(&mut fast, &mut reference);
    fast.install_wasm_procedures(&module(), vec![segment_binding(DJGPP)])
        .unwrap();
}

#[test]
fn rebased_procedures_follow_a_moved_block() {
    let mut fast = segmented();
    let mut reference = segmented();
    let mut binding = segment_binding(DJGPP);
    binding.execution_sites = Some(
        [(0, 5), (5, 3), (8, 2), (10, 1)]
            .into_iter()
            .map(|(offset, length)| ExecutionSite {
                ip: ENTRY + offset,
                length,
            })
            .collect(),
    );
    fast.install_wasm_procedures(&module(), vec![binding])
        .unwrap();
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 4);
    reference_steps(&mut reference, 4);
    // The block moves; until the plan follows, calls decline.
    enter_block(&mut fast, MOVED);
    enter_block(&mut reference, MOVED);
    let (_, count) = fast.step_with_idle_budget(32).unwrap();
    reference_steps(&mut reference, count);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 1);
    // A move outside RAM is rejected and changes nothing.
    assert!(fast.rebase_wasm_procedures(u32::MAX - 0x100).is_err());
    assert_eq!(fast.wasm_procedure_code_segment(), Some(DJGPP));
    for machine in [&mut fast, &mut reference] {
        machine.cpu.registers.ip = ENTRY;
        machine.cpu.registers.set32(Gpr32::Esp, STACK);
        machine.bus.write_u32(MOVED + STACK, RETURN).unwrap();
    }
    fast.rebase_wasm_procedures(MOVED).unwrap();
    assert_eq!(
        fast.wasm_procedure_code_segment(),
        Some(CodeSegment {
            base: MOVED,
            limit: CODE_LIMIT
        })
    );
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 4);
    reference_steps(&mut reference, 4);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 2);
    same_state(&mut fast, &mut reference);
    // Old code bytes no longer prove anything; the moved copy does.
    fast.bus.write_u8(BASE + ENTRY, 0xcc).unwrap();
    reference.bus.write_u8(BASE + ENTRY, 0xcc).unwrap();
    for machine in [&mut fast, &mut reference] {
        machine.cpu.registers.ip = ENTRY;
        machine.cpu.registers.set32(Gpr32::Esp, STACK);
    }
    assert_eq!(fast.step_with_idle_budget(32).unwrap().1, 4);
    reference_steps(&mut reference, 4);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 3);
    same_state(&mut fast, &mut reference);
}
