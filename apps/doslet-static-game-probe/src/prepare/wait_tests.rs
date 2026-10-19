use super::*;

/// Minimal redistributable LE container; no game bytes or runtime state are
/// inferred from its load hints. CFG discovery uses original object offsets.
fn image(code: &[u8], code32: bool) -> SymbolicImage {
    let mut bytes = vec![0; 232 + code.len()];
    bytes[..2].copy_from_slice(b"LE");
    bytes[8..10].copy_from_slice(&2u16.to_le_bytes());
    bytes[10..12].copy_from_slice(&1u16.to_le_bytes());
    for (offset, value) in [
        (20, 1),
        (24, 1),
        (40, 4096),
        (44, code.len() as u32),
        (48, 8),
        (56, 28),
        (64, 196),
        (68, 1),
        (72, 220),
        (104, 224),
        (108, 232),
        (128, 232),
        (196, code.len() as u32),
        (200, 0x10000),
        (204, if code32 { 0x2007 } else { 7 }),
        (208, 1),
        (212, 1),
    ] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    bytes[222] = 1;
    bytes[232..].copy_from_slice(code);
    let le = LeImage::discover(&bytes, LeLimits::default()).unwrap();
    SymbolicImage::from_le(&le, ImageLimits::default()).unwrap()
}

#[test]
fn external_policy_tries_uncertain_aliases_but_excludes_local_progress_without_profiles() {
    let external = [
        0xa1, 0x6c, 4, 0, 0, 0x3b, 5, 0x80, 4, 0, 0, 0x74, 0xf3, 0xc3,
    ];
    let progress = [
        0xff, 5, 0x6c, 4, 0, 0, 0x83, 0x3d, 0x6c, 4, 0, 0, 0, 0x75, 0xf1, 0xc3,
    ];
    let uncertain = [
        0xa1, 0x6c, 4, 0, 0, 0x89, 0x1f, 0x39, 0xd0, 0x74, 0xf5, 0xc3,
    ];
    let code = [&external[..], &progress[..], &uncertain[..]].concat();
    let image = image(&code, true);
    let address = |offset| doslet_aot::ObjectAddress {
        object_number: 1,
        offset,
    };
    let graph = StaticCfg::analyze(
        &image,
        &[
            address(external.len() as u32),
            address((external.len() + progress.len()) as u32),
        ],
        CfgLimits::default(),
    )
    .unwrap();
    let plan = WaitCandidates::discover(&graph, WaitLimits::default()).unwrap();
    assert_eq!(plan.candidates().len(), 3);
    let selected = select_waits(Some(&plan), None, WaitPolicy::External).unwrap();
    assert_eq!(
        selected,
        [
            (address(0), 6),
            (address((external.len() + progress.len()) as u32), 8)
        ]
    );
    assert_eq!(
        select_waits(Some(&plan), None, WaitPolicy::Static)
            .unwrap()
            .len(),
        3
    );
    assert!(
        select_waits(Some(&plan), None, WaitPolicy::Cold)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn external_predicate_hint_reaches_runtime_and_wakes_on_actual_timer_irq() {
    // Compare two variables via a same-value temporary store, as in CalcTics.
    let program = [
        0xb8, 0x40, 0, 0x8e, 0xd8, // DS=40h
        0xa1, 0x6c, 0, 0xa3, 0x82, 0, // oldtime=ticks
        0xa1, 0x6c, 0, 0xa3, 0x80, 0, 0xa1, 0x80, 0, 0x3b, 6, 0x82, 0,
        // A rel32 JE in code16 keeps this exact guest loop recoverable by the
        // unbound CFG; near16 targets deliberately require a load-origin proof.
        0x66, 0x0f, 0x84, 0xec, 0xff, 0xff, 0xff, 0x42, 0xf4, 0xeb,
        0xfd, // DX marks wakeup; HLT loop
    ];
    let image = image(&program, false);
    let graph = StaticCfg::analyze(&image, &[], CfgLimits::default()).unwrap();
    let plan = WaitCandidates::discover(&graph, WaitLimits::default()).unwrap();
    let selected = select_waits(Some(&plan), None, WaitPolicy::External).unwrap();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].0.offset, 11);
    assert_eq!(selected[0].1, 10);
    let mut actual = Session::new(&program).unwrap();
    actual.set_dynamic_core(false);
    actual.set_decode_cache(false);
    actual.set_idle_loops(false);
    actual.set_vga_wait_acceleration(false);
    actual.set_synchronization_analysis(false);
    actual.set_adaptive_idle(false);
    actual.set_idle_observation(false);
    actual.set_cpu_mhz(1).unwrap();
    actual.start();
    assert_eq!(actual.advance(9), 9);
    let mut reference = Session::load_state(&actual.save_state().unwrap()).unwrap();
    for _ in 0..3 {
        let steps = actual.debug_advance_wait_candidate(65_536, selected[0].1);
        assert_eq!(reference.advance(65_536), steps);
        assert_eq!(
            actual.save_state().unwrap(),
            reference.save_state().unwrap()
        );
    }
    assert_ne!(actual.registers().get(doslet_x86::Gpr16::Dx), 0);
    assert!(actual.idle_loop_stats().skipped_steps > 0);
    assert!(actual.fault().is_none());
}
