//! Opt-in DPMI experiment. The host owns its gates, so guest-installed handlers
//! execute before any host service. Direct dispatch is restricted to unchanged
//! runtime IVT shims and bounded, nonblocking services.
use super::*;
use doslet_x86::{Access, CpuContext, DescriptorTable, Gpr32, SegmentRegister as Seg};
mod memory;
pub use memory::{Block, MemoryStats};
#[cfg(feature = "save-state")]
mod saved;

const ENTRY: u32 = 0x800;
const RM_RETURN: u32 = 0x810;
const RAW_PM: u32 = 0x820;
const RAW_RM: u32 = 0x830;
const SAVE_PM: u32 = 0x840;
const SAVE_RM: u32 = 0x850;
const CB_RETURN: u32 = 0x860;
const TIMER_RETURN: u32 = 0x870;
const EXCEPTION_RETURN: u32 = 0x880;
const CB_BASE: u32 = 0x2000;
const PM_BASE: u32 = 0x1000;
const GDT: u32 = 0x100000;
const LDT: u32 = 0x101000;
const IDT: u32 = 0x111000;
const POOL: u32 = 0x120000;
/// A 16 KiB locked stack for one exception handler, between the IDT and the
/// nested frames' stack slices below `POOL`.
const EXCEPTION_STACK: u32 = POOL - 16 * 0x400;
const CODE: u16 = 11;
const DATA: u16 = 19;
const DESCRIPTORS: usize = 8192;
/// Stack words 0300h–0302h copy, within a host stack slice of 1 KiB.
const MAX_STACK_WORDS: u16 = 256;
/// LDT entries 1–15 (selectors 04h–7Ch) are reserved for 000Dh.
const SPECIFIC: usize = 16;
const GENERAL: [Gpr32; 8] = [
    Gpr32::Eax,
    Gpr32::Ecx,
    Gpr32::Edx,
    Gpr32::Ebx,
    Gpr32::Esp,
    Gpr32::Ebp,
    Gpr32::Esi,
    Gpr32::Edi,
];
const RM_OFFSETS: [u32; 8] = [28, 24, 20, 16, 46, 8, 4, 0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Host DPMI with guest real-mode execution for every reflected service.
    Reflect,
    /// The same host, with direct dispatch of eligible runtime services.
    Direct,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub entered: bool,
    pub calls: u64,
    pub direct: u64,
    pub reflected: u64,
    pub unsupported: u64,
    pub last_function: u16,
    pub recent_functions: [u16; 16],
    pub unsupported_functions: [u16; 16],
    pub memory: MemoryStats,
}

#[derive(Clone)]
struct Frame {
    context: CpuContext,
    structure: Option<u32>,
    callback: bool,
    timer: bool,
    /// A client exception handler runs; its RETF reaches `EXCEPTION_RETURN`.
    exception: bool,
}

#[derive(Clone, Copy)]
struct Callback {
    code: u16,
    ip: u32,
    data: u16,
    offset: u32,
    structure: u32,
}

#[derive(Clone)]
pub(super) struct Host {
    pub(super) mode: Mode,
    stats: Stats,
    used: [bool; DESCRIPTORS],
    dos_segments: [u16; DESCRIPTORS],
    exceptions: [(u16, u32); 32],
    memory: memory::Memory,
    frames: [Option<Frame>; 16],
    depth: usize,
    private_segment: u16,
    bits32: bool,
    callbacks: [Option<Callback>; 32],
    pub(super) direct_return_pending: bool,
}

impl Host {
    /// A client registered an exception handler, so faults must be precise.
    pub(super) fn handles_exceptions(&self) -> bool {
        self.exceptions.iter().any(|&(selector, _)| selector != 0)
    }

    /// The host's memory pool lies within `ram` bytes of installed RAM.
    #[cfg(feature = "save-state")]
    pub(super) fn fits(&self, ram: u32) -> bool {
        self.memory.end <= ram
    }
}

impl Session {
    /// Setup-only host installation; callers select whether to expose DPMI.
    pub fn enable_dpmi(&mut self, mode: Mode) -> Result<(), RuntimeError> {
        if self.status != SessionStatus::Ready || self.dpmi.is_some() {
            return Err(RuntimeError::InvalidLaunch);
        }
        let end = self.machine.bus.ram_size() as u32;
        self.dpmi = Some(Box::new(Host {
            mode,
            stats: Stats::default(),
            used: [false; DESCRIPTORS],
            dos_segments: [0; DESCRIPTORS],
            exceptions: [(0, 0); 32],
            memory: memory::Memory::new(POOL, end),
            frames: core::array::from_fn(|_| None),
            depth: 0,
            private_segment: 0,
            bits32: false,
            callbacks: [None; 32],
            direct_return_pending: false,
        }));
        for offset in [
            ENTRY,
            RM_RETURN,
            RAW_PM,
            RAW_RM,
            SAVE_PM,
            SAVE_RM,
            CB_RETURN,
            TIMER_RETURN,
            EXCEPTION_RETURN,
        ] {
            self.dpmi_marker(offset)?;
        }
        for offset in [SAVE_PM, SAVE_RM] {
            self.machine.bus.write_u8(bios::SHIM_BASE + offset, 0xCB)?;
        }
        for i in 0..32 {
            self.dpmi_marker(CB_BASE + i * 8)?;
        }
        Ok(())
    }

    /// The installed host, if any. Loaders place protected-mode code by it.
    pub fn dpmi_mode(&self) -> Option<Mode> {
        self.dpmi.as_ref().map(|h| h.mode)
    }

    pub fn dpmi_stats(&self) -> Option<Stats> {
        self.dpmi.as_ref().map(|h| Stats {
            memory: h.memory.stats,
            ..h.stats
        })
    }

    /// Extended-memory pool managed by the built-in host, excluding its tables.
    pub fn dpmi_memory_capacity(&self) -> Option<u64> {
        self.dpmi.as_ref().map(|h| h.memory.capacity())
    }

    pub fn dpmi_blocks(&self) -> impl Iterator<Item = &Block> {
        self.dpmi
            .iter()
            .flat_map(|h| h.memory.blocks.iter())
            .filter(|b| b.handle != 0)
    }

    fn dpmi_marker(&mut self, offset: u32) -> Result<(), RuntimeError> {
        // The byte after IRET is never executed. Its signature prevents an
        // altered shim from accidentally entering the host.
        for (i, b) in [0xCF, b'D', b'P', b'M', b'I'].into_iter().enumerate() {
            self.machine
                .bus
                .write_u8(bios::SHIM_BASE + offset + i as u32, b)?;
        }
        Ok(())
    }

    pub(super) fn dpmi_probe(&mut self) {
        let r = &mut self.machine.cpu.registers;
        if self.dpmi.as_ref().is_none_or(|h| h.stats.entered) {
            r.set(Gpr16::Ax, 1);
            return;
        }
        r.set(Gpr16::Ax, 0);
        r.set(Gpr16::Bx, 1);
        r.set(Gpr16::Cx, 3);
        r.set(Gpr16::Dx, 0x005A);
        r.set(Gpr16::Si, 0x400); // 16 KiB, including bounded nested RM stacks.
        r.es = bios::SHIM_SEGMENT;
        r.set(Gpr16::Di, ENTRY as u16);
    }

