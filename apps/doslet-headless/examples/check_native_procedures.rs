//! Actual native Session replay through the same backend used by the desktop.
#[allow(dead_code)]
#[path = "../../common/procedure_input.rs"]
mod procedure_input;
use doslet_runtime::{Session, SessionStatus};
use sha2::{Digest, Sha256};
use std::{error::Error, fs::File, io::Read, path::Path, time::Instant};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DiagnosticMode {
    #[default]
    Entries,
    Memory,
    PcBus,
}

fn configure_diagnostic(session: &mut Session, mode: DiagnosticMode) -> Result<()> {
    #[cfg(feature = "wasm-procedure-diagnostics")]
    {
        use doslet_runtime::wasm_procedures::DiagnosticMode as RuntimeMode;
        session.set_wasm_procedure_diagnostic_mode(match mode {
            DiagnosticMode::Entries => RuntimeMode::Normal,
            DiagnosticMode::Memory => RuntimeMode::SharedWithoutEntries,
            DiagnosticMode::PcBus => RuntimeMode::SharedPcBus,
        })?;
    }
    #[cfg(not(feature = "wasm-procedure-diagnostics"))]
    {
        let _ = session;
        if mode != DiagnosticMode::Entries {
            return Err("memory diagnosis requires the wasm-procedure-diagnostics feature".into());
        }
    }
    Ok(())
}
fn read(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err("native AOT input limit exceeded".into());
    }
    Ok(bytes)
}
fn configure(session: &mut Session, vga_waits: bool) -> Result<()> {
    session.set_dynamic_core(false);
    session.set_decode_cache(true);
    session.set_idle_loops(false);
    session.set_vga_wait_acceleration(vga_waits);
    session.set_synchronization_analysis(false);
    session.set_adaptive_cpu(false);
    session.set_adaptive_idle(false);
    session.set_idle_observation(false);
    session.debug_cpu_mut().set_execution_labels(None)?;
    session.start();
    Ok(())
}
fn timed_advance(session: &mut Session, budget: u32, seconds: &mut f64) -> u32 {
    let start = Instant::now();
    let completed = session.advance(budget);
    *seconds += start.elapsed().as_secs_f64();
    completed
}

