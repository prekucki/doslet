//! Bounded AT keyboard-controller command/output-port foundation.
#![no_std]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControllerError {
    UnsupportedCommand(u8),
    UnsupportedData(u8),
    OutputFull,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputLines {
    pub a20_enabled: bool,
    pub reset_requested: bool,
}
#[derive(Clone, Copy, Default)]
enum Pending {
    #[default]
    None,
    CommandByte,
    OutputPort,
}

pub struct Controller {
    command_byte: u8,
    output_port: u8,
    pending: Pending,
    last_command: bool,
    output: [u8; 16],
    head: usize,
    len: usize,
}
impl Default for Controller {
    fn default() -> Self {
        Self::new()
    }
}
impl Controller {
    pub const fn new() -> Self {
        Self {
            command_byte: 0x45,
            output_port: 1,
            pending: Pending::None,
            last_command: false,
            output: [0; 16],
            head: 0,
            len: 0,
        }
    }
    pub fn status(&self) -> u8 {
        4 | u8::from(self.len != 0) | (u8::from(self.last_command) << 3)
    }
    pub fn read_data(&mut self) -> u8 {
        if self.len == 0 {
            return 0xFF;
        }
        let byte = self.output[self.head];
        self.head = (self.head + 1) % self.output.len();
        self.len -= 1;
        byte
    }
    fn reply(&mut self, byte: u8) -> Result<(), ControllerError> {
        if self.len == self.output.len() {
            return Err(ControllerError::OutputFull);
        }
        self.output[(self.head + self.len) % self.output.len()] = byte;
        self.len += 1;
        Ok(())
    }
    /// Machine wiring can reflect the A20 signal without introducing a device dependency.
    pub fn set_a20_line(&mut self, enabled: bool) {
        self.output_port = (self.output_port & !2) | (u8::from(enabled) << 1);
    }
    pub fn write_command(&mut self, command: u8) -> Result<(), ControllerError> {
        self.last_command = true;
        match command {
            0x20 => self.reply(self.command_byte)?,
            0x60 => self.pending = Pending::CommandByte,
            0xAA => self.reply(0x55)?,
            0xAB => self.reply(0)?,
            0xAD => self.command_byte |= 0x10,
            0xAE => self.command_byte &= !0x10,
            0xD0 => self.reply(self.output_port)?,
            0xD1 => self.pending = Pending::OutputPort,
            0xFF => {} // Pulse command with no output lines selected: no operation.
            _ => return Err(ControllerError::UnsupportedCommand(command)),
        }
        Ok(())
    }
    pub fn write_data(&mut self, value: u8) -> Result<Option<OutputLines>, ControllerError> {
        self.last_command = false;
        match self.pending {
            Pending::CommandByte => {
                self.command_byte = value;
                self.pending = Pending::None;
                Ok(None)
            }
            Pending::OutputPort => {
                self.output_port = value;
                self.pending = Pending::None;
                Ok(Some(OutputLines {
                    a20_enabled: value & 2 != 0,
                    reset_requested: value & 1 == 0,
                }))
            }
            Pending::None => Err(ControllerError::UnsupportedData(value)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn controller_output_port_reports_a20_and_reset_lines() {
        let mut c = Controller::new();
        assert_eq!(c.status() & 3, 0);
        c.write_command(0xAD).unwrap();
        c.write_command(0xD0).unwrap();
        assert_eq!(c.status() & 1, 1);
        assert_eq!(c.read_data(), 1);
        c.write_command(0xD1).unwrap();
        assert_eq!(
            c.write_data(3),
            Ok(Some(OutputLines {
                a20_enabled: true,
                reset_requested: false
            }))
        );
        c.set_a20_line(false);
        c.write_command(0xD0).unwrap();
        assert_eq!(c.read_data(), 1);
        c.write_command(0xD1).unwrap();
        assert_eq!(
            c.write_data(0),
            Ok(Some(OutputLines {
                a20_enabled: false,
                reset_requested: true
            }))
        );
    }
    #[test]
    fn command_byte_tests_and_reply_capacity_are_bounded() {
        let mut c = Controller::new();
        c.write_command(0x60).unwrap();
        c.write_data(0x12).unwrap();
        c.write_command(0x20).unwrap();
        assert_eq!(c.read_data(), 0x12);
        c.write_command(0xAA).unwrap();
        assert_eq!(c.read_data(), 0x55);
        for _ in 0..16 {
            c.write_command(0xAB).unwrap();
        }
        assert_eq!(c.write_command(0xAB), Err(ControllerError::OutputFull));
        assert_eq!(c.write_command(0xFF), Ok(()));
        assert_eq!(
            c.write_command(0xFE),
            Err(ControllerError::UnsupportedCommand(0xFE))
        );
    }
}
