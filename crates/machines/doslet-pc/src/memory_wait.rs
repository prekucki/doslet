//! Opt-in batching of explicitly selected CMP [dword],0 / JNE iterations.
use crate::{MachineError, PcMachine};
use doslet_x86::{Access, SegmentRegister, flags};

/// Instruction offsets and a DS-relative operand selected by the caller.
/// Code and data are validated again before every batch. Disabled by default.
#[derive(Clone, Copy)]
pub struct MemoryWaitSites {
    pub starts: [u32; 3],
    pub operand: u32,
}

impl PcMachine {
    pub fn set_memory_wait_sites(&mut self, sites: Option<MemoryWaitSites>) {
        self.memory_wait_sites = sites;
    }

    pub fn memory_wait_steps(&self) -> u64 {
        self.memory_wait_steps
    }

    pub(crate) fn advance_memory_wait(&mut self, budget: u32) -> Result<Option<u32>, MachineError> {
        let Some(sites) = self.memory_wait_sites else {
            return Ok(None);
        };
        let r = &self.cpu.registers;
        if budget < 2
            || !sites.starts.contains(&r.ip)
            || !r.code32()
            || self.cpu.is_halted()
            || !self.cpu.accepts_maskable_interrupt()
            || self.cpu.has_interrupt_shadow()
            || self.bus.pic.has_interrupt()
            || self.cpu.flags & (flags::TRAP | flags::RESUME | flags::VIRTUAL | flags::ALIGNMENT)
                != 0
            || self.cpu.control.cr0 & 0x8000_0000 != 0
        {
            return Ok(None);
        }
        let Ok(code) = r.address(SegmentRegister::Cs, r.ip, 9, Access::Execute) else {
            return Ok(None);
        };
        let Some(code_end) = (code as usize).checked_add(9) else {
            return Ok(None);
        };
        let operand = sites.operand.to_le_bytes();
        if self.bus.ram_view(code as usize..code_end)
            != Some(&[
                0x83, 0x3D, operand[0], operand[1], operand[2], operand[3], 0, 0x75, 0xF7,
            ])
        {
            return Ok(None);
        }
        let Ok(data) = r.address(SegmentRegister::Ds, sites.operand, 4, Access::Read) else {
            return Ok(None);
        };
        let Some(data_end) = (data as usize).checked_add(4) else {
            return Ok(None);
        };
        let Some(bytes) = self.bus.ram_view(data as usize..data_end) else {
            return Ok(None);
        };
        let value = u32::from_le_bytes(bytes.try_into().expect("four-byte RAM view"));
        if value == 0 || value > i32::MAX as u32 {
            return Ok(None);
        }
        // CMP positive,0 leaves only low-byte parity among arithmetic flags.
        // Requiring the already matching flags also preserves a first arrival
        // that must execute CMP before whole iterations become invariant.
        let arithmetic = flags::CARRY
            | flags::PARITY
            | flags::AUXILIARY
            | flags::ZERO
            | flags::SIGN
            | flags::OVERFLOW;
        let expected = if (value as u8).count_ones().is_multiple_of(2) {
            flags::PARITY
        } else {
            0
        };
        if self.cpu.flags & arithmetic != expected {
            return Ok(None);
        }
        // IRQs and DMA may change RAM. Never cross their event-producing step;
        // preserve the CMP/JNE phase there by returning to ordinary execution.
        let steps = budget.min(self.idle_steps_to_event(u32::MAX).saturating_sub(1)) / 2 * 2;
        if steps == 0 {
            return Ok(None);
        }
        self.advance_hardware_steps(steps)?;
        self.memory_wait_steps += u64::from(steps);
        #[cfg(feature = "profiling")]
        {
            self.bus.stats.cpu_steps += u64::from(steps);
        }
        Ok(Some(steps))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use doslet_bus::MemoryBus;
    use doslet_x86::{Gpr32, SegmentRegister};

    const START: u32 = 0x10020;
    const DATA: u32 = 0x3000;
    fn machine(dma: bool) -> PcMachine {
        let mut m = crate::timing::tests::idle_machine(dma);
        m.bus.set_a20(true);
        for (address, bytes) in [
            (0x818, &[0xFF, 0xFF, 0, 0, 0, 0x9A, 0xCF, 0][..]),
            (0x820, &[0xFF, 0xFF, 0, 0, 0, 0x92, 0xCF, 0][..]),
            (
                0,
                &[
                    0xB8, 0x15, 0, 0x0F, 0x01, 0xF0, 0x90, 0x66, 0xEA, 0x20, 0, 1, 0, 0x18, 0,
                ][..],
            ),
            (
                START,
                &[0x83, 0x3D, 0, 0x30, 0, 0, 0, 0x75, 0xF7, 0xEB, 0xFE][..],
            ),
            // IRQ0 decrements the synthetic countdown only while positive.
            (
                0x20000,
                &[
                    0x50, 0x83, 0x3D, 0, 0x30, 0, 0, 0, 0x7E, 6, 0xFF, 0x0D, 0, 0x30, 0, 0, 0xB0,
                    0x20, 0xE6, 0x20, 0x58, 0xCF,
                ][..],
            ),
            // SB block IRQ: acknowledge DSP and PIC without touching countdown.
            (
                0x20100,
                &[
                    0x50, 0x52, 0x66, 0xBA, 0x2F, 2, 0xEC, 0xB0, 0x20, 0xE6, 0x20, 0x5A, 0x58, 0xCF,
                ][..],
            ),
        ] {
            for (offset, byte) in bytes.iter().enumerate() {
                m.bus.write_u8(address + offset as u32, *byte).unwrap();
            }
        }
        m.cpu.control.gdtr.base = 0x800;
        m.cpu.control.gdtr.limit = 0x7F;
        for _ in 0..4 {
            m.cpu.step(&mut m.bus).unwrap();
        }
        for segment in [
            SegmentRegister::Ds,
            SegmentRegister::Es,
            SegmentRegister::Ss,
        ] {
            m.cpu.load_segment(&mut m.bus, segment, 0x20).unwrap();
        }
        m.cpu.registers.set32(Gpr32::Esp, 0x9000);
        m.cpu.control.idtr.base = 0x1000;
        m.cpu.control.idtr.limit = 0x7FF;
        for (vector, low) in [(8, 0_u16), (15, 0x100)] {
            let gate = [low as u8, (low >> 8) as u8, 0x18, 0, 0, 0x8E, 2, 0];
            for (offset, byte) in gate.into_iter().enumerate() {
                m.bus
                    .write_u8(0x1000 + vector * 8 + offset as u32, byte)
                    .unwrap();
            }
        }
        m.bus.pit.write_control(0x34).unwrap();
        m.bus.pit.write_counter(0, 0x20).unwrap();
        m.bus.pit.write_counter(0, 0x4E).unwrap(); // 20,000 PIT ticks.
        m.bus.write_u32(DATA, 2).unwrap();
        m.cpu.flags = flags::INTERRUPT | 2;
        m.set_memory_wait_sites(Some(MemoryWaitSites {
            starts: [START, 0, 0],
            operand: DATA,
        }));
        m
    }

    fn advance(m: &mut PcMachine, mut budget: u32) -> u32 {
        let mut dispatches = 0;
        while budget != 0 {
            let (_, used) = m.step_with_idle_budget(budget).unwrap();
            assert!(used > 0 && used <= budget);
            budget -= used;
            dispatches += 1;
        }
        dispatches
    }

    fn compare(a: &mut PcMachine, b: &mut PcMachine) {
        assert_eq!(a.cpu.registers, b.cpu.registers);
        assert_eq!(a.cpu.flags, b.cpu.flags);
        assert_eq!(a.time(), b.time());
        assert_eq!(a.bus.ram_view(0..0xA0000), b.bus.ram_view(0..0xA0000));
        assert_eq!(a.bus.vga.vram_view(), b.bus.vga.vram_view());
        assert_eq!(a.bus.pic.read_command(), b.bus.pic.read_command());
        assert_eq!(a.bus.dsp.irq_status(), b.bus.dsp.irq_status());
        for _ in 0..2 {
            assert_eq!(a.bus.pit.read_counter(0), b.bus.pit.read_counter(0));
        }
        let mut pa = [[0; 2]; 512];
        let mut pb = [[0; 2]; 512];
        assert_eq!(a.drain_pcm(&mut pa), b.drain_pcm(&mut pb));
        assert_eq!(pa, pb);
    }

    #[test]
    fn countdown_exit_matches_reference_with_partial_budgets_irqs_dma_and_pcm() {
        for dma in [false, true] {
            let mut fast = machine(dma);
            let mut reference = machine(dma);
            reference.set_memory_wait_sites(None);
            let mut dispatches = 0;
            let mut total = 0;
            for _ in 0..40 {
                for budget in [1, 2, 3, 7, 9999] {
                    dispatches += advance(&mut fast, budget);
                    advance(&mut reference, budget);
                    total += budget;
                    compare(&mut fast, &mut reference);
                }
            }
            assert_eq!(fast.bus.read_u32(DATA).unwrap(), 0);
            assert!(fast.memory_wait_steps() > 200_000);
            assert!(dispatches < total / 2);
        }
    }

    #[test]
    fn changed_code_zero_negative_flags_debug_irq_and_shadow_fall_back() {
        for case in 0..9 {
            let mut m = machine(false);
            // A completed CMP/JNE pair makes arithmetic flags invariant.
            advance(&mut m, 2);
            match case {
                0 => m.bus.write_u32(DATA, 0).unwrap(),
                1 => m.bus.write_u32(DATA, u32::MAX).unwrap(),
                2 => m.cpu.flags |= flags::CARRY,
                3 => m.cpu.flags |= flags::TRAP,
                4 => m.cpu.flags &= !flags::INTERRUPT,
                5 => m.bus.pic.raise_edge(0),
                6 => m.bus.write_u8(START + 6, 1).unwrap(),
                7 => m.cpu.control.cr0 |= 0x8000_0000,
                _ => {
                    m.bus.write_u8(0x5000, 0xFB).unwrap();
                    m.cpu.registers.ip = 0x5000;
                    m.cpu.step(&mut m.bus).unwrap();
                    m.cpu.registers.ip = START;
                }
            }
            assert_eq!(m.advance_memory_wait(10000).unwrap(), None, "case={case}");
            assert_eq!(m.memory_wait_steps(), 0);
        }
    }
}
