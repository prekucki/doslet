//! Warm native-to-WASM call cost with the production shared-memory owner.
//! Compilation, setup and RAM allocation are excluded; this is not game FPS.
use doslet_wasm_aot::procedure_memory::{Exit, SharedProcedureMemory};
use doslet_x86_wasm::static_program::{
    Address, Alu, Frame, Operation, Register, Source, Width,
    memory::{SegmentBounds, StackAddress},
    procedure_memory::{
        ProcedureCompileOptions, ProcedureFunction, ProcedureId, ProcedureInstruction,
        ProcedureProgram, ProcedureReturn,
    },
};
use std::{hint::black_box, time::Instant};

const CALLS: u32 = 200_000;
const STACK: usize = 0x4000;
const ENTRY: u32 = 0x8000;
const RETURN: u32 = 0x9000;

fn address(offset: u32) -> Address {
    Address { space: 0, offset }
}

fn main() -> Result<(), String> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.iter().any(|arg| arg != "--indexed") || args.len() > 1 {
        return Err("usage: bench-shared-call [--indexed]".into());
    }
    let indexed = !args.is_empty();
    for operations in [1, 32] {
        let program = ProcedureProgram {
            functions: vec![ProcedureFunction {
                id: ProcedureId(9),
                entry: address(ENTRY),
                instructions: (0..operations)
                    .map(|i| ProcedureInstruction {
                        address: address(ENTRY + i * 3),
                        operation: Operation::Alu {
                            kind: Alu::Add,
                            target: Register::Eax,
                            source: Source::Immediate(1),
                        },
                    })
                    .collect(),
                terminator: ProcedureReturn {
                    address: address(ENTRY + operations * 3),
                    width: Width::Dword,
                    pop_bytes: 0,
                },
            }],
        }
        .compile(&ProcedureCompileOptions::default())
        .map_err(|e| format!("compile: {e:?}"))?;
        let mut owner = SharedProcedureMemory::new(&program.into_wasm(), &[9], 4 * 1024 * 1024)?;
        owner.ram_mut()[STACK..STACK + 4].copy_from_slice(&RETURN.to_le_bytes());
        owner.protect_source(ENTRY, operations * 3 + 1)?;
        let descriptor = owner.descriptor(
            [[SegmentBounds {
                valid: true,
                base: 0,
                first: 0,
                last: u32::MAX,
            }; 6]; 2],
            StackAddress::Bits32,
        );
        let mut times = Vec::with_capacity(7);
        let index = owner.function_index(9).expect("prepared export");
        for sample in 0..9 {
            let started = Instant::now();
            let mut checksum = 0u64;
            for i in 0..CALLS {
                let mut frame = Frame {
                    address: address(ENTRY),
                    ..Frame::default()
                };
                frame.gpr[Register::Eax as usize] = i;
                frame.gpr[Register::Esp as usize] = STACK as u32;
                let (exit, effects) = if indexed {
                    owner.execute_index(black_box(index), &mut frame, black_box(&descriptor))?
                } else {
                    owner.execute(black_box(9), &mut frame, black_box(&descriptor))?
                };
                assert_eq!(exit, Exit::Complete);
                assert_eq!(frame.address.offset, RETURN);
                assert_eq!(frame.gpr[Register::Esp as usize], STACK as u32 + 4);
                assert_eq!(frame.gpr[Register::Eax as usize], i + operations);
                assert_eq!(frame.exact_total(), Some(u64::from(operations + 1)));
                assert_eq!(effects.ram_writes, 0);
                checksum += u64::from(black_box(frame.gpr[Register::Eax as usize]));
            }
            black_box(checksum);
            let elapsed = started.elapsed().as_secs_f64();
            if sample >= 2 {
                times.push(elapsed);
            }
        }
        times.sort_unstable_by(f64::total_cmp);
        println!(
            "shared CALL: indexed={indexed} instructions={} calls={} median_ns={:.3} min_ns={:.3} max_ns={:.3}; 2 warmups + 7 measured, result checked every call",
            operations + 1,
            CALLS,
            times[3] * 1e9 / f64::from(CALLS),
            times[0] * 1e9 / f64::from(CALLS),
            times[6] * 1e9 / f64::from(CALLS),
        );
    }
    Ok(())
}
