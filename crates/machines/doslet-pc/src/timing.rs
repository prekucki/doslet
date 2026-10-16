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
    fn steps_until_nanos(&self, nanos: u64) -> u64 {
        (u128::from(nanos) * u128::from(self.mhz))
            .saturating_sub(u128::from(self.fraction))
            .div_ceil(4000)
            .min(u128::from(u64::MAX)) as u64
    }
}

pub(crate) struct Timing {
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
            cpu: StepClock::new(cpu_mhz),
            time: EmuTime::ZERO,
            pit_fraction: 0,
            pcm_fraction: 0,
        }
    }
}
impl PcMachine {
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

    /// Functional clock: four configured nominal CPU clocks per execution/IRQ/idle
    /// step. This is deterministic across batches, not cycle-accurate timing.
    pub(crate) fn advance_hardware(&mut self) -> Result<(), MachineError> {
        self.advance_hardware_steps(1)
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
        let steps = self.cpu.steps_until_nanos(nanos);
        let steps = if bus.hardware_config().sb16 {
            bus.opl.next_sample_nanos().map_or(steps, |deadline| {
                steps.min(self.cpu.steps_until_nanos(deadline))
            })
        } else {
            steps
        };
        budget.min(steps.max(1) as u32)
    }
}
impl PcMachine {
    #[inline]
    pub(crate) fn advance_hardware_steps(&mut self, steps: u32) -> Result<(), MachineError> {
        let nanos = self.timing.cpu.advance(steps);
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
        if !self.bus.opl.started() && self.bus.pcm.is_full() {
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
                if !self.bus.opl.started() {
                    self.bus.pcm.push(frame);
                }
            }
            if event.raised_irq {
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
            let fm = bus.mixer.fm_samples(fm);
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
