//! MT-32 parameter memory addressed by Roland DT1 SysEx. It holds only values
//! sent by the guest and documented power-on defaults; ROM timbres stay absent.

/// Packs a three-byte 7-bit SysEx address into a linear byte offset.
pub const fn memaddr(high: u8, middle: u8, low: u8) -> u32 {
    (high as u32) << 14 | (middle as u32) << 7 | low as u32
}

pub const PATCH_TEMP: u32 = memaddr(0x03, 0x00, 0x00);
pub const RHYTHM_TEMP: u32 = memaddr(0x03, 0x01, 0x10);
pub const TIMBRE_TEMP: u32 = memaddr(0x04, 0x00, 0x00);
pub const PATCHES: u32 = memaddr(0x05, 0x00, 0x00);
pub const TIMBRES: u32 = memaddr(0x08, 0x00, 0x00);
pub const SYSTEM: u32 = memaddr(0x10, 0x00, 0x00);
pub const DISPLAY: u32 = memaddr(0x20, 0x00, 0x00);

/// Common parameters plus four partials; memory slots pad this to 256 bytes.
pub const TIMBRE_SIZE: usize = 246;
/// Rhythm setup covers keys 24..=108 (MT-32 hardware uses 24..=87).
pub const RHYTHM_KEYS: usize = 85;
pub const PARTS: usize = 9;
pub const RHYTHM_PART: usize = 8;

/// System area offsets.
pub const REVERB_MODE: usize = 1;
pub const REVERB_LEVEL: usize = 3;
pub const CHANNEL_ASSIGN: usize = 13;
pub const MASTER_VOLUME: usize = 22;

/// Patch (and patch-temp) byte offsets.
pub const TIMBRE_GROUP: usize = 0;
pub const TIMBRE_NUMBER: usize = 1;
pub const KEY_SHIFT: usize = 2;
pub const BENDER_RANGE: usize = 4;
pub const REVERB_SWITCH: usize = 6;
pub const OUTPUT_LEVEL: usize = 8;
pub const PANPOT: usize = 9;

/// Which state a write touched. Bit sets index parts, memory timbres and keys.
#[derive(Clone, Copy, Default)]
pub struct Changes {
    pub patch_temp: u16,
    pub patch_timbre: u16,
    pub timbre_temp: u8,
    pub timbre_temp_whole: u8,
    pub timbres: u64,
    pub rhythm_timbre: u128,
    pub system: u32,
    pub display: bool,
    pub ignored: bool,
}
impl Changes {
    pub fn merge(&mut self, other: &Self) {
        self.patch_temp |= other.patch_temp;
        self.patch_timbre |= other.patch_timbre;
        self.timbre_temp |= other.timbre_temp;
        self.timbre_temp_whole |= other.timbre_temp_whole;
        self.timbres |= other.timbres;
        self.rhythm_timbre |= other.rhythm_timbre;
        self.system |= other.system;
        self.display |= other.display;
        self.ignored |= other.ignored;
    }
}

const REGIONS: [(u32, usize); 7] = [
    (SYSTEM, 23),
    (TIMBRES, 64 * 256),
    (PATCHES, 128 * 8),
    (PATCH_TEMP, PARTS * 16),
    (RHYTHM_TEMP, RHYTHM_KEYS * 4),
    (TIMBRE_TEMP, 8 * TIMBRE_SIZE),
    (DISPLAY, 20),
];

