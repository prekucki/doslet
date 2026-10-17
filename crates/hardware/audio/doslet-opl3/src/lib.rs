//! YMF262 register ports, guest-clock timers and Nuked OPL3 synthesis.
//! The upstream synthesis dependency currently requires `std`.

pub const SAMPLE_RATE: u32 = 48_000;

#[derive(Default)]
struct Timer {
    preset: u8,
    running: bool,
    masked: bool,
    overflow: bool,
    deadline: u64,
}

impl Timer {
    fn period(&self, tick: u64) -> u64 {
        (256 - u64::from(self.preset)) * tick
    }
    fn update(&mut self, now: u64, tick: u64) {
        if self.running && now >= self.deadline {
            self.overflow = true;
            let period = self.period(tick);
            self.deadline = now.saturating_add(period - (now - self.deadline) % period);
        }
    }
    fn control(&mut self, start: bool, masked: bool, now: u64, tick: u64) {
        self.masked = masked;
        if masked {
            self.overflow = false;
        }
        if start && !self.running {
            self.deadline = now.saturating_add(self.period(tick));
        }
        self.running = start;
    }
}

pub struct Opl3 {
    chip: nuked_opl3::Opl3Chip,
    address: [u8; 2],
    timers: [Timer; 2],
    started: bool,
    fraction: u64,
}
impl Default for Opl3 {
    fn default() -> Self {
        Self {
            chip: nuked_opl3::Opl3Chip::new(SAMPLE_RATE),
            address: [0; 2],
            timers: core::array::from_fn(|_| Timer::default()),
            started: false,
            fraction: 0,
        }
    }
}
impl Opl3 {
    pub fn started(&self) -> bool {
        self.started
    }
    pub fn next_sample_nanos(&self) -> Option<u64> {
        self.started
            .then(|| (1_000_000_000 - self.fraction).div_ceil(u64::from(SAMPLE_RATE)))
    }
    /// Offsets 0/1 select the primary bank; 2/3 select the secondary bank.
    pub fn write(&mut self, offset: u8, value: u8, now: u64) {
        let bank = usize::from((offset >> 1) & 1);
        if offset & 1 == 0 {
            self.address[bank] = value;
            return;
        }
        self.update_timers(now);
        let register = u16::from(self.address[bank]) | ((bank as u16) << 8);
        match register {
            2 => self.timers[0].preset = value,
            3 => self.timers[1].preset = value,
            4 => {
                if value & 0x80 != 0 {
                    for timer in &mut self.timers {
                        timer.overflow = false;
                    }
                } else {
                    self.timers[0].control(value & 1 != 0, value & 0x40 != 0, now, 80_000);
                    self.timers[1].control(value & 2 != 0, value & 0x20 != 0, now, 320_000);
                }
            }
            _ => {
                self.started = true;
                // The CPU/machine already orders writes on the guest timeline.
                self.chip.write_register(register, value);
            }
        }
    }
    fn update_timers(&mut self, now: u64) {
        self.timers[0].update(now, 80_000);
        self.timers[1].update(now, 320_000);
    }
    /// YMF262 status is readable only on the primary address port. Other
    /// ports float high; synthesis registers do not support readback.
    pub fn read(&mut self, offset: u8, now: u64) -> u8 {
        if offset & 3 != 0 {
            return 0xFF;
        }
        self.update_timers(now);
        let mut flags = 0;
        for (timer, bit) in self.timers.iter().zip([0x40, 0x20]) {
            if timer.overflow && !timer.masked {
                flags |= bit;
            }
        }
        flags | if flags != 0 { 0x80 } else { 0 }
    }
    /// Fixed-size generation on the guest clock; caller owns the output queue.
    /// Machine calls are bounded to at most one millisecond.
    pub fn advance(&mut self, nanos: u32, end: u64, mut emit: impl FnMut(u64, [i16; 2])) {
        if !self.started {
            return;
        }
        self.fraction += u64::from(nanos) * u64::from(SAMPLE_RATE);
        while self.fraction >= 1_000_000_000 {
            self.fraction -= 1_000_000_000;
            let mut sample = [0; 2];
            self.chip
                .generate_resampled(&mut sample)
                .expect("stereo buffer");
            let time = end - self.fraction / u64::from(SAMPLE_RATE);
            emit(time, sample);
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
