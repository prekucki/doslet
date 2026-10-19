// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct PcPcBus {
    #[prost(message, optional, tag = "1")]
    config: Option<<HardwareConfig as State>::Wire>,
    #[prost(message, optional, tag = "4")]
    gameport: Option<<doslet_gameport::Gameport as State>::Wire>,
    #[prost(uint64, tag = "5")]
    guest_nanos: u64,
    #[prost(bool, tag = "6")]
    keyboard_irq_line: bool,
    // Legacy observer fields remain decodable. New snapshots use zero because
    // every dependent instruction/proof mark is recreated after restoration.
    #[prost(uint32, tag = "7")]
    vga_status_read_generation: u32,
    #[prost(uint64, tag = "8")]
    device_accesses: u64,
    #[prost(uint64, tag = "9")]
    cmos_scratch_writes: u64,
    #[prost(bool, tag = "10")]
    observe_vga_status: bool,
    #[prost(message, optional, tag = "12")]
    ram: Option<<Ram as State>::Wire>,
    #[prost(message, optional, tag = "13")]
    vga: Option<<Vga as State>::Wire>,
    #[prost(message, optional, tag = "14")]
    pic: Option<<Pic as State>::Wire>,
    #[prost(message, optional, tag = "15")]
    slave_pic: Option<<Pic as State>::Wire>,
    #[prost(message, optional, tag = "16")]
    pit: Option<<Pit as State>::Wire>,
    #[prost(message, optional, tag = "17")]
    mpu: Option<<Mpu401 as State>::Wire>,
    #[prost(message, optional, tag = "18")]
    dsp: Option<<doslet_sb_dsp::Dsp as State>::Wire>,
    #[prost(message, optional, tag = "19")]
    mixer: Option<<doslet_sb_mixer::Mixer as State>::Wire>,
    #[prost(message, optional, tag = "20")]
    opl: Option<<doslet_opl3::Opl3 as State>::Wire>,
    #[prost(message, repeated, tag = "21")]
    dma: Vec<<doslet_i8237::Dma as State>::Wire>,
    #[prost(bytes = "vec", tag = "22")]
    dma_pages: Vec<u8>,
    #[prost(message, optional, tag = "23")]
    pcm: Option<<crate::audio::PcmQueue as State>::Wire>,
    #[prost(message, repeated, tag = "24")]
    digital_pcm: Vec<<doslet_pcm::Frame as State>::Wire>,
    #[prost(message, optional, tag = "25")]
    a20: Option<<A20Gate as State>::Wire>,
    #[prost(message, optional, tag = "26")]
    keyboard: Option<<Controller as State>::Wire>,
    #[prost(message, optional, tag = "27")]
    cmos: Option<<Cmos as State>::Wire>,
    #[prost(uint32, tag = "28")]
    system_control: u32,
    #[prost(uint64, tag = "29")]
    awe_writes: u64,
    #[prost(uint32, optional, tag = "30")]
    audio_use: Option<u32>,
}
impl State for PcBus {
    type Wire = PcPcBus;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            config: Some(State::save(&self.config)),
            gameport: Some(State::save(&self.gameport)),
            guest_nanos: self.guest_nanos,
            keyboard_irq_line: self.keyboard_irq_line,
            vga_status_read_generation: 0,
            device_accesses: 0,
            cmos_scratch_writes: self.cmos_scratch_writes,
            observe_vga_status: self.observe_vga_status,
            ram: Some(State::save(&self.ram)),
            vga: Some(self.vga.save_with_memory(self.vram_view())),
            pic: Some(State::save(&self.pic)),
            slave_pic: Some(State::save(&self.slave_pic)),
            pit: Some(State::save(&self.pit)),
            mpu: Some(State::save(&self.mpu)),
            dsp: Some(State::save(&self.dsp)),
            mixer: Some(State::save(&self.mixer)),
            opl: Some(State::save(&self.opl)),
            dma: self.dma.iter().map(State::save).collect(),
            dma_pages: self.dma_pages.to_vec(),
            pcm: Some(State::save(&self.pcm)),
            digital_pcm: self.digital_pcm.iter().map(State::save).collect(),
            a20: Some(State::save(&self.a20)),
            keyboard: Some(State::save(&self.keyboard)),
            cmos: Some(State::save(&self.cmos)),
            system_control: u32::from(self.system_control),
            awe_writes: self.awe_writes,
            audio_use: Some(
                u32::from(self.audio_use.sb16)
                    | u32::from(self.audio_use.opl3) << 1
                    | u32::from(self.audio_use.mpu401) << 2,
            ),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = {
            let mut value = Self::from_config(HardwareConfig::load(doslet_state::required(
                w.config.clone(),
                "PC config",
            )?)?)
            .map_err(|_| InvalidState("PC hardware"))?;
            value.config = <HardwareConfig as State>::load(doslet_state::required(
                w.config,
                "PcPcBus.config",
            )?)?;
            value.gameport = <doslet_gameport::Gameport as State>::load(doslet_state::required(
                w.gameport,
                "PcPcBus.gameport",
            )?)?;
            value.guest_nanos = w.guest_nanos;
            value.keyboard_irq_line = w.keyboard_irq_line;
            value.vga_status_read_generation = 0;
            value.device_accesses = 0;
            value.cmos_scratch_writes = w.cmos_scratch_writes;
            value.observe_vga_status = w.observe_vga_status;
            value.ram = <Ram as State>::load(doslet_state::required(w.ram, "PcPcBus.ram")?)?;
            value.vga = <Vga as State>::load(doslet_state::required(w.vga, "PcPcBus.vga")?)?;
            value.pic = <Pic as State>::load(doslet_state::required(w.pic, "PcPcBus.pic")?)?;
            value.slave_pic =
                <Pic as State>::load(doslet_state::required(w.slave_pic, "PcPcBus.slave_pic")?)?;
            value.pit = <Pit as State>::load(doslet_state::required(w.pit, "PcPcBus.pit")?)?;
            value.mpu = <Mpu401 as State>::load(doslet_state::required(w.mpu, "PcPcBus.mpu")?)?;
            value.dsp =
                <doslet_sb_dsp::Dsp as State>::load(doslet_state::required(w.dsp, "PcPcBus.dsp")?)?;
            value.mixer = <doslet_sb_mixer::Mixer as State>::load(doslet_state::required(
                w.mixer,
                "PcPcBus.mixer",
            )?)?;
            value.opl =
                <doslet_opl3::Opl3 as State>::load(doslet_state::required(w.opl, "PcPcBus.opl")?)?;
            value.dma = doslet_state::narrow(
                w.dma
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { <doslet_i8237::Dma as State>::load(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "PcPcBus.dma length",
            )?;
            value.dma_pages = doslet_state::narrow(w.dma_pages, "PcPcBus.dma_pages length")?;
            value.pcm = <crate::audio::PcmQueue as State>::load(doslet_state::required(
                w.pcm,
                "PcPcBus.pcm",
            )?)?;
            value.digital_pcm = doslet_state::narrow(
                w.digital_pcm
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> { <doslet_pcm::Frame as State>::load(v) })
                    .collect::<Result<Vec<_>, _>>()?,
                "PcPcBus.digital_pcm length",
            )?;
            value.a20 = <A20Gate as State>::load(doslet_state::required(w.a20, "PcPcBus.a20")?)?;
            value.keyboard = <Controller as State>::load(doslet_state::required(
                w.keyboard,
                "PcPcBus.keyboard",
            )?)?;
            value.cmos = <Cmos as State>::load(doslet_state::required(w.cmos, "PcPcBus.cmos")?)?;
            value.system_control =
                doslet_state::narrow(w.system_control, "PcPcBus.system_control")?;
            value.awe_writes = w.awe_writes;
            // Older states cannot establish that a device has never been used.
            let used = w.audio_use.unwrap_or(7);
            if used > 7 {
                return Err(InvalidState("PcPcBus audio use"));
            }
            value.audio_use = crate::AudioUse {
                sb16: used & 1 != 0,
                opl3: used & 2 != 0,
                mpu401: used & 4 != 0,
            };
            value
        };
        if !(value.config.memory_bytes() == value.ram.len()) {
            return Err(InvalidState("PcPcBus invariants"));
        }
        Ok(value)
    }
}

