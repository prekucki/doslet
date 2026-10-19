use crate::{MachineError, PcMachine};
use doslet_time::EmuTime;

// Precompute four CPU clocks in nanoseconds plus a retained fraction, keeping
// division out of ordinary single steps even with a configurable frequency.
struct StepClock {
    fraction: u64,
    mhz: u64,
    whole: u64,
    remainder: u64,
}
impl Default for StepClock {
    fn default() -> Self {
        Self::new(33)
    }
}
impl StepClock {
    fn new(mhz: u16) -> Self {
        let mhz = u64::from(mhz);
        Self {
            fraction: 0,
            mhz,
            whole: 4000 / mhz,
            remainder: 4000 % mhz,
        }
    }
    fn advance(&mut self, steps: u32) -> u64 {
        if steps == 1 {
            let fraction = self.fraction + self.remainder;
            let carry = u64::from(fraction >= self.mhz);
            self.fraction = fraction - carry * self.mhz;
            self.whole + carry
        } else {
            let fraction = self.fraction + u64::from(steps) * self.remainder;
            self.fraction = fraction % self.mhz;
            u64::from(steps) * self.whole + fraction / self.mhz
        }
    }
    #[cfg(all(feature = "wasm-procedures", not(target_arch = "wasm32")))]
    fn nanos_for_steps(&self, steps: u32) -> u64 {
        let fraction = self.fraction + u64::from(steps) * self.remainder;
        u64::from(steps) * self.whole + fraction / self.mhz
    }
    fn steps_until_nanos(&self, nanos: u64) -> u64 {
        (u128::from(nanos) * u128::from(self.mhz))
            .saturating_sub(u128::from(self.fraction))
            .div_ceil(4000)
            .min(u128::from(u64::MAX)) as u64
    }
}

pub(crate) struct TierTiming {
    clocks: [Option<StepClock>; 4],
    previous: [u64; 4],
    nanos: [u64; 4],
    slowest: u64,
    denominator: u64,
    costs: [u64; 4],
    fraction: u64,
}

pub(crate) struct Timing {
    /// Transient diagnostic execution mode; never serialized.
    external_cpu: bool,
    pub(crate) tiers: Option<TierTiming>,
    cpu: StepClock,
    pub time: EmuTime,
    pit_fraction: u64,
    pcm_fraction: u64,
}
impl Default for Timing {
    fn default() -> Self {
        Self::new(33)
    }
}
impl Timing {
    pub(crate) fn new(cpu_mhz: u16) -> Self {
        Self {
            external_cpu: false,
            tiers: None,
            cpu: StepClock::new(cpu_mhz),
            time: EmuTime::ZERO,
            pit_fraction: 0,
            pcm_fraction: 0,
        }
    }
}
impl PcMachine {
    #[cfg(all(feature = "wasm-procedures", not(target_arch = "wasm32")))]
    pub(crate) fn procedure_fits_before_irq(&self, steps: u32) -> bool {
        if !self.cpu.accepts_maskable_interrupt() || self.timing.external_cpu {
            return true;
        }
        self.timing.tiers.is_none()
            && self
                .diagnostic_service_horizon()
                .0
                .is_none_or(|nanos| self.timing.cpu.nanos_for_steps(steps) <= nanos)
    }

