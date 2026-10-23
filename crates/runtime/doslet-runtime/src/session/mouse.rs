use super::*;

impl Session {
    /// Relative reads take precedence for one second of guest time, even if
    /// absolute position queries are interleaved. Host motion is not evidence.
    /// Guest time of the latest relative-motion read, for host-side policy.
    pub fn pointer_relative_last_read(&self) -> Option<u64> {
        self.mouse_last_relative_read
    }

    pub fn pointer_relative_in_use(&self) -> bool {
        self.mouse_last_relative_read
            .is_some_and(|last| self.emulated_nanos().saturating_sub(last) < 1_000_000_000)
    }

    /// A visible driver cursor or recent guest input consumption justifies
    /// hiding the host cursor. Installation probes and host motion do not.
    pub fn pointer_in_use(&self) -> bool {
        self.mouse.visible >= 0
            || self
                .mouse_last_read
                .is_some_and(|last| self.emulated_nanos().saturating_sub(last) < 500_000_000)
    }

    pub(super) fn mouse_video_mode_changed(&mut self) {
        self.mouse_fraction = [0.0; 2];
        let (_, height) = self.pointer_dimensions();
        let old_height = u32::from(self.mouse.y_range.1.saturating_sub(self.mouse.y_range.0)) + 1;
        let old_y = u32::from(self.mouse.y.saturating_sub(self.mouse.y_range.0));
        self.mouse.y = (old_y * u32::from(height) / old_height).min(u32::from(height - 1)) as u16;
        self.mouse.x = self.mouse.x.min(639);
        self.mouse.x_range = (0, 639);
        self.mouse.y_range = (0, height - 1);
        self.mouse.motion = (0, 0);
        self.mouse.pending = 0;
        self.mouse.revision = self.mouse.revision.wrapping_add(1);
    }
    pub fn pointer_move(&mut self, x: u16, y: u16) {
        if self.pointer_position() != (x, y) {
            self.menu_idle_input();
        }
        self.mouse_fraction = [0.0; 2];
        self.mouse.move_to(x, y);
    }
    /// Relative host movement continues at window edges. Fractional movement
    /// is retained; movement beyond guest bounds never creates a return debt.
    pub fn pointer_relative(&mut self, dx: f64, dy: f64) {
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        if dx != 0.0 || dy != 0.0 {
            self.menu_idle_input();
        }
        let position = [self.mouse.x, self.mouse.y];
        let mut motion = [0; 2];
        let ranges = [self.mouse.x_range, self.mouse.y_range];
        for (axis, delta) in [dx, dy].into_iter().enumerate() {
            let delta = delta.clamp(-32767.0, 32767.0) + self.mouse_fraction[axis];
            let whole = delta as i32;
            motion[axis] = whole as i16;
            self.mouse_fraction[axis] = delta - f64::from(whole);
            let value = i32::from(position[axis]) + whole;
            let (low, high) = ranges[axis];
            if value < i32::from(low) || value > i32::from(high) {
                self.mouse_fraction[axis] = 0.0;
            }
        }
        self.mouse.move_relative(motion[0], motion[1]);
    }
    pub fn pointer_button(&mut self, button: u8, pressed: bool) {
        let before = self.mouse.buttons;
        self.mouse.button(button, pressed);
        if before != self.mouse.buttons {
            self.menu_idle_input();
        }
    }
    pub fn release_pointer_buttons(&mut self) {
        if self.mouse.buttons != 0 {
            self.menu_idle_input();
        }
        self.mouse.release_buttons();
    }
    pub fn pointer_dimensions(&self) -> (u16, u16) {
        (
            640,
            if self.video_mode() == doslet_vga::VideoMode::Vbe101 {
                480
            } else {
                200
            },
        )
    }
    pub fn pointer_position(&self) -> (u16, u16) {
        (self.mouse.x, self.mouse.y)
    }
    pub fn pointer_buttons(&self) -> u16 {
        self.mouse.buttons
    }
    pub fn pointer_ranges(&self) -> ((u16, u16), (u16, u16)) {
        (self.mouse.x_range, self.mouse.y_range)
    }
    pub(super) fn dispatch_mouse(&mut self) -> Result<(), RuntimeError> {
        if self.machine.cpu.protected_mode() {
            // Our DPMI host reflects IRQ 74h through the real-mode mouse
            // wrapper. Dispatch its host trampoline before executing IRET.
            if self.dpmi_stats().is_some_and(|stats| stats.entered)
                && !self.mouse.callback_active
                && self.mouse.pending & self.mouse.callback.0 != 0
                && self.machine.cpu.accepts_maskable_interrupt()
            {
                self.machine.cpu.interrupt(&mut self.machine.bus, 0x74)?;
                self.mouse.callback_active = true;
                self.dpmi_dispatch()?;
            }
            return Ok(());
        }
        let r = &self.machine.cpu.registers;
        if r.cs == bios::SHIM_SEGMENT {
            if self.mouse.callback_active && r.ip == 0x408 {
                self.mouse_last_read = Some(self.emulated_nanos());
                let event = self.mouse.pending & self.mouse.callback.0;
                self.mouse.pending = 0;
                let r = &mut self.machine.cpu.registers;
                r.set(Gpr16::Ax, event);
                r.set(Gpr16::Bx, self.mouse.buttons);
                r.set(Gpr16::Cx, self.mouse.x);
                r.set(Gpr16::Dx, self.mouse.y);
                r.set(Gpr16::Si, self.mouse.motion.0 as u16);
                r.set(Gpr16::Di, self.mouse.motion.1 as u16);
            } else if r.ip == 0x415 {
                self.mouse.callback_active = false;
            }
        } else if !self.mouse.callback_active
            && self.mouse.pending & self.mouse.callback.0 != 0
            && self.machine.cpu.accepts_maskable_interrupt()
        {
            self.machine.cpu.interrupt(&mut self.machine.bus, 0x74)?;
            self.mouse.callback_active = true;
        }
        Ok(())
    }
    pub(super) fn mouse_service(&mut self, ax: u16) -> Result<(), RuntimeError> {
        let r = self.registers();
        let (bx, cx, dx, es) = (r.get(Gpr16::Bx), r.get(Gpr16::Cx), r.get(Gpr16::Dx), r.es);
        match ax {
            0 | 0x21 => {
                self.mouse_last_read = None;
                self.mouse_last_relative_read = None;
                self.mouse_fraction = [0.0; 2];
                self.mouse = doslet_dos::mouse::Mouse::default();
                self.mouse.y_range.1 = self.pointer_dimensions().1 - 1;
                self.mouse.y = self.pointer_dimensions().1 / 2;
                self.machine.cpu.registers.set(Gpr16::Ax, 0xFFFF);
                self.machine.cpu.registers.set(Gpr16::Bx, 2);
            }
            1 => {
                self.mouse.visible = self.mouse.visible.saturating_add(1);
                self.mouse.revision = self.mouse.revision.wrapping_add(1);
            }
            2 => {
                self.mouse.visible = self.mouse.visible.saturating_sub(1);
                self.mouse.revision = self.mouse.revision.wrapping_add(1);
            }
            3 => {
                self.mouse_last_read = Some(self.emulated_nanos());
                self.machine
                    .cpu
                    .registers
                    .set(Gpr16::Bx, self.mouse.buttons);
                self.machine.cpu.registers.set(Gpr16::Cx, self.mouse.x);
                self.machine.cpu.registers.set(Gpr16::Dx, self.mouse.y);
            }
            4 => {
                self.mouse_fraction = [0.0; 2];
                self.mouse.move_to(cx, dx);
            }
            5 | 6 if bx < 3 => {
                self.mouse_last_read = Some(self.emulated_nanos());
                let history = if ax == 5 {
                    &mut self.mouse.presses[usize::from(bx)]
                } else {
                    &mut self.mouse.releases[usize::from(bx)]
                };
                self.machine.cpu.registers.set(Gpr16::Bx, history.count);
                self.machine.cpu.registers.set(Gpr16::Cx, history.x);
                self.machine.cpu.registers.set(Gpr16::Dx, history.y);
                self.machine
                    .cpu
                    .registers
                    .set(Gpr16::Ax, self.mouse.buttons);
                history.count = 0;
            }
            7 | 8 if cx <= dx => {
                if ax == 7 {
                    self.mouse.x_range = (cx, dx);
                } else {
                    self.mouse.y_range = (cx, dx);
                }
                self.mouse.move_to(self.mouse.x, self.mouse.y);
            }
            9 => {
                self.mouse.hotspot = (bx as i16, cx as i16);
                for (index, word) in self.mouse.masks.iter_mut().enumerate() {
                    *word = self
                        .machine
                        .bus
                        .read_u16(linear_address(es, dx.wrapping_add(index as u16 * 2)))?;
                }
                self.mouse.revision = self.mouse.revision.wrapping_add(1);
            }
            0x0B => {
                self.mouse_last_read = Some(self.emulated_nanos());
                self.mouse_last_relative_read = Some(self.emulated_nanos());
                self.machine
                    .cpu
                    .registers
                    .set(Gpr16::Cx, self.mouse.motion.0 as u16);
                self.machine
                    .cpu
                    .registers
                    .set(Gpr16::Dx, self.mouse.motion.1 as u16);
                self.mouse.motion = (0, 0);
            }
            0x0C | 0x14 => {
                let previous = self.mouse.callback;
                self.mouse.callback = (cx, es, dx);
                self.mouse.pending = 0;
                self.machine.bus.write_u16(0xF0409, dx)?;
                self.machine.bus.write_u16(0xF040B, es)?;
                if ax == 0x14 {
                    self.machine.cpu.registers.set(Gpr16::Cx, previous.0);
                    self.machine.cpu.registers.es = previous.1;
                    self.machine.cpu.registers.set(Gpr16::Dx, previous.2);
                }
            }
            // Absolute host positions bypass relative-input acceleration.
            0x0F if cx != 0 && dx != 0 => self.mouse.mickey_ratio = (cx, dx),
            0x13 => self.mouse.double_speed_threshold = if dx == 0 { 64 } else { dx },
            // Logitech SWIFT installation probe: no 3D device/driver support.
            0x53C1 => self.machine.cpu.registers.set(Gpr16::Ax, 0),
            0x15 => self.machine.cpu.registers.set(Gpr16::Bx, 160),
            0x16 | 0x17 => {
                let mut words = [0u16; 80];
                if ax == 0x16 {
                    let m = &self.mouse;
                    words[..13].copy_from_slice(&[
                        m.x,
                        m.y,
                        m.buttons,
                        m.x_range.0,
                        m.x_range.1,
                        m.y_range.0,
                        m.y_range.1,
                        m.visible as u16,
                        m.motion.0 as u16,
                        m.motion.1 as u16,
                        m.mickey_ratio.0,
                        m.mickey_ratio.1,
                        m.double_speed_threshold,
                    ]);
                    for (i, history) in m.presses.iter().chain(&m.releases).enumerate() {
                        words[13 + i * 3..16 + i * 3].copy_from_slice(&[
                            history.count,
                            history.x,
                            history.y,
                        ]);
                    }
                    words[31..63].copy_from_slice(&m.masks);
                    words[63..68].copy_from_slice(&[
                        m.hotspot.0 as u16,
                        m.hotspot.1 as u16,
                        m.callback.0,
                        m.callback.1,
                        m.callback.2,
                    ]);
                    for (i, word) in words.iter().enumerate() {
                        self.machine
                            .bus
                            .write_u16(linear_address(es, dx.wrapping_add(i as u16 * 2)), *word)?;
                    }
                } else {
                    for (i, word) in words.iter_mut().enumerate() {
                        *word = self
                            .machine
                            .bus
                            .read_u16(linear_address(es, dx.wrapping_add(i as u16 * 2)))?;
                    }
                    if words[3] > words[4]
                        || words[5] > words[6]
                        || words[10] == 0
                        || words[11] == 0
                    {
                        return Err(RuntimeError::UnsupportedService { vector: 0x33, ax });
                    }
                    let m = &mut self.mouse;
                    m.x = words[0];
                    m.y = words[1];
                    m.buttons = words[2] & 7;
                    m.x_range = (words[3], words[4]);
                    m.y_range = (words[5], words[6]);
                    m.visible = words[7] as i16;
                    m.motion = (words[8] as i16, words[9] as i16);
                    m.mickey_ratio = (words[10], words[11]);
                    m.double_speed_threshold = words[12];
                    for (i, history) in m.presses.iter_mut().chain(&mut m.releases).enumerate() {
                        history.count = words[13 + i * 3];
                        history.x = words[14 + i * 3];
                        history.y = words[15 + i * 3];
                    }
                    m.masks.copy_from_slice(&words[31..63]);
                    m.hotspot = (words[63] as i16, words[64] as i16);
                    m.callback = (words[65], words[66], words[67]);
                    m.pending = 0;
                    m.revision = m.revision.wrapping_add(1);
                    self.machine.bus.write_u16(0xF0409, m.callback.2)?;
                    self.machine.bus.write_u16(0xF040B, m.callback.1)?;
                }
            }
            0x24 => {
                self.machine.cpu.registers.set(Gpr16::Bx, 0x0800);
                self.machine.cpu.registers.set(Gpr16::Cx, 0x0400); // Selected driver API, PS/2 style, no hardware IRQ backend.
            }
            _ => return Err(RuntimeError::UnsupportedService { vector: 0x33, ax }),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_queries_dominate_absolute_queries_for_one_guest_second() {
        let mut session = Session::new(&[0x90, 0xeb, 0xfd]).unwrap();
        assert!(!session.pointer_relative_in_use());
        session.pointer_relative(3.0, 4.0);
        assert!(!session.pointer_relative_in_use());
        session.mouse_service(0x0b).unwrap();
        session.mouse_service(3).unwrap();
        assert!(session.pointer_relative_in_use());
        while session.emulated_nanos() < 999_999_999 {
            session
                .debug_advance_device_time(999_999_999 - session.emulated_nanos())
                .unwrap();
        }
        assert!(session.pointer_relative_in_use());
        session.debug_advance_device_time(1).unwrap();
        assert!(!session.pointer_relative_in_use());
        session.mouse_service(0x0b).unwrap();
        #[cfg(feature = "save-state")]
        assert!(
            Session::load_state(&session.save_state().unwrap())
                .unwrap()
                .pointer_relative_in_use()
        );
        session.mouse_service(0x21).unwrap();
        assert!(!session.pointer_relative_in_use());
    }

    #[test]
    fn pointer_usage_requires_guest_reads_or_visible_driver_cursor_and_expires() {
        let mut session = Session::new(&[0xFB, 0xF4, 0xEB, 0xFD]).unwrap();
        assert!(!session.pointer_in_use());
        session.pointer_move(123, 45);
        session.pointer_button(0, true);
        session.mouse_service(0).unwrap();
        session.mouse_service(0x24).unwrap();
        assert!(!session.pointer_in_use());
        session.mouse_service(3).unwrap();
        assert!(session.pointer_in_use());
        session.start();
        session.advance(5_000_000);
        assert_eq!(session.status(), SessionStatus::Running);
        assert!(session.emulated_nanos() >= 500_000_000);
        assert!(!session.pointer_in_use());
        session.mouse_service(1).unwrap();
        assert!(session.pointer_in_use());
        session.mouse_service(2).unwrap();
        assert!(!session.pointer_in_use());
        session.mouse_service(0x0B).unwrap();
        assert!(session.pointer_in_use());
        session.mouse_service(0x21).unwrap();
        assert!(!session.pointer_in_use());
    }
}
