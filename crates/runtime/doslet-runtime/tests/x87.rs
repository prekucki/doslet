use doslet_runtime::{Session, SessionStatus};

#[test]
fn logarithm_and_exponential_execute_without_allocating_after_setup() {
    let mut program = vec![0; 0x108];
    let code = [
        0xD9, 0xED, // FLDLN2.
        0xD9, 6, 0, 2, // FLD real32 [0200h] = 8.
        0xD9, 0xF1, // FYL2X = ln(8).
        0xDD, 0x1E, 8, 2, // FSTP real64 [0208h].
        0xD9, 6, 4, 2, // FLD real32 [0204h] = 0.5.
        0xD9, 0xF0, // F2XM1 = sqrt(2)-1.
        0xDD, 0x1E, 0x10, 2, // FSTP real64 [0210h].
        0xB8, 0, 0x4C, 0xCD, 0x21,
    ];
    program[..code.len()].copy_from_slice(&code);
    program[0x100..0x104].copy_from_slice(&8.0_f32.to_bits().to_le_bytes());
    program[0x104..0x108].copy_from_slice(&0.5_f32.to_bits().to_le_bytes());
    let mut session = Session::new(&program).unwrap();
    session.start();
    let measured = allocation_counter::measure(|| {
        session.advance(100);
    });
    assert_eq!(
        session.status(),
        SessionStatus::Exited(0),
        "{:?}",
        session.fault()
    );
    assert_eq!(measured.count_total, 0);
    assert_eq!(measured.bytes_total, 0);
    let logarithm = f64::from_le_bytes(
        session
            .ram_view(0x10208..0x10210)
            .unwrap()
            .try_into()
            .unwrap(),
    );
    let exponential = f64::from_le_bytes(
        session
            .ram_view(0x10210..0x10218)
            .unwrap()
            .try_into()
            .unwrap(),
    );
    assert!((logarithm - core::f64::consts::LN_2 * 3.0).abs() < 1e-15);
    assert!((exponential - (core::f64::consts::SQRT_2 - 1.0)).abs() < 1e-15);
}