    pub(super) fn dpmi_dispatch(&mut self) -> Result<(), RuntimeError> {
        if self.dpmi.as_ref().unwrap().direct_return_pending {
            self.dpmi.as_mut().unwrap().direct_return_pending = false;
            return Ok(());
        }
        let cpu = &self.machine.cpu;
        let ip = cpu.registers.ip;
        let real = !cpu.protected_mode();
        if (real && cpu.registers.cs != bios::SHIM_SEGMENT)
            || (!real
                && (cpu.registers.cs != CODE
                    || !cpu.registers.code32()
                    || cpu.registers.segment_cache(Seg::Cs).base != bios::SHIM_BASE))
        {
            return Ok(());
        }
        let callback =
            real && (CB_BASE..CB_BASE + 32 * 8).contains(&ip) && (ip - CB_BASE).is_multiple_of(8);
        // The BIOS invokes INT 1Ch in real mode. DPMI must deliver that tick
        // to the installed protected handler as well. A handler chaining back
        // to the default reflection must reach the real IRET without recursion.
        if real && ip == 0x1C && self.machine.bus.read_u8(bios::SHIM_BASE + ip)? == 0xCF {
            return self.dpmi_timer_tick();
        }
        let known = if real {
            callback || matches!(ip, ENTRY | RM_RETURN | RAW_PM | SAVE_RM)
        } else {
            matches!(
                ip,
                RAW_RM | SAVE_PM | CB_RETURN | TIMER_RETURN | EXCEPTION_RETURN
            ) || ((PM_BASE..PM_BASE + 256 * 8).contains(&ip) && (ip - PM_BASE).is_multiple_of(8))
        };
        if !known {
            return Ok(());
        }
        for (i, b) in [
            if matches!(ip, SAVE_PM | SAVE_RM) {
                0xCB
            } else {
                0xCF
            },
            b'D',
            b'P',
            b'M',
            b'I',
        ]
        .into_iter()
        .enumerate()
        {
            if self.machine.bus.read_u8(bios::SHIM_BASE + ip + i as u32)? != b {
                return Ok(());
            }
        }
        if matches!(ip, SAVE_PM | SAVE_RM) {
            return Ok(());
        }
        if matches!(ip, RAW_PM | RAW_RM) {
            return self.dpmi_raw_switch(ip == RAW_PM);
        }
        if callback {
            return self.dpmi_callback(((ip - CB_BASE) / 8) as usize);
        }
        if ip == CB_RETURN {
            return self.dpmi_callback_return();
        }
        if ip == EXCEPTION_RETURN {
            return self.dpmi_exception_return();
        }
        if ip == TIMER_RETURN {
            let h = self.dpmi.as_mut().unwrap();
            if h.depth == 0 || !h.frames[h.depth - 1].as_ref().unwrap().timer {
                return Err(dpmi_error(0x8021));
            }
            h.depth -= 1;
            let frame = h.frames[h.depth].take().unwrap();
            self.machine.cpu.restore_context(&frame.context);
            return Ok(());
        }
        if real {
            if ip == ENTRY {
                self.dpmi_enter()
            } else {
                self.dpmi_real_return()
            }
        } else {
            let vector = ((ip - PM_BASE) / 8) as u8;
            if vector == 0x31 {
                self.dpmi_int31()
            } else if vector == 0x21 && self.registers().get_byte(4) == 0x4C {
                self.status = SessionStatus::Exited(self.registers().get_byte(0));
                Ok(())
            } else {
                self.dpmi_reflect(vector, None, None, false)
            }
        }
    }

    /// Deliver a CPU fault of the protected-mode client to the handler it
    /// registered with 0203h, as DPMI 0.9 describes: on a locked host stack
    /// with interrupts disabled and a frame of the return address, error code,
    /// CS:EIP, EFLAGS and SS:ESP. Faults in host code, faults while a handler
    /// runs and vectors without a handler keep stopping the session.
    pub(super) fn dpmi_exception(&mut self, error: &RuntimeError) -> bool {
        let RuntimeError::Cpu(error) = error else {
            return false;
        };
        let Some(fault) = error.fault() else {
            return false;
        };
        let Some(h) = &self.dpmi else {
            return false;
        };
        let (selector, offset) = h.exceptions[usize::from(fault.vector)];
        let cpu = &self.machine.cpu;
        if !h.stats.entered
            || selector == 0
            || !cpu.protected_mode()
            || cpu.registers.cs == CODE
            || h.depth == h.frames.len()
            || h.frames[..h.depth].iter().flatten().any(|f| f.exception)
        {
            return false;
        }
        let bits32 = h.bits32;
        let client = [
            fault.error_code.unwrap_or(0),
            cpu.registers.ip,
            u32::from(cpu.registers.cs),
            cpu.flags,
            cpu.registers.get32(Gpr32::Esp),
            u32::from(cpu.registers.ss),
        ];
        let context = cpu.context();
        self.deliver_exception(selector, offset, bits32, client, context)
            .is_ok()
    }

    fn deliver_exception(
        &mut self,
        selector: u16,
        offset: u32,
        bits32: bool,
        client: [u32; 6],
        context: CpuContext,
    ) -> Result<(), RuntimeError> {
        // States saved before this marker existed restore without it.
        self.dpmi_marker(EXCEPTION_RETURN)?;
        let width = if bits32 { 4 } else { 2 };
        let top = EXCEPTION_STACK - 8 * width;
        for (index, value) in [EXCEPTION_RETURN, u32::from(CODE)]
            .into_iter()
            .chain(client)
            .enumerate()
        {
            let at = top + index as u32 * width;
            if bits32 {
                self.machine.bus.write_u32(at, value)?;
            } else {
                self.machine.bus.write_u16(at, value as u16)?;
            }
        }
        self.machine
            .cpu
            .load_segment(&mut self.machine.bus, Seg::Cs, selector)?;
        self.machine
            .cpu
            .load_segment(&mut self.machine.bus, Seg::Ss, DATA)?;
        self.machine.cpu.registers.set32(Gpr32::Esp, top);
        self.machine.cpu.registers.ip = offset;
        // Interrupts and single-stepping stay off until the handler returns.
        self.machine.cpu.flags &= !0x300;
        let h = self.dpmi.as_mut().unwrap();
        h.frames[h.depth] = Some(Frame {
            context,
            structure: None,
            callback: false,
            timer: false,
            exception: true,
        });
        h.depth += 1;
        self.machine.advance_service_step()?;
        Ok(())
    }

    /// The handler's RETF consumed the return address; the client resumes at
    /// the frame's CS:EIP, EFLAGS and SS:ESP, which the handler may change.
    fn dpmi_exception_return(&mut self) -> Result<(), RuntimeError> {
        let h = self.dpmi.as_mut().unwrap();
        if h.depth == 0 || !h.frames[h.depth - 1].as_ref().unwrap().exception {
            return Err(dpmi_error(0x8021));
        }
        let bits32 = h.bits32;
        h.depth -= 1;
        h.frames[h.depth] = None;
        let width = if bits32 { 4 } else { 2 };
        let stack = self
            .machine
            .cpu
            .registers
            .segment_cache(Seg::Ss)
            .base
            .wrapping_add(self.machine.cpu.stack_pointer());
        let mut frame = [0u32; 6];
        for (index, value) in frame.iter_mut().enumerate() {
            let at = stack.wrapping_add(index as u32 * width);
            *value = if bits32 {
                self.machine.bus.read_u32(at)?
            } else {
                u32::from(self.machine.bus.read_u16(at)?)
            };
        }
        let [_, eip, cs, eflags, esp, ss] = frame;
        self.machine
            .cpu
            .load_segment(&mut self.machine.bus, Seg::Ss, ss as u16)?;
        self.machine.cpu.registers.set32(Gpr32::Esp, esp);
        self.machine
            .cpu
            .load_segment(&mut self.machine.bus, Seg::Cs, cs as u16)?;
        self.machine.cpu.registers.ip = eip;
        // IOPL and VM stay the host's; reserved bit 1 stays set.
        let kept = 0x0002_3000;
        self.machine.cpu.flags = (eflags & !kept) | (self.machine.cpu.flags & kept) | 2;
        Ok(())
    }

