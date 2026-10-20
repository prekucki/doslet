use super::*;
use doslet_x86_wasm::static_program::{
    Accounting, Address, CompileOptions, Operation, Program, Region, RegionId, Register, Terminator,
};
use wasm_encoder::{
    CodeSection, ConstExpr, DataSection, ExportKind, ExportSection, Function, FunctionSection,
    Instruction as W, MemArg, MemorySection, MemoryType, Module, TypeSection, ValType,
};

fn memory_arg(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }
}

fn runtime(trap: bool) -> Vec<u8> {
    runtime_with_admission(trap, None)
}

// Some(true) supplies a reserving WASM admission function; Some(false) exports
// the ordinary callback under that name to exercise an incompatible signature.
fn runtime_with_admission(trap: bool, admission: Option<bool>) -> Vec<u8> {
    runtime_with_exports(trap, admission, false)
}

fn runtime_with_exports(trap: bool, admission: Option<bool>, wrong_memory_write: bool) -> Vec<u8> {
    let mut types = TypeSection::new();
    types
        .ty()
        .function([ValType::I32, ValType::I32], [ValType::I32]);
    types.ty().function([ValType::I32], [ValType::I32]);
    if admission == Some(true) {
        types
            .ty()
            .function([ValType::I32, ValType::I32, ValType::I32], [ValType::I32]);
    }
    let mut functions = FunctionSection::new();
    functions.function(0);
    functions.function(1);
    if admission == Some(true) {
        functions.function(2);
    }
    let mut memory = MemorySection::new();
    memory.memory(MemoryType {
        minimum: 1,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    exports.export("static_runtime", ExportKind::Func, 0);
    exports.export("peek", ExportKind::Func, 1);
    if let Some(valid) = admission {
        exports.export("static_admit", ExportKind::Func, if valid { 2 } else { 0 });
    }
    if wrong_memory_write {
        exports.export("static_memory_write", ExportKind::Func, 0);
    }
    let mut callback = Function::new([]);
    if trap {
        callback.instruction(&W::Unreachable);
    }
    // A fallback exits. A service increments its private counter and changes
    // the shared register/address frame, then continues in another region.
    for op in [
        W::LocalGet(1),
        W::I32Eqz,
        W::If(wasm_encoder::BlockType::Result(ValType::I32)),
        W::I32Const(256),
        W::I32Const(256),
        W::I32Load(memory_arg(0)),
        W::I32Const(1),
        W::I32Add,
        W::I32Store(memory_arg(0)),
        W::LocalGet(0),
        W::LocalGet(0),
        W::I32Load(memory_arg(0)),
        W::I32Const(100),
        W::I32Add,
        W::I32Store(memory_arg(0)),
        W::LocalGet(0),
        W::I32Const(10),
        W::I32Store(memory_arg(abi::OFFSET as u64)),
        W::I32Const(abi::CONTINUE as i32),
        W::Else,
        W::I32Const(abi::EXIT as i32),
        W::End,
        W::End,
    ] {
        callback.instruction(&op);
    }
    let mut peek = Function::new([]);
    peek.instruction(&W::LocalGet(0));
    peek.instruction(&W::I32Load(memory_arg(0)));
    peek.instruction(&W::End);
    let mut code = CodeSection::new();
    code.function(&callback);
    code.function(&peek);
    if admission == Some(true) {
        let mut admit = Function::new([(1, ValType::I32)]);
        for op in [
            W::I32Const(264),
            W::I32Const(264),
            W::I32Load(memory_arg(0)),
            W::I32Const(1),
            W::I32Add,
            W::I32Store(memory_arg(0)),
            W::I32Const(268),
            W::LocalGet(1),
            W::I32Store(memory_arg(0)),
            W::I32Const(272),
            W::LocalGet(2),
            W::I32Store(memory_arg(0)),
            W::I32Const(276),
            W::LocalGet(0),
            W::I32Store(memory_arg(0)),
            W::I32Const(260),
            W::I32Load(memory_arg(0)),
            W::LocalTee(3),
            W::LocalGet(2),
            W::I32GeU,
            W::If(wasm_encoder::BlockType::Result(ValType::I32)),
            W::I32Const(260),
            W::LocalGet(3),
            W::LocalGet(2),
            W::I32Sub,
            W::I32Store(memory_arg(0)),
            W::I32Const(1),
            W::Else,
            W::I32Const(0),
            W::End,
            W::End,
        ] {
            admit.instruction(&op);
        }
        code.function(&admit);
    }
    let mut data = DataSection::new();
    data.active(0, &ConstExpr::i32_const(44), 0x1234_5678u32.to_le_bytes());
    data.active(0, &ConstExpr::i32_const(260), 5u32.to_le_bytes());
    let mut module = Module::new();
    module.section(&types);
    module.section(&functions);
    module.section(&memory);
    module.section(&exports);
    module.section(&code);
    module.section(&data);
    module.finish()
}

fn program() -> Vec<u8> {
    program_with_admission(false)
}

fn program_with_admission(admitted: bool) -> Vec<u8> {
    Program {
        regions: vec![
            Region {
                id: RegionId(0),
                entry: Address {
                    space: 1,
                    offset: 0,
                },
                operations: vec![Operation::Inc(Register::Eax)],
                terminator: Terminator::Jump(RegionId(0)),
                accounting: Accounting::Counted,
            },
            Region {
                id: RegionId(1),
                entry: Address {
                    space: 1,
                    offset: 10,
                },
                operations: vec![],
                terminator: Terminator::Fallback(Address {
                    space: 1,
                    offset: 10,
                }),
                accounting: Accounting::Counted,
            },
        ],
    }
    .compile(&CompileOptions {
        runtime_admission: admitted,
        service_interval_regions: if admitted { 64 } else { 2 },
        ..CompileOptions::default()
    })
    .unwrap()
    .into_wasm()
}

#[test]
fn wasm_callback_and_generated_code_share_memory_without_a_host_bridge() {
    let artifacts = SharedArtifacts::compile(&runtime(false), &program()).unwrap();
    let mut first = artifacts.instantiate().unwrap();
    let second = artifacts.instantiate().unwrap();
    first
        .write_frame(
            0,
            &Frame {
                address: Address {
                    space: 1,
                    offset: 0,
                },
                remaining_budget: 100,
                ..Frame::default()
            },
        )
        .unwrap();
    assert_eq!(first.run(0).unwrap(), Exit::Runtime);
    let frame = first.read_frame(0).unwrap();
    assert_eq!(frame.gpr[0], 102);
    assert_eq!(frame.charged_work, 4);
    assert_eq!(frame.address.offset, 10);
    assert_eq!(first.call_runtime::<u32, u32>("peek", 256).unwrap(), 1);
    assert_eq!(
        first.call_runtime::<u32, u32>("peek", 44).unwrap(),
        0x1234_5678
    );
    assert_eq!(second.read_frame(0).unwrap().gpr[0], 0);
}

#[test]
fn runtime_trap_stops_all_entry_and_bad_frames_are_checked() {
    let artifacts = SharedArtifacts::compile(&runtime(true), &program()).unwrap();
    let mut runner = artifacts.instantiate().unwrap();
    assert!(runner.write_frame(65535, &Frame::default()).is_err());
    assert!(runner.read_frame(65535).is_err());
    runner
        .write_frame(
            0,
            &Frame {
                address: Address {
                    space: 1,
                    offset: 0,
                },
                remaining_budget: 100,
                ..Frame::default()
            },
        )
        .unwrap();
    assert!(runner.run(0).is_err());
    assert!(runner.run(0).unwrap_err().contains("stopped"));
    assert!(
        runner
            .call_runtime::<u32, u32>("peek", 256)
            .unwrap_err()
            .contains("stopped")
    );
}

#[test]
fn wasm_admission_links_directly_and_refuses_before_crossing_its_horizon() {
    let artifacts = SharedArtifacts::compile(
        &runtime_with_admission(false, Some(true)),
        &program_with_admission(true),
    )
    .unwrap();
    let mut runner = artifacts.instantiate().unwrap();
    let pointer = 1024;
    runner
        .write_frame(
            pointer,
            &Frame {
                address: Address {
                    space: 1,
                    offset: 0,
                },
                remaining_budget: 100,
                ..Frame::default()
            },
        )
        .unwrap();
    assert_eq!(runner.run(pointer).unwrap(), Exit::Runtime);
    let frame = runner.read_frame(pointer).unwrap();
    assert_eq!(
        (frame.gpr[0], frame.charged_work, frame.remaining_budget),
        (2, 4, 96)
    );
    assert_eq!(
        frame.address,
        Address {
            space: 1,
            offset: 0
        }
    );
    assert_eq!(runner.call_runtime::<u32, u32>("peek", 260).unwrap(), 1);
    assert_eq!(runner.call_runtime::<u32, u32>("peek", 264).unwrap(), 3);
    assert_eq!(runner.call_runtime::<u32, u32>("peek", 268).unwrap(), 0);
    assert_eq!(runner.call_runtime::<u32, u32>("peek", 272).unwrap(), 2);
    assert_eq!(
        runner.call_runtime::<u32, u32>("peek", 276).unwrap(),
        pointer
    );
}

#[test]
fn admitted_program_requires_a_correctly_typed_runtime_export() {
    let missing = SharedArtifacts::compile(&runtime(false), &program_with_admission(true)).unwrap();
    let error = match missing.instantiate() {
        Ok(_) => panic!("missing admission export linked"),
        Err(error) => error,
    };
    assert!(
        error.contains("requires runtime export static_admit"),
        "{error}"
    );

    // An invalid optional export is diagnosed even for a program that does not
    // import it, so the runtime cannot silently advertise an incompatible hook.
    for admitted in [false, true] {
        let wrong = SharedArtifacts::compile(
            &runtime_with_admission(false, Some(false)),
            &program_with_admission(admitted),
        )
        .unwrap();
        let error = match wrong.instantiate() {
            Ok(_) => panic!("incompatible admission export linked"),
            Err(error) => error,
        };
        assert!(
            error.contains("static_admit must have signature"),
            "{error}"
        );
    }
}

#[test]
fn memory_write_helper_requires_a_correctly_typed_runtime_export() {
    let program = Program {
        regions: vec![Region {
            id: RegionId(0),
            entry: Address {
                space: 1,
                offset: 0,
            },
            operations: vec![Operation::Nop],
            terminator: Terminator::Fallback(Address {
                space: 1,
                offset: 1,
            }),
            accounting: Accounting::Counted,
        }],
    }
    .compile(&CompileOptions {
        direct_memory: true,
        inline_admission: true,
        memory_write_helper: true,
        ..CompileOptions::default()
    })
    .unwrap();
    let missing = SharedArtifacts::compile(&runtime(false), program.wasm()).unwrap();
    let Err(error) = missing.instantiate() else {
        panic!("missing memory helper export linked");
    };
    assert!(
        error.contains("require runtime export static_memory_write"),
        "{error}"
    );
    let wrong =
        SharedArtifacts::compile(&runtime_with_exports(false, None, true), program.wasm()).unwrap();
    let Err(error) = wrong.instantiate() else {
        panic!("incompatible memory helper export linked");
    };
    assert!(
        error.contains("static_memory_write must have signature"),
        "{error}"
    );
}
