use crate::native_input;
use doslet_runtime::{Session, SessionStatus};

fn compare_buffers(a: &Session, b: &Session) {
    assert_eq!(a.vram_view(), b.vram_view());
    match (a.frame(), b.frame()) {
        (Some(a), Some(b)) => {
            assert_eq!(
                (a.width, a.height, a.revision),
                (b.width, b.height, b.revision)
            );
            assert_eq!(a.pixels, b.pixels);
            assert_eq!(a.palette, b.palette);
        }
        (None, None) => {}
        _ => panic!("scanout differs"),
    }
    // The aperture is device memory; compare ordinary RAM on both sides.
    assert_eq!(a.ram_view(0..0xA0000), b.ram_view(0..0xA0000));
    let end = if a.a20_enabled() {
        16 * 1024 * 1024
    } else {
        0x100000
    };
    assert_eq!(a.ram_view(0xB0000..end), b.ram_view(0xB0000..end));
}

#[test]
#[ignore = "requires the local PRINCE installation; compares 300 million guest steps"]
fn local_prince_decode_cache_matches_reference_during_execution() {
    compare_local_prince(false);
}

#[test]
#[cfg(all(
    feature = "dynamic-core",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
#[ignore = "requires the local PRINCE installation; compares 300 million guest steps"]
fn local_prince_dynamic_core_matches_reference_during_execution() {
    compare_local_prince(true);
}

fn compare_local_prince(dynamic: bool) {
    let path = std::env::var_os("DOSLET_PRINCE_EXE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../PRINCE/PRINCE.EXE")
        });
    let mut cached = native_input::load_game(&path).unwrap();
    let mut reference = native_input::load_game(&path).unwrap();
    cached.set_dynamic_core(dynamic);
    reference.set_decode_cache(false);
    cached.start();
    reference.start();
    let mut midi_a = [0; 1024];
    let mut midi_b = [0; 1024];
    let mut pcm_a = [[0; 2]; 4096];
    let mut pcm_b = [[0; 2]; 4096];
    let mut remaining = 300_000_000_u32;
    let mut batches = 0;
    while remaining != 0 {
        let budget = [1, 7, 31, 257, 1009, 4096, 10000][batches % 7].min(remaining);
        if dynamic {
            cached.prepare_dynamic(2);
        }
        let done = cached.advance(budget);
        assert_eq!(done, reference.advance(budget));
        assert_eq!(
            done,
            budget,
            "{:?} / {:?}",
            cached.fault(),
            reference.fault()
        );
        remaining -= done;
        batches += 1;
        assert_eq!(cached.status(), SessionStatus::Running);
        assert_eq!(cached.registers(), reference.registers(), "batch {batches}");
        assert_eq!(
            cached.eflags(),
            reference.eflags(),
            "batch {batches}, steps {}, registers {:?}, native {:?}",
            cached.steps(),
            cached.registers(),
            cached.dynamic_stats(),
        );
        assert_eq!(cached.control_registers(), reference.control_registers());
        assert_eq!(cached.emulated_nanos(), reference.emulated_nanos());
        assert_eq!(cached.a20_enabled(), reference.a20_enabled());
        assert_eq!(cached.cpu_halted(), reference.cpu_halted());
        assert_eq!(cached.midi_stats(), reference.midi_stats());
        assert_eq!(cached.video_mode(), reference.video_mode());
        assert_eq!(cached.file_access_stats(), reference.file_access_stats());
        let count = cached.drain_midi(&mut midi_a);
        assert_eq!(count, reference.drain_midi(&mut midi_b));
        assert_eq!(&midi_a[..count], &midi_b[..count]);
        let count = cached.drain_pcm(&mut pcm_a);
        assert_eq!(count, reference.drain_pcm(&mut pcm_b));
        assert_eq!(&pcm_a[..count], &pcm_b[..count]);
        #[cfg(feature = "profiling")]
        {
            let a = cached.execution_stats();
            let b = reference.execution_stats();
            // VGA recognition may combine different numbers of equivalent
            // polling iterations at single-step versus slice boundaries.
            // Dispatched port reads and acceleration counters describe that
            // implementation work, not architectural guest progress.
            assert_eq!(
                (a.cpu_steps, a.idle_steps, a.irq_entries),
                (b.cpu_steps, b.idle_steps, b.irq_entries),
            );
        }
        if batches % 1024 == 0 {
            compare_buffers(&cached, &reference);
        }
    }
    assert!(cached.a20_enabled());
    compare_buffers(&cached, &reference);
    assert_eq!(cached.console(), reference.console());
    assert_eq!(cached.loaded_overlays(), reference.loaded_overlays());
    #[cfg(feature = "profiling")]
    {
        let stats = cached.decode_cache_stats();
        if !dynamic {
            assert!(stats.hits + stats.batched_steps > 100_000_000);
        }
        println!("{stats:?}");
    }
    if dynamic {
        assert!(cached.dynamic_stats().executed_steps > 0);
        println!(
            "Dynamic core: {:?}; compiler status: {:?}",
            cached.dynamic_stats(),
            cached.dynamic_error()
        );
    }
    println!(
        "Compared {batches} bounded batches, full final RAM/VRAM, VGA/palette, MIDI and PCM through 300000000 steps"
    );
}
