use super::*;

const DAY_CENTIS: u64 = 24 * 60 * 60 * 100;

impl Session {
    pub(super) fn dos_time_centis(&self) -> u64 {
        (u64::from(self.dos_time_origin_centis)
            + (self.emulated_nanos() - self.dos_time_origin_nanos) / 10_000_000)
            % DAY_CENTIS
    }

    pub(super) fn set_dos_time(&mut self) -> Result<(), RuntimeError> {
        let r = self.registers();
        let (hour, minute) = (r.get_byte(5), r.get_byte(1)); // CH, CL.
        let (second, hundredth) = (r.get_byte(6), r.get_byte(2)); // DH, DL.
        if hour >= 24 || minute >= 60 || second >= 60 || hundredth >= 100 {
            self.machine.cpu.registers.set_byte(0, 0xFF);
            return Ok(());
        }
        let centis = ((u32::from(hour) * 60 + u32::from(minute)) * 60 + u32::from(second)) * 100
            + u32::from(hundredth);
        self.dos_time_origin_nanos = self.emulated_nanos();
        self.dos_time_origin_centis = centis;
        // DOS's clock driver also sets the BIOS tick count and CMOS time.
        // Guest execution/PIT/audio time remains monotonic and independent.
        let ticks = u64::from(centis) * 0x1800B0 / DAY_CENTIS;
        self.machine.bus.write_u32(0x46C, ticks as u32)?;
        self.machine.bus.write_u8(0x470, 0)?;
        let cmos = &mut self.machine.bus.cmos;
        let selected = cmos.selected();
        cmos.select(0x0B);
        let mode = cmos.read();
        let encode = |value: u8| {
            if mode & 4 != 0 {
                value
            } else {
                ((value / 10) << 4) | (value % 10)
            }
        };
        let rtc_hour = if mode & 2 != 0 {
            encode(hour)
        } else {
            encode(if hour % 12 == 0 { 12 } else { hour % 12 }) | if hour >= 12 { 0x80 } else { 0 }
        };
        for (register, value) in [(4, rtc_hour), (2, encode(minute)), (0, encode(second))] {
            cmos.select(register);
            cmos.write(value);
        }
        cmos.select(selected);
        self.machine.cpu.registers.set_byte(0, 0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(session: &mut Session, cx: u16, dx: u16) {
        session.machine.cpu.registers.set(Gpr16::Ax, 0x2D00);
        session.machine.cpu.registers.set(Gpr16::Cx, cx);
        session.machine.cpu.registers.set(Gpr16::Dx, dx);
        session.set_dos_time().unwrap();
    }

    #[test]
    fn dos_time_advances_wraps_and_resets_without_rewinding_hardware() {
        let mut session = Session::new(&[0xFB, 0xF4, 0xEB, 0xFD]).unwrap();
        session.start();
        session.advance(1_000_000);
        let nanos = session.emulated_nanos();
        set(&mut session, 0x173B, 0x3B63); // 23:59:59.99.
        assert_eq!(session.emulated_nanos(), nanos);
        assert_eq!(session.dos_time_centis(), DAY_CENTIS - 1);
        session.advance(1_000_000);
        let elapsed = (session.emulated_nanos() - nanos) / 10_000_000;
        assert_eq!(session.dos_time_centis(), elapsed - 1);
        let later = session.emulated_nanos();
        set(&mut session, 0, 0);
        assert_eq!(session.emulated_nanos(), later);
        assert_eq!(session.dos_time_centis(), 0);
        assert_eq!(session.machine.bus.read_u32(0x46C).unwrap(), 0);
    }

    #[test]
    fn invalid_time_preserves_dos_bios_and_cmos_clocks() {
        let mut session = Session::new(&[0xEB, 0xFE]).unwrap();
        set(&mut session, 0x0C22, 0x382A); // 12:34:56.42.
        let ticks = session.machine.bus.read_u32(0x46C).unwrap();
        for (cx, dx) in [(0x1800, 0), (0x003C, 0), (0, 0x3C00), (0, 100)] {
            set(&mut session, cx, dx);
            assert_eq!(session.registers().get_byte(0), 0xFF);
            assert_eq!(session.dos_time_centis(), 4_529_642);
            assert_eq!(session.machine.bus.read_u32(0x46C).unwrap(), ticks);
            session.machine.bus.cmos.select(4);
            assert_eq!(session.machine.bus.cmos.read(), 0x12);
        }
    }

    #[test]
    fn cmos_encoding_respects_binary_bcd_and_twelve_hour_modes_and_selected_index() {
        for (mode, hour) in [(0, 0x81), (2, 0x13), (4, 0x81), (6, 13)] {
            let mut session = Session::new(&[0xEB, 0xFE]).unwrap();
            session.machine.bus.cmos.select(0x0B);
            session.machine.bus.cmos.write(mode);
            session.machine.bus.cmos.select(0x97);
            set(&mut session, 0x0D22, 0x3800); // 13:34:56.
            assert_eq!(session.machine.bus.cmos.selected(), 0x97);
            session.machine.bus.cmos.select(4);
            assert_eq!(session.machine.bus.cmos.read(), hour);
            session.machine.bus.cmos.select(2);
            assert_eq!(
                session.machine.bus.cmos.read(),
                if mode & 4 != 0 { 34 } else { 0x34 }
            );
        }
    }
}
