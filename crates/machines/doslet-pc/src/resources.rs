//! Setup-only exclusive ISA claims compiled into constant-time concrete routes.
use alloc::{boxed::Box, vec};
use doslet_config::HardwareConfig;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemoryEndpoint {
    Ram,
    Vga,
    Absent,
}
pub(crate) struct MemoryRoutes {
    pages: Box<[MemoryEndpoint]>,
}
impl MemoryRoutes {
    pub fn new(bytes: usize) -> Self {
        let mut pages = vec![MemoryEndpoint::Ram; bytes / 4096].into_boxed_slice();
        let start = doslet_vga::APERTURE_START as usize / 4096;
        let end = (doslet_vga::APERTURE_START as usize + doslet_vga::APERTURE_LEN) / 4096;
        pages[start..end].fill(MemoryEndpoint::Vga);
        Self { pages }
    }
    #[inline]
    pub fn endpoint(&self, address: u32) -> MemoryEndpoint {
        self.pages
            .get(address as usize / 4096)
            .copied()
            .unwrap_or(MemoryEndpoint::Absent)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceId {
    Motherboard,
    Vga,
    Sb16,
    Mpu401,
    Gameport,
    Probe,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceError {
    Port {
        port: u16,
        first: DeviceId,
        second: DeviceId,
    },
    Irq {
        line: u8,
        first: DeviceId,
        second: DeviceId,
    },
    Dma {
        channel: u8,
        first: DeviceId,
        second: DeviceId,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Endpoint {
    Absent,
    Probe,
    AweProbe,
    Vga,
    VgaStatus,
    Dma0,
    Dma1,
    DmaPage,
    MixerIndex,
    MixerData,
    DspReset,
    DspData,
    DspCommand,
    DspStatus,
    DspWordAck,
    CmosIndex,
    CmosData,
    KeyboardData,
    KeyboardCommand,
    SystemControl,
    A20,
    MidiData,
    MidiCommand,
    PicCommand,
    PicData,
    SlavePicCommand,
    SlavePicData,
    PitData,
    PitControl,
    Gameport,
    Opl,
}
const ENDPOINTS: [Endpoint; 31] = [
    Endpoint::Absent,
    Endpoint::Probe,
    Endpoint::AweProbe,
    Endpoint::Vga,
    Endpoint::VgaStatus,
    Endpoint::Dma0,
    Endpoint::Dma1,
    Endpoint::DmaPage,
    Endpoint::MixerIndex,
    Endpoint::MixerData,
    Endpoint::DspReset,
    Endpoint::DspData,
    Endpoint::DspCommand,
    Endpoint::DspStatus,
    Endpoint::DspWordAck,
    Endpoint::CmosIndex,
    Endpoint::CmosData,
    Endpoint::KeyboardData,
    Endpoint::KeyboardCommand,
    Endpoint::SystemControl,
    Endpoint::A20,
    Endpoint::MidiData,
    Endpoint::MidiCommand,
    Endpoint::PicCommand,
    Endpoint::PicData,
    Endpoint::SlavePicCommand,
    Endpoint::SlavePicData,
    Endpoint::PitData,
    Endpoint::PitControl,
    Endpoint::Gameport,
    Endpoint::Opl,
];
impl Endpoint {
    fn id(self) -> u16 {
        ENDPOINTS.iter().position(|e| *e == self).unwrap() as u16
    }
}
pub(crate) struct Routes {
    read: Box<[u16]>,
    write: Box<[u16]>,
    pub irq: [Option<DeviceId>; 16],
    pub dma: [Option<DeviceId>; 8],
}
impl Routes {
    pub fn build(config: HardwareConfig) -> Result<Self, ResourceError> {
        let mut this = Self {
            read: vec![0; 65536].into_boxed_slice(),
            write: vec![0; 65536].into_boxed_slice(),
            irq: [None; 16],
            dma: [None; 8],
        };
        // Probe fallbacks are explicitly weaker than any installed endpoint.
        for range in [
            0x210..=0x21F,
            0x230..=0x23F,
            0x240..=0x24F,
            0x250..=0x25F,
            0x260..=0x26F,
            0x280..=0x28F,
            0x320..=0x321,
            0x530..=0x537,
            0x608..=0x60B,
            0xE84..=0xE87,
            0xF44..=0xF47,
        ] {
            for port in range {
                this.fallback(port, Endpoint::Probe);
            }
        }
        for range in [0x600..=0x6FF, 0xA00..=0xAFF, 0xE00..=0xEFF] {
            for port in range {
                this.fallback(port, Endpoint::AweProbe);
            }
        }
        this.fallback(0x3CD, Endpoint::Probe);
        // Pro Audio Spectrum audio-filter register. Some AdLib drivers clear
        // it before probing OPL; this machine has no PAS card to receive it.
        this.fallback(0xB8A, Endpoint::Probe);
        for port in 0..=15 {
            this.claim_port(
                DeviceId::Motherboard,
                port,
                Some(Endpoint::Dma0),
                Some(Endpoint::Dma0),
            )?;
        }
        for port in (0xC0..=0xDE).step_by(2) {
            this.claim_port(
                DeviceId::Motherboard,
                port,
                Some(Endpoint::Dma1),
                Some(Endpoint::Dma1),
            )?;
        }
        for port in [0x81, 0x82, 0x83, 0x87, 0x89, 0x8A, 0x8B, 0x8F] {
            this.claim_port(
                DeviceId::Motherboard,
                port,
                Some(Endpoint::DmaPage),
                Some(Endpoint::DmaPage),
            )?;
        }
        for (port, endpoint) in [
            (0x20, Endpoint::PicCommand),
            (0x21, Endpoint::PicData),
            (0xA0, Endpoint::SlavePicCommand),
            (0xA1, Endpoint::SlavePicData),
            (0x60, Endpoint::KeyboardData),
            (0x64, Endpoint::KeyboardCommand),
            (0x61, Endpoint::SystemControl),
            (0x70, Endpoint::CmosIndex),
            (0x71, Endpoint::CmosData),
            (0x92, Endpoint::A20),
        ] {
            this.claim_port(DeviceId::Motherboard, port, Some(endpoint), Some(endpoint))?;
        }
        for port in 0x40..=0x42 {
            this.claim_port(
                DeviceId::Motherboard,
                port,
                Some(Endpoint::PitData),
                Some(Endpoint::PitData),
            )?;
        }
        this.claim_port(
            DeviceId::Motherboard,
            0x43,
            None,
            Some(Endpoint::PitControl),
        )?;
        for port in [
            0x3B4, 0x3B5, 0x3D4, 0x3D5, 0x3C0, 0x3C1, 0x3C2, 0x3C4, 0x3C5, 0x3C6, 0x3C7, 0x3C8,
            0x3C9, 0x3CC, 0x3CE, 0x3CF,
        ] {
            this.claim_port(
                DeviceId::Vga,
                port,
                Some(Endpoint::Vga),
                Some(Endpoint::Vga),
            )?;
        }
        for port in [0x3BA, 0x3DA] {
            this.claim_port(DeviceId::Vga, port, Some(Endpoint::VgaStatus), None)?;
        }
        for line in [0, 1, 2, 8, 13] {
            this.claim_irq(DeviceId::Motherboard, line)?;
        }
        for channel in [2, 4] {
            this.claim_dma(DeviceId::Motherboard, channel)?;
        }
        if config.sb16 {
            for port in (0x220..=0x223).chain(0x228..=0x229).chain(0x388..=0x38B) {
                this.claim_port(
                    DeviceId::Sb16,
                    port,
                    Some(Endpoint::Opl),
                    Some(Endpoint::Opl),
                )?;
            }
            for (port, endpoint, write) in [
                (0x224, Endpoint::MixerIndex, true),
                (0x225, Endpoint::MixerData, true),
                (0x226, Endpoint::DspReset, true),
                (0x22A, Endpoint::DspData, false),
                (0x22C, Endpoint::DspCommand, true),
                (0x22E, Endpoint::DspStatus, false),
                (0x22F, Endpoint::DspWordAck, false),
            ] {
                this.claim_port(
                    DeviceId::Sb16,
                    port,
                    Some(endpoint),
                    if write { Some(endpoint) } else { None },
                )?;
            }
            this.claim_irq(DeviceId::Sb16, config.sb_irq)?;
            this.claim_dma(DeviceId::Sb16, config.sb_dma8)?;
            this.claim_dma(DeviceId::Sb16, config.sb_dma16)?;
        }
        if config.mpu401 {
            this.claim_port(
                DeviceId::Mpu401,
                0x330,
                Some(Endpoint::MidiData),
                Some(Endpoint::MidiData),
            )?;
            this.claim_port(
                DeviceId::Mpu401,
                0x331,
                Some(Endpoint::MidiCommand),
                Some(Endpoint::MidiCommand),
            )?;
        }
        if config.gameport {
            this.claim_port(
                DeviceId::Gameport,
                0x201,
                Some(Endpoint::Gameport),
                Some(Endpoint::Gameport),
            )?;
        }
        Ok(this)
    }
    fn fallback(&mut self, port: u16, endpoint: Endpoint) {
        let id = endpoint.id();
        if self.read[port as usize] <= 2 {
            self.read[port as usize] = id;
        }
        if self.write[port as usize] <= 2 {
            self.write[port as usize] = id;
        }
    }
    fn owner(endpoint: Endpoint) -> DeviceId {
        match endpoint {
            Endpoint::Vga | Endpoint::VgaStatus => DeviceId::Vga,
            Endpoint::MixerIndex
            | Endpoint::Opl
            | Endpoint::MixerData
            | Endpoint::DspReset
            | Endpoint::DspData
            | Endpoint::DspCommand
            | Endpoint::DspStatus
            | Endpoint::DspWordAck => DeviceId::Sb16,
            Endpoint::MidiData | Endpoint::MidiCommand => DeviceId::Mpu401,
            Endpoint::Gameport => DeviceId::Gameport,
            Endpoint::Absent | Endpoint::Probe | Endpoint::AweProbe => DeviceId::Probe,
            _ => DeviceId::Motherboard,
        }
    }
    fn claim_port(
        &mut self,
        owner: DeviceId,
        port: u16,
        read: Option<Endpoint>,
        write: Option<Endpoint>,
    ) -> Result<(), ResourceError> {
        for (table, endpoint) in [(&self.read, read), (&self.write, write)] {
            if endpoint.is_some() && table[port as usize] > 2 {
                return Err(ResourceError::Port {
                    port,
                    first: Self::owner(ENDPOINTS[table[port as usize] as usize]),
                    second: owner,
                });
            }
        }
        if let Some(e) = read {
            self.read[port as usize] = e.id();
        }
        if let Some(e) = write {
            self.write[port as usize] = e.id();
        }
        Ok(())
    }
    fn claim_irq(&mut self, owner: DeviceId, line: u8) -> Result<(), ResourceError> {
        if let Some(first) = self.irq[line as usize] {
            return Err(ResourceError::Irq {
                line,
                first,
                second: owner,
            });
        }
        self.irq[line as usize] = Some(owner);
        Ok(())
    }
    fn claim_dma(&mut self, owner: DeviceId, channel: u8) -> Result<(), ResourceError> {
        if let Some(first) = self.dma[channel as usize] {
            return Err(ResourceError::Dma {
                channel,
                first,
                second: owner,
            });
        }
        self.dma[channel as usize] = Some(owner);
        Ok(())
    }
    #[inline]
    pub fn read(&self, port: u16) -> Endpoint {
        ENDPOINTS[self.read[port as usize] as usize]
    }
    #[inline]
    pub fn write(&self, port: u16) -> Endpoint {
        ENDPOINTS[self.write[port as usize] as usize]
    }
    pub fn allows_irq(&self, line: u8) -> bool {
        self.irq.get(line as usize) == Some(&Some(DeviceId::Sb16))
    }
    pub fn allows_dma(&self, channel: u8) -> bool {
        self.dma.get(channel as usize) == Some(&Some(DeviceId::Sb16))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlap_fails_before_partial_claim_and_fallback_is_weaker() {
        let mut r = Routes::build(HardwareConfig::default()).unwrap();
        assert_eq!(
            r.claim_port(DeviceId::Gameport, 0x230, Some(Endpoint::Gameport), None),
            Ok(())
        );
        assert_eq!(r.read(0x230), Endpoint::Gameport);
        assert!(matches!(
            r.claim_port(DeviceId::Gameport, 0x224, Some(Endpoint::Gameport), None),
            Err(ResourceError::Port {
                first: DeviceId::Sb16,
                ..
            })
        ));
        assert_eq!(r.read(0x224), Endpoint::MixerIndex);
        assert!(r.claim_irq(DeviceId::Sb16, 1).is_err());
        assert!(r.claim_dma(DeviceId::Sb16, 2).is_err());
    }
}
