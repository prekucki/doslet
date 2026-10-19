//! Experimental profile-guided x86 -> WebAssembly translation.
//! Preparation allocates; execution borrows the runtime and never copies RAM.
#![no_std]
extern crate alloc;

use alloc::vec::Vec;
use doslet_x86::{
    analysis,
    translation::{self, Instruction, Operation},
};

mod emit;
mod speed_profile;
pub use speed_profile::ProfileReport;
pub mod host;
#[cfg(any(feature = "native-test", test))]
pub mod native;
pub mod static_program;

pub const ABI_VERSION: u32 = 1;
pub const MAX_BLOCKS: usize = 4096;
pub const DISPATCH_SLOTS: usize = 8192;
pub const DISPATCH_PROBES: u32 = 16;
pub const MAX_INSTRUCTIONS: usize = 16;
pub const MAX_QUANTUM: u32 = 4096;
pub const STATE_BYTES: usize = 64 + DISPATCH_SLOTS * 8;
pub const FLAGS: u32 = 32;
pub const IP: u32 = 36;
pub const QUOTA: u32 = 40;
pub const BINDINGS: u32 = 64;

#[derive(Debug)]
pub struct Block {
    pub address_hint: u32,
    pub bytes: Vec<u8>,
    pub instructions: Vec<Instruction>,
}
pub struct Translation {
    blocks: Vec<Block>,
    profile: Vec<u8>,
    report: ProfileReport,
}
impl Translation {
    /// Detect speed-tiers JSON or the optional DSJ1 code-profile format.
    pub fn from_bytes(profile: &[u8]) -> Result<Self, &'static str> {
        if profile.starts_with(b"DSJ1") {
            Self::from_profile(profile)
        } else {
            Self::from_speed_tiers(profile)
        }
    }
    /// Import DSJ1 observed-code profiles, including ones made by the native JIT.
    /// Hints are never trusted as proof that executable bytes are still present.
    pub fn from_profile(profile: &[u8]) -> Result<Self, &'static str> {
        if profile.len() < 6
            || profile.len() > doslet_x86::DYNAMIC_PROFILE_MAX_BYTES
            || &profile[..4] != b"DSJ1"
        {
            return Err("invalid DSJ1 header or size");
        }
        let records = u16::from_le_bytes([profile[4], profile[5]]) as usize;
        if records > 256 {
            return Err("too many DSJ1 records");
        }
        let mut blocks = Vec::new();
        let mut cursor = 6;
        for _ in 0..records {
            let header = profile
                .get(cursor..cursor + 5)
                .ok_or("truncated DSJ1 record")?;
            let hint = u32::from_le_bytes(header[..4].try_into().unwrap());
            let length = header[4] as usize;
            if !(1..=240).contains(&length) {
                return Err("invalid DSJ1 code length");
            }
            cursor += 5;
            let bytes = profile
                .get(cursor..cursor + length)
                .ok_or("truncated DSJ1 code")?;
            cursor += length;
            let mut offset = 0;
            while offset < bytes.len() && blocks.len() < MAX_BLOCKS {
                let start = offset;
                let mut instructions = Vec::new();
                while offset < bytes.len() && instructions.len() < MAX_INSTRUCTIONS {
                    let Some(i) = translation::decode(&bytes[offset..]) else {
                        break;
                    };
                    offset += usize::from(i.length);
                    instructions.push(i);
                    if matches!(i.operation, Operation::Branch { .. }) {
                        break;
                    }
                }
                if !instructions.is_empty() {
                    blocks.push(Block {
                        address_hint: hint.wrapping_add(start as u32),
                        bytes: bytes[start..offset].to_vec(),
                        instructions,
                    });
                } else if let Some(i) = analysis::decode(&bytes[offset..], 0) {
                    // Use an actual decoded length; never hunt for opcodes byte by byte.
                    offset += usize::from(i.length);
                } else {
                    break;
                }
            }
        }
        if cursor != profile.len() {
            return Err("trailing DSJ1 data");
        }
        let report = ProfileReport {
            input_nodes: records,
            compiled_entries: blocks.len(),
            ..ProfileReport::default()
        };
        Ok(Self {
            blocks,
            profile: profile.to_vec(),
            report,
        })
    }
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }
    pub fn report(&self) -> ProfileReport {
        self.report
    }
    /// A single module with internal block dispatch and one runtime poll import.
    /// Custom sections retain the input profile and the ABI version.
    pub fn wasm(&self) -> Vec<u8> {
        emit::module(self)
    }
}

#[cfg(test)]
mod tests;
