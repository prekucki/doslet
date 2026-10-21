//! Synthetic DJGPP layout through the real cache builder and PC owner.
use crate::procedure_cache::{CodeSegment, Recipe, Target, build_target};
use doslet_aot::{ImageLimits, SymbolicImage, coff_offset_bindings};
use doslet_executable::{CoffImage, CoffLimits};
use doslet_pc::{PcMachine, io::PcBus};
use doslet_x86::{Gpr32, SegmentRegister};
use doslet_x86_wasm::static_program::procedure_memory::bundle;

use crate::native_input::wasm_procedures::procedure_manifest;

mod fixture {
    include!("../../../tests/fixtures/djgpp_coff.rs");
}
const BASE: u32 = 0x120000;
const LIMIT: u32 = 0xffff;
const ENTRY: u32 = fixture::TEXT_VADDR + 10;
const DATA: u32 = 0x3000;
const OUTPUT: u32 = 0x4000;
const STACK: u32 = 0x7000;
const RETURN: u32 = 0x7800;

fn image(exe: &[u8]) -> SymbolicImage {
    let coff = CoffImage::discover(exe, CoffLimits::default()).unwrap();
    SymbolicImage::from_coff_with_text(&coff, None, ImageLimits::default()).unwrap()
}

fn compile(text: &[u8], recipe: &Recipe, entry_offset: u32) -> crate::procedure_cache::Module {
    let exe = fixture::djgpp_exe(text, &[0], 16, 0, &[]);
    let image = image(&exe);
    let bindings = coff_offset_bindings(&image, 8);
    build_target(
        &Target {
            exe: &exe,
            image: &image,
            bindings: &bindings,
            code: CodeSegment {
                base: BASE,
                limit: LIMIT,
            },
            dpmi: "direct",
            layout: "synthetic-nonflat-slots",
        },
        recipe,
        &mut |_, _, sources| {
            Ok(sources
                .first()
                .is_some_and(|a| a.object_number == 1 && a.offset == entry_offset))
        },
        &mut |_| {},
    )
    .unwrap()
}

fn text() -> Vec<u8> {
    // CALL writer; MOV [disp slot],EAX; loop: MOV AL,[EBX+disp32];
    // ADD EDI,imm32; MOV [EDI],AL; DEC ECX; JNZ loop; RET;
    // writer: MOV [imm slot],EBX; RET. Both code writers are CFG-reachable.
    let mut code = vec![
        0xe8, 27, 0, 0, 0, 0xa3, 0, 0, 0, 0, 0x8a, 0x83, 0, 0x30, 0, 0, 0x81, 0xc7, 4, 0, 0, 0,
        0x88, 7, 0x49, 0x75, 0xef, 0xc3, 0x90, 0x90, 0x90, 0x90, 0x89, 0x1d, 0, 0, 0, 0, 0xc3,
    ];
    code[6..10].copy_from_slice(&(fixture::TEXT_VADDR + 12).to_le_bytes());
    code[34..38].copy_from_slice(&(fixture::TEXT_VADDR + 18).to_le_bytes());
    code
}

