//! IMUL boundary metadata must allow generated procedures to prove a live tier.
use super::*;

#[test]
fn imul_metadata_enables_tiered_procedures_with_exact_reference_state() {
    for (code, operation) in [
        (
            vec![0x0f, 0xaf, 0xc1, 0xc3],
            Operation::SignedMultiply {
                target: Register::Eax,
                left: Source::Register(Register::Eax),
                right: Source::Register(Register::Ecx),
            },
        ),
        (
            vec![0x0f, 0xaf, 0x03, 0xc3],
            Operation::SignedMultiplyMemory {
                target: Register::Eax,
                address: operand(Some(Register::Ebx), 0),
                source: Source::Register(Register::Eax),
            },
        ),
        (
            vec![0x66, 0x0f, 0xaf, 0x03, 0xc3],
            Operation::SignedMultiplyMemory {
                target: Register::Ax,
                address: operand(Some(Register::Ebx), 0),
                source: Source::Register(Register::Ax),
            },
        ),
        (
            vec![0x67, 0x0f, 0xaf, 0x06, DATA as u8, (DATA >> 8) as u8, 0xc3],
            Operation::SignedMultiplyMemory {
                target: Register::Eax,
                address: MemoryAddress {
                    address16: true,
                    ..operand(None, DATA)
                },
                source: Source::Register(Register::Eax),
            },
        ),
    ] {
        let length = code.len() as u32 - 1;
        let compiled = ProcedureProgram {
            functions: vec![ProcedureFunction {
                id: ProcedureId(9),
                entry: address(ENTRY),
                instructions: vec![ProcedureInstruction {
                    address: address(ENTRY),
                    operation,
                }],
                terminator: ProcedureReturn {
                    address: address(ENTRY + length),
                    width: Width::Dword,
                    pop_bytes: 0,
                },
            }],
        }
        .compile(&ProcedureCompileOptions::default())
        .unwrap();
        for (lhs, rhs) in [(3, 7), (0x8000_0000, 2), (0x1111_8000, u32::MAX)] {
            for tier in 0..4 {
                let (mut fast, mut reference) = (machine(), machine());
                for machine in [&mut fast, &mut reference] {
                    machine
                        .bus
                        .debug_ram_mut(ENTRY, code.len() as u32)
                        .unwrap()
                        .copy_from_slice(&code);
                    machine.cpu.registers.set32(Gpr32::Eax, lhs);
                    machine.cpu.registers.set32(Gpr32::Ecx, rhs);
                    machine.cpu.flags = 0xad7;
                    machine.bus.write_u32(DATA, rhs).unwrap();
                    tier_tests::set_tiers(machine, &code, tier, [Some(133), Some(8), Some(33)]);
                }
                let mut binding = binding();
                binding.maximum_instructions = 2;
                binding.code = vec![CodePart {
                    linear: ENTRY,
                    expected: code.clone(),
                }];
                let sources = source_instructions(&binding).expect("IMUL source metadata");
                assert_eq!(sources.len(), 2);
                assert_eq!(sources[0].length, length as u8);
                assert_eq!(sources[0].flow, doslet_x86::translation::NearFlow::Next);
                fast.install_wasm_procedures(compiled.wasm(), vec![binding])
                    .unwrap();
                fast.set_wasm_procedure_tier_accounting(true).unwrap();
                assert_eq!(fast.try_wasm_procedure(32).unwrap(), Some(2));
                reference_steps(&mut reference, 2);
                tier_tests::same_tiered_state(&mut fast, &mut reference);
                let mut counts = [0; 4];
                counts[usize::from(tier)] = 2;
                assert_eq!(fast.cpu.execution_counts(), counts);
                assert_eq!(fast.wasm_procedure_stats().unwrap().tier_refusals, 0);

                // Metadata availability must not weaken frozen source guards.
                fast.cpu.registers.ip = ENTRY;
                fast.cpu.registers.set32(Gpr32::Esp, STACK);
                fast.bus.debug_ram_mut(ENTRY, 1).unwrap()[0] ^= 1;
                let before = fast.cpu.snapshot();
                assert_eq!(fast.try_wasm_procedure(32).unwrap(), None);
                assert!(fast.cpu.matches(&before));
            }
        }
    }
}
