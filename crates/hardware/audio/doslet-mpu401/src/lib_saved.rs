// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct MpuQueue {
    #[prost(bytes = "vec", tag = "1")]
    bytes: Vec<u8>,
    #[prost(uint64, tag = "2")]
    head: u64,
    #[prost(uint64, tag = "3")]
    len: u64,
}
impl<const N: usize> State for Queue<N> {
    type Wire = MpuQueue;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            bytes: self.bytes.to_vec(),
            head: self.head as u64,
            len: self.len as u64,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            bytes: doslet_state::narrow(w.bytes, "MpuQueue.bytes length")?,
            head: doslet_state::narrow(w.head, "MpuQueue.head")?,
            len: doslet_state::narrow(w.len, "MpuQueue.len")?,
        };
        if !(N != 0 && value.head < N && value.len <= N) {
            return Err(InvalidState("MpuQueue invariants"));
        }
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct MpuMpu401 {
    #[prost(bool, tag = "1")]
    uart: bool,
    #[prost(uint32, optional, tag = "2")]
    acknowledgement: Option<u32>,
    #[prost(message, optional, tag = "3")]
    output: Option<<Queue<1024> as State>::Wire>,
    #[prost(uint64, tag = "4")]
    commands: u64,
    #[prost(uint64, tag = "5")]
    transmitted: u64,
    #[prost(uint64, tag = "6")]
    transmit_ready_nanos: u64,
}
impl State for Mpu401 {
    type Wire = MpuMpu401;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            uart: self.uart,
            acknowledgement: self.acknowledgement.as_ref().map(|v| u32::from(*(v))),
            output: Some(State::save(&self.output)),
            commands: self.commands,
            transmitted: self.transmitted,
            transmit_ready_nanos: self.transmit_ready_nanos,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            uart: w.uart,
            acknowledgement: w
                .acknowledgement
                .map(|v| -> Result<_, InvalidState> {
                    doslet_state::narrow(v, "MpuMpu401.acknowledgement")
                })
                .transpose()?,
            output: <Queue<1024> as State>::load(doslet_state::required(
                w.output,
                "MpuMpu401.output",
            )?)?,
            commands: w.commands,
            transmitted: w.transmitted,
            transmit_ready_nanos: w.transmit_ready_nanos,
        };
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restoration_preserves_transmit_busy_and_old_states_default_to_ready() {
        let mut mpu = Mpu401::new();
        mpu.write_command(0x3F).unwrap();
        mpu.write_data(0x90, 10_000_000).unwrap();
        let mut restored = Mpu401::load(mpu.save()).unwrap();
        assert_eq!(restored.status(10_319_999) & 0x40, 0x40);
        assert_eq!(
            restored.write_data(60, 10_319_999),
            Err(MpuError::TransmitBusy)
        );
        restored.write_data(60, 10_320_000).unwrap();
        let mut bytes = [0; 2];
        assert_eq!(restored.drain(&mut bytes), 2);
        assert_eq!(bytes, [0x90, 60]);
        let mut legacy = mpu.save();
        legacy.transmit_ready_nanos = 0; // Absent field in older Protobuf states.
        assert_eq!(Mpu401::load(legacy).unwrap().status(10_000_000) & 0x40, 0);
    }
}
