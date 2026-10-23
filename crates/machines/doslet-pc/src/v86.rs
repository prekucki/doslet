//! One v86 CPU/RAM owner and the existing Rust PC devices. No interpreter or
//! RAM image is exchanged between engines during execution.
use super::{MachineError, PcMachine, StepOutcome, memory::storage::Storage};
use alloc::{boxed::Box, string::String};
use doslet_bus::{BusError, MemoryBus, PortBus};
use doslet_v86::{
    Context, DeviceRequest, LDTR_SEGMENT, Registers, Segment, Statistics, TASK_SEGMENT,
};
use doslet_v86_host::{Core, Cpu as V86Cpu, Execution, devices::DeviceHost};
use doslet_x86::{
    Access, CpuContext, DescriptorTable, Gpr32, SegmentCache, SegmentRegister, flags,
};

pub(crate) struct NoDevices;
impl MemoryBus for NoDevices {
    fn read_u8(&mut self, address: u32) -> Result<u8, BusError> {
        Err(BusError::UnmappedMemory(address))
    }
    fn write_u8(&mut self, address: u32, _: u8) -> Result<(), BusError> {
        Err(BusError::UnmappedMemory(address))
    }
}
impl PortBus for NoDevices {
    fn read_port_u8(&mut self, port: u16) -> Result<u8, BusError> {
        Err(BusError::UnmappedPort(port))
    }
    fn write_port_u8(&mut self, port: u16, _: u8) -> Result<(), BusError> {
        Err(BusError::UnmappedPort(port))
    }
}

pub(crate) struct Memory {
    pub cpu: V86Cpu<NoDevices>,
    pub generations: Box<[u64]>,
    previous_generations: Box<[u64]>,
    pub host_writes: bool,
    context: Option<Context>,
    pub steps: u64,
    pub interrupts: u64,
    pub imports: u64,
    error: Option<String>,
    host_clock: bool,
    host_clock_yielded: bool,
    ram_waits: bool,
    ram_wait_parks: u64,
    ram_wait_parked: bool,
    wait_site: Option<u32>,
    port_polls: [PortPoll; 2],
    vga_trap_site: Option<u32>,
    vga_last_read_step: u64,
    vga_trap_parks: u64,
    vga_trap_parked: bool,
}

struct PortPoll {
    enabled: bool,
    site: Option<u32>,
    last_step: u64,
    streak: u8,
    active: bool,
    dense_reads: u64,
}

impl PortPoll {
    fn new() -> Self {
        Self {
            enabled: true,
            site: None,
            last_step: 0,
            streak: 0,
            active: false,
            dense_reads: 0,
        }
    }

    fn clear(&mut self) {
        self.site = None;
        self.streak = 0;
        self.active = false;
    }

    fn observe(&mut self, site: u32, step: u64, waiting: bool) {
        if !self.enabled || !waiting {
            self.clear();
            return;
        }
        self.streak = if self.site == Some(site) && step.saturating_sub(self.last_step) <= 16 {
            self.streak.saturating_add(1)
        } else {
            1
        };
        self.site = Some(site);
        self.last_step = step;
        self.active = self.streak >= 3;
        self.dense_reads += u64::from(self.active);
    }
}

fn port_poll_waiting(port: u16, code: &[u8], value: u8) -> bool {
    match port {
        0x3da if code.len() >= 5 && code[..3] == [0xEC, 0xA8, 0x08] && code[4] == 0xFB => {
            match code[3] {
                0x74 => value & 8 == 0, // JE back while retrace is clear.
                0x75 => value & 8 != 0, // JNE back while retrace is set.
                _ => false,
            }
        }
        0x331
            if code.len() >= 9 && code[..9] == [0xEC, 0xEB, 0, 0x24, 0x40, 0x74, 6, 0xE2, 0xF7] =>
        {
            value & 0x40 != 0 // LOOP back only while transmit-busy.
        }
        _ => false,
    }
}

impl Memory {
    fn invalidate_host_writes(&mut self) -> Result<(), String> {
        if self.host_writes {
            for (index, (&now, old)) in self
                .generations
                .iter()
                .zip(self.previous_generations.iter_mut())
                .enumerate()
            {
                if now != *old {
                    self.cpu
                        .invalidate(index as u32 * 4096, (index as u32 + 1) * 4096)
                        .map_err(|error| alloc::format!("{error:#}"))?;
                    *old = now;
                }
            }
            self.host_writes = false;
        }
        Ok(())
    }
}

