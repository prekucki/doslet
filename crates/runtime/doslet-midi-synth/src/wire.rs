//! MIDI wire framing and replay of channel state at host output transitions.
//! These helpers never synthesize samples and never allocate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    InvalidStatus(u8),
    MessageTooLong,
    InterruptedMessage,
}
pub struct Decoder {
    bytes: [u8; 1024],
    used: usize,
    expected: usize,
    status: u8,
    system: bool,
    realtime: [u8; 1],
}
impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}
impl Decoder {
    pub const fn new() -> Self {
        Self {
            bytes: [0; 1024],
            used: 0,
            expected: 0,
            status: 0,
            system: false,
            realtime: [0],
        }
    }
    pub fn push(&mut self, byte: u8) -> Result<Option<&[u8]>, WireError> {
        if byte >= 0xF8 {
            if matches!(byte, 0xF9 | 0xFD) {
                return Err(WireError::InvalidStatus(byte));
            }
            self.realtime[0] = byte;
            if byte == 0xFF {
                self.status = 0;
                self.used = 0;
                self.system = false;
            }
            return Ok(Some(&self.realtime));
        }
        if self.system {
            if byte & 0x80 != 0 && byte != 0xF7 {
                return Err(WireError::InterruptedMessage);
            }
            if self.used == self.bytes.len() {
                return Err(WireError::MessageTooLong);
            }
            self.bytes[self.used] = byte;
            self.used += 1;
            if byte == 0xF7 {
                self.system = false;
                self.status = 0;
                return Ok(Some(&self.bytes[..self.used]));
            }
            return Ok(None);
        }
        if byte & 0x80 != 0 {
            self.status = byte;
            self.used = 1;
            self.bytes[0] = byte;
            self.expected = match byte {
                0x80..=0xBF | 0xE0..=0xEF | 0xF2 => 3,
                0xC0..=0xDF | 0xF1 | 0xF3 => 2,
                0xF0 => {
                    self.system = true;
                    return Ok(None);
                }
                0xF6 => {
                    self.status = 0;
                    return Ok(Some(&self.bytes[..1]));
                }
                _ => return Err(WireError::InvalidStatus(byte)),
            };
            return Ok(None);
        }
        if self.status == 0 {
            return Ok(None);
        }
        self.bytes[self.used] = byte;
        self.used += 1;
        if self.used != self.expected {
            return Ok(None);
        }
        self.used = 1;
        if self.status >= 0xF0 {
            self.status = 0;
        }
        Ok(Some(&self.bytes[..self.expected]))
    }
    pub fn pending(&self) -> &[u8] {
        if self.status == 0 {
            &[]
        } else {
            &self.bytes[..self.used]
        }
    }
}

