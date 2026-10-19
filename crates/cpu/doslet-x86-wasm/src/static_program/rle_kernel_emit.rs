//! Atomic ordinary-RAM RLE over a fresh owner-proved finite source prefix.

use super::procedure_memory_emit::{COMPLETED, EXIT_STATUS, PC, finish, initialize};
use super::*;
use crate::static_program::{
    memory,
    procedure_memory::{COMPLETE, ProcedureCompileError, RleCall, SIDE_EXIT},
};

// Reuse the ordinary procedure's i32 memory scratch locals. The shared ALU
// emitter only uses LEFT/RIGHT/RESULT/CARRY/WIDE; these values survive it.
const SOURCE: u32 = WIDE + 1;
const DESTINATION: u32 = SOURCE + 1;
const COUNT_POINTER: u32 = SOURCE + 2;
const VALUE_POINTER: u32 = SOURCE + 3;
const STACK: u32 = SOURCE + 4;
const REMAINING: u32 = SOURCE + 5;
const VALUE: u32 = SOURCE + 6;
const CONSUMED: u32 = SOURCE + 7;
const OUTPUT: u32 = SOURCE + 8;
const LENGTH: u32 = SOURCE + 9;
const TOKEN: u32 = SOURCE + 10;
const LAST_BYTE: u32 = SOURCE + 11;

fn header(f: &mut Function, word: u32) {
    load(f, RleCall::OFFSET + word * 4, false);
}
fn store_pointer(f: &mut Function, pointer: u32, offset: u32, local: u32) {
    get(f, pointer);
    get(f, local);
    ins(
        f,
        W::I32Store(MemArg {
            align: 0,
            ..arg(offset, false)
        }),
    );
}
fn source_byte(f: &mut Function) {
    get(f, SOURCE);
    get(f, CONSUMED);
    ins(f, W::I32Add);
    ins(
        f,
        W::I32Load8U(MemArg {
            align: 0,
            ..arg(0, false)
        }),
    );
    get(f, CONSUMED);
    c(f, 1);
    ins(f, W::I32Add);
    set(f, CONSUMED);
}
fn increment(f: &mut Function, local: u32, amount: u32) {
    get(f, local);
    c(f, amount);
    ins(f, W::I32Add);
    set(f, local);
}
fn fill_run(f: &mut Function) {
    // Positive pending count or unsigned 0..63 decoded count, clipped to output.
    header(f, 8);
    get(f, OUTPUT);
    ins(f, W::I32Sub);
    set(f, LENGTH);
    get(f, REMAINING);
    get(f, LENGTH);
    ins(f, W::I32LtU);
    ins(f, W::If(BlockType::Empty));
    get(f, REMAINING);
    set(f, LENGTH);
    ins(f, W::End);
    get(f, LENGTH);
    ins(f, W::If(BlockType::Empty));
    get(f, DESTINATION);
    get(f, OUTPUT);
    ins(f, W::I32Add);
    get(f, VALUE);
    get(f, LENGTH);
    ins(f, W::MemoryFill(0));
    get(f, VALUE);
    c(f, 255);
    ins(f, W::I32And);
    set(f, LAST_BYTE);
    ins(f, W::End);
    get(f, OUTPUT);
    get(f, LENGTH);
    ins(f, W::I32Add);
    set(f, OUTPUT);
    get(f, REMAINING);
    get(f, LENGTH);
    ins(f, W::I32Sub);
    set(f, REMAINING);
}