    /// Install native scheduling labels and rates; None is zero-cost/MAX.
    /// Device clocks are charged once from completed CPU counters per batch.
    pub fn set_execution_tiers(
        &mut self,
        labels: Option<alloc::vec::Vec<doslet_x86::ExecutionLabel>>,
        rates: [Option<u16>; 3],
    ) -> Result<(), &'static str> {
        if rates[1..].iter().any(Option::is_none)
            || rates.iter().flatten().any(|r| !(1..=1000).contains(r))
        {
            return Err("invalid tier rates");
        }
        let rates = [rates[0], rates[1], rates[2], rates[2]];
        let enabled = labels.is_some();
        self.cpu.set_execution_labels(labels)?;
        fn gcd(mut a: u64, mut b: u64) -> u64 {
            while b != 0 {
                (a, b) = (b, a % b);
            }
            a
        }
        let denominator = rates
            .into_iter()
            .flatten()
            .fold(1, |d, r| d / gcd(d, u64::from(r)) * u64::from(r));
        self.timing.tiers = enabled.then(|| TierTiming {
            denominator,
            costs: rates.map(|r| r.map_or(0, |r| 4000 * (denominator / u64::from(r)))),
            fraction: self.timing.cpu.fraction * denominator / self.timing.cpu.mhz,
            clocks: rates.map(|r| r.map(StepClock::new)),
            previous: [0; 4],
            nanos: [0; 4],
            slowest: u64::from(rates.into_iter().flatten().min().unwrap()),
        });
        Ok(())
    }
    pub fn execution_tier_stats(&self) -> ([u64; 4], [u64; 4]) {
        (
            self.cpu.execution_counts(),
            self.timing.tiers.as_ref().map_or([0; 4], |t| t.nanos),
        )
    }

    /// Diagnostic host-driven clock experiment. The owner must advance devices
    /// after every bounded CPU batch; no host clock enters the machine library.
    pub fn debug_external_cpu_clock(&mut self, enabled: bool) {
        self.timing.external_cpu = enabled;
    }

    /// Advance at most one millisecond, splitting at sample/timer deadlines.
    /// Unlike cooperative idle, elapsed CPU time must be charged even with IF=0
    /// or an IRQ already pending. Returns the actual device time advanced.
    pub fn debug_advance_device_time(&mut self, nanos: u64) -> Result<u64, MachineError> {
        let mut remaining = nanos.min(1_000_000);
        let requested = remaining;
        for _ in 0..4096 {
            if remaining == 0 {
                break;
            }
            let mut slice = remaining;
            if let Some(ticks) = self
                .bus
                .pit
                .ticks_until_pulse(0)
                .expect("installed channel")
            {
                slice = slice.min(
                    (u64::from(ticks) * 1_000_000_000 - self.timing.pit_fraction)
                        .div_ceil(1_193_182),
                );
            }
            if self.hardware_config().keyboard == doslet_config::KeyboardMode::At
                && let Some(deadline) = self.bus.keyboard.next_deadline()
            {
                slice = slice.min(deadline.saturating_sub(self.time().as_nanos()).max(1));
            }
            if self.hardware_config().sb16 {
                if self.bus.dsp.playback_format().is_some() {
                    slice = slice.min(
                        (1_000_000_000 - self.timing.pcm_fraction)
                            .div_ceil(u64::from(self.bus.dsp.sample_rate())),
                    );
                }
                if let Some(next) = self.bus.opl.next_sample_nanos() {
                    slice = slice.min(next);
                }
            }
            let slice = slice.max(1).min(remaining);
            self.advance_hardware_nanos(slice)?;
            remaining -= slice;
        }
        Ok(requested - remaining)
    }

    /// Change instruction throughput without resetting device clocks or guest time.
    /// Retain the sub-nanosecond CPU phase, rounded down to the new denominator.
    pub fn set_cpu_mhz(&mut self, mhz: u16) -> Result<(), MachineError> {
        if !(1..=1000).contains(&mhz) {
            return Err(MachineError::Hardware(
                doslet_config::HardwareError::CpuClock,
            ));
        }
        let fraction = self.timing.cpu.fraction * u64::from(mhz) / self.timing.cpu.mhz;
        self.timing.cpu = StepClock::new(mhz);
        self.timing.cpu.fraction = fraction;
        self.bus.set_cpu_mhz(mhz);
        Ok(())
    }

    /// One bounded hardware slice for an explicit cooperative idle policy.
    /// This is not proof of instruction equivalence. Stop at each device event;
    /// callers must resume CPU execution as soon as an IRQ becomes eligible.
    pub fn advance_cooperative_idle(
        &mut self,
        budget: u32,
        nanos: u64,
    ) -> Result<u32, MachineError> {
        use doslet_x86::flags;
        if budget == 0
            || nanos == 0
            || self.cpu.is_halted()
            || !self.cpu.accepts_maskable_interrupt()
            || self.cpu.has_interrupt_shadow()
            || self.interrupt_pending()
            || self.cpu.flags & (flags::TRAP | flags::RESUME | flags::VIRTUAL | flags::ALIGNMENT)
                != 0
            || self.cpu.control.cr0 & 0x8000_0000 != 0
        {
            return Ok(0);
        }
        let steps = self.steps_to_event_nanos(budget, nanos);
        self.advance_hardware_steps(steps)?;
        Ok(steps)
    }
    /// Functional clock: four configured nominal CPU clocks per execution/IRQ/idle
    /// step. This is deterministic across batches, not cycle-accurate timing.
    pub(crate) fn advance_hardware(&mut self) -> Result<(), MachineError> {
        self.advance_hardware_steps(1)
    }
    /// Diagnostic horizon for known scheduled CPU service sources. Not permission
    /// to skip a guest loop: input, other devices and RAM dependencies are unproven.
    /// Sample-generation events are deliberately distinct from DSP block IRQs.
    pub fn diagnostic_service_horizon(&self) -> (Option<u64>, &'static str) {
        if self.bus.pic.has_interrupt() {
            return (Some(0), "pending IRQ");
        }
        let mut result = (None, "unknown");
        let mut include = |nanos: u64, source| {
            if result.0.is_none_or(|old| nanos < old) {
                result = (Some(nanos), source);
            }
        };
        if let Some(ticks) = self
            .bus
            .pit
            .ticks_until_pulse(0)
            .expect("installed channel")
        {
            include(
                (u64::from(ticks) * 1_000_000_000 - self.timing.pit_fraction).div_ceil(1_193_182),
                "PIT pulse",
            );
        }
        if self.hardware_config().keyboard == doslet_config::KeyboardMode::At
            && let Some(deadline) = self.bus.keyboard.next_deadline()
        {
            include(
                deadline.saturating_sub(self.time().as_nanos()),
                "keyboard event",
            );
        }
        if self.hardware_config().sb16
            && let Some(frames) = self.bus.dsp.frames_until_block_irq()
        {
            include(
                (u64::from(frames) * 1_000_000_000 - self.timing.pcm_fraction)
                    .div_ceil(u64::from(self.bus.dsp.sample_rate())),
                "DSP block IRQ (earliest)",
            );
        }
        result
    }
    pub(crate) fn idle_steps_to_event(&self, budget: u32) -> u32 {
        self.steps_to_event_nanos(budget, 1_000_000)
    }
    pub(crate) fn steps_to_event_nanos(&self, budget: u32, deadline: u64) -> u32 {
        self.timing
            .steps_to_event_nanos(&self.bus, budget, deadline)
    }
}
impl Timing {
    pub(crate) fn steps_to_event_nanos(
        &self,
        bus: &crate::PcBus,
        budget: u32,
        deadline: u64,
    ) -> u32 {
        // Bound hardware-only work to one millisecond of guest time even when
        // no wake source is programmed. This is independent of host refresh.
        let mut nanos = deadline.min(1_000_000);
        if let Some(ticks) = bus.pit.ticks_until_pulse(0).expect("installed channel") {
            let deadline =
                (u64::from(ticks) * 1_000_000_000 - self.pit_fraction).div_ceil(1_193_182);
            nanos = nanos.min(deadline);
        }
        if bus.hardware_config().keyboard == doslet_config::KeyboardMode::At
            && let Some(deadline) = bus.keyboard.next_deadline()
        {
            nanos = nanos.min(deadline.saturating_sub(self.time.as_nanos()));
        }
        if bus.hardware_config().sb16 && bus.dsp.playback_format().is_some() {
            let deadline =
                (1_000_000_000 - self.pcm_fraction).div_ceil(u64::from(bus.dsp.sample_rate()));
            nanos = nanos.min(deadline);
        }
        let steps = if let Some(tiers) = &self.tiers {
            nanos.saturating_mul(tiers.slowest) / 4000
        } else {
            self.cpu.steps_until_nanos(nanos)
        };
        let steps = if bus.hardware_config().sb16 {
            bus.opl.next_sample_nanos().map_or(steps, |deadline| {
                steps.min(if let Some(tiers) = &self.tiers {
                    deadline.saturating_mul(tiers.slowest) / 4000
                } else {
                    self.cpu.steps_until_nanos(deadline)
                })
            })
        } else {
            steps
        };
        budget.min(steps.max(1) as u32)
    }
}
impl PcMachine {
    pub(crate) fn advance_opl_bus_time(&mut self, accesses: u64) -> Result<(), MachineError> {
        if accesses != 0 && !self.timing.external_cpu {
            // Functional ISA approximation: eight bus clocks at 8 MHz per
            // transaction, independent of CPU throughput and execution tiers.
            // I/O ends native slices; observed execution uses this same path.
            self.debug_advance_device_time(accesses * 1_000)?;
        }
        Ok(())
    }

