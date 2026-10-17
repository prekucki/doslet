pub mod interpreter;
pub const DYNAMIC_PROFILE_MAX_BYTES: usize = 6 + 256 * 245;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DynamicExitReason {
    #[default]
    Budget,
    UnsupportedInstruction,
    GraphLimit,
    LoadGuard,
    StoreBounds,
    ActiveCode,
    GenerationBounds,
    GenerationExhausted,
    RegisterEnd,
    LoopExit,
    SegmentGuard,
    RamUnavailable,
    ReadOnlyBus,
}

/// Aggregated native exits. Slots with `count == 0` are unused.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DynamicExitRecord {
    pub cs: u16,
    pub entry_ip: u32,
    pub exit_ip: u32,
    pub entry_linear: u32,
    pub exit_linear: u32,
    pub reason: DynamicExitReason,
    pub count: u64,
    pub steps: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DynamicStats {
    pub compiled_blocks: u64,
    pub executed_blocks: u64,
    pub executed_steps: u64,
    pub invalidations: u64,
    pub compilation_failures: u64,
    /// Least-recently-used executable-memory banks retired during preparation.
    pub code_cache_evictions: u64,
    pub prepared_hits: u64,
    /// Hardware/IRQ scheduling boundaries amortized across native and RAM work.
    pub execution_batches: u64,
    pub batched_interpreter_steps: u64,
    /// Subset of native work performed by guarded RAM/CFG traces.
    pub ram_trace_steps: u64,
    pub ram_trace_entries: u64,
}

#[cfg(not(all(
    feature = "dynamic-core",
    not(target_arch = "wasm32"),
    any(target_arch = "x86_64", target_arch = "aarch64")
)))]
impl interpreter::Cpu {
    pub fn set_dynamic_exit_profiling(&mut self, enabled: bool) -> Result<(), &'static str> {
        if enabled {
            Err("native dynamic core unavailable")
        } else {
            Ok(())
        }
    }
    pub fn dynamic_exit_profile(&self) -> &[DynamicExitRecord] {
        &[]
    }
    pub fn dynamic_exit_profile_dropped(&self) -> u64 {
        0
    }
    pub fn set_dynamic_core(&mut self, _enabled: bool) {}
    pub fn dynamic_core_enabled(&self) -> bool {
        false
    }
    pub fn dynamic_stats(&self) -> DynamicStats {
        DynamicStats::default()
    }
    pub fn dynamic_error(&self) -> Option<&str> {
        None
    }
    pub fn prepare_dynamic(&mut self, _limit: usize) -> usize {
        0
    }
    pub fn dynamic_steps<B: doslet_bus::MemoryBus>(&mut self, _bus: &B, _budget: u32) -> u32 {
        0
    }
    pub fn dynamic_steps_mut<B: doslet_bus::MemoryBus>(
        &mut self,
        _bus: &mut B,
        _budget: u32,
    ) -> u32 {
        0
    }
    pub fn dynamic_steps_with_limit<B: doslet_bus::MemoryBus>(
        &mut self,
        _bus: &B,
        _budget: u32,
        _limit: impl FnOnce() -> u32,
    ) -> u32 {
        0
    }
    pub fn dynamic_batch<B: doslet_bus::CpuBus>(
        &mut self,
        _bus: &mut B,
        _budget: u32,
        _limit: impl FnOnce(&B) -> u32,
    ) -> u32 {
        0
    }
    pub fn load_dynamic_profile(&mut self, _bytes: &[u8]) -> Result<(), &'static str> {
        Err("native dynamic core unavailable")
    }
    pub fn dynamic_batch_until_block<B: doslet_bus::CpuBus>(
        &mut self,
        bus: &mut B,
        budget: u32,
        limit: impl FnOnce(&B) -> u32,
        head: u32,
    ) -> u32 {
        self.interpret_batch_until_block(bus, budget, limit, head)
    }
    pub fn dynamic_batch_until_blocks<B: doslet_bus::CpuBus>(
        &mut self,
        bus: &mut B,
        budget: u32,
        limit: impl FnOnce(&B) -> u32,
        heads: [u32; 2],
    ) -> u32 {
        self.interpret_batch_until_blocks(bus, budget, limit, heads)
    }
    pub fn write_dynamic_profile(&self, _output: &mut [u8]) -> usize {
        0
    }
}

#[cfg(all(
    feature = "dynamic-core",
    not(target_arch = "wasm32"),
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
pub use interpreter::DynamicCompilation;
