//! Synthetic register blocks and dense native loops; not a game measurement.
use doslet_runtime::{Session, SessionStatus};
use std::time::Instant;

include!("../../../tests/fixtures/protected_graphics.rs");

fn main() {
    benchmark(
        "straight blocks (16 INC + interpreted backedge)",
        &[
            0xFA, // CLI: the synthetic workload installs no protected IRQ handlers.
            0x40, 0x41, 0x42, 0x43, 0x40, 0x41, 0x42, 0x43, 0x40, 0x41, 0x42, 0x43, 0x40, 0x41,
            0x42, 0x43, 0xEB, 0xEE,
        ],
        false,
        false,
    );
    benchmark(
        "dense conditional loop (ADC + DEC + native JNZ)",
        &[
            0xFA, // CLI
            0xBB, 1, 0, 0, 0, // MOV EBX,1
            0xB9, 0xFF, 0xFF, 0xFF, 0x7F, // MOV ECX,7fffffffh
            0x11, 0xD8, // ADC EAX,EBX: consumes carry from the preceding iteration.
            0x49, // DEC ECX: updates the branch condition while preserving carry.
            0x75, 0xFB, // JNZ to ADC, kept inside the bounded native function.
        ],
        true,
        false,
    );
    benchmark(
        "RAM linked-list scan (three reads and internal exit)",
        &[
            0xFA, // CLI
            0xB8, 0x10, 0, 0, 0, 0x8E, 0xD8, 0x8E, 0xC0, // Flat DS and ES.
            0xB0, 2, 0xE6, 0x92, // Enable A20.
            0xBE, 0, 6, 1, 0, // MOV ESI,10600h
            0xBF, 0, 0, 0x12, 0, // MOV EDI,120000h
            0xB9, 0xC0, 0, 0, 0, 0xFC, 0xF3, 0xA5, // Copy the list to high RAM.
            0xBE, 0, 0, 0x12, 0, 0xBF, 0, 0, 0, 0, // Reset ESI and accumulator.
            0xBA, 0xFF, 0xFF, 0xFF, 0xFF, // MOV EDX,-1
            0x8B, 0x06, 0x39, 0xD0, 0x74, 0x0C, // MOV; CMP; JE output
            0x8B, 0x5E, 4, 0x01, 0xDF, // MOV EBX,[ESI+4]; ADD EDI,EBX
            0x8B, 0x76, 8, 0x85, 0xF6, 0x75, 0xEE, // MOV; TEST; JNZ start
            0x89, 0x3D, 0, 0, 4, 0, 0xF4, // Output and HLT if an exit occurs.
        ],
        true,
        true,
    );
    benchmark(
        "RAM linked-list update (native dword stores)",
        &[
            0xFA, 0xB8, 0x10, 0, 0, 0, 0x8E, 0xD8, 0x8E, 0xC0, 0xB0, 2, 0xE6, 0x92, 0xBE, 0, 6, 1,
            0, 0xBF, 0, 0, 0x12, 0, 0xB9, 0xC0, 0, 0, 0, 0xFC, 0xF3, 0xA5, 0xBE, 0, 0, 0x12, 0,
            0xBF, 0, 0, 0x14, 0, 0x8B, 0x06, // MOV EAX,[ESI]
            0x83, 0xC0, 1, // ADD EAX,1
            0x89, 0x07, // MOV [EDI],EAX
            0x8B, 0x76, 8, // MOV ESI,[ESI+8]
            0x85, 0xF6, // TEST ESI,ESI
            0x75, 0xF2, // JNZ loop
            0xF4,
        ],
        true,
        true,
    );
}

