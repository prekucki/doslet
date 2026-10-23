//! Independent MPU-401 UART foundation. MIDI synthesis belongs to the host adapter.
#![no_std]

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MpuError {
    UnsupportedCommand(u8),
    UartRequired,
    TransmitBusy,
    OutputFull,
}

// MIDI serial framing: one start bit, eight data bits and one stop bit at
// 31,250 bits/s. This is guest hardware time, independent of host queue drains.
const BYTE_NANOS: u64 = 320_000;

struct Queue<const N: usize> {
    bytes: [u8; N],
    head: usize,
    len: usize,
}
impl<const N: usize> Queue<N> {
    const fn new() -> Self {
        assert!(N != 0);
        Self {
            bytes: [0; N],
            head: 0,
            len: 0,
        }
    }
    fn push(&mut self, byte: u8) -> bool {
        if self.len == N {
            return false;
        }
        self.bytes[(self.head + self.len) % N] = byte;
        self.len += 1;
        true
    }
    fn pop(&mut self) -> Option<u8> {
        if self.len == 0 {
            return None;
        }
        let byte = self.bytes[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(byte)
    }
}

pub struct Mpu401 {
    uart: bool,
    acknowledgement: Option<u8>,
    output: Queue<1024>,
    commands: u64,
    transmitted: u64,
    transmit_ready_nanos: u64,
}
impl Default for Mpu401 {
    fn default() -> Self {
        Self::new()
    }
}
impl Mpu401 {
    pub const fn new() -> Self {
        Self {
            uart: false,
            acknowledgement: None,
            output: Queue::new(),
            commands: 0,
            transmitted: 0,
            transmit_ready_nanos: 0,
        }
    }
    pub fn status(&self, nanos: u64) -> u8 {
        0x3F | if self.acknowledgement.is_none() {
            0x80
        } else {
            0
        } | if self.output.len == 1024 || nanos < self.transmit_ready_nanos {
            0x40
        } else {
            0
        }
    }
    /// Remaining wire time before the transmit-busy status bit clears.
    /// A full output queue has no clock-only deadline.
    pub fn transmit_ready_in_nanos(&self, nanos: u64) -> Option<u64> {
        (self.output.len < 1024).then(|| self.transmit_ready_nanos.saturating_sub(nanos))
    }
    pub fn pending_output(&self) -> usize {
        self.output.len
    }
    pub fn command_count(&self) -> u64 {
        self.commands
    }
    pub fn transmitted_bytes(&self) -> u64 {
        self.transmitted
    }
    pub fn uart_mode(&self) -> bool {
        self.uart
    }
    pub fn read_data(&mut self) -> u8 {
        self.acknowledgement.take().unwrap_or(0xFE)
    }
    pub fn write_command(&mut self, command: u8) -> Result<(), MpuError> {
        self.commands += 1;
        if self.uart && command != 0xFF {
            return Ok(());
        }
        match command {
            0xFF => {
                let was_uart = self.uart;
                self.uart = false;
                self.acknowledgement = if was_uart { None } else { Some(0xFE) };
            }
            0x3F => {
                self.uart = true;
                self.acknowledgement = Some(0xFE);
            }
            // The MPU accepts command-port writes even when this UART-only
            // implementation has no intelligent-mode behavior for them.
            // Acknowledge the command rather than turning device probing into
            // a fatal PC bus error.
            _ => self.acknowledgement = Some(0xFE),
        }
        Ok(())
    }
    /// Accept one byte and hold the write-ready bit busy for its wire time.
    /// Output delivery retains the frontend's bounded-batch timing; draining
    /// accepted bytes does not make the serial transmitter ready sooner.
    pub fn write_data(&mut self, byte: u8, nanos: u64) -> Result<(), MpuError> {
        if !self.uart {
            return Err(MpuError::UartRequired);
        }
        if nanos < self.transmit_ready_nanos {
            return Err(MpuError::TransmitBusy);
        }
        if !self.output.push(byte) {
            return Err(MpuError::OutputFull);
        }
        self.transmitted += 1;
        self.transmit_ready_nanos = nanos.saturating_add(BYTE_NANOS);
        Ok(())
    }
    /// Drain into caller storage in wire order, without allocations or dropping bytes.
    pub fn drain(&mut self, output: &mut [u8]) -> usize {
        let mut written = 0;
        for byte in output {
            let Some(value) = self.output.pop() else {
                break;
            };
            *byte = value;
            written += 1;
        }
        written
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reset_ack_uart_status_and_midi_order() {
        let mut mpu = Mpu401::new();
        assert_eq!(mpu.status(0), 0xBF);
        assert_eq!(mpu.write_data(0x90, 0), Err(MpuError::UartRequired));
        mpu.write_command(0xFF).unwrap();
        assert_eq!(mpu.status(0), 0x3F);
        assert_eq!(mpu.read_data(), 0xFE);
        assert_eq!(mpu.status(0), 0xBF);
        mpu.write_command(0x3F).unwrap();
        assert_eq!(mpu.read_data(), 0xFE);
        for (i, byte) in [0xC0, 40, 0x90, 60, 100].into_iter().enumerate() {
            mpu.write_data(byte, i as u64 * BYTE_NANOS).unwrap();
        }
        let mut output = [0; 8];
        assert_eq!(mpu.drain(&mut output), 5);
        assert_eq!(&output[..5], &[0xC0, 40, 0x90, 60, 100]);
        assert_eq!(mpu.drain(&mut output), 0);
    }
    #[test]
    fn capacity_and_wrap_preserve_bytes_and_reset_does_not_drop_transmitted_data() {
        let mut mpu = Mpu401::new();
        mpu.write_command(0x3F).unwrap();
        mpu.read_data();
        for index in 0..1024 {
            mpu.write_data(index as u8, index * BYTE_NANOS).unwrap();
        }
        assert_eq!(mpu.status(1024 * BYTE_NANOS), 0xFF);
        assert_eq!(
            mpu.write_data(1, 1024 * BYTE_NANOS),
            Err(MpuError::OutputFull)
        );
        let mut first = [0; 700];
        assert_eq!(mpu.drain(&mut first), 700);
        for (index, byte) in first.iter().enumerate() {
            assert_eq!(*byte, index as u8);
        }
        for index in 1024..1300 {
            mpu.write_data(index as u8, index * BYTE_NANOS).unwrap();
        }
        mpu.write_command(0xFF).unwrap();
        let mut rest = [0; 600];
        assert_eq!(mpu.drain(&mut rest), 600);
        for (index, byte) in rest.iter().enumerate() {
            assert_eq!(*byte, (index + 700) as u8);
        }
        assert_eq!(mpu.write_command(0xAC), Ok(()));
        assert_eq!(mpu.read_data(), 0xFE);
    }

    #[test]
    fn draining_and_reset_do_not_bypass_the_guest_serial_clock() {
        let mut mpu = Mpu401::new();
        mpu.write_command(0x3F).unwrap();
        // Receive-ready (bit 7) and transmit-busy (bit 6) are independent.
        mpu.write_data(0xB0, 1_000_000).unwrap();
        assert_eq!(mpu.status(1_000_000), 0x7F);
        assert_eq!(mpu.read_data(), 0xFE);
        assert_eq!(mpu.status(1_000_000), 0xFF);
        let mut bytes = [0; 3];
        assert_eq!(mpu.drain(&mut bytes), 1);
        assert_eq!(bytes[0], 0xB0);
        assert_eq!(mpu.status(1_319_999) & 0x40, 0x40);
        assert_eq!(mpu.transmit_ready_in_nanos(1_319_999), Some(1));
        assert_eq!(mpu.write_data(11, 1_319_999), Err(MpuError::TransmitBusy));
        assert_eq!(mpu.transmitted_bytes(), 1);
        assert_eq!(mpu.pending_output(), 0);
        assert_eq!(mpu.status(1_320_000) & 0x40, 0);
        assert_eq!(mpu.transmit_ready_in_nanos(1_320_000), Some(0));
        mpu.write_data(11, 1_320_000).unwrap();
        mpu.write_command(0xFF).unwrap();
        mpu.write_command(0x3F).unwrap();
        assert_eq!(mpu.status(1_320_000) & 0x40, 0x40);
        mpu.write_data(127, 1_640_000).unwrap();
        assert_eq!(mpu.drain(&mut bytes), 2);
        assert_eq!(&bytes[..2], &[11, 127]);
    }
}

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
