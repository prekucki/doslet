//! PC composition. Devices have no knowledge of this machine or its CPU.
#![no_std]
extern crate alloc;
mod audio;
mod idle_loop;
pub mod io;
pub mod memory;
mod memory_wait;
mod recurrence;
pub use recurrence::RecurrenceMark;
pub mod resources;
mod timing;
mod vga_wait;
#[cfg(feature = "wasm-procedures")]
pub mod wasm_procedures;
pub use memory_wait::MemoryWaitSites;

use core::fmt;
use doslet_bus::BusError;
pub use doslet_mc146818::Snapshot as CmosSnapshot;
use doslet_x86::{Cpu, CpuError, StepOutcome};
pub use idle_loop::IdleProofMark;
pub use io::PcBus;
mod audio_config;
pub use audio_config::{AudioConfigError, AudioUse};
#[cfg(test)]
mod config_tests;
#[cfg(test)]
mod opl_tests;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioStatus {
    pub dsp: doslet_sb_dsp::Diagnostics,
    pub rate: u32,
    pub format: Option<doslet_sb_dsp::Format>,
    pub configured_format: Option<doslet_sb_dsp::Format>,
    pub speaker: bool,
    pub opl_started: bool,
    pub opl_key_on_writes: u64,
    pub opl_peak: u16,
    pub fm_levels: [u8; 4],
    pub irq_enabled_sources: u8,
    pub irq: Option<u8>,
    pub dma8: Option<u8>,
    pub dma16: Option<u8>,
    pub pcm_frames: u64,
    pub pcm_nonzero: u64,
    pub awe_writes: u64,
}
#[cfg(feature = "profiling")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExecutionStats {
    /// CPU scheduling steps, including individual REP iterations.
    pub cpu_steps: u64,
    pub idle_steps: u64,
    pub idle_batches: u64,
    pub irq_entries: u64,
    pub port_reads: u64,
    pub vga_status_reads: u64,
    pub pit_reads: u64,
    pub bda_tick_reads: u64,
    /// Logical IN/AND/Jcc units accounted for without interpreter dispatch.
    pub vga_wait_steps: u64,
    pub vga_wait_batches: u64,
    /// Steps of proven idle-loop passes accounted for without execution.
    pub idle_loop_steps: u64,
    pub idle_loop_skips: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineError {
    Hardware(doslet_config::HardwareError),
    Resource(resources::ResourceError),
    Keyboard(doslet_i8042::ControllerError),
    Cpu(CpuError),
    Bus(BusError),
    Dma(doslet_i8237::DmaError),
    Dsp(doslet_sb_dsp::DspError),
    UnsupportedAudioRouting,
    AudioQueueFull,
    ClockOverflow,
    #[cfg(feature = "wasm-procedures")]
    WasmProcedure(wasm_procedures::ExecutionError),
}
impl From<CpuError> for MachineError {
    fn from(error: CpuError) -> Self {
        Self::Cpu(error)
    }
}
impl From<BusError> for MachineError {
    fn from(error: BusError) -> Self {
        Self::Bus(error)
    }
}
impl From<doslet_i8237::DmaError> for MachineError {
    fn from(error: doslet_i8237::DmaError) -> Self {
        Self::Dma(error)
    }
}
impl From<doslet_sb_dsp::DspError> for MachineError {
    fn from(error: doslet_sb_dsp::DspError) -> Self {
        Self::Dsp(error)
    }
}
impl fmt::Display for MachineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hardware(error) => write!(f, "hardware configuration: {error:?}"),
            Self::Resource(error) => write!(f, "resource conflict: {error:?}"),
            Self::Keyboard(error) => write!(f, "keyboard: {error:?}"),
            Self::Cpu(error) => error.fmt(f),
            Self::Bus(error) => error.fmt(f),
            Self::Dma(error) => write!(f, "DMA: {error:?}"),
            Self::Dsp(error) => write!(f, "DSP: {error:?}"),
            Self::UnsupportedAudioRouting => f.write_str("unsupported SB16 DMA/IRQ routing"),
            Self::AudioQueueFull => f.write_str("PCM queue full; host must drain produced audio"),
            Self::ClockOverflow => f.write_str("emulated clock overflow"),
            #[cfg(feature = "wasm-procedures")]
            Self::WasmProcedure(error) => write!(f, "WASM procedure failed: {error:?}"),
        }
    }
}
impl core::error::Error for MachineError {}

