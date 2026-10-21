use doslet_static_probe::x87;
use doslet_x86_wasm::static_program::abi;
use wasm_encoder::{
    BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction as W, MemArg, MemoryType, Module, TypeSection, ValType,
};

fn memory(offset: u64) -> MemArg {
    MemArg {
        offset,
        align: 2,
        memory_index: 0,
    }
}

/// Only two WASM helper imports and one memory import. No host callback or
/// private Rust-layout offset is embedded. Omitting a memory re-export avoids
/// duplicate export names when Binaryen merges this module with its owner.
pub fn emit() -> Vec<u8> {
    let mut module = Module::new();
    let mut types = TypeSection::new();
    types.ty().function([ValType::I32; 5], [ValType::I64]);
    types.ty().function([ValType::I32], [ValType::I32]);
    let mut imports = ImportSection::new();
    imports.import(
        abi::IMPORT_MODULE,
        "memory",
        EntityType::Memory(MemoryType {
            minimum: 0,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        }),
    );
    imports.import(
        abi::IMPORT_MODULE,
        "static_x87_operation",
        EntityType::Function(0),
    );
    imports.import(
        abi::IMPORT_MODULE,
        "x87_probe_frame",
        EntityType::Function(1),
    );
    let mut functions = FunctionSection::new();
    functions.function(1);
    let mut exports = ExportSection::new();
    exports.export("run", ExportKind::Func, 2);
    let mut f = Function::new([(5, ValType::I32), (1, ValType::I64)]);
    // 0=handle, 1=frame, 2=iterations, 3=iteration, 4=checksum,
    // 5=converted integer, 6=packed helper result.
    for op in [
        W::LocalGet(0),
        W::Call(1),
        W::LocalSet(1),
        W::LocalGet(1),
        W::I32Load(memory(0)),
        W::LocalSet(2),
        W::LocalGet(2),
        W::I32Eqz,
        W::LocalGet(2),
        W::I32Const(x87::MAX_ITERATIONS as i32),
        W::I32GtU,
        W::I32Or,
        W::If(BlockType::Empty),
        W::LocalGet(1),
        W::I32Const(5),
        W::I32Store(memory(12)),
        W::I32Const(abi::EXIT_BUDGET as i32),
        W::Return,
        W::End,
        W::LocalGet(1),
        W::I32Load(memory(4)),
        W::LocalSet(4),
        W::Block(BlockType::Empty),
        W::Loop(BlockType::Empty),
        W::LocalGet(3),
        W::LocalGet(2),
        W::I32GeU,
        W::BrIf(1),
    ] {
        f.instruction(&op);
    }
    for operation in x87::LOAD..=x87::STORE_INTEGER_POP {
        f.instruction(&W::LocalGet(0));
        f.instruction(&W::I32Const(operation as i32));
        match operation {
            x87::LOAD => {
                for op in [
                    W::LocalGet(3),
                    W::I32Const(0xffff),
                    W::I32And,
                    W::I32Const(x87::BASE as i32),
                    W::I32Add,
                ] {
                    f.instruction(&op);
                }
            }
            x87::MULTIPLY => {
                f.instruction(&W::I32Const(x87::MULTIPLIER as i32));
            }
            x87::ADD => {
                f.instruction(&W::I32Const(x87::ADDEND as i32));
            }
            _ => {
                f.instruction(&W::I32Const(0));
            }
        }
        for op in [
            W::I32Const((x87::START_IP + operation * 4) as i32),
            W::I32Const((x87::OPERAND + operation * 4) as i32),
            W::Call(0),
            W::LocalSet(6),
            W::LocalGet(6),
            W::I64Const(x87::ACCEPTED as i64),
            W::I64And,
            W::I64Eqz,
            W::If(BlockType::Empty),
            W::LocalGet(1),
            W::I32Const(operation as i32 + 1),
            W::I32Store(memory(12)),
            W::I32Const(abi::EXIT_BUDGET as i32),
            W::Return,
            W::End,
        ] {
            f.instruction(&op);
        }
    }
    for op in [
        W::LocalGet(6),
        W::I32WrapI64,
        W::LocalSet(5),
        W::LocalGet(4),
        W::I32Const(5),
        W::I32Rotl,
        W::LocalGet(5),
        W::I32Add,
        W::LocalGet(3),
        W::I32Const(0x9e37),
        W::I32Xor,
        W::I32Add,
        W::LocalSet(4),
        W::LocalGet(3),
        W::I32Const(1),
        W::I32Add,
        W::LocalSet(3),
        W::LocalGet(1),
        W::LocalGet(4),
        W::I32Store(memory(4)),
        W::LocalGet(1),
        W::LocalGet(3),
        W::I32Store(memory(8)),
        W::Br(0),
        W::End,
        W::End,
        W::I32Const(abi::EXIT_RUNTIME as i32),
        W::End,
    ] {
        f.instruction(&op);
    }
    let mut code = CodeSection::new();
    code.function(&f);
    module.section(&types);
    module.section(&imports);
    module.section(&functions);
    module.section(&exports);
    module.section(&code);
    module.finish()
}
