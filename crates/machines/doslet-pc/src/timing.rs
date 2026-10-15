use crate::{MachineError, PcMachine};
use doslet_time::EmuTime;

// Four 33-MHz clocks are 121 ns plus 7/33 ns. Keeping that fraction
// directly removes variable-frequency division from ordinary CPU steps.
#[derive(Default)]
struct StepClock {
    fraction: u64,
}
impl StepClock {
    fn advance(&mut self, steps: u32) -> u64 {
        if steps == 1 {
            let fraction = self.fraction + 7;
            let carry = u64::from(fraction >= 33);
            self.fraction = fraction - carry * 33;
            121 + carry
        } else {
            let fraction = self.fraction + u64::from(steps) * 7;
            self.fraction = fraction % 33;
            u64::from(steps) * 121 + fraction / 33
        }
    }
    fn steps_until_nanos(&self, nanos: u64) -> u64 {
        (u128::from(nanos) * 33)
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
        Self {
            cpu: StepClock::default(),
            time: EmuTime::ZERO,
            pit_fraction: 0,
            pcm_fraction: 0,
        }
    }
}
impl PcMachine {
    /// Functional clock: four nominal 33-MHz clocks per execution/IRQ/idle
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
        if self.bus.pcm.is_full() {
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
                self.bus.pcm.push(doslet_pcm::Frame {
                    nanos: end,
                    duration: duration as u32,
                    samples,
                });
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
}
#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use doslet_bus::{MemoryBus, PortBus};
    #[test]
    fn step_clock_matches_reference_and_deadlines_at_every_fraction() {
        for prefix in 0..33 {
            let mut clock = StepClock::default();
            let mut reference =
                doslet_time::ClockRate::new(core::num::NonZeroU32::new(33_000_000).unwrap());
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
