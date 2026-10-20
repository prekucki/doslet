//! State comparison of an actually executed interval. This never skips work.
use crate::{CmosSnapshot, PcMachine};

pub struct RecurrenceMark {
    space: u64,
    device_accesses: u64,
    scratch_writes: u64,
    interrupts: u64,
    cmos: CmosSnapshot,
}

impl PcMachine {
    /// Starts a separate bounded RAM journal. Writable native RAM borrows are
    /// declined while it is valid; decoded batches continue to record stores.
    pub fn begin_recurrence(&mut self) -> RecurrenceMark {
        RecurrenceMark {
            space: self.bus.ram.begin_recurrence(),
            device_accesses: self.bus.device_accesses,
            scratch_writes: self.bus.cmos_scratch_writes,
            interrupts: self.interrupt_entries,
            cmos: self.bus.cmos.snapshot(),
        }
    }
    /// All written bytes and CMOS registers must be restored. Any interrupt,
    /// mapping change, overflow or other device access rejects the interval.
    /// The runtime separately checks the complete CPU and input state.
    pub fn end_recurrence(&mut self, mark: RecurrenceMark) -> bool {
        let ram_unchanged = self.bus.ram.end_recurrence(mark.space);
        ram_unchanged
            && self.interrupt_entries == mark.interrupts
            && self.bus.device_accesses.wrapping_sub(mark.device_accesses)
                == self
                    .bus
                    .cmos_scratch_writes
                    .wrapping_sub(mark.scratch_writes)
            && self.bus.cmos.snapshot() == mark.cmos
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doslet_bus::{MemoryBus, PortBus};

    #[test]
    fn journal_tracks_overlapping_stores_and_cmos_scratch_without_weakening_idle_proof() {
        let mut m = PcMachine::default();
        m.bus.set_a20(true);
        m.bus.write_port_u8(0x70, 0x0f).unwrap();
        m.bus.write_port_u8(0x71, 9).unwrap();
        for _ in 0..4 {
            let strict = m.begin_idle_proof();
            let mark = m.begin_recurrence();
            assert!(m.bus.native_ram_mut().is_none());
            m.bus.write_u32(0x3000, 0x12345678).unwrap();
            m.bus.write_u16(0x3000, 0).unwrap();
            m.bus.write_u16(0x3002, 0).unwrap();
            m.bus.write_port_u8(0x70, 0x0f).unwrap();
            m.bus.write_port_u8(0x71, 0).unwrap();
            m.bus.write_port_u8(0x71, 9).unwrap();
            assert!(m.end_recurrence(mark));
            assert!(!m.end_idle_proof(strict));
            assert!(m.bus.native_ram_mut().is_some());
        }
    }

    #[test]
    fn journal_rejects_effects_mapping_changes_capacity_and_interrupts() {
        for case in 0..7 {
            let mut m = PcMachine::default();
            m.bus.set_a20(true);
            let mark = m.begin_recurrence();
            match case {
                0 => m.bus.write_u8(0x3000, 1).unwrap(),
                1 => {
                    m.bus.read_port_u8(0x70).unwrap();
                }
                2 => {
                    // Unchanged final selection still touched another register.
                    m.bus.write_port_u8(0x70, 0x0c).unwrap();
                    m.bus.write_port_u8(0x70, 0).unwrap();
                }
                3 => {
                    m.bus.set_a20(false);
                    m.bus.set_a20(true);
                }
                4 => {
                    for i in 0..2049 {
                        m.bus.write_u8(0x3000 + i, 0).unwrap();
                    }
                    // An exhausted journal must not penalize the heavy phase
                    // with an unnecessary native-store fallback.
                    assert!(m.bus.native_ram_mut().is_some());
                }
                5 => {
                    m.cpu.flags = 0x202;
                    m.bus.pic.raise_edge(0);
                    m.step().unwrap();
                    assert_eq!(m.interrupt_entries, 1);
                }
                _ => {
                    m.bus.write_u8(0xa0000, 0).unwrap();
                }
            }
            assert!(!m.end_recurrence(mark), "case {case}");
            let mark = m.begin_recurrence();
            assert!(m.end_recurrence(mark));
        }
    }
}
