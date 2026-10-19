//! Runtime side of ABI v1. No WASM engine or frontend dependency.
use crate::{
    BINDINGS, DISPATCH_PROBES, DISPATCH_SLOTS, FLAGS, IP, MAX_QUANTUM, QUOTA, STATE_BYTES,
    Translation,
};
use doslet_bus::CpuBus;
use doslet_x86::{Access, Cpu, CpuError, Gpr32, SegmentRegister, StepOutcome, flags};

const REGISTERS: [Gpr32; 8] = [
    Gpr32::Eax,
    Gpr32::Ecx,
    Gpr32::Edx,
    Gpr32::Ebx,
    Gpr32::Esp,
    Gpr32::Ebp,
    Gpr32::Esi,
    Gpr32::Edi,
];

/// The runtime owns CPU, memory, interrupts, service interception and clocks.
/// A grant promises no external code/mapping changes, IRQ, debugger stop,
/// service interception or device deadline before its end. Return zero if unsure.
/// `interpret` must execute one complete ordinary runtime step (including IRQs
/// and services), or report Halted/error. It is never recursively called by WASM.
pub trait Runtime {
    type Error;
    fn cpu(&self) -> &Cpu;
    fn cpu_mut(&mut self) -> &mut Cpu;
    fn running(&self) -> bool {
        !self.cpu().is_halted()
    }
    /// Apply pending host input before deciding whether a translated grant is safe.
    fn prepare(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
    fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]>;
    fn translated_budget(&self, remaining: u32) -> u32;
    /// CPU state has been committed before this callback. Advance guest clocks
    /// and counters by exactly `steps`, without executing CPU instructions.
    fn account_translated(&mut self, steps: u32) -> Result<(), Self::Error>;
    fn interpret(&mut self) -> Result<StepOutcome, Self::Error>;
}