impl PcMachine {
    pub fn enable_v86(&mut self, module: &[u8]) -> Result<(), String> {
        if self.v86_enabled() {
            return Ok(());
        }
        #[cfg(feature = "wasm-procedures")]
        if self.wasm_procedures.is_some() {
            return Err("v86 and procedure modules require different RAM owners".into());
        }
        let core = Core::compile(module).map_err(|error| alloc::format!("{error:#}"))?;
        let mut cpu = core
            .instantiate(NoDevices, self.bus.ram.len() as u32)
            .map_err(|error| alloc::format!("{error:#}"))?;
        cpu.physical_ram_mut()
            .copy_from_slice(self.bus.ram.storage.bytes());
        cpu.set_machine_boundaries(true)
            .map_err(|error| alloc::format!("{error:#}"))?;
        let generations = self
            .bus
            .ram
            .storage
            .generations()
            .to_vec()
            .into_boxed_slice();
        let previous_generations = generations.clone();
        if self.cpu.execution_labels_enabled() {
            return Err("v86 does not support native execution tiers".into());
        }
        self.bus.ram.replace_storage(Storage::V86(Box::new(Memory {
            cpu,
            generations,
            previous_generations,
            host_writes: false,
            context: None,
            steps: 0,
            interrupts: 0,
            imports: 0,
            error: None,
            host_clock: false,
            host_clock_yielded: false,
            ram_waits: true,
            ram_wait_parks: 0,
            ram_wait_parked: false,
            wait_site: None,
            port_polls: [
                PortPoll::new(),
                PortPoll {
                    enabled: false,
                    ..PortPoll::new()
                },
            ],
            vga_trap_site: None,
            vga_last_read_step: 0,
            vga_trap_parks: 0,
            vga_trap_parked: false,
        })));
        self.cpu.set_dynamic_core(false);
        Ok(())
    }

    pub fn v86_enabled(&self) -> bool {
        matches!(self.bus.ram.storage, Storage::V86(_))
    }

    /// A runtime-owned protected-mode exception host receives faults before
    /// v86's IDT entry writes an interrupt/error-code frame on the guest stack.
    pub fn set_v86_exception_exits(&mut self, enabled: bool) -> Result<(), MachineError> {
        if let Storage::V86(owner) = &mut self.bus.ram.storage {
            owner
                .cpu
                .set_exception_exits(enabled)
                .map_err(|_| MachineError::V86("exception interception setup failed"))?;
        }
        Ok(())
    }

