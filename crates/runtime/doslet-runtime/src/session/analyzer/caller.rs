//! Bounded, diagnostic-only inventory of an actual returning caller iteration.
use super::*;
use alloc::{boxed::Box, vec};
use doslet_bus::observation::Event;

const LIMIT: u32 = 32_768;
const INTERVAL: u64 = 250_000_000;
const EFFECTS: usize = 512;
const RAM_BYTES: usize = 2048;

#[derive(Clone, Copy, Debug)]
pub struct CallerEffect {
    pub pc: u32,
    pub kind: Kind,
    pub irq: Option<u8>,
    pub device: bool,
    pub first_address: u32,
    pub last_address: u32,
    pub width: u8,
    pub last_value: u32,
    pub count: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct CallerStats {
    pub head: u32,
    pub caller: u32,
    pub attempts: u64,
    pub returns: u64,
    pub complete: u64,
    pub exhausted: u64,
    pub steps: u64,
    pub irq_steps: u64,
    pub service_gaps: u64,
    pub device_gaps: u64,
    pub dropped_effects: u64,
    pub last_steps: u32,
    pub last_nanos: u64,
    pub last_complete: bool,
    pub last_cpu_unchanged: bool,
    pub last_input_unchanged: bool,
    pub last_cmos_unchanged: bool,
    pub last_ram_changed_bytes: Option<u32>,
    /// First changed bytes in fixed hash-table order, (address, before, after).
    pub last_ram_changes: [Option<(u32, u8, u8)>; 16],
    pub reason: &'static str,
}
impl Default for CallerStats {
    fn default() -> Self {
        Self {
            head: 0,
            caller: 0,
            attempts: 0,
            returns: 0,
            complete: 0,
            exhausted: 0,
            steps: 0,
            irq_steps: 0,
            service_gaps: 0,
            device_gaps: 0,
            dropped_effects: 0,
            last_steps: 0,
            last_nanos: 0,
            last_complete: false,
            last_cpu_unchanged: false,
            last_input_unchanged: false,
            last_cmos_unchanged: false,
            last_ram_changed_bytes: None,
            last_ram_changes: [None; 16],
            reason: "no caller trace",
        }
    }
}

struct Active {
    cpu: doslet_x86::CpuSnapshot,
    cmos: doslet_pc::CmosSnapshot,
    input: super::super::idle::InputState,
    context: Context,
    entry_ip: u32,
    sp: u32,
    began: u64,
    steps: u32,
    incomplete: bool,
    irq: Option<(u8, u16, u32, u32)>,
    ram_incomplete: bool,
    mapping_space: Option<u64>,
}
pub(super) struct CallerProbe {
    pub stats: CallerStats,
    pub effects: Box<[Option<CallerEffect>]>,
    ram: Box<[Option<(u32, u8, u8)>]>,
    active: Option<Active>,
    next: u64,
}
impl Default for CallerProbe {
    fn default() -> Self {
        Self {
            stats: CallerStats::default(),
            effects: vec![None; EFFECTS].into_boxed_slice(),
            ram: vec![None; RAM_BYTES].into_boxed_slice(),
            active: None,
            next: 0,
        }
    }
}
impl CallerProbe {
    pub fn stop(&mut self, reason: &'static str, now: u64) {
        if let Some(active) = self.active.take() {
            self.stats.last_steps = active.steps;
            self.stats.last_nanos = now.saturating_sub(active.began);
            self.stats.last_complete = false;
            self.stats.reason = reason;
        }
    }
    fn begin(&mut self, head: u32, caller: u32, active: Active) {
        self.next = active.began.saturating_add(INTERVAL);
        if (self.stats.head, self.stats.caller) != (head, caller) {
            self.stats = CallerStats {
                head,
                caller,
                ..Default::default()
            };
            self.effects.fill(None);
        }
        self.stats.attempts += 1;
        self.ram.fill(None);
        self.stats.reason = "tracing caller; no work/wait classification";
        self.active = Some(active);
    }
    fn record(&mut self, pc: u32, event: Event, device: bool, irq: Option<u8>) {
        if !device && event.kind == Kind::Write {
            self.record_ram(event);
        }
        if !device && event.kind == Kind::Read {
            return;
        }
        let port = matches!(event.kind, Kind::PortRead | Kind::PortWrite);
        // Reserve device slots so stack traffic cannot hide the port/MMIO
        // operation that prevented the existing unchanged-device proof.
        let range = if device {
            EFFECTS - 16..EFFECTS
        } else {
            0..EFFECTS - 16
        };
        let slot = self.effects[range.clone()].iter_mut().find(|e| {
            e.is_some_and(|e| {
                e.pc == pc
                    && e.kind == event.kind
                    && e.irq == irq
                    && e.device == device
                    && e.width == event.width
                    && (!port || e.first_address == event.address)
            })
        });
        if let Some(Some(e)) = slot {
            e.count += 1;
            e.first_address = e.first_address.min(event.address);
            e.last_address = e.last_address.max(event.address);
            e.last_value = event.value;
        } else if let Some(slot) = self.effects[range].iter_mut().find(|e| e.is_none()) {
            *slot = Some(CallerEffect {
                pc,
                kind: event.kind,
                irq,
                device,
                first_address: event.address,
                last_address: event.address,
                width: event.width,
                last_value: event.value,
                count: 1,
            });
        } else {
            self.stats.dropped_effects += 1;
            if let Some(active) = &mut self.active {
                active.incomplete = true;
            }
        }
    }
    fn record_ram(&mut self, event: Event) {
        let Some(previous) = event.previous else {
            self.active.as_mut().unwrap().ram_incomplete = true;
            return;
        };
        for offset in 0..u32::from(event.width) {
            let address = event.address.wrapping_add(offset);
            let old = (previous >> (offset * 8)) as u8;
            let new = (event.value >> (offset * 8)) as u8;
            if old == new {
                continue;
            }
            let first = address.wrapping_mul(0x9e37_79b9) as usize % RAM_BYTES;
            let mut found = false;
            for probe in 0..RAM_BYTES {
                let slot = &mut self.ram[(first + probe) % RAM_BYTES];
                match slot {
                    Some((a, _, value)) if *a == address => {
                        *value = new;
                        found = true;
                        break;
                    }
                    None => {
                        *slot = Some((address, old, new));
                        found = true;
                        break;
                    }
                    _ => {}
                }
            }
            if !found {
                self.active.as_mut().unwrap().ram_incomplete = true;
            }
        }
    }
}

impl Session {
    pub(super) fn analyzer_caller_begin(
        &mut self,
        head: u32,
        caller: u32,
        context: Context,
        sp: u32,
    ) {
        let now = self.emulated_nanos();
        if self.analyzer.caller.active.is_some()
            || now < self.analyzer.caller.next
            || self.analyzer.recurrence.follows(head)
        {
            return;
        }
        self.analyzer.caller.begin(
            head,
            caller,
            Active {
                cpu: self.machine.cpu.snapshot(),
                cmos: self.machine.bus.cmos.snapshot(),
                input: self.input_state(),
                context,
                entry_ip: self.registers().ip,
                sp,
                began: now,
                steps: 0,
                incomplete: false,
                irq: None,
                ram_incomplete: !self.machine.bus.a20_enabled(),
                mapping_space: self.machine.bus.code_check(head, 1).map(|c| c.space),
            },
        );
        self.analyzer_interrupt_vga_boundary();
        self.analyzer.gap("bounded caller effect inventory");
    }
    /// Detailed caller effects are borrowed separately from lightweight HUD stats.
    pub fn synchronization_caller_effects(&self) -> &[Option<CallerEffect>] {
        &self.analyzer.caller.effects
    }
    pub(super) fn analyzer_caller_advance(&mut self) -> Result<Option<u32>, RuntimeError> {
        let Some(active) = &self.analyzer.caller.active else {
            return Ok(None);
        };
        let r = self.registers();
        let (cs, ip, pc, sp) = (r.cs, r.ip, self.debug_pc(), r.get32(Gpr32::Esp));
        let now = self.emulated_nanos();
        let context = Context::new(
            r,
            self.machine.cpu.control.cr0,
            self.machine.cpu.control.cr3,
        );
        if ip == active.entry_ip && sp == active.sp && active.irq.is_none() && active.steps != 0 {
            let return_pc = self
                .machine
                .bus
                .debug_ram(sp, 4)
                .map(|b| u32::from_le_bytes(b.try_into().unwrap()));
            let complete = !active.incomplete
                && context == Some(active.context)
                && return_pc == self.analyzer.caller.stats.caller.checked_add(5);
            let cpu_unchanged = self.machine.cpu.matches(&active.cpu);
            let input_unchanged = self.input_state() == active.input;
            let cmos_unchanged = self.machine.bus.cmos.snapshot() == active.cmos;
            let anchor_context = active.context;
            let ram_changed = (!active.ram_incomplete && !active.incomplete).then(|| {
                self.analyzer
                    .caller
                    .ram
                    .iter()
                    .flatten()
                    .filter(|(_, old, new)| old != new)
                    .count() as u32
            });
            let mut ram_changes = [None; 16];
            for (slot, change) in ram_changes.iter_mut().zip(
                self.analyzer
                    .caller
                    .ram
                    .iter()
                    .flatten()
                    .filter(|(_, old, new)| old != new),
            ) {
                *slot = Some(*change);
            }
            self.analyzer.caller.stats.returns += 1;
            self.analyzer.caller.stats.complete += u64::from(complete);
            self.analyzer.caller.stop(
                if complete {
                    "complete caller iteration; effects not classified as waiting"
                } else {
                    "caller returned with incomplete effect coverage"
                },
                now,
            );
            self.analyzer.caller.stats.last_complete = complete;
            self.analyzer.caller.stats.last_cpu_unchanged = cpu_unchanged;
            self.analyzer.caller.stats.last_input_unchanged = input_unchanged;
            self.analyzer.caller.stats.last_cmos_unchanged = cmos_unchanged;
            self.analyzer.caller.stats.last_ram_changed_bytes = ram_changed;
            self.analyzer.caller.stats.last_ram_changes = ram_changes;
            if complete
                && cpu_unchanged
                && input_unchanged
                && cmos_unchanged
                && ram_changed == Some(0)
            {
                self.analyzer_recurrence_learn(
                    self.analyzer.caller.stats.head,
                    self.analyzer.caller.stats.caller,
                    anchor_context,
                );
            }
            return Ok(None);
        }
        let is_iret = self.machine.bus.code_bytes(pc, 1) == Some(&[0xcf][..])
            || self.machine.bus.code_bytes(pc, 2) == Some(&[0x66, 0xcf][..]);
        let mut observation = doslet_pc::StepObservation::default();
        let used = self.analyzer_observe_step(&mut observation, false)?;
        let probe = &mut self.analyzer.caller;
        let active = probe.active.as_mut().unwrap();
        if !self.machine.bus.a20_enabled()
            || active.mapping_space.is_none()
            || self
                .machine
                .bus
                .code_check(probe.stats.head, 1)
                .map(|c| c.space)
                != active.mapping_space
        {
            active.ram_incomplete = true;
        }
        active.steps = active.steps.saturating_add(used);
        probe.stats.steps += u64::from(used);
        if let Some(vector) = observation.irq {
            if active.irq.is_some() {
                probe.stop("nested IRQ during caller trace", now);
                return Ok(Some(used));
            }
            active.irq = Some((vector, cs, ip, sp));
        }
        let irq = active.irq.map(|i| i.0);
        probe.stats.irq_steps += u64::from(used) * u64::from(irq.is_some());
        probe.stats.service_gaps += u64::from(
            observation.service_writes_unobserved || observation.service_device_accesses_unobserved,
        );
        probe.stats.device_gaps += u64::from(observation.device_writes_unobserved);
        active.incomplete |= observation.trace.incomplete
            || observation.service_writes_unobserved
            || observation.service_device_accesses_unobserved
            || observation.device_writes_unobserved;
        // IRQ entry's stack stores are not executed by the interrupted PC.
        let writer = if observation.irq.is_some() { 0 } else { pc };
        for &event in observation.trace.events[..observation.trace.len]
            .iter()
            .flatten()
        {
            let device = matches!(event.kind, Kind::PortRead | Kind::PortWrite)
                || !self.machine.bus.is_plain_ram(event.address, event.width);
            probe.record(writer, event, device, irq);
        }
        let r = &self.machine.cpu.registers;
        let active = probe.active.as_mut().unwrap();
        if is_iret
            && observation.irq.is_none()
            && active
                .irq
                .is_some_and(|(_, cs, ip, sp)| (r.cs, r.ip, r.get32(Gpr32::Esp)) == (cs, ip, sp))
        {
            active.irq = None;
        }
        if active.steps >= LIMIT {
            probe.stats.exhausted += 1;
            probe.stop(
                "caller trace step limit; incomplete iteration",
                self.machine.time().as_nanos(),
            );
        }
        Ok(Some(used))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(repeat: bool) -> Session {
        let mut s = super::super::super::cooperative::tests::session();
        s.set_menu_idle_site(None);
        s.set_idle_loops(false);
        s.set_vga_wait_acceleration(false);
        s.machine.cpu.flags = 2;
        // CALL guard; MOV DX,3DAh; IN AL,DX; MOV [3004h],EAX; JMP caller.
        let mut code = [
            0xe8, 0x3b, 0, 0, 0, 0x66, 0xba, 0xda, 3, 0xec, 0xa3, 4, 0x30, 0, 0, 0xeb, 0xef,
        ];
        if !repeat {
            code[16] = 0xfe;
        }
        for (i, byte) in code.into_iter().enumerate() {
            s.machine.bus.write_u8(0x10000 + i as u32, byte).unwrap();
        }
        for (i, byte) in [0x83, 0x3d, 0, 0x30, 0, 0, 0, 0x75, 1, 0x90, 0xc3]
            .into_iter()
            .enumerate()
        {
            s.machine.bus.write_u8(0x10040 + i as u32, byte).unwrap();
        }
        s.machine.bus.write_u32(0x3000, 100).unwrap();
        s.machine.cpu.registers.ip = 0x10000;
        s.start();
        s
    }

    #[test]
    fn complete_caller_inventory_preserves_execution_and_allocates_nothing() {
        let mut s = session(true);
        let mut reference = session(true);
        s.set_synchronization_analysis(true);
        let allocations = allocation_counter::measure(|| {
            for _ in 0..50 {
                s.advance(17);
                reference.advance(17);
                assert!(s.machine.cpu.matches(&reference.machine.cpu.snapshot()));
                assert_eq!(s.emulated_nanos(), reference.emulated_nanos());
                assert_eq!(
                    s.machine.bus.ram_view(0..0xa0000),
                    reference.machine.bus.ram_view(0..0xa0000)
                );
            }
        });
        assert_eq!(allocations.count_total, 0);
        let stats = s.synchronization_analysis();
        assert_eq!(stats.caller.complete, 1);
        assert_eq!(stats.caller.last_steps, 8);
        assert!(stats.caller.last_complete);
        let input = s
            .synchronization_caller_effects()
            .iter()
            .flatten()
            .find(|e| e.kind == Kind::PortRead)
            .unwrap();
        assert_eq!(
            (input.pc, input.first_address, input.count),
            (0x10009, 0x3da, 1)
        );
        assert!(input.device);
        assert_eq!(input.irq, None);
        assert!(
            s.synchronization_caller_effects()
                .iter()
                .flatten()
                .any(|e| e.kind == Kind::Write
                    && e.pc == 0x1000a
                    && e.first_address == 0x3004
                    && !e.device)
        );
        assert!(stats.candidates.iter().all(|c| c.predicted_mhz.is_none()));
    }

    #[test]
    fn overlapping_ram_writes_that_restore_bytes_have_no_net_change() {
        let mut s = session(true);
        // Change a dword, then restore its two halves independently.
        let code = [
            0xc7, 5, 4, 0x30, 0, 0, 0x78, 0x56, 0x34, 0x12, 0x66, 0xc7, 5, 4, 0x30, 0, 0, 0, 0,
            0x66, 0xc7, 5, 6, 0x30, 0, 0, 0, 0, 0xeb, 0xdd,
        ];
        for (i, byte) in code.into_iter().enumerate() {
            s.machine.bus.write_u8(0x10005 + i as u32, byte).unwrap();
        }
        s.set_synchronization_analysis(true);
        s.advance(40);
        assert_eq!(s.fault(), None);
        let c = s.synchronization_analysis().caller;
        assert!(c.last_complete);
        assert_eq!(c.last_ram_changed_bytes, Some(0));
        assert!(c.last_ram_changes.iter().all(Option::is_none));
    }

    #[test]
    fn ram_capacity_and_mapping_gaps_do_not_report_unchanged_memory() {
        let mut s = session(true);
        // More distinct modified bytes than the fixed ledger can hold.
        let code = [
            0xbf, 0, 0x40, 0, 0, 0xb9, 1, 8, 0, 0, 0xb0, 1, 0xf3, 0xaa, 0xeb, 0xeb,
        ];
        for (i, byte) in code.into_iter().enumerate() {
            s.machine.bus.write_u8(0x10005 + i as u32, byte).unwrap();
        }
        s.set_synchronization_analysis(true);
        s.advance(2200);
        let c = s.synchronization_analysis().caller;
        assert!(c.last_complete);
        assert_eq!(c.last_ram_changed_bytes, None);

        let mut s = session(true);
        s.set_synchronization_analysis(true);
        s.advance(2);
        assert!(s.analyzer.caller.active.is_some());
        s.machine.bus.set_a20(false);
        s.advance(1);
        s.machine.bus.set_a20(true);
        s.advance(40);
        assert_eq!(
            s.synchronization_analysis().caller.last_ram_changed_bytes,
            None
        );
    }

    #[test]
    fn nonreturning_caller_exhausts_without_a_complete_iteration() {
        let mut s = session(false);
        s.set_synchronization_analysis(true);
        s.advance(LIMIT + 100);
        let c = s.synchronization_analysis().caller;
        assert_eq!(s.fault(), None);
        assert_eq!((c.complete, c.returns, c.exhausted), (0, 0, 1));
        assert_eq!(c.last_steps, LIMIT);
        assert!(!c.last_complete);
        assert!(s.analyzer.caller.active.is_none());
    }
}
