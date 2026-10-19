//! Bounded read-only dynamic CALL/RET attribution; instruction counts, not time.
//! Run with --features wasm-static-test. No generated procedure is executed.
//! Opcode recognition is diagnostic, not a full decoder or ABI proof. Code16
//! transitions and unmatched initial/nonlocal returns limit global attribution.
#[cfg(feature = "wasm-static-test")]
#[allow(dead_code)]
#[path = "../../common/procedure_input.rs"]
mod procedure_input;
#[cfg(feature = "wasm-static-test")]
#[allow(dead_code)]
#[path = "../../common/wasm_game.rs"]
mod wasm_game;

#[cfg(not(feature = "wasm-static-test"))]
fn main() {
    eprintln!("inspect_heavy_procedures requires --features wasm-static-test");
}

#[cfg(feature = "wasm-static-test")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use doslet_aot::{
        ImageLimits, ObjectAddress, SymbolicImage,
        binding::{BindingLimits, DosFlatBindingPlan},
        cfg::{CfgLimits, StaticCfg},
        wasm::memory_procedures::{MemoryProcedureLimits, PreparedMemoryProcedures},
    };
    use doslet_executable::{LeImage, LeLimits};
    use doslet_runtime::{Session, SessionStatus};
    use doslet_static_game_probe::prepare::bindings_from_json;
    use doslet_x86::{Access, Gpr32, SegmentRegister};
    use std::{cmp::Reverse, collections::HashMap, path::Path};

    #[derive(Default)]
    struct Stats {
        calls: u64,
        returned: u64,
        own: u64,
        inclusive: u64,
        io: u64,
        callers: HashMap<u32, u64>,
    }
    struct Frame {
        entry: u32,
        return_pc: u32,
        return_cs: u16,
    }
    struct External {
        return_pc: u32,
        return_cs: u16,
        irq: bool,
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err("usage: inspect_heavy_procedures CONFIG STATE BINDINGS STEPS".into());
    }
    let steps: u64 = args[3].to_str().ok_or("steps UTF-8")?.parse()?;
    if !(1..=20_000_000).contains(&steps) {
        return Err("steps must be 1..=20,000,000".into());
    }
    let input = procedure_input::load(Path::new(&args[0]))?;
    let mut session = Session::load_state_with_media(&std::fs::read(&args[1])?, &input.media)?;
    let declarations = bindings_from_json(&input.exe, &std::fs::read(&args[2])?)?;
    let le = LeImage::discover(&input.exe, LeLimits::default())?;
    let image = SymbolicImage::from_le(&le, ImageLimits::default())?;
    let binding = DosFlatBindingPlan::new(&image, &declarations, BindingLimits::default())?;
    let graph = StaticCfg::analyze_bound(&binding, &[], CfgLimits::default())?;
    let prepared = PreparedMemoryProcedures::from_bound_graph(
        &graph,
        &binding,
        MemoryProcedureLimits {
            max_work_per_call: Some(4096),
            ..MemoryProcedureLimits::default()
        },
    )?;
    let address = |linear: u32| {
        image.objects().iter().find_map(|object| {
            let base = binding.object_binding(object.object_number())?.linear_base;
            let offset = linear.checked_sub(base)?;
            (offset < object.metadata().virtual_size).then_some(ObjectAddress {
                object_number: object.object_number(),
                offset,
            })
        })
    };
    let location = |linear: u32| {
        address(linear).map_or_else(
            || "unmapped".to_owned(),
            |a| format!("object{}+{:#x}", a.object_number, a.offset),
        )
    };
    wasm_game::setup(&mut session);
    session.set_adaptive_cpu(false);
    session.debug_cpu_mut().set_execution_labels(None)?;
    let mut frames = Vec::<Frame>::new();
    let mut external = Vec::<External>::new();
    let mut functions = HashMap::<u32, Stats>::new();
    let mut histogram = HashMap::<(u32, u32, bool), (u64, [u8; 15], u8)>::new();
    let mut cpu = 0u64;
    let mut unknown = 0u64;
    let mut irq = 0u64;
    let mut services = 0u64;
    let mut mismatch = 0u64;
    let mut unknown_calls = 0u64;
    let initial = session.steps();
    let initial_ns = session.emulated_nanos();
    let mut pcm = [[0; 2]; 2048];
    let mut midi = [0; 2048];
    for iteration in 0..steps {
        let before_pc = session.debug_pc();
        let before_cs = session.registers().cs;
        let before_irq = session.debug_irq_entries();
        let observed = session.debug_observed_step();
        let after_pc = session.debug_pc();
        let after_cs = session.registers().cs;
        if session.debug_irq_entries() != before_irq {
            external.push(External {
                return_pc: before_pc,
                return_cs: before_cs,
                irq: true,
            });
        }
        if let Some(o) = observed {
            cpu += 1;
            // Service interception can restore the interrupted/calling context
            // before the actual observed instruction is selected.
            while external
                .last()
                .is_some_and(|e| e.return_pc == o.linear && e.return_cs == before_cs)
            {
                external.pop();
            }
            let mut prefix = 0usize;
            let mut operand32 = o.code32;
            while prefix < usize::from(o.available)
                && matches!(
                    o.bytes[prefix],
                    0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x66 | 0x67 | 0xf0 | 0xf2 | 0xf3
                )
            {
                if o.bytes[prefix] == 0x66 {
                    operand32 = !o.code32;
                }
                prefix += 1;
            }
            let op = o.bytes.get(prefix).copied().unwrap_or(0);
            if let Some(e) = external.last() {
                if e.irq {
                    irq += 1;
                } else {
                    services += 1;
                }
            } else {
                if let Some(current) = frames.last() {
                    functions.get_mut(&current.entry).unwrap().own += 1;
                    let row = histogram
                        .entry((current.entry, o.linear, o.code32))
                        .or_insert((0, o.bytes, o.available));
                    row.0 += 1;
                } else {
                    unknown += 1;
                }
                for frame in &frames {
                    functions.get_mut(&frame.entry).unwrap().inclusive += 1;
                }
                if matches!(op, 0xe4..=0xe7 | 0xec..=0xef)
                    && let Some(frame) = frames.last()
                {
                    functions.get_mut(&frame.entry).unwrap().io += 1;
                }
                let call = matches!(op, 0xe8 | 0x9a)
                    || (op == 0xff
                        && o.bytes
                            .get(prefix + 1)
                            .is_some_and(|m| matches!((m >> 3) & 7, 2 | 3)));
                if call {
                    // Read only the small architectural return slot pushed by
                    // the call, supporting indirect calls without guessing len.
                    let registers = session.registers();
                    let width = if operand32 { 4 } else { 2 };
                    let sp = registers.get32(Gpr32::Esp);
                    let return_ip = registers
                        .address(SegmentRegister::Ss, sp, width, Access::Read)
                        .ok()
                        .and_then(|at| session.ram_view(at as usize..at as usize + width as usize))
                        .map(|b| {
                            if width == 4 {
                                u32::from_le_bytes(b.try_into().unwrap())
                            } else {
                                u32::from(u16::from_le_bytes(b.try_into().unwrap()))
                            }
                        });
                    if let Some(return_ip) = return_ip {
                        let return_pc = if before_cs == after_cs {
                            after_pc.wrapping_sub(registers.ip).wrapping_add(return_ip)
                        } else {
                            before_pc.wrapping_sub(o.ip).wrapping_add(return_ip)
                        };
                        let stats = functions.entry(after_pc).or_default();
                        stats.calls += 1;
                        *stats.callers.entry(o.linear).or_default() += 1;
                        frames.push(Frame {
                            entry: after_pc,
                            return_pc,
                            return_cs: before_cs,
                        });
                        if frames.len() > 128 {
                            return Err("call stack exceeded 128".into());
                        }
                    } else {
                        unknown_calls += 1;
                        frames.clear();
                    }
                } else if matches!(op, 0xc2 | 0xc3 | 0xca | 0xcb)
                    && let Some(frame) = frames.pop()
                {
                    if frame.return_pc == after_pc && frame.return_cs == after_cs {
                        functions.get_mut(&frame.entry).unwrap().returned += 1;
                    } else {
                        mismatch += 1;
                        frames.clear();
                    }
                }
            }
            if matches!(op, 0xcc..=0xce) {
                let length = prefix as u32 + if op == 0xcd { 2 } else { 1 };
                external.push(External {
                    return_pc: o.linear.wrapping_add(length),
                    return_cs: before_cs,
                    irq: false,
                });
            }
            while external
                .last()
                .is_some_and(|e| e.return_pc == after_pc && e.return_cs == after_cs)
            {
                external.pop();
            }
        }
        if iteration.is_multiple_of(4096) {
            while session.drain_pcm(&mut pcm) != 0 {}
            while session.drain_midi(&mut midi) != 0 {}
        }
        if session.status() != SessionStatus::Running || session.storage_waiting() {
            break;
        }
    }
    let own: u64 = functions.values().map(|s| s.own).sum();
    assert_eq!(cpu, own + unknown + irq + services);
    println!(
        "Replay CPU={cpu} own={own} unknown={unknown} IRQ={irq} software_service={services} completed={} emulated_ns={} status={:?} fault={:?} mismatched_returns={mismatch} unreadable_call_slots={unknown_calls} open_frames={} external_depth={}",
        session.steps() - initial,
        session.emulated_nanos() - initial_ns,
        session.status(),
        session.fault(),
        frames.len(),
        external.len()
    );
    println!(
        "Exclusive dynamic CALL/RET ownership; counts are guest instructions, never host time. Initial/unmatched stack context is unknown. IRQ/software-INT bodies excluded. Inclusive counts overlap and must not be summed."
    );
    let mut ranked: Vec<_> = functions.into_iter().collect();
    ranked.sort_unstable_by_key(|(entry, s)| (Reverse(s.own), *entry));
    for (entry, s) in ranked.iter().take(40) {
        let diagnostic =
            address(*entry).and_then(|a| prepared.diagnostics().iter().find(|d| d.entry == a));
        let accepted = address(*entry).is_some_and(|a| prepared.entries().contains(&a));
        println!(
            "FUNCTION {} linear={entry:#x} calls={} returned={} self={} share={:.5}% inclusive={} IO={} accepted={} diagnostic={diagnostic:?}",
            location(*entry),
            s.calls,
            s.returned,
            s.own,
            100.0 * s.own as f64 / cpu as f64,
            s.inclusive,
            s.io,
            accepted
        );
        let mut callers: Vec<_> = s.callers.iter().collect();
        callers.sort_unstable_by_key(|&(pc, count)| (Reverse(*count), *pc));
        for (pc, count) in callers.into_iter().take(8) {
            println!("  CALLER {} linear={pc:#x} calls={count}", location(*pc));
        }
        let mut rows: Vec<_> = histogram
            .iter()
            .filter(|((root, _, _), _)| root == entry)
            .collect();
        rows.sort_unstable_by_key(|((_, pc, _), (count, _, _))| (Reverse(*count), *pc));
        for ((_, pc, code32), (count, bytes, available)) in rows.into_iter().take(16) {
            let decoded = (*code32)
                .then(|| doslet_x86::analysis::decode(&bytes[..usize::from(*available)], *pc))
                .flatten();
            println!(
                "  INSTRUCTION {} linear={pc:#x} visits={count} code32={code32} bytes={:02x?} decoded={decoded:?}",
                location(*pc),
                &bytes[..usize::from(*available)]
            );
        }
    }
    Ok(())
}
