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

#[derive(Clone, Copy, Default)]
enum KeyboardPending {
    #[default]
    None,
    Leds,
    Typematic,
    ScanSet,
}

pub struct Controller {
    command_byte: u8,
    output_port: u8,
    pending: Pending,
    last_command: bool,
    keyboard_pending: KeyboardPending,
    leds: u8,
    typematic: u8,
    scan_set: u8,
    scanning: bool,
    last_reply: u8,
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
            keyboard_pending: KeyboardPending::None,
            leds: 0,
            typematic: 0x2B,
            scan_set: 2,
            scanning: true,
            last_reply: 0xFA,
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
        self.last_reply = byte;
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
    fn replies(&mut self, bytes: &[u8]) -> Result<(), ControllerError> {
        if bytes.len() > self.output.len() - self.len {
            return Err(ControllerError::OutputFull);
        }
        for &byte in bytes {
            self.reply(byte)?;
        }
        Ok(())
    }
    pub fn led_state(&self) -> u8 {
        self.leds
    }
    fn keyboard_defaults(&mut self) {
        self.leds = 0;
        self.typematic = 0x2B;
        self.scan_set = 2;
        self.keyboard_pending = KeyboardPending::None;
    }
    fn keyboard_data(&mut self, value: u8) -> Result<(), ControllerError> {
        match self.keyboard_pending {
            KeyboardPending::Leds => {
                self.reply(0xFA)?;
                self.leds = value & 7;
                self.keyboard_pending = KeyboardPending::None;
            }
            KeyboardPending::Typematic => {
                self.reply(0xFA)?;
                self.typematic = value & 0x7F;
                self.keyboard_pending = KeyboardPending::None;
            }
            KeyboardPending::ScanSet => {
                match value {
                    0 => self.replies(&[0xFA, self.scan_set])?,
                    1..=3 => {
                        self.reply(0xFA)?;
                        self.scan_set = value;
                    }
                    _ => {
                        self.reply(0xFE)?;
                        return Ok(());
                    }
                }
                self.keyboard_pending = KeyboardPending::None;
            }
            KeyboardPending::None => match value {
                0xED | 0xF0 | 0xF3 => {
                    self.reply(0xFA)?;
                    self.keyboard_pending = match value {
                        0xED => KeyboardPending::Leds,
                        0xF0 => KeyboardPending::ScanSet,
                        _ => KeyboardPending::Typematic,
                    };
                }
                0xEE => self.reply(0xEE)?,
                0xF2 => self.replies(&[0xFA, 0xAB, 0x83])?,
                0xF4 => {
                    self.reply(0xFA)?;
                    self.scanning = true;
                }
                0xF5 | 0xF6 => {
                    self.reply(0xFA)?;
                    self.keyboard_defaults();
                    self.scanning = value == 0xF6;
                }
                0xFE => self.reply(self.last_reply)?,
                0xFF => {
                    self.replies(&[0xFA, 0xAA])?;
                    self.keyboard_defaults();
                    self.scanning = true;
                }
                _ => return Err(ControllerError::UnsupportedData(value)),
            },
        }
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
            Pending::None => {
                self.keyboard_data(value)?;
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keyboard_commands_acknowledge_parameters_without_changing_a20() {
        let mut c = Controller::new();
        for byte in [0xED, 7, 0xF3, 0x20] {
            assert_eq!(c.write_data(byte), Ok(None));
            assert_eq!(c.read_data(), 0xFA);
        }
        assert_eq!(c.led_state(), 7);
        assert_eq!(c.typematic, 0x20);
        c.write_command(0xD0).unwrap();
        assert_eq!(c.read_data(), 1);
        c.write_data(0xF2).unwrap();
        assert_eq!(
            [c.read_data(), c.read_data(), c.read_data()],
            [0xFA, 0xAB, 0x83]
        );
        c.write_data(0xFF).unwrap();
        assert_eq!([c.read_data(), c.read_data()], [0xFA, 0xAA]);
        assert_eq!(c.led_state(), 0);
        assert!(c.scanning);
        assert_eq!(
            c.write_data(0x12),
            Err(ControllerError::UnsupportedData(0x12))
        );
    }
    #[test]
    fn multibyte_keyboard_replies_do_not_publish_partial_responses_when_full() {
        let mut c = Controller::new();
        for _ in 0..15 {
            c.write_command(0xAB).unwrap();
        }
        assert_eq!(c.write_data(0xF2), Err(ControllerError::OutputFull));
        for _ in 0..15 {
            assert_eq!(c.read_data(), 0);
        }
        assert_eq!(c.status() & 1, 0);
        c.write_data(0xF0).unwrap();
        assert_eq!(c.read_data(), 0xFA);
        c.write_data(0).unwrap();
        assert_eq!([c.read_data(), c.read_data()], [0xFA, 2]);
        c.write_data(0xF0).unwrap();
        assert_eq!(c.read_data(), 0xFA);
        c.write_data(9).unwrap();
        assert_eq!(c.read_data(), 0xFE);
        c.write_data(1).unwrap();
        assert_eq!(c.read_data(), 0xFA);
        assert_eq!(c.scan_set, 1);
    }
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