    fn dpmi_timer_tick(&mut self) -> Result<(), RuntimeError> {
        let h = self.dpmi.as_ref().unwrap();
        if !h.stats.entered || h.frames[..h.depth].iter().flatten().any(|f| f.timer) {
            return Ok(());
        }
        let at = IDT + 0x1C * 8;
        let low = self.machine.bus.read_u32(at)?;
        let high = self.machine.bus.read_u32(at + 4)?;
        if low == (u32::from(CODE) << 16) | (PM_BASE + 0x1C * 8) && high & 0xFFFF0000 == 0 {
            return Ok(());
        }
        let h = self.dpmi.as_mut().unwrap();
        if h.depth == h.frames.len() {
            return Err(dpmi_error(0x8012));
        }
        h.frames[h.depth] = Some(Frame {
            context: self.machine.cpu.context(),
            structure: None,
            callback: false,
            timer: true,
            exception: false,
        });
        h.depth += 1;
        let sp = POOL - h.depth as u32 * 0x400;
        self.machine.cpu.set_protected_mode(true);
        self.machine.cpu.set_current_privilege_level(3);
        self.machine.cpu.control.idtr = DescriptorTable {
            base: IDT,
            limit: 2047,
        };
        for (segment, selector) in [
            (Seg::Cs, CODE),
            (Seg::Ss, DATA),
            (Seg::Ds, 0),
            (Seg::Es, 0),
            (Seg::Fs, 0),
            (Seg::Gs, 0),
        ] {
            self.machine
                .cpu
                .load_segment(&mut self.machine.bus, segment, selector)?;
        }
        self.machine.cpu.registers.set32(Gpr32::Esp, sp);
        self.machine.cpu.registers.ip = TIMER_RETURN;
        self.machine.cpu.interrupt(&mut self.machine.bus, 0x1C)?;
        Ok(())
    }

    fn dpmi_enter(&mut self) -> Result<(), RuntimeError> {
        if self.dpmi.as_ref().unwrap().stats.entered {
            return Err(RuntimeError::UnsupportedService {
                vector: 0x2F,
                ax: 0x1687,
            });
        }
        let r = self.registers();
        let bits32 = r.get(Gpr16::Ax) & 1 != 0;
        let private = r.es;
        let sp = r.get(Gpr16::Sp);
        let ss = r.ss;
        let ds = r.ds;
        let ret_ip = self.machine.bus.read_u16(linear_address(ss, sp))?;
        let ret_cs = self
            .machine
            .bus
            .read_u16(linear_address(ss, sp.wrapping_add(2)))?;
        self.machine.bus.set_a20(true);
        self.dpmi_descriptor_at(GDT + 8, bios::SHIM_BASE, 0xFFFF, 0x9A, 0x40)?;
        self.dpmi_descriptor_at(GDT + 16, 0, u32::MAX, 0x92, 0x40)?;
        self.dpmi_descriptor_at(GDT + 24, LDT, (DESCRIPTORS * 8 - 1) as u32, 0x82, 0)?;
        // Conventional DPMI BIOS-data selector used by DOS/4GW's DOS services.
        self.dpmi_descriptor_at(GDT + 0x40, 0x400, 0xFFFF, 0x92, 0)?;
        self.machine.cpu.control.gdtr = DescriptorTable {
            base: GDT,
            limit: 0xFF,
        };
        self.machine.cpu.control.ldtr = Some(DescriptorTable {
            base: LDT,
            limit: (DESCRIPTORS * 8 - 1) as u16,
        });
        self.machine.cpu.control.ldtr_selector = 24;
        self.machine.cpu.control.idtr = DescriptorTable {
            base: IDT,
            limit: 2047,
        };
        for vector in 0..256u16 {
            let offset = PM_BASE + u32::from(vector) * 8;
            self.dpmi_marker(offset)?;
            self.dpmi_gate(vector as u8, CODE, offset)?;
        }
        let cs_sel = self.dpmi_alloc_descriptors(1).map_err(dpmi_error)?;
        let ds_sel = self.dpmi_alloc_descriptors(1).map_err(dpmi_error)?;
        let ss_sel = self.dpmi_alloc_descriptors(1).map_err(dpmi_error)?;
        let psp_sel = self.dpmi_alloc_descriptors(1).map_err(dpmi_error)?;
        for (sel, seg, access, flags, limit) in [
            (cs_sel, ret_cs, 0x9A, 0, 0xFFFF),
            (ds_sel, ds, 0x92, if bits32 { 0x40 } else { 0 }, 0xFFFF),
            (ss_sel, ss, 0x92, if bits32 { 0x40 } else { 0 }, 0xFFFF),
            (psp_sel, 0x1000, 0x92, 0, 0xFF),
        ] {
            self.dpmi_descriptor_at(
                LDT + u32::from(sel & !7),
                u32::from(seg) * 16,
                limit,
                access,
                flags,
            )?;
        }
        let environment = self.machine.bus.read_u16(0x1002C)?;
        if environment != 0 {
            let env_sel = self.dpmi_real_alias(environment)?;
            self.machine.bus.write_u16(0x1002C, env_sel)?;
        }
        self.machine.cpu.set_protected_mode(true);
        self.machine.cpu.set_current_privilege_level(3);
        for (segment, selector) in [
            (Seg::Cs, cs_sel),
            (Seg::Ds, ds_sel),
            (Seg::Ss, ss_sel),
            (Seg::Es, psp_sel),
            (Seg::Fs, 0),
            (Seg::Gs, 0),
        ] {
            self.machine
                .cpu
                .load_segment(&mut self.machine.bus, segment, selector)?;
        }
        self.machine.cpu.registers.ip = u32::from(ret_ip);
        self.machine
            .cpu
            .registers
            .set32(Gpr32::Esp, u32::from(sp.wrapping_add(4)));
        self.machine.cpu.flags = (self.machine.cpu.flags & !1) | 0x3000;
        let h = self.dpmi.as_mut().unwrap();
        h.bits32 = bits32;
        h.private_segment = private;
        h.stats.entered = true;
        Ok(())
    }

    fn dpmi_descriptor_at(
        &mut self,
        at: u32,
        base: u32,
        mut limit: u32,
        access: u8,
        mut flags: u8,
    ) -> Result<(), RuntimeError> {
        let access = if access & 0x10 != 0 {
            access | 0x60
        } else {
            access
        };
        if limit > 0xFFFFF {
            limit >>= 12;
            flags |= 0x80;
        }
        let bytes = [
            limit as u8,
            (limit >> 8) as u8,
            base as u8,
            (base >> 8) as u8,
            (base >> 16) as u8,
            access,
            ((limit >> 16) as u8 & 15) | flags,
            (base >> 24) as u8,
        ];
        for (i, byte) in bytes.into_iter().enumerate() {
            self.machine.bus.write_u8(at + i as u32, byte)?;
        }
        Ok(())
    }

    fn dpmi_alloc_descriptors(&mut self, count: u16) -> Result<u16, u16> {
        let h = self.dpmi.as_mut().unwrap();
        let count = usize::from(count);
        if count == 0 || count > DESCRIPTORS - SPECIFIC {
            return Err(0x8011);
        }
        for first in SPECIFIC..=DESCRIPTORS - count {
            if h.used[first..first + count].iter().all(|v| !v) {
                h.used[first..first + count].fill(true);
                return Ok((first as u16 * 8) | 7);
            }
        }
        Err(0x8011)
    }

