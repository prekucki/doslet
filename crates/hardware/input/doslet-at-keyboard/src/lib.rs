//! AT keyboard protocol, physical pressed state and guest-time typematic.
#![no_std]
use doslet_input::PhysicalKey;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyboardError {
    UnsupportedCommand(u8),
    UnsupportedKey(PhysicalKey),
}
#[derive(Clone, Copy, Default)]
enum Pending {
    #[default]
    None,
    Leds,
    Typematic,
    ScanSet,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bytes {
    pub bytes: [u8; 8],
    pub len: usize,
}
impl Bytes {
    pub const fn empty() -> Self {
        Self {
            bytes: [0; 8],
            len: 0,
        }
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
    fn push(&mut self, b: u8) {
        self.bytes[self.len] = b;
        self.len += 1;
    }
    fn from(bytes: &[u8]) -> Self {
        let mut out = Self::empty();
        for &b in bytes {
            out.push(b);
        }
        out
    }
}
#[derive(Clone)]
pub struct Keyboard {
    pending: Pending,
    leds: u8,
    typematic: u8,
    scan_set: u8,
    scanning: bool,
    last_reply: u8,
    pressed: [u64; 4],
    repeat: Option<PhysicalKey>,
    deadline: u64,
}
impl Default for Keyboard {
    fn default() -> Self {
        Self::new()
    }
}
impl Keyboard {
    pub const fn new() -> Self {
        Self {
            pending: Pending::None,
            leds: 0,
            typematic: 0x2B,
            scan_set: 2,
            scanning: true,
            last_reply: 0xFA,
            pressed: [0; 4],
            repeat: None,
            deadline: 0,
        }
    }
    pub fn led_state(&self) -> u8 {
        self.leds
    }
    pub fn scan_set(&self) -> u8 {
        self.scan_set
    }
    pub fn typematic(&self) -> u8 {
        self.typematic
    }
    pub fn scanning(&self) -> bool {
        self.scanning
    }
    pub fn set_last_reply(&mut self, value: u8) {
        self.last_reply = value;
    }
    fn defaults(&mut self) {
        self.leds = 0;
        self.typematic = 0x2B;
        self.scan_set = 2;
        self.pending = Pending::None;
        self.repeat = None;
    }
    pub fn command(&mut self, value: u8) -> Result<Bytes, KeyboardError> {
        let bytes = match self.pending {
            Pending::Leds => {
                self.leds = value & 7;
                self.pending = Pending::None;
                Bytes::from(&[0xFA])
            }
            Pending::Typematic => {
                self.typematic = value & 0x7F;
                self.pending = Pending::None;
                Bytes::from(&[0xFA])
            }
            Pending::ScanSet => match value {
                0 => {
                    self.pending = Pending::None;
                    Bytes::from(&[0xFA, self.scan_set])
                }
                1 | 2 => {
                    self.scan_set = value;
                    self.pending = Pending::None;
                    Bytes::from(&[0xFA])
                }
                _ => Bytes::from(&[0xFE]),
            },
            Pending::None => match value {
                0xED | 0xF0 | 0xF3 => {
                    self.pending = match value {
                        0xED => Pending::Leds,
                        0xF0 => Pending::ScanSet,
                        _ => Pending::Typematic,
                    };
                    Bytes::from(&[0xFA])
                }
                0xEE => Bytes::from(&[0xEE]),
                0xF2 => Bytes::from(&[0xFA, 0xAB, 0x83]),
                0xF4 => {
                    self.scanning = true;
                    Bytes::from(&[0xFA])
                }
                0xF5 | 0xF6 => {
                    self.defaults();
                    self.scanning = value == 0xF6;
                    Bytes::from(&[0xFA])
                }
                0xFE => Bytes::from(&[self.last_reply]),
                0xFF => {
                    self.defaults();
                    self.scanning = true;
                    self.pressed.fill(0);
                    Bytes::from(&[0xFA, 0xAA])
                }
                _ => return Err(KeyboardError::UnsupportedCommand(value)),
            },
        };
        Ok(bytes)
    }
    pub fn is_pressed(&self, key: PhysicalKey) -> bool {
        key.0 < 256 && self.pressed[key.0 as usize / 64] & (1 << (key.0 % 64)) != 0
    }
    pub fn key(
        &mut self,
        key: PhysicalKey,
        pressed: bool,
        translated: bool,
        nanos: u64,
    ) -> Result<Bytes, KeyboardError> {
        let sequence = self.encode(key, pressed, translated)?;
        if self.is_pressed(key) == pressed {
            return Ok(Bytes::empty());
        }
        let bit = 1u64 << (key.0 % 64);
        let slot = &mut self.pressed[key.0 as usize / 64];
        if pressed {
            *slot |= bit;
        } else {
            *slot &= !bit;
        }
        if pressed && !matches!(key.0, 0xE0..=0xE7 | 0x39 | 0x47 | 0x53 | 0x48) {
            self.repeat = Some(key);
            self.deadline =
                nanos.saturating_add(u64::from(1 + ((self.typematic >> 5) & 3)) * 250_000_000);
        }
        if !pressed && self.repeat == Some(key) {
            self.repeat = None;
        }
        Ok(if self.scanning {
            sequence
        } else {
            Bytes::empty()
        })
    }
    pub fn next_deadline(&self) -> Option<u64> {
        if self.scanning && self.repeat.is_some() {
            Some(self.deadline)
        } else {
            None
        }
    }
    /// Inhibited serial delivery retains pressed state but never accumulates
    /// typematic catch-up work for a later execution batch.
    pub fn inhibit_repeat(&mut self, nanos: u64) {
        if self.repeat.is_some() {
            self.deadline = nanos.saturating_add(self.repeat_period());
        }
    }
    fn repeat_period(&self) -> u64 {
        u64::from((8 + (self.typematic & 7)) as u32 * (1u32 << ((self.typematic >> 3) & 3)))
            * 1_000_000_000
            / 240
    }
    pub fn repeat(&mut self, translated: bool, nanos: u64) -> Result<Bytes, KeyboardError> {
        let Some(key) = self.repeat else {
            return Ok(Bytes::empty());
        };
        if !self.scanning || nanos < self.deadline {
            return Ok(Bytes::empty());
        }
        let period = self.repeat_period();
        self.deadline = self.deadline.saturating_add(period);
        self.encode(key, true, translated)
    }
    fn encode(
        &self,
        key: PhysicalKey,
        pressed: bool,
        translated: bool,
    ) -> Result<Bytes, KeyboardError> {
        let set1 = translated || self.scan_set == 1;
        if key.0 == 0x48 {
            return Ok(if !pressed {
                Bytes::empty()
            } else if set1 {
                Bytes::from(&[0xE1, 0x1D, 0x45, 0xE1, 0x9D, 0xC5])
            } else {
                Bytes::from(&[0xE1, 0x14, 0x77, 0xE1, 0xF0, 0x14, 0xF0, 0x77])
            });
        }
        if key.0 == 0x46 {
            return Ok(if set1 {
                if pressed {
                    Bytes::from(&[0xE0, 0x2A, 0xE0, 0x37])
                } else {
                    Bytes::from(&[0xE0, 0xB7, 0xE0, 0xAA])
                }
            } else if pressed {
                Bytes::from(&[0xE0, 0x12, 0xE0, 0x7C])
            } else {
                Bytes::from(&[0xE0, 0xF0, 0x7C, 0xE0, 0xF0, 0x12])
            });
        }
        let (one, two, extended) = codes(key).ok_or(KeyboardError::UnsupportedKey(key))?;
        let mut out = Bytes::empty();
        if extended {
            out.push(0xE0);
        }
        if set1 {
            out.push(one | if pressed { 0 } else { 0x80 });
        } else {
            if !pressed {
                out.push(0xF0);
            }
            out.push(two);
        }
        Ok(out)
    }
}
fn codes(key: PhysicalKey) -> Option<(u8, u8, bool)> {
    const LETTERS: [(u8, u8); 26] = [
        (0x1E, 0x1C),
        (0x30, 0x32),
        (0x2E, 0x21),
        (0x20, 0x23),
        (0x12, 0x24),
        (0x21, 0x2B),
        (0x22, 0x34),
        (0x23, 0x33),
        (0x17, 0x43),
        (0x24, 0x3B),
        (0x25, 0x42),
        (0x26, 0x4B),
        (0x32, 0x3A),
        (0x31, 0x31),
        (0x18, 0x44),
        (0x19, 0x4D),
        (0x10, 0x15),
        (0x13, 0x2D),
        (0x1F, 0x1B),
        (0x14, 0x2C),
        (0x16, 0x3C),
        (0x2F, 0x2A),
        (0x11, 0x1D),
        (0x2D, 0x22),
        (0x15, 0x35),
        (0x2C, 0x1A),
    ];
    if (4..=29).contains(&key.0) {
        let (a, b) = LETTERS[(key.0 - 4) as usize];
        return Some((a, b, false));
    }
    const DIGITS: [u8; 10] = [0x16, 0x1E, 0x26, 0x25, 0x2E, 0x36, 0x3D, 0x3E, 0x46, 0x45];
    if (30..=39).contains(&key.0) {
        let i = (key.0 - 30) as usize;
        return Some((2 + i as u8, DIGITS[i], false));
    }
    let (a, b, e) = match key.0 {
        40 => (0x1C, 0x5A, false),
        41 => (1, 0x76, false),
        42 => (0x0E, 0x66, false),
        43 => (0x0F, 0x0D, false),
        44 => (0x39, 0x29, false),
        45 => (0x0C, 0x4E, false),
        46 => (0x0D, 0x55, false),
        47 => (0x1A, 0x54, false),
        48 => (0x1B, 0x5B, false),
        49 | 50 => (0x2B, 0x5D, false),
        51 => (0x27, 0x4C, false),
        52 => (0x28, 0x52, false),
        53 => (0x29, 0x0E, false),
        54 => (0x33, 0x41, false),
        55 => (0x34, 0x49, false),
        56 => (0x35, 0x4A, false),
        57 => (0x3A, 0x58, false),
        58 => (0x3B, 0x05, false),
        59 => (0x3C, 0x06, false),
        60 => (0x3D, 0x04, false),
        61 => (0x3E, 0x0C, false),
        62 => (0x3F, 0x03, false),
        63 => (0x40, 0x0B, false),
        64 => (0x41, 0x83, false),
        65 => (0x42, 0x0A, false),
        66 => (0x43, 0x01, false),
        67 => (0x44, 0x09, false),
        68 => (0x57, 0x78, false),
        69 => (0x58, 0x07, false),
        71 => (0x46, 0x7E, false),
        73 => (0x52, 0x70, true),
        74 => (0x47, 0x6C, true),
        75 => (0x49, 0x7D, true),
        76 => (0x53, 0x71, true),
        77 => (0x4F, 0x69, true),
        78 => (0x51, 0x7A, true),
        79 => (0x4D, 0x74, true),
        80 => (0x4B, 0x6B, true),
        81 => (0x50, 0x72, true),
        82 => (0x48, 0x75, true),
        83 => (0x45, 0x77, false),
        84 => (0x35, 0x4A, true),
        85 => (0x37, 0x7C, false),
        86 => (0x4A, 0x7B, false),
        87 => (0x4E, 0x79, false),
        88 => (0x1C, 0x5A, true),
        89 => (0x4F, 0x69, false),
        90 => (0x50, 0x72, false),
        91 => (0x51, 0x7A, false),
        92 => (0x4B, 0x6B, false),
        93 => (0x4C, 0x73, false),
        94 => (0x4D, 0x74, false),
        95 => (0x47, 0x6C, false),
        96 => (0x48, 0x75, false),
        97 => (0x49, 0x7D, false),
        98 => (0x52, 0x70, false),
        99 => (0x53, 0x71, false),
        100 => (0x56, 0x61, false),
        101 => (0x5D, 0x2F, true),
        0xE0 => (0x1D, 0x14, false),
        0xE1 => (0x2A, 0x12, false),
        0xE2 => (0x38, 0x11, false),
        0xE3 => (0x5B, 0x1F, true),
        0xE4 => (0x1D, 0x14, true),
        0xE5 => (0x36, 0x59, false),
        0xE6 => (0x38, 0x11, true),
        0xE7 => (0x5C, 0x27, true),
        _ => return None,
    };
    Some((a, b, e))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scan_sets_breaks_and_guest_repeat() {
        let mut k = Keyboard::new();
        assert_eq!(
            k.key(PhysicalKey(4), true, false, 0).unwrap().as_slice(),
            &[0x1C]
        );
        assert_eq!(k.repeat(false, 499_999_999).unwrap().len, 0);
        assert_eq!(k.repeat(false, 500_000_000).unwrap().as_slice(), &[0x1C]);
        assert_eq!(
            k.key(PhysicalKey(4), false, false, 500_000_001)
                .unwrap()
                .as_slice(),
            &[0xF0, 0x1C]
        );
        assert_eq!(
            k.key(PhysicalKey(79), true, true, 0).unwrap().as_slice(),
            &[0xE0, 0x4D]
        );
        assert_eq!(
            k.key(PhysicalKey(79), false, true, 0).unwrap().as_slice(),
            &[0xE0, 0xCD]
        );
    }
    #[test]
    fn unsupported_set_three_is_resend_without_mutation() {
        let mut k = Keyboard::new();
        k.command(0xF0).unwrap();
        assert_eq!(k.command(3).unwrap().as_slice(), &[0xFE]);
        assert_eq!(k.scan_set(), 2);
        assert_eq!(k.command(1).unwrap().as_slice(), &[0xFA]);
        assert_eq!(k.scan_set(), 1);
    }
}

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
