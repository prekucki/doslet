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
pub use memory_wait::MemoryWaitSites;

use core::fmt;
use doslet_bus::BusError;
pub use doslet_mc146818::Snapshot as CmosSnapshot;
use doslet_x86::{Cpu, CpuError, StepOutcome};
pub use idle_loop::IdleProofMark;
pub use io::PcBus;
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
        }
    }
}
impl core::error::Error for MachineError {}

/// Per-instruction diagnostics, collected only by the observed specialization.
#[derive(Default)]
pub struct StepObservation {
    pub trace: doslet_bus::observation::Trace,
    pub irq: Option<u8>,
    pub device_writes_unobserved: bool,
    pub service_writes_unobserved: bool,
    pub service_device_accesses_unobserved: bool,
}

pub struct PcMachine {
    pub cpu: Cpu,
    pub bus: PcBus,
    timing: timing::Timing,
    vga_wait: vga_wait::WaitState,
    memory_wait_sites: Option<MemoryWaitSites>,
    memory_wait_steps: u64,
    batch_yield_ip: Option<[u32; 2]>,
    interrupt_entries: u64,
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
            interrupt_entries: 0,
        })
    }
}
impl PcMachine {
    /// Optional instruction-boundary scheduling hints; ordinary execution has none.
    pub fn set_batch_yield_ips(&mut self, ips: Option<[u32; 2]>) {
        self.batch_yield_ip = ips;
    }
    pub fn set_batch_yield_ip(&mut self, ip: Option<u32>) {
        self.batch_yield_ip = ip.map(|ip| [ip; 2]);
    }
    pub fn hardware_config(&self) -> doslet_config::HardwareConfig {
        self.bus.hardware_config()
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
        if !OBSERVE
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
            let native = self.cpu.dynamic_core_enabled() && !self.bus.ram.recurrence_recording();
            let consumed = if let Some(head) = self.batch_yield_ip {
                if native {
                    self.cpu
                        .dynamic_batch_until_blocks(&mut self.bus, budget, limit, head)
                } else {
                    self.cpu
                        .interpret_batch_until_blocks(&mut self.bus, budget, limit, head)
                }
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
    pub fn audio_status(&self) -> AudioStatus {
        let (pcm_frames, pcm_nonzero) = self.bus.pcm.stats();
        AudioStatus {
            dsp: self.bus.dsp.diagnostics(),
            rate: self.bus.dsp.sample_rate(),
            format: self.bus.dsp.playback_format(),
            configured_format: self.bus.dsp.configured_format(),
            speaker: self.bus.dsp.speaker_enabled(),
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