    /// A host entered through a gate saves the client's data segments and
    /// reloads them on return, so descriptor changes take effect at once;
    /// DJGPP raises its DS limit this way. Selectors that are no longer valid,
    /// such as one just freed, become null, as 0001h requires. The host's IRET
    /// reloads CS.
    fn dpmi_reload_segments(&mut self) -> Result<(), RuntimeError> {
        for segment in [Seg::Ss, Seg::Ds, Seg::Es, Seg::Fs, Seg::Gs] {
            let selector = self.machine.cpu.registers.selector(segment);
            if self
                .machine
                .cpu
                .load_segment(&mut self.machine.bus, segment, selector)
                .is_err()
                && segment != Seg::Ss
            {
                self.machine
                    .cpu
                    .load_segment(&mut self.machine.bus, segment, 0)?;
            }
        }
        Ok(())
    }

    /// Copy a moved block's contents within installed RAM.
    fn dpmi_move(&mut self, from: u32, to: u32, length: u32) -> Result<(), RuntimeError> {
        let start = from.min(to);
        let span = from.max(to) - start + length;
        let ram = self
            .machine
            .bus
            .debug_ram_mut(start, span)
            .ok_or_else(|| dpmi_error(0x8013))?;
        let source = (from - start) as usize;
        ram.copy_within(source..source + length as usize, (to - start) as usize);
        Ok(())
    }

    fn dpmi_real_alias(&mut self, segment: u16) -> Result<u16, RuntimeError> {
        let selector = self.dpmi_alloc_descriptors(1).map_err(dpmi_error)?;
        self.dpmi_descriptor_at(
            LDT + u32::from(selector & !7),
            u32::from(segment) * 16,
            0xFFFF,
            0x92,
            0,
        )?;
        Ok(selector)
    }

    fn dpmi_gate(&mut self, vector: u8, selector: u16, offset: u32) -> Result<(), RuntimeError> {
        let at = IDT + u32::from(vector) * 8;
        self.machine
            .bus
            .write_u32(at, (u32::from(selector) << 16) | (offset & 0xFFFF))?;
        self.machine
            .bus
            .write_u32(at + 4, (offset & 0xFFFF0000) | 0xEE00)?;
        Ok(())
    }

    fn dpmi_pointer(
        &self,
        segment: Seg,
        offset: u32,
        length: u8,
        access: Access,
    ) -> Result<u32, RuntimeError> {
        Ok(self.registers().address(segment, offset, length, access)?)
    }

    fn dpmi_offset(&self, reg: Gpr32) -> u32 {
        let value = self.registers().get32(reg);
        if self.dpmi.as_ref().unwrap().bits32 {
            value
        } else {
            value & 0xFFFF
        }
    }

    fn dpmi_cf(&mut self, error: Option<u16>) -> Result<(), RuntimeError> {
        let sp = self.machine.cpu.stack_pointer();
        let at = self.dpmi_pointer(Seg::Ss, sp.wrapping_add(8), 4, Access::Write)?;
        let flags = self.machine.bus.read_u32(at)?;
        self.machine
            .bus
            .write_u32(at, (flags & !1) | u32::from(error.is_some()))?;
        if let Some(error) = error {
            self.machine.cpu.registers.set(Gpr16::Ax, error);
        }
        Ok(())
    }

    fn dpmi_int31(&mut self) -> Result<(), RuntimeError> {
        let ax = self.registers().get(Gpr16::Ax);
        let h = self.dpmi.as_mut().unwrap();
        h.stats.calls += 1;
        h.stats.last_function = ax;
        h.stats.recent_functions[(h.stats.calls as usize - 1) % 16] = ax;
        if (0x0300..=0x0302).contains(&ax) {
            let vector = self.registers().get_byte(3);
            let at = self.dpmi_pointer(Seg::Es, self.dpmi_offset(Gpr32::Edi), 50, Access::Write)?;
            // Old DOS/4GW clients set the historical PIC-reset flag (BH bit
            // zero). This virtual PC retains the same PIC mapping in both modes.
            let words = self.registers().get(Gpr16::Cx);
            if words > MAX_STACK_WORDS || (ax == 0x300 && self.registers().get_byte(7) & !1 != 0) {
                return self.dpmi_cf(Some(0x8021));
            }
            let target = if ax == 0x300 {
                None
            } else {
                Some((
                    self.machine.bus.read_u16(at + 44)?,
                    self.machine.bus.read_u16(at + 42)?,
                ))
            };
            return self.dpmi_reflect_with(vector, Some(at), target, ax == 0x301, words);
        }
        let result = self.dpmi_function(ax)?;
        if matches!(ax, 0x0001 | 0x0007..=0x0009 | 0x000C) {
            self.dpmi_reload_segments()?;
        }
        self.dpmi_cf(result)
    }

