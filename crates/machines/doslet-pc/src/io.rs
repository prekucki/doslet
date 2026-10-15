use crate::memory::{A20Gate, Ram};
use core::ops::Range;
use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_i8042::Controller;
use doslet_i8254::Pit;
use doslet_i8259::Pic;
use doslet_mc146818::Cmos;
use doslet_mpu401::Mpu401;
use doslet_vga::{APERTURE_LEN, APERTURE_START, Vga};

pub struct PcBus {
    /// CPU-independent notification generation. Only successful active VGA
    /// status reads increment it; the PC compares before/after one CPU step.
    pub(crate) vga_status_read_generation: u32,
    pub(crate) observe_vga_status: bool,
    #[cfg(feature = "profiling")]
    pub(crate) stats: crate::ExecutionStats,
    ram: Ram,
    pub vga: Vga,
    pub pic: Pic,
    pub slave_pic: Pic,
    pub pit: Pit,
    pub mpu: Mpu401,
    pub dsp: doslet_sb_dsp::Dsp,
    pub mixer: doslet_sb_mixer::Mixer,
    pub dma: [doslet_i8237::Dma; 2],
    dma_pages: [u8; 8],
    pub(crate) pcm: crate::audio::PcmQueue,
    a20: A20Gate,
    pub keyboard: Controller,
    pub cmos: Cmos,
    system_control: u8,
    pub(crate) awe_writes: u64,
}
impl Default for PcBus {
    fn default() -> Self {
        let mut pit = Pit::default();
        pit.write_control(0x36).unwrap();
        pit.write_counter(0, 0).unwrap();
        pit.write_counter(0, 0).unwrap();
        pit.set_gate(2, false).unwrap();
        let mut slave_pic = Pic::new(0x70);
        slave_pic.write_data(0xFF);
        Self {
            vga_status_read_generation: 0,
            observe_vga_status: true,
            #[cfg(feature = "profiling")]
            stats: crate::ExecutionStats::default(),
            ram: Ram::default(),
            vga: Vga::default(),
            pic: Pic::default(),
            slave_pic,
            pit,
            mpu: Mpu401::default(),
            dsp: doslet_sb_dsp::Dsp::default(),
            mixer: doslet_sb_mixer::Mixer::default(),
            dma: core::array::from_fn(|_| doslet_i8237::Dma::default()),
            dma_pages: [0; 8],
            pcm: crate::audio::PcmQueue::default(),
            keyboard: Controller::default(),
            a20: A20Gate::default(),
            cmos: Cmos::new(640, ((crate::memory::RAM_SIZE - 0x100000) / 1024) as u16),
            system_control: 0,
            awe_writes: 0,
        }
    }
}
impl PcBus {
    pub fn text_revision(&self) -> u64 {
        self.ram.text_revision()
    }
    pub(crate) fn dma_sample(&mut self, channel: u8) -> Result<Option<u16>, crate::MachineError> {
        use doslet_i8237::Direction;
        let controller = usize::from(channel / 4);
        if channel >= 8 || channel == 4 {
            return Err(crate::MachineError::UnsupportedAudioRouting);
        }
        let local = channel & 3;
        self.dma[controller].request(local, true)?;
        let Some(offset) =
            self.dma[controller].transfer_address(local, Direction::MemoryToDevice)?
        else {
            self.dma[controller].request(local, false)?;
            return Ok(None);
        };
        let address = self.dma_physical_address(channel, offset).unwrap();
        // ISA DMA generates physical addresses independently of the CPU A20 line.
        // Audio transfers currently require ordinary RAM; MMIO DMA is unsupported.
        if (APERTURE_START..APERTURE_START + APERTURE_LEN as u32).contains(&address) {
            return Err(BusError::UnmappedMemory(address).into());
        }
        let low = self.ram.read(address)?;
        let value = if channel >= 4 {
            u16::from_le_bytes([low, self.ram.read(address + 1)?])
        } else {
            u16::from(low)
        };
        self.dma[controller].complete_transfer(local)?;
        self.dma[controller].request(local, false)?;
        Ok(Some(value))
    }
    fn dma_page_channel(port: u16) -> usize {
        match port {
            0x87 => 0,
            0x83 => 1,
            0x81 => 2,
            0x82 => 3,
            0x8F => 4,
            0x8B => 5,
            0x89 => 6,
            0x8A => 7,
            _ => unreachable!(),
        }
    }
    pub fn dma_physical_address(&self, channel: u8, offset: u16) -> Option<u32> {
        let page = u32::from(*self.dma_pages.get(usize::from(channel))?);
        Some(if channel < 4 {
            (page << 16) | u32::from(offset)
        } else {
            ((page & 0xFE) << 16) | (u32::from(offset) << 1)
        })
    }
    /// Borrow only ordinary RAM; a range touching device memory is rejected.
    pub fn ram_view(&self, range: Range<usize>) -> Option<&[u8]> {
        if !self.a20.enabled && range.end > 0x100000 {
            return None;
        }
        let start = APERTURE_START as usize;
        let end = start + APERTURE_LEN;
        if range.start < end && range.end > start {
            return None;
        }
        self.ram.view(range)
    }
    pub fn a20_enabled(&self) -> bool {
        self.a20.enabled
    }
    pub fn set_a20(&mut self, enabled: bool) {
        if self.a20.enabled != enabled {
            self.a20.generation = self.a20.generation.saturating_add(1);
            self.ram.set_mapping(self.a20.generation);
        }
        self.a20.enabled = enabled;
        self.keyboard.set_a20_line(enabled);
    }
    /// Wide accesses may use RAM only if every byte maps contiguously and
    /// the complete range excludes MMIO. All other accesses retain byte order.
    #[inline]
    fn wide_ram_range(&self, address: u32, length: u32) -> Option<Range<usize>> {
        let start = self.a20.address(address);
        if start > crate::memory::RAM_SIZE as u32 - length
            || (!self.a20.enabled && (address & 0xFFFFF) > 0x100000 - length)
            || (start < APERTURE_START + APERTURE_LEN as u32 && start + length > APERTURE_START)
        {
            return None;
        }
        Some(start as usize..start as usize + length as usize)
    }
    #[inline]
    fn read_wide<const N: usize>(&mut self, address: u32) -> Result<[u8; N], BusError> {
        if let Some(range) = self.wide_ram_range(address, N as u32)
            && let Some(bytes) = self.ram.view(range.clone())
        {
            #[cfg(feature = "profiling")]
            {
                // Count the same BDA bytes as the reference byte path.
                self.stats.bda_tick_reads +=
                    range.end.min(0x470).saturating_sub(range.start.max(0x46C)) as u64;
            }
            return Ok(bytes.try_into().expect("wide RAM range length"));
        }
        let mut bytes = [0; N];
        for (offset, byte) in bytes.iter_mut().enumerate() {
            *byte = self.read_u8(address.wrapping_add(offset as u32))?;
        }
        Ok(bytes)
    }
    #[inline]
    fn write_wide<const N: usize>(&mut self, address: u32, bytes: [u8; N]) -> Result<(), BusError> {
        if let Some(range) = self.wide_ram_range(address, N as u32)
            && let Some(ram) = self.ram.view_mut(range)
        {
            ram.copy_from_slice(&bytes);
            return Ok(());
        }
        for (offset, byte) in bytes.into_iter().enumerate() {
            self.write_u8(address.wrapping_add(offset as u32), byte)?;
        }
        Ok(())
    }
}
impl MemoryBus for PcBus {
    fn code_check(&self, address: u32, length: u8) -> Option<doslet_bus::CodeCheck> {
        if length == 0 {
            return None;
        }
        // Physical pages depend only on the address, length and A20 state, which
        // the RAM's code space tracks; hits need not translate again.
        self.ram
            .code_check(self.wide_ram_range(address, u32::from(length))?)
    }
    #[inline]
    fn code_valid(&self, check: &doslet_bus::CodeCheck) -> bool {
        self.ram.code_valid(check)
    }
    fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
        // Decoding may inspect a short view at RAM/MMIO/A20 boundaries, but
        // never touches a device or changes guest state on a failed attempt.
        let start = self.a20.address(address);
        if start >= crate::memory::RAM_SIZE as u32
            || (APERTURE_START..APERTURE_START + APERTURE_LEN as u32).contains(&start)
        {
            return None;
        }
        let mut length = u32::from(length).min(crate::memory::RAM_SIZE as u32 - start);
        if start < APERTURE_START {
            length = length.min(APERTURE_START - start);
        }
        if !self.a20.enabled {
            length = length.min(0x100000 - (address & 0xFFFFF));
        }
        self.ram
            .view(start as usize..start as usize + length as usize)
    }
    #[inline]
    fn read_u16(&mut self, address: u32) -> Result<u16, BusError> {
        self.read_wide(address).map(u16::from_le_bytes)
    }
    #[inline]
    fn read_u32(&mut self, address: u32) -> Result<u32, BusError> {
        self.read_wide(address).map(u32::from_le_bytes)
    }
    #[inline]
    fn write_u16(&mut self, address: u32, value: u16) -> Result<(), BusError> {
        self.write_wide(address, value.to_le_bytes())
    }
    #[inline]
    fn write_u32(&mut self, address: u32, value: u32) -> Result<(), BusError> {
        self.write_wide(address, value.to_le_bytes())
    }
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        let address = self.a20.address(address);
        #[cfg(feature = "profiling")]
        if (0x46C..0x470).contains(&address) {
            self.stats.bda_tick_reads += 1;
        }
        if (APERTURE_START..APERTURE_START + APERTURE_LEN as u32).contains(&address) {
            self.vga
                .read_memory((address - APERTURE_START) as usize)
                .ok_or(BusError::UnmappedMemory(address))
        } else {
            self.ram.read(address)
        }
    }
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        let address = self.a20.address(address);
        if (APERTURE_START..APERTURE_START + APERTURE_LEN as u32).contains(&address) {
            if self
                .vga
                .write_memory((address - APERTURE_START) as usize, value)
            {
                Ok(())
            } else {
                Err(BusError::UnmappedMemory(address))
            }
        } else {
            self.ram.write(address, value)
        }
    }
}
impl PortBus for PcBus {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
        if port == 0x61 {
            return Ok(self.system_control | (u8::from(self.pit.output(2).unwrap()) << 5));
        }
        #[cfg(feature = "profiling")]
        {
            self.stats.port_reads += 1;
            self.stats.vga_status_reads += u64::from(matches!(port, 0x3BA | 0x3DA));
            self.stats.pit_reads += u64::from((0x40..=0x42).contains(&port));
        }
        match port {
            0x3C6..=0x3C8 => Ok(self.vga.read_port(port).unwrap()),
            0x3CD => Ok(0xFF), // No Tseng ET3000/ET4000 segment-select register.
            0x3C9 => Ok(self.vga.read_dac()),
            0..=0x0F => self.dma[0]
                .read_register(port as u8)
                .map_err(|_| BusError::UnmappedPort(port)),
            0xC0..=0xDE if port & 1 == 0 => self.dma[1]
                .read_register(((port - 0xC0) / 2) as u8)
                .map_err(|_| BusError::UnmappedPort(port)),
            0x81 | 0x82 | 0x83 | 0x87 | 0x89 | 0x8A | 0x8B | 0x8F => {
                Ok(self.dma_pages[Self::dma_page_channel(port)])
            }
            0x224 => Ok(self.mixer.index()),
            0x225 => {
                self.mixer.set_interrupt_status(self.dsp.irq_status());
                self.mixer.read().map_err(|_| BusError::UnmappedPort(port))
            }
            0x226 => Ok(0xFF), // DSP reset is write-only.
            0x22A => Ok(self.dsp.read_data()),
            0x22C => Ok(self.dsp.write_status()),
            0x22E => {
                self.dsp.acknowledge_irq(false);
                Ok(self.dsp.read_status())
            }
            0x22F => {
                self.dsp.acknowledge_irq(true);
                Ok(0xFF)
            }
            // The emulated DSP is fixed at 220h. DOS setup programs probe
            // common legacy Sound Blaster bases; unclaimed ISA reads float high.
            0x210..=0x21F
            | 0x230..=0x23F
            | 0x240..=0x24F
            | 0x250..=0x25F
            | 0x260..=0x26F
            | 0x280..=0x28F => Ok(0xFF),
            // No OPTi/CS4231-compatible WSS codec is installed. These are
            // documented codec decode windows used by DOS setup probes.
            0x530..=0x537 | 0x608..=0x60B | 0xE84..=0xE87 | 0xF44..=0xF47 => Ok(0xFF),
            // No AWE/EMU8000 is installed. Its probe ranges float high on
            // this ISA machine; they must not identify a nonexistent synth.
            0x600..=0x6FF | 0xA00..=0xAFF | 0xE00..=0xEFF => Ok(0xFF),
            0x3BA | 0x3DA => {
                let value = self
                    .vga
                    .read_status(port)
                    .ok_or(BusError::UnmappedPort(port))?;
                if self.observe_vga_status {
                    self.vga_status_read_generation =
                        self.vga_status_read_generation.wrapping_add(1);
                }
                Ok(value)
            }
            0x3B4 | 0x3B5 | 0x3D4 | 0x3D5 | 0x3C0 | 0x3C1 | 0x3C4 | 0x3C5 | 0x3CC | 0x3CE
            | 0x3CF => self.vga.read_port(port).ok_or(BusError::UnmappedPort(port)),
            0x70 => Ok(self.cmos.selected()),
            0x71 => Ok(self.cmos.read()),
            0x60 => Ok(self.keyboard.read_data()),
            0x64 => Ok(self.keyboard.status()),
            0x92 => Ok(if self.a20.enabled { 2 } else { 0 }),
            0x330 => Ok(self.mpu.read_data()),
            0x331 => Ok(self.mpu.status()),
            0x320..=0x321 => Ok(0xFF), // No device at the probed 320h base.
            0xA0 => Ok(self.slave_pic.read_command()),
            0xA1 => Ok(self.slave_pic.read_data()),
            0x20 => Ok(self.pic.read_command()),
            0x21 => Ok(self.pic.read_data()),
            0x40..=0x42 => self
                .pit
                .read_counter((port - 0x40) as u8)
                .map_err(|_| BusError::UnmappedPort(port)),
            _ => Err(BusError::UnmappedPort(port)),
        }
    }
    fn write_port_u8(&mut self, port: u16, value: u8) -> Result<(), BusError> {
        if port == 0x61 {
            self.pit.set_gate(2, value & 1 != 0).unwrap();
            self.system_control = value & 0x0F;
            return Ok(());
        }
        match port {
            0x3CD => {} // No Tseng vendor bank register on the generic VBE adapter.
            0x3C6 => {
                self.vga.write_port(port, value);
            }
            0x3C7 => self.vga.set_dac_read_index(value),
            0..=0x0F => self.dma[0]
                .write_register(port as u8, value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            0xC0..=0xDE if port & 1 == 0 => self.dma[1]
                .write_register(((port - 0xC0) / 2) as u8, value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            0x81 | 0x82 | 0x83 | 0x87 | 0x89 | 0x8A | 0x8B | 0x8F => {
                self.dma_pages[Self::dma_page_channel(port)] = value
            }
            0x224 => self.mixer.select(value),
            0x225 => self
                .mixer
                .write(value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            0x226 => self.dsp.write_reset(value),
            0x22C => self
                .dsp
                .write_command(value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            0x210..=0x21F
            | 0x230..=0x23F
            | 0x240..=0x24F
            | 0x250..=0x25F
            | 0x260..=0x26F
            | 0x280..=0x28F => {} // No DSP at these candidate bases.
            0x320..=0x321 => {} // No device at the probed 320h base.
            0x530..=0x537 | 0x608..=0x60B | 0xE84..=0xE87 | 0xF44..=0xF47 => {} // No WSS codec is installed.
            0x600..=0x6FF | 0xA00..=0xAFF | 0xE00..=0xEFF => {
                self.awe_writes += 1;
            } // Absent AWE probe banks.
            0x3B4 | 0x3B5 | 0x3D4 | 0x3D5 | 0x3C0 | 0x3C2 | 0x3C4 | 0x3C5 | 0x3CE | 0x3CF => {
                if !self.vga.write_port(port, value) {
                    return Err(BusError::UnmappedPort(port));
                }
            }
            0x70 => self.cmos.select(value),
            0x71 => self.cmos.write(value),
            0x60 => {
                if let Some(lines) = self
                    .keyboard
                    .write_data(value)
                    .map_err(|_| BusError::UnmappedPort(port))?
                {
                    if lines.reset_requested {
                        return Err(BusError::UnmappedPort(port));
                    }
                    self.set_a20(lines.a20_enabled);
                }
            }
            0x64 => self
                .keyboard
                .write_command(value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            0x92 => {
                if value & 1 != 0 {
                    return Err(BusError::UnmappedPort(port));
                }
                self.set_a20(value & 2 != 0);
            }
            0x330 => self
                .mpu
                .write_data(value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            0x331 => self
                .mpu
                .write_command(value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            0xA0 => self.slave_pic.write_command(value),
            0xA1 => self.slave_pic.write_data(value),
            0x20 => self.pic.write_command(value),
            0x21 => self.pic.write_data(value),
            0x40..=0x42 => self
                .pit
                .write_counter((port - 0x40) as u8, value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            0x43 => self
                .pit
                .write_control(value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            0x3C8 => self.vga.set_dac_index(value),
            0x3C9 => self.vga.write_dac(value),
            _ => return Err(BusError::UnmappedPort(port)),
        }
        Ok(())
    }
}

#[cfg(test)]
mod cache_tests;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_tseng_bank_register_does_not_echo_probe_patterns_or_change_vbe_bank() {
        let mut bus = PcBus::default();
        for pattern in [0x55, 0xAA, 0xFF] {
            bus.write_port_u8(0x3CD, pattern).unwrap();
            assert_eq!(bus.read_port_u8(0x3CD), Ok(0xFF));
        }
    }
    #[test]
    fn channel_two_mode_zero_counts_guest_ticks_with_port_61_gate_and_output() {
        let mut machine = crate::PcMachine::default();
        machine.bus.write_port_u8(0x43, 0xB0).unwrap();
        machine.bus.write_port_u8(0x42, 3).unwrap();
        machine.bus.write_port_u8(0x42, 0).unwrap();
        machine.advance_pit_ticks(100);
        assert_eq!(machine.bus.read_port_u8(0x61), Ok(0));
        machine.bus.write_port_u8(0x61, 3).unwrap();
        machine.advance_pit_ticks(2);
        assert_eq!(machine.bus.read_port_u8(0x61), Ok(3));
        machine.bus.write_port_u8(0x43, 0x80).unwrap();
        machine.advance_pit_ticks(1);
        assert_eq!(machine.bus.read_port_u8(0x42), Ok(1)); // Latched before terminal count.
        assert_eq!(machine.bus.read_port_u8(0x42), Ok(0));
        assert_eq!(machine.bus.read_port_u8(0x61), Ok(0x23));
        assert!(!machine.bus.pic.has_interrupt()); // Channel 2 is not IRQ0.
    }
    #[test]
    fn slave_pic_ports_keep_masks_and_initialization_independent_of_master() {
        let mut bus = PcBus::default();
        assert_eq!(bus.read_port_u8(0xA1), Ok(0xFF));
        bus.write_port_u8(0x21, 0xFC).unwrap();
        bus.write_port_u8(0xA1, 0x80).unwrap();
        assert_eq!(bus.read_port_u8(0x21), Ok(0xFC));
        assert_eq!(bus.read_port_u8(0xA1), Ok(0x80));
        bus.write_port_u8(0xA0, 0x11).unwrap();
        for byte in [0x70, 2, 1, 0xFB] {
            bus.write_port_u8(0xA1, byte).unwrap();
        }
        assert_eq!(bus.read_port_u8(0xA1), Ok(0xFB));
        bus.slave_pic.raise_edge(2);
        assert_eq!(bus.slave_pic.acknowledge(), Some(0x72));
        bus.write_port_u8(0xA0, 0x0B).unwrap();
        assert_eq!(bus.read_port_u8(0xA0), Ok(4));
        bus.write_port_u8(0xA0, 0x20).unwrap();
        assert_eq!(bus.read_port_u8(0xA0), Ok(0));
        assert!(!bus.pic.has_interrupt()); // Slave IRQ sources/cascade wiring remain pending.
    }
    #[test]
    fn wide_memory_matches_byte_accesses_at_mapping_boundaries() {
        let mut wide = PcBus::default();
        let mut bytes = PcBus::default();
        wide.vga.set_mode_13h();
        bytes.vga.set_mode_13h();
        for a20 in [false, true] {
            wide.set_a20(a20);
            bytes.set_a20(a20);
            for boundary in [
                0x100,
                0x46C,
                0x470,
                APERTURE_START,
                APERTURE_START + APERTURE_LEN as u32,
                APERTURE_START | (1 << 20),
                (APERTURE_START + APERTURE_LEN as u32) | (1 << 20),
                0x100000,
                0x200000,
                0x300000,
                crate::memory::RAM_SIZE as u32,
                u32::MAX,
            ] {
                for delta in -4_i32..=4 {
                    let address = boundary.wrapping_add_signed(delta);
                    for length in [2, 4] {
                        let value = 0xB1C2D3E4_u32;
                        let fast = if length == 2 {
                            wide.write_u16(address, value as u16)
                        } else {
                            wide.write_u32(address, value)
                        };
                        let slow = (0..length).try_for_each(|offset| {
                            bytes.write_u8(
                                address.wrapping_add(offset),
                                (value >> (offset * 8)) as u8,
                            )
                        });
                        assert_eq!(fast, slow, "write {address:#x}, {length}, A20={a20}");
                        let fast = if length == 2 {
                            wide.read_u16(address).map(u32::from)
                        } else {
                            wide.read_u32(address)
                        };
                        let slow = (0..length).try_fold(0_u32, |value, offset| {
                            bytes
                                .read_u8(address.wrapping_add(offset))
                                .map(|byte| value | (u32::from(byte) << (offset * 8)))
                        });
                        assert_eq!(fast, slow, "read {address:#x}, {length}, A20={a20}");
                        // Also verify successful bytes preceding a failing access.
                        for offset in 0..length {
                            assert_eq!(
                                wide.read_u8(address.wrapping_add(offset)),
                                bytes.read_u8(address.wrapping_add(offset))
                            );
                        }
                    }
                }
            }
            assert_eq!(
                wide.ram.view(0..crate::memory::RAM_SIZE),
                bytes.ram.view(0..crate::memory::RAM_SIZE)
            );
            assert_eq!(
                wide.vga.frame().unwrap().revision,
                bytes.vga.frame().unwrap().revision
            );
            #[cfg(feature = "profiling")]
            assert_eq!(wide.stats.bda_tick_reads, bytes.stats.bda_tick_reads);
        }
    }
    #[test]
    fn word_dma_wraps_at_128k_and_ignores_the_cpu_a20_gate() {
        let mut bus = PcBus::default();
        bus.set_a20(true);
        bus.write_u16(0x11_FFFE, 0x1234).unwrap();
        bus.write_u16(0x10_0000, 0x5678).unwrap();
        bus.set_a20(false);
        bus.write_port_u8(0x8B, 0x11).unwrap(); // Page bit 0 is ignored by 16-bit DMA.
        bus.write_port_u8(0xD8, 0).unwrap();
        for value in [0xFF, 0xFF] {
            bus.write_port_u8(0xC4, value).unwrap();
        }
        for value in [1, 0] {
            bus.write_port_u8(0xC6, value).unwrap();
        }
        bus.write_port_u8(0xD6, 0x49).unwrap();
        bus.write_port_u8(0xD4, 1).unwrap();
        assert_eq!(bus.dma_sample(5).unwrap(), Some(0x1234));
        assert_eq!(bus.dma_sample(5).unwrap(), Some(0x5678)); // Wrap stays in the same 128-KiB page.
        assert_eq!(bus.dma_sample(5).unwrap(), None);
        assert_eq!(bus.read_port_u8(0xD0).unwrap() & 15, 2);
        assert_eq!(bus.read_port_u8(0x8B), Ok(0x11));
    }
    #[test]
    fn wide_vga_io_selects_then_writes_and_absent_awe_probes_float_high() {
        let mut bus = PcBus::default();
        bus.write_port_u16(0x3C4, 0x0F02).unwrap();
        assert_eq!(bus.read_port_u16(0x3C4), Ok(0x0F02));
        bus.write_port_u16(0xE22, 0xE0).unwrap();
        assert_eq!(bus.read_port_u16(0xE20), Ok(0xFFFF));
        assert_eq!(
            bus.read_port_u8(0xFFFF),
            Err(BusError::UnmappedPort(0xFFFF))
        );
    }
    #[test]
    fn a20_is_a_line_gate_and_keyboard_and_fast_port_share_its_signal() {
        let mut bus = PcBus::default();
        bus.write_u8(0, 0x11).unwrap();
        assert_eq!(bus.read_u8(0x100000).unwrap(), 0x11);
        bus.write_u8(0x200000, 0x33).unwrap();
        assert_eq!(bus.read_u8(0x200000).unwrap(), 0x33);
        assert!(bus.ram_view(0x100000..0x100001).is_none());
        bus.write_port_u8(0x92, 2).unwrap();
        bus.write_u16(0xFFFFF, 0xBBAA).unwrap();
        assert_eq!(bus.read_u16(0xFFFFF).unwrap(), 0xBBAA);
        bus.write_port_u8(0x64, 0xD0).unwrap();
        assert_eq!(bus.read_port_u8(0x60).unwrap(), 3);
        bus.write_port_u8(0x64, 0xD1).unwrap();
        bus.write_port_u8(0x60, 1).unwrap();
        assert!(!bus.a20_enabled());
        assert_eq!(bus.read_u16(0xFFFFF).unwrap(), 0x11AA);
        assert_eq!(bus.read_port_u8(0x92).unwrap(), 0);
        assert_eq!(
            bus.write_port_u8(0x92, 1),
            Err(BusError::UnmappedPort(0x92))
        );
    }
    #[test]
    fn word_crossing_ram_and_mmio_keeps_device_side_effects() {
        let mut bus = PcBus::default();
        bus.vga.set_mode_13h();
        bus.write_u16(APERTURE_START - 1, 0x1234).unwrap();
        assert_eq!(bus.read_u8(APERTURE_START - 1).unwrap(), 0x34);
        assert_eq!(bus.vga.frame().unwrap().pixels[0], 0x12);
        assert!(bus.ram_view(0x9FFFF..0xA0001).is_none());
        assert_eq!(bus.ram_view(0x9FFFF..0xA0000).unwrap(), &[0x34]);
    }
    #[test]
    fn port_routing_connects_programmed_pit_to_pic() {
        let mut machine = crate::PcMachine::default();
        machine.bus.write_port_u8(0x43, 0x34).unwrap();
        machine.bus.write_port_u8(0x40, 2).unwrap();
        machine.bus.write_port_u8(0x40, 0).unwrap();
        machine.advance_pit_ticks(2);
        assert_eq!(machine.bus.pic.acknowledge(), Some(8));
        assert!(machine.bus.write_port_u8(0xFFFF, 0).is_err());
    }
}
