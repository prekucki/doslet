extern crate std;

use super::*;
use alloc::vec::Vec;
use doslet_x87::{X87, pc24_binary};
use wasm_encoder::{
    CodeSection, ExportKind, ExportSection, FunctionSection, Module, TypeSection, ValType,
};
use wasmi::{Engine, Linker, Store, TypedFunc};

type Call = TypedFunc<(u64, u64, u32, u32), (u64, u32, u32)>;

struct Runner {
    store: Store<()>,
    functions: Vec<Call>,
}

const OPS: [u8; 6] = [0, 1, 4, 5, 6, 7];

impl Runner {
    fn new() -> Self {
        let mut module = Module::new();
        let mut types = TypeSection::new();
        types.ty().function(
            [ValType::I64, ValType::I64, ValType::I32, ValType::I32],
            [ValType::I64, ValType::I32, ValType::I32],
        );
        module.section(&types);
        let mut functions = FunctionSection::new();
        let mut exports = ExportSection::new();
        let mut code = CodeSection::new();
        for index in 0..7 {
            functions.function(0);
            exports.export(&alloc::format!("op{index}"), ExportKind::Func, index);
            let mut f = Function::new([
                (NumericLocals::F64_COUNT + 3, ValType::F64),
                (NumericLocals::I64_COUNT, ValType::I64),
                (NumericLocals::I32_COUNT + 1, ValType::I32),
            ]);
            let n = NumericLocals {
                f64_base: 4,
                i64_base: 19,
                i32_base: 24,
            };
            get(&mut f, 0);
            ins(&mut f, W::F64ReinterpretI64);
            set(&mut f, 17);
            get(&mut f, 1);
            ins(&mut f, W::F64ReinterpretI64);
            set(&mut f, 18);
            let refuse = |f: &mut Function| {
                get(f, 0);
                get(f, 3);
                c(f, 0);
                ins(f, W::Return);
            };
            if index < 6 {
                emit_binary(&mut f, n, OPS[index as usize], 17, 18, 2, 3, 16, refuse);
                get(&mut f, 16);
                ins(&mut f, W::I64ReinterpretF64);
            } else {
                emit_integer32(&mut f, n, 17, 2, 3, 30, refuse);
                get(&mut f, 30);
                ins(&mut f, W::I64ExtendI32U);
            }
            get(&mut f, 3);
            c(&mut f, 1);
            ins(&mut f, W::End);
            code.function(f.wasm());
        }
        module.section(&functions);
        module.section(&exports);
        module.section(&code);
        let engine = Engine::default();
        let module = wasmi::Module::new(&engine, module.finish()).unwrap();
        assert_eq!(module.imports().count(), 0);
        let mut store = Store::new(&engine, ());
        let instance = Linker::new(&engine)
            .instantiate_and_start(&mut store, &module)
            .unwrap();
        let functions = (0..7)
            .map(|index| {
                instance
                    .get_typed_func(&store, &alloc::format!("op{index}"))
                    .unwrap()
            })
            .collect();
        Self { store, functions }
    }

    fn binary(&mut self, a: f64, b: f64, op: usize, rc: u16, status: u16) {
        let control = 0x7f | (rc << 10);
        let actual = self.functions[op]
            .call(
                &mut self.store,
                (a.to_bits(), b.to_bits(), control.into(), status.into()),
            )
            .unwrap();
        let expected = match pc24_binary(a, b, OPS[op], control) {
            None => (a.to_bits(), u32::from(status), 0),
            Some(result) => (
                result.value.to_bits(),
                u32::from(status & !0x200)
                    | (u32::from(result.inexact) << 5)
                    | (u32::from(result.up) << 9),
                1,
            ),
        };
        assert_eq!(actual, expected, "a={a:?} b={b:?} op={} RC={rc}", OPS[op]);
    }

    fn integer(&mut self, value: f64, rc: u16) {
        let control = 0x7f | (rc << 10);
        let mut reference = X87::new();
        reference.load_control(control);
        reference.load_real(value.to_bits(), false).unwrap();
        reference.restore_status(reference.status() | 0x4700);
        let initial_status = reference.status();
        let bits = reference.store_integer(32).unwrap() as u32;
        let invalid = reference.status() & 1 != 0;
        let actual = self.functions[6]
            .call(
                &mut self.store,
                (value.to_bits(), 0, control.into(), initial_status.into()),
            )
            .unwrap();
        let expected = if invalid {
            (value.to_bits(), u32::from(initial_status), 0)
        } else {
            (u64::from(bits), u32::from(reference.status()), 1)
        };
        assert_eq!(actual, expected, "FIST32 value={value:?} RC={rc}");
    }
}

fn next(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

fn operand(seed: &mut u64) -> f64 {
    let random = next(seed);
    let exponent = 1023 - 448 + next(seed) % 897;
    let bits = (random & 0x800f_ffff_ffff_ffff) | (exponent << 52);
    f64::from_bits(if random & 31 == 0 {
        bits & (1 << 63)
    } else {
        bits
    })
}

#[test]
fn emitted_pc24_arithmetic_matches_random_and_residual_boundaries() {
    let mut runner = Runner::new();
    let mut seed = 0x1234_abcd_9841_4317;
    for index in 0..12_000 {
        let a = operand(&mut seed);
        let b = operand(&mut seed);
        runner.binary(a, b, index % 6, (index % 4) as u16, next(&mut seed) as u16);
    }
    for rc in 0..4 {
        for a in [
            0.0,
            -0.0,
            1.0,
            -1.0,
            1.0000000596046448,
            -1.0000000596046448,
            f64::from_bits(0x3fff_ffff_ffff_ffff),
            2.0,
            1.0 / 3.0,
            f64::MIN_POSITIVE,
            f64::INFINITY,
            f64::NAN,
            f64::from_bits(1),
        ] {
            for b in [
                0.0,
                -0.0,
                1.0,
                -1.0,
                2.0,
                3.0,
                1.0 / 16_777_216.0,
                f64::from_bits(0x3e6f_ffff_ffff_ffff),
                f64::from_bits(0x3e70_0000_0000_0001),
            ] {
                for op in 0..6 {
                    runner.binary(a, b, op, rc, 0x4567);
                }
            }
        }
        // Exact products and quotients whose RN53 lies on a PC24 boundary.
        for index in 0..512 {
            let a = f64::from_bits(0x3ff0_0000_0000_0000 + (index << 28));
            let b = f64::from_bits(0x3ff0_0000_0000_0000 + (index & 3));
            for op in 0..6 {
                runner.binary(a, b, op, rc, 0x0200);
            }
        }
    }
}

#[test]
fn emitted_integer32_matches_reference_and_refuses_before_status() {
    let mut runner = Runner::new();
    for rc in 0..4 {
        for value in [
            0.0,
            -0.0,
            0.5,
            -0.5,
            1.5,
            -1.5,
            2.5,
            -2.5,
            2_147_483_647.0,
            2_147_483_647.5,
            2_147_483_648.0,
            -2_147_483_648.0,
            -2_147_483_648.5,
            -2_147_483_649.0,
            1e100,
            -1e100,
            1e-100,
            -1e-100,
        ] {
            runner.integer(value, rc);
        }
        let mut seed = 0xfedc_ba98_7654_3210;
        for _ in 0..2000 {
            let value = (next(&mut seed) as i32 as f64) / 32.0;
            runner.integer(value, rc);
        }
    }
}
