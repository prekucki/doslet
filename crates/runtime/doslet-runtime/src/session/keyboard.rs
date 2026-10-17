use super::*;

impl Session {
    /// Add a BIOS scan-code/ASCII pair to the guest-visible 15-key ring.
    /// Full queues reject the newest event rather than allocate or overwrite.
    pub fn push_key(&mut self, scan: u8, ascii: u8) -> Result<bool, RuntimeError> {
        let head = self.machine.bus.read_u16(0x41A)?;
        let tail = self.machine.bus.read_u16(0x41C)?;
        if !Self::valid_key_pointer(head) || !Self::valid_key_pointer(tail) {
            return Ok(false);
        }
        let next = if tail == 0x3C { 0x1E } else { tail + 2 };
        if next == head {
            return Ok(false);
        }
        self.machine.bus.write_u16(
            0x400 + u32::from(tail),
            (u16::from(scan) << 8) | u16::from(ascii),
        )?;
        self.machine.bus.write_u16(0x41C, next)?;
        self.menu_idle_input();
        Ok(true)
    }
    pub fn set_key_modifiers(&mut self, flags: u8) -> Result<(), RuntimeError> {
        self.machine.bus.write_u8(0x417, flags)?;
        Ok(())
    }
    pub fn waiting_for_key(&self) -> bool {
        self.registers().cs == bios::SHIM_SEGMENT
            && matches!(self.registers().ip, 0x300..=0x304 | 0x310..=0x314)
    }
    fn valid_key_pointer(pointer: u16) -> bool {
        (0x1E..0x3E).contains(&pointer) && pointer & 1 == 0
    }
    fn key(&mut self, consume: bool) -> Result<Option<u16>, RuntimeError> {
        let head = self.machine.bus.read_u16(0x41A)?;
        let tail = self.machine.bus.read_u16(0x41C)?;
        if head == tail || !Self::valid_key_pointer(head) || !Self::valid_key_pointer(tail) {
            return Ok(None);
        }
        let value = self.machine.bus.read_u16(0x400 + u32::from(head))?;
        if consume {
            self.machine
                .bus
                .write_u16(0x41A, if head == 0x3C { 0x1E } else { head + 2 })?;
        }
        Ok(Some(value))
    }
    fn saved_zero(&mut self, zero: bool) -> Result<(), RuntimeError> {
        let r = self.registers();
        let address = linear_address(r.ss, r.get(Gpr16::Sp).wrapping_add(4));
        let flags = self.machine.bus.read_u16(address)?;
        self.machine
            .bus
            .write_u16(address, (flags & !0x40) | if zero { 0x40 } else { 0 })?;
        Ok(())
    }
    pub(super) fn keyboard_service(&mut self, ax: u16) -> Result<(), RuntimeError> {
        match ax >> 8 {
            0 | 0x10 => {
                if let Some(key) = self.key(true)? {
                    self.machine.cpu.registers.set(Gpr16::Ax, key);
                } else {
                    self.machine.cpu.registers.ip = 0x300;
                }
            }
            1 | 0x11 => {
                let key = self.key(false)?;
                self.saved_zero(key.is_none())?;
                if let Some(key) = key {
                    self.machine.cpu.registers.set(Gpr16::Ax, key);
                }
            }
            2 => {
                let flags = self.machine.bus.read_u8(0x417)?;
                self.machine.cpu.registers.set_byte(0, flags);
            }
            5 => {
                let key = self.registers().get(Gpr16::Cx);
                let accepted = self.push_key((key >> 8) as u8, key as u8)?;
                self.machine.cpu.registers.set_byte(0, u8::from(!accepted));
            }
            _ => return Err(RuntimeError::UnsupportedService { vector: 0x16, ax }),
        }
        Ok(())
    }
    pub(super) fn dos_keyboard_service(&mut self, ax: u16) -> Result<(), RuntimeError> {
        let function = ax >> 8;
        match function {
            0x0B => {
                let pending = self.extended_key.is_some() || self.key(false)?.is_some();
                self.machine
                    .cpu
                    .registers
                    .set_byte(0, if pending { 255 } else { 0 });
            }
            0x0C => {
                let tail = self.machine.bus.read_u16(0x41C)?;
                self.machine.bus.write_u16(0x41A, tail)?;
                self.extended_key = None;
                if matches!(ax as u8, 1 | 6 | 7 | 8) {
                    self.machine.cpu.registers.set_byte(4, ax as u8);
                    self.dos_keyboard_service(u16::from(ax as u8) << 8)?;
                }
            }
            6 if self.registers().get_byte(2) != 255 => {
                let byte = self.registers().get_byte(2);
                self.console_output(byte)?;
                self.machine.cpu.registers.set_byte(0, byte);
            }
            1 | 6 | 7 | 8 => {
                let key = if let Some(scan) = self.extended_key.take() {
                    Some(u16::from(scan))
                } else if let Some(key) = self.key(true)? {
                    if key as u8 == 0 {
                        self.extended_key = Some((key >> 8) as u8);
                    }
                    Some(key)
                } else {
                    None
                };
                if let Some(key) = key {
                    self.machine.cpu.registers.set_byte(0, key as u8);
                    if function == 1 && key as u8 != 0 {
                        self.console_output(key as u8)?;
                    }
                    if function == 6 {
                        self.saved_zero(false)?;
                    }
                } else if function == 6 {
                    self.machine.cpu.registers.set_byte(0, 0);
                    self.saved_zero(true)?;
                } else {
                    self.machine.cpu.registers.ip = 0x310;
                }
            }
            _ => return Err(RuntimeError::UnsupportedService { vector: 0x21, ax }),
        }
        Ok(())
    }
}

