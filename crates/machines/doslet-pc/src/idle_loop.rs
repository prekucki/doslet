//! Machine support for skipping proven idle loops.
//!
//! The runtime proves that one pass of a guest loop leaves the CPU state, RAM
//! and devices unchanged: the pass returns to the same architectural state,
//! every byte it wrote holds its previous value again, and it touched no port
//! or device memory. Until a device event, every further pass then repeats
//! exactly, so whole passes can be accounted for as time without execution.
use crate::{MachineError, PcMachine};

/// Device accesses when a proof began.
#[derive(Clone, Copy)]
pub struct IdleProofMark {
    device_accesses: u64,
}

impl PcMachine {
    /// Start observing RAM writes and device accesses for one candidate pass.
    pub fn begin_idle_proof(&mut self) -> IdleProofMark {
        self.bus.begin_ram_write_log();
        IdleProofMark {
            device_accesses: self.bus.device_accesses,
        }
    }

    /// Stop observing. True when the pass left RAM unchanged and touched no device.
    pub fn end_idle_proof(&mut self, mark: IdleProofMark) -> bool {
        let unchanged = self.bus.end_ram_write_log();
        unchanged && self.bus.device_accesses == mark.device_accesses
    }

    /// RAM mutations so far; a change after a proof means memory may differ.
    pub fn ram_writes(&self) -> u64 {
        self.bus.ram_writes()
    }

    /// Device accesses so far, including those of runtime services.
    pub fn device_accesses(&self) -> u64 {
        self.bus.device_accesses
    }

    /// An interrupt would be delivered before the next instruction.
    pub fn interrupt_pending(&self) -> bool {
        self.cpu.accepts_maskable_interrupt() && self.bus.pic.has_interrupt()
    }

    /// Account for whole passes of a proven loop that end strictly before the
    /// next hardware event and within `budget`. Returns the skipped steps.
    pub fn skip_idle_passes(&mut self, pass: u32, budget: u32) -> Result<u32, MachineError> {
        if pass == 0 || self.cpu.is_halted() || self.interrupt_pending() {
            return Ok(0);
        }
        let event = self.steps_to_event_nanos(u32::MAX, u64::MAX);
        let steps = budget.min(event.saturating_sub(1)) / pass * pass;
        if steps == 0 {
            return Ok(0);
        }
        self.advance_hardware_steps(steps)?;
        #[cfg(feature = "profiling")]
        {
            self.bus.stats.cpu_steps += u64::from(steps);
            self.bus.stats.idle_loop_steps += u64::from(steps);
            self.bus.stats.idle_loop_skips += 1;
        }
        Ok(steps)
    }
}