fn write(bus: &mut PcBus, at: u32, bytes: &[u8]) {
    bus.debug_ram_mut(at, bytes.len() as u32)
        .unwrap()
        .copy_from_slice(bytes);
}
fn word(bus: &mut PcBus, at: u32, value: u32) {
    write(bus, at, &value.to_le_bytes());
}
fn descriptor(bus: &mut PcBus, selector: u16, base: u32, code: bool) {
    let bytes = [
        0xff,
        0xff,
        base as u8,
        (base >> 8) as u8,
        (base >> 16) as u8,
        if code { 0x9b } else { 0x93 },
        0x4f,
        (base >> 24) as u8,
    ];
    write(bus, 0x800 + u32::from(selector), &bytes);
}
fn machine(code: &[u8]) -> PcMachine {
    let mut machine = PcMachine::from_config(doslet_config::HardwareConfig {
        memory_mib: 4,
        ..Default::default()
    })
    .unwrap();
    machine.bus.set_a20(true);
    descriptor(&mut machine.bus, 8, BASE, true);
    descriptor(&mut machine.bus, 16, BASE, false);
    machine.cpu.control.cr0 |= 1;
    machine.cpu.control.gdtr.base = 0x800;
    machine.cpu.control.gdtr.limit = 0xff;
    write(&mut machine.bus, 0, &[0x90]);
    machine.cpu.step(&mut machine.bus).unwrap(); // Synchronize PE caches.
    for segment in SegmentRegister::ALL {
        machine
            .cpu
            .load_segment(
                &mut machine.bus,
                segment,
                if segment == SegmentRegister::Cs {
                    8
                } else {
                    16
                },
            )
            .unwrap();
    }
    write(&mut machine.bus, BASE, &[0x90]);
    machine.cpu.registers.ip = 0;
    machine.cpu.step(&mut machine.bus).unwrap(); // SS interrupt shadow.
    machine.cpu.set_dynamic_core(false);
    machine.cpu.flags = 2;
    write(&mut machine.bus, BASE + fixture::TEXT_VADDR, code);
    write(
        &mut machine.bus,
        BASE + DATA,
        &[0x11, 0x22, 0x33, 0x44, 0xaa, 0xbb, 0xcc, 0xdd],
    );
    reset(&mut machine, DATA, 3, OUTPUT);
    machine
}
fn reset(machine: &mut PcMachine, displacement: u32, stride: u32, output: u32) {
    machine.cpu.registers.ip = ENTRY;
    machine.cpu.registers.set32(Gpr32::Ebx, 1);
    machine.cpu.registers.set32(Gpr32::Edi, output);
    machine.cpu.registers.set32(Gpr32::Ecx, 2);
    machine.cpu.registers.set32(Gpr32::Esp, STACK);
    word(&mut machine.bus, BASE + STACK, RETURN);
    word(
        &mut machine.bus,
        BASE + fixture::TEXT_VADDR + 12,
        displacement,
    );
    word(&mut machine.bus, BASE + fixture::TEXT_VADDR + 18, stride);
}
fn same(fast: &PcMachine, reference: &PcMachine) {
    assert!(fast.cpu.matches(&reference.cpu.snapshot()));
    assert_eq!(fast.time(), reference.time());
    assert_eq!(fast.ram_writes(), reference.ram_writes());
    assert_eq!(fast.device_accesses(), reference.device_accesses());
    assert_eq!(
        fast.bus.debug_ram(BASE, 0x10000).unwrap(),
        reference.bus.debug_ram(BASE, 0x10000).unwrap()
    );
}

#[test]
fn nonflat_cache_slots_read_patched_values_and_guard_only_immutable_source_bytes() {
    let text = text();
    let module = compile(&text, &Recipe::DESKTOP, 10);
    let metadata = bundle::read(&module.bytes).unwrap().unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(metadata.metadata).unwrap();
    assert_eq!(manifest["analysis_version"], 1);
    assert_eq!(manifest["functions"][0]["analysis"]["x87_class"], "none");
    assert_eq!(
        manifest["functions"][0]["analysis"]["maximum_work"],
        manifest["functions"][0]["maximum_work"]
    );
    let bindings = procedure_manifest::parse(metadata.metadata).unwrap();
    assert_eq!(bindings.len(), 1);
    let binding = &bindings[0];
    assert_eq!(binding.entry_ip, ENTRY);
    assert_eq!(binding.code_segment.base, BASE);
    let guarded = |at| {
        binding
            .code
            .iter()
            .any(|part| part.linear <= at && at < part.linear + part.expected.len() as u32)
    };
    for slot in [
        BASE + fixture::TEXT_VADDR + 12,
        BASE + fixture::TEXT_VADDR + 18,
    ] {
        for byte in slot..slot + 4 {
            assert!(!guarded(byte), "operand slot must be a source hole");
        }
    }
    assert!(guarded(BASE + ENTRY));
    assert!(guarded(BASE + ENTRY + 1));
    assert!(guarded(BASE + fixture::TEXT_VADDR + 16));
    let mut fast = machine(&text);
    let mut reference = machine(&text);
    fast.install_wasm_procedures(&module.bytes, bindings)
        .unwrap();
    assert!(fast.rebase_wasm_procedures(BASE + 0x100000).is_err());
    for (displacement, stride, output) in [(DATA, 3, OUTPUT), (DATA + 4, 5, OUTPUT + 0x20)] {
        for machine in [&mut fast, &mut reference] {
            reset(machine, displacement, stride, output);
        }
        let (_, work) = fast.step_with_idle_budget(4096).unwrap();
        assert_eq!(work, 11);
        for _ in 0..work {
            assert_eq!(reference.step_with_idle_budget(1).unwrap().1, 1);
        }
        assert_eq!(fast.cpu.registers.ip, RETURN);
        same(&fast, &reference);
        assert_eq!(
            fast.bus.debug_ram(BASE + output + stride, 1).unwrap(),
            &[if displacement == DATA { 0x22 } else { 0xbb }]
        );
    }
    let stats = fast.wasm_procedure_stats().unwrap();
    assert_eq!(stats.calls, 2);
    assert_eq!(stats.source_refusals, 0);
    // A real opcode change retains fallback, while changed operand slots above
    // were accepted without renewing the immutable source-byte expectation.
    for machine in [&mut fast, &mut reference] {
        reset(machine, DATA, 3, OUTPUT + 0x40);
        write(&mut machine.bus, BASE + ENTRY, &[0x88]); // MOV [EBX+disp],AL, same length.
    }
    let (_, work) = fast.step_with_idle_budget(1).unwrap();
    for _ in 0..work {
        reference.step_with_idle_budget(1).unwrap();
    }
    same(&fast, &reference);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 2);
    assert!(fast.wasm_procedure_stats().unwrap().source_refusals > 0);
}