    fn dpmi_function(&mut self, function: u16) -> Result<Option<u16>, RuntimeError> {
        let r = self.registers();
        let (bx, cx, dx) = (r.get(Gpr16::Bx), r.get(Gpr16::Cx), r.get(Gpr16::Dx));
        let pair = (u32::from(bx) << 16) | u32::from(cx);
        let handle = (u32::from(r.get(Gpr16::Si)) << 16) | u32::from(r.get(Gpr16::Di));
        let index = usize::from(bx >> 3);
        let own = bx & 4 != 0 && index < DESCRIPTORS && self.dpmi.as_ref().unwrap().used[index];
        let descriptor = LDT + u32::from(bx & !7);
        match function {
            0x0000 => match self.dpmi_alloc_descriptors(cx) {
                Ok(selector) => {
                    for i in 0..cx {
                        self.dpmi_descriptor_at(
                            LDT + u32::from((selector & !7) + i * 8),
                            0,
                            0,
                            0x92,
                            0,
                        )?;
                    }
                    self.machine.cpu.registers.set(Gpr16::Ax, selector);
                }
                Err(e) => return Ok(Some(e)),
            },
            0x000D => {
                let h = self.dpmi.as_mut().unwrap();
                if bx & 4 == 0 || !(1..SPECIFIC).contains(&index) {
                    return Ok(Some(0x8022));
                }
                if h.used[index] {
                    return Ok(Some(0x8011));
                }
                h.used[index] = true;
                self.dpmi_descriptor_at(descriptor, 0, 0, 0x92, 0)?;
            }
            0x0001 if own => {
                self.dpmi.as_mut().unwrap().used[index] = false;
                self.machine.bus.write_u32(descriptor, 0)?;
                self.machine.bus.write_u32(descriptor + 4, 0)?;
            }
            0x0002 => {
                let sel = match self.dpmi_alloc_descriptors(1) {
                    Ok(s) => s,
                    Err(e) => return Ok(Some(e)),
                };
                self.dpmi_descriptor_at(
                    LDT + u32::from(sel & !7),
                    u32::from(bx) * 16,
                    0xFFFF,
                    0x92,
                    0,
                )?;
                self.machine.cpu.registers.set(Gpr16::Ax, sel);
            }
            0x0003 => self.machine.cpu.registers.set(Gpr16::Ax, 8),
            0x0006 if own => {
                let base = u32::from(self.machine.bus.read_u16(descriptor + 2)?)
                    | (u32::from(self.machine.bus.read_u8(descriptor + 4)?) << 16)
                    | (u32::from(self.machine.bus.read_u8(descriptor + 7)?) << 24);
                self.machine
                    .cpu
                    .registers
                    .set(Gpr16::Cx, (base >> 16) as u16);
                self.machine.cpu.registers.set(Gpr16::Dx, base as u16);
            }
            0x0007 if own => {
                self.machine.bus.write_u16(descriptor + 2, dx)?;
                self.machine.bus.write_u8(descriptor + 4, cx as u8)?;
                self.machine.bus.write_u8(descriptor + 7, (cx >> 8) as u8)?;
            }
            0x0008 if own => {
                let limit = (u32::from(cx) << 16) | u32::from(dx);
                let old = self.machine.bus.read_u8(descriptor + 6)?;
                let (raw, granularity) = if limit > 0xFFFFF {
                    (limit >> 12, 0x80)
                } else {
                    (limit, 0)
                };
                self.machine.bus.write_u16(descriptor, raw as u16)?;
                self.machine.bus.write_u8(
                    descriptor + 6,
                    (old & 0x70) | granularity | ((raw >> 16) as u8 & 15),
                )?;
            }
            0x0009 if own => {
                if cx & 0x10 == 0 {
                    return Ok(Some(0x8021));
                }
                self.machine.bus.write_u8(descriptor + 5, cx as u8 | 0x60)?;
                let limit = self.machine.bus.read_u8(descriptor + 6)? & 15;
                self.machine
                    .bus
                    .write_u8(descriptor + 6, limit | ((cx >> 8) as u8 & 0xF0))?;
            }
            0x000A if own => {
                let sel = match self.dpmi_alloc_descriptors(1) {
                    Ok(s) => s,
                    Err(e) => return Ok(Some(e)),
                };
                let to = LDT + u32::from(sel & !7);
                for i in 0..8 {
                    let byte = self.machine.bus.read_u8(descriptor + i)?;
                    self.machine.bus.write_u8(to + i, byte)?;
                }
                self.machine.bus.write_u8(to + 5, 0xF2)?;
                self.machine.cpu.registers.set(Gpr16::Ax, sel);
            }
            0x000B | 0x000C if own => {
                let address = self.dpmi_pointer(
                    Seg::Es,
                    self.dpmi_offset(Gpr32::Edi),
                    8,
                    if function == 0xB {
                        Access::Write
                    } else {
                        Access::Read
                    },
                )?;
                let (from, to) = if function == 0xB {
                    (descriptor, address)
                } else {
                    (address, descriptor)
                };
                if function == 0xC && self.machine.bus.read_u8(from + 5)? & 0x10 == 0 {
                    return Ok(Some(0x8021));
                }
                for i in 0..8 {
                    let b = self.machine.bus.read_u8(from + i)?;
                    self.machine.bus.write_u8(
                        to + i,
                        if function == 0xC && i == 5 {
                            b | 0x60
                        } else {
                            b
                        },
                    )?;
                }
            }
            0x0001 | 0x0006..=0x000C => return Ok(Some(0x8022)),
            0x0100 => {
                if bx == 0 {
                    return Ok(Some(0x8021));
                }
                if !self.dpmi.as_ref().unwrap().bits32 && bx > 0x1000 {
                    return Ok(Some(0x8001));
                }
                let sel = match self.dpmi_alloc_descriptors(1) {
                    Ok(s) => s,
                    Err(e) => return Ok(Some(e)),
                };
                let segment = match self.memory.allocate(bx, 0x1000) {
                    Ok(s) => s,
                    Err(e) => {
                        self.dpmi.as_mut().unwrap().used[usize::from(sel >> 3)] = false;
                        self.machine.cpu.registers.set(Gpr16::Bx, e.available);
                        return Ok(Some(e.code));
                    }
                };
                self.dpmi.as_mut().unwrap().dos_segments[usize::from(sel >> 3)] = segment;
                self.dpmi_descriptor_at(
                    LDT + u32::from(sel & !7),
                    u32::from(segment) * 16,
                    u32::from(bx) * 16 - 1,
                    0x92,
                    0,
                )?;
                self.memory.write_mcbs(&mut self.machine.bus)?;
                self.machine.cpu.registers.set(Gpr16::Ax, segment);
                self.machine.cpu.registers.set(Gpr16::Dx, sel);
            }
            0x0101 | 0x0102 => {
                if function == 0x102 && bx == 0 {
                    return Ok(Some(0x8021));
                }
                if function == 0x102 && !self.dpmi.as_ref().unwrap().bits32 && bx > 0x1000 {
                    return Ok(Some(0x8001));
                }
                let i = usize::from(dx >> 3);
                if dx & 4 == 0
                    || i >= DESCRIPTORS
                    || self.dpmi.as_ref().unwrap().dos_segments[i] == 0
                {
                    return Ok(Some(9));
                }
                let seg = self.dpmi.as_ref().unwrap().dos_segments[i];
                let result = if function == 0x101 {
                    self.memory.free(seg)
                } else {
                    self.memory.resize(seg, bx)
                };
                if let Err(e) = result {
                    self.machine.cpu.registers.set(Gpr16::Bx, e.available);
                    return Ok(Some(e.code));
                }
                if function == 0x101 {
                    let h = self.dpmi.as_mut().unwrap();
                    h.used[i] = false;
                    h.dos_segments[i] = 0;
                } else {
                    self.dpmi_descriptor_at(
                        LDT + u32::from(dx & !7),
                        u32::from(seg) * 16,
                        u32::from(bx) * 16 - 1,
                        0x92,
                        0,
                    )?;
                }
                self.memory.write_mcbs(&mut self.machine.bus)?;
            }
            0x0200 => {
                let at = u32::from(bx as u8) * 4;
                let off = self.machine.bus.read_u16(at)?;
                let seg = self.machine.bus.read_u16(at + 2)?;
                self.machine.cpu.registers.set(Gpr16::Cx, seg);
                self.machine.cpu.registers.set(Gpr16::Dx, off);
            }
            0x0201 => {
                self.machine.bus.write_u16(u32::from(bx as u8) * 4, dx)?;
                self.machine
                    .bus
                    .write_u16(u32::from(bx as u8) * 4 + 2, cx)?;
            }
            0x0202 | 0x0203 if (bx as u8) < 32 => {
                if function == 0x202 {
                    let (sel, off) = self.dpmi.as_ref().unwrap().exceptions[usize::from(bx as u8)];
                    self.machine.cpu.registers.set(Gpr16::Cx, sel);
                    self.machine.cpu.registers.set32(Gpr32::Edx, off);
                } else {
                    let off = self.dpmi_offset(Gpr32::Edx);
                    let h = self.dpmi.as_mut().unwrap();
                    h.exceptions[usize::from(bx as u8)] = (cx, off);
                    let handled = h.handles_exceptions();
                    self.machine.cpu.set_precise_faults(handled);
                }
            }
            0x0204 => {
                let at = IDT + u32::from(bx as u8) * 8;
                let lo = self.machine.bus.read_u32(at)?;
                let hi = self.machine.bus.read_u32(at + 4)?;
                self.machine.cpu.registers.set(Gpr16::Cx, (lo >> 16) as u16);
                self.machine
                    .cpu
                    .registers
                    .set32(Gpr32::Edx, (lo & 0xFFFF) | (hi & 0xFFFF0000));
            }
            0x0205 => self.dpmi_gate(bx as u8, cx, self.dpmi_offset(Gpr32::Edx))?,
            0x0400 => {
                let r = &mut self.machine.cpu.registers;
                r.set(Gpr16::Ax, 0x005A);
                r.set(Gpr16::Bx, 3);
                r.set(Gpr16::Cx, 3);
                r.set(Gpr16::Dx, 0x0870);
            }
            0x0305 | 0x0306 => {
                let r = &mut self.machine.cpu.registers;
                if function == 0x305 {
                    r.set(Gpr16::Ax, 4);
                }
                r.set(Gpr16::Bx, bios::SHIM_SEGMENT);
                r.set(
                    Gpr16::Cx,
                    if function == 0x305 {
                        SAVE_RM as u16
                    } else {
                        RAW_PM as u16
                    },
                );
                r.set(Gpr16::Si, CODE);
                r.set32(Gpr32::Edi, if function == 0x305 { SAVE_PM } else { RAW_RM });
            }
            0x0303 => {
                let offset = self.dpmi_offset(Gpr32::Edi);
                let structure = self.dpmi_pointer(Seg::Es, offset, 50, Access::Write)?;
                let callback = Callback {
                    code: self.registers().ds,
                    ip: self.dpmi_offset(Gpr32::Esi),
                    data: self.registers().es,
                    offset,
                    structure,
                };
                let h = self.dpmi.as_mut().unwrap();
                let Some(slot) = h.callbacks.iter().position(Option::is_none) else {
                    return Ok(Some(0x8015));
                };
                h.callbacks[slot] = Some(callback);
                self.machine
                    .cpu
                    .registers
                    .set(Gpr16::Cx, bios::SHIM_SEGMENT);
                self.machine
                    .cpu
                    .registers
                    .set(Gpr16::Dx, (CB_BASE + slot as u32 * 8) as u16);
            }
            0x0304 => {
                let off = u32::from(dx);
                if cx != bios::SHIM_SEGMENT
                    || !(CB_BASE..CB_BASE + 32 * 8).contains(&off)
                    || !(off - CB_BASE).is_multiple_of(8)
                {
                    return Ok(Some(0x8024));
                }
                let slot = ((off - CB_BASE) / 8) as usize;
                if self.dpmi.as_mut().unwrap().callbacks[slot].take().is_none() {
                    return Ok(Some(0x8024));
                }
            }
            0x0500 => {
                let at =
                    self.dpmi_pointer(Seg::Es, self.dpmi_offset(Gpr32::Edi), 48, Access::Write)?;
                let (largest, free) = self.dpmi.as_ref().unwrap().memory.free_space();
                let total = (self.machine.bus.ram_size() as u32 - POOL) / 4096;
                for (i, value) in [
                    largest,
                    largest / 4096,
                    largest / 4096,
                    total,
                    free / 4096,
                    free / 4096,
                    total,
                    free / 4096,
                    0,
                    u32::MAX,
                    u32::MAX,
                    u32::MAX,
                ]
                .into_iter()
                .enumerate()
                {
                    self.machine.bus.write_u32(at + i as u32 * 4, value)?;
                }
            }
            0x0501 | 0x0503 => {
                let memory = &mut self.dpmi.as_mut().unwrap().memory;
                let result = if function == 0x501 {
                    memory.allocate(pair).map(|block| (block, None))
                } else {
                    memory.resize(handle, pair)
                };
                match result {
                    Ok((block, moved)) => {
                        if let Some((from, length)) = moved {
                            self.dpmi_move(from, block.address, length)?;
                        }
                        let r = &mut self.machine.cpu.registers;
                        r.set(Gpr16::Bx, (block.address >> 16) as u16);
                        r.set(Gpr16::Cx, block.address as u16);
                        r.set(Gpr16::Si, (block.handle >> 16) as u16);
                        r.set(Gpr16::Di, block.handle as u16);
                    }
                    Err(e) => return Ok(Some(e)),
                }
            }
            0x0502 => {
                if let Err(e) = self.dpmi.as_mut().unwrap().memory.free(handle) {
                    return Ok(Some(e));
                }
            }
            // No paging: every page is resident, and paging hints change nothing.
            0x0600..=0x0603 | 0x0702 | 0x0703 => {}
            0x0604 => {
                self.machine.cpu.registers.set(Gpr16::Bx, 0);
                self.machine.cpu.registers.set(Gpr16::Cx, 4096);
            }
            0x0900..=0x0902 => {
                let at = self.dpmi_pointer(
                    Seg::Ss,
                    self.machine.cpu.stack_pointer() + 8,
                    4,
                    Access::Write,
                )?;
                let flags = self.machine.bus.read_u32(at)?;
                self.machine
                    .cpu
                    .registers
                    .set_byte(0, u8::from(flags & 0x200 != 0));
                if function != 0x902 {
                    self.machine.bus.write_u32(
                        at,
                        (flags & !0x200) | if function == 0x901 { 0x200 } else { 0 },
                    )?;
                }
            }
            // The x87 is present or absent for the whole machine; a client
            // that emulates it handles its own #NM through the exceptions.
            0x0E01 => {}
            0x0E00 => self.machine.cpu.registers.set(
                Gpr16::Ax,
                if self.machine.cpu.fpu.is_some() {
                    0x4D
                } else {
                    0
                },
            ),
            _ => {
                let stats = &mut self.dpmi.as_mut().unwrap().stats;
                if stats.unsupported < 16 {
                    stats.unsupported_functions[stats.unsupported as usize] = function;
                }
                stats.unsupported += 1;
                return Ok(Some(0x8001));
            }
        }
        Ok(None)
    }

