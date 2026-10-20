//! Real interpreted CALL, registered WASM leaf, and architectural RET.
use super::*;
use doslet_x86::procedure_calls::{
    FrozenCode, ProcedureAbi, ProcedureCallOutcome, ProcedureContext, ProcedureEntry,
    ProcedureRegistry,
};

const CALLER: u32 = 0x800;
const LEAF: u32 = START + 32;

pub(super) fn run(artifact: &ProcedureArtifact) -> Result<(), String> {
    let mut executor = artifact.instantiate(ProcedureId(2))?;
    let (mut cpu, mut bus) = reference()?;
    let (mut ordinary, mut ordinary_bus) = reference()?;
    for ram in [&mut bus.0, &mut ordinary_bus.0] {
        ram[CALLER as usize] = 0xe8;
        ram[CALLER as usize + 1..CALLER as usize + 5]
            .copy_from_slice(&LEAF.wrapping_sub(CALLER + 5).to_le_bytes());
    }
    cpu.set_decode_cache(true);
    ordinary.set_decode_cache(true);
    let context = ProcedureContext::capture(&cpu).ok_or("unsupported procedure context")?;
    let entries = [ProcedureEntry {
        entry_ip: LEAF,
        context,
        function_id: 2,
        abi: ProcedureAbi::REGISTER_LEAF_NEAR32,
        instruction_cost: 3,
    }];
    let registry = ProcedureRegistry::new(&entries, FrozenCode::AssumeImmutable)
        .map_err(|error| format!("registry: {error:?}"))?;
    let mut random = 0x1357_2468_u32;
    for sample in 0..1024 {
        let mut input = [0; 8];
        for value in &mut input {
            random = random.wrapping_mul(1664525).wrapping_add(1013904223);
            *value = random;
        }
        input[4] = STACK;
        let flags = 0x202 | (random & 0x8d5);
        for state in [&mut cpu, &mut ordinary] {
            state.registers.ip = CALLER;
            state.flags = flags;
            for (register, value) in REGS.into_iter().zip(input) {
                state.registers.set32(register, value);
            }
        }
        // The interpreter performs the real guest CALL and its return-address
        // store in both runs. Only the proved leaf body is replaced.
        cpu.step(&mut bus).map_err(|error| error.to_string())?;
        for _ in 0..4 {
            ordinary
                .step(&mut ordinary_bus)
                .map_err(|error| error.to_string())?;
        }
        let result = cpu
            .try_call_procedure(&mut bus, &registry, &mut executor, 3)
            .map_err(|error| format!("procedure dispatch: {error:?}"))?;
        if result != (ProcedureCallOutcome::Executed { instructions: 3 }) {
            return Err(format!("procedure did not execute: {result:?}"));
        }
        if cpu.snapshot() != ordinary.snapshot()
            || bus.0 != ordinary_bus.0
            || bus.1 != ordinary_bus.1
        {
            return Err(format!("integrated CPU/RAM mismatch at sample {sample}"));
        }
        if cpu.registers.ip != CALLER + 5 || cpu.registers.get32(Gpr32::Esp) != STACK {
            return Err("integrated CALL/RET boundary mismatch".into());
        }
    }
    println!(
        "PASS INTEGRATED: 1024 interpreted CALL -> registered Wasmtime AOT leaf -> real RET boundaries; complete CPU snapshots, all 64KiB RAM, page generations and 1+3 instruction counts match ordinary x86 interpretation. No ESP normalization or omitted guest stack bytes."
    );
    Ok(())
}
