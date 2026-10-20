// Checked-in Protobuf bindings and component conversion. No protoc at build time.
extern crate alloc;
use super::*;
#[allow(unused_imports)]
use alloc::vec::Vec;
use doslet_state::{InvalidState, State};

#[derive(Clone, PartialEq, prost::Message)]
pub struct PcA20Gate {
    #[prost(bool, tag = "1")]
    enabled: bool,
    #[prost(uint64, tag = "2")]
    generation: u64,
}
impl State for A20Gate {
    type Wire = PcA20Gate;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            enabled: self.enabled,
            generation: self.generation,
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        let value = Self {
            enabled: w.enabled,
            generation: w.generation,
        };
        Ok(value)
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub struct PcRam {
    #[prost(bytes = "vec", tag = "1")]
    bytes: Vec<u8>,
}
impl State for Ram {
    type Wire = PcRam;
    fn save(&self) -> Self::Wire {
        Self::Wire {
            bytes: self.storage.bytes().to_vec(),
        }
    }
    fn load(w: Self::Wire) -> Result<Self, InvalidState> {
        if !matches!(w.bytes.len(), 4194304 | 8388608 | 16777216 | 33554432) {
            return Err(InvalidState("PcRam length"));
        }
        Ok(Self::from_storage(storage::Storage::from_bytes(
            w.bytes.into_boxed_slice(),
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_adopts_saved_bytes_with_fresh_code_identity() {
        let mut original = Ram::new(4 * 1024 * 1024);
        original.write(PAGE_SIZE as u32, 42).unwrap();
        let original_check = original.code_check(PAGE_SIZE..PAGE_SIZE + 1).unwrap();
        let wire = original.save();
        let pointer = wire.bytes.as_ptr();
        let mut restored = Ram::load(wire).unwrap();
        assert_eq!(restored.storage.bytes().as_ptr(), pointer);
        assert_eq!(restored.read(PAGE_SIZE as u32), Ok(42));
        assert!(!restored.code_valid(&original_check));
        let restored_check = restored.code_check(PAGE_SIZE..PAGE_SIZE + 1).unwrap();
        assert_eq!(restored_check.generations, [0, 0]);
        assert_eq!(restored.writes(), 0);
        restored.write(PAGE_SIZE as u32, 24).unwrap();
        assert_eq!(original.read(PAGE_SIZE as u32), Ok(42));
        assert_eq!(restored.read(PAGE_SIZE as u32), Ok(24));
        assert!(!restored.code_valid(&restored_check));
    }
}
