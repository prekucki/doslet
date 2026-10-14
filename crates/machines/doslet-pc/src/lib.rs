//! PC composition. Devices have no knowledge of this machine or its CPU.
#![no_std]
extern crate alloc;
mod audio;
pub mod io;
pub mod memory;
mod timing;

use core::fmt;
use doslet_bus::BusError;
use doslet_x86::{Cpu, CpuError, StepOutcome};
pub use io::PcBus;
#[cfg(feature = "profiling")]
#[derive(Clone, Copy, Debug, Default)]
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineError {
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

pub struct PcMachine {
    pub cpu: Cpu,
    pub bus: PcBus,
    timing: timing::Timing,
}
impl Default for PcMachine {
    fn default() -> Self {
        let mut bus = PcBus::default();
        bus.cmos.select(0x14);
        bus.cmos.write(2); // x87 installed in the default PC composition.
        bus.cmos.select(0);
        Self {
            cpu: Cpu::default().with_x87(),
            bus,
            timing: timing::Timing::default(),
        }
    }
}
impl PcMachine {
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
    pub fn step(&mut self) -> Result<StepOutcome, MachineError> {
        self.step_with_idle_budget(1).map(|(outcome, _)| outcome)
    }
    /// Normal execution consumes one logical step. Only a halted CPU may use
    /// the remaining budget to advance directly to a device deadline.
    pub fn step_with_idle_budget(
        &mut self,
        budget: u32,
    ) -> Result<(StepOutcome, u32), MachineError> {
        #[cfg(feature = "profiling")]
        if self.cpu.is_halted() {
            self.bus.stats.idle_steps += 1;
        } else {
            self.bus.stats.cpu_steps += 1;
        }
        let outcome = if self.cpu.accepts_maskable_interrupt() {
            if let Some(vector) = self.bus.pic.acknowledge() {
                #[cfg(feature = "profiling")]
                {
                    self.bus.stats.irq_entries += 1;
                    if self.cpu.is_halted() {
                        self.bus.stats.idle_steps -= 1;
                    } else {
                        self.bus.stats.cpu_steps -= 1;
                    }
                }
                self.cpu.interrupt(&mut self.bus, vector)?;
                StepOutcome::Continue
            } else {
                self.cpu.step(&mut self.bus)?
            }
        } else {
            self.cpu.step(&mut self.bus)?
        };
        self.advance_hardware()?;
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
    pub fn time(&self) -> doslet_time::EmuTime {
        self.timing.time
    }
    /// Explicit peripheral clock input; the functional machine clock also uses this path.
    pub fn advance_pit_ticks(&mut self, ticks: u32) {
        if self.bus.pit.advance(0, ticks).unwrap_or(0) != 0 {
            self.bus.pic.raise_edge(0);
        }
    }
}
