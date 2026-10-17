// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct VgaVideoMode {
    #[prost(uint32, tag = "1")]
    value: u32,
}
impl State for VideoMode {
    type Wire = VgaVideoMode;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            value: match self {
                Self::Text3 => 0,
                Self::Mode13 => 1,
                Self::Vbe101 => 2,
            },
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        match w.value {
            0 => Ok(Self::Text3),
            1 => Ok(Self::Mode13),
            2 => Ok(Self::Vbe101),
            _ => Err(InvalidState("VgaVideoMode tag")),
        }
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VgaVgaPaletteItem {
    #[prost(bytes = "vec", tag = "1")]
    value: Vec<u8>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct VgaVga {
    #[prost(bytes = "vec", tag = "1")]
    memory: Vec<u8>,
    #[prost(message, repeated, tag = "2")]
    palette: Vec<VgaVgaPaletteItem>,
    #[prost(uint64, tag = "3")]
    dac_cursor: u64,
    #[prost(uint64, tag = "4")]
    dac_read_cursor: u64,
    #[prost(bool, tag = "5")]
    dac_reading: bool,
    #[prost(uint32, tag = "6")]
    dac_mask: u32,
    #[prost(message, optional, tag = "7")]
    registers: Option<<registers::Registers as State>::Wire>,
    #[prost(message, optional, tag = "8")]
    mode: Option<<VideoMode as State>::Wire>,
    #[prost(uint32, tag = "9")]
    bank: u32,
    #[prost(uint32, tag = "10")]
    raster_ticks: u32,
    #[prost(uint64, tag = "11")]
    pending_nanos: u64,
    #[prost(uint64, tag = "12")]
    pixel_fraction: u64,
    #[prost(uint64, tag = "13")]
    revision: u64,
}
impl State for Vga {
    type Wire = VgaVga;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            memory: self.memory.to_vec(),
            palette: self
                .palette
                .iter()
                .map(|v| VgaVgaPaletteItem {
                    value: (v).to_vec(),
                })
                .collect(),
            dac_cursor: self.dac_cursor as u64,
            dac_read_cursor: self.dac_read_cursor as u64,
            dac_reading: self.dac_reading,
            dac_mask: u32::from(self.dac_mask),
            registers: Some(State::save(&self.registers)),
            mode: Some(State::save(&self.mode)),
            bank: u32::from(self.bank),
            // JIT and interpreter batches flush lazy raster time at different
            // boundaries. Encode the same effective phase in canonical form.
            raster_ticks: self.raster_phase(),
            pending_nanos: 0,
            pixel_fraction: (self.pending_nanos * u64::from(self.pixel_clock_hz())
                + self.pixel_fraction)
                % 1_000_000_000,
            revision: self.revision,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            memory: (w.memory).into_boxed_slice(),
            palette: doslet_state::narrow(
                w.palette
                    .into_iter()
                    .map(|v| -> Result<_, InvalidState> {
                        doslet_state::narrow(v.value, "VgaVga.palette length")
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                "VgaVga.palette length",
            )?,
            dac_cursor: doslet_state::narrow(w.dac_cursor, "VgaVga.dac_cursor")?,
            dac_read_cursor: doslet_state::narrow(w.dac_read_cursor, "VgaVga.dac_read_cursor")?,
            dac_reading: w.dac_reading,
            dac_mask: doslet_state::narrow(w.dac_mask, "VgaVga.dac_mask")?,
            registers: <registers::Registers as State>::load(doslet_state::required(
                w.registers,
                "VgaVga.registers",
            )?)?,
            mode: <VideoMode as State>::load(doslet_state::required(w.mode, "VgaVga.mode")?)?,
            bank: doslet_state::narrow(w.bank, "VgaVga.bank")?,
            raster_ticks: w.raster_ticks,
            pending_nanos: w.pending_nanos,
            pixel_fraction: w.pixel_fraction,
            revision: w.revision,
        };
        if !(value.memory.len() == VRAM_LEN
            && value.dac_cursor < 768
            && value.dac_read_cursor < 768
            && value.pending_nanos < 1_000_000
            && value.raster_ticks < value.raster_total()
            && value.pixel_fraction < 1_000_000_000)
        {
            return Err(InvalidState("VgaVga invariants"));
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equivalent_lazy_clock_phases_have_identical_portable_states() {
        let mut a = Vga::default();
        let mut b = Vga::default();
        a.advance_nanos(123_456);
        b.advance_nanos(12_345);
        b.flush_raster_time();
        b.advance_nanos(111_111);
        assert_ne!(a.pending_nanos, b.pending_nanos);
        assert_eq!(a.save(), b.save());
        let mut restored = Vga::load(a.save()).unwrap();
        for delta in [1, 10_001, 1_234_567] {
            a.advance_nanos(delta);
            restored.advance_nanos(delta);
            assert_eq!(a.save(), restored.save());
        }
    }
}