    /// The native owner supplies elapsed time separately from CPU work.
    pub fn set_v86_host_clock(&mut self, enabled: bool) -> Result<(), &'static str> {
        let Storage::V86(owner) = &mut self.bus.ram.storage else {
            return Err("host-clock execution requires v86");
        };
        owner.host_clock = enabled;
        if !enabled {
            owner.ram_wait_parked = false;
            owner.wait_site = None;
            owner.vga_trap_site = None;
            owner.vga_trap_parked = false;
            for poll in &mut owner.port_polls {
                poll.clear();
            }
            owner
                .cpu
                .set_wait_boundary(None)
                .map_err(|_| "wait boundary reset failed")?;
        }
        self.debug_external_cpu_clock(enabled);
        Ok(())
    }

    pub fn v86_host_clock_enabled(&self) -> bool {
        matches!(&self.bus.ram.storage, Storage::V86(owner) if owner.host_clock)
    }

    pub fn v86_host_clock_yielded(&self) -> bool {
        matches!(&self.bus.ram.storage, Storage::V86(owner) if owner.host_clock && owner.host_clock_yielded)
    }

    pub fn set_v86_ram_waits(&mut self, enabled: bool) {
        if let Storage::V86(owner) = &mut self.bus.ram.storage {
            owner.ram_waits = enabled;
            if !enabled {
                owner.wait_site = None;
                owner.ram_wait_parked = false;
                let _ = owner.cpu.set_wait_boundary(owner.vga_trap_site);
            }
        }
    }

    pub fn v86_ram_waits(&self) -> Option<(bool, u64)> {
        let Storage::V86(owner) = &self.bus.ram.storage else {
            return None;
        };
        Some((owner.ram_waits, owner.ram_wait_parks))
    }

    pub fn v86_ram_wait_parked(&self) -> bool {
        matches!(&self.bus.ram.storage, Storage::V86(owner) if owner.ram_wait_parked)
    }

    pub fn set_v86_vga_polling(&mut self, enabled: bool) {
        if let Storage::V86(owner) = &mut self.bus.ram.storage {
            owner.port_polls[0].enabled = enabled;
            if !enabled {
                owner.port_polls[0].clear();
                owner.vga_trap_site = None;
                owner.vga_trap_parked = false;
                let _ = owner.cpu.set_wait_boundary(owner.wait_site);
            }
        }
    }

    pub fn v86_vga_polling(&self) -> Option<(bool, u64)> {
        let Storage::V86(owner) = &self.bus.ram.storage else {
            return None;
        };
        Some((owner.port_polls[0].enabled, owner.vga_trap_parks))
    }

    pub fn v86_vga_poll_active(&self) -> bool {
        matches!(&self.bus.ram.storage, Storage::V86(owner) if owner.port_polls[0].active)
    }

    pub fn v86_vga_poll_wait_nanos(&self) -> Option<u64> {
        let Storage::V86(owner) = &self.bus.ram.storage else {
            return None;
        };
        if !owner.vga_trap_parked {
            return None;
        }
        Some(
            self.bus
                .vga
                .retrace_deadline(0x3da)
                .unwrap_or(1_000_000)
                .min(4_000_000),
        )
    }

    pub fn set_v86_mpu_polling(&mut self, enabled: bool) {
        if let Storage::V86(owner) = &mut self.bus.ram.storage {
            owner.port_polls[1].enabled = enabled;
            if !enabled {
                owner.port_polls[1].clear();
            }
        }
    }

    pub fn v86_mpu_polling(&self) -> Option<(bool, u64)> {
        let Storage::V86(owner) = &self.bus.ram.storage else {
            return None;
        };
        Some((owner.port_polls[1].enabled, owner.port_polls[1].dense_reads))
    }

    pub fn v86_mpu_poll_active(&self) -> bool {
        matches!(&self.bus.ram.storage, Storage::V86(owner) if owner.port_polls[1].active)
    }

    pub fn v86_mpu_poll_wait_nanos(&self) -> Option<u64> {
        if !self.v86_mpu_poll_active() {
            return None;
        }
        Some(
            self.bus
                .mpu
                .transmit_ready_in_nanos(self.bus.guest_nanos)
                .unwrap_or(500_000)
                .min(500_000),
        )
    }

    fn v86_vga_wait_ready(&self) -> bool {
        let Storage::V86(owner) = &self.bus.ram.storage else {
            return false;
        };
        let Some(site) = owner.vga_trap_site else {
            return false;
        };
        if !owner.host_clock
            || !owner.port_polls[0].enabled
            || owner.steps.saturating_sub(owner.vga_last_read_step) != 2
            || !self.cpu.accepts_maskable_interrupt()
            || self.cpu.has_interrupt_shadow()
            || self.bus.pic.has_interrupt()
        {
            return false;
        }
        let r = &self.cpu.registers;
        if r.address(SegmentRegister::Cs, r.ip, 1, Access::Execute)
            .ok()
            != Some(site)
        {
            return false;
        }
        let Some(code) = self.bus.ram_view(site as usize..site as usize + 5) else {
            return false;
        };
        let Some(status) = self.bus.vga.read_port(0x3da) else {
            return false;
        };
        let branch_taken = match code[3] {
            0x74 => self.cpu.flags & flags::ZERO != 0,
            0x75 => self.cpu.flags & flags::ZERO == 0,
            _ => false,
        };
        branch_taken && port_poll_waiting(0x3da, code, status)
    }

    fn v86_wait_pattern(code: &[u8]) -> bool {
        code.len() >= 19
            && code[0] == 0xA1
            && code[5..11] == [0x89, 0x04, 0x24, 0x8B, 0x04, 0x24]
            && code[11..13] == [0x3B, 0x05]
            && code[17..19] == [0x74, 0xED]
    }

    /// Find a candidate around the current IP, then reuse its physical site.
    /// A cached site is revalidated against live code on every CPU batch.
    fn v86_prepare_ram_wait(&mut self) -> Result<Option<u32>, MachineError> {
        let Storage::V86(owner) = &self.bus.ram.storage else {
            return Ok(None);
        };
        let enabled = owner.ram_waits && owner.host_clock;
        let cached = owner.wait_site;
        let r = &self.cpu.registers;
        let current = r
            .address(SegmentRegister::Cs, r.ip, 1, Access::Execute)
            .ok();
        let site = if enabled
            && r.code32()
            && self.bus.a20_enabled()
            && self.cpu.control.cr0 & 0x8000_0000 == 0
        {
            let cached = cached.filter(|&at| {
                current.is_some_and(|ip| ip / 4096 == at / 4096)
                    && self
                        .bus
                        .ram_view(at as usize..at as usize + 19)
                        .is_some_and(Self::v86_wait_pattern)
            });
            cached.or_else(|| {
                // A translated batch may leave IP beyond the loop body. Scan
                // backwards as well as forwards within this code page.
                let start = r.ip.saturating_sub(256).max(r.ip & !0xfff);
                let end =
                    r.ip.saturating_add(64)
                        .min((r.ip | 0xfff).saturating_sub(18));
                if end < start {
                    return None;
                }
                let count = end - start + 19;
                let at = r
                    .address(SegmentRegister::Cs, start, 1, Access::Execute)
                    .ok()?;
                let last = r
                    .address(SegmentRegister::Cs, end, 19, Access::Execute)
                    .ok()?;
                if last != at + end - start {
                    return None;
                }
                let code = self
                    .bus
                    .ram_view(at as usize..at as usize + count as usize)?;
                (0..=end - start)
                    .find(|&offset| Self::v86_wait_pattern(&code[offset as usize..]))
                    .map(|offset| at + offset)
            })
        } else {
            None
        };
        if let Storage::V86(owner) = &mut self.bus.ram.storage
            && site != owner.wait_site
        {
            owner
                .cpu
                .set_wait_boundary(site.or(owner.vga_trap_site))
                .map_err(|_| MachineError::V86("wait boundary setup failed"))?;
            owner.wait_site = site;
        }
        Ok(site)
    }

    /// Recognize a complete, idempotent RAM-polling pass at its backedge.
    /// The two absolute operands are read from live code, never game profiles.
    /// ROTT's CalcTics has this shape, but any identical loop qualifies.
    fn v86_ram_wait_ready(&self) -> bool {
        let Storage::V86(owner) = &self.bus.ram.storage else {
            return false;
        };
        let cpu = &self.cpu;
        let r = &cpu.registers;
        if !owner.host_clock
            || !owner.ram_waits
            || !r.code32()
            || !self.bus.a20_enabled()
            || cpu.control.cr0 & 0x8000_0000 != 0
            || !cpu.accepts_maskable_interrupt()
            || cpu.has_interrupt_shadow()
            || self.bus.pic.has_interrupt()
            || cpu.flags & (flags::TRAP | flags::RESUME | flags::VIRTUAL | flags::ALIGNMENT) != 0
        {
            return false;
        }
        let Some(code) = r
            .address(SegmentRegister::Cs, r.ip, 19, Access::Execute)
            .ok()
            .and_then(|at| self.bus.ram_view(at as usize..at as usize + 19))
        else {
            return false;
        };
        // MOV EAX,[ticks]; MOV [ESP],EAX; MOV EAX,[ESP];
        // CMP EAX,[oldtime]; JE back to the first MOV.
        if !Self::v86_wait_pattern(code) {
            return false;
        }
        let ticks_at = u32::from_le_bytes(code[1..5].try_into().unwrap());
        let oldtime_at = u32::from_le_bytes(code[13..17].try_into().unwrap());
        let read = |seg, offset| {
            r.address(seg, offset, 4, Access::Read)
                .ok()
                .and_then(|at| self.bus.ram_view(at as usize..at as usize + 4))
                .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
        };
        let Some(ticks) = read(SegmentRegister::Ds, ticks_at) else {
            return false;
        };
        let Some(oldtime) = read(SegmentRegister::Ds, oldtime_at) else {
            return false;
        };
        let Some(temporary) = read(SegmentRegister::Ss, cpu.stack_pointer()) else {
            return false;
        };
        let arithmetic = flags::CARRY
            | flags::PARITY
            | flags::AUXILIARY
            | flags::ZERO
            | flags::SIGN
            | flags::OVERFLOW;
        ticks == oldtime
            && ticks == temporary
            && ticks == r.get32(Gpr32::Eax)
            && cpu.flags & arithmetic == (flags::ZERO | flags::PARITY)
    }

    /// A proven loop has already reached its head. Keep the guest CPU parked
    /// until an interrupt can be delivered or the host explicitly changes its
    /// state. Device time and audio may still advance between calls.
    fn v86_ram_wait_latched(&self) -> bool {
        let Storage::V86(owner) = &self.bus.ram.storage else {
            return false;
        };
        owner.ram_wait_parked
            && owner.host_clock
            && owner.ram_waits
            && !owner.host_writes
            && !self.bus.pic.has_interrupt()
            && self.cpu.accepts_maskable_interrupt()
            && !self.cpu.has_interrupt_shadow()
            && owner.wait_site.is_some_and(|site| {
                self.cpu
                    .registers
                    .address(
                        SegmentRegister::Cs,
                        self.cpu.registers.ip,
                        1,
                        Access::Execute,
                    )
                    .ok()
                    == Some(site)
            })
    }

    pub fn v86_last_error(&self) -> Option<&str> {
        let Storage::V86(owner) = &self.bus.ram.storage else {
            return None;
        };
        owner.error.as_deref()
    }
    pub fn set_v86_profiling(&mut self, enabled: bool) {
        if let Storage::V86(owner) = &mut self.bus.ram.storage {
            owner.cpu.set_profiling(enabled);
        }
    }
    pub fn wait_for_v86_compilation(&mut self) -> Result<(), String> {
        if let Storage::V86(owner) = &mut self.bus.ram.storage {
            owner
                .cpu
                .wait_for_compilation()
                .map_err(|error| alloc::format!("{error:#}"))?;
        }
        Ok(())
    }

    pub fn v86_stats(&mut self) -> Option<(Statistics, u64, u64, u64)> {
        let Storage::V86(owner) = &mut self.bus.ram.storage else {
            return None;
        };
        owner
            .cpu
            .statistics()
            .ok()
            .map(|stats| (stats, owner.steps, owner.interrupts, owner.imports))
    }

    pub(crate) fn step_v86(&mut self, budget: u32) -> Result<(StepOutcome, u32), MachineError> {
        if self.v86_ram_wait_latched() {
            if let Storage::V86(owner) = &mut self.bus.ram.storage {
                owner.host_clock_yielded = true;
            }
            return Ok((StepOutcome::Continue, 0));
        }
        if let Storage::V86(owner) = &mut self.bus.ram.storage {
            owner.ram_wait_parked = false;
        }
        let ram_wait_site = self.v86_prepare_ram_wait()?;
        if self.v86_vga_wait_ready() {
            if let Storage::V86(owner) = &mut self.bus.ram.storage {
                owner.host_clock_yielded = true;
                owner.vga_trap_parks += 1;
                owner.vga_trap_parked = true;
            }
            return Ok((StepOutcome::Continue, 0));
        }
        if self.v86_ram_wait_ready() {
            if let Storage::V86(owner) = &mut self.bus.ram.storage {
                owner.host_clock_yielded = true;
                owner.ram_wait_parks += 1;
                owner.ram_wait_parked = true;
            }
            return Ok((StepOutcome::Continue, 0));
        }
        let wait_site = ram_wait_site.or(match &self.bus.ram.storage {
            Storage::V86(owner) => owner.vga_trap_site,
            _ => None,
        });
        let context = self.cpu.context();
        let entry_ip = self
            .cpu
            .registers
            .address(
                SegmentRegister::Cs,
                self.cpu.registers.ip,
                1,
                Access::Execute,
            )
            .ok();
        let opl_accesses = self.bus.opl_accesses;
        let fpu = self
            .cpu
            .fpu
            .as_ref()
            .map(|fpu| fpu.save_environment(true, true, true));
        let grant = if self.v86_host_clock_enabled() {
            budget.clamp(1, doslet_v86::MAX_BATCH_STEPS)
        } else {
            self.timing
                .steps_to_event_nanos(&self.bus, budget.saturating_add(1), 1_000_000)
                .saturating_sub(1)
                .min(budget.min(4096))
                .max(1)
        };
        let execution = if wait_site.is_some_and(|site| {
            self.cpu
                .registers
                .address(
                    SegmentRegister::Cs,
                    self.cpu.registers.ip,
                    1,
                    Access::Execute,
                )
                .ok()
                .is_some_and(|ip| ip >= site.saturating_sub(256) && ip <= site.saturating_add(64))
        }) {
            // A translated block can cross the wait site without returning.
            // Interpret only its vicinity so the boundary can stop at the head.
            Execution::Interpreter
        } else {
            Execution::Jit
        };
        let storage = core::mem::replace(
            &mut self.bus.ram.storage,
            Storage::Owned {
                bytes: Box::default(),
                generations: Box::default(),
            },
        );
        let Storage::V86(mut owner) = storage else {
            unreachable!("v86 RAM owner");
        };
        let mut port_access = false;
        let mut port_observation = PortObservation::default();
        let operation: Result<_, MachineError> = (|| {
            owner
                .invalidate_host_writes()
                .map_err(|_| MachineError::V86("host code invalidation failed"))?;
            let state = from_shadow(&context, fpu, owner.context.as_ref());
            if owner.context.as_ref() != Some(&state) {
                owner
                    .cpu
                    .restore_context(&state)
                    .map_err(|_| MachineError::V86("context import failed"))?;
                owner.imports += 1;
            }
            owner.cpu.set_time_microseconds(self.bus.guest_nanos / 1000);
            owner
                .cpu
                .set_a20(self.bus.a20_enabled())
                .map_err(|_| MachineError::V86("A20 update failed"))?;
            let irq = if self.cpu.accepts_maskable_interrupt() {
                self.bus.pic.acknowledge()
            } else {
                None
            };
            let result = owner
                .cpu
                .run_with_devices(
                    &mut Devices(&mut self.bus, &mut port_access, &mut port_observation),
                    grant,
                    execution,
                    irq,
                )
                .map_err(|error| {
                    owner.error = Some(alloc::format!("{error:#}"));
                    error
                        .downcast_ref::<BusError>()
                        .copied()
                        .map_or(MachineError::V86("CPU execution failed"), MachineError::Bus)
                })?;
            if irq.is_some() {
                self.interrupt_entries = self.interrupt_entries.wrapping_add(1);
            }
            owner.steps += u64::from(result.steps);
            if owner.host_clock
                && !port_observation.other_port
                && irq.is_none()
                && let (Some(port), Some(site)) = (port_observation.read_port, entry_ip)
            {
                let index = if port == 0x3da { 0 } else { 1 };
                let code_len = if port == 0x3da { 5 } else { 9 };
                let waiting = port_observation.read_value.is_some_and(|value| {
                    owner
                        .cpu
                        .physical_ram()
                        .get(site as usize..site as usize + code_len)
                        .is_some_and(|code| port_poll_waiting(port, code, value))
                });
                owner.port_polls[index].observe(site, owner.steps, waiting);
                owner.port_polls[1 - index].clear();
                if index == 0 {
                    if owner.port_polls[0].active {
                        owner.vga_last_read_step = owner.steps;
                        if owner.vga_trap_site != Some(site) {
                            owner.vga_trap_site = Some(site);
                            owner
                                .cpu
                                .set_wait_boundary(owner.wait_site.or(Some(site)))
                                .map_err(|_| MachineError::V86("VGA wait trap setup failed"))?;
                        }
                    } else if owner.vga_trap_site.is_some() {
                        owner.vga_trap_site = None;
                        owner
                            .cpu
                            .set_wait_boundary(owner.wait_site)
                            .map_err(|_| MachineError::V86("VGA wait trap reset failed"))?;
                    }
                } else if owner.vga_trap_site.is_some() {
                    owner.vga_trap_site = None;
                    owner
                        .cpu
                        .set_wait_boundary(owner.wait_site)
                        .map_err(|_| MachineError::V86("VGA wait trap reset failed"))?;
                }
            } else {
                for poll in &mut owner.port_polls {
                    if port_observation.other_port
                        || irq.is_some()
                        || owner.steps.saturating_sub(poll.last_step) > 16
                    {
                        poll.clear();
                    }
                }
                if (port_observation.other_port || irq.is_some()) && owner.vga_trap_site.is_some() {
                    owner.vga_trap_site = None;
                    owner
                        .cpu
                        .set_wait_boundary(owner.wait_site)
                        .map_err(|_| MachineError::V86("VGA wait trap reset failed"))?;
                }
            }
            owner.interrupts += u64::from(result.interrupt.is_some());
            let updated = owner
                .cpu
                .context()
                .map_err(|_| MachineError::V86("context export failed"))?;
            to_shadow(&mut self.cpu, &updated, &context, updated.fpu != state.fpu);
            owner.context = Some(updated);
            Ok(result)
        })();
        // Restore ownership before propagating any error or advancing devices.
        // No Wasmtime call or native callback can retain the bus/RAM borrow.
        owner.host_clock_yielded = port_access;
        owner.ram_wait_parked = false;
        owner.vga_trap_parked = false;
        self.bus.ram.storage = Storage::V86(owner);
        let result = operation?;
        self.bus.ram.external_execution_complete();
        if let Some(exception) = result.exception {
            return Err(doslet_x86::CpuError::ArchitecturalFault {
                cs: self.cpu.registers.cs,
                ip: self.cpu.registers.ip,
                vector: exception.vector,
                error_code: exception.error_code,
            }
            .into());
        }
        let steps = result.steps.max(1);
        self.advance_hardware_steps(steps)?;
        self.advance_opl_bus_time(self.bus.opl_accesses.wrapping_sub(opl_accesses))?;
        #[cfg(feature = "profiling")]
        {
            self.bus.stats.cpu_steps += u64::from(steps);
        }
        if result.halted && self.cpu.accepts_maskable_interrupt() && !self.v86_host_clock_enabled()
        {
            let idle = self.advance_idle(budget.saturating_sub(steps))?;
            Ok((StepOutcome::Continue, steps + idle))
        } else {
            Ok((
                if result.halted && !self.cpu.accepts_maskable_interrupt() {
                    StepOutcome::Halted
                } else {
                    StepOutcome::Continue
                },
                steps,
            ))
        }
    }
}