fn benchmark(name: &str, code: &[u8], expect_native_loop: bool, ram_scan: bool) {
    let mut program = protected_graphics_com();
    if ram_scan {
        program.resize(0x800, 0);
        program[0x210..0x218].copy_from_slice(&[0xFF, 0xFF, 0, 0, 0, 0x92, 0xCF, 0]);
        for index in 0..64_u32 {
            let offset = 0x500 + index as usize * 12;
            program[offset..offset + 4].copy_from_slice(&index.to_le_bytes());
            program[offset + 4..offset + 8].copy_from_slice(&index.wrapping_mul(17).to_le_bytes());
            let next = 0x120000 + ((index + 1) % 64) * 12;
            program[offset + 8..offset + 12].copy_from_slice(&next.to_le_bytes());
        }
    }
    program[0x300..0x300 + code.len()].copy_from_slice(code);
    let mut native = Session::new(&program).unwrap();
    let mut reference = Session::new(&program).unwrap();
    let mut batched = Session::new(&program).unwrap();
    reference.set_dynamic_core(false);
    assert!(
        native.dynamic_core_enabled(),
        "build with the dynamic-core feature"
    );
    // Batching-only keeps the dynamic scheduler but never prepares native code.
    for session in [&mut native, &mut reference, &mut batched] {
        session.start();
        assert_eq!(session.advance(10_000), 10_000);
    }
    let setup = Instant::now();
    for _ in 0..128 {
        if native.prepare_dynamic(8) == 0 {
            break;
        }
    }
    let compilation = setup.elapsed();
    let run = |session: &mut Session| {
        let start = Instant::now();
        for _ in 0..500 {
            assert_eq!(session.advance(10_000), 10_000);
        }
        start.elapsed()
    };
    // Warm both execution paths before three measured pairs with alternating order.
    run(&mut reference);
    run(&mut native);
    run(&mut batched);
    let mut interpreted = [std::time::Duration::ZERO; 3];
    let mut compiled = [std::time::Duration::ZERO; 3];
    let mut batching = [std::time::Duration::ZERO; 3];
    let measured_before = native.dynamic_stats();
    for round in 0..3 {
        let before = native.dynamic_stats();
        for position in 0..3 {
            match (round + position) % 3 {
                0 => interpreted[round] = run(&mut reference),
                1 => compiled[round] = run(&mut native),
                _ => batching[round] = run(&mut batched),
            }
        }
        assert_eq!(native.registers(), batched.registers());
        assert_eq!(native.eflags(), batched.eflags());
        assert_eq!(native.emulated_nanos(), batched.emulated_nanos());
        for range in [0..0xA0000, 0xB0000..0x1000000] {
            assert_eq!(
                native.ram_view(range.clone()),
                reference.ram_view(range.clone())
            );
            assert_eq!(native.ram_view(range.clone()), batched.ram_view(range));
        }
        assert_eq!(native.vram_view(), reference.vram_view());
        assert_eq!(native.vram_view(), batched.vram_view());
        assert_eq!(native.status(), SessionStatus::Running);
        assert_eq!(native.registers(), reference.registers());
        assert_eq!(native.eflags(), reference.eflags());
        assert_eq!(native.control_registers(), reference.control_registers());
        assert_eq!(native.emulated_nanos(), reference.emulated_nanos());
        assert_eq!(native.steps(), reference.steps());
        let after = native.dynamic_stats();
        let native_steps = after.executed_steps - before.executed_steps;
        let native_entries = after.executed_blocks - before.executed_blocks;
        assert!(native_entries > 0);
        println!(
            "{name}, round {}: cached interpreter={:?}, batching-only={:?}, native={:?}, native_steps={native_steps}, native_entries={native_entries}, steps/entry={:.2}",
            round + 1,
            interpreted[round],
            batching[round],
            compiled[round],
            native_steps as f64 / native_entries as f64,
        );
    }
    interpreted.sort_unstable();
    compiled.sort_unstable();
    batching.sort_unstable();
    assert_eq!(batched.dynamic_stats().compiled_blocks, 0);
    let measured_after = native.dynamic_stats();
    let native_steps = measured_after.executed_steps - measured_before.executed_steps;
    let native_entries = measured_after.executed_blocks - measured_before.executed_blocks;
    assert!(native_steps > 1_000_000);
    if expect_native_loop {
        assert!(
            native_steps > native_entries * 16,
            "dense loop must execute more than one 16-instruction block per native entry"
        );
    }
    println!(
        "{name}, synthetic 5M steps (median of three): cached interpreter={:?}, batching-only={:?}, native={:?}, compilation={compilation:?}",
        interpreted[1], batching[1], compiled[1]
    );
    println!(
        "Matching registers/flags/time; {:?}",
        native.dynamic_stats()
    );
}