/// Per-instruction diagnostics, collected only by the observed specialization.
#[derive(Default)]
pub struct StepObservation {
    /// Actual CPU entry after runtime services; absent for IRQ-only/HLT steps.
    pub instruction: Option<InstructionObservation>,
    pub trace: doslet_bus::observation::Trace,
    pub irq: Option<u8>,
    pub device_writes_unobserved: bool,
    pub service_writes_unobserved: bool,
    pub service_device_accesses_unobserved: bool,
}
#[derive(Clone, Copy)]
pub struct InstructionObservation {
    pub linear: u32,
    pub ip: u32,
    pub code32: bool,
    pub bytes: [u8; 15],
    pub available: u8,
}

pub struct PcMachine {
    pub cpu: Cpu,
    pub bus: PcBus,
    timing: timing::Timing,
    vga_wait: vga_wait::WaitState,
    memory_wait_sites: Option<MemoryWaitSites>,
    memory_wait_steps: u64,
    batch_yield_ip: Option<[u32; 2]>,
    /// Transient interpreter policy; never part of a portable guest state.
    borrowed_interpreter_bus: bool,
    interrupt_entries: u64,
    #[cfg(feature = "wasm-procedures")]
    wasm_procedures: Option<wasm_procedures::Plan>,
}
impl Default for PcMachine {
    fn default() -> Self {
        Self::from_config(doslet_config::HardwareConfig::default()).expect("valid default hardware")
    }
}
impl PcMachine {
    /// Attempt only the existing strict VGA proof. A failed attempt executes
    /// nothing; diagnostics can retain validated ordinary wait batching.
    pub fn try_vga_wait_batch(&mut self, budget: u32) -> Result<Option<u32>, MachineError> {
        if self.vga_wait.ready(budget) {
            self.advance_vga_wait(budget)
        } else {
            Ok(None)
        }
    }
    pub fn from_config(config: doslet_config::HardwareConfig) -> Result<Self, MachineError> {
        let mut bus = PcBus::from_config(config)?;
        bus.cmos.select(0x14);
        bus.cmos.write(if config.x87 { 2 } else { 0 });
        bus.cmos.select(0);
        Ok(Self {
            cpu: if config.x87 {
                Cpu::default().with_x87()
            } else {
                Cpu::default()
            },
            bus,
            timing: timing::Timing::new(config.cpu_mhz),
            vga_wait: vga_wait::WaitState::default(),
            memory_wait_sites: None,
            memory_wait_steps: 0,
            batch_yield_ip: None,
            borrowed_interpreter_bus: !cfg!(target_arch = "wasm32"),
            interrupt_entries: 0,
            #[cfg(feature = "wasm-procedures")]
            wasm_procedures: None,
        })
    }
}
impl PcMachine {
    /// RAM-only interpreter borrow, enabled by default for a native owner.
    /// Logging and recurrence recording keep the ordinary PC bus. Foreign WASM
    /// backing retains its existing RAM/VGA borrowing policy independently.
    pub fn set_borrowed_interpreter_bus(&mut self, enabled: bool) {
        self.borrowed_interpreter_bus = enabled;
    }