#[derive(Default)]
struct PortObservation {
    read_port: Option<u16>,
    read_value: Option<u8>,
    other_port: bool,
}

struct Devices<'a>(&'a mut super::PcBus, &'a mut bool, &'a mut PortObservation);
impl DeviceHost for Devices<'_> {
    fn access(&mut self, ram: &mut [u8], request: DeviceRequest) -> Result<u32, BusError> {
        // Time-sensitive port polling must let the external clock progress.
        // MMIO/framebuffer writes retain large CPU grants.
        *self.1 |= matches!(
            request,
            DeviceRequest::ReadPort { .. } | DeviceRequest::WritePort { .. }
        );
        let tracked_read = matches!(
            request,
            DeviceRequest::ReadPort {
                port: 0x3da | 0x331,
                width: 1
            }
        );
        match request {
            DeviceRequest::ReadPort {
                port: port @ (0x3da | 0x331),
                width: 1,
            } => {
                self.2.read_port = Some(port);
            }
            DeviceRequest::ReadPort { .. } | DeviceRequest::WritePort { .. } => {
                self.2.other_port = true
            }
            _ => {}
        }
        let result = self.0.v86_access(ram, request)?;
        if tracked_read {
            self.2.read_value = Some(result as u8);
        }
        Ok(result)
    }
}

