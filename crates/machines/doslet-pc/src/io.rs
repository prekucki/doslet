use crate::memory::{A20Gate, Ram};
use crate::resources::{Endpoint, MemoryEndpoint, MemoryRoutes, Routes};
use core::ops::Range;
use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_config::{HardwareConfig, KeyboardMode};
use doslet_i8042::Controller;
use doslet_i8254::Pit;
use doslet_i8259::Pic;
use doslet_mc146818::Cmos;
use doslet_mpu401::Mpu401;
use doslet_vga::{APERTURE_LEN, APERTURE_START, Vga};

pub struct PcBus {
    config: HardwareConfig,
    routes: Routes,
    memory_routes: MemoryRoutes,
    pub gameport: doslet_gameport::Gameport,
    pub(crate) guest_nanos: u64,
    keyboard_irq_line: bool,
    /// CPU-independent notification generation. Only successful active VGA
    /// status reads increment it; the PC compares before/after one CPU step.
    pub(crate) vga_status_read_generation: u32,
    /// Port and device-memory accesses, for idle proofs to reject passes
    /// whose results could depend on device state.
    pub(crate) device_accesses: u64,
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
        Self::from_config(HardwareConfig::default()).expect("valid default hardware")
    }
}
impl PcBus {
    pub fn from_config(config: HardwareConfig) -> Result<Self, crate::MachineError> {
        config.validate().map_err(crate::MachineError::Hardware)?;
        let routes = Routes::build(config).map_err(crate::MachineError::Resource)?;
        let mut pit = Pit::default();
        pit.write_control(0x36).unwrap();
        pit.write_counter(0, 0).unwrap();
        pit.write_counter(0, 0).unwrap();
        pit.set_gate(2, false).unwrap();
        let mut slave_pic = Pic::new(0x70);
        slave_pic.write_data(0xFF);
        Ok(Self {
            config,
            routes,
            memory_routes: MemoryRoutes::new(config.memory_bytes()),
            gameport: doslet_gameport::Gameport::default(),
            guest_nanos: 0,
            keyboard_irq_line: false,
            vga_status_read_generation: 0,
            device_accesses: 0,
            observe_vga_status: true,
            #[cfg(feature = "profiling")]
            stats: crate::ExecutionStats::default(),
            ram: Ram::new(config.memory_bytes()),
            vga: Vga::default(),
            pic: Pic::default(),
            slave_pic,
            pit,
            mpu: Mpu401::default(),
            dsp: doslet_sb_dsp::Dsp::default(),
            mixer: doslet_sb_mixer::Mixer::with_routing(
                config.sb_irq,
                config.sb_dma8,
                config.sb_dma16,
            ),
            dma: core::array::from_fn(|_| doslet_i8237::Dma::default()),
            dma_pages: [0; 8],
            pcm: crate::audio::PcmQueue::default(),
            keyboard: Controller::default(),
            a20: A20Gate::default(),
            cmos: Cmos::new(640, ((config.memory_bytes() - 0x100000) / 1024) as u16),
            system_control: 0,
            awe_writes: 0,
        })
    }
}
impl PcBus {
    pub fn hardware_config(&self) -> HardwareConfig {
        self.config
    }
    pub fn ram_size(&self) -> usize {
        self.ram.len()
    }
    pub fn read_pending_keyboard(&mut self) -> Option<u8> {
        if !self.keyboard.has_output() {
            return None;
        }
        let byte = self.keyboard.read_data();
        self.keyboard_irq_line = false;
        self.update_keyboard_irq();
        Some(byte)
    }
    pub(crate) fn update_keyboard_irq(&mut self) {
        let pending = self.config.keyboard == KeyboardMode::At && self.keyboard.irq_pending();
        if pending && !self.keyboard_irq_line {
            self.pic.raise_edge(1);
        }
        self.keyboard_irq_line = pending;
    }
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
    pub(crate) fn begin_ram_write_log(&mut self) {
        self.ram.begin_write_log();
    }
    pub(crate) fn end_ram_write_log(&mut self) -> bool {
        self.ram.end_write_log()
    }
    pub(crate) fn ram_writes(&self) -> u64 {
        self.ram.writes()
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
        if start > self.ram.len() as u32 - length
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
    fn native_ram_mut(&mut self) -> Option<doslet_bus::NativeRamMut<'_>> {
        if !self.a20.enabled {
            return None;
        }
        let size = self.ram.len();
        let mut view = self.ram.native_view_mut(0x500..size)?;
        view.excluded_start = APERTURE_START;
        view.excluded_len = APERTURE_LEN as u32;
        Some(view)
    }
    fn native_ram(&self) -> Option<doslet_bus::NativeRam<'_>> {
        // A20-on identity mapping. Exclude IVT/BDA by starting above 0x4ff,
        // and mark the VGA backing bytes as a forbidden interval. The native
        // complete-access guard must reject the interval before touching RAM.
        if !self.a20.enabled {
            return None;
        }
        let start = 0x500;
        Some(doslet_bus::NativeRam {
            linear_base: start as u32,
            bytes: self.ram.view(start..self.ram.len())?,
            excluded_start: APERTURE_START,
            excluded_len: APERTURE_LEN as u32,
        })
    }
    #[inline]
    fn is_plain_ram(&self, address: u32, length: u8) -> bool {
        length != 0 && self.wide_ram_range(address, u32::from(length)).is_some()
    }
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
        if start >= self.ram.len() as u32
            || (APERTURE_START..APERTURE_START + APERTURE_LEN as u32).contains(&start)
        {
            return None;
        }
        let mut length = u32::from(length).min(self.ram.len() as u32 - start);
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
        let endpoint = self.memory_routes.endpoint(address);
        if !matches!(endpoint, MemoryEndpoint::Ram) {
            self.device_accesses = self.device_accesses.wrapping_add(1);
        }
        match endpoint {
            MemoryEndpoint::Vga => self
                .vga
                .read_memory((address - APERTURE_START) as usize)
                .ok_or(BusError::UnmappedMemory(address)),
            MemoryEndpoint::Ram => self.ram.read(address),
            MemoryEndpoint::Absent => Err(BusError::UnmappedMemory(address)),
        }
    }
    fn write_u8(&mut self, address: u32, value: u8) -> Result<(), BusError> {
        let address = self.a20.address(address);
        let endpoint = self.memory_routes.endpoint(address);
        if !matches!(endpoint, MemoryEndpoint::Ram) {
            self.device_accesses = self.device_accesses.wrapping_add(1);
        }
        match endpoint {
            MemoryEndpoint::Vga => {
                if self
                    .vga
                    .write_memory((address - APERTURE_START) as usize, value)
                {
                    Ok(())
                } else {
                    Err(BusError::UnmappedMemory(address))
                }
            }
            MemoryEndpoint::Ram => self.ram.write(address, value),
            MemoryEndpoint::Absent => Err(BusError::UnmappedMemory(address)),
        }
    }
}
impl PortBus for PcBus {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
        self.device_accesses = self.device_accesses.wrapping_add(1);
        #[cfg(feature = "profiling")]
        {
            self.stats.port_reads += 1;
            self.stats.vga_status_reads += u64::from(matches!(port, 0x3BA | 0x3DA));
            self.stats.pit_reads += u64::from((0x40..=0x42).contains(&port));
        }
        match self.routes.read(port) {
            Endpoint::Absent => Err(BusError::UnmappedPort(port)),
            Endpoint::Probe | Endpoint::AweProbe | Endpoint::DspReset => Ok(0xFF),
            Endpoint::SystemControl => {
                Ok(self.system_control | (u8::from(self.pit.output(2).unwrap()) << 5))
            }
            Endpoint::Vga => {
                if port == 0x3C9 {
                    Ok(self.vga.read_dac())
                } else {
                    self.vga.read_port(port).ok_or(BusError::UnmappedPort(port))
                }
            }
            Endpoint::VgaStatus => {
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
            Endpoint::Dma0 => self.dma[0]
                .read_register(port as u8)
                .map_err(|_| BusError::UnmappedPort(port)),
            Endpoint::Dma1 => self.dma[1]
                .read_register(((port - 0xC0) / 2) as u8)
                .map_err(|_| BusError::UnmappedPort(port)),
            Endpoint::DmaPage => Ok(self.dma_pages[Self::dma_page_channel(port)]),
            Endpoint::MixerIndex => Ok(self.mixer.index()),
            Endpoint::MixerData => {
                self.mixer.set_interrupt_status(self.dsp.irq_status());
                self.mixer.read().map_err(|_| BusError::UnmappedPort(port))
            }
            Endpoint::DspData => Ok(self.dsp.read_data()),
            Endpoint::DspCommand => Ok(self.dsp.write_status()),
            Endpoint::DspStatus => {
                self.dsp.acknowledge_irq(false);
                Ok(self.dsp.read_status())
            }
            Endpoint::DspWordAck => {
                self.dsp.acknowledge_irq(true);
                Ok(0xFF)
            }
            Endpoint::CmosIndex => Ok(self.cmos.selected()),
            Endpoint::CmosData => Ok(self.cmos.read()),
            Endpoint::KeyboardData => Ok(self.read_pending_keyboard().unwrap_or(0xFF)),
            Endpoint::KeyboardCommand => Ok(self.keyboard.status()),
            Endpoint::A20 => Ok(if self.a20.enabled { 2 } else { 0 }),
            Endpoint::MidiData => Ok(self.mpu.read_data()),
            Endpoint::MidiCommand => Ok(self.mpu.status()),
            Endpoint::PicCommand => Ok(self.pic.read_command()),
            Endpoint::PicData => Ok(self.pic.read_data()),
            Endpoint::SlavePicCommand => Ok(self.slave_pic.read_command()),
            Endpoint::SlavePicData => Ok(self.slave_pic.read_data()),
            Endpoint::PitData => self
                .pit
                .read_counter((port - 0x40) as u8)
                .map_err(|_| BusError::UnmappedPort(port)),
            Endpoint::Gameport => Ok(self.gameport.read(self.guest_nanos)),
            Endpoint::PitControl => Err(BusError::UnmappedPort(port)),
        }
    }
    fn write_port_u8(&mut self, port: u16, value: u8) -> Result<(), BusError> {
        self.device_accesses = self.device_accesses.wrapping_add(1);
        match self.routes.write(port) {
            Endpoint::Absent => return Err(BusError::UnmappedPort(port)),
            Endpoint::Probe => {}
            Endpoint::AweProbe => self.awe_writes += 1,
            Endpoint::SystemControl => {
                self.pit.set_gate(2, value & 1 != 0).unwrap();
                self.system_control = value & 0x0F;
            }
            Endpoint::Vga => match port {
                0x3C7 => self.vga.set_dac_read_index(value),
                0x3C8 => self.vga.set_dac_index(value),
                0x3C9 => self.vga.write_dac(value),
                _ => {
                    if !self.vga.write_port(port, value) {
                        return Err(BusError::UnmappedPort(port));
                    }
                }
            },
            Endpoint::Dma0 => self.dma[0]
                .write_register(port as u8, value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            Endpoint::Dma1 => self.dma[1]
                .write_register(((port - 0xC0) / 2) as u8, value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            Endpoint::DmaPage => self.dma_pages[Self::dma_page_channel(port)] = value,
            Endpoint::MixerIndex => self.mixer.select(value),
            Endpoint::MixerData => {
                if let Some((irq, dma8, dma16)) = self.mixer.proposed_routing(value)
                    && (!irq.is_some_and(|v| self.routes.allows_irq(v))
                        || !dma8.is_some_and(|v| self.routes.allows_dma(v))
                        || !dma16.is_some_and(|v| self.routes.allows_dma(v)))
                {
                    return Err(BusError::UnmappedPort(port));
                }
                self.mixer
                    .write(value)
                    .map_err(|_| BusError::UnmappedPort(port))?;
            }
            Endpoint::DspReset => self.dsp.write_reset(value),
            Endpoint::DspCommand => self
                .dsp
                .write_command(value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            Endpoint::CmosIndex => self.cmos.select(value),
            Endpoint::CmosData => self.cmos.write(value),
            Endpoint::KeyboardData => {
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
                self.update_keyboard_irq();
            }
            Endpoint::KeyboardCommand => {
                self.keyboard
                    .write_command(value)
                    .map_err(|_| BusError::UnmappedPort(port))?;
                self.update_keyboard_irq();
            }
            Endpoint::A20 => {
                if value & 1 != 0 {
                    return Err(BusError::UnmappedPort(port));
                }
                self.set_a20(value & 2 != 0);
            }
            Endpoint::MidiData => self
                .mpu
                .write_data(value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            Endpoint::MidiCommand => self
                .mpu
                .write_command(value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            Endpoint::PicCommand => self.pic.write_command(value),
            Endpoint::PicData => self.pic.write_data(value),
            Endpoint::SlavePicCommand => self.slave_pic.write_command(value),
            Endpoint::SlavePicData => self.slave_pic.write_data(value),
            Endpoint::PitData => self
                .pit
                .write_counter((port - 0x40) as u8, value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            Endpoint::PitControl => self
                .pit
                .write_control(value)
                .map_err(|_| BusError::UnmappedPort(port))?,
            Endpoint::Gameport => self.gameport.trigger(self.guest_nanos),
            Endpoint::VgaStatus
            | Endpoint::DspData
            | Endpoint::DspStatus
            | Endpoint::DspWordAck => return Err(BusError::UnmappedPort(port)),
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
    fn native_write_view_requires_flat_a20_and_tracks_physical_pages() {
        let mut bus = PcBus::default();
        assert!(bus.native_ram_mut().is_none());
        bus.set_a20(true);
        let address = APERTURE_START + APERTURE_LEN as u32;
        let before = bus.code_check(address, 4).unwrap();
        {
            let view = bus.native_ram_mut().unwrap();
            assert_eq!(view.linear_base, 0x500);
            assert_eq!(view.bytes.len(), crate::memory::RAM_SIZE - 0x500);
            assert_eq!(view.excluded_start, APERTURE_START);
            assert_eq!(view.excluded_len, APERTURE_LEN as u32);
            assert_eq!(view.page_generations.len(), crate::memory::RAM_SIZE / 4096);
        }
        assert!(bus.code_valid(&before));
        {
            let view = bus.native_ram_mut().unwrap();
            let page = (address >> 12) as usize;
            assert_ne!(view.page_generations[page], u64::MAX);
            let offset = (address - view.linear_base) as usize;
            view.bytes[offset..offset + 4].copy_from_slice(&42u32.to_le_bytes());
            view.page_generations[page] += 1;
        }
        assert!(!bus.code_valid(&before));
        assert_eq!(bus.read_u32(address), Ok(42));
        bus.set_a20(false);
        assert!(bus.native_ram_mut().is_none());
    }
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
