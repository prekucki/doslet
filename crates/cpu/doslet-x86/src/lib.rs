//! Allocation-free, statically dispatched x86 interpreter foundation.
//! Selected real/protected-mode i386 forms with explicit unsupported diagnostics.
#![no_std]
extern crate alloc;

mod decode;
pub mod engines;
pub mod flags;
pub mod state;
pub mod x87_decode;
pub use engines::{DYNAMIC_PROFILE_MAX_BYTES, DynamicExitReason, DynamicExitRecord, DynamicStats};

#[cfg(feature = "profiling")]
pub use engines::interpreter::DecodeCacheStats;
pub use engines::interpreter::{Cpu, CpuError, CpuSnapshot, StepOutcome};
pub use state::{
    Access, ControlRegisters, DescriptorTable, Gpr16, Gpr32, Registers, SegmentCache,
    SegmentRegister,
};

/// Convenience translation for an address space with a closed 20-bit gate.
/// CPU execution uses `linear_address`; the machine owns A20 wiring.
pub const fn real_address(segment: u16, offset: u16) -> u32 {
    linear_address(segment, offset) & 0xFFFFF
}
/// Segment translation before the machine applies its A20 address gate.
pub const fn linear_address(segment: u16, offset: u16) -> u32 {
    (segment as u32) * 16 + offset as u32
}

#[cfg(all(
    feature = "dynamic-core",
    not(target_arch = "wasm32"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use engines::DynamicCompilation;
