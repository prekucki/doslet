use doslet_runtime::{
    Session,
    wasm_procedures::{Binding, CodePart, CodeSegment, KernelInterrupts},
};
use doslet_x86_wasm::static_program::{
    Address, Width,
    procedure_memory::{
        ProcedureCompileOptions, ProcedureFunction, ProcedureId, ProcedureProgram, ProcedureReturn,
    },
};

/// Install a real module to migrate the session's RAM and VRAM into its WASM owner.
/// The procedure is never called: presentation must work immediately after setup.
pub fn install(session: &mut Session) {
    const ENTRY: u32 = 0x8000;
    let entry = Address {
        space: 0,
        offset: ENTRY,
    };
    let module = ProcedureProgram {
        functions: vec![ProcedureFunction {
            id: ProcedureId(1),
            entry,
            instructions: vec![],
            terminator: ProcedureReturn {
                address: entry,
                width: Width::Dword,
                pop_bytes: 0,
            },
        }],
    }
    .compile(&ProcedureCompileOptions::default())
    .unwrap()
    .into_wasm();
    assert!(session.debug_write_memory(ENTRY, &[0xc3]));
    session
        .install_wasm_procedures(
            &module,
            vec![Binding {
                x87: None,
                execution_sites: None,
                function_id: 1,
                entry_ip: ENTRY,
                maximum_instructions: 1,
                kernel: None,
                rle_kernel: None,
                finite_computation: None,
                kernel_interrupts: KernelInterrupts::Defer,
                code_segment: CodeSegment::FLAT,
                code: vec![CodePart {
                    linear: ENTRY,
                    expected: vec![0xc3],
                }],
            }],
        )
        .unwrap();
    assert!(session.wasm_procedure_stats().is_some());
}
