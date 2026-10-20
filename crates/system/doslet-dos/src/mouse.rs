//! Selected DOS mouse-driver state. Host adapters supply logical coordinates.
#[derive(Clone, Copy, Default)]
pub struct ButtonHistory {
    pub count: u16,
    pub x: u16,
    pub y: u16,
}
pub struct Mouse {
    pub x: u16,
    pub y: u16,
    pub buttons: u16,
    pub x_range: (u16, u16),
    pub y_range: (u16, u16),
    pub visible: i16,
    pub motion: (i16, i16),
    pub mickey_ratio: (u16, u16),
    pub double_speed_threshold: u16,
    pub presses: [ButtonHistory; 3],
    pub releases: [ButtonHistory; 3],
    pub masks: [u16; 32],
    pub hotspot: (i16, i16),
    pub revision: u64,
    pub callback: (u16, u16, u16),
    pub pending: u16,
    pub callback_active: bool,
}
impl Default for Mouse {
    fn default() -> Self {
        let mut masks = [0xFFFF; 32];
        masks[16..].copy_from_slice(&[
            0x8000, 0xC000, 0xE000, 0xF000, 0xF800, 0xFC00, 0xFE00, 0xFF00, 0xFF80, 0xF800, 0xD800,
            0x8C00, 0x0C00, 0x0600, 0x0600, 0,
        ]);
        Self {
            x: 320,
            y: 100,
            buttons: 0,
            x_range: (0, 639),
            y_range: (0, 199),
            visible: -1,
            motion: (0, 0),
            mickey_ratio: (8, 16),
            double_speed_threshold: 64,
            presses: [ButtonHistory::default(); 3],
            releases: [ButtonHistory::default(); 3],
            masks,
            hotspot: (0, 0),
            revision: 0,
            callback: (0, 0, 0),
            pending: 0,
            callback_active: false,
        }
    }
}
impl Mouse {
    /// Raw motion counters keep moving even when the logical cursor is clamped.
    /// Games using INT 33h/000Bh maintain their own cursor position.
    pub fn move_relative(&mut self, dx: i16, dy: i16) {
        let motion = self.motion;
        let x = (i32::from(self.x) + i32::from(dx))
            .clamp(i32::from(self.x_range.0), i32::from(self.x_range.1));
        let y = (i32::from(self.y) + i32::from(dy))
            .clamp(i32::from(self.y_range.0), i32::from(self.y_range.1));
        self.move_to(x as u16, y as u16);
        self.motion = (motion.0.saturating_add(dx), motion.1.saturating_add(dy));
        if dx != 0 || dy != 0 {
            self.pending |= 1;
        }
    }
    pub fn move_to(&mut self, x: u16, y: u16) {
        let x = x.clamp(self.x_range.0, self.x_range.1);
        let y = y.clamp(self.y_range.0, self.y_range.1);
        self.motion.0 = self
            .motion
            .0
            .saturating_add((i32::from(x) - i32::from(self.x)).clamp(-32768, 32767) as i16);
        self.motion.1 = self
            .motion
            .1
            .saturating_add((i32::from(y) - i32::from(self.y)).clamp(-32768, 32767) as i16);
        if (x, y) != (self.x, self.y) {
            self.pending |= 1;
            self.revision = self.revision.wrapping_add(1);
        }
        self.x = x;
        self.y = y;
    }
    pub fn button(&mut self, button: u8, pressed: bool) {
        if button >= 3 {
            return;
        }
        let mask = 1 << button;
        if (self.buttons & mask != 0) == pressed {
            return;
        }
        self.pending |= 1 << (1 + button * 2 + u8::from(!pressed));
        self.buttons = (self.buttons & !mask) | if pressed { mask } else { 0 };
        let history = if pressed {
            &mut self.presses[usize::from(button)]
        } else {
            &mut self.releases[usize::from(button)]
        };
        history.count = history.count.saturating_add(1);
        history.x = self.x;
        history.y = self.y;
        self.revision = self.revision.wrapping_add(1);
    }
    pub fn release_buttons(&mut self) {
        for button in 0..3 {
            self.button(button, false);
        }
    }
}

#[cfg(feature = "save-state")]
#[path = "mouse_saved.rs"]
mod saved_state;