fn segment(cache: SegmentCache) -> Segment {
    Segment {
        selector: cache.selector,
        base: cache.base,
        limit: cache.limit,
        access: cache.access,
        default32: cache.default32,
        usable: cache.usable,
    }
}
fn from_shadow(cpu: &CpuContext, fpu: Option<[u8; 108]>, previous: Option<&Context>) -> Context {
    let mut segments = [Segment::default(); 8];
    for (target, source) in segments.iter_mut().zip(cpu.caches) {
        *target = segment(source);
    }
    if let Some(ldtr) = cpu.control.ldtr {
        segments[LDTR_SEGMENT] = Segment {
            selector: cpu.control.ldtr_selector,
            base: ldtr.base,
            limit: u32::from(ldtr.limit),
            access: 0x82,
            usable: true,
            default32: false,
        };
    }
    if let Some(task) = cpu.control.task {
        segments[TASK_SEGMENT] = segment(task);
    }
    Context {
        registers: Registers {
            gpr: cpu.general,
            eip: cpu.ip,
            eflags: cpu.flags,
            segments: core::array::from_fn(|index| cpu.caches[index].selector),
        },
        segments,
        cr: [
            cpu.control.cr0,
            0,
            cpu.control.cr2,
            cpu.control.cr3,
            previous.map_or(0, |state| state.cr[4]),
        ],
        gdtr: (cpu.control.gdtr.base, cpu.control.gdtr.limit),
        idtr: (cpu.control.idtr.base, cpu.control.idtr.limit),
        cpl: cpu.cpl,
        protected: cpu.protected,
        code32: cpu.caches[1].default32,
        stack32: cpu.caches[2].default32,
        halted: cpu.halted,
        interrupt_shadow: cpu.interrupt_shadow != 0,
        fpu: fpu.unwrap_or_else(|| {
            previous.map_or_else(
                || {
                    let mut image = [0; 108];
                    image[..2].copy_from_slice(&0x37fu16.to_le_bytes());
                    for offset in [2, 6, 8, 10, 26] {
                        image[offset..offset + 2].copy_from_slice(&0xffffu16.to_le_bytes());
                    }
                    image
                },
                |state| state.fpu,
            )
        }),
    }
}
fn to_shadow(cpu: &mut doslet_x86::Cpu, state: &Context, prior: &CpuContext, fpu_changed: bool) {
    let mut context = prior.clone();
    context.general = state.registers.gpr;
    context.ip = state.registers.eip;
    context.flags = state.registers.eflags;
    context.protected = state.protected;
    context.cpl = state.cpl;
    context.halted = state.halted;
    context.interrupt_shadow = u8::from(state.interrupt_shadow);
    context.control.cr0 = state.cr[0];
    context.control.cr2 = state.cr[2];
    context.control.cr3 = state.cr[3];
    context.control.gdtr = DescriptorTable {
        base: state.gdtr.0,
        limit: state.gdtr.1,
    };
    context.control.idtr = DescriptorTable {
        base: state.idtr.0,
        limit: state.idtr.1,
    };
    for (index, source) in state.segments[..6].iter().enumerate() {
        context.caches[index] = SegmentCache {
            selector: source.selector,
            base: source.base,
            limit: source.limit,
            access: source.access,
            flags: prior.caches[index].flags,
            default32: source.default32,
            usable: source.usable,
        };
    }
    let ldtr = state.segments[LDTR_SEGMENT];
    context.control.ldtr_selector = ldtr.selector;
    context.control.ldtr = (ldtr.selector & !3 != 0).then_some(DescriptorTable {
        base: ldtr.base,
        limit: ldtr.limit as u16,
    });
    let task = state.segments[TASK_SEGMENT];
    context.control.task = (task.selector & !3 != 0).then_some(SegmentCache {
        selector: task.selector,
        base: task.base,
        limit: task.limit,
        access: task.access,
        flags: 0,
        default32: task.default32,
        usable: task.usable,
    });
    cpu.restore_context(&context);
    if fpu_changed && let Some(fpu) = &mut cpu.fpu {
        fpu.restore_environment(&state.fpu, true, true, true);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn port_poll_requires_three_dense_reads_from_one_site() {
        let mut poll = PortPoll::new();
        poll.observe(0x200, 1, true);
        poll.observe(0x200, 8, true);
        assert!(!poll.active);
        poll.observe(0x200, 16, true);
        assert!(poll.active);
        assert_eq!(poll.dense_reads, 1);
        poll.observe(0x200, 17, false);
        assert!(!poll.active);
        poll.observe(0x201, 18, true);
        assert!(!poll.active);
        poll.observe(0x201, 40, true);
        assert_eq!(poll.streak, 1);
        poll.clear();
        assert!(!poll.active);
        poll.enabled = false;
        poll.observe(0x201, 41, true);
        assert!(!poll.active);
    }

    #[test]
    fn port_poll_waits_only_while_the_observed_branch_repeats() {
        assert!(port_poll_waiting(0x3da, &[0xEC, 0xA8, 8, 0x74, 0xFB], 0));
        assert!(!port_poll_waiting(0x3da, &[0xEC, 0xA8, 8, 0x74, 0xFB], 8));
        assert!(port_poll_waiting(0x3da, &[0xEC, 0xA8, 8, 0x75, 0xFB], 8));
        assert!(!port_poll_waiting(0x3da, &[0xEC, 0xA8, 8, 0x75, 0xFB], 0));
        let mpu = [0xEC, 0xEB, 0, 0x24, 0x40, 0x74, 6, 0xE2, 0xF7];
        assert!(port_poll_waiting(0x331, &mpu, 0x40));
        assert!(!port_poll_waiting(0x331, &mpu, 0));
        assert!(!port_poll_waiting(0x3da, &[0xEC, 0xA8, 1, 0x74, 0xFB], 0));
    }

    #[test]
    #[ignore = "requires tools/v86-build"]
    fn idempotent_ram_poll_parks_without_changing_cpu_or_guest_clock() {
        use doslet_bus::MemoryBus;

        let module = std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../target/v86/doslet-v86.wasm"
        ))
        .unwrap();
        let mut machine = PcMachine::default();
        machine.bus.set_a20(true);
        let code = [
            0xA1, 0, 0x30, 0, 0, // MOV EAX,[3000h]
            0x89, 0x04, 0x24, // MOV [ESP],EAX
            0x8B, 0x04, 0x24, // MOV EAX,[ESP]
            0x3B, 0x05, 4, 0x30, 0, 0, // CMP EAX,[3004h]
            0x74, 0xED, // JE back to the first MOV
            0xF4, // HLT when the clock changes
        ];
        for (index, byte) in code.into_iter().enumerate() {
            machine.bus.write_u8(0x10000 + index as u32, byte).unwrap();
        }
        for at in [0x3000, 0x3004, 0x4000] {
            machine.bus.write_u32(at, 7).unwrap();
        }
        machine.enable_v86(&module).unwrap();
        machine.set_v86_host_clock(true).unwrap();
        let mut context = machine.cpu.context();
        let data = SegmentCache {
            selector: 0x10,
            base: 0,
            limit: u32::MAX,
            access: 0x93,
            flags: 0xC,
            default32: true,
            usable: true,
        };
        context.caches = [data; 6];
        context.caches[1] = SegmentCache {
            selector: 8,
            access: 0x9B,
            ..data
        };
        context.protected = true;
        context.control.cr0 = 0x11;
        context.ip = 0x10000;
        context.general[0] = 7;
        context.general[4] = 0x4000;
        context.flags = flags::INTERRUPT | flags::ZERO | flags::PARITY | 2;
        machine.cpu.restore_context(&context);
        let nanos = machine.bus.guest_nanos;
        let (outcome, consumed) = machine.step_with_idle_budget(4096).unwrap();
        assert_eq!(outcome, StepOutcome::Continue);
        assert_eq!(consumed, 0);
        assert!(machine.cpu.context() == context);
        assert_eq!(machine.bus.guest_nanos, nanos);
        assert_eq!(machine.v86_ram_waits(), Some((true, 1)));

        // Host clock visits must not execute or recount this loop before an
        // interrupt arrives. The owner may still service device/audio time.
        for _ in 0..8 {
            let (_, consumed) = machine.step_with_idle_budget(4096).unwrap();
            assert_eq!(consumed, 0);
            assert!(machine.v86_ram_wait_parked());
        }
        assert_eq!(machine.v86_ram_waits(), Some((true, 1)));
        assert!(machine.cpu.context() == context);

        machine.bus.pic.raise_edge(0);
        assert!(!machine.v86_ram_wait_latched());
        // This isolated protected-mode fixture has no IDT gate; the full
        // session IRQ test covers delivery through a configured handler.
        assert!(machine.bus.pic.acknowledge().is_some());
        machine.bus.pic.write_command(0x20);

        machine.set_v86_ram_waits(false);
        assert!(!machine.v86_ram_wait_ready());
        machine.set_v86_ram_waits(true);
        machine.set_v86_host_clock(false).unwrap();
        assert!(!machine.v86_ram_wait_ready());
        machine.set_v86_host_clock(true).unwrap();
        assert!(machine.v86_ram_wait_ready());

        // An IRQ-updated value must resume ordinary v86 execution.
        machine.bus.write_u32(0x3000, 8).unwrap();
        assert!(!machine.v86_ram_wait_ready());
        let (_, consumed) = machine.step_with_idle_budget(1).unwrap();
        assert_eq!(consumed, 1);
        assert_eq!(machine.v86_ram_waits(), Some((true, 1)));

        // A large v86 batch may finish inside the loop, not at its first byte.
        // The nearby code scan must install a boundary and land on that byte.
        machine.bus.write_u32(0x3000, 7).unwrap();
        machine.set_v86_ram_waits(false);
        machine.set_v86_ram_waits(true);
        context.ip = 0x1000B;
        machine.cpu.restore_context(&context);
        for _ in 0..8 {
            machine.step_with_idle_budget(4096).unwrap();
            if machine.v86_ram_waits().is_some_and(|(_, parks)| parks > 1) {
                break;
            }
        }
        assert!(machine.v86_ram_wait_parked());
        assert_eq!(machine.v86_ram_waits(), Some((true, 2)));

        // Look behind IP by more than the old 32-byte window.
        machine.set_v86_ram_waits(false);
        machine.set_v86_ram_waits(true);
        context.ip = 0x10080;
        machine.cpu.restore_context(&context);
        assert_eq!(machine.v86_prepare_ram_wait().unwrap(), Some(0x10000));
    }
}