pub struct Memory {
    pub patch_temp: [[u8; 16]; PARTS],
    pub rhythm_temp: [[u8; 4]; RHYTHM_KEYS],
    pub timbre_temp: [[u8; TIMBRE_SIZE]; 8],
    pub patches: [[u8; 8]; 128],
    pub timbres: [[u8; 256]; 64],
    pub system: [u8; 23],
    pub display: [u8; 20],
    written: [u64; 384],
}
impl Memory {
    /// Power-on state. Part programs and pans come from the caller because the
    /// real values live in the control ROM.
    pub fn new(programs: &[u8; 8]) -> Self {
        let mut memory = Self {
            patch_temp: [[0; 16]; PARTS],
            rhythm_temp: [[0, 100, 7, 1]; RHYTHM_KEYS],
            timbre_temp: [[0; TIMBRE_SIZE]; 8],
            patches: [[0; 8]; 128],
            timbres: [[0; 256]; 64],
            // Room reverb (time 5, level 3), parts on channels 2..9, rhythm on
            // 10, master volume 100. Translation reads neither tune nor reserve.
            system: [
                0x4A, 0, 5, 3, 3, 10, 6, 4, 3, 0, 0, 0, 6, 1, 2, 3, 4, 5, 6, 7, 8, 9, 100,
            ],
            display: [b' '; 20],
            written: [0; 384],
        };
        for (program, patch) in memory.patches.iter_mut().enumerate() {
            *patch = [program as u8 / 64, program as u8 % 64, 24, 50, 12, 0, 1, 0];
        }
        for (part, temp) in memory.patch_temp.iter_mut().enumerate() {
            if part < 8 {
                temp[..8].copy_from_slice(&memory.patches[usize::from(programs[part] & 127)]);
            }
            temp[OUTPUT_LEVEL] = 80;
            temp[PANPOT] = 7;
        }
        memory
    }
    /// Tracks guest writes and channel-message side effects; never ROM bytes.
    pub(crate) fn mark_written(&mut self, start: u32, length: usize, written: bool) {
        for address in start..start + length as u32 {
            let mut base = 0;
            for (region, size) in REGIONS {
                if let Some(offset) = offset(address, region, size) {
                    let index = base + offset;
                    let bit = 1u64 << (index % 64);
                    if written {
                        self.written[index / 64] |= bit;
                    } else {
                        self.written[index / 64] &= !bit;
                    }
                    break;
                }
                base += size;
            }
        }
    }
    pub(crate) fn copy_written(&mut self, source: u32, destination: u32, length: usize) {
        for i in 0..length {
            let address = source + i as u32;
            let mut base = 0;
            let mut written = false;
            for (region, size) in REGIONS {
                if let Some(offset) = offset(address, region, size) {
                    let index = base + offset;
                    written = self.written[index / 64] & (1 << (index % 64)) != 0;
                    break;
                }
                base += size;
            }
            self.mark_written(destination + i as u32, 1, written);
        }
    }
    /// Emits only written runs, in bounded DT1 packets with valid checksums.
    /// System/channel assignments and memory timbres precede temporary state.
    pub fn replay<F: FnMut(&[u8])>(&self, emit: &mut F) {
        let mut compact = 0;
        let mut packet = [0u8; 267];
        for (region, size) in REGIONS {
            let mut offset = 0;
            while offset < size {
                let index = compact + offset;
                if self.written[index / 64] & (1 << (index % 64)) == 0 {
                    offset += 1;
                    continue;
                }
                let start = offset;
                let mut count = 0;
                while offset < size && count < 256 {
                    let index = compact + offset;
                    if self.written[index / 64] & (1 << (index % 64)) == 0 {
                        break;
                    }
                    packet[8 + count] = self.byte_at(region, offset);
                    count += 1;
                    offset += 1;
                }
                let address = region + start as u32;
                packet[..8].copy_from_slice(&[
                    0xF0,
                    0x41,
                    0x10,
                    0x16,
                    0x12,
                    ((address >> 14) & 127) as u8,
                    ((address >> 7) & 127) as u8,
                    (address & 127) as u8,
                ]);
                let sum = packet[5..8 + count]
                    .iter()
                    .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
                packet[8 + count] = 0u8.wrapping_sub(sum) & 127;
                packet[9 + count] = 0xF7;
                emit(&packet[..10 + count]);
            }
            compact += size;
        }
    }
    fn byte_at(&self, region: u32, offset: usize) -> u8 {
        match region {
            SYSTEM => self.system[offset],
            TIMBRES => self.timbres[offset / 256][offset % 256],
            PATCHES => self.patches[offset / 8][offset % 8],
            PATCH_TEMP => self.patch_temp[offset / 16][offset % 16],
            RHYTHM_TEMP => self.rhythm_temp[offset / 4][offset % 4],
            TIMBRE_TEMP => self.timbre_temp[offset / TIMBRE_SIZE][offset % TIMBRE_SIZE],
            DISPLAY => self.display[offset],
            _ => unreachable!(),
        }
    }
    /// Writes guest bytes from linear address `start`. Unknown addresses are
    /// skipped and reported, as the device ignores them.
    pub fn write(&mut self, start: u32, data: &[u8]) -> Changes {
        self.mark_written(start, data.len(), true);
        let mut changes = Changes::default();
        for (i, &byte) in data.iter().enumerate() {
            let address = start + i as u32;
            if let Some(o) = offset(address, PATCH_TEMP, PARTS * 16) {
                self.patch_temp[o / 16][o % 16] = byte;
                changes.patch_temp |= 1 << (o / 16);
                if o % 16 <= TIMBRE_NUMBER {
                    changes.patch_timbre |= 1 << (o / 16);
                }
            } else if let Some(o) = offset(address, RHYTHM_TEMP, RHYTHM_KEYS * 4) {
                self.rhythm_temp[o / 4][o % 4] = byte;
                if o % 4 == 0 {
                    changes.rhythm_timbre |= 1 << (o / 4);
                }
            } else if let Some(o) = offset(address, TIMBRE_TEMP, 8 * TIMBRE_SIZE) {
                self.timbre_temp[o / TIMBRE_SIZE][o % TIMBRE_SIZE] = byte;
                changes.timbre_temp |= 1 << (o / TIMBRE_SIZE);
            } else if let Some(o) = offset(address, PATCHES, 128 * 8) {
                self.patches[o / 8][o % 8] = byte;
            } else if let Some(o) = offset(address, TIMBRES, 64 * 256) {
                self.timbres[o / 256][o % 256] = byte;
                changes.timbres |= 1 << (o / 256);
            } else if let Some(o) = offset(address, SYSTEM, 23) {
                self.system[o] = byte;
                changes.system |= 1 << o;
            } else if let Some(o) = offset(address, DISPLAY, 20) {
                self.display[o] = byte;
                changes.display = true;
            } else {
                changes.ignored = true;
            }
        }
        let end = start as usize + data.len();
        for part in 0..8 {
            let base = TIMBRE_TEMP as usize + part * TIMBRE_SIZE;
            if start as usize <= base && end >= base + TIMBRE_SIZE {
                changes.timbre_temp_whole |= 1 << part;
            }
        }
        changes
    }
    /// MIDI channel (0..=15) a part receives on, or None when it is off.
    pub fn channel(&self, part: usize) -> Option<u8> {
        let channel = self.system[CHANNEL_ASSIGN + part];
        (channel < 16).then_some(channel)
    }
}
fn offset(address: u32, base: u32, len: usize) -> Option<usize> {
    let o = address.checked_sub(base)? as usize;
    (o < len).then_some(o)
}
