//! Opt-in, version-specific observations of the local HEROES timer paths.
//! This observer neither patches guest code nor changes its registers/memory.
use doslet_runtime::Session;
use doslet_x86::Gpr32;
use std::collections::BTreeMap;

fn dword(session: &Session, address: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        session.ram_view(address..address + 4)?.try_into().ok()?,
    ))
}

#[derive(Default)]
pub struct Audit {
    steps: u64,
    callbacks: u64,
    loops: [u64; 3],
    taken: [u64; 3],
    cooperative: u64,
    writes: BTreeMap<u32, u64>,
    previous_tick: Option<u64>,
    periods: u64,
    period_sum: u64,
    period_min: u64,
    period_max: u64,
}

impl Audit {
    pub fn advance_one(&mut self, session: &mut Session) {
        let r = session.registers();
        let (cs, ip) = (r.cs, r.ip);
        let before = dword(session, 0x281098);
        let tick = dword(session, 0x2831D0);
        let flags = session.flags();
        let caller = if cs == 0x180 && ip == 0x219F90 {
            dword(session, r.get32(Gpr32::Esp) as usize)
        } else {
            None
        };
        session.advance(1);
        self.steps += 1;
        // A pending IRQ can enter before the observed instruction executes.
        // Count only its actual successor, not a pre-interrupt PC observation.
        if cs == 0x180 && session.registers().cs == cs {
            for (index, branch) in [0x1E2823, 0x1E2888, 0x1E3321].into_iter().enumerate() {
                if ip == branch && [ip - 7, ip + 2].contains(&session.registers().ip) {
                    self.loops[index] += 1;
                    if session.registers().ip == ip - 7 {
                        self.taken[index] += 1;
                    }
                }
            }
            if ip == 0x205C24 && session.registers().ip == ip + 3 {
                self.cooperative += 1;
            }
            if ip == 0x219F90 && session.registers().ip == ip + 1 {
                self.callbacks += 1;
                if self.callbacks <= 3 {
                    println!(
                        "TIMER callback step={} ns={} return={:?} IF={} counter={before:?}",
                        session.steps(),
                        session.emulated_nanos(),
                        caller.map(|pc| format!("{pc:08X}")),
                        flags & 0x200 != 0
                    );
                }
            }
        }
        if let Some(after) = dword(session, 0x281098)
            && before != Some(after)
        {
            let count = self.writes.entry(ip).or_default();
            *count += 1;
            if *count <= 3 {
                println!(
                    "TIMER write step={} ns={} pc={cs:04X}:{ip:08X} IF={} {before:?}->{after}",
                    session.steps(),
                    session.emulated_nanos(),
                    flags & 0x200 != 0
                );
            }
        }
        if tick != dword(session, 0x2831D0) {
            let now = session.emulated_nanos();
            if let Some(previous) = self.previous_tick {
                let period = now - previous;
                self.period_min = if self.periods == 0 {
                    period
                } else {
                    self.period_min.min(period)
                };
                self.period_max = self.period_max.max(period);
                self.period_sum += period;
                self.periods += 1;
            }
            self.previous_tick = Some(now);
        }
    }

    pub fn print(&self, session: &Session) {
        println!(
            "TIMER AUDIT steps={} callbacks={} loop_branches={:?} taken={:?} cooperative={} writes={:X?} period_ns=min:{} max:{} mean:{} samples:{} final_counter={:?}",
            self.steps,
            self.callbacks,
            self.loops,
            self.taken,
            self.cooperative,
            self.writes,
            self.period_min,
            self.period_max,
            self.period_sum.checked_div(self.periods).unwrap_or(0),
            self.periods,
            dword(session, 0x281098)
        );
        for slot in 0..16 {
            if dword(session, 0x263E64 + slot * 4) == Some(0x219F90) {
                println!(
                    "TIMER slot={slot} state={:?} phase_us={:?} period_us={:?} pending={:?}",
                    dword(session, 0x263EA4 + slot * 4),
                    dword(session, 0x263EE4 + slot * 4),
                    dword(session, 0x263F24 + slot * 4),
                    dword(session, 0x263F64 + slot * 4)
                );
            }
        }
    }
}