pub(in crate::static_program) fn body(
    entry: Address,
    max_output: usize,
) -> Result<Function, ProcedureCompileError> {
    let mut f = initialize(false);
    c(&mut f, entry.offset);
    set(&mut f, PC);
    c(&mut f, SIDE_EXIT);
    set(&mut f, EXIT_STATUS);
    ins(&mut f, W::Block(BlockType::Empty));
    header(&mut f, 0);
    c(&mut f, RleCall::VERSION);
    ins(&mut f, W::I32Ne);
    ins(&mut f, W::BrIf(0));
    header(&mut f, 15);
    ins(&mut f, W::BrIf(0));
    for (word, local) in [
        (1, SOURCE),
        (2, DESTINATION),
        (3, COUNT_POINTER),
        (4, VALUE_POINTER),
        (5, STACK),
        (9, REMAINING),
        (10, VALUE),
    ] {
        header(&mut f, word);
        set(&mut f, local);
    }
    // Preserve all six original prologue dword writes, including dead locals.
    for (offset, register) in [
        (20, Register::Esi),
        (16, Register::Edi),
        (12, Register::Ebp),
        (8, Register::Eax),
        (4, Register::Ecx),
        (0, Register::Ebx),
    ] {
        store_pointer(&mut f, STACK, offset, GPR + register.index());
    }
    get(&mut f, REMAINING);
    c(&mut f, 0);
    ins(&mut f, W::I32GtS);
    ins(&mut f, W::If(BlockType::Empty));
    fill_run(&mut f);
    ins(&mut f, W::End);
    ins(&mut f, W::Block(BlockType::Empty));
    ins(&mut f, W::Loop(BlockType::Empty));
    get(&mut f, OUTPUT);
    header(&mut f, 8);
    ins(&mut f, W::I32GeU);
    ins(&mut f, W::BrIf(1));
    source_byte(&mut f);
    set(&mut f, TOKEN);
    get(&mut f, TOKEN);
    c(&mut f, 0xc0);
    ins(&mut f, W::I32And);
    c(&mut f, 0xc0);
    ins(&mut f, W::I32Eq);
    ins(&mut f, W::If(BlockType::Empty));
    get(&mut f, TOKEN);
    c(&mut f, 63);
    ins(&mut f, W::I32And);
    set(&mut f, REMAINING);
    source_byte(&mut f);
    set(&mut f, VALUE);
    fill_run(&mut f);
    ins(&mut f, W::Else);
    get(&mut f, DESTINATION);
    get(&mut f, OUTPUT);
    ins(&mut f, W::I32Add);
    get(&mut f, TOKEN);
    ins(
        &mut f,
        W::I32Store8(MemArg {
            align: 0,
            ..arg(0, false)
        }),
    );
    get(&mut f, TOKEN);
    set(&mut f, LAST_BYTE);
    increment(&mut f, OUTPUT, 1);
    ins(&mut f, W::End);
    ins(&mut f, W::Br(0));
    ins(&mut f, W::End);
    ins(&mut f, W::End);
    // Literal-only calls preserve even a negative initial count, and calls
    // completed by a pending run preserve all 32 bits of the initial value.
    header(&mut f, 13);
    ins(&mut f, W::If(BlockType::Empty));
    store_pointer(&mut f, COUNT_POINTER, 0, REMAINING);
    ins(&mut f, W::End);
    header(&mut f, 14);
    ins(&mut f, W::If(BlockType::Empty));
    store_pointer(&mut f, VALUE_POINTER, 0, VALUE);
    ins(&mut f, W::End);
    get(&mut f, CONSUMED);
    ins(&mut f, W::If(BlockType::Result(ValType::I32)));
    read_register(&mut f, Register::Ebx);
    c(&mut f, !255);
    ins(&mut f, W::I32And);
    get(&mut f, LAST_BYTE);
    ins(&mut f, W::I32Or);
    ins(&mut f, W::Else);
    get(&mut f, REMAINING);
    ins(&mut f, W::End);
    set(&mut f, GPR + Register::Ecx.index());
    get(&mut f, CONSUMED);
    set(&mut f, GPR + Register::Eax.index());
    header(&mut f, 12);
    set(&mut f, GPR + Register::Ebx.index());
    header(&mut f, 8);
    set(&mut f, GPR + Register::Edx.index());
    // The last flag writer is the original ADD ESP,12 at entry ESP-24.
    read_register(&mut f, Register::Esp);
    c(&mut f, 24);
    ins(&mut f, W::I32Sub);
    set(&mut f, GPR + Register::Esp.index());
    operation(
        &mut f,
        Operation::Alu {
            kind: Alu::Add,
            target: Register::Esp,
            source: Source::Immediate(12),
        },
    );
    increment(&mut f, GPR + Register::Esp.index(), 24);
    get(&mut f, FRAME);
    load(&mut f, memory::OFFSET + memory::RAM_WRITES, true);
    wide(&mut f, 6);
    ins(&mut f, W::I64Add);
    for word in [8, 13, 14] {
        header(&mut f, word);
        ins(&mut f, W::I64ExtendI32U);
        ins(&mut f, W::I64Add);
    }
    ins(
        &mut f,
        W::I64Store(arg(memory::OFFSET + memory::RAM_WRITES, true)),
    );
    header(&mut f, 7);
    set(&mut f, COMPLETED);
    header(&mut f, 6);
    set(&mut f, PC);
    c(&mut f, COMPLETE);
    set(&mut f, EXIT_STATUS);
    ins(&mut f, W::End);
    finish(&mut f, entry);
    if f.byte_len() > max_output {
        return Err(ProcedureCompileError::OutputTooLarge);
    }
    Ok(f)
}