    #[inline]
    pub(crate) fn advance_hardware_steps(&mut self, steps: u32) -> Result<(), MachineError> {
        if self.timing.external_cpu {
            return Ok(());
        }
        let nanos = self.charge_cpu_steps(steps);
        self.advance_hardware_nanos(nanos)
    }

    /// A finite computation may finish before delivering its next IRQ. Device
    /// events still run at their own deadlines, including every DMA/FM sample.
    /// This policy is used only by explicitly admitted atomic procedures.
    #[cfg(all(feature = "wasm-procedures", not(target_arch = "wasm32")))]
    pub(crate) fn advance_procedure_hardware_steps(
        &mut self,
        steps: u32,
    ) -> Result<(), MachineError> {
        if self.timing.external_cpu {
            return Ok(());
        }
        let mut remaining = self.charge_cpu_steps(steps);
        while remaining != 0 {
            remaining -= self.debug_advance_device_time(remaining)?;
        }
        Ok(())
    }

    fn charge_cpu_steps(&mut self, steps: u32) -> u64 {
        if let Some(tiers) = &mut self.timing.tiers {
            let counts = self.cpu.execution_counts();
            let mut delta = core::array::from_fn::<_, 4, _>(|i| counts[i] - tiers.previous[i]);
            // IRQ entry, HLT and explicit hardware-only skips are GREY unless
            // a proved wait supplied its actual per-tier iteration counts.
            let missing = u64::from(steps).saturating_sub(delta.iter().sum());
            if missing != 0 {
                self.cpu.account_execution([0, 0, missing, 0]);
                delta[2] += missing;
            }
            tiers.previous = self.cpu.execution_counts();
            let mut total = tiers.fraction;
            for (i, n) in delta.into_iter().enumerate() {
                let ns = tiers.clocks[i]
                    .as_mut()
                    .map_or(0, |clock| clock.advance(n as u32));
                tiers.nanos[i] += ns;
                total += n * tiers.costs[i];
            }
            tiers.fraction = total % tiers.denominator;
            total / tiers.denominator
        } else {
            self.timing.cpu.advance(steps)
        }
    }

