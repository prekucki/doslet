//! MT-32 MIDI dialect to General MIDI, without Roland ROM data. The translator
//! keeps the SysEx-addressed MT-32 state and emits GM channel messages only.
//! It approximates instruments with GM programs; it is not LA synthesis.
#![no_std]
pub mod memory;
pub mod tables;
mod timbre;

use memory::{Changes, Memory, PARTS, RHYTHM_PART, TIMBRE_SIZE};

/// GM rendering of an MT-32 timbre.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Program(u8),
    /// A fixed GM drum-kit note on channel 10, whatever key is played.
    Drum(u8),
    Silent,
}

/// Counters for behavior the translation drops or approximates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub sysex: u64,
    pub ignored_sysex: u64,
    pub checksum_errors: u64,
    pub unmapped_notes: u64,
    pub guest_timbre_loads: u64,
}

const DRUM_CHANNEL: u8 = 9;
const UNSENT: u8 = 0xFF;
/// Slots of the last values sent per GM channel; refreshes emit differences.
const PROGRAM: usize = 0;
const BEND_RANGE: usize = 1;
const VOLUME: usize = 2;
const PAN: usize = 3;
const REVERB: usize = 4;

#[derive(Clone, Copy)]
struct Held {
    channel: u8,
    note: u8,
}
const NOT_HELD: Held = Held {
    channel: UNSENT,
    note: 0,
};

#[derive(Clone, Copy)]
struct Part {
    target: Target,
    transpose: i8,
    /// Level trim in dB for the playing ROM preset; guest timbres use 0.
    trim: i8,
    /// Timbre-temp bytes describe the playing timbre (guest data, not a ROM preset).
    guest_timbre: bool,
    rpn: [u8; 2],
    held: [Held; 128],
}
const PART: Part = Part {
    target: Target::Silent,
    transpose: 0,
    trim: 0,
    guest_timbre: false,
    rpn: [127, 127],
    held: [NOT_HELD; 128],
};