#[test]
fn desktop_recipe_keeps_bounded_memory_counter_fragments_without_finite_metadata() {
    // PUSH ES; loop: CMP byte[EAX],0; JZ done; SUB byte[EAX],1;
    // MOV[EDI],ECX; JMP loop; done: POP ES; RET.
    let text = [
        0x06, 0x80, 0x38, 0, 0x74, 7, 0x80, 0x28, 1, 0x89, 0x0f, 0xeb, 0xf4, 0x07, 0xc3,
    ];
    let exe = fixture::djgpp_exe(&text, &[0], 16, 0, &[]);
    let image = image(&exe);
    let bindings = coff_offset_bindings(&image, 8);
    let target = Target {
        exe: &exe,
        image: &image,
        bindings: &bindings,
        code: CodeSegment {
            base: BASE,
            limit: LIMIT,
        },
        dpmi: "direct",
        layout: "bounded-fragment-recipe",
    };
    let keep = |_: &doslet_aot::cfg::StaticCfg<'_>,
                _: &SymbolicImage,
                sources: &[doslet_aot::ObjectAddress]| {
        Ok(sources
            .first()
            .is_some_and(|a| a.object_number == 1 && a.offset == 1))
    };
    let module = build_target(&target, &Recipe::DESKTOP, &mut { keep }, &mut |_| {}).unwrap();
    let metadata = bundle::read(&module.bytes).unwrap().unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(metadata.metadata).unwrap();
    let functions = manifest["functions"].as_array().unwrap();
    assert_eq!(functions.len(), 1);
    assert_eq!(functions[0]["entry"], fixture::TEXT_VADDR + 1);
    assert_eq!(functions[0]["maximum_work"], 128);
    let sites = functions[0]["execution_sites"].as_array().unwrap();
    assert_eq!(sites.first().unwrap()["ip"], fixture::TEXT_VADDR + 1);
    assert!(
        !sites
            .iter()
            .any(|site| site["ip"] == fixture::TEXT_VADDR + 13)
    ); // Unexecuted POP ES.
    assert!(functions[0]["finite_computation"].is_null());
    assert_eq!(manifest["bounded_fragments"], true);
    let legacy = Recipe {
        bounded_fragments: false,
        long_fragments: false,
        ..Recipe::DESKTOP
    };
    assert!(build_target(&target, &legacy, &mut { keep }, &mut |_| {}).is_err());
    let missing_limit = Recipe {
        max_work_per_call: None,
        ..Recipe::DESKTOP
    };
    assert!(build_target(&target, &missing_limit, &mut { keep }, &mut |_| {}).is_err());
}