    /// Optional instruction-boundary scheduling hints; ordinary execution has none.
    pub fn batch_yield_ips(&self) -> Option<[u32; 2]> {
        self.batch_yield_ip
    }
    pub fn set_batch_yield_ips(&mut self, ips: Option<[u32; 2]>) {
        self.batch_yield_ip = ips;
    }
    pub fn set_batch_yield_ip(&mut self, ip: Option<u32>) {
        self.batch_yield_ip = ip.map(|ip| [ip; 2]);
    }
    pub fn hardware_config(&self) -> doslet_config::HardwareConfig {
        self.bus.hardware_config()
    }
    pub fn set_keyboard_mode(
        &mut self,
        mode: doslet_config::KeyboardMode,
    ) -> Result<(), MachineError> {
        if mode != self.hardware_config().keyboard {
            self.release_keys()?;
            self.bus.set_keyboard_mode(mode);
        }
        Ok(())
    }
    pub fn key_event(
        &mut self,
        key: doslet_input::PhysicalKey,
        pressed: bool,
    ) -> Result<(), MachineError> {
        if self.hardware_config().keyboard == doslet_config::KeyboardMode::At {
            self.bus
                .keyboard
                .key_event(key, pressed)
                .map_err(MachineError::Keyboard)?;
            self.bus.update_keyboard_irq();
        }
        Ok(())
    }
    pub fn release_keys(&mut self) -> Result<(), MachineError> {
        self.bus
            .keyboard
            .release_keys()
            .map_err(MachineError::Keyboard)?;
        self.bus.update_keyboard_irq();
        Ok(())
    }
    pub fn set_joystick(&mut self, axes: [i16; 2], buttons: [bool; 2]) {
        self.bus.gameport.set_state(axes, buttons);
    }
    /// Disable recognition as well as batching for interpreter comparisons.
    pub fn set_vga_wait_acceleration(&mut self, enabled: bool) {
        self.vga_wait.enabled = enabled;
        self.bus.observe_vga_status = enabled;
        self.vga_wait.candidate = None;
    }
    pub fn vga_wait_acceleration_enabled(&self) -> bool {
        self.vga_wait.enabled
    }
    /// Consume empty logical steps up to the first device deadline. CPU work
    /// and eligible IRQ delivery still happen through step(). The caller owns
    /// the finite budget and accounts for the returned logical step count.
    #[inline]
    pub fn advance_idle(&mut self, budget: u32) -> Result<u32, MachineError> {
        if budget == 0
            || !self.cpu.is_halted()
            || !self.cpu.accepts_maskable_interrupt()
            || self.bus.pic.has_interrupt()
        {
            return Ok(0);
        }
        let steps = self.idle_steps_to_event(budget);
        self.advance_hardware_steps(steps)?;
        #[cfg(feature = "profiling")]
        {
            self.bus.stats.idle_steps += u64::from(steps);
            self.bus.stats.idle_batches += 1;
        }
        Ok(steps)
    }
    #[cfg(feature = "profiling")]
    pub fn execution_stats(&self) -> ExecutionStats {
        self.bus.stats
    }
    /// IRQ sequence already maintained for recurrence invalidation; no profiler required.
    pub fn interrupt_entries(&self) -> u64 {
        self.interrupt_entries
    }
    pub fn step(&mut self) -> Result<StepOutcome, MachineError> {
        self.step_with_idle_budget(1).map(|(outcome, _)| outcome)
    }
    /// Diagnostic foreign engine: grant only the existing event-free batching
    /// horizon. Its exclusive owner must validate memory accesses, commit their
    /// effects before device time, and forbid unhandled I/O or CPU control changes.
    pub fn debug_register_engine_budget(&self, budget: u32) -> u32 {
        self.register_engine_budget(budget, false)
    }
    /// Foreign register engine with live, homogeneous execution-tier guards.
    /// The existing slowest-tier event horizon bounds every admitted tier.
    pub fn debug_tiered_register_engine_budget(&self, budget: u32) -> u32 {
        self.register_engine_budget(budget, true)
    }
    fn register_engine_budget(&self, budget: u32, allow_tiers: bool) -> u32 {
        if self.vga_wait.candidate.is_some()
            || self.memory_wait_sites.is_some()
            || self.batch_yield_ip.is_some()
            || self.cpu.is_halted()
            || (self.cpu.execution_labels_enabled() && !allow_tiers)
            || self.bus.ram.recurrence_recording()
            || (self.cpu.accepts_maskable_interrupt() && self.bus.pic.has_interrupt())
        {
            return 0;
        }
        self.timing
            .steps_to_event_nanos(&self.bus, budget.saturating_add(1), 1_000_000)
            .saturating_sub(1)
            .min(budget)
    }
    /// Commit already-executed register work; never execute guest code here.
    pub fn debug_account_register_engine(&mut self, steps: u32) -> Result<(), MachineError> {
        assert!(
            steps <= self.debug_register_engine_budget(steps),
            "foreign engine exceeded device horizon"
        );
        self.advance_hardware_steps(steps)?;
        #[cfg(feature = "profiling")]
        {
            self.bus.stats.cpu_steps += u64::from(steps);
        }
        Ok(())
    }
    /// Charge a tier-uniform, already executed register grant. Classifications
    /// are checked before dispatch, not inferred from the committed destination.
    pub fn debug_account_tiered_register_engine(
        &mut self,
        steps: u32,
        tier: u8,
    ) -> Result<(), MachineError> {
        assert!(tier < 4 && self.cpu.execution_labels_enabled());
        assert!(
            steps <= self.debug_tiered_register_engine_budget(steps),
            "tiered foreign engine exceeded device horizon"
        );
        let mut counts = [0; 4];
        counts[usize::from(tier)] = u64::from(steps);
        self.cpu.account_execution(counts);
        self.advance_hardware_steps(steps)?;
        #[cfg(feature = "profiling")]
        {
            self.bus.stats.cpu_steps += u64::from(steps);
        }
        Ok(())
    }
    /// Account for one instruction replaced by a runtime service. The caller
    /// must preserve its CPU effects and the interrupt boundary separately.
    /// Execution labels classify it at the current CS:IP, just like a CPU step.
    pub fn advance_service_step(&mut self) -> Result<(), MachineError> {
        if self.cpu.execution_labels_enabled() {
            let r = &self.cpu.registers;
            let address = r
                .segment_cache(doslet_x86::SegmentRegister::Cs)
                .base
                .wrapping_add(r.ip);
            let tier = self.cpu.execution_tier_at(&self.bus, address);
            let mut counts = [0; 4];
            counts[usize::from(tier)] = 1;
            self.cpu.account_execution(counts);
        }
        self.advance_hardware_steps(1)?;
        #[cfg(feature = "profiling")]
        {
            self.bus.stats.cpu_steps += 1;
        }
        Ok(())
    }
    /// Normal execution consumes one logical step. Native/RAM slices, HLT and
    /// confirmed waits may consume more, within the budget and device deadlines.
    pub fn step_with_idle_budget(
        &mut self,
        budget: u32,
    ) -> Result<(StepOutcome, u32), MachineError> {
        self.step_mode::<false>(budget, None)
    }
    /// Execute one scheduling unit with actual bus transactions and IRQ entry.
    /// No JIT or wait batching can bypass this bounded trace.
    pub fn step_observed(
        &mut self,
        observation: &mut StepObservation,
    ) -> Result<(StepOutcome, u32), MachineError> {
        *observation = StepObservation::default();
        self.step_mode::<true>(1, Some(observation))
    }
    fn step_mode<const OBSERVE: bool>(
        &mut self,
        budget: u32,
        mut observation: Option<&mut StepObservation>,
    ) -> Result<(StepOutcome, u32), MachineError> {
        #[cfg(feature = "wasm-procedures")]
        self.check_wasm_procedure_failure()?;
        if OBSERVE {
            observation.as_mut().unwrap().instruction = None;
        }
        if OBSERVE
            && !self.cpu.is_halted()
            && !(self.cpu.accepts_maskable_interrupt() && self.bus.pic.has_interrupt())
        {
            use doslet_bus::MemoryBus;
            use doslet_x86::{Access, SegmentRegister};
            let r = &self.cpu.registers;
            let linear = r.segment_cache(SegmentRegister::Cs).base.wrapping_add(r.ip);
            let mut entry = InstructionObservation {
                linear,
                ip: r.ip,
                code32: r.code32(),
                bytes: [0; 15],
                available: 0,
            };
            for length in 1..=15 {
                if r.address(SegmentRegister::Cs, r.ip, length, Access::Execute)
                    .is_err()
                {
                    break;
                }
                let Some(code) = self.bus.code_bytes(linear, length) else {
                    break;
                };
                if code.len() < usize::from(length) {
                    break;
                }
                entry.bytes[..usize::from(length)].copy_from_slice(&code[..usize::from(length)]);
                entry.available = length;
            }
            observation.as_mut().unwrap().instruction = Some(entry);
        }
        if !OBSERVE
            && self
                .memory_wait_sites
                .is_some_and(|sites| sites.starts.contains(&self.cpu.registers.ip))
            && let Some(consumed) = self.advance_memory_wait(budget)?
        {
            return Ok((StepOutcome::Continue, consumed));
        }
        if !OBSERVE
            && self.vga_wait.ready(budget)
            && let Some(consumed) = self.advance_vga_wait(budget)?
        {
            return Ok((StepOutcome::Continue, consumed));
        }
        #[cfg(feature = "wasm-procedures")]
        let wasm_zero_exit = if !OBSERVE {
            match self.try_wasm_procedure(budget)? {
                Some(0) => true,
                Some(consumed) => return Ok((StepOutcome::Continue, consumed)),
                None => false,
            }
        } else {
            false
        };
        #[cfg(not(feature = "wasm-procedures"))]
        let wasm_zero_exit = false;
        if !OBSERVE
            && !wasm_zero_exit
            && budget > 1
            && self.vga_wait.candidate.is_none()
            && self.memory_wait_sites.is_none()
            && !(self.cpu.accepts_maskable_interrupt() && self.bus.pic.has_interrupt())
        {
            // Native/register and ordinary-RAM work cannot observe devices.
            // Check IRQs once and flush time/counters at the end of the slice,
            // strictly before PIT/DSP deadlines. I/O and IF changes end a slice.
            let timing = &self.timing;
            let limit = |bus: &io::PcBus| {
                timing
                    .steps_to_event_nanos(bus, budget.saturating_add(1), 1_000_000)
                    .saturating_sub(1)
                    .min(budget)
            };
            // A valid recurrence journal requires decoded stores. Select that
            // batch path once instead of repeatedly attempting writable native
            // traces which cannot obtain RAM. Overflow and completed intervals
            // immediately restore the ordinary native path.
            // Foreign engines such as the register AOT host register entries
            // directly; batches then yield there instead of running through.
            #[cfg(feature = "wasm-procedures")]
            let prepared_entries = self.wasm_procedures_active() || self.cpu.has_prepared_entries();
            #[cfg(feature = "wasm-procedure-diagnostics")]
            let prepared_entries = prepared_entries && self.wasm_procedure_dispatch_enabled();
            #[cfg(not(feature = "wasm-procedures"))]
            let prepared_entries = self.cpu.has_prepared_entries();
            let native = self.cpu.dynamic_core_enabled()
                && !self.bus.ram.recurrence_recording()
                && !prepared_entries;
            #[cfg(feature = "wasm-procedures")]
            let foreign_backing = matches!(self.bus.ram.storage, memory::storage::Storage::Wasm(_));
            #[cfg(not(feature = "wasm-procedures"))]
            let foreign_backing = false;
            let borrowed_ready = !native
                && self.bus.a20_enabled()
                && (foreign_backing || self.borrowed_interpreter_bus)
                && self.bus.ram.direct_execution_ready();
            #[cfg(feature = "wasm-procedure-diagnostics")]
            let borrowed_ready =
                borrowed_ready && (!foreign_backing || self.wasm_procedure_borrowed_bus_enabled());
            let borrowed = if borrowed_ready {
                // Resolve the hardware deadline before exclusively borrowing
                // the backing. Device state cannot change inside this batch.
                let grant = limit(&self.bus);
                let mut bus = self
                    .bus
                    .interpreter_batch_bus()
                    .expect("ready identity-mapped interpreter RAM");
                let consumed = if let Some(head) = self.batch_yield_ip {
                    self.cpu
                        .interpret_batch_until_blocks(&mut bus, budget, |_| grant, head)
                } else if prepared_entries {
                    self.cpu
                        .interpret_batch_until_prepared_entry(&mut bus, budget, |_| grant)
                } else {
                    self.cpu.interpret_batch(&mut bus, budget, |_| grant)
                };
                let effects = bus.finish();
                self.bus.complete_interpreter_batch(effects);
                // A native RAM-only borrow can stop at a VGA operand before
                // completing anything. Retry through the complete PC bus so
                // repeated VGA work retains ordinary interpreter batching.
                // A positive prefix is returned once; the following call owns
                // the remaining work. Foreign backing keeps its prior policy.
                if consumed == 0 && !foreign_backing {
                    None
                } else {
                    Some(consumed)
                }
            } else {
                None
            };
            let consumed = if let Some(consumed) = borrowed {
                consumed
            } else if let Some(head) = self.batch_yield_ip {
                if native {
                    self.cpu
                        .dynamic_batch_until_blocks(&mut self.bus, budget, limit, head)
                } else {
                    self.cpu
                        .interpret_batch_until_blocks(&mut self.bus, budget, limit, head)
                }
            } else if prepared_entries {
                self.cpu
                    .interpret_batch_until_prepared_entry(&mut self.bus, budget, limit)
            } else if native {
                self.cpu.dynamic_batch(&mut self.bus, budget, limit)
            } else {
                self.cpu.interpret_batch(&mut self.bus, budget, limit)
            };
            if consumed != 0 {
                self.advance_hardware_steps(consumed)?;
                #[cfg(feature = "profiling")]
                {
                    self.bus.stats.cpu_steps += u64::from(consumed);
                }
                return Ok((StepOutcome::Continue, consumed));
            }
        }
        let read_generation = self.bus.vga_status_read_generation;
        let opl_accesses = self.bus.opl_accesses;
        let candidate_valid = self.vga_wait.candidate.is_some() && self.validate_vga_wait();
        let mut interrupted = false;
        #[cfg(feature = "profiling")]
        if self.cpu.is_halted() {
            self.bus.stats.idle_steps += 1;
        } else {
            self.bus.stats.cpu_steps += 1;
        }
        let outcome = if self.cpu.accepts_maskable_interrupt() {
            if let Some(vector) = self.bus.pic.acknowledge() {
                self.interrupt_entries = self.interrupt_entries.wrapping_add(1);
                interrupted = true;
                if OBSERVE {
                    observation.as_mut().unwrap().irq = Some(vector);
                }
                #[cfg(feature = "profiling")]
                {
                    self.bus.stats.irq_entries += 1;
                    if self.cpu.is_halted() {
                        self.bus.stats.idle_steps -= 1;
                    } else {
                        self.bus.stats.cpu_steps -= 1;
                    }
                }
                if OBSERVE {
                    let mut bus = doslet_bus::observation::Observed {
                        bus: &mut self.bus,
                        trace: &mut observation.as_mut().unwrap().trace,
                    };
                    self.cpu.interrupt(&mut bus, vector)?;
                } else {
                    self.cpu.interrupt(&mut self.bus, vector)?;
                }
                StepOutcome::Continue
            } else {
                if OBSERVE {
                    let mut bus = doslet_bus::observation::Observed {
                        bus: &mut self.bus,
                        trace: &mut observation.as_mut().unwrap().trace,
                    };
                    self.cpu.step(&mut bus)?
                } else {
                    self.cpu.step(&mut self.bus)?
                }
            }
        } else {
            if OBSERVE {
                let mut bus = doslet_bus::observation::Observed {
                    bus: &mut self.bus,
                    trace: &mut observation.as_mut().unwrap().trace,
                };
                self.cpu.step(&mut bus)?
            } else {
                self.cpu.step(&mut self.bus)?
            }
        };
        let before_writes = if OBSERVE { self.bus.ram_writes() } else { 0 };
        self.advance_hardware()?;
        self.advance_opl_bus_time(self.bus.opl_accesses.wrapping_sub(opl_accesses))?;
        if OBSERVE {
            observation.as_mut().unwrap().device_writes_unobserved =
                before_writes != self.bus.ram_writes();
        }
        self.observe_vga_wait(
            candidate_valid,
            interrupted,
            self.bus.vga_status_read_generation != read_generation,
        );
        // An interrupt-enabled HLT idles in bounded clock steps until an IRQ.
        if outcome == StepOutcome::Halted && self.cpu.accepts_maskable_interrupt() {
            let idle = self.advance_idle(budget.saturating_sub(1))?;
            Ok((StepOutcome::Continue, 1 + idle))
        } else {
            Ok((outcome, 1))
        }
    }
    pub fn drain_pcm(&mut self, output: &mut [[i16; 2]]) -> usize {
        self.bus.pcm.drain(output)
    }
    pub fn drain_pcm_timed(&mut self, output: &mut [doslet_pcm::Frame]) -> usize {
        self.bus.pcm.drain_timed(output)
    }
    /// Configure host-only source taps at a control boundary, before execution.
    pub fn capture_audio_sources(&mut self, enabled: bool) {
        if enabled && self.bus.audio_sources.is_none() {
            self.bus.audio_sources = Some(core::array::from_fn(|_| Default::default()));
        } else if !enabled {
            self.bus.audio_sources = None;
        }
    }
    pub fn drain_audio_source(
        &mut self,
        fm: bool,
        output: &mut [doslet_pcm::SourceFrame],
    ) -> usize {
        self.bus
            .audio_sources
            .as_mut()
            .map_or(0, |sources| sources[usize::from(fm)].drain(output))
    }
    pub fn audio_status(&self) -> AudioStatus {
        let (pcm_frames, pcm_nonzero) = self.bus.pcm.stats();
        AudioStatus {
            dsp: self.bus.dsp.diagnostics(),
            rate: self.bus.dsp.sample_rate(),
            format: self.bus.dsp.playback_format(),
            configured_format: self.bus.dsp.configured_format(),
            speaker: self.bus.dsp.speaker_enabled(),
            opl_started: self.bus.opl.started(),
            opl_key_on_writes: self.bus.opl.key_on_writes(),
            opl_peak: self.bus.opl.peak(),
            fm_levels: self.bus.mixer.fm_levels(),
            irq_enabled_sources: self.bus.mixer.enabled_irq_sources(),
            irq: self
                .hardware_config()
                .sb16
                .then(|| self.bus.mixer.interrupt_line())
                .flatten(),
            dma8: self
                .hardware_config()
                .sb16
                .then(|| self.bus.mixer.dma_channel(false))
                .flatten(),
            dma16: self
                .hardware_config()
                .sb16
                .then(|| self.bus.mixer.dma_channel(true))
                .flatten(),
            pcm_frames,
            pcm_nonzero,
            awe_writes: self.bus.awe_writes,
        }
    }
    pub fn time(&self) -> doslet_time::EmuTime {
        self.timing.time
    }
    /// Explicit peripheral clock input; the functional machine clock also uses this path.
    pub fn advance_pit_ticks(&mut self, ticks: u32) {
        if self.bus.pit.advance(0, ticks).unwrap_or(0) != 0 {
            self.bus.pic.raise_edge(0);
        }
        self.bus.pit.advance(1, ticks).unwrap();
        self.bus.pit.advance(2, ticks).unwrap();
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    #[test]
    fn observed_irq_and_device_steps_preserve_guest_state_and_pcm() {
        for dma in [false, true] {
            let mut observed = timing::tests::idle_machine(dma);
            let mut reference = timing::tests::idle_machine(dma);
            let mut irqs = 0;
            for _ in 0..10000 {
                let mut trace = StepObservation::default();
                assert_eq!(
                    observed.step_observed(&mut trace).unwrap(),
                    reference.step_with_idle_budget(1).unwrap()
                );
                irqs += usize::from(trace.irq.is_some());
                assert!(!trace.trace.incomplete);
                assert!(observed.cpu.matches(&reference.cpu.snapshot()));
                assert_eq!(observed.time(), reference.time());
                assert_eq!(
                    observed.bus.pic.read_command(),
                    reference.bus.pic.read_command()
                );
                let mut a = [[0; 2]; 8];
                let mut b = [[0; 2]; 8];
                assert_eq!(observed.drain_pcm(&mut a), reference.drain_pcm(&mut b));
                assert_eq!(a, b);
            }
            assert!(irqs > 0);
            assert_eq!(
                observed.bus.ram_view(0..0xa0000),
                reference.bus.ram_view(0..0xa0000)
            );
            assert_eq!(observed.bus.vga.vram_view(), reference.bus.vga.vram_view());
        }
    }
}

#[cfg(feature = "save-state")]
#[path = "lib_saved.rs"]
mod saved_state;