    fn dpmi_reflect(
        &mut self,
        vector: u8,
        structure: Option<u32>,
        procedure: Option<(u16, u16)>,
        far_return: bool,
    ) -> Result<(), RuntimeError> {
        self.dpmi_reflect_with(vector, structure, procedure, far_return, 0)
    }

    /// Reflect to real mode, first copying `words` stack words of the client,
    /// from above the INT 31h frame, to the real-mode stack, as arguments of
    /// the called procedure or interrupt handler. They are not copied back.
    fn dpmi_reflect_with(
        &mut self,
        vector: u8,
        structure: Option<u32>,
        procedure: Option<(u16, u16)>,
        far_return: bool,
        words: u16,
    ) -> Result<(), RuntimeError> {
        let mut arguments = [0u8; 2 * MAX_STACK_WORDS as usize];
        let arguments = &mut arguments[..2 * usize::from(words)];
        if words != 0 {
            // Host gates are 32-bit: INT 31h pushed EIP, CS and EFLAGS dwords.
            let top = self.machine.cpu.stack_pointer().wrapping_add(12);
            for (i, byte) in arguments.iter_mut().enumerate() {
                let at = self.dpmi_pointer(Seg::Ss, top.wrapping_add(i as u32), 1, Access::Read)?;
                *byte = self.machine.bus.read_u8(at)?;
            }
        }
        let h = self.dpmi.as_ref().unwrap();
        if h.depth == h.frames.len() {
            return if structure.is_some() {
                self.dpmi_cf(Some(0x8012))
            } else {
                Err(dpmi_error(0x8012))
            };
        }
        let context = self.machine.cpu.context();
        let mut rm = RealRegisters::from_cpu(&self.machine.cpu);
        if let Some(at) = structure {
            rm = RealRegisters::read(&mut self.machine.bus, at)?;
        } else {
            // DOS/4GW translates pointer-taking DOS calls itself. Generic
            // reflection only accepts selectors representing real addresses.
            for seg in [Seg::Ds, Seg::Es, Seg::Fs, Seg::Gs] {
                let cache = self.registers().segment_cache(seg);
                rm.segments[seg as usize] =
                    if cache.usable && cache.base < 0x100000 && cache.base & 15 == 0 {
                        (cache.base >> 4) as u16
                    } else {
                        0
                    };
            }
            rm.segments[Seg::Ss as usize] = 0;
            rm.general[4] = 0;
            let flags_at = self.dpmi_pointer(
                Seg::Ss,
                self.machine.cpu.stack_pointer() + 8,
                4,
                Access::Read,
            )?;
            rm.flags = self.machine.bus.read_u32(flags_at)? as u16;
        }
        let (segment, offset) = if let Some(target) = procedure {
            target
        } else {
            (
                self.machine.bus.read_u16(u32::from(vector) * 4 + 2)?,
                self.machine.bus.read_u16(u32::from(vector) * 4)?,
            )
        };
        let service = if segment == bios::SHIM_SEGMENT {
            bios::service_at(&mut self.machine.bus, u32::from(offset))?
        } else {
            None
        };
        let service = service.filter(|s| {
            words == 0
                && (procedure.is_some() || s.vector() == vector)
                && direct_service(s.vector(), rm.general[0] as u16)
        });
        if !far_return
            && self.dpmi.as_ref().unwrap().mode == Mode::Direct
            && structure.is_some()
            && let Some(service) = service
        {
            self.dpmi.as_mut().unwrap().stats.direct += 1;
            // Preserve even the scratch IRET frame left behind by reflection.
            // It may be on a caller-provided stack in ordinary guest memory.
            let h = self.dpmi.as_ref().unwrap();
            let (ss, sp) = if rm.segments[Seg::Ss as usize] == 0 && rm.general[4] == 0 {
                (h.private_segment, (0x4000 - h.depth * 0x400) as u16)
            } else {
                (rm.segments[Seg::Ss as usize], rm.general[4] as u16)
            };
            let frame_flags = rm.flags | if procedure.is_none() { 2 } else { 0 };
            for (offset, value) in [
                (6, RM_RETURN as u16),
                (4, bios::SHIM_SEGMENT),
                (2, frame_flags),
            ] {
                self.machine
                    .bus
                    .write_u16(linear_address(ss, sp.wrapping_sub(offset)), value)?;
            }
            // Service adapters consume real-mode register values, but execute
            // no guest instructions and never change CR0 or the hidden caches.
            rm.apply_values(&mut self.machine.cpu);
            self.service_flags = Some(frame_flags);
            let result = self.service(service.vector());
            let mut output = RealRegisters::from_cpu(&self.machine.cpu);
            let flags = self.service_flags.take().unwrap();
            output.flags = flags | 2;
            self.machine.cpu.restore_context(&context);
            result?;
            self.machine
                .bus
                .write_u16(linear_address(ss, sp.wrapping_sub(2)), flags)?;
            output.write(&mut self.machine.bus, structure.unwrap())?;
            // Replace the real-mode IRET, retaining its logical time. The next
            // bounded step executes the protected-mode IRET as in Reflect mode.
            self.dpmi.as_mut().unwrap().direct_return_pending = true;
            return self.dpmi_cf(None);
        }
        let h = self.dpmi.as_mut().unwrap();
        if h.depth == h.frames.len() {
            return Err(dpmi_error(0x8012));
        }
        if rm.segments[Seg::Ss as usize] == 0 && rm.general[4] == 0 {
            rm.segments[Seg::Ss as usize] = h.private_segment;
            rm.general[4] = 0x4000 - (h.depth as u32 * 0x400);
        }
        h.frames[h.depth] = Some(Frame {
            context,
            structure,
            callback: false,
            timer: false,
            exception: false,
        });
        h.depth += 1;
        h.stats.reflected += 1;
        self.machine.cpu.set_protected_mode(false);
        rm.apply_values(&mut self.machine.cpu);
        self.machine.cpu.control.idtr = DescriptorTable {
            base: 0,
            limit: 0x3FF,
        };
        for (seg, value) in Seg::ALL.into_iter().zip(rm.segments) {
            self.machine.cpu.load_segment(
                &mut self.machine.bus,
                seg,
                if seg == Seg::Cs {
                    bios::SHIM_SEGMENT
                } else {
                    value
                },
            )?;
        }
        self.machine.cpu.registers.ip = RM_RETURN;
        self.machine.cpu.flags = u32::from(rm.flags) | 2;
        if words != 0 {
            let sp = self.registers().get(Gpr16::Sp).wrapping_sub(2 * words);
            let stack = self.registers().ss;
            for (i, &byte) in arguments.iter().enumerate() {
                self.machine
                    .bus
                    .write_u8(linear_address(stack, sp.wrapping_add(i as u16)), byte)?;
            }
            self.machine.cpu.registers.set(Gpr16::Sp, sp);
        }
        if let Some((seg, off)) = procedure {
            let sp = self
                .registers()
                .get(Gpr16::Sp)
                .wrapping_sub(if far_return { 4 } else { 6 });
            let stack = self.registers().ss;
            self.machine
                .bus
                .write_u16(linear_address(stack, sp), RM_RETURN as u16)?;
            self.machine.bus.write_u16(
                linear_address(stack, sp.wrapping_add(2)),
                bios::SHIM_SEGMENT,
            )?;
            if !far_return {
                self.machine
                    .bus
                    .write_u16(linear_address(stack, sp.wrapping_add(4)), rm.flags)?;
            }
            self.machine.cpu.registers.set(Gpr16::Sp, sp);
            self.machine
                .cpu
                .load_segment(&mut self.machine.bus, Seg::Cs, seg)?;
            self.machine.cpu.registers.ip = u32::from(off);
            self.machine.cpu.flags &= if far_return { !0x100 } else { !0x300 };
        } else {
            self.machine.cpu.interrupt(&mut self.machine.bus, vector)?;
        }
        Ok(())
    }

