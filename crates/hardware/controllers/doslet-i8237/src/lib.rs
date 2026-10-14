//! Independent 8237 register state and bounded peripheral transfer accounting.
//! Machine wiring supplies page registers, transfer width, memory and arbitration.
#![no_std]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DmaError {
    InvalidRegister(u8),
    InvalidChannel(u8),
    UnsupportedCommand(u8),
    UnsupportedMode(u8),
    WrongDirection,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Verify,
    DeviceToMemory,
    MemoryToDevice,
}
#[derive(Clone, Copy, Debug, Default)]
struct Channel {
    base_address: u16,
    address: u16,
    base_count: u16,
    count: u16,
    mode: u8,
}
#[derive(Debug)]
pub struct Dma {
    channels: [Channel; 4],
    high: bool,
    mask: u8,
    command: u8,
    software_request: u8,
    dreq: u8,
    terminal: u8,
}
impl Default for Dma {
    fn default() -> Self {
        Self {
            channels: [Channel::default(); 4],
            high: false,
            mask: 15,
            command: 0,
            software_request: 0,
            dreq: 0,
            terminal: 0,
        }
    }
}
impl Dma {
    pub fn read_register(&mut self, register: u8) -> Result<u8, DmaError> {
        match register {
            0..=7 => {
                let channel = &self.channels[usize::from(register / 2)];
                let value = if register & 1 == 0 {
                    channel.address
                } else {
                    channel.count
                };
                let result = (value >> if self.high { 8 } else { 0 }) as u8;
                self.high = !self.high;
                Ok(result)
            }
            8 => {
                let value = self.terminal | ((self.software_request | self.dreq) << 4);
                self.terminal = 0;
                Ok(value)
            }
            15 => Ok(self.mask | 0xF0),
            _ => Err(DmaError::InvalidRegister(register)),
        }
    }
    pub fn write_register(&mut self, register: u8, value: u8) -> Result<(), DmaError> {
        match register {
            0..=7 => {
                let channel = &mut self.channels[usize::from(register / 2)];
                let (base, current) = if register & 1 == 0 {
                    (&mut channel.base_address, &mut channel.address)
                } else {
                    (&mut channel.base_count, &mut channel.count)
                };
                let shift = if self.high { 8 } else { 0 };
                *base = (*base & !(0xFF << shift)) | (u16::from(value) << shift);
                *current = *base;
                self.high = !self.high;
            }
            8 => {
                if value & !4 != 0 {
                    return Err(DmaError::UnsupportedCommand(value));
                }
                self.command = value;
            }
            9 => {
                let bit = 1 << (value & 3);
                if value & 4 != 0 {
                    self.software_request |= bit;
                } else {
                    self.software_request &= !bit;
                }
            }
            10 => {
                let bit = 1 << (value & 3);
                if value & 4 != 0 {
                    self.mask |= bit;
                } else {
                    self.mask &= !bit;
                }
            }
            11 => {
                if value & 12 == 12 {
                    return Err(DmaError::UnsupportedMode(value));
                }
                self.channels[usize::from(value & 3)].mode = value;
            }
            12 => self.high = false,
            13 => {
                self.high = false;
                self.mask = 15;
                self.command = 0;
                self.software_request = 0;
                self.terminal = 0;
            }
            14 => self.mask = 0,
            15 => self.mask = value & 15,
            _ => return Err(DmaError::InvalidRegister(register)),
        }
        Ok(())
    }
    pub fn request(&mut self, channel: u8, active: bool) -> Result<(), DmaError> {
        if channel >= 4 {
            return Err(DmaError::InvalidChannel(channel));
        }
        if active {
            self.dreq |= 1 << channel;
        } else {
            self.dreq &= !(1 << channel);
        }
        Ok(())
    }
    /// One eligible transfer address, without changing counters. The machine
    /// performs the memory/device access before calling complete_transfer.
    pub fn transfer_address(
        &self,
        channel: u8,
        direction: Direction,
    ) -> Result<Option<u16>, DmaError> {
        let state = self
            .channels
            .get(usize::from(channel))
            .ok_or(DmaError::InvalidChannel(channel))?;
        if self.command & 4 != 0
            || self.mask & (1 << channel) != 0
            || (self.software_request | self.dreq) & (1 << channel) == 0
        {
            return Ok(None);
        }
        if state.mode & 0xC0 == 0xC0 {
            return Err(DmaError::UnsupportedMode(state.mode));
        }
        let configured = match (state.mode >> 2) & 3 {
            0 => Direction::Verify,
            1 => Direction::DeviceToMemory,
            2 => Direction::MemoryToDevice,
            _ => return Err(DmaError::UnsupportedMode(state.mode)),
        };
        if configured != direction {
            return Err(DmaError::WrongDirection);
        }
        Ok(Some(state.address))
    }
    /// Commit one byte/word unit. Returns terminal-count, reloading auto-init
    /// state or masking a completed single transfer. The page is external.
    pub fn complete_transfer(&mut self, channel: u8) -> Result<bool, DmaError> {
        let state = self
            .channels
            .get_mut(usize::from(channel))
            .ok_or(DmaError::InvalidChannel(channel))?;
        state.address = if state.mode & 0x20 == 0 {
            state.address.wrapping_add(1)
        } else {
            state.address.wrapping_sub(1)
        };
        let terminal = state.count == 0;
        state.count = state.count.wrapping_sub(1);
        if terminal {
            self.terminal |= 1 << channel;
            self.software_request &= !(1 << channel);
            if state.mode & 0x10 != 0 {
                state.address = state.base_address;
                state.count = state.base_count;
            } else {
                self.mask |= 1 << channel;
            }
        }
        Ok(terminal)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn program(dma: &mut Dma, address: u16, count: u16, mode: u8) {
        dma.write_register(12, 0).unwrap();
        for byte in address.to_le_bytes() {
            dma.write_register(2, byte).unwrap();
        }
        for byte in count.to_le_bytes() {
            dma.write_register(3, byte).unwrap();
        }
        dma.write_register(11, mode).unwrap();
        dma.write_register(10, 1).unwrap();
        dma.request(1, true).unwrap();
    }
    #[test]
    fn shared_flipflop_mask_direction_and_terminal_count_are_observable() {
        let mut dma = Dma::default();
        program(&mut dma, 0xFFFF, 1, 0x49);
        assert_eq!(
            dma.transfer_address(1, Direction::MemoryToDevice),
            Ok(Some(0xFFFF))
        );
        assert_eq!(
            dma.transfer_address(1, Direction::DeviceToMemory),
            Err(DmaError::WrongDirection)
        );
        assert!(!dma.complete_transfer(1).unwrap());
        assert_eq!(
            dma.transfer_address(1, Direction::MemoryToDevice),
            Ok(Some(0))
        );
        assert!(dma.complete_transfer(1).unwrap());
        assert_eq!(dma.transfer_address(1, Direction::MemoryToDevice), Ok(None));
        assert_eq!(dma.read_register(8), Ok(0x22));
        assert_eq!(dma.read_register(8), Ok(0x20));
        dma.write_register(12, 0).unwrap();
        assert_eq!(dma.read_register(3), Ok(0xFF));
        assert_eq!(dma.read_register(2), Ok(0)); // Same flipflop: high address byte.
        dma.write_register(12, 0).unwrap();
        assert_eq!(dma.read_register(2), Ok(1));
    }
    #[test]
    fn auto_init_decrements_and_reloads_without_crossing_the_page_owner() {
        let mut dma = Dma::default();
        program(&mut dma, 0, 1, 0x79);
        assert!(!dma.complete_transfer(1).unwrap());
        assert_eq!(
            dma.transfer_address(1, Direction::MemoryToDevice),
            Ok(Some(0xFFFF))
        );
        assert!(dma.complete_transfer(1).unwrap());
        assert_eq!(
            dma.transfer_address(1, Direction::MemoryToDevice),
            Ok(Some(0))
        );
        dma.write_register(8, 4).unwrap();
        assert_eq!(dma.transfer_address(1, Direction::MemoryToDevice), Ok(None));
        assert_eq!(
            dma.write_register(8, 1),
            Err(DmaError::UnsupportedCommand(1))
        );
    }
}