/// Translates complete MIDI messages (as framed by a wire decoder) from the
/// MT-32 dialect to GM. Parts 1..=8 play on GM channels 1..=8, rhythm on 10.
/// Translation is allocation-free; state is fixed at about 23 KiB.
pub struct Translator {
    memory: Memory,
    parts: [Part; PARTS],
    /// Keys whose rhythm-setup timbre the guest has written since reset.
    rhythm_written: u128,
    sent: [[u8; 5]; 16],
    setup_pending: bool,
    stats: Stats,
}
impl Default for Translator {
    fn default() -> Self {
        Self::new()
    }
}
impl Translator {
    pub fn new() -> Self {
        let mut translator = Self {
            memory: Memory::new(&tables::POWER_ON_PROGRAMS),
            parts: [PART; PARTS],
            rhythm_written: 0,
            sent: [[UNSENT; 5]; 16],
            setup_pending: true,
            stats: Stats::default(),
        };
        for part in 0..8 {
            translator.resolve(part);
        }
        translator
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }
    /// Text last written to the module's 20-character display.
    pub fn display(&self) -> &[u8; 20] {
        &self.memory.display
    }
    /// GM target currently used by melodic part `part` (0..=7).
    pub fn part_target(&self, part: usize) -> Target {
        self.parts[part].target
    }
    /// Read-only view of the MT-32 parameter memory written so far.
    pub fn memory(&self) -> &Memory {
        &self.memory
    }
    /// Re-sends the complete GM channel setup, e.g. to a newly selected output.
    /// Held notes are not replayed.
    pub fn setup<F: FnMut(&[u8])>(&mut self, emit: &mut F) {
        self.sent = [[UNSENT; 5]; 16];
        self.setup_pending = false;
        for part in 0..PARTS {
            self.refresh(part, emit);
        }
    }
    /// Translates one complete message. Realtime bytes are dropped: the MT-32
    /// does not reset on FFh and GM outputs need no clock.
    pub fn translate<F: FnMut(&[u8])>(&mut self, message: &[u8], emit: &mut F) {
        if self.setup_pending {
            self.setup(emit);
        }
        let Some(&status) = message.first() else {
            return;
        };
        if status == 0xF0 {
            self.sysex(message, emit);
            return;
        }
        if !(0x80..0xF0).contains(&status) || message[1..].iter().any(|b| *b >= 0x80) {
            return;
        }
        let channel = status & 15;
        for part in 0..PARTS {
            if self.memory.channel(part) == Some(channel) {
                self.channel_message(part, status & 0xF0, &message[1..], emit);
            }
        }
    }
    fn channel_message<F: FnMut(&[u8])>(
        &mut self,
        part: usize,
        kind: u8,
        data: &[u8],
        emit: &mut F,
    ) {
        let first = data.first().copied().unwrap_or(0);
        let second = data.get(1).copied().unwrap_or(0);
        match kind {
            0x90 if second != 0 => self.note_on(part, first, second, emit),
            0x80 | 0x90 => self.note_off(part, first, second, emit),
            0xB0 => self.control(part, first, second, emit),
            0xC0 if part != RHYTHM_PART => {
                let patch = self.memory.patches[usize::from(first)];
                self.memory.patch_temp[part][..8].copy_from_slice(&patch);
                self.memory
                    .mark_written(memory::PATCH_TEMP + part as u32 * 16, 8, true);
                self.load_timbre(part);
                self.refresh(part, emit);
            }
            0xE0 if part != RHYTHM_PART => emit(&[0xE0 | output_channel(part), first, second]),
            _ => {} // The MT-32 ignores aftertouch; rhythm ignores program and bend.
        }
    }
    fn note_on<F: FnMut(&[u8])>(&mut self, part: usize, key: u8, velocity: u8, emit: &mut F) {
        let target = if part == RHYTHM_PART {
            self.rhythm_note(key).map(|note| (DRUM_CHANNEL, note))
        } else {
            let state = &self.parts[part];
            match state.target {
                Target::Program(_) => {
                    let note = i16::from(key) + i16::from(state.transpose);
                    (0..128)
                        .contains(&note)
                        .then_some((output_channel(part), note as u8))
                }
                Target::Drum(note) => Some((DRUM_CHANNEL, note)),
                Target::Silent => None,
            }
        };
        self.note_off(part, key, 64, emit);
        let Some((channel, note)) = target else {
            self.stats.unmapped_notes += 1;
            return;
        };
        // Drum-note presets share the rhythm channel volume; trim by velocity.
        let velocity = match self.parts[part].target {
            Target::Drum(_) if part != RHYTHM_PART => {
                let factor = u32::from(tables::db_factor(self.parts[part].trim));
                (u32::from(velocity) * factor / 1000).max(1) as u8
            }
            _ => velocity,
        };
        emit(&[0x90 | channel, note, velocity]);
        self.parts[part].held[usize::from(key)] = Held { channel, note };
    }
    fn note_off<F: FnMut(&[u8])>(&mut self, part: usize, key: u8, velocity: u8, emit: &mut F) {
        let held = core::mem::replace(&mut self.parts[part].held[usize::from(key)], NOT_HELD);
        if held.channel != UNSENT {
            emit(&[0x80 | held.channel, held.note, velocity]);
        }
    }
    fn release_all<F: FnMut(&[u8])>(&mut self, part: usize, emit: &mut F) {
        for key in 0..128 {
            self.note_off(part, key, 64, emit);
        }
    }
    fn control<F: FnMut(&[u8])>(&mut self, part: usize, number: u8, value: u8, emit: &mut F) {
        let out = output_channel(part);
        let melodic = part != RHYTHM_PART;
        match number {
            1 | 64 if melodic => emit(&[0xB0 | out, number, value]),
            11 | 121 => emit(&[0xB0 | out, number, value]),
            7 => {
                self.memory.patch_temp[part][memory::OUTPUT_LEVEL] =
                    (u16::from(value) * 100 / 127) as u8;
                self.memory.mark_written(
                    memory::PATCH_TEMP
                        + part as u32 * 16
                        + if number == 7 {
                            memory::OUTPUT_LEVEL
                        } else {
                            memory::PANPOT
                        } as u32,
                    1,
                    true,
                );
                self.refresh_mix(part, emit);
            }
            10 => {
                self.memory.patch_temp[part][memory::PANPOT] = (u16::from(value) * 14 / 127) as u8;
                self.memory.mark_written(
                    memory::PATCH_TEMP
                        + part as u32 * 16
                        + if number == 7 {
                            memory::OUTPUT_LEVEL
                        } else {
                            memory::PANPOT
                        } as u32,
                    1,
                    true,
                );
                self.refresh_mix(part, emit);
            }
            100 | 101 => self.parts[part].rpn[usize::from(number == 100)] = value,
            6 if melodic && self.parts[part].rpn == [0, 0] => {
                self.memory.patch_temp[part][memory::BENDER_RANGE] = value.min(24);
                self.memory.mark_written(
                    memory::PATCH_TEMP + part as u32 * 16 + memory::BENDER_RANGE as u32,
                    1,
                    true,
                );
                self.refresh(part, emit);
            }
            123..=127 => self.release_all(part, emit),
            _ => {} // Not recognized by the MT-32.
        }
    }
    fn sysex<F: FnMut(&[u8])>(&mut self, message: &[u8], emit: &mut F) {
        self.stats.sysex += 1;
        // F0 41 <device> 16 12 <address:3> <data..> <checksum> F7
        let roland_dt1 = message.len() >= 10
            && message[1] == 0x41
            && message[2] <= 0x10
            && message[3] == 0x16
            && message[4] == 0x12
            && message[message.len() - 1] == 0xF7;
        if !roland_dt1 {
            self.stats.ignored_sysex += 1;
            return;
        }
        let body = &message[5..message.len() - 1];
        if body.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)) & 0x7F != 0 {
            self.stats.checksum_errors += 1;
            return;
        }
        if body[0] == 0x7F {
            self.reset(emit);
            return;
        }
        let data = &body[3..body.len() - 1];
        if data.is_empty() {
            self.stats.ignored_sysex += 1;
            return;
        }
        let address = memory::memaddr(body[0], body[1], body[2]);
        let device = message[2];
        let mut changes = Changes::default();
        if device == 0x10 {
            changes = self.memory.write(address, data);
        } else if address >= memory::memaddr(3, 0, 0) {
            changes.ignored = true;
        } else {
            // Device IDs 0..=15 address the melodic parts receiving on that channel.
            for part in 0..8 {
                if self.memory.channel(part) != Some(device) {
                    continue;
                }
                let target = match address >> 14 {
                    0 => memory::PATCH_TEMP + part as u32 * 16 + address,
                    1 => memory::RHYTHM_TEMP + address - memory::memaddr(1, 0, 0),
                    _ => {
                        memory::TIMBRE_TEMP + (part * TIMBRE_SIZE) as u32 + address
                            - memory::memaddr(2, 0, 0)
                    }
                };
                changes.merge(&self.memory.write(target, data));
            }
        }
        if changes.ignored {
            self.stats.ignored_sysex += 1;
        }
        self.apply(&changes, emit);
    }
    fn apply<F: FnMut(&[u8])>(&mut self, changes: &Changes, emit: &mut F) {
        if changes.system & (0x1FF << memory::CHANNEL_ASSIGN) != 0 {
            // Parts may have moved channel; their pending note-offs would be lost.
            for part in 0..PARTS {
                self.release_all(part, emit);
            }
        }
        self.rhythm_written |= changes.rhythm_timbre;
        let mut refresh = changes.patch_temp | u16::from(changes.timbre_temp);
        for part in 0..8 {
            let bit = 1 << part;
            let [group, number] = self.timbre_ref(part);
            let memory_timbre = group == 2 && changes.timbres & (1 << number) != 0;
            if changes.patch_timbre & bit != 0 || memory_timbre {
                self.load_timbre(part);
                refresh |= bit;
            } else if u16::from(changes.timbre_temp_whole) & bit != 0 {
                self.parts[part].guest_timbre = true;
                self.stats.guest_timbre_loads += 1;
            }
        }
        let mix = 1 << memory::MASTER_VOLUME
            | 1 << memory::REVERB_MODE
            | 1 << memory::REVERB_TIME
            | 1 << memory::REVERB_LEVEL;
        if changes.system & mix != 0 {
            refresh = 0x1FF;
        }
        for part in 0..PARTS {
            if refresh & (1 << part) != 0 {
                if part != RHYTHM_PART {
                    self.resolve(part);
                }
                self.refresh(part, emit);
            }
        }
    }
    /// MT-32 "all parameters reset": power-on memory, notes released.
    fn reset<F: FnMut(&[u8])>(&mut self, emit: &mut F) {
        for part in 0..PARTS {
            self.release_all(part, emit);
            let out = output_channel(part);
            // Note Off cannot stop voices retained by the GM sustain pedal.
            // The MT-32 reset also restores expression, modulation and bend;
            // refreshing patch/mixer parameters alone leaves those controls stale.
            emit(&[0xB0 | out, 120, 0]);
            emit(&[0xB0 | out, 121, 0]);
        }
        let stats = self.stats;
        *self = Self::new();
        self.stats = stats;
        self.setup(emit);
    }
    fn timbre_ref(&self, part: usize) -> [u8; 2] {
        let patch = &self.memory.patch_temp[part];
        [
            patch[memory::TIMBRE_GROUP] & 3,
            patch[memory::TIMBRE_NUMBER] & 63,
        ]
    }
    /// Loads the patch's timbre into timbre temp, as the device does on program
    /// change. ROM presets have no bytes here and are mapped by number.
    fn load_timbre(&mut self, part: usize) {
        let [group, number] = self.timbre_ref(part);
        let guest = group == 2;
        let destination = memory::TIMBRE_TEMP + (part * TIMBRE_SIZE) as u32;
        if guest {
            self.memory.copy_written(
                memory::TIMBRES + u32::from(number) * 256,
                destination,
                TIMBRE_SIZE,
            );
        } else {
            self.memory.mark_written(destination, TIMBRE_SIZE, false);
        }
        if guest {
            let timbre = &self.memory.timbres[usize::from(number)][..TIMBRE_SIZE];
            self.memory.timbre_temp[part].copy_from_slice(timbre);
            self.stats.guest_timbre_loads += 1;
        }
        self.parts[part].guest_timbre = guest;
        self.resolve(part);
    }
    fn resolve(&mut self, part: usize) {
        let [group, number] = self.timbre_ref(part);
        let key_shift = self.memory.patch_temp[part][memory::KEY_SHIFT].min(48) as i8 - 24;
        let (target, transpose, trim) = if self.parts[part].guest_timbre {
            let (target, transpose) = timbre::melodic(&self.memory.timbre_temp[part]);
            (target, transpose, 0)
        } else if group < 2 {
            let program = group * 64 + number;
            let trim = tables::PRESET_TRIM_DB[usize::from(program)];
            match tables::preset_drum(program) {
                Some(note) => (Target::Drum(note), 0, trim),
                None => (
                    Target::Program(tables::PRESETS[usize::from(program)].1),
                    tables::preset_transpose(program),
                    trim,
                ),
            }
        } else {
            let note = tables::RHYTHM_TIMBRES
                .get(usize::from(number))
                .map(|n| n[0]);
            (note.map_or(Target::Silent, Target::Drum), 0, 0)
        };
        self.parts[part].target = target;
        self.parts[part].trim = trim;
        self.parts[part].transpose = transpose.saturating_add(key_shift).clamp(-48, 48);
    }
    fn rhythm_note(&self, key: u8) -> Option<u8> {
        if !(24..=108).contains(&key) {
            return None;
        }
        let index = usize::from(key - 24);
        if self.rhythm_written & (1 << index) == 0 {
            return tables::default_rhythm_note(key);
        }
        match self.memory.rhythm_temp[index][0] {
            timbre @ 0..=63 => timbre::drum(&self.memory.timbres[usize::from(timbre)]),
            timbre @ 64..=127 => tables::rhythm_timbre_note(timbre - 64, key),
            _ => None,
        }
    }
    /// Sends program, bend range, reverb send and mix for a part when changed.
    fn refresh<F: FnMut(&[u8])>(&mut self, part: usize, emit: &mut F) {
        let out = output_channel(part);
        let patch = self.memory.patch_temp[part];
        if part != RHYTHM_PART {
            if let Target::Program(program) = self.parts[part].target
                && self.changed(out, PROGRAM, program)
            {
                emit(&[0xC0 | out, program]);
            }
            let range = patch[memory::BENDER_RANGE].min(24);
            if self.changed(out, BEND_RANGE, range) {
                for (number, value) in [
                    (101, 0),
                    (100, 0),
                    (6, range),
                    (38, 0),
                    (101, 127),
                    (100, 127),
                ] {
                    emit(&[0xB0 | out, number, value]);
                }
            }
        }
        let system = &self.memory.system;
        let on = part == RHYTHM_PART || patch[memory::REVERB_SWITCH] & 1 != 0;
        let reverb = if on {
            tables::reverb_send(
                system[memory::REVERB_MODE],
                system[memory::REVERB_TIME],
                system[memory::REVERB_LEVEL],
            )
        } else {
            0
        };
        if self.changed(out, REVERB, reverb) {
            emit(&[0xB0 | out, 91, reverb]);
        }
        self.refresh_mix(part, emit);
    }
    fn refresh_mix<F: FnMut(&[u8])>(&mut self, part: usize, emit: &mut F) {
        let out = output_channel(part);
        let patch = self.memory.patch_temp[part];
        let trim = match self.parts[part].target {
            _ if part == RHYTHM_PART => tables::RHYTHM_TRIM_DB,
            Target::Program(_) => self.parts[part].trim,
            // Drum-note presets are trimmed by velocity on the rhythm channel.
            Target::Drum(_) | Target::Silent => 0,
        };
        let volume = tables::part_volume(
            patch[memory::OUTPUT_LEVEL],
            self.memory.system[memory::MASTER_VOLUME],
            trim,
        );
        if self.changed(out, VOLUME, volume) {
            emit(&[0xB0 | out, 7, volume]);
        }
        // MT-32 panpot 0 is right, opposite to GM.
        let pan = 127 - (u16::from(patch[memory::PANPOT].min(14)) * 127 / 14) as u8;
        if self.changed(out, PAN, pan) {
            emit(&[0xB0 | out, 10, pan]);
        }
    }
    fn changed(&mut self, channel: u8, slot: usize, value: u8) -> bool {
        let sent = &mut self.sent[usize::from(channel)][slot];
        let changed = *sent != value;
        *sent = value;
        changed
    }
}
fn output_channel(part: usize) -> u8 {
    if part == RHYTHM_PART {
        DRUM_CHANNEL
    } else {
        part as u8
    }
}