    fn dpmi_real_return(&mut self) -> Result<(), RuntimeError> {
        let mut output = RealRegisters::from_cpu(&self.machine.cpu);
        let h = self.dpmi.as_mut().unwrap();
        if h.depth == 0 {
            return Err(dpmi_error(0x8021));
        }
        h.depth -= 1;
        let frame = h.frames[h.depth].take().unwrap();
        if frame.callback {
            return Err(dpmi_error(0x8021));
        }
        self.machine.cpu.restore_context(&frame.context);
        if let Some(at) = frame.structure {
            // DPMI does not expose the private return trampoline or stack.
            let input = RealRegisters::read(&mut self.machine.bus, at)?;
            output.segments[Seg::Cs as usize] = input.segments[Seg::Cs as usize];
            output.segments[Seg::Ss as usize] = input.segments[Seg::Ss as usize];
            output.ip = input.ip;
            output.general[4] = input.general[4];
            output.write(&mut self.machine.bus, at)?;
            self.dpmi_cf(None)?;
        } else {
            for (reg, value) in GENERAL.into_iter().zip(output.general) {
                if reg != Gpr32::Esp {
                    self.machine.cpu.registers.set32(reg, value);
                }
            }
            let at = self.dpmi_pointer(
                Seg::Ss,
                self.machine.cpu.stack_pointer() + 8,
                4,
                Access::Write,
            )?;
            let old = self.machine.bus.read_u32(at)?;
            self.machine
                .bus
                .write_u32(at, (old & !0x8D5) | (u32::from(output.flags) & 0x8D5))?;
        }
        Ok(())
    }

