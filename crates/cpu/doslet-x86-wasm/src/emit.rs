use crate::{BINDINGS, DISPATCH_PROBES, DISPATCH_SLOTS, IP, QUOTA, Translation};
use alloc::{borrow::Cow, vec::Vec};
use doslet_x86::translation::{Operation as Op, Source};
use wasm_encoder::{
    BlockType, CodeSection, ConstExpr, CustomSection, ElementSection, Elements, EntityType,
    ExportKind, ExportSection, Function, FunctionSection, ImportSection, Instruction as W, MemArg,
    MemorySection, MemoryType, Module, RefType, TableSection, TableType, TypeSection, ValType,
};

fn ins(f: &mut Function, op: W<'_>) {
    f.instruction(&op);
}
fn c(f: &mut Function, value: u32) {
    ins(f, W::I32Const(value as i32));
}
fn get(f: &mut Function, local: u32) {
    ins(f, W::LocalGet(local));
}
fn set(f: &mut Function, local: u32) {
    ins(f, W::LocalSet(local));
}
fn arg(offset: u32) -> MemArg {
    MemArg {
        offset: offset as u64,
        align: 2,
        memory_index: 0,
    }
}
fn load(f: &mut Function, offset: u32) {
    c(f, 0);
    ins(f, W::I32Load(arg(offset)));
}
fn store_local(f: &mut Function, offset: u32, local: u32) {
    c(f, 0);
    get(f, local);
    ins(f, W::I32Store(arg(offset)));
}
fn source(f: &mut Function, source: Source) {
    match source {
        Source::Register(r) => get(f, r as u32),
        Source::Immediate(v) => c(f, v),
    }
}
// Block locals: r0..r7, flags=8, ip=9, lhs=10, rhs=11, result=12,
// carry-in=13, wide-result(i64)=14. All arithmetic is explicitly wrapping.
fn alu(f: &mut Function, operation: u8, preserve_carry: bool) {
    let subtract = matches!(operation, 3 | 5 | 7);
    let logical = matches!(operation, 1 | 4 | 6);
    if matches!(operation, 2 | 3) {
        get(f, 8);
        c(f, 1);
        ins(f, W::I32And);
    } else {
        c(f, 0);
    }
    set(f, 13);
    get(f, 10);
    if !logical {
        ins(f, W::I64ExtendI32U);
    }
    get(f, 11);
    if !logical {
        ins(f, W::I64ExtendI32U);
        get(f, 13);
        ins(f, W::I64ExtendI32U);
        ins(f, W::I64Add);
        ins(f, if subtract { W::I64Sub } else { W::I64Add });
        ins(f, W::LocalTee(14));
        ins(f, W::I32WrapI64);
    } else {
        ins(
            f,
            match operation {
                1 => W::I32Or,
                4 => W::I32And,
                _ => W::I32Xor,
            },
        );
    }
    set(f, 12);
    get(f, 8);
    c(f, !0x8d5);
    ins(f, W::I32And);
    c(f, 2);
    ins(f, W::I32Or);
    // ZF, SF, PF; undefined AF for logical ops follows the interpreter (zero).
    get(f, 12);
    ins(f, W::I32Eqz);
    c(f, 6);
    ins(f, W::I32Shl);
    ins(f, W::I32Or);
    get(f, 12);
    c(f, 31);
    ins(f, W::I32ShrU);
    c(f, 7);
    ins(f, W::I32Shl);
    ins(f, W::I32Or);
    get(f, 12);
    c(f, 255);
    ins(f, W::I32And);
    ins(f, W::I32Popcnt);
    c(f, 1);
    ins(f, W::I32And);
    ins(f, W::I32Eqz);
    c(f, 2);
    ins(f, W::I32Shl);
    ins(f, W::I32Or);
    if !logical {
        // Bit 32 detects carry and borrow, including ADC/SBB at 0xffffffff.
        get(f, 14);
        ins(f, W::I64Const(32));
        ins(f, W::I64ShrU);
        ins(f, W::I32WrapI64);
        c(f, 1);
        ins(f, W::I32And);
        ins(f, W::I32Or);
        get(f, 10);
        get(f, 11);
        ins(f, W::I32Xor);
        get(f, 12);
        ins(f, W::I32Xor);
        c(f, 16);
        ins(f, W::I32And);
        ins(f, W::I32Or);
        get(f, 10);
        get(f, 11);
        ins(f, W::I32Xor);
        if !subtract {
            c(f, u32::MAX);
            ins(f, W::I32Xor);
        }
        get(f, 10);
        get(f, 12);
        ins(f, W::I32Xor);
        ins(f, W::I32And);
        c(f, 31);
        ins(f, W::I32ShrU);
        c(f, 11);
        ins(f, W::I32Shl);
        ins(f, W::I32Or);
    }
    if preserve_carry {
        c(f, !1);
        ins(f, W::I32And);
        get(f, 8);
        c(f, 1);
        ins(f, W::I32And);
        ins(f, W::I32Or);
    }
    set(f, 8);
}
fn flag(f: &mut Function, bit: u32) {
    get(f, 8);
    c(f, bit);
    ins(f, W::I32And);
    c(f, 0);
    ins(f, W::I32Ne);
}
fn condition(f: &mut Function, code: u8) {
    match code >> 1 {
        0 => flag(f, 0x800),
        1 => flag(f, 1),
        2 => flag(f, 0x40),
        3 => {
            flag(f, 1);
            flag(f, 0x40);
            ins(f, W::I32Or);
        }
        4 => flag(f, 0x80),
        5 => flag(f, 4),
        _ => {
            flag(f, 0x80);
            flag(f, 0x800);
            ins(f, W::I32Xor);
            if code >> 1 == 7 {
                flag(f, 0x40);
                ins(f, W::I32Or);
            }
        }
    }
    if code & 1 != 0 {
        ins(f, W::I32Eqz);
    }
}
fn block(block: &crate::Block) -> Function {
    let mut f = Function::new([(14, ValType::I32), (1, ValType::I64)]);
    for r in 0..10 {
        load(&mut f, r * 4);
        set(&mut f, r);
    }
    for i in &block.instructions {
        get(&mut f, 9);
        c(&mut f, i.length as u32);
        ins(&mut f, W::I32Add);
        set(&mut f, 9);
        match i.operation {
            Op::Move { target, source: s } => {
                source(&mut f, s);
                set(&mut f, target as u32);
            }
            Op::Alu {
                target,
                source: s,
                operation,
                store,
            } => {
                get(&mut f, target as u32);
                set(&mut f, 10);
                source(&mut f, s);
                set(&mut f, 11);
                alu(&mut f, operation, false);
                if store {
                    get(&mut f, 12);
                    set(&mut f, target as u32);
                }
            }
            Op::Inc {
                register,
                decrement,
            } => {
                get(&mut f, register as u32);
                set(&mut f, 10);
                c(&mut f, 1);
                set(&mut f, 11);
                alu(&mut f, if decrement { 5 } else { 0 }, true);
                get(&mut f, 12);
                set(&mut f, register as u32);
            }
            Op::Exchange { left, right } => {
                get(&mut f, left as u32);
                set(&mut f, 10);
                get(&mut f, right as u32);
                set(&mut f, left as u32);
                get(&mut f, 10);
                set(&mut f, right as u32);
            }
            Op::Unary { register, negate } => {
                if negate {
                    c(&mut f, 0);
                    set(&mut f, 10);
                    get(&mut f, register as u32);
                    set(&mut f, 11);
                    alu(&mut f, 5, false);
                    get(&mut f, 12);
                } else {
                    get(&mut f, register as u32);
                    c(&mut f, u32::MAX);
                    ins(&mut f, W::I32Xor);
                }
                set(&mut f, register as u32);
            }
            Op::Branch {
                condition: cc,
                displacement,
            } => {
                get(&mut f, 9);
                c(&mut f, displacement as u32);
                ins(&mut f, W::I32Add);
                if let Some(cc) = cc {
                    get(&mut f, 9);
                    condition(&mut f, cc);
                    ins(&mut f, W::Select);
                }
                set(&mut f, 9);
            }
        }
    }
    for r in 0..10 {
        store_local(&mut f, r * 4, r);
    }
    c(&mut f, block.instructions.len() as u32);
    ins(&mut f, W::End);
    f
}

