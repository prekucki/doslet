//! Bounded AT keyboard-controller command/output-port foundation.
#![no_std]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControllerError {
    UnsupportedCommand(u8),
    UnsupportedData(u8),
    OutputFull,
    UnsupportedKey(doslet_input::PhysicalKey),
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
    protocol: doslet_at_keyboard::Keyboard,
    nanos: u64,
    output: [u8; 512],
    head: usize,
    len: usize,
    releases: [u8; 512],
    release_head: usize,
    release_len: usize,
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
            protocol: doslet_at_keyboard::Keyboard::new(),
            nanos: 0,
            output: [0; 512],
            head: 0,
            len: 0,
            releases: [0; 512],
            release_head: 0,
            release_len: 0,
        }
    }
    pub fn status(&self) -> u8 {
        4 | u8::from(self.has_output()) | (u8::from(self.last_command) << 3)
    }
    pub fn read_data(&mut self) -> u8 {
        if self.release_len != 0 {
            let byte = self.releases[self.release_head];
            self.release_head = (self.release_head + 1) % self.releases.len();
            self.release_len -= 1;
            self.protocol.set_last_reply(byte);
            return byte;
        }
        if self.len == 0 {
            return 0xFF;
        }
        let byte = self.output[self.head];
        self.head = (self.head + 1) % self.output.len();
        self.len -= 1;
        self.protocol.set_last_reply(byte);
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
        self.protocol.led_state()
    }
    fn keyboard_data(&mut self, value: u8) -> Result<(), ControllerError> {
        let mut protocol = self.protocol.clone();
        let bytes = protocol
            .command(value)
            .map_err(|_| ControllerError::UnsupportedData(value))?;
        self.replies(bytes.as_slice())?;
        self.protocol = protocol;
        Ok(())
    }
    pub fn irq_pending(&self) -> bool {
        self.has_output() && self.command_byte & 0x11 == 1
    }
    pub fn has_output(&self) -> bool {
        self.len != 0 || self.release_len != 0
    }
    pub fn next_deadline(&self) -> Option<u64> {
        if self.command_byte & 0x10 == 0 {
            self.protocol.next_deadline()
        } else {
            None
        }
    }
    pub fn key_event(
        &mut self,
        key: doslet_input::PhysicalKey,
        pressed: bool,
    ) -> Result<(), ControllerError> {
        // A disabled serial interface retains bounded key transitions until
        // enabled, including releases of keys held before inhibition.
        let mut protocol = self.protocol.clone();
        let bytes = protocol
            .key(key, pressed, self.command_byte & 0x40 != 0, self.nanos)
            .map_err(|_| ControllerError::UnsupportedKey(key))?;
        self.replies(bytes.as_slice())?;
        self.protocol = protocol;
        Ok(())
    }
    pub fn release_keys(&mut self) -> Result<(), ControllerError> {
        // Discard ordinary queued input, preserving releases already awaiting
        // delivery. Repeated pause/focus events cannot erase prior break codes.
        self.head = 0;
        self.len = 0;
        for usage in 0..256 {
            let key = doslet_input::PhysicalKey(usage);
            if self.protocol.is_pressed(key) {
                let mut protocol = self.protocol.clone();
                let bytes = protocol
                    .key(key, false, self.command_byte & 0x40 != 0, self.nanos)
                    .map_err(|_| ControllerError::UnsupportedKey(key))?;
                if bytes.len > self.releases.len() - self.release_len {
                    return Err(ControllerError::OutputFull);
                }
                for &byte in bytes.as_slice() {
                    self.releases[(self.release_head + self.release_len) % self.releases.len()] =
                        byte;
                    self.release_len += 1;
                }
                self.protocol = protocol;
            }
        }
        Ok(())
    }
    pub fn advance_time(&mut self, nanos: u64) -> Result<(), ControllerError> {
        self.nanos = nanos;
        if self.command_byte & 0x10 != 0 {
            self.protocol.inhibit_repeat(nanos);
            return Ok(());
        }
        if self
            .protocol
            .next_deadline()
            .is_none_or(|deadline| deadline > nanos)
        {
            return Ok(());
        }
        let mut protocol = self.protocol.clone();
        let bytes = protocol
            .repeat(self.command_byte & 0x40 != 0, nanos)
            .map_err(|_| ControllerError::UnsupportedData(0))?;
        self.replies(bytes.as_slice())?;
        self.protocol = protocol;
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
    fn repeated_focus_releases_preserve_pending_breaks_and_discard_pending_makes() {
        let mut c = Controller::new();
        c.key_event(doslet_input::PhysicalKey(0xE1), true).unwrap();
        assert_eq!(c.read_data(), 0x2A);
        c.release_keys().unwrap();
        c.release_keys().unwrap();
        c.release_keys().unwrap();
        assert_eq!(c.read_data(), 0xAA);
        assert!(!c.has_output());
        c.key_event(doslet_input::PhysicalKey(4), true).unwrap();
        c.release_keys().unwrap();
        c.release_keys().unwrap();
        assert_eq!(c.read_data(), 0x9E);
        assert!(!c.has_output());
    }
    #[test]
    fn inhibited_interface_retains_release_and_never_replays_typematic_backlog() {
        let mut c = Controller::new();
        let key = doslet_input::PhysicalKey(4);
        c.key_event(key, true).unwrap();
        assert_eq!(c.read_data(), 0x1E);
        c.write_command(0xAD).unwrap();
        c.key_event(key, false).unwrap();
        assert!(!c.irq_pending());
        c.advance_time(10_000_000_000).unwrap();
        c.write_command(0xAE).unwrap();
        assert!(c.irq_pending());
        assert_eq!(c.read_data(), 0x9E);
        assert_eq!(c.next_deadline(), None);
        c.key_event(key, true).unwrap();
        c.read_data();
        c.write_command(0xAD).unwrap();
        c.advance_time(20_000_000_000).unwrap();
        c.write_command(0xAE).unwrap();
        c.advance_time(20_000_000_001).unwrap();
        assert!(!c.has_output());
    }
    #[test]
    fn keyboard_commands_acknowledge_parameters_without_changing_a20() {
        let mut c = Controller::new();
        for byte in [0xED, 7, 0xF3, 0x20] {
            assert_eq!(c.write_data(byte), Ok(None));
            assert_eq!(c.read_data(), 0xFA);
        }
        assert_eq!(c.led_state(), 7);
        assert_eq!(c.protocol.typematic(), 0x20);
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
        assert!(c.protocol.scanning());
        assert_eq!(
            c.write_data(0x12),
            Err(ControllerError::UnsupportedData(0x12))
        );
    }
    #[test]
    fn multibyte_keyboard_replies_do_not_publish_partial_responses_when_full() {
        let mut c = Controller::new();
        for _ in 0..511 {
            c.write_command(0xAB).unwrap();
        }
        assert_eq!(c.write_data(0xF2), Err(ControllerError::OutputFull));
        for _ in 0..511 {
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
        assert_eq!(c.protocol.scan_set(), 1);
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
        for _ in 0..512 {
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

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
