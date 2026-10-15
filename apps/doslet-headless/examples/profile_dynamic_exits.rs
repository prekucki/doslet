//! Count actual native exits in a real workload; never use this as a time benchmark.
//! Only addresses, reasons and counts are exported, never original guest code bytes.
#[path = "../../common/native_input.rs"]
mod native_input;

use doslet_runtime::{Session, SessionStatus};
use std::{error::Error, path::Path};

#[derive(Default, Debug, PartialEq, Eq)]
struct Output {
    midi: u64,
    pcm: u64,
    midi_bytes: u64,
    pcm_bytes: u64,
}

fn hash(value: &mut u64, byte: u8) {
    *value = (*value ^ u64::from(byte)).wrapping_mul(0x100000001b3);
}

fn drain(session: &mut Session, output: &mut Output) {
    let mut midi = [0; 1024];
    let mut pcm = [[0_i16; 2]; 128];
    loop {
        let count = session.drain_midi(&mut midi);
        if count == 0 {
            break;
        }
        for &byte in &midi[..count] {
            hash(&mut output.midi, byte);
            output.midi_bytes += 1;
        }
    }
    loop {
        let count = session.drain_pcm(&mut pcm);
        if count == 0 {
            break;
        }
        for frame in &pcm[..count] {
            for sample in frame {
                for byte in sample.to_le_bytes() {
                    hash(&mut output.pcm, byte);
                    output.pcm_bytes += 1;
                }
            }
        }
    }
}

fn compare(a: &Session, b: &Session, output_a: &Output, output_b: &Output) {
    assert_eq!(a.registers(), b.registers());
    assert_eq!(a.eflags(), b.eflags());
    assert_eq!(a.control_registers(), b.control_registers());
    assert_eq!(a.steps(), b.steps());
    assert_eq!(a.emulated_nanos(), b.emulated_nanos());
    assert_eq!(a.cpu_halted(), b.cpu_halted());
    assert_eq!(a.a20_enabled(), b.a20_enabled());
    assert_eq!(a.midi_stats(), b.midi_stats());
    assert_eq!(a.file_access_stats(), b.file_access_stats());
    assert_eq!(a.video_mode(), b.video_mode());
    assert_eq!(a.loaded_overlays(), b.loaded_overlays());
    assert_eq!(a.console(), b.console());
    assert_eq!(output_a, output_b);
    #[cfg(feature = "profiling")]
    assert_eq!(a.execution_stats(), b.execution_stats());
    assert_eq!(a.vram_view(), b.vram_view());
    for range in [0..0xa0000, 0xb0000..0x1000000] {
        assert_eq!(a.ram_view(range.clone()), b.ram_view(range));
    }
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
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().collect();
    let path = args
        .get(1)
        .ok_or("usage: profile_dynamic_exits EXE [STEPS]")?;
    let steps: u64 = args
        .get(2)
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(1_000_000_000);
    if steps == 0 || steps > 1_000_000_000 {
        return Err("STEPS must be in 1..=1000000000".into());
    }
    let mut native = native_input::load_game(Path::new(path))?;
    let mut reference = native_input::load_game(Path::new(path))?;
    native.set_dynamic_exit_profiling(true)?;
    assert!(native.dynamic_core_enabled(), "build with dynamic-core");
    reference.set_dynamic_core(false);
    native.start();
    reference.start();
    let mut native_output = Output::default();
    let mut reference_output = Output::default();
    println!("end_steps,cs,entry_ip,exit_ip,entry_linear,exit_linear,reason,count,native_steps");
    while native.steps() < steps {
        // Snapshots allocate only at explicit host diagnostic boundaries.
        let before = native.dynamic_exit_profile().to_vec();
        let end = (native.steps() + 100_000_000).min(steps);
        while native.steps() < end {
            let budget = (end - native.steps()).min(10_000) as u32;
            native.prepare_dynamic(2);
            assert_eq!(native.advance(budget), budget, "{:?}", native.fault());
            assert_eq!(reference.advance(budget), budget, "{:?}", reference.fault());
            assert_eq!(native.status(), SessionStatus::Running);
            assert_eq!(reference.status(), SessionStatus::Running);
            drain(&mut native, &mut native_output);
            drain(&mut reference, &mut reference_output);
        }
        compare(&reference, &native, &reference_output, &native_output);
        for (index, record) in native.dynamic_exit_profile().iter().enumerate() {
            let old_count = before.get(index).map_or(0, |r| r.count);
            let old_steps = before.get(index).map_or(0, |r| r.steps);
            let count = record.count - old_count;
            if count != 0 {
                println!(
                    "{},{:04X},{:08X},{:08X},{:08X},{:08X},{:?},{},{}",
                    native.steps(),
                    record.cs,
                    record.entry_ip,
                    record.exit_ip,
                    record.entry_linear,
                    record.exit_linear,
                    record.reason,
                    count,
                    record.steps - old_steps
                );
            }
        }
        assert_eq!(
            native.dynamic_exit_profile_dropped(),
            0,
            "profile capacity exhausted"
        );
        eprintln!(
            "Matched state/RAM/VRAM/scanout and MIDI/PCM through {} steps; {:?}",
            native.steps(),
            native.dynamic_stats()
        );
    }
    Ok(())
}
