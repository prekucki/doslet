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
