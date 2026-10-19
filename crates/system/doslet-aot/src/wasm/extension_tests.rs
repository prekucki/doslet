use super::*;
use doslet_x86_wasm::static_program::memory::{Descriptor, Effects, RamDescriptor, SegmentBounds};

#[test]
fn original_register_extensions_and_accumulator_forms_preserve_aliases_and_flags() {
    for (bits, bytes, target, value) in [
        (DefaultBitness::Bits32, vec![0x0f, 0xb6, 0xd0], 2, 0xf3),
        (
            DefaultBitness::Bits32,
            vec![0x0f, 0xbe, 0xd4],
            2,
            0xffff_ff80,
        ),
        (
            DefaultBitness::Bits32,
            vec![0x66, 0x0f, 0xb6, 0xc0],
            0,
            0x8765_00f3,
        ),
        (
            DefaultBitness::Bits32,
            vec![0x66, 0x0f, 0xbe, 0xc4],
            0,
            0x8765_ff80,
        ),
        (DefaultBitness::Bits32, vec![0x0f, 0xb7, 0xc0], 0, 0x80f3),
        (
            DefaultBitness::Bits32,
            vec![0x0f, 0xbf, 0xc0],
            0,
            0xffff_80f3,
        ),
        (
            DefaultBitness::Bits32,
            vec![0x66, 0x0f, 0xb7, 0xd0],
            2,
            0x1111_80f3,
        ),
        (
            DefaultBitness::Bits32,
            vec![0x66, 0x0f, 0xbf, 0xd0],
            2,
            0x1111_80f3,
        ),
        (DefaultBitness::Bits32, vec![0x98], 0, 0xffff_80f3),
        (DefaultBitness::Bits32, vec![0x66, 0x98], 0, 0x8765_fff3),
        (DefaultBitness::Bits16, vec![0x66, 0x98], 0, 0xffff_80f3),
        (DefaultBitness::Bits16, vec![0x98], 0, 0x8765_fff3),
        (
            DefaultBitness::Bits16,
            vec![0x0f, 0xb6, 0xd0],
            2,
            0x1111_00f3,
        ),
        (
            DefaultBitness::Bits16,
            vec![0x0f, 0xbe, 0xd4],
            2,
            0x1111_ff80,
        ),
    ] {
        let mut code = bytes.clone();
        code.push(0xf4);
        let image = image(&code, bits);
        let binding = binding(&image, 0x1000);
        let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
        let prepared =
            PreparedProgram::from_bound_cfg(&graph, &binding, LoweringLimits::default()).unwrap();
        assert_eq!(prepared.report().register_instructions, 1);
        let mut gpr = [0x8765_80f3, 0, 0x1111_2222, 0, 0, 0, 0, 0];
        let (frame, calls) = execute(
            &prepared,
            Frame {
                gpr,
                eflags: 0xad7,
                address: address(at(0)),
                remaining_budget: 1,
                ..Frame::default()
            },
        );
        gpr[target] = value;
        assert_eq!(frame.gpr, gpr, "{bits:?} {bytes:02x?}");
        assert_eq!(frame.eflags, 0xad7);
        assert_eq!(frame.charged_work, 1);
        assert_eq!(frame.address, address(at(bytes.len() as u32)));
        assert!(calls.is_empty());
    }
}

#[test]
fn original_memory_extensions_read_source_width_and_capture_aliasing_address() {
    for (bytes, target, value) in [
        (vec![0x0f, 0xb6, 0x05, 0, 5, 0, 0], 0, 0x80),
        (vec![0x0f, 0xbe, 0x05, 0, 5, 0, 0], 0, 0xffff_ff80),
        (vec![0x0f, 0xb7, 0x15, 0, 5, 0, 0], 2, 0xf180),
        (vec![0x0f, 0xbf, 0x15, 0, 5, 0, 0], 2, 0xffff_f180),
        (vec![0x66, 0x0f, 0xbe, 0x05, 0, 5, 0, 0], 0, 0xff80),
        (vec![0x66, 0x0f, 0xb7, 0x15, 0, 5, 0, 0], 2, 0x1111_f180),
        (vec![0x0f, 0xbe, 0x00], 0, 0xffff_ff80),
        (vec![0x66, 0x67, 0x0f, 0xbe, 0x16, 0, 5], 2, 0x1111_ff80),
    ] {
        let mut code = bytes.clone();
        code.push(0xf4);
        let image = image(&code, DefaultBitness::Bits32);
        let binding = binding(&image, 0x1000);
        let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default()).unwrap();
        let prepared = PreparedProgram::from_bound_cfg(
            &graph,
            &binding,
            LoweringLimits {
                direct_memory: true,
                ..LoweringLimits::default()
            },
        )
        .unwrap();
        assert_eq!(prepared.report().memory_extensions, 1);
        let mut gpr = [0x500, 0, 0x1111_2222, 0, 0, 0, 0, 0];
        let (frame, calls) = execute_setup(
            &prepared,
            Frame {
                gpr,
                eflags: 0xad7,
                address: address(at(0)),
                remaining_budget: 1,
                ..Frame::default()
            },
            CompileOptions {
                direct_memory: true,
                ..CompileOptions::default()
            },
            |memory| {
                memory[4096..4098].copy_from_slice(&[0x80, 0xf1]);
                Descriptor {
                    ram: RamDescriptor {
                        pointer: 4096,
                        linear_base: 0x500,
                        length: 2,
                        ..RamDescriptor::default()
                    },
                    windows: [[SegmentBounds {
                        valid: true,
                        base: 0,
                        first: 0,
                        last: u32::MAX,
                    }; 6]; 2],
                    ..Descriptor::default()
                }
                .write(&mut memory[72..])
                .unwrap();
            },
            |memory| {
                assert_eq!(&memory[4096..4098], &[0x80, 0xf1]);
                assert_eq!(Effects::read(&memory[72..]).unwrap().ram_writes, 0);
                assert_eq!(Effects::read(&memory[72..]).unwrap().device_reads, 0);
            },
        );
        gpr[target] = value;
        assert_eq!(frame.gpr, gpr, "{bytes:02x?}");
        assert_eq!(frame.eflags, 0xad7);
        assert_eq!(frame.charged_work, 1);
        assert_eq!(frame.address, address(at(bytes.len() as u32)));
        assert!(calls.is_empty());
    }
}
