//! Wire types for the isolated, pinned v86 CPU/JIT module.
#![no_std]

pub const UPSTREAM_REVISION: &str = "cc37f35c43bf14865d74ad76879ecf9410f69eb1";
pub const ABI_VERSION: u32 = 4;
pub const MAX_BATCH_STEPS: u32 = 65_536;
pub const TABLE_OFFSET: u32 = 1024;
pub const MAX_COMPILED_MODULES: u64 = 256;
pub const TASK_SEGMENT: usize = 6;
pub const LDTR_SEGMENT: usize = 7;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Registers {
    /// EAX, ECX, EDX, EBX, ESP, EBP, ESI, EDI.
    pub gpr: [u32; 8],
    pub eip: u32,
    pub eflags: u32,
    /// ES, CS, SS, DS, FS, GS.
    pub segments: [u16; 6],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunResult {
    pub steps: u32,
    pub halted: bool,
    pub interrupt: Option<u8>,
    pub exception: Option<Exception>,
    pub interrupt_shadow: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exception {
    pub vector: u8,
    pub error_code: Option<u32>,
}

/// CPU/device boundary. Ordinary RAM remains owned by the CPU's WASM memory.
#[derive(Clone, Copy, Debug)]
pub enum DeviceRequest {
    ReadPort { port: u16, width: u8 },
    WritePort { port: u16, width: u8, value: u32 },
    ReadMemory { address: u32, width: u8 },
    WriteMemory { address: u32, width: u8, value: u32 },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Segment {
    pub selector: u16,
    pub base: u32,
    pub limit: u32,
    pub access: u8,
    pub default32: bool,
    pub usable: bool,
}

/// Small architectural values exchanged at host-service/scheduling boundaries,
/// never a RAM or VRAM snapshot. The x87 image uses the 108-byte FSAVE layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Context {
    pub registers: Registers,
    /// ES, CS, SS, DS, FS, GS, TR, LDTR (the upstream v86 order).
    pub segments: [Segment; 8],
    pub cr: [u32; 5],
    pub gdtr: (u32, u16),
    pub idtr: (u32, u16),
    pub cpl: u8,
    pub protected: bool,
    pub code32: bool,
    pub stack32: bool,
    pub halted: bool,
    pub interrupt_shadow: bool,
    pub fpu: [u8; 108],
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Statistics {
    pub run_calls: u64,
    pub run_nanos: u64,
    pub context_nanos: u64,
    pub compilation_nanos: u64,
    pub compiled_modules: u64,
    pub jit_entries: u64,
    pub invalidated_modules: u64,
    pub compilation_exhausted: bool,
    pub compilation_pending: bool,
}