const CC: [u8; 8] = [0, 32, 7, 10, 11, 64, 1, 91];
const DEFAULT_CC: [u8; 8] = [0, 0, 100, 64, 127, 0, 0, 0];
pub const REPLAY_CAPACITY: usize = 16384;
pub struct State {
    velocity: [[u8; 128]; 16],
    held: [[bool; 128]; 16],
    program: [u8; 16],
    controls: [[u8; 8]; 16],
    rpn: [[u8; 2]; 16],
    bend_range: [[u8; 2]; 16],
    bend: [u16; 16],
}
impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}
impl State {
    pub const fn new() -> Self {
        Self {
            velocity: [[0; 128]; 16],
            held: [[false; 128]; 16],
            program: [0; 16],
            controls: [DEFAULT_CC; 16],
            bend: [8192; 16],
            rpn: [[127; 2]; 16],
            bend_range: [[2, 0]; 16],
        }
    }
    pub fn receive(&mut self, message: &[u8]) {
        let Some(&status) = message.first() else {
            return;
        };
        if status == 0xFF {
            *self = Self::new();
            return;
        }
        if !(0x80..=0xEF).contains(&status) {
            return;
        }
        if message.len() < 2 || message[1] >= 128 || message.get(2).is_some_and(|b| *b >= 128) {
            return;
        }
        let c = usize::from(status & 15);
        let a = usize::from(message[1]);
        let b = message.get(2).copied().unwrap_or(0);
        match status & 0xF0 {
            0x90 if b != 0 => {
                self.velocity[c][a] = b;
                self.held[c][a] = true;
            }
            0x80 | 0x90 => {
                self.held[c][a] = false;
                if self.controls[c][5] < 64 {
                    self.velocity[c][a] = 0;
                }
            }
            0xC0 => self.program[c] = a as u8,
            0xE0 => self.bend[c] = a as u16 | (u16::from(b) << 7),
            0xB0 => {
                match a {
                    101 => self.rpn[c][0] = b,
                    100 => self.rpn[c][1] = b,
                    6 | 38 if self.rpn[c] == [0, 0] => self.bend_range[c][usize::from(a == 38)] = b,
                    _ => {}
                }
                if let Some(i) = CC.iter().position(|&cc| usize::from(cc) == a) {
                    self.controls[c][i] = b;
                }
                if a == 120 {
                    self.velocity[c].fill(0);
                    self.held[c].fill(false);
                }
                if a >= 123 {
                    self.held[c].fill(false);
                }
                if a == 121 {
                    self.controls[c][4] = 127;
                    self.controls[c][5] = 0;
                    self.bend[c] = 8192;
                    self.rpn[c] = [127; 2];
                }
                if self.controls[c][5] < 64 {
                    for n in 0..128 {
                        if !self.held[c][n] {
                            self.velocity[c][n] = 0;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    pub fn write(&self, output: &mut [u8; REPLAY_CAPACITY]) -> usize {
        let mut used = 0;
        for c in 0..16 {
            let channel = c as u8;
            let mut append = |bytes: &[u8]| {
                output[used..used + bytes.len()].copy_from_slice(bytes);
                used += bytes.len();
            };
            for (cc, value) in [
                (101, 0),
                (100, 0),
                (6, self.bend_range[c][0]),
                (38, self.bend_range[c][1]),
                (101, self.rpn[c][0]),
                (100, self.rpn[c][1]),
            ] {
                append(&[0xB0 | channel, cc, value]);
            }
            for (i, &cc) in CC.iter().enumerate() {
                append(&[0xB0 | channel, cc, self.controls[c][i]]);
            }
            append(&[0xC0 | channel, self.program[c]]);
            append(&[
                0xE0 | channel,
                (self.bend[c] & 127) as u8,
                (self.bend[c] >> 7) as u8,
            ]);
        }
        used += self.write_notes(&mut output[used..]);
        used
    }
    /// Active and sustain-held notes only, after restoring device parameters.
    pub fn write_notes(&self, output: &mut [u8]) -> usize {
        let mut used = 0;
        for c in 0..16 {
            for n in 0..128 {
                if self.velocity[c][n] != 0 {
                    output[used..used + 3].copy_from_slice(&[
                        0x90 | c as u8,
                        n as u8,
                        self.velocity[c][n],
                    ]);
                    used += 3;
                    if !self.held[c][n] {
                        output[used..used + 3].copy_from_slice(&[0x80 | c as u8, n as u8, 0]);
                        used += 3;
                    }
                }
            }
        }
        used
    }
    pub fn write_with_decoder(
        &self,
        output: &mut [u8; REPLAY_CAPACITY],
        decoder: &Decoder,
    ) -> usize {
        let used = self.write(output);
        let pending = decoder.pending();
        output[used..used + pending.len()].copy_from_slice(pending);
        used + pending.len()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replay_restores_rpn_bend_range_and_the_current_selector() {
        let mut state = State::new();
        for message in [
            [0xB0, 101, 0],
            [0xB0, 100, 0],
            [0xB0, 6, 12],
            [0xB0, 38, 25],
            [0xB0, 101, 127],
            [0xB0, 100, 127],
        ] {
            state.receive(&message);
        }
        let mut replay = [0; REPLAY_CAPACITY];
        let count = state.write(&mut replay);
        let mut restored = State::new();
        let mut decoder = Decoder::new();
        for &byte in &replay[..count] {
            if let Some(message) = decoder.push(byte).unwrap() {
                restored.receive(message);
            }
        }
        assert_eq!(restored.bend_range[0], [12, 25]);
        assert_eq!(restored.rpn[0], [127, 127]);
    }
    #[test]
    fn framing_restores_running_status_across_batches_and_realtime() {
        let mut d = Decoder::new();
        assert!(d.push(0x90).unwrap().is_none());
        assert!(d.push(60).unwrap().is_none());
        assert_eq!(d.push(0xF8).unwrap(), Some(&[0xF8][..]));
        assert_eq!(d.push(100).unwrap(), Some(&[0x90, 60, 100][..]));
        assert!(d.push(61).unwrap().is_none());
        assert_eq!(d.push(0).unwrap(), Some(&[0x90, 61, 0][..]));
        for byte in [0xF0, 0x41, 0x10] {
            assert!(d.push(byte).unwrap().is_none());
        }
        assert_eq!(d.push(0xF7).unwrap(), Some(&[0xF0, 0x41, 0x10, 0xF7][..]));
    }
    #[test]
    fn replay_keeps_program_controllers_and_sustained_released_keys() {
        let mut state = State::new();
        for message in [
            &[0xC0, 48][..],
            &[0xB0, 64, 127],
            &[0x90, 60, 100],
            &[0x80, 60, 0],
            &[0xB0, 7, 42],
        ] {
            state.receive(message);
        }
        let mut bytes = [0; REPLAY_CAPACITY];
        let len = state.write(&mut bytes);
        assert!(bytes[..len].windows(3).any(|b| b == [0x90, 60, 100]));
        assert!(bytes[..len].windows(3).any(|b| b == [0x80, 60, 0]));
        state.receive(&[0xB0, 64, 0]);
        let len = state.write(&mut bytes);
        assert!(!bytes[..len].windows(3).any(|b| b == [0x90, 60, 100]));
    }
    #[test]
    fn switching_outputs_restores_a_partial_running_status_message() {
        let state = State::new();
        let mut source = Decoder::new();
        source.push(0x90).unwrap();
        source.push(60).unwrap();
        let mut replay = [0; REPLAY_CAPACITY];
        let len = state.write_with_decoder(&mut replay, &source);
        let mut target = Decoder::new();
        for &byte in &replay[..len] {
            target.push(byte).unwrap();
        }
        assert_eq!(target.push(100).unwrap(), Some(&[0x90, 60, 100][..]));
    }
    #[test]
    fn oversized_sysex_stops_at_a_fixed_capacity() {
        let mut decoder = Decoder::new();
        decoder.push(0xF0).unwrap();
        for _ in 0..1023 {
            decoder.push(1).unwrap();
        }
        assert_eq!(decoder.push(0xF7), Err(WireError::MessageTooLong));
    }
}
