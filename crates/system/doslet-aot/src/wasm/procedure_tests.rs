use super::*;
use crate::procedures::{ProcedureLimits, ProcedurePlan};
use crate::wasm::procedures::{
    PreparedRegisterProcedures, ProcedureLoweringLimits, ProcedureLoweringRejection,
    RegisterProcedureAbi,
};
use doslet_x86_wasm::static_program::procedure::{ProcedureCompileOptions, ProcedureId};

#[test]
fn second_pass_executes_bound_original_register_leaf_with_explicit_abi() {
    // ADD EAX,EDX; XOR AH,CL; RET.
    let image = image(&[0x01, 0xd0, 0x30, 0xcc, 0xc3], DefaultBitness::Bits32);
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let leaves = ProcedurePlan::discover(&graph, ProcedureLimits::default()).unwrap();
    let prepared = PreparedRegisterProcedures::from_bound_leaves(
        &leaves,
        &binding,
        ProcedureLoweringLimits::default(),
    )
    .unwrap();
    assert!(prepared.diagnostics().is_empty());
    assert_eq!(prepared.entries().len(), 1);
    let entry = prepared.entries()[0];
    assert_eq!(entry.entry, at(0));
    assert_eq!(entry.linear_address, 0x10000);
    assert_eq!(entry.abi, RegisterProcedureAbi::RegisterLeafNear32);
    assert_eq!(entry.instruction_cost, 3);
    let compiled = prepared
        .program()
        .compile(&ProcedureCompileOptions::default())
        .unwrap();
    assert_eq!(compiled.expanded_work(ProcedureId(0)), Some(3));
    let engine = Engine::default();
    let module = Module::new(&engine, compiled.wasm()).unwrap();
    let mut store = Store::new(&engine, ());
    let instance = Linker::<()>::new(&engine)
        .instantiate_and_start(&mut store, &module)
        .unwrap();
    let function = instance.get_typed_func::<(u32,u32,u32,u32,u32,u32,u32,u32,u32),(u32,u32,u32,u32,u32,u32,u32,u32,u32,u64)>(&store,"procedure_0").unwrap();
    let actual = function
        .call(
            &mut store,
            (
                0x12345678, 0x56, 0x55, 0x33, 0x7000, 0x44, 0x66, 0x77, 0x202,
            ),
        )
        .unwrap();
    assert_eq!(actual.0, (0x12345678_u32 + 0x55) ^ (0x56 << 8));
    assert_eq!(
        (
            actual.1, actual.2, actual.3, actual.4, actual.5, actual.6, actual.7
        ),
        (0x56, 0x55, 0x33, 0x7000, 0x44, 0x66, 0x77)
    );
    assert_eq!(actual.9, 3);
}

#[test]
fn second_pass_rejects_guest_memory_stack_registers_and_other_return_abis() {
    for (code, reason) in [
        (
            &[0x8b, 0x00, 0xc3][..],
            ProcedureLoweringRejection::UnsupportedInstruction,
        ),
        (
            &[0x89, 0xe0, 0xc3][..],
            ProcedureLoweringRejection::StackRegister,
        ),
        (&[0xc2, 8, 0][..], ProcedureLoweringRejection::ReturnAbi),
        (&[0x66, 0xc3][..], ProcedureLoweringRejection::ReturnAbi),
    ] {
        let image = image(code, DefaultBitness::Bits32);
        let binding = binding(&image, 0x10000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let leaves = ProcedurePlan::discover(&graph, ProcedureLimits::default()).unwrap();
        assert_eq!(leaves.procedures().len(), 1);
        let prepared = PreparedRegisterProcedures::from_bound_leaves(
            &leaves,
            &binding,
            ProcedureLoweringLimits::default(),
        )
        .unwrap();
        assert!(prepared.entries().is_empty());
        assert!(prepared.program().functions.is_empty());
        assert_eq!(prepared.diagnostics()[0].reason, reason);
    }
}

#[test]
fn second_pass_keeps_input_and_owned_storage_bounded() {
    let image = image(&[0x40, 0xc3], DefaultBitness::Bits32);
    let binding = binding(&image, 0x10000);
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
    let leaves = ProcedurePlan::discover(&graph, ProcedureLimits::default()).unwrap();
    for limits in [
        ProcedureLoweringLimits {
            max_input_procedures: 0,
            ..ProcedureLoweringLimits::default()
        },
        ProcedureLoweringLimits {
            max_input_instructions: 1,
            ..ProcedureLoweringLimits::default()
        },
        ProcedureLoweringLimits {
            max_owned_bytes: 0,
            ..ProcedureLoweringLimits::default()
        },
    ] {
        assert!(PreparedRegisterProcedures::from_bound_leaves(&leaves, &binding, limits).is_err());
    }
}
