//! CMOS configuration-register foundation; RTC ticking and interrupts are pending.
#![no_std]

/// Small register state for diagnostics; taking it has no read side effects.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Snapshot {
    registers: [u8; 128],
    selected: u8,
}

pub struct Cmos {
    registers: [u8; 128],
    selected: u8,
}
impl Cmos {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            registers: self.registers,
            selected: self.selected,
        }
    }
    pub fn new(base_kib: u16, extended_kib: u16) -> Self {
        let mut registers = [0; 128];
        // Initial guest clock: 2026-10-14 12:00:00, BCD, 24-hour mode.
        registers[4] = 0x12;
        registers[6] = 4;
        registers[7] = 0x14;
        registers[8] = 0x10;
        registers[9] = 0x26;
        registers[0x32] = 0x20;
        registers[0x0A] = 0x26;
        registers[0x0B] = 2;
        registers[0x0D] = 0x80;
        registers[0x15..0x17].copy_from_slice(&base_kib.to_le_bytes());
        registers[0x17..0x19].copy_from_slice(&extended_kib.to_le_bytes());
        registers[0x30..0x32].copy_from_slice(&extended_kib.to_le_bytes());
        Self {
            registers,
            selected: 0,
        }
    }
    pub fn select(&mut self, value: u8) {
        self.selected = value;
    }
    pub fn selected(&self) -> u8 {
        self.selected
    }
    pub fn read(&mut self) -> u8 {
        let index = usize::from(self.selected & 0x7F);
        let value = self.registers[index];
        if index == 0x0C {
            self.registers[index] = 0;
        }
        value
    }
    pub fn write(&mut self, value: u8) {
        let index = usize::from(self.selected & 0x7F);
        if !matches!(index, 0x0C | 0x0D) {
            self.registers[index] = value;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_includes_selection_and_does_not_acknowledge_status() {
        let mut cmos = Cmos::new(640, 15360);
        cmos.registers[0x0c] = 0x80;
        cmos.select(0x0c);
        let before = cmos.snapshot();
        assert_eq!(cmos.snapshot(), before);
        assert_eq!(cmos.read(), 0x80);
        assert_ne!(cmos.snapshot(), before);
        let acknowledged = cmos.snapshot();
        cmos.select(0x8c);
        assert_ne!(cmos.snapshot(), acknowledged);
    }
    #[test]
    fn memory_configuration_and_index_nmi_bit_do_not_overlap() {
        let mut cmos = Cmos::new(640, 15360);
        for (index, expected) in [
            (0x15, 0x80),
            (0x16, 2),
            (0x17, 0),
            (0x18, 0x3C),
            (0x30, 0),
            (0x31, 0x3C),
        ] {
            cmos.select(index | 0x80);
            assert_eq!(cmos.read(), expected);
            assert_eq!(cmos.selected(), index | 0x80);
        }
        cmos.select(0x0D);
        cmos.write(0);
        assert_eq!(cmos.read(), 0x80);
    }
}

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
