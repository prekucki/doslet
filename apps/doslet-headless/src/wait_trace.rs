//! Host-only wait audit. Observations never change guest execution or devices.
use doslet_runtime::Session;
use doslet_x86::{Access, Gpr16, SegmentCache, SegmentRegister};
use std::{
    collections::BTreeMap,
    io::{self, Write},
};

#[derive(Clone, Copy)]
pub struct Before {
    cs: SegmentCache,
    ip: u32,
    linear: u32,
    opcode: u8,
    target: Option<u32>,
    port: u16,
    ax: u16,
    irq_entries: u64,
    port_reads: u64,
}
#[derive(Default)]
struct Branch {
    taken: u64,
    fallthrough: u64,
}
#[derive(Default)]
struct Port {
    count: u64,
    and: u8,
    or: u8,
}
#[derive(Default)]
struct Segment {
    count: u64,
    low: u32,
    high: u32,
}
fn entry<'a, K: Ord, V: Default>(
    map: &'a mut BTreeMap<K, V>,
    key: K,
    dropped: &mut u64,
) -> Option<&'a mut V> {
    if map.len() >= 65536 && !map.contains_key(&key) {
        *dropped += 1;
        None
    } else {
        Some(map.entry(key).or_default())
    }
}
#[derive(Default)]
pub struct WaitTrace {
    branches: BTreeMap<(u32, u32, u8), Branch>,
    ports: BTreeMap<(u32, u16), Port>,
    services: BTreeMap<(u8, u16), u64>,
    halts: BTreeMap<u32, u64>,
    segments: BTreeMap<(u32, u32, bool), Segment>,
    steps: u64,
    dropped: u64,
}
impl WaitTrace {
    pub fn before(&self, session: &Session) -> Option<Before> {
        let r = session.registers();
        let cs = r.segment_cache(SegmentRegister::Cs);
        let length = (u64::from(cs.limit).saturating_sub(u64::from(r.ip)) + 1).min(15) as u8;
        let mut linear = r
            .address(SegmentRegister::Cs, r.ip, length, Access::Execute)
            .ok()?;
        if !session.a20_enabled() {
            linear &= !(1 << 20);
        }
        let bytes = session.ram_view(linear as usize..linear as usize + usize::from(length))?;
        let mut at = 0;
        let mut wide = cs.default32;
        while bytes.get(at).is_some_and(|byte| {
            matches!(
                byte,
                0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 | 0x66 | 0x67 | 0xF0 | 0xF2 | 0xF3
            )
        }) {
            if bytes[at] == 0x66 {
                wide = !cs.default32;
            }
            at += 1;
        }
        let opcode = *bytes.get(at)?;
        at += 1;
        let near = opcode == 0xE9
            || (opcode == 0x0F && bytes.get(at).is_some_and(|b| (0x80..=0x8F).contains(b)));
        if opcode == 0x0F && near {
            at += 1;
        }
        let displacement = if near {
            if wide {
                let raw: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
                at += 4;
                Some(i32::from_le_bytes(raw))
            } else {
                let raw: [u8; 2] = bytes.get(at..at + 2)?.try_into().ok()?;
                at += 2;
                Some(i32::from(i16::from_le_bytes(raw)))
            }
        } else if matches!(opcode, 0x70..=0x7F | 0xE0..=0xE3 | 0xEB) {
            let displacement = i32::from(*bytes.get(at)? as i8);
            at += 1;
            Some(displacement)
        } else {
            None
        };
        let target = displacement
            .filter(|displacement| *displacement < 0)
            .and_then(|displacement| {
                let mut offset =
                    r.ip.wrapping_add(at as u32)
                        .wrapping_add(displacement as u32);
                if (near && !wide) || (!near && !cs.default32) {
                    offset &= 0xFFFF;
                }
                let mut address = r
                    .address(SegmentRegister::Cs, offset, 1, Access::Execute)
                    .ok()?;
                if !session.a20_enabled() {
                    address &= !(1 << 20);
                }
                Some(address)
            });
        let port = if matches!(opcode, 0xE4 | 0xE5) {
            u16::from(*bytes.get(at)?)
        } else {
            r.get(Gpr16::Dx)
        };
        let stats = session.execution_stats();
        Some(Before {
            cs,
            ip: r.ip,
            linear,
            opcode,
            target,
            port,
            ax: r.get(Gpr16::Ax),
            irq_entries: stats.irq_entries,
            port_reads: stats.port_reads,
        })
    }
    pub fn after(&mut self, session: &Session, before: Option<Before>) {
        self.steps += 1;
        let Some(before) = before else {
            return;
        };
        let stats = session.execution_stats();
        if stats.irq_entries != before.irq_entries {
            return;
        }
        if let Some(segment) = entry(
            &mut self.segments,
            (before.cs.base, before.cs.limit, before.cs.default32),
            &mut self.dropped,
        ) {
            if segment.count == 0 {
                segment.low = before.linear;
            }
            segment.count += 1;
            segment.low = segment.low.min(before.linear);
            segment.high = segment.high.max(before.linear.saturating_add(15));
        }
        let r = session.registers();
        if let Some(target) = before.target {
            let Some(entry) = entry(
                &mut self.branches,
                (
                    before.linear,
                    target,
                    if before.cs.default32 { 32 } else { 16 },
                ),
                &mut self.dropped,
            ) else {
                return;
            };
            let mut after = r
                .address(SegmentRegister::Cs, r.ip, 1, Access::Execute)
                .unwrap_or(u32::MAX);
            if !session.a20_enabled() {
                after &= !(1 << 20);
            }
            if r.segment_cache(SegmentRegister::Cs) == before.cs && after == target {
                entry.taken += 1;
            } else if r.segment_cache(SegmentRegister::Cs) == before.cs {
                entry.fallthrough += 1;
            }
        }
        if matches!(before.opcode, 0xE4 | 0xE5 | 0xEC | 0xED)
            && stats.port_reads > before.port_reads
        {
            let Some(entry) = entry(
                &mut self.ports,
                (before.linear, before.port),
                &mut self.dropped,
            ) else {
                return;
            };
            let value = r.get_byte(0);
            if entry.count == 0 {
                entry.and = value;
            }
            entry.count += 1;
            entry.and &= value;
            entry.or |= value;
        }
        if before.cs.selector == 0xF000
            && before.ip <= 255
            && let Some(entry) = entry(
                &mut self.services,
                (before.ip as u8, before.ax),
                &mut self.dropped,
            )
        {
            *entry += 1;
        }
        if before.opcode == 0xF4
            && r.ip != before.ip
            && let Some(entry) = entry(&mut self.halts, before.linear, &mut self.dropped)
        {
            *entry += 1;
        }
    }
    pub fn write(&self, mut out: impl Write) -> io::Result<()> {
        writeln!(
            out,
            "{{\"steps\":{},\"dropped\":{},\"branches\":[",
            self.steps, self.dropped
        )?;
        for (i, ((source, target, bits), branch)) in self.branches.iter().enumerate() {
            if i != 0 {
                writeln!(out, ",")?;
            }
            write!(
                out,
                "{{\"source\":{source},\"target\":{target},\"bits\":{bits},\"taken\":{},\"fallthrough\":{}}}",
                branch.taken, branch.fallthrough
            )?;
        }
        writeln!(out, "],\"ports\":[")?;
        for (i, ((source, port), entry)) in self.ports.iter().enumerate() {
            if i != 0 {
                writeln!(out, ",")?;
            }
            write!(
                out,
                "{{\"source\":{source},\"port\":{port},\"reads\":{},\"and\":{},\"or\":{}}}",
                entry.count, entry.and, entry.or
            )?;
        }
        writeln!(out, "],\"services\":[")?;
        for (i, ((vector, ax), count)) in self.services.iter().enumerate() {
            if i != 0 {
                writeln!(out, ",")?;
            }
            write!(out, "{{\"vector\":{vector},\"ax\":{ax},\"calls\":{count}}}")?;
        }
        writeln!(out, "],\"halts\":[")?;
        for (i, (source, count)) in self.halts.iter().enumerate() {
            if i != 0 {
                writeln!(out, ",")?;
            }
            write!(out, "{{\"source\":{source},\"calls\":{count}}}")?;
        }
        writeln!(out, "],\"segments\":[")?;
        for (i, ((base, limit, wide), segment)) in self.segments.iter().enumerate() {
            if i != 0 {
                writeln!(out, ",")?;
            }
            write!(
                out,
                "{{\"base\":{base},\"limit\":{limit},\"bits\":{},\"steps\":{},\"observed_low\":{},\"observed_high\":{}}}",
                if *wide { 32 } else { 16 },
                segment.count,
                segment.low,
                segment.high
            )?;
        }
        writeln!(out, "]}}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trace_reports_executed_io_and_back_edges_without_changing_the_machine() {
        let code = [
            0xB8, 0x13, 0, 0xCD, 0x10, 0xBA, 0xDA, 3, 0xB4, 8, 0xEC, 0x84, 0xC4, 0x74, 0xFB, 0xEB,
            0xFE,
        ];
        let mut observed = Session::new(&code).unwrap();
        let mut reference = Session::new(&code).unwrap();
        observed.start();
        reference.start();
        let mut trace = WaitTrace::default();
        for _ in 0..200 {
            let before = trace.before(&observed);
            observed.advance(1);
            trace.after(&observed, before);
            reference.advance(1);
        }
        assert_eq!(observed.registers(), reference.registers());
        assert_eq!(observed.emulated_nanos(), reference.emulated_nanos());
        assert_eq!(
            observed.ram_view(0x10000..0x11000),
            reference.ram_view(0x10000..0x11000)
        );
        assert_eq!(trace.ports.len(), 1);
        assert!(trace.ports.values().next().unwrap().count > 40);
        assert!(trace.branches.values().any(|branch| branch.taken > 40));
        assert!(trace.services.contains_key(&(0x10, 0x0013)));
        let mut json = Vec::new();
        trace.write(&mut json).unwrap();
        assert!(String::from_utf8(json).unwrap().contains("\"port\":986"));
    }
}