#[test]
fn generated_long_scalar_prefix_keeps_x87_exit_bytes_without_executing_or_tiering_them() {
    let mut text = vec![0x40; 40];
    text.extend_from_slice(&[0xd9, 0xe8, 0xc3]);
    let module = compile(&text, &Recipe::DESKTOP, 0);
    let metadata = bundle::read(&module.bytes).unwrap().unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(metadata.metadata).unwrap();
    let functions = manifest["functions"].as_array().unwrap();
    assert_eq!(functions.len(), 1);
    let sites = functions[0]["execution_sites"].as_array().unwrap();
    assert_eq!(sites.len(), 40);
    for (offset, site) in sites.iter().enumerate() {
        assert_eq!(site["ip"], fixture::TEXT_VADDR + offset as u32);
        assert_eq!(site["length"], 1);
    }
    assert!(
        functions[0]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|source| source["linear"] == BASE + fixture::TEXT_VADDR + 40
                && source["bytes"] == serde_json::json!([0xd9, 0xe8]))
    );
    let parsed = procedure_manifest::parse(metadata.metadata).unwrap();
    assert_eq!(parsed[0].execution_sites.as_ref().unwrap().len(), 40);
}

#[test]
fn opt_in_nonflat_x87_cache_keeps_metadata_and_exact_loop_state() {
    // MOV ECX,3; loop: FLD real32[DATA]; FILD integer32[DATA+4];
    // FADDP ST1,ST0; FLD ST0; FXCH ST1; FADDP ST1,ST0;
    // FISTP integer32[EDI]; ADD EDI,4; DEC ECX; JNZ loop; RET.
    let text = [
        0xb9, 3, 0, 0, 0, 0xd9, 0x05, 0, 0x30, 0, 0, 0xdb, 0x05, 4, 0x30, 0, 0, 0xde, 0xc1, 0xd9,
        0xc0, 0xd9, 0xc9, 0xde, 0xc1, 0xdb, 0x1f, 0x83, 0xc7, 4, 0x49, 0x75, 0xe4, 0xc3,
    ];
    let recipe = Recipe {
        pc24_locals: true,
        ..Recipe::DESKTOP
    };
    let module = compile(&text, &recipe, 0);
    let metadata = bundle::read(&module.bytes).unwrap().unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(metadata.metadata).unwrap();
    assert_eq!(manifest["pc24_locals"], true);
    let analysis = &manifest["functions"][0]["analysis"];
    assert_eq!(analysis["execution"], "bounded-loop");
    assert_eq!(analysis["x87_class"], "independent-balanced");
    assert_eq!(analysis["x87_effects"]["stack_max"], 2);
    assert_eq!(analysis["x87_effects"]["declared_exits_balanced"], true);
    assert_eq!(analysis["x87_effects"]["all_boundaries_balanced"], false);
    assert_eq!(analysis["x87_effects"]["preserves_full_state"], false);
    let bindings = procedure_manifest::parse(metadata.metadata).unwrap();
    assert_eq!(bindings.len(), 1);
    let binding = &bindings[0];
    assert_eq!(binding.entry_ip, fixture::TEXT_VADDR);
    assert_eq!(binding.code_segment.base, BASE);
    let x87 = binding.x87.expect("explicit compiler x87 permissions");
    assert_eq!(x87.read_mask, 0);
    assert_ne!(x87.write_mask, 0);
    let sites = binding.execution_sites.as_ref().unwrap();
    assert_eq!(sites.len(), 12);
    assert_eq!(sites[1].ip, fixture::TEXT_VADDR + 5);
    assert_eq!(sites[1].length, 6);
    assert_eq!(sites.last().unwrap().ip, fixture::TEXT_VADDR + 33);

    let mut fast = machine(&text);
    let mut reference = machine(&text);
    for machine in [&mut fast, &mut reference] {
        // The older scalar fixture's reset patches two text operands. Restore
        // this independent program before installing its immutable sources.
        write(&mut machine.bus, BASE + fixture::TEXT_VADDR, &text);
        machine.cpu.registers.ip = fixture::TEXT_VADDR;
        machine.cpu.registers.set32(Gpr32::Edi, OUTPUT);
        machine.cpu.registers.set32(Gpr32::Esp, STACK);
        word(&mut machine.bus, BASE + STACK, RETURN);
        word(&mut machine.bus, BASE + DATA, 1.5f32.to_bits());
        word(&mut machine.bus, BASE + DATA + 4, 2);
        let fpu = machine.cpu.fpu.as_mut().unwrap();
        fpu.load_control(0x007f);
        fpu.push(7.0).unwrap();
    }
    fast.install_wasm_procedures(&module.bytes, bindings)
        .unwrap();
    let (_, work) = fast.step_with_idle_budget(4096).unwrap();
    assert_eq!(work, 32);
    for _ in 0..work {
        assert_eq!(reference.step_with_idle_budget(1).unwrap().1, 1);
    }
    same(&fast, &reference);
    assert!(
        fast.cpu
            .fpu
            .as_ref()
            .unwrap()
            .bitwise_eq(reference.cpu.fpu.as_ref().unwrap())
    );
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 1);
    assert_eq!(fast.cpu.registers.ip, RETURN);
    for offset in [0, 4, 8] {
        assert_eq!(
            fast.bus.debug_ram(BASE + OUTPUT + offset, 4).unwrap(),
            7u32.to_le_bytes()
        );
    }
    let environment = fast
        .cpu
        .fpu
        .as_ref()
        .unwrap()
        .save_environment(true, true, false);
    assert_eq!(
        &environment[12..16],
        &(fixture::TEXT_VADDR + 25).to_le_bytes()
    );
    assert_eq!(&environment[16..18], &8u16.to_le_bytes());
    assert_eq!(&environment[18..20], &0x31fu16.to_le_bytes());
    assert_eq!(&environment[20..24], &(OUTPUT + 8).to_le_bytes());
    assert_eq!(&environment[24..26], &16u16.to_le_bytes());
}