#[derive(Debug)]
pub enum Error<E> {
    Runtime(E),
    Protocol(&'static str),
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub polls: u64,
    pub translated_steps: u64,
    pub interpreted_steps: u64,
    pub grants: u64,
    pub zero_budget: u64,
    pub rejected_context: u64,
    pub missing_code: u64,
}

pub struct Host<R> {
    translation: Translation,
    runtime: R,
    outstanding: u32,
    stats: Stats,
}
impl<R: Runtime> Host<R> {
    pub fn new(translation: Translation, runtime: R) -> Self {
        Self {
            translation,
            runtime,
            outstanding: 0,
            stats: Stats::default(),
        }
    }
    pub fn translation(&self) -> &Translation {
        &self.translation
    }
    pub fn runtime(&self) -> &R {
        &self.runtime
    }
    pub fn runtime_mut(&mut self) -> &mut R {
        &mut self.runtime
    }
    /// Restore a benchmark owner between completed runs, keeping prepared code.
    /// A run must flush its final grant before replacing runtime state.
    pub fn replace_runtime(&mut self, runtime: R) -> Result<(), &'static str> {
        if self.outstanding != 0 {
            return Err("cannot replace an active translated runtime");
        }
        self.runtime = runtime;
        self.stats = Stats::default();
        Ok(())
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }
    /// ABI import `doslet.poll(completed, remaining) -> block index`.
    /// -1 means one interpreter step succeeded, -2 stops this run.
    /// Call only from the matching compiler-produced module, synchronously and
    /// exclusively. Memory is that module's exported two-page scratch memory.
    pub fn poll(
        &mut self,
        completed: u32,
        remaining: u32,
        memory: &mut [u8],
    ) -> Result<i32, Error<R::Error>> {
        if memory.len() < STATE_BYTES {
            return Err(Error::Protocol("short ABI memory"));
        }
        if completed > self.outstanding {
            return Err(Error::Protocol("translated grant exceeded"));
        }
        if completed != 0 {
            let cpu = self.runtime.cpu_mut();
            for (i, reg) in REGISTERS.iter().enumerate() {
                cpu.registers.set32(*reg, read(memory, i as u32 * 4));
            }
            cpu.flags = read(memory, FLAGS);
            cpu.registers.ip = read(memory, IP);
            self.runtime
                .account_translated(completed)
                .map_err(Error::Runtime)?;
            self.stats.translated_steps += u64::from(completed);
        }
        self.outstanding = 0;
        self.stats.polls += 1;
        if remaining == 0 || !self.runtime.running() {
            return Ok(-2);
        }
        self.runtime.prepare().map_err(Error::Runtime)?;
        let quota = self
            .runtime
            .translated_budget(remaining)
            .min(remaining)
            .min(MAX_QUANTUM);
        let cpu = self.runtime.cpu();
        let cs = cpu.registers.segment_cache(SegmentRegister::Cs);
        let eligible = quota != 0
            && !cpu.is_halted()
            && cpu.control.cr0 & 0x8000_0001 == 1
            && cpu.registers.code32()
            && cs.base == 0
            && cs.limit == u32::MAX
            && !cpu.has_interrupt_shadow()
            && cpu.flags & (flags::TRAP | flags::RESUME | flags::VIRTUAL) == 0
            && !cpu.execution_labels_enabled();
        if quota == 0 {
            self.stats.zero_budget += 1;
        } else if !eligible {
            self.stats.rejected_context += 1;
        }
        if eligible {
            let ip = cpu.registers.ip;
            for (i, r) in REGISTERS.iter().enumerate() {
                write(memory, i as u32 * 4, cpu.registers.get32(*r));
            }
            write(memory, FLAGS, cpu.flags);
            write(memory, IP, ip);
            write(memory, QUOTA, quota);
            memory[BINDINGS as usize..STATE_BYTES].fill(0);
            for (index, block) in self.translation.blocks.iter().enumerate() {
                let matches = |address: u32| {
                    cpu.registers.address(
                        SegmentRegister::Cs,
                        address,
                        block.bytes.len() as u8,
                        Access::Execute,
                    ) == Ok(address)
                        && self.runtime.code_bytes(address, block.bytes.len() as u8)
                            == Some(block.bytes.as_slice())
                };
                // Hints populate cross-block bindings. Current bytes also allow
                // a profile record to bind at a different load address.
                for address in [
                    Some(block.address_hint),
                    (ip != block.address_hint).then_some(ip),
                ]
                .into_iter()
                .flatten()
                {
                    if matches(address) {
                        bind(
                            memory,
                            address,
                            index as u32,
                            block.instructions.len() as u32,
                        );
                    }
                }
            }
            if let Some(index) = resolve(memory, ip, quota) {
                self.outstanding = quota;
                self.stats.grants += 1;
                return Ok(index);
            }
            self.stats.missing_code += 1;
        }
        match self.runtime.interpret().map_err(Error::Runtime)? {
            StepOutcome::Continue => {
                self.stats.interpreted_steps += 1;
                Ok(-1)
            }
            StepOutcome::Halted => {
                // HLT itself consumed one step. The next poll stops on halted CPU.
                self.stats.interpreted_steps += 1;
                Ok(-1)
            }
        }
    }
}
fn hash(ip: u32) -> u32 {
    ip.wrapping_mul(0x9e37_79b1) >> 19
}
fn bind(memory: &mut [u8], ip: u32, index: u32, count: u32) {
    for probe in 0..DISPATCH_PROBES {
        let slot = (hash(ip) + probe) & (DISPATCH_SLOTS as u32 - 1);
        let offset = BINDINGS + slot * 8;
        if read(memory, offset + 4) == 0 {
            write(memory, offset, ip);
            write(memory, offset + 4, (count << 16) | (index + 1));
            return;
        }
        if read(memory, offset) == ip {
            return;
        }
    }
    // Saturated probe chains deliberately leave this entry on the interpreter.
}
fn resolve(memory: &[u8], ip: u32, quota: u32) -> Option<i32> {
    for probe in 0..DISPATCH_PROBES {
        let slot = (hash(ip) + probe) & (DISPATCH_SLOTS as u32 - 1);
        let offset = BINDINGS + slot * 8;
        let packed = read(memory, offset + 4);
        if packed == 0 {
            return None;
        }
        if read(memory, offset) == ip {
            return ((packed >> 16) <= quota).then_some(((packed & 0xffff) - 1) as i32);
        }
    }
    None
}
fn read(memory: &[u8], offset: u32) -> u32 {
    u32::from_le_bytes(
        memory[offset as usize..offset as usize + 4]
            .try_into()
            .unwrap(),
    )
}
fn write(memory: &mut [u8], offset: u32, value: u32) {
    memory[offset as usize..offset as usize + 4].copy_from_slice(&value.to_le_bytes());
}

/// CPU/bus test adapter. No PC timers or runtime service interception; use a
/// machine/session Runtime implementation before running an actual game.
pub struct CpuRuntime<B> {
    pub cpu: Cpu,
    pub bus: B,
    pub translated_limit: u32,
}
impl<B: CpuBus> Runtime for CpuRuntime<B> {
    type Error = CpuError;
    fn cpu(&self) -> &Cpu {
        &self.cpu
    }
    fn cpu_mut(&mut self) -> &mut Cpu {
        &mut self.cpu
    }
    fn code_bytes(&self, address: u32, length: u8) -> Option<&[u8]> {
        self.bus.code_bytes(address, length)
    }
    fn translated_budget(&self, remaining: u32) -> u32 {
        remaining.min(self.translated_limit)
    }
    fn account_translated(&mut self, _steps: u32) -> Result<(), CpuError> {
        Ok(())
    }
    fn interpret(&mut self) -> Result<StepOutcome, CpuError> {
        self.cpu.step(&mut self.bus)
    }
}