/// Host time per scheduling step of small real-mode x87 loops. Data lives on
/// its own 4-KiB page, so stores do not invalidate the decoded code page.
/// `cargo test -p doslet-runtime --release --locked --test x87 x87_loop -- --ignored --nocapture`
/// `X87_BENCH_STEPS` (default 20,000,000) sets the measured steps per loop.
#[test]
#[ignore = "host timing measurement"]
fn x87_loop_microbenchmark() {
    const LOOPS: [(&str, &[u8]); 9] = [
        ("fnop x4", &[0xD9, 0xD0, 0xD9, 0xD0, 0xD9, 0xD0, 0xD9, 0xD0]),
        ("nop x4", &[0x90, 0x90, 0x90, 0x90]),
        (
            "fld1/fld1/fxch/fxch/fstp st/fstp st",
            &[
                0xD9, 0xE8, 0xD9, 0xE8, 0xD9, 0xC9, 0xD9, 0xC9, 0xDD, 0xD8, 0xDD, 0xD8,
            ],
        ),
        (
            "fld m32/fstp st x2",
            &[
                0xD9, 0x06, 0x00, 0x10, 0xDD, 0xD8, 0xD9, 0x06, 0x04, 0x10, 0xDD, 0xD8,
            ],
        ),
        (
            "mov ax,[m]/mov [m],ax x2",
            &[
                0x8B, 0x06, 0x00, 0x10, 0x89, 0x06, 0x04, 0x10, 0x8B, 0x06, 0x00, 0x10, 0x89, 0x06,
                0x04, 0x10,
            ],
        ),
        (
            "integer mov/imul/add/mov",
            &[
                0x8B, 0x06, 0x00, 0x10, // MOV AX,[1000h].
                0xF7, 0x2E, 0x02, 0x10, // IMUL WORD [1002h].
                0x01, 0xD8, // ADD AX,BX.
                0x89, 0x06, 0x04, 0x10, // MOV [1004h],AX.
            ],
        ),
        (
            "fld/fmul/fadd/fstp",
            &[
                0xD9, 0x06, 0x00, 0x10, // FLD DWORD [1000h].
                0xD8, 0x0E, 0x04, 0x10, // FMUL DWORD [1004h].
                0xD8, 0x06, 0x08, 0x10, // FADD DWORD [1008h].
                0xD9, 0x1E, 0x0C, 0x10, // FSTP DWORD [100Ch].
            ],
        ),
        (
            "fld/fdiv/fsqrt/fstp",
            &[
                0xD9, 0x06, 0x00, 0x10, // FLD DWORD [1000h].
                0xD8, 0x36, 0x04, 0x10, // FDIV DWORD [1004h].
                0xD9, 0xFA, // FSQRT.
                0xD9, 0x1E, 0x0C, 0x10, // FSTP DWORD [100Ch].
            ],
        ),
        (
            "fld/fld/fxch/fistp/fstp",
            &[
                0xD9, 0x06, 0x00, 0x10, // FLD DWORD [1000h].
                0xD9, 0x06, 0x04, 0x10, // FLD DWORD [1004h].
                0xD9, 0xC9, // FXCH ST(1).
                0xDB, 0x1E, 0x14, 0x10, // FISTP DWORD [1014h].
                0xD9, 0x1E, 0x0C, 0x10, // FSTP DWORD [100Ch].
            ],
        ),
    ];
    let steps: u64 = std::env::var("X87_BENCH_STEPS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(20_000_000);
    for (control, (name, body), variant) in [0x0C7F_u16, 0x027F, 0x037F]
        .into_iter()
        .flat_map(|control| LOOPS.into_iter().map(move |body| (control, body)))
        .flat_map(|(control, body)| [0, 1, 2].map(|variant| (control, body, variant)))
    {
        {
            // FNINIT; FLDCW [1010h]; body; JMP body. Code starts at CS:0100h,
            // so offset 1000h is program byte F00h.
            let mut program = vec![0; 0xF18];
            let mut code = vec![0xDB, 0xE3, 0xD9, 0x2E, 0x10, 0x10];
            code.extend_from_slice(body);
            code.extend_from_slice(&[0xEB, (-(body.len() as i8) - 2) as u8]);
            program[..code.len()].copy_from_slice(&code);
            let words = name.starts_with("integer");
            for (offset, value) in [(0xF00, 1.5_f32), (0xF04, 1.000_000_1), (0xF08, 0.25)] {
                program[offset..offset + 4].copy_from_slice(&value.to_bits().to_le_bytes());
            }
            if words {
                program[0xF00..0xF02].copy_from_slice(&3_u16.to_le_bytes());
                program[0xF02..0xF04].copy_from_slice(&5_u16.to_le_bytes());
            }
            program[0xF10..0xF12].copy_from_slice(&control.to_le_bytes());
            let mut session = Session::new(&program).unwrap();
            // Each iteration repeats the same state; measure execution, not
            // the idle-loop proof that would otherwise skip it.
            session.set_idle_loops(false);
            session.set_x87_batching(variant != 0);
            session.set_x87_fast_paths(variant == 2);
            session.start();
            let run = |session: &mut Session, steps: u64| {
                let target = session.steps() + steps;
                while session.steps() < target {
                    session.advance((target - session.steps()).min(10_000) as u32);
                    assert_eq!(
                        session.status(),
                        SessionStatus::Running,
                        "{:?}",
                        session.fault()
                    );
                }
            };
            run(&mut session, 1_000_000);
            let started = std::time::Instant::now();
            run(&mut session, steps);
            let elapsed = started.elapsed();
            eprintln!(
                "CW {control:04X} {name} ({}): {:.1} ns/step",
                ["reference", "batched", "batched, fast paths"][variant],
                elapsed.as_nanos() as f64 / steps as f64
            );
        }
    }
}