#[test]
fn rejected_pc24_cfg_keeps_exact_nonflat_prefix_before_first_branch() {
    let mut text = vec![0xdb, 0x00]; // FILD integer32[EAX].
    text.resize(33, 0x43); // 31 INC EBX instructions.
    text.extend_from_slice(&[0x74, 4, 0xeb, 0xfe, 0xcc, 0xcc, 0xeb, 0xfe]);
    let recipe = Recipe {
        pc24_locals: true,
        ..Recipe::DESKTOP
    };
    let module = compile(&text, &recipe, 0);
    let metadata = bundle::read(&module.bytes).unwrap().unwrap();
    let bindings = procedure_manifest::parse(metadata.metadata).unwrap();
    assert_eq!(bindings.len(), 1);
    let binding = &bindings[0];
    assert_eq!(binding.maximum_instructions, 32);
    assert!(binding.x87.is_some());
    let boundary = fixture::TEXT_VADDR + 33;
    assert_eq!(binding.execution_sites.as_ref().unwrap().len(), 32);
    assert!(
        !binding
            .execution_sites
            .as_ref()
            .unwrap()
            .iter()
            .any(|site| site.ip == boundary)
    );
    assert!(
        binding
            .code
            .iter()
            .any(|part| part.linear == BASE + boundary && part.expected == [0x74, 4])
    );
    let mut fast = machine(&text);
    let mut reference = machine(&text);
    for machine in [&mut fast, &mut reference] {
        write(&mut machine.bus, BASE + fixture::TEXT_VADDR, &text);
        machine.cpu.registers.ip = fixture::TEXT_VADDR;
        machine.cpu.registers.set32(Gpr32::Eax, DATA);
        machine.cpu.registers.set32(Gpr32::Ebx, 5);
        word(&mut machine.bus, BASE + DATA, (-12345i32) as u32);
        machine.cpu.fpu.as_mut().unwrap().load_control(0x007f);
    }
    fast.install_wasm_procedures(&module.bytes, bindings)
        .unwrap();
    assert_eq!(fast.step_with_idle_budget(4096).unwrap().1, 32);
    for _ in 0..32 {
        assert_eq!(reference.step_with_idle_budget(1).unwrap().1, 1);
    }
    same(&fast, &reference);
    assert!(
        fast.cpu
            .fpu
            .as_ref()
            .unwrap()
            .bitwise_eq(reference.cpu.fpu.as_ref().unwrap())
    );
    assert_eq!(fast.cpu.registers.ip, boundary);
    assert_eq!(fast.cpu.registers.get32(Gpr32::Ebx), 36);
    assert_eq!(fast.wasm_procedure_stats().unwrap().calls, 1);
    assert_eq!(fast.wasm_procedure_stats().unwrap().side_exits, 1);
}
