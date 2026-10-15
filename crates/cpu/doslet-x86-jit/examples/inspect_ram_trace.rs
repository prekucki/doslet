//! Inspect actual host code for the original linked-list benchmark, outside execution.
use doslet_x86_jit::{
    Compiler, DwordMemoryAddress, TraceInstruction as I, TraceOperation as O, TraceSource as S,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let prefix = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/doslet-ram-trace".into());
    let memory = |displacement| {
        S::Memory(DwordMemoryAddress {
            base: Some(6),
            index: None,
            scale: 0,
            displacement,
        })
    };
    // Same original eight-instruction list scan as bench_dynamic, not guest game data.
    let instructions = [
        I {
            offset: 0,
            length: 2,
            operation: O::Move {
                target: 0,
                source: memory(0),
            },
        },
        I {
            offset: 2,
            length: 2,
            operation: O::Alu {
                target: 0,
                source: S::Register(2),
                operation: 7,
                store: false,
            },
        },
        I {
            offset: 4,
            length: 2,
            operation: O::Branch {
                condition: Some(4),
                target: 18,
            },
        },
        I {
            offset: 6,
            length: 3,
            operation: O::Move {
                target: 3,
                source: memory(4),
            },
        },
        I {
            offset: 9,
            length: 2,
            operation: O::Alu {
                target: 7,
                source: S::Register(3),
                operation: 0,
                store: true,
            },
        },
        I {
            offset: 11,
            length: 3,
            operation: O::Move {
                target: 6,
                source: memory(8),
            },
        },
        I {
            offset: 14,
            length: 2,
            operation: O::Alu {
                target: 6,
                source: S::Register(6),
                operation: 4,
                store: false,
            },
        },
        I {
            offset: 16,
            length: 2,
            operation: O::Branch {
                condition: Some(5),
                target: 0,
            },
        },
    ];
    let mut compiler = Compiler::new()?;
    let (_, diagnostic) = compiler.compile_trace_diagnostic(&instructions)?;
    std::fs::write(format!("{prefix}.bin"), &diagnostic.code)?;
    std::fs::write(format!("{prefix}.clif"), &diagnostic.ir)?;
    println!(
        "{} guest instructions / 18 guest bytes -> {} native bytes",
        instructions.len(),
        diagnostic.code.len()
    );
    println!("Host code: {prefix}.bin; Cranelift IR: {prefix}.clif");
    Ok(())
}
