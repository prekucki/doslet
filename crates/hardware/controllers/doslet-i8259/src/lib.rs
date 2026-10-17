//! Single, edge-triggered 8259 foundation: mask, fixed priority, acknowledge and EOI.
//! Cascading, rotating priorities and level-triggered operation are not implemented.
#![no_std]

#[derive(Debug)]
pub struct Pic {
    pending: u8,
    mask: u8,
    in_service: u8,
    vector_base: u8,
    init_word: u8,
    needs_icw4: bool,
    single: bool,
    read_isr: bool,
}

impl Default for Pic {
    fn default() -> Self {
        Self::new(8)
    }
}
impl Pic {
    pub const fn new(vector_base: u8) -> Self {
        Self {
            pending: 0,
            mask: 0,
            in_service: 0,
            vector_base: vector_base & 0xF8,
            init_word: 0,
            needs_icw4: false,
            single: true,
            read_isr: false,
        }
    }
    pub fn raise_edge(&mut self, irq: u8) {
        if irq < 8 {
            self.pending |= 1 << irq;
        }
    }
    pub fn has_interrupt(&self) -> bool {
        self.next_irq().is_some()
    }
    fn next_irq(&self) -> Option<u8> {
        let candidates = self.pending & !self.mask;
        let irq = candidates.trailing_zeros();
        if irq >= 8 || irq >= self.in_service.trailing_zeros() {
            None
        } else {
            Some(irq as u8)
        }
    }
    pub fn acknowledge(&mut self) -> Option<u8> {
        let irq = self.next_irq()?;
        self.pending &= !(1 << irq);
        self.in_service |= 1 << irq;
        Some(self.vector_base.wrapping_add(irq))
    }
    pub fn read_command(&self) -> u8 {
        if self.read_isr {
            self.in_service
        } else {
            self.pending
        }
    }
    pub fn read_data(&self) -> u8 {
        self.mask
    }
    pub fn write_command(&mut self, value: u8) {
        if value & 0x10 != 0 {
            self.pending = 0;
            self.in_service = 0;
            self.mask = 0;
            self.needs_icw4 = value & 1 != 0;
            self.single = value & 2 != 0;
            self.init_word = 2;
        } else if value & 0x08 != 0 {
            if value & 2 != 0 {
                self.read_isr = value & 1 != 0;
            }
        } else if value & 0x20 != 0 {
            let irq = if value & 0x40 != 0 {
                u32::from(value & 7)
            } else {
                self.in_service.trailing_zeros()
            };
            if irq < 8 {
                self.in_service &= !(1 << irq);
            }
        }
    }
    pub fn write_data(&mut self, value: u8) {
        match self.init_word {
            2 => {
                self.vector_base = value & 0xF8;
                self.init_word = if !self.single {
                    3
                } else if self.needs_icw4 {
                    4
                } else {
                    0
                };
            }
            3 => self.init_word = if self.needs_icw4 { 4 } else { 0 },
            4 => self.init_word = 0,
            _ => self.mask = value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mask_priority_and_eoi_preserve_pending_edges() {
        let mut pic = Pic::default();
        pic.write_data(1 << 1);
        pic.raise_edge(1);
        pic.raise_edge(3);
        assert_eq!(pic.acknowledge(), Some(11));
        pic.write_data(0);
        assert_eq!(pic.acknowledge(), Some(9));
        pic.raise_edge(4);
        assert!(!pic.has_interrupt());
        pic.write_command(0x20);
        pic.write_command(0x20);
        assert_eq!(pic.acknowledge(), Some(12));
    }
    #[test]
    fn initialization_relocates_vectors() {
        let mut pic = Pic::default();
        pic.write_command(0x13);
        pic.write_data(0x20);
        pic.write_data(1);
        pic.raise_edge(0);
        assert_eq!(pic.acknowledge(), Some(0x20));
    }
}

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