#[cfg(test)]
mod observer_tests {
    use super::*;

    #[test]
    fn snapshots_canonicalize_observers_without_mutating_the_live_bus() {
        let mut bus = PcBus::default();
        bus.write_port_u8(0x3c8, 7).unwrap();
        let canonical = bus.save();
        bus.vga_status_read_generation = u32::MAX;
        bus.device_accesses = u64::MAX;
        assert!(bus.save() == canonical);
        assert_eq!(bus.vga_status_read_generation, u32::MAX);
        assert_eq!(bus.device_accesses, u64::MAX);

        // Old snapshots can contain arbitrary absolute observer values. Their
        // guest state survives, while new marks start with fresh counters.
        let mut legacy = canonical.clone();
        legacy.vga_status_read_generation = u32::MAX;
        legacy.device_accesses = u64::MAX;
        let restored = PcBus::load(legacy).unwrap();
        assert_eq!(restored.vga_status_read_generation, 0);
        assert_eq!(restored.device_accesses, 0);
        assert!(restored.save() == canonical);
    }
}

#[cfg(test)]
mod audio_use_tests {
    use super::*;
    #[test]
    fn legacy_states_conservatively_lock_audio_and_reject_unknown_usage_bits() {
        let mut wire = PcBus::default().save();
        wire.audio_use = None;
        let restored = PcBus::load(wire.clone()).unwrap();
        assert_eq!(
            restored.audio_use,
            crate::AudioUse {
                sb16: true,
                opl3: true,
                mpu401: true
            }
        );
        wire.audio_use = Some(8);
        assert!(PcBus::load(wire).is_err());
    }
}