/// Preserve module, source footprints and entry lookup, but force each source
/// proof to fail. This isolates installed-backend overhead from WASM calls.
fn reject_sources(path: &Path) -> Result<std::path::PathBuf> {
    let mut manifest: serde_json::Value = serde_json::from_slice(&read(path, 16 * 1024 * 1024)?)?;
    let functions = manifest["functions"]
        .as_array_mut()
        .ok_or("missing functions")?;
    for function in functions {
        let first = function["sources"][0]["bytes"][0]
            .as_u64()
            .and_then(|byte| u8::try_from(byte).ok())
            .ok_or("missing diagnostic source byte")?;
        function["sources"][0]["bytes"][0] = serde_json::json!(first ^ 0xff);
    }
    let destination = path.with_file_name("diagnostic-rejected-sources.json");
    if destination == path {
        return Err("diagnostic manifest must differ from the input".into());
    }
    std::fs::write(&destination, serde_json::to_vec_pretty(&manifest)?)?;
    Ok(destination)
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if !(3..=11).contains(&args.len()) {
        return Err(
            "usage: check_native_procedures CONFIG.toml INITIAL.state PROCEDURES.wasm [STEPS] [--bench] [--procedure-irqs] [--no-vga-waits | --reference-no-vga-waits] [--diagnose-dispatch [--diagnostic-mode entries|memory|pc-bus]]"
                .into(),
        );
    }
    let mut requested_steps = None;
    let mut bench = false;
    let mut procedure_irqs = false;
    let mut diagnose_dispatch = false;
    let mut diagnostic_mode = None;
    let mut vga_waits = None;
    let mut options = args[3..].iter();
    while let Some(argument) = options.next() {
        match argument.to_str() {
            Some("--bench") if !bench => bench = true,
            Some("--procedure-irqs") if !procedure_irqs => procedure_irqs = true,
            Some("--diagnose-dispatch") if !diagnose_dispatch => diagnose_dispatch = true,
            Some("--no-vga-waits") if vga_waits.is_none() => vga_waits = Some((false, false)),
            Some("--reference-no-vga-waits") if vga_waits.is_none() => {
                vga_waits = Some((false, true));
            }
            Some("--diagnostic-mode") if diagnostic_mode.is_none() => {
                diagnostic_mode = Some(match options.next().and_then(|value| value.to_str()) {
                    Some("entries") => DiagnosticMode::Entries,
                    Some("memory") => DiagnosticMode::Memory,
                    Some("pc-bus") => DiagnosticMode::PcBus,
                    _ => return Err("diagnostic mode must be entries, memory or pc-bus".into()),
                });
            }
            Some(value) if requested_steps.is_none() && !value.starts_with('-') => {
                requested_steps = Some(value.parse::<u64>().map_err(|_| "invalid steps")?);
            }
            _ => return Err("invalid or repeated native replay option".into()),
        }
    }
    let steps = requested_steps.unwrap_or(2_000_000);
    if steps == 0 || steps > 100_000_000 {
        return Err("steps must be 1..100000000".into());
    }
    let config = Path::new(&args[0]);
    if diagnose_dispatch && procedure_irqs {
        return Err("dispatch diagnosis requires the ordinary interpreter reference".into());
    }
    if diagnostic_mode.is_some() && !diagnose_dispatch {
        return Err("diagnostic mode requires --diagnose-dispatch".into());
    }
    let diagnostic_mode = diagnostic_mode.unwrap_or_default();
    // Match native desktop execution by default. The asymmetric mode checks
    // accelerated waiting against real IN/AND/Jcc execution from the same state.
    let (reference_vga_waits, translated_vga_waits) = vga_waits.unwrap_or((true, true));
    println!("VGA wait acceleration: native={reference_vga_waits} shared={translated_vga_waits}");
    let rejected_sources = if diagnose_dispatch {
        Some(reject_sources(Path::new(&args[2]))?)
    } else {
        None
    };
    let translated_manifest = rejected_sources.as_deref().unwrap_or(Path::new(&args[2]));
    if diagnose_dispatch {
        println!(
            "DIAGNOSTIC {diagnostic_mode:?}: installed WASM memory; translated calls disabled by source rejection or dispatch suppression"
        );
    }
    let procedure_input::Input { media, .. } = procedure_input::load(config)?;
    let initial = read(Path::new(&args[1]), 256 * 1024 * 1024)?;
    let mut native_samples = Vec::new();
    let mut aot_samples = Vec::new();
    for round in 0usize..if bench { 9 } else { 1 } {
        let mut reference = Session::load_state_with_media(&initial, &media)?;
        let mut translated = Session::load_state_with_media(&initial, &media)?;
        configure(&mut reference, reference_vga_waits)?;
        configure(&mut translated, translated_vga_waits)?;
        if procedure_irqs {
            procedure_input::install_procedures(&mut reference, Path::new(&args[2]), true)?;
        }
        procedure_input::install_procedures(&mut translated, translated_manifest, false)?;
        if diagnose_dispatch {
            configure_diagnostic(&mut translated, diagnostic_mode)?;
        }
        let mut pcm = [[0; 2]; 2048];
        let mut midi = [0; 2048];
        let mut reference_pcm = Sha256::new();
        let mut translated_pcm = Sha256::new();
        let mut reference_midi = Sha256::new();
        let mut translated_midi = Sha256::new();
        let mut completed = 0u64;
        let mut native_seconds = 0.0;
        let mut aot_seconds = 0.0;
        while completed < steps {
            let budget = (steps - completed).min(4096) as u32;
            let (ordinary, actual) = if round.is_multiple_of(2) {
                (
                    timed_advance(&mut reference, budget, &mut native_seconds),
                    timed_advance(&mut translated, budget, &mut aot_seconds),
                )
            } else {
                let actual = timed_advance(&mut translated, budget, &mut aot_seconds);
                let ordinary = timed_advance(&mut reference, budget, &mut native_seconds);
                (ordinary, actual)
            };
            let (mut ordinary, mut actual) = (u64::from(ordinary), u64::from(actual));
            // Whole computations may finish past a soft chunk budget. Bring
            // the lagging replay to the same logical boundary before comparing
            // state/audio; charge every catch-up advance to that engine.
            for _ in 0..8 {
                if ordinary == actual {
                    break;
                }
                if ordinary < actual {
                    ordinary += u64::from(timed_advance(
                        &mut reference,
                        (actual - ordinary).min(u64::from(u32::MAX)) as u32,
                        &mut native_seconds,
                    ));
                } else {
                    actual += u64::from(timed_advance(
                        &mut translated,
                        (ordinary - actual).min(u64::from(u32::MAX)) as u32,
                        &mut aot_seconds,
                    ));
                }
            }
            if ordinary != actual
                || ordinary == 0
                || reference.status() != SessionStatus::Running
                || translated.status() != SessionStatus::Running
            {
                return Err(format!("replay progress mismatch at {completed}: {ordinary}/{actual}, reference={:?}, AOT={:?}",reference.fault(),translated.fault()).into());
            }
            completed += ordinary;
            for (session, pcm_hash, midi_hash) in [
                (&mut reference, &mut reference_pcm, &mut reference_midi),
                (&mut translated, &mut translated_pcm, &mut translated_midi),
            ] {
                loop {
                    let count = session.drain_pcm(&mut pcm);
                    if count == 0 {
                        break;
                    }
                    for sample in &pcm[..count] {
                        for channel in sample {
                            pcm_hash.update(channel.to_le_bytes());
                        }
                    }
                }
                loop {
                    let count = session.drain_midi(&mut midi);
                    if count == 0 {
                        break;
                    }
                    midi_hash.update(&midi[..count]);
                }
            }
        }
        // The portable envelope records acceleration policy. Compare complete
        // guest state after normalizing only that policy, outside execution.
        if reference_vga_waits != translated_vga_waits {
            reference.set_vga_wait_acceleration(translated_vga_waits);
        }
        let reference_state = reference.save_state()?;
        let actual_state = translated.save_state()?;
        let pcm_equal = reference_pcm.finalize() == translated_pcm.finalize();
        let midi_equal = reference_midi.finalize() == translated_midi.finalize();
        let stats = translated
            .wasm_procedure_stats()
            .ok_or("AOT backend not installed")?;
        if procedure_irqs {
            let reference_stats = reference
                .wasm_procedure_stats()
                .ok_or("interpreter boundaries missing")?;
            println!("Interpreter procedure boundaries: {reference_stats:?}");
            if reference_stats.interpreted_calls == 0
                || reference_stats.interpreted_calls != stats.kernel_calls
                || reference_stats.interpreted_instructions != stats.kernel_instructions
            {
                return Err(
                    "reference and AOT did not execute matching marked procedure work".into(),
                );
            }
        }
        println!(
            "Native Session: trial={round} steps={completed} stats={stats:?} reference_ms={:.3} aot_ms={:.3}; preparation/compilation/state/audio hashing excluded",
            native_seconds * 1000.0,
            aot_seconds * 1000.0
        );
        #[cfg(feature = "profiling")]
        println!(
            "Decode cache: native={:?} shared={:?}",
            reference.decode_cache_stats(),
            translated.decode_cache_stats()
        );
        #[cfg(feature = "profiling")]
        println!(
            "Execution counters: native={:?} shared={:?}",
            reference.execution_stats(),
            translated.execution_stats()
        );
        if reference_state != actual_state || !pcm_equal || !midi_equal {
            let first = reference_state
                .iter()
                .zip(&actual_state)
                .position(|(a, b)| a != b);
            eprintln!(
                "State diagnostic: first differing wire byte={first:?}; CPU reference={:?}, AOT={:?}; flags={:#x}/{:#x}",
                reference.debug_cpu().registers,
                translated.debug_cpu().registers,
                reference.debug_cpu().flags,
                translated.debug_cpu().flags
            );
            let ram_bytes = usize::from(reference.hardware_config().memory_mib) * 1024 * 1024;
            let mut different_bytes = 0usize;
            for range in [0..0xa0000, 0xc0000..ram_bytes] {
                let a = reference
                    .ram_view(range.clone())
                    .ok_or("reference RAM diagnostic")?;
                let b = translated
                    .ram_view(range.clone())
                    .ok_or("AOT RAM diagnostic")?;
                for (offset, (&a, &b)) in a.iter().zip(b).enumerate() {
                    if a != b {
                        if different_bytes < 8 {
                            eprintln!(
                                "RAM differs at {:#x}: {a:#04x}/{b:#04x}",
                                range.start + offset
                            );
                        }
                        different_bytes += 1;
                    }
                }
            }
            eprintln!(
                "Different ordinary RAM bytes={different_bytes}; IRQ entries={}/{}; visible pixels equal={}",
                reference.debug_irq_entries(),
                translated.debug_irq_entries(),
                reference
                    .frame()
                    .zip(translated.frame())
                    .is_some_and(|(a, b)| a.pixels == b.pixels)
            );
            let output = Path::new(&args[2]).parent().unwrap_or(Path::new("."));
            std::fs::write(output.join("mismatch-reference.state"), &reference_state)?;
            std::fs::write(output.join("mismatch-aot.state"), &actual_state)?;
            return Err(format!(
                "full portable state/audio mismatch: state={} PCM={pcm_equal} MIDI={midi_equal}",
                reference_state == actual_state
            )
            .into());
        }
        if diagnose_dispatch {
            let sources_match_mode = match diagnostic_mode {
                DiagnosticMode::Entries => stats.source_refusals != 0,
                DiagnosticMode::Memory | DiagnosticMode::PcBus => stats.source_refusals == 0,
            };
            if stats.calls != 0 || stats.translated_instructions != 0 || !sources_match_mode {
                return Err("dispatch diagnosis did not reject all reached procedure calls".into());
            }
        } else if stats.calls == 0 || stats.translated_instructions == 0 {
            return Err("no eligible AOT procedure reached in this replay; execution coverage remains unverified".into());
        }
        println!(
            "PASS: actual native Session, shared RAM, full portable CPU/RAM/VRAM/device state, guest clocks, PCM and MIDI agree with cached native interpretation; no per-call RAM copies."
        );
        if !bench || round >= 2 {
            native_samples.push(native_seconds * 1000.0);
            aot_samples.push(aot_seconds * 1000.0);
        }
    }
    if bench {
        native_samples.sort_by(f64::total_cmp);
        aot_samples.sort_by(f64::total_cmp);
        println!(
            "BENCH native Session, procedure_irq_boundaries={procedure_irqs}, diagnostic_mode={:?}, vga_waits(native/shared)={reference_vga_waits}/{translated_vga_waits}, release workload={steps} logical instructions, 2 warmups + 7 measured replays, alternating order, every replay full-state/audio verified: native_median_ms={:.3} AOT_median_ms={:.3} speedup={:.3}x",
            diagnose_dispatch.then_some(diagnostic_mode),
            native_samples[3],
            aot_samples[3],
            native_samples[3] / aot_samples[3]
        );
    } else {
        println!("Single replay: correctness smoke only; use --bench for repeated timings.");
    }
    Ok(())
}