pub fn module(t: &Translation) -> Vec<u8> {
    let mut m = Module::new();
    let mut types = TypeSection::new();
    types
        .ty()
        .function([ValType::I32, ValType::I32], [ValType::I32]); // poll
    types.ty().function([], [ValType::I32]); // block / resolve
    types.ty().function([ValType::I32], [ValType::I32]); // run
    m.section(&types);
    let mut imports = ImportSection::new();
    imports.import("doslet", "poll", EntityType::Function(0));
    m.section(&imports);
    let n = t.blocks.len() as u32;
    let mut functions = FunctionSection::new();
    for _ in 0..n + 1 {
        functions.function(1);
    }
    functions.function(2);
    m.section(&functions);
    let mut tables = TableSection::new();
    tables.table(TableType {
        element_type: RefType::FUNCREF,
        table64: false,
        minimum: n as u64,
        maximum: Some(n as u64),
        shared: false,
    });
    m.section(&tables);
    let mut memories = MemorySection::new();
    memories.memory(MemoryType {
        minimum: 2,
        maximum: Some(2),
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    m.section(&memories);
    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    exports.export("run", ExportKind::Func, n + 2);
    m.section(&exports);
    let indices: Vec<u32> = (1..=n).collect();
    let mut elements = ElementSection::new();
    elements.active(
        Some(0),
        &ConstExpr::i32_const(0),
        Elements::Functions(Cow::Borrowed(&indices)),
    );
    m.section(&elements);
    let mut code = CodeSection::new();
    for b in &t.blocks {
        code.function(&block(b));
    }
    // resolve(): bounded hash lookup, rather than a scan of thousands of blocks.
    // Locals: slot=0, probes=1, offset=2, packed entry=3.
    let mut resolve = Function::new([(4, ValType::I32)]);
    load(&mut resolve, IP);
    c(&mut resolve, 0x9e37_79b1);
    ins(&mut resolve, W::I32Mul);
    c(&mut resolve, 19);
    ins(&mut resolve, W::I32ShrU);
    set(&mut resolve, 0);
    ins(&mut resolve, W::Loop(BlockType::Empty));
    get(&mut resolve, 0);
    c(&mut resolve, 8);
    ins(&mut resolve, W::I32Mul);
    c(&mut resolve, BINDINGS);
    ins(&mut resolve, W::I32Add);
    set(&mut resolve, 2);
    get(&mut resolve, 2);
    ins(&mut resolve, W::I32Load(arg(4)));
    ins(&mut resolve, W::LocalTee(3));
    ins(&mut resolve, W::I32Eqz);
    ins(&mut resolve, W::If(BlockType::Empty));
    c(&mut resolve, u32::MAX);
    ins(&mut resolve, W::Return);
    ins(&mut resolve, W::End);
    get(&mut resolve, 2);
    ins(&mut resolve, W::I32Load(arg(0)));
    load(&mut resolve, IP);
    ins(&mut resolve, W::I32Eq);
    get(&mut resolve, 3);
    c(&mut resolve, 16);
    ins(&mut resolve, W::I32ShrU);
    load(&mut resolve, QUOTA);
    ins(&mut resolve, W::I32LeU);
    ins(&mut resolve, W::I32And);
    ins(&mut resolve, W::If(BlockType::Empty));
    get(&mut resolve, 3);
    c(&mut resolve, 0xffff);
    ins(&mut resolve, W::I32And);
    c(&mut resolve, 1);
    ins(&mut resolve, W::I32Sub);
    ins(&mut resolve, W::Return);
    ins(&mut resolve, W::End);
    get(&mut resolve, 1);
    c(&mut resolve, 1);
    ins(&mut resolve, W::I32Add);
    ins(&mut resolve, W::LocalTee(1));
    c(&mut resolve, DISPATCH_PROBES);
    ins(&mut resolve, W::I32GeU);
    ins(&mut resolve, W::If(BlockType::Empty));
    c(&mut resolve, u32::MAX);
    ins(&mut resolve, W::Return);
    ins(&mut resolve, W::End);
    get(&mut resolve, 0);
    c(&mut resolve, 1);
    ins(&mut resolve, W::I32Add);
    c(&mut resolve, DISPATCH_SLOTS as u32 - 1);
    ins(&mut resolve, W::I32And);
    set(&mut resolve, 0);
    ins(&mut resolve, W::Br(0));
    ins(&mut resolve, W::End);
    ins(&mut resolve, W::Unreachable);
    ins(&mut resolve, W::End);
    code.function(&resolve);
    // run(budget): outer loop crosses to the host; inner loop stays in this module.
    // Locals: remaining=1, completed=2, id=3, count=4.
    let mut run = Function::new([(4, ValType::I32)]);
    get(&mut run, 0);
    set(&mut run, 1);
    ins(&mut run, W::Loop(BlockType::Empty));
    get(&mut run, 2);
    get(&mut run, 1);
    ins(&mut run, W::Call(0));
    set(&mut run, 3);
    c(&mut run, 0);
    set(&mut run, 2);
    get(&mut run, 3);
    c(&mut run, (-2_i32) as u32);
    ins(&mut run, W::I32Eq);
    ins(&mut run, W::If(BlockType::Empty));
    get(&mut run, 0);
    get(&mut run, 1);
    ins(&mut run, W::I32Sub);
    ins(&mut run, W::Return);
    ins(&mut run, W::End);
    get(&mut run, 3);
    c(&mut run, u32::MAX);
    ins(&mut run, W::I32Eq);
    ins(&mut run, W::If(BlockType::Empty));
    get(&mut run, 1);
    c(&mut run, 1);
    ins(&mut run, W::I32Sub);
    set(&mut run, 1);
    ins(&mut run, W::Else);
    ins(&mut run, W::Loop(BlockType::Empty));
    get(&mut run, 3);
    ins(
        &mut run,
        W::CallIndirect {
            type_index: 1,
            table_index: 0,
        },
    );
    set(&mut run, 4);
    get(&mut run, 1);
    get(&mut run, 4);
    ins(&mut run, W::I32Sub);
    set(&mut run, 1);
    get(&mut run, 2);
    get(&mut run, 4);
    ins(&mut run, W::I32Add);
    set(&mut run, 2);
    load(&mut run, QUOTA);
    get(&mut run, 4);
    ins(&mut run, W::I32Sub);
    set(&mut run, 4);
    store_local(&mut run, QUOTA, 4);
    ins(&mut run, W::Call(n + 1));
    ins(&mut run, W::LocalTee(3));
    c(&mut run, 0);
    ins(&mut run, W::I32GeS);
    ins(&mut run, W::BrIf(0));
    ins(&mut run, W::End); // inner
    ins(&mut run, W::End); // if
    ins(&mut run, W::Br(0));
    ins(&mut run, W::End); // outer
    ins(&mut run, W::Unreachable);
    ins(&mut run, W::End);
    code.function(&run);
    m.section(&code);
    m.section(&CustomSection {
        name: Cow::Borrowed("doslet.profile"),
        data: Cow::Borrowed(&t.profile),
    });
    m.section(&CustomSection {
        name: Cow::Borrowed("doslet.abi"),
        data: Cow::Owned(crate::ABI_VERSION.to_le_bytes().to_vec()),
    });
    m.finish()
}