    fn advance_hardware_nanos(&mut self, nanos: u64) -> Result<(), MachineError> {
        self.timing.time = self
            .timing
            .time
            .checked_advance(nanos)
            .ok_or(MachineError::ClockOverflow)?;
        self.timing.pit_fraction += nanos * 1_193_182;
        let ticks = self.timing.pit_fraction / 1_000_000_000;
        self.timing.pit_fraction %= 1_000_000_000;
        if ticks != 0 {
            self.advance_pit_ticks(ticks as u32);
        }
        // Normal steps and idle deadline slices are bounded to <=1 ms.
        self.bus.vga.advance_nanos(nanos as u32);
        self.bus.guest_nanos = self.time().as_nanos();
        if self.hardware_config().keyboard == doslet_config::KeyboardMode::At {
            self.bus
                .keyboard
                .advance_time(self.time().as_nanos())
                .map_err(MachineError::Keyboard)?;
            self.bus.update_keyboard_irq();
        }
        self.advance_digital_audio(nanos)?;
        self.advance_fm_audio(nanos)
    }
    fn advance_digital_audio(&mut self, nanos: u64) -> Result<(), MachineError> {
        if !self.hardware_config().sb16 {
            self.timing.pcm_fraction = 0;
            return Ok(());
        }
        let Some(format) = self.bus.dsp.playback_format() else {
            self.timing.pcm_fraction = 0;
            return Ok(());
        };
        self.timing.pcm_fraction += nanos * u64::from(self.bus.dsp.sample_rate());
        if self.timing.pcm_fraction < 1_000_000_000 {
            return Ok(());
        }
        self.timing.pcm_fraction -= 1_000_000_000;
        if (!self.bus.opl.started() && self.bus.pcm.is_full())
            || self
                .bus
                .audio_sources
                .as_ref()
                .is_some_and(|s| s[0].is_full())
        {
            return Err(MachineError::AudioQueueFull);
        }
        let channel = self
            .bus
            .mixer
            .dma_channel(format.word)
            .ok_or(MachineError::UnsupportedAudioRouting)?;
        for _ in 0..if format.stereo { 2 } else { 1 } {
            if self.bus.dsp.playback_format().is_none() {
                break;
            }
            let Some(value) = self.bus.dma_sample(channel)? else {
                break;
            };
            let event = self.bus.dsp.accept_sample(value)?;
            if let Some(frame) = event.frame {
                let samples = if self.bus.dsp.speaker_enabled() {
                    frame
                } else {
                    [0; 2]
                };
                let rate = u64::from(self.bus.dsp.sample_rate());
                let duration = 1_000_000_000_u64.div_ceil(rate);
                let end = self
                    .time()
                    .as_nanos()
                    .saturating_sub(self.timing.pcm_fraction / rate);
                let frame = doslet_pcm::Frame {
                    nanos: end,
                    duration: duration as u32,
                    samples,
                };
                self.bus.digital_pcm[0] = self.bus.digital_pcm[1];
                self.bus.digital_pcm[1] = frame;
                if let Some(sources) = &mut self.bus.audio_sources {
                    sources[0].push(doslet_pcm::SourceFrame {
                        nanos: end,
                        rate: rate as u32,
                        samples,
                    });
                }
                if !self.bus.opl.started() {
                    self.bus.pcm.push(frame);
                }
            }
            if event.raised_irq
                && self.bus.mixer.enabled_irq_sources() & (if format.word { 2 } else { 1 }) != 0
            {
                let irq = self
                    .bus
                    .mixer
                    .interrupt_line()
                    .ok_or(MachineError::UnsupportedAudioRouting)?;
                if irq >= 8 {
                    return Err(MachineError::UnsupportedAudioRouting);
                }
                self.bus.pic.raise_edge(irq);
            }
        }
        Ok(())
    }
    fn advance_fm_audio(&mut self, nanos: u64) -> Result<(), MachineError> {
        if !self.hardware_config().sb16 {
            return Ok(());
        }
        let bus = &mut self.bus;
        let mut full = false;
        bus.opl.advance(nanos as u32, bus.guest_nanos, |time, fm| {
            // Match DOSBox Staging's OPL analog-output calibration (1.5x).
            // Apply it after guest attenuation so quieter voices retain headroom.
            let fm = bus.mixer.fm_samples(fm).map(|sample| {
                (i32::from(sample) * 3 / 2).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
            });
            if let Some(sources) = &mut bus.audio_sources
                && !sources[1].push(doslet_pcm::SourceFrame {
                    nanos: time,
                    rate: doslet_opl3::SAMPLE_RATE,
                    samples: fm,
                })
            {
                full = true;
            }
            let digital = bus.digital_pcm.iter().rev().find(|frame| {
                frame.duration != 0
                    && frame.nanos <= time
                    && time - frame.nanos < u64::from(frame.duration)
            });
            let mut samples = fm;
            if let Some(digital) = digital {
                for (sample, digital) in samples.iter_mut().zip(digital.samples) {
                    *sample = sample.saturating_add(digital);
                }
            }
            // Silent FM needs no queued frames. A timer-only detection probe
            // must not create an endless silent audio stream.
            if (samples != [0; 2] || digital.is_some())
                && !bus.pcm.push(doslet_pcm::Frame {
                    nanos: time,
                    duration: 1_000_000_000_u32.div_ceil(doslet_opl3::SAMPLE_RATE),
                    samples,
                })
            {
                full = true;
            }
        });
        if full {
            Err(MachineError::AudioQueueFull)
        } else {
            Ok(())
        }
    }
}
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use doslet_bus::{MemoryBus, PortBus};
    #[test]
    fn service_horizon_uses_audio_block_not_each_sample_and_keeps_phase() {
        let mut m = PcMachine::default();
        m.bus.pit.write_control(0x34).unwrap();
        m.bus.pit.write_counter(0, 0).unwrap();
        m.bus.pit.write_counter(0, 0).unwrap();
        for byte in [0x41, 0x07, 0xd0, 0xc6, 0, 3, 0] {
            m.bus.dsp.write_command(byte).unwrap();
        }
        m.timing.pcm_fraction = 500_000_000;
        let before = m.bus.dsp.diagnostics().dma_units;
        assert_eq!(
            m.diagnostic_service_horizon(),
            (Some(1_750_000), "DSP block IRQ (earliest)")
        );
        assert_eq!(m.bus.dsp.diagnostics().dma_units, before);
        assert_eq!(m.timing.pcm_fraction, 500_000_000);
        m.bus.pic.raise_edge(0);
        assert_eq!(m.diagnostic_service_horizon(), (Some(0), "pending IRQ"));
    }
    #[test]
    fn live_clock_change_retains_machine_and_device_phase() {
        let mut m = idle_machine(true);
        m.advance_hardware_steps(7).unwrap();
        let cpu = m.cpu.snapshot();
        let time = m.time();
        let pit = m.timing.pit_fraction;
        let pcm = m.timing.pcm_fraction;
        let old_fraction = m.timing.cpu.fraction;
        assert!(m.set_cpu_mhz(0).is_err());
        assert!(m.set_cpu_mhz(1001).is_err());
        m.set_cpu_mhz(133).unwrap();
        assert_eq!(m.time(), time);
        assert!(m.cpu.matches(&cpu));
        assert_eq!(m.timing.pit_fraction, pit);
        assert_eq!(m.timing.pcm_fraction, pcm);
        assert_eq!(m.timing.cpu.fraction, old_fraction * 133 / 33);
        assert_eq!(m.hardware_config().cpu_mhz, 133);
        // 133 steps at 133 MHz are exactly 4 us, regardless of retained phase.
        m.advance_hardware_steps(133).unwrap();
        assert_eq!(m.time().as_nanos() - time.as_nanos(), 4000);
    }

    #[test]
    fn dma_ack_deadlines_follow_device_time_at_different_cpu_speeds() {
        for mhz in [8, 33, 133] {
            let mut m = idle_machine(true); // 2 kHz, one DMA unit per block.
            m.set_cpu_mhz(mhz).unwrap();
            let half_ms = u32::from(mhz) * 125;
            // As in ordinary execution, each hardware batch ends no later
            // than the next sample event.
            m.advance_hardware_steps(half_ms).unwrap();
            m.advance_hardware_steps(half_ms).unwrap();
            assert_eq!(m.time().as_nanos(), 1_000_000);
            assert_eq!(m.bus.dsp.diagnostics().block_irq_deadlines, 1);
            assert_eq!(m.bus.dsp.diagnostics().block_irq_deadline_misses, 1);
            m.bus.read_port_u8(0x22f).unwrap();
            // The next block arrives in exactly 500 us at every CPU speed.
            m.advance_hardware_steps(half_ms).unwrap();
            assert_eq!(m.bus.dsp.diagnostics().block_irq_deadlines, 2);
            assert_eq!(m.bus.dsp.diagnostics().block_irq_deadline_misses, 1);
            m.advance_hardware_steps(half_ms).unwrap();
            assert_eq!(m.bus.dsp.diagnostics().block_irq_deadlines, 3);
            assert_eq!(m.bus.dsp.diagnostics().block_irq_deadline_misses, 2);
        }
    }

    #[test]
    fn external_device_time_matches_small_steps_with_pending_irqs_and_audio() {
        for dma in [false, true] {
            let mut external = idle_machine(dma);
            let mut reference = idle_machine(dma);
            // At 1000 MHz each ordinary step is exactly four nanoseconds.
            reference.set_cpu_mhz(1000).unwrap();
            external.cpu.flags &= !doslet_x86::flags::INTERRUPT;
            let cpu = external.cpu.snapshot();
            assert_eq!(external.debug_advance_device_time(0).unwrap(), 0);
            for _ in 0..3 {
                assert_eq!(
                    external.debug_advance_device_time(u64::MAX).unwrap(),
                    1_000_000
                );
                for _ in 0..250_000 {
                    reference.advance_hardware().unwrap();
                }
                assert_eq!(external.time(), reference.time());
                assert_eq!(external.timing.pit_fraction, reference.timing.pit_fraction);
                assert_eq!(external.timing.pcm_fraction, reference.timing.pcm_fraction);
                assert_eq!(
                    external.bus.pic.read_command(),
                    reference.bus.pic.read_command()
                );
                assert_eq!(
                    external.bus.dsp.diagnostics(),
                    reference.bus.dsp.diagnostics()
                );
                assert_eq!(
                    external.bus.vga.read_status(0x3DA),
                    reference.bus.vga.read_status(0x3DA)
                );
                let mut a = [[0; 2]; 128];
                let mut b = [[0; 2]; 128];
                assert_eq!(external.drain_pcm(&mut a), reference.drain_pcm(&mut b));
                assert_eq!(a, b);
                assert!(external.cpu.matches(&cpu));
            }
        }
    }

    #[test]
    fn external_cpu_clock_freezes_time_but_delivers_charged_irq_on_next_step() {
        let mut m = idle_machine(false);
        m.debug_external_cpu_clock(true);
        m.step().unwrap(); // HLT
        assert_eq!(m.time().as_nanos(), 0);
        assert_eq!(m.timing.cpu.fraction, 0);
        assert!(m.cpu.is_halted());
        assert_eq!(m.debug_advance_device_time(40_000).unwrap(), 40_000);
        assert!(m.interrupt_pending());
        m.step().unwrap();
        assert!(!m.cpu.is_halted());
        assert_eq!(m.cpu.registers.ip, 0x200);
        assert_eq!(m.time().as_nanos(), 40_000);
        m.debug_external_cpu_clock(false);
        m.step().unwrap();
        assert_eq!(m.time().as_nanos(), 40_121);
    }

    #[test]
    fn cooperative_idle_matches_single_hardware_steps_and_stops_for_irq() {
        for dma in [false, true] {
            let mut fast = idle_machine(dma);
            let mut reference = idle_machine(dma);
            let cpu = fast.cpu.snapshot();
            let mut total = 0;
            loop {
                let used = fast.advance_cooperative_idle(10000, 500_000).unwrap();
                if used == 0 {
                    break;
                }
                for _ in 0..used {
                    reference.advance_hardware().unwrap();
                }
                total += used;
                assert_eq!(fast.time(), reference.time());
                assert_eq!(
                    fast.bus.pic.read_command(),
                    reference.bus.pic.read_command()
                );
                assert_eq!(fast.bus.dsp.irq_status(), reference.bus.dsp.irq_status());
                let mut a = [[0; 2]; 32];
                let mut b = [[0; 2]; 32];
                assert_eq!(fast.drain_pcm(&mut a), reference.drain_pcm(&mut b));
                assert_eq!(a, b);
                assert!(total < 10000);
            }
            assert!(total > 0);
            assert!(fast.interrupt_pending());
            assert!(fast.cpu.matches(&cpu));
            assert_eq!(fast.advance_cooperative_idle(10000, 500_000).unwrap(), 0);
        }
    }
    #[test]
    fn step_clock_matches_reference_and_deadlines_at_every_fraction() {
        for mhz in [1, 8, 25, 33, 66, 100, 132, 133, 1000] {
            for prefix in 0..u32::from(mhz) {
                let mut clock = StepClock::new(mhz);
                let mut reference = doslet_time::ClockRate::new(
                    core::num::NonZeroU32::new(u32::from(mhz) * 1_000_000).unwrap(),
                );
                assert_eq!(clock.advance(prefix), reference.advance(prefix * 4));
                for nanos in [0, 1, 120, 121, 122, 1000, 1_000_000, u64::MAX] {
                    assert_eq!(
                        clock.steps_until_nanos(nanos),
                        reference.ticks_until_nanos(nanos).div_ceil(4)
                    );
                }
                for steps in [0, 1, 1, 7, 33, 8250, 1000000] {
                    assert_eq!(clock.advance(steps), reference.advance(steps * 4));
                }
            }
        }
    }
    #[test]
    fn cpu_clock_changes_work_per_guest_second_without_changing_device_clocks() {
        let mut machines = [33, 132].map(|cpu_mhz| {
            PcMachine::from_config(doslet_config::HardwareConfig {
                cpu_mhz,
                ..Default::default()
            })
            .unwrap()
        });
        for (machine, steps) in machines.iter_mut().zip([8250, 33000]) {
            machine.bus.pit.write_control(0x34).unwrap();
            machine.bus.pit.write_counter(0, 0xA9).unwrap();
            machine.bus.pit.write_counter(0, 4).unwrap();
            for _ in 0..steps {
                machine.advance_hardware().unwrap();
            }
            assert_eq!(machine.time().as_nanos(), 1_000_000);
            machine.bus.pit.write_control(0).unwrap();
        }
        let [slow, fast] = &mut machines;
        assert_eq!(slow.bus.pit.read_counter(0), fast.bus.pit.read_counter(0));
        assert_eq!(slow.bus.pit.read_counter(0), fast.bus.pit.read_counter(0));
        assert_eq!(
            slow.bus.vga.read_status(0x3DA),
            fast.bus.vga.read_status(0x3DA)
        );
        assert_eq!(slow.bus.guest_nanos, fast.bus.guest_nanos);
    }
    #[test]
    fn single_and_batched_hardware_advances_preserve_clock_fraction() {
        let mut machine = PcMachine::default();
        let mut reference =
            doslet_time::ClockRate::new(core::num::NonZeroU32::new(33_000_000).unwrap());
        let mut nanos = 0;
        for i in 0..20000 {
            let steps = [1, 1, 7, 31, 33, 1000][i % 6];
            machine.advance_hardware_steps(steps).unwrap();
            nanos += reference.advance(steps * 4);
            assert_eq!(machine.time().as_nanos(), nanos);
        }
    }
    pub(crate) fn idle_machine(dma: bool) -> PcMachine {
        let mut machine = PcMachine::default();
        for (i, byte) in [0xF4, 0xEB, 0xFD].iter().enumerate() {
            machine.bus.write_u8(i as u32, *byte).unwrap();
        }
        for vector in [8, 15] {
            machine.bus.write_u16(vector * 4, 0x200).unwrap();
            machine.bus.write_u16(vector * 4 + 2, 0).unwrap();
        }
        for (i, byte) in [0xBA, 0x2F, 2, 0xEC, 0xB0, 0x20, 0xE6, 0x20, 0xCF]
            .iter()
            .enumerate()
        {
            machine.bus.write_u8(0x200 + i as u32, *byte).unwrap();
        }
        machine.bus.pit.write_control(0x34).unwrap();
        machine.bus.pit.write_counter(0, 37).unwrap();
        machine.bus.pit.write_counter(0, 0).unwrap();
        if dma {
            for i in 0..16 {
                machine
                    .bus
                    .write_u16(0x10000 + i * 2, (i * 1024) as u16)
                    .unwrap();
            }
            machine.bus.write_port_u8(0xD8, 0).unwrap();
            for value in [0, 0x80] {
                machine.bus.write_port_u8(0xC4, value).unwrap();
            }
            for value in [15, 0] {
                machine.bus.write_port_u8(0xC6, value).unwrap();
            }
            machine.bus.write_port_u8(0x8B, 1).unwrap();
            machine.bus.write_port_u8(0xD6, 0x59).unwrap();
            machine.bus.write_port_u8(0xD4, 1).unwrap();
            for value in [0xD1, 0x41, 0x07, 0xD0, 0xB6, 0x10, 0, 0] {
                machine.bus.write_port_u8(0x22C, value).unwrap();
            }
        }
        machine
    }
    #[test]
    fn idle_deadlines_preserve_irq_registers_dma_pcm_and_raster_phase() {
        for dma in [false, true] {
            let mut reference = idle_machine(dma);
            let mut fast = idle_machine(dma);
            let budget = 100000;
            for _ in 0..budget {
                reference.step().unwrap();
            }
            let mut remaining = budget;
            let mut dispatches = 0;
            while remaining > 0 {
                let idle = fast.advance_idle(remaining.min(10000)).unwrap();
                let consumed = if idle == 0 {
                    fast.step().unwrap();
                    1
                } else {
                    idle
                };
                remaining -= consumed;
                dispatches += 1;
            }
            assert!(dispatches < 10000, "{dispatches}");
            assert_eq!(fast.time(), reference.time());
            assert_eq!(fast.cpu.registers, reference.cpu.registers);
            assert_eq!(fast.cpu.flags, reference.cpu.flags);
            assert_eq!(fast.cpu.is_halted(), reference.cpu.is_halted());
            assert_eq!(
                fast.bus.pic.read_command(),
                reference.bus.pic.read_command()
            );
            assert_eq!(
                fast.bus.vga.read_port(0x3DA),
                reference.bus.vga.read_port(0x3DA)
            );
            assert_eq!(fast.bus.dsp.irq_status(), reference.bus.dsp.irq_status());
            assert_eq!(
                fast.bus.pit.read_counter(0),
                reference.bus.pit.read_counter(0)
            );
            assert_eq!(
                fast.bus.pit.read_counter(0),
                reference.bus.pit.read_counter(0)
            );
            let mut a = [[0; 2]; 4096];
            let mut b = [[0; 2]; 4096];
            let count = fast.drain_pcm(&mut a);
            assert_eq!(count, reference.drain_pcm(&mut b));
            assert_eq!(a, b);
            if dma {
                assert!(count > 0);
            }
        }
    }

    #[cfg(all(feature = "wasm-procedures", not(target_arch = "wasm32")))]
    #[test]
    fn atomic_procedure_time_services_every_dma_sample_and_timer_event() {
        let mut reference = idle_machine(true);
        let mut fast = idle_machine(true);
        reference.set_cpu_mhz(8).unwrap();
        fast.set_cpu_mhz(8).unwrap();
        // Advance hardware without entering the guest IRQ handler. The atomic
        // computation does the same after returning to its single CPU owner.
        for _ in 0..4096 {
            reference.advance_hardware_steps(1).unwrap();
        }
        fast.advance_procedure_hardware_steps(4096).unwrap();
        assert_eq!(fast.time(), reference.time());
        assert_eq!(fast.timing.cpu.fraction, reference.timing.cpu.fraction);
        assert_eq!(fast.timing.pit_fraction, reference.timing.pit_fraction);
        assert_eq!(fast.timing.pcm_fraction, reference.timing.pcm_fraction);
        assert_eq!(fast.bus.dsp.diagnostics(), reference.bus.dsp.diagnostics());
        assert_eq!(
            fast.bus.pic.read_command(),
            reference.bus.pic.read_command()
        );
        assert_eq!(
            fast.bus.vga.read_port(0x3da),
            reference.bus.vga.read_port(0x3da)
        );
        let mut a = [doslet_pcm::EMPTY; 64];
        let mut b = [doslet_pcm::EMPTY; 64];
        assert!(fast.audio_status().pcm_frames > 0);
        assert_eq!(
            fast.drain_pcm_timed(&mut a),
            reference.drain_pcm_timed(&mut b)
        );
        assert_eq!(a, b);
    }
    #[test]
    fn idle_is_bounded_and_does_not_skip_pending_irqs_or_disabled_interrupts() {
        let mut machine = idle_machine(false);
        machine.step().unwrap();
        assert_eq!(machine.advance_idle(0).unwrap(), 0);
        assert!(machine.advance_idle(u32::MAX).unwrap() <= 8250);
        machine.bus.pic.raise_edge(0);
        assert_eq!(machine.advance_idle(10000).unwrap(), 0);
        machine.cpu.flags = 2;
        assert_eq!(machine.advance_idle(10000).unwrap(), 0);
    }
    #[test]
    fn mixer_irq_mask_blocks_dma_delivery_without_dropping_pending_completion() {
        for mask in [0x0b, 0, 1] {
            let mut machine = idle_machine(true);
            machine.bus.write_port_u8(0x224, 0x83).unwrap();
            machine.bus.write_port_u8(0x225, mask).unwrap();
            machine.advance_hardware_steps(8250).unwrap();
            assert_eq!(machine.bus.dsp.irq_status(), 2);
            assert_eq!(machine.bus.pic.read_command() & 0x80 != 0, mask & 2 != 0);
            machine.bus.write_port_u8(0x225, 0x0b).unwrap();
            assert_ne!(machine.bus.pic.read_command() & 0x80, 0);
            machine.bus.read_port_u8(0x22f).unwrap();
            assert_eq!(machine.bus.dsp.irq_status(), 0);
        }
    }

    #[test]
    fn clocked_stereo_dma_generates_samples_blocks_and_terminal_count() {
        let mut machine = PcMachine::default();
        machine.cpu.flags = 2; // Inspect pending IRQs rather than enter an uninstalled handler.
        machine.bus.write_u8(0, 0x90).unwrap();
        machine.bus.write_u8(1, 0xEB).unwrap();
        machine.bus.write_u8(2, 0xFD).unwrap();
        for (index, value) in [0x8000, 0x7FFF, 0, 0x1234, 0x4000, 0xC000, 1, 2]
            .iter()
            .enumerate()
        {
            machine
                .bus
                .write_u16(0x10000 + index as u32 * 2, *value)
                .unwrap();
        }
        machine.bus.write_port_u8(0xD8, 0).unwrap();
        for value in [0, 0x80] {
            machine.bus.write_port_u8(0xC4, value).unwrap();
        }
        for value in [7, 0] {
            machine.bus.write_port_u8(0xC6, value).unwrap();
        }
        machine.bus.write_port_u8(0x8B, 1).unwrap();
        machine.bus.write_port_u8(0xD6, 0x59).unwrap();
        machine.bus.write_port_u8(0xD4, 1).unwrap();
        for value in [0xD1, 0x41, 3, 0xE8, 0xB6, 0x30, 3, 0] {
            machine.bus.write_port_u8(0x22C, value).unwrap();
        }
        for _ in 0..33000 {
            machine.step().unwrap();
        }
        assert_eq!(machine.time().as_nanos(), 4_000_000);
        let mut frames = [[0; 2]; 8];
        assert_eq!(machine.drain_pcm(&mut frames), 4);
        assert_eq!(
            &frames[..4],
            &[[i16::MIN, i16::MAX], [0, 0x1234], [0x4000, -0x4000], [1, 2]]
        );
        assert_eq!(machine.bus.pic.read_command(), 0x80);
        assert_eq!(machine.bus.dsp.irq_status(), 2);
        assert_eq!(machine.bus.read_port_u8(0xD0).unwrap() & 15, 2);
        machine.bus.read_port_u8(0x22F).unwrap();
        assert_eq!(machine.bus.dsp.irq_status(), 0);
    }
}

#[cfg(feature = "save-state")]
#[path = "timing_saved.rs"]
mod saved_state;
