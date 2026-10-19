use super::{Address, abi};

/// Owned small CPU boundary state. `charged_work` saturates at u64::MAX and sets
/// COUNTER_OVERFLOW. It excludes HOT instructions. `exact_total()` therefore
/// returns None after either HOT execution or counter overflow. Generated code
/// preserves unrelated EFLAGS/status bits and the two reserved frame words.
/// Arithmetic operations force EFLAGS bit 1, matching the interpreter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Frame {
    pub gpr: [u32; 8],
    pub eflags: u32,
    pub address: Address,
    pub remaining_budget: u64,
    pub charged_work: u64,
    pub status: u32,
}
impl Default for Frame {
    fn default() -> Self {
        Self {
            gpr: [0; 8],
            eflags: 2,
            address: Address::default(),
            remaining_budget: 0,
            charged_work: 0,
            status: 0,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameError;
impl core::fmt::Display for FrameError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("static-program frame requires 72 bytes")
    }
}
impl core::error::Error for FrameError {}

impl Frame {
    pub fn uncounted_seen(&self) -> bool {
        self.status & abi::UNCOUNTED_SEEN != 0
    }
    pub fn exact_total(&self) -> Option<u64> {
        (self.status & (abi::UNCOUNTED_SEEN | abi::COUNTER_OVERFLOW) == 0)
            .then_some(self.charged_work)
    }
    pub fn read(bytes: &[u8]) -> Result<Self, FrameError> {
        let bytes = bytes.get(..abi::FRAME_BYTES).ok_or(FrameError)?;
        let word = |offset: u32| {
            let i = offset as usize;
            u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]])
        };
        let wide = |offset| u64::from(word(offset)) | (u64::from(word(offset + 4)) << 32);
        Ok(Self {
            gpr: core::array::from_fn(|i| word(i as u32 * 4)),
            eflags: word(abi::EFLAGS),
            address: Address {
                space: word(abi::SPACE),
                offset: word(abi::OFFSET),
            },
            remaining_budget: wide(abi::BUDGET),
            charged_work: wide(abi::CHARGED_WORK),
            status: word(abi::STATUS),
        })
    }
    /// Write defined fields only; reserved bytes keep the caller's contents.
    pub fn write(&self, bytes: &mut [u8]) -> Result<(), FrameError> {
        let bytes = bytes.get_mut(..abi::FRAME_BYTES).ok_or(FrameError)?;
        for (i, value) in self.gpr.iter().enumerate() {
            bytes[i * 4..i * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (offset, value) in [
            (abi::EFLAGS, self.eflags),
            (abi::SPACE, self.address.space),
            (abi::OFFSET, self.address.offset),
            (abi::STATUS, self.status),
        ] {
            bytes[offset as usize..offset as usize + 4].copy_from_slice(&value.to_le_bytes());
        }
        for (offset, value) in [
            (abi::BUDGET, self.remaining_budget),
            (abi::CHARGED_WORK, self.charged_work),
        ] {
            bytes[offset as usize..offset as usize + 8].copy_from_slice(&value.to_le_bytes());
        }
        Ok(())
    }
}