    fn dpmi_raw_switch(&mut self, protected: bool) -> Result<(), RuntimeError> {
        let r = self.registers();
        let (ds, es, ss, cs, sp, ip) = (
            r.get(Gpr16::Ax),
            r.get(Gpr16::Cx),
            r.get(Gpr16::Dx),
            r.get(Gpr16::Si),
            r.get32(Gpr32::Ebx),
            r.get32(Gpr32::Edi),
        );
        self.machine.cpu.set_protected_mode(protected);
        self.machine
            .cpu
            .set_current_privilege_level(if protected { 3 } else { 0 });
        self.machine.cpu.control.idtr = if protected {
            DescriptorTable {
                base: IDT,
                limit: 2047,
            }
        } else {
            DescriptorTable {
                base: 0,
                limit: 0x3FF,
            }
        };
        for (seg, value) in [
            (Seg::Cs, cs),
            (Seg::Ds, ds),
            (Seg::Es, es),
            (Seg::Ss, ss),
            (Seg::Fs, 0),
            (Seg::Gs, 0),
        ] {
            self.machine
                .cpu
                .load_segment(&mut self.machine.bus, seg, value)?;
        }
        self.machine
            .cpu
            .registers
            .set32(Gpr32::Esp, if protected { sp } else { sp & 0xFFFF });
        self.machine.cpu.registers.ip = if protected { ip } else { ip & 0xFFFF };
        Ok(())
    }

    fn dpmi_callback(&mut self, slot: usize) -> Result<(), RuntimeError> {
        let rm = RealRegisters::from_cpu(&self.machine.cpu);
        let context = self.machine.cpu.context();
        let h = self.dpmi.as_mut().unwrap();
        let callback = h.callbacks[slot].ok_or_else(|| dpmi_error(0x8024))?;
        if h.depth == h.frames.len() {
            return Err(dpmi_error(0x8012));
        }
        h.frames[h.depth] = Some(Frame {
            context,
            structure: Some(callback.structure),
            callback: true,
            timer: false,
            exception: false,
        });
        h.depth += 1;
        let sp = POOL - h.depth as u32 * 0x400;
        let stack_alias = (0x80 + (h.depth as u16 - 1) * 8) | 3;
        let bits32 = h.bits32;
        self.dpmi_descriptor_at(
            GDT + u32::from(stack_alias & !7),
            u32::from(rm.segments[Seg::Ss as usize]) * 16,
            0xFFFF,
            0x92,
            0,
        )?;
        rm.write(&mut self.machine.bus, callback.structure)?;
        self.machine.cpu.set_protected_mode(true);
        self.machine.cpu.set_current_privilege_level(3);
        self.machine.cpu.control.idtr = DescriptorTable {
            base: IDT,
            limit: 2047,
        };
        for (seg, sel) in [
            (Seg::Cs, callback.code),
            (Seg::Ss, DATA),
            (Seg::Ds, stack_alias),
            (Seg::Es, callback.data),
            (Seg::Fs, 0),
            (Seg::Gs, 0),
        ] {
            self.machine
                .cpu
                .load_segment(&mut self.machine.bus, seg, sel)?;
        }
        let bytes = if bits32 { 4 } else { 2 };
        for (i, value) in [CB_RETURN, u32::from(CODE), 2].into_iter().enumerate() {
            let at = sp - 3 * bytes + i as u32 * bytes;
            if bits32 {
                self.machine.bus.write_u32(at, value)?;
            } else {
                self.machine.bus.write_u16(at, value as u16)?;
            }
        }
        self.machine.cpu.registers.set32(Gpr32::Esp, sp - 3 * bytes);
        self.machine
            .cpu
            .registers
            .set32(Gpr32::Esi, rm.general[4] & 0xFFFF);
        self.machine
            .cpu
            .registers
            .set32(Gpr32::Edi, callback.offset);
        self.machine.cpu.registers.ip = callback.ip;
        self.machine.cpu.flags = 2;
        Ok(())
    }

    fn dpmi_callback_return(&mut self) -> Result<(), RuntimeError> {
        // DPMI permits a callback to return a different ES:(E)DI structure.
        let at = self.dpmi_pointer(Seg::Es, self.dpmi_offset(Gpr32::Edi), 50, Access::Read)?;
        let h = self.dpmi.as_mut().unwrap();
        if h.depth == 0 {
            return Err(dpmi_error(0x8021));
        }
        h.depth -= 1;
        let frame = h.frames[h.depth].take().unwrap();
        if !frame.callback {
            return Err(dpmi_error(0x8021));
        }
        let rm = RealRegisters::read(&mut self.machine.bus, at)?;
        self.machine.cpu.restore_context(&frame.context);
        rm.apply_values(&mut self.machine.cpu);
        for (seg, value) in Seg::ALL.into_iter().zip(rm.segments) {
            self.machine
                .cpu
                .load_segment(&mut self.machine.bus, seg, value)?;
        }
        self.machine.cpu.flags = u32::from(rm.flags) | 2;
        Ok(())
    }
}

fn dpmi_error(ax: u16) -> RuntimeError {
    RuntimeError::UnsupportedService { vector: 0x31, ax }
}

fn direct_service(vector: u8, ax: u16) -> bool {
    match vector {
        0x10 => matches!(
            ax,
            0x4F00..=0x4F03 | 0x4F05 | 0x1010 | 0x1012 | 0x1015 | 0x1017
        ),
        0x33 => matches!(ax, 0..=0xB | 0x15 | 0x1A | 0x1B | 0x24),
        0x16 => matches!(ax >> 8, 1 | 2 | 5 | 0x11),
        0x21 => matches!(
            ax >> 8,
            0x0B | 0x19 | 0x2A | 0x2C | 0x30 | 0x33 | 0x51 | 0x62
        ),
        _ => false,
    }
}

struct RealRegisters {
    general: [u32; 8],
    segments: [u16; 6],
    ip: u16,
    flags: u16,
}
impl RealRegisters {
    fn from_cpu(cpu: &doslet_x86::Cpu) -> Self {
        Self {
            general: GENERAL.map(|r| cpu.registers.get32(r)),
            segments: Seg::ALL.map(|s| cpu.registers.selector(s)),
            ip: cpu.registers.ip as u16,
            flags: cpu.flags as u16,
        }
    }
    fn read(bus: &mut impl MemoryBus, at: u32) -> Result<Self, BusError> {
        let mut general = [0; 8];
        for (i, offset) in RM_OFFSETS.into_iter().enumerate() {
            general[i] = if i == 4 {
                u32::from(bus.read_u16(at + offset)?)
            } else {
                bus.read_u32(at + offset)?
            };
        }
        Ok(Self {
            general,
            segments: [
                bus.read_u16(at + 34)?,
                bus.read_u16(at + 44)?,
                bus.read_u16(at + 48)?,
                bus.read_u16(at + 36)?,
                bus.read_u16(at + 38)?,
                bus.read_u16(at + 40)?,
            ],
            ip: bus.read_u16(at + 42)?,
            flags: bus.read_u16(at + 32)?,
        })
    }
    fn write(&self, bus: &mut impl MemoryBus, at: u32) -> Result<(), BusError> {
        for (i, offset) in RM_OFFSETS.into_iter().enumerate() {
            if i == 4 {
                bus.write_u16(at + offset, self.general[i] as u16)?;
            } else {
                bus.write_u32(at + offset, self.general[i])?;
            }
        }
        for (seg, offset) in Seg::ALL.into_iter().zip([34, 44, 48, 36, 38, 40]) {
            bus.write_u16(at + offset, self.segments[seg as usize])?;
        }
        bus.write_u16(at + 42, self.ip)?;
        bus.write_u16(at + 32, self.flags)
    }
    fn apply_values(&self, cpu: &mut doslet_x86::Cpu) {
        for (reg, value) in GENERAL.into_iter().zip(self.general) {
            cpu.registers.set32(reg, value);
        }
        let [es, cs, ss, ds, fs, gs] = self.segments;
        cpu.registers.es = es;
        cpu.registers.cs = cs;
        cpu.registers.ss = ss;
        cpu.registers.ds = ds;
        cpu.registers.fs = fs;
        cpu.registers.gs = gs;
        cpu.registers.ip = u32::from(self.ip);
    }
}

#[cfg(test)]
mod tests;