impl Session {
    /// Consume one controller byte only when the installed IRQ1 handler runs.
    /// A guest replacing vector 09h receives raw hardware bytes instead.
    pub(super) fn keyboard_irq(&mut self) -> Result<(), RuntimeError> {
        let Some(raw) = self.machine.bus.read_pending_keyboard() else {
            return Ok(());
        };
        if raw == 0xE0 {
            self.keyboard_prefix = true;
            return Ok(());
        }
        if matches!(raw, 0xFA | 0xFE | 0xE1) {
            self.keyboard_prefix = false;
            return Ok(());
        }
        let extended = core::mem::take(&mut self.keyboard_prefix);
        let released = raw & 0x80 != 0;
        let scan = raw & 0x7F;
        let mut modifiers = self.machine.bus.read_u8(0x417)?;
        let flag = match scan {
            0x2A if !extended => 2,
            0x36 if !extended => 1,
            0x1D => 4,
            0x38 => 8,
            _ => 0,
        };
        if flag != 0 {
            if released {
                modifiers &= !flag;
            } else {
                modifiers |= flag;
            }
            self.machine.bus.write_u8(0x417, modifiers)?;
            return Ok(());
        }
        if released {
            return Ok(());
        }
        if !extended && matches!(scan, 0x3A | 0x45 | 0x46) {
            modifiers ^= match scan {
                0x3A => 0x40,
                0x45 => 0x20,
                _ => 0x10,
            };
            self.machine.bus.write_u8(0x417, modifiers)?;
            return Ok(());
        }
        let shift = modifiers & 3 != 0;
        let ctrl = modifiers & 4 != 0;
        let alt = modifiers & 8 != 0;
        let mut ascii = if extended {
            match scan {
                0x1C => 13,
                0x35 => b'/',
                _ => 0,
            }
        } else {
            scan_ascii(scan, shift, modifiers & 0x40 != 0)
        };
        if ctrl {
            ascii = match ascii.to_ascii_uppercase() {
                b'A'..=b'Z' => ascii.to_ascii_uppercase() - b'A' + 1,
                b'[' => 27,
                b'\\' => 28,
                b']' => 29,
                b'6' => 30,
                b'-' => 31,
                _ => ascii,
            };
        }
        let printable = ascii != 0;
        if alt {
            ascii = 0;
        }
        if printable || matches!(scan,0x3B..=0x44|0x47..=0x53|0x57|0x58) {
            self.push_key(scan, ascii)?;
        }
        Ok(())
    }
}
fn scan_ascii(scan: u8, shift: bool, caps: bool) -> u8 {
    let letter = match scan {
        0x10..=0x19 => b"qwertyuiop".get((scan - 0x10) as usize).copied(),
        0x1E..=0x26 => b"asdfghjkl".get((scan - 0x1E) as usize).copied(),
        0x2C..=0x32 => b"zxcvbnm".get((scan - 0x2C) as usize).copied(),
        _ => None,
    };
    if let Some(letter) = letter {
        return if shift ^ caps {
            letter.to_ascii_uppercase()
        } else {
            letter
        };
    }
    match scan {
        1 => 27,
        0x02..=0x0B => {
            if shift {
                b"!@#$%^&*()"[(scan - 2) as usize]
            } else {
                b"1234567890"[(scan - 2) as usize]
            }
        }
        0x0C => {
            if shift {
                b'_'
            } else {
                b'-'
            }
        }
        0x0D => {
            if shift {
                b'+'
            } else {
                b'='
            }
        }
        0x0E => 8,
        0x0F => 9,
        0x1C => 13,
        0x39 => 32,
        0x1A => {
            if shift {
                b'{'
            } else {
                b'['
            }
        }
        0x1B => {
            if shift {
                b'}'
            } else {
                b']'
            }
        }
        0x27 => {
            if shift {
                b':'
            } else {
                b';'
            }
        }
        0x28 => {
            if shift {
                b'"'
            } else {
                b'\''
            }
        }
        0x29 => {
            if shift {
                b'~'
            } else {
                b'`'
            }
        }
        0x2B => {
            if shift {
                b'|'
            } else {
                b'\\'
            }
        }
        0x33 => {
            if shift {
                b'<'
            } else {
                b','
            }
        }
        0x34 => {
            if shift {
                b'>'
            } else {
                b'.'
            }
        }
        0x35 => {
            if shift {
                b'?'
            } else {
                b'/'
            }
        }
        _ => 0,
    }
}
