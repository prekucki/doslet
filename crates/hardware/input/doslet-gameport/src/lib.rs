//! One two-axis, two-button joystick on the standard ISA gameport.
#![no_std]
#[derive(Default)]
pub struct Gameport {
    axes: [i16; 2],
    buttons: [bool; 2],
    deadlines: [u64; 2],
}
impl Gameport {
    pub fn set_state(&mut self, axes: [i16; 2], buttons: [bool; 2]) {
        self.axes = axes.map(|v| v.max(-32767));
        self.buttons = buttons;
    }
    pub fn state(&self) -> ([i16; 2], [bool; 2]) {
        (self.axes, self.buttons)
    }
    /// Functional RC timing: 24 us plus up to 1.1 ms of axis resistance.
    pub fn trigger(&mut self, nanos: u64) {
        for i in 0..2 {
            let delay = 24_000 + (i64::from(self.axes[i]) + 32767) as u64 * 1_100_000 / 65534;
            self.deadlines[i] = nanos.saturating_add(delay);
        }
    }
    pub fn read(&self, nanos: u64) -> u8 {
        0xCC | u8::from(nanos < self.deadlines[0])
            | (u8::from(nanos < self.deadlines[1]) << 1)
            | (u8::from(!self.buttons[0]) << 4)
            | (u8::from(!self.buttons[1]) << 5)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadlines_and_buttons_use_guest_time() {
        let mut g = Gameport::default();
        g.set_state([-32767, 32767], [true, false]);
        g.trigger(10);
        assert_eq!(g.read(10), 0xEF);
        assert_eq!(g.read(24_010), 0xEE);
        assert_eq!(g.read(1_124_010), 0xEC);
    }
}

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
